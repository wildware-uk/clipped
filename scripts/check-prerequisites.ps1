#Requires -Version 5.1

<#
.SYNOPSIS
    Checks that the machine has everything needed to build and run Clipped.

.DESCRIPTION
    Prints one pass/fail line per prerequisite and, when something required is
    absent, exits non-zero with a summary naming what is missing and how to
    install it. The point is that a contributor learns "the Windows SDK is not
    installed" from this script rather than from a linker error several minutes
    into a build.

    Every external thing the script probes is a parameter with a sensible
    default, so its outcome can be steered without touching the machine. That is
    how scripts/test-check-prerequisites.ps1 reaches the states worth testing:
    absence, by pointing a probe at something that is genuinely not there, and
    the harder "present but wrong" states - Visual Studio without the C++
    workload, Node on the wrong major, a pin that is not installed - by pointing
    a probe at a stand-in that answers the way the real tool would. The
    detection, parsing and reporting under test are the production code either
    way.

    Prerequisites are either required - a missing one fails the run - or
    recommended, which reports a warning and leaves the exit code alone.
    Recommended covers tooling that is not needed for the part of the project
    that exists today, such as Node before the desktop application lands.

.PARAMETER RustupCommand
    Name or path of the rustup executable.

.PARAMETER CargoCommand
    Name or path of the cargo executable, used to check that the rustfmt and
    clippy components are actually usable.

.PARAMETER NodeCommand
    Name or path of the node executable.

.PARAMETER FfprobeCommand
    Name or path of the ffprobe executable, used by the media tests.

.PARAMETER VsWherePath
    Path to vswhere.exe, the Visual Studio installer's own query tool. It is
    installed at a fixed location by any Visual Studio or Build Tools install,
    so its absence is itself evidence that neither is present.

.PARAMETER WindowsKitsRegistryPath
    Registry key holding the Windows SDK installation root.

.PARAMETER RustToolchainFile
    Path to rust-toolchain.toml, whose pinned channel the installed rustc is
    compared against. Defaults to the file in the repository root.

.PARAMETER NvmrcFile
    Path to .nvmrc, whose pinned version the installed node is compared
    against. Defaults to the file in the repository root.

.PARAMETER DesktopManifest
    Path to the desktop application's package.json, defaulting to
    apps/desktop/package.json. Node is only required once this file exists;
    until then a missing Node is a warning.

.PARAMETER GraphicsAdapterInventory
    Path to a JSON file describing display adapters (Name, DriverVersion,
    DriverDate), used instead of querying WMI. Left empty in normal use. It
    exists so the adapter reporting and the stale-driver warning can be tested
    against fixed hardware descriptions rather than against whichever GPU the
    person running the tests happens to own.

.PARAMETER MinimumWindowsBuild
    Lowest acceptable Windows build number.

.PARAMETER MaximumDriverAgeDays
    Age at which a graphics driver is reported as stale. Hardware encoding is
    the part of Clipped most sensitive to driver age, so an old driver is worth
    mentioning even though it is not a hard failure.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts/check-prerequisites.ps1

.OUTPUTS
    Exit code 0 when every required prerequisite is present, 1 otherwise.
#>

[CmdletBinding()]
param(
    [string] $RustupCommand = 'rustup',
    [string] $CargoCommand = 'cargo',
    [string] $NodeCommand = 'node',
    [string] $FfprobeCommand = 'ffprobe',
    [string] $VsWherePath = (Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'),
    [string] $WindowsKitsRegistryPath = 'HKLM:\SOFTWARE\Microsoft\Windows Kits\Installed Roots',
    [string] $RustToolchainFile = '',
    [string] $NvmrcFile = '',
    [string] $DesktopManifest = '',
    [string] $GraphicsAdapterInventory = '',
    [int] $MinimumWindowsBuild = 19044,
    [int] $MaximumDriverAgeDays = 730
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Windows PowerShell does not populate $PSScriptRoot while binding parameter
# defaults, so the repository-relative defaults are filled in here instead.
$repositoryRoot = Split-Path -Parent $PSScriptRoot
if (-not $RustToolchainFile) { $RustToolchainFile = Join-Path $repositoryRoot 'rust-toolchain.toml' }
if (-not $NvmrcFile) { $NvmrcFile = Join-Path $repositoryRoot '.nvmrc' }
if (-not $DesktopManifest) { $DesktopManifest = Join-Path $repositoryRoot 'apps\desktop\package.json' }

# Windows 10 21H2 is build 19044. SPEC.md section 3 targets "Windows 11 /
# modern Windows 10"; 19044 is where that line is drawn, because it is the
# oldest Windows 10 release Microsoft still services. The capture backend does
# not exist yet, so no API-level floor has been established - crates/capture is
# a documentation-only placeholder. If the backend turns out to need something
# newer, this number moves in M1 and the reason is recorded there.

$script:InstallGuide = 'docs/prerequisites.md'

function New-CheckResult {
    <#
    .SYNOPSIS
        Builds the record for one prerequisite.
    .PARAMETER Status
        Pass, Fail or Warn. Only Fail affects the exit code.
    .PARAMETER Detail
        What was found, or what was looked for and not found.
    .PARAMETER Fix
        What the reader should do about it. Required for anything not passing.
    #>
    param(
        [Parameter(Mandatory)] [string] $Name,
        [Parameter(Mandatory)] [ValidateSet('Pass', 'Fail', 'Warn')] [string] $Status,
        [Parameter(Mandatory)] [string] $Detail,
        [string] $Fix = ''
    )

    [pscustomobject]@{
        Name   = $Name
        Status = $Status
        Detail = $Detail
        Fix    = $Fix
    }
}

function Invoke-Probe {
    <#
    .SYNOPSIS
        Runs an external command, tolerating its absence and its failure.
    .DESCRIPTION
        Returns Found/ExitCode/Output rather than throwing, so a check can tell
        "not installed" apart from "installed but unhappy" and report the
        difference. Only application commands are resolved, so a PowerShell
        alias or function of the same name cannot be mistaken for the real tool.

        Output is flattened to plain text. Native tools report version
        information on either stream, and Windows PowerShell wraps redirected
        native standard error in error records whose default rendering prefixes
        the executable name and appends a CategoryInfo block. Rendering those
        into a contributor-facing message produces "rustup.exe : error:
        toolchain ... is not installed" followed by several lines of call-stack
        noise, none of which is information. Each record is reduced to the line
        the tool actually wrote.
    .PARAMETER WorkingDirectory
        Directory to run the command from, when it exists. Tools that read
        per-directory configuration - rustup reading rust-toolchain.toml, for
        one - answer differently depending on where they are invoked.
    #>
    param(
        [Parameter(Mandatory)] [string] $Command,
        [string[]] $Arguments = @(),
        [string] $WorkingDirectory = ''
    )

    $resolved = Get-Command -Name $Command -CommandType Application -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if (-not $resolved) {
        return [pscustomobject]@{ Found = $false; ExitCode = $null; Output = ''; Path = '' }
    }

    $ErrorActionPreference = 'Continue'
    $global:LASTEXITCODE = 0

    $pushed = $false
    $lines = @()
    $exitCode = 0
    try {
        if ($WorkingDirectory -and (Test-Path -LiteralPath $WorkingDirectory)) {
            Push-Location -LiteralPath $WorkingDirectory
            $pushed = $true
        }

        $lines = @(& $resolved.Source @Arguments 2>&1 | ForEach-Object {
                if ($_ -is [System.Management.Automation.ErrorRecord]) { $_.Exception.Message }
                else { [string] $_ }
            })
        $exitCode = $LASTEXITCODE
    } finally {
        if ($pushed) { Pop-Location }
    }

    [pscustomobject]@{
        Found    = $true
        ExitCode = $exitCode
        Output   = ($lines -join [Environment]::NewLine).Trim()
        Path     = $resolved.Source
    }
}

function Get-PinnedRustVersion {
    <#
    .SYNOPSIS
        Reads the channel pinned by rust-toolchain.toml, or '' if unreadable.
    #>
    param([Parameter(Mandatory)] [string] $Path)

    if (-not (Test-Path -LiteralPath $Path)) { return '' }

    $match = Select-String -LiteralPath $Path -Pattern '^\s*channel\s*=\s*"([^"]+)"' |
        Select-Object -First 1
    if (-not $match) { return '' }

    $match.Matches[0].Groups[1].Value
}

function Get-PinnedNodeVersion {
    <#
    .SYNOPSIS
        Reads the version pinned by .nvmrc, or '' if unreadable.
    #>
    param([Parameter(Mandatory)] [string] $Path)

    if (-not (Test-Path -LiteralPath $Path)) { return '' }

    $contents = (Get-Content -LiteralPath $Path -TotalCount 1)
    if (-not $contents) { return '' }

    $contents.Trim().TrimStart('v')
}

function Test-WindowsVersion {
    <#
    .SYNOPSIS
        Checks the running Windows build against the supported minimum.
    #>
    param([Parameter(Mandatory)] [int] $MinimumBuild)

    $version = [Environment]::OSVersion.Version
    $name = 'Windows version'

    # The edition comes from CIM rather than the registry because
    # HKLM\...\CurrentVersion\ProductName still reads "Windows 10 Pro" on
    # Windows 11, which would make a passing line look like a failing one. The
    # feature update (23H2, 24H2, ...) only exists in the registry.
    $caption = 'Windows'
    $release = ''
    try {
        $caption = (Get-CimInstance -ClassName Win32_OperatingSystem -ErrorAction Stop).Caption.Trim()
    } catch {
        # Reporting detail only; the build number is what the decision uses.
    }
    try {
        $currentVersionKey = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion'
        $properties = Get-ItemProperty -LiteralPath $currentVersionKey
        if ($properties.PSObject.Properties.Name -contains 'DisplayVersion') {
            $release = $properties.DisplayVersion
        }
    } catch {
        # As above.
    }

    $described = "$caption build $($version.Build)"
    if ($release) { $described = "$caption $release (build $($version.Build))" }

    if ($version.Build -ge $MinimumBuild) {
        return New-CheckResult -Name $name -Status 'Pass' -Detail $described
    }

    New-CheckResult -Name $name -Status 'Fail' `
        -Detail "$described is below the minimum supported build $MinimumBuild" `
        -Fix 'Update Windows (Settings > Windows Update). Clipped supports Windows 10 21H2 (build 19044) and later, and Windows 11.'
}

function Test-VisualStudioBuildTools {
    <#
    .SYNOPSIS
        Checks for an installation carrying the MSVC C++ x64 toolset.
    .DESCRIPTION
        Queries the Visual Studio installer instead of guessing at paths, so
        Build Tools, Community, Professional and Enterprise all count, and a
        Visual Studio install without the C++ workload is correctly reported as
        missing rather than as present.
    #>
    param([Parameter(Mandatory)] [string] $VsWhere)

    $name = 'Visual Studio C++ build tools'
    $fix = 'Install "Visual Studio 2022 Build Tools" from https://visualstudio.microsoft.com/downloads/ and select the "Desktop development with C++" workload.'

    if (-not (Test-Path -LiteralPath $VsWhere)) {
        return New-CheckResult -Name $name -Status 'Fail' `
            -Detail "no Visual Studio installer found (looked for $VsWhere)" -Fix $fix
    }

    $probe = Invoke-Probe -Command $VsWhere -Arguments @(
        '-products', '*',
        '-requires', 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64',
        '-latest', '-format', 'json'
    )

    if ($probe.ExitCode -ne 0) {
        return New-CheckResult -Name $name -Status 'Fail' `
            -Detail "the Visual Studio installer query failed (exit $($probe.ExitCode)): $($probe.Output)" -Fix $fix
    }

    $parsed = $null
    if ($probe.Output) {
        try {
            $parsed = ConvertFrom-Json -InputObject $probe.Output
        } catch {
            return New-CheckResult -Name $name -Status 'Fail' `
                -Detail "could not read the Visual Studio installation list: $($_.Exception.Message)" -Fix $fix
        }
    }

    # vswhere prints "[]" when nothing satisfies -requires, which is exactly the
    # case this check exists to catch: Visual Studio present, C++ workload not.
    #
    # Windows PowerShell's ConvertFrom-Json emits a JSON array as one collection
    # object rather than as its elements, so anything that collects its output -
    # @(...) around a pipeline, most obviously - ends up holding the array
    # instead of what is in it. For "[]" that is a Count of 1, not 0, so a
    # zero-count guard never fires and the "first installation" is an empty
    # Object[] with no displayName to read; under Set-StrictMode that is a
    # terminating error. Selecting the records that actually describe an
    # installation flattens the collection and guards the properties at once,
    # and does not care which shape ConvertFrom-Json chose.
    $installations = @($parsed | Where-Object {
            $null -ne $_ -and $_.PSObject.Properties.Name -contains 'installationVersion'
        })

    if ($installations.Count -eq 0) {
        return New-CheckResult -Name $name -Status 'Fail' `
            -Detail 'Visual Studio is installed but no instance has the MSVC x64 C++ toolset' -Fix $fix
    }

    $installation = $installations[0]
    $displayName = 'Visual Studio'
    if ($installation.PSObject.Properties.Name -contains 'displayName') {
        $displayName = $installation.displayName
    }

    New-CheckResult -Name $name -Status 'Pass' `
        -Detail "$displayName $($installation.installationVersion)"
}

function Test-WindowsSdk {
    <#
    .SYNOPSIS
        Checks that a Windows 10/11 SDK is installed.
    #>
    param([Parameter(Mandatory)] [string] $RegistryPath)

    $name = 'Windows SDK'
    $fix = 'Install the "Windows 11 SDK" component from the Visual Studio Installer, or the standalone SDK from https://developer.microsoft.com/windows/downloads/windows-sdk/.'

    if (-not (Test-Path -LiteralPath $RegistryPath)) {
        return New-CheckResult -Name $name -Status 'Fail' `
            -Detail "no SDK registration found at $RegistryPath" -Fix $fix
    }

    $root = ''
    try {
        $root = (Get-ItemProperty -LiteralPath $RegistryPath).KitsRoot10
    } catch {
        $root = ''
    }

    if (-not $root -or -not (Test-Path -LiteralPath $root)) {
        return New-CheckResult -Name $name -Status 'Fail' `
            -Detail 'the SDK is registered but its installation root is missing from disk' -Fix $fix
    }

    $includeRoot = Join-Path $root 'Include'
    $versions = @()
    if (Test-Path -LiteralPath $includeRoot) {
        $versions = @(Get-ChildItem -LiteralPath $includeRoot -Directory -ErrorAction SilentlyContinue |
                Where-Object { $_.Name -match '^10\.0\.\d+\.\d+$' } |
                Select-Object -ExpandProperty Name |
                Sort-Object { [version] $_ })
    }

    if ($versions.Count -eq 0) {
        return New-CheckResult -Name $name -Status 'Fail' `
            -Detail "no SDK headers under $includeRoot" -Fix $fix
    }

    New-CheckResult -Name $name -Status 'Pass' -Detail "$($versions[-1]) in $root"
}

function Test-RustToolchain {
    <#
    .SYNOPSIS
        Checks that the pinned toolchain is installed and is the one in effect.
    .DESCRIPTION
        Two questions, because they fail independently and have different fixes.

        Is the pin installed? "rustup run <pin> rustc --version" names the
        toolchain explicitly, so its exit code answers that and nothing else. It
        cannot detect a pin being bypassed, because it bypasses the pin
        machinery itself to ask.

        Is the pin in effect? "rustup show active-toolchain", run from the
        repository root, reports the toolchain a cargo command typed there would
        actually use. Two things override rust-toolchain.toml and neither is
        visible in it: a directory override left behind by "rustup override
        set", and the RUSTUP_TOOLCHAIN environment variable. A contributor in
        that state builds with a toolchain the repository never asked for, and
        the only symptom is lint or format output that nobody else can
        reproduce.
    #>
    param(
        [Parameter(Mandatory)] [string] $Rustup,
        [Parameter(Mandatory)] [string] $PinnedVersion,
        [Parameter(Mandatory)] [string] $RepositoryPath
    )

    $name = 'Rust toolchain'
    $fix = 'Install rustup from https://rustup.rs, then run "rustup toolchain install" from the repository root to fetch the pinned toolchain.'

    $rustupProbe = Invoke-Probe -Command $Rustup -Arguments @('--version')
    if (-not $rustupProbe.Found) {
        return New-CheckResult -Name $name -Status 'Fail' `
            -Detail "$Rustup is not on PATH" -Fix $fix
    }

    $pinProbe = Invoke-Probe -Command $Rustup -WorkingDirectory $RepositoryPath `
        -Arguments @('run', $PinnedVersion, 'rustc', '--version')
    if ($pinProbe.ExitCode -ne 0) {
        return New-CheckResult -Name $name -Status 'Fail' `
            -Detail "the pinned toolchain $PinnedVersion is not installed: $($pinProbe.Output)" `
            -Fix "Run `"rustup toolchain install $PinnedVersion`" from the repository root."
    }

    $activeProbe = Invoke-Probe -Command $Rustup -WorkingDirectory $RepositoryPath `
        -Arguments @('show', 'active-toolchain')
    if ($activeProbe.ExitCode -ne 0) {
        return New-CheckResult -Name $name -Status 'Fail' `
            -Detail "rustup could not report the toolchain active in $RepositoryPath`: $($activeProbe.Output)" `
            -Fix $fix
    }

    # "1.97.1-x86_64-pc-windows-msvc (overridden by 'C:\clipped\rust-toolchain.toml')"
    # - the toolchain name is the first token, the reason follows in brackets.
    $activeLine = @($activeProbe.Output -split "`r?`n" | Where-Object { $_.Trim() }) |
        Select-Object -First 1
    if (-not $activeLine) {
        return New-CheckResult -Name $name -Status 'Fail' `
            -Detail "rustup reported no toolchain active in $RepositoryPath" -Fix $fix
    }

    $activeToolchain = ($activeLine.Trim() -split '\s+')[0]
    if ($activeToolchain -notmatch "^$([regex]::Escape($PinnedVersion))(-|$)") {
        return New-CheckResult -Name $name -Status 'Fail' `
            -Detail "the toolchain active in $RepositoryPath is $activeToolchain, not the pinned $PinnedVersion" `
            -Fix 'Something is overriding rust-toolchain.toml. Clear the RUSTUP_TOOLCHAIN environment variable, and run "rustup override unset" in the repository root.'
    }

    New-CheckResult -Name $name -Status 'Pass' `
        -Detail "$activeToolchain, the toolchain pinned by rust-toolchain.toml"
}

function Test-RustComponents {
    <#
    .SYNOPSIS
        Checks that rustfmt and clippy can actually run.
    .DESCRIPTION
        rust-toolchain.toml asks for both, but a toolchain installed before that
        file existed will not have them, and the symptom is a confusing "no such
        subcommand" part way through a verification run.
    #>
    param([Parameter(Mandatory)] [string] $Cargo)

    $name = 'rustfmt and clippy'
    $fix = 'Run "rustup component add rustfmt clippy".'

    $missing = @()
    foreach ($component in @('fmt', 'clippy')) {
        $probe = Invoke-Probe -Command $Cargo -Arguments @($component, '--version')
        if (-not $probe.Found) {
            return New-CheckResult -Name $name -Status 'Fail' `
                -Detail "$Cargo is not on PATH" `
                -Fix 'Install rustup from https://rustup.rs; it provides cargo.'
        }
        if ($probe.ExitCode -ne 0) { $missing += "cargo $component" }
    }

    if ($missing.Count -gt 0) {
        return New-CheckResult -Name $name -Status 'Fail' `
            -Detail "$($missing -join ' and ') not available" -Fix $fix
    }

    New-CheckResult -Name $name -Status 'Pass' -Detail 'both components respond to --version'
}

function Test-Node {
    <#
    .SYNOPSIS
        Checks Node against the version pinned by .nvmrc.
    .DESCRIPTION
        A major version mismatch fails, because native modules and the Tauri
        tooling are built against a major. A lower patch on the right major is
        only a warning, since it still runs.
    #>
    param(
        [Parameter(Mandatory)] [string] $Node,
        [Parameter(Mandatory)] [string] $PinnedVersion,
        [Parameter(Mandatory)] [bool] $Required
    )

    $name = 'Node.js'
    $fix = "Install Node $PinnedVersion from https://nodejs.org, or run `"nvm install`" / `"fnm use`" in the repository root to pick up .nvmrc."

    $probe = Invoke-Probe -Command $Node -Arguments @('--version')
    if (-not $probe.Found) {
        $status = 'Warn'
        if ($Required) { $status = 'Fail' }
        return New-CheckResult -Name $name -Status $status `
            -Detail "$Node is not on PATH (pinned version is $PinnedVersion)" -Fix $fix
    }

    $installedVersion = ''
    if ($probe.Output -match 'v?(\d+\.\d+\.\d+)') { $installedVersion = $Matches[1] }
    if (-not $installedVersion) {
        return New-CheckResult -Name $name -Status 'Warn' `
            -Detail "could not read a version from '$($probe.Output)'" -Fix $fix
    }

    $installedMajor = [int] ($installedVersion.Split('.')[0])
    $pinnedMajor = [int] ($PinnedVersion.Split('.')[0])

    if ($installedMajor -ne $pinnedMajor) {
        $status = 'Warn'
        if ($Required) { $status = 'Fail' }
        return New-CheckResult -Name $name -Status $status `
            -Detail "Node $installedVersion is a different major version to the pinned $PinnedVersion" -Fix $fix
    }

    if ([version] $installedVersion -lt [version] $PinnedVersion) {
        return New-CheckResult -Name $name -Status 'Warn' `
            -Detail "Node $installedVersion is older than the pinned $PinnedVersion" -Fix $fix
    }

    New-CheckResult -Name $name -Status 'Pass' -Detail "Node $installedVersion (.nvmrc pins $PinnedVersion)"
}

function Get-GraphicsAdapter {
    <#
    .SYNOPSIS
        Lists display adapters, from WMI or from a JSON inventory file.
    .DESCRIPTION
        The inventory path exists for the tests. Adapter reporting and the
        stale-driver warning are exactly the behaviour that cannot be exercised
        against real hardware without the result depending on the machine and on
        today's date, so the tests describe the hardware instead.
    #>
    param([string] $InventoryPath = '')

    if (-not $InventoryPath) {
        return @(Get-CimInstance -ClassName Win32_VideoController -ErrorAction Stop)
    }

    if (-not (Test-Path -LiteralPath $InventoryPath)) {
        throw "no graphics adapter inventory at $InventoryPath"
    }

    $records = ConvertFrom-Json -InputObject (Get-Content -LiteralPath $InventoryPath -Raw)

    # As in the Visual Studio check: an empty JSON array arrives as one
    # property-less object, so records are selected by the property they must
    # have rather than counted.
    @($records | Where-Object { $null -ne $_ -and $_.PSObject.Properties.Name -contains 'Name' } |
            ForEach-Object {
                $driverDate = $null
                if ($_.PSObject.Properties.Name -contains 'DriverDate' -and $_.DriverDate) {
                    $driverDate = [datetime]::Parse($_.DriverDate, [cultureinfo]::InvariantCulture)
                }
                $driverVersion = ''
                if ($_.PSObject.Properties.Name -contains 'DriverVersion') { $driverVersion = $_.DriverVersion }

                [pscustomobject]@{
                    Name          = $_.Name
                    DriverVersion = $driverVersion
                    DriverDate    = $driverDate
                }
            })
}

function Test-GraphicsAdapter {
    <#
    .SYNOPSIS
        Reports the graphics adapters and the hardware encoder each implies.
    .DESCRIPTION
        Whether a specific codec is available cannot be answered from an adapter
        name - that needs the encoder itself, which the recorder queries at
        runtime - so this reports what is present and flags a driver old enough
        to be worth updating before blaming Clipped for an encoder failure.

        A stale driver and an absence of hardware encoding are independent
        facts, and a machine can have both, so both are reported. Falling back
        to software encoding costs game performance, which is the more
        consequential of the two and must not be hidden behind the driver
        advice.
    #>
    param(
        [Parameter(Mandatory)] [int] $MaximumAgeDays,
        [string] $InventoryPath = ''
    )

    $name = 'GPU and driver'

    $adapters = @()
    try {
        $adapters = @(Get-GraphicsAdapter -InventoryPath $InventoryPath)
    } catch {
        return New-CheckResult -Name $name -Status 'Warn' `
            -Detail "could not query display adapters: $($_.Exception.Message)" `
            -Fix 'Check the display adapters listed in Device Manager.'
    }

    if ($adapters.Count -eq 0) {
        return New-CheckResult -Name $name -Status 'Fail' `
            -Detail 'no display adapter reported by Windows' `
            -Fix 'Install a graphics driver from the GPU vendor. Clipped needs a display adapter to capture and encode.'
    }

    $described = @()
    $stale = @()
    $encoders = @()
    foreach ($adapter in $adapters) {
        $encoder = 'software encoding only'
        switch -Regex ($adapter.Name) {
            'NVIDIA' { $encoder = 'NVENC' }
            'AMD|Radeon|ATI' { $encoder = 'AMF' }
            'Intel' { $encoder = 'Quick Sync' }
        }
        $encoders += $encoder

        $age = ''
        if ($adapter.DriverDate) {
            $days = [int] ((Get-Date) - $adapter.DriverDate).TotalDays
            $age = ", driver dated $($adapter.DriverDate.ToString('yyyy-MM-dd'))"
            if ($days -gt $MaximumAgeDays) {
                $stale += "$($adapter.Name) ($days days old)"
            }
        }
        $described += "$($adapter.Name) [$encoder, $($adapter.DriverVersion)$age]"
    }

    $detail = $described -join '; '

    $notes = @()
    $fixes = @()

    # The pipeline is wrapped because a Where-Object that matches nothing
    # returns $null, and Set-StrictMode makes reading .Count on $null a
    # terminating error - which on a machine with no recognised encoder would
    # take the whole report down instead of warning about software encoding.
    $hardwareEncoders = @($encoders | Where-Object { $_ -ne 'software encoding only' })

    if ($hardwareEncoders.Count -eq 0) {
        $notes += 'no NVENC, AMF or Quick Sync capable adapter recognised'
        $fixes += 'Recording will fall back to software encoding, which costs game performance.'
    }

    if ($stale.Count -gt 0) {
        $notes += "driver older than $MaximumAgeDays days: $($stale -join ', ')"
        $fixes += 'Update the graphics driver from the GPU vendor before reporting hardware encoding problems.'
    }

    if ($notes.Count -eq 0) {
        return New-CheckResult -Name $name -Status 'Pass' -Detail $detail
    }

    New-CheckResult -Name $name -Status 'Warn' `
        -Detail "$detail - $($notes -join '; ')" -Fix ($fixes -join ' ')
}

function Test-Ffprobe {
    <#
    .SYNOPSIS
        Checks for ffprobe, which the media tests use to inspect recordings.
    #>
    param([Parameter(Mandatory)] [string] $Ffprobe)

    $name = 'ffprobe'
    $fix = 'Install FFmpeg (for example "winget install Gyan.FFmpeg") and make sure ffprobe is on PATH. Only the media tests need it.'

    $probe = Invoke-Probe -Command $Ffprobe -Arguments @('-version')
    if (-not $probe.Found) {
        return New-CheckResult -Name $name -Status 'Warn' `
            -Detail "$Ffprobe is not on PATH" -Fix $fix
    }

    $version = ($probe.Output -split "`n")[0].Trim()
    New-CheckResult -Name $name -Status 'Pass' -Detail $version
}

function Write-CheckLine {
    <#
    .SYNOPSIS
        Prints one aligned status line.
    #>
    param([Parameter(Mandatory)] [psobject] $Result)

    $label = @{ Pass = '[ OK ]'; Fail = '[FAIL]'; Warn = '[WARN]' }[$Result.Status]
    $colour = @{ Pass = 'Green'; Fail = 'Red'; Warn = 'Yellow' }[$Result.Status]

    Write-Host "$label " -ForegroundColor $colour -NoNewline
    Write-Host ("{0,-30} {1}" -f $Result.Name, $Result.Detail)
}

function Invoke-PrerequisiteCheck {
    <#
    .SYNOPSIS
        Runs one check, turning an unexpected error into a reported failure.
    .DESCRIPTION
        A check that throws must not take the whole report with it. This script
        exists to replace an obscure failure with a readable one, so a bug in a
        single check aborting the run before a single status line is printed is
        the worst outcome available to it. The remaining checks still run and
        the reader still gets a summary, with the broken check named.
    #>
    param(
        [Parameter(Mandatory)] [string] $Name,
        [Parameter(Mandatory)] [scriptblock] $Check
    )

    try {
        & $Check
    } catch {
        New-CheckResult -Name $Name -Status 'Fail' `
            -Detail "the check itself failed: $($_.Exception.Message)" `
            -Fix 'This is a bug in scripts/check-prerequisites.ps1 rather than a missing prerequisite. Please report it, quoting the message above.'
    }
}

$pinnedRust = Get-PinnedRustVersion -Path $RustToolchainFile
if (-not $pinnedRust) {
    Write-Host "Could not read the pinned Rust channel from $RustToolchainFile." -ForegroundColor Red
    exit 2
}

$pinnedNode = Get-PinnedNodeVersion -Path $NvmrcFile
if (-not $pinnedNode) {
    Write-Host "Could not read the pinned Node version from $NvmrcFile." -ForegroundColor Red
    exit 2
}

$nodeRequired = Test-Path -LiteralPath $DesktopManifest

Write-Host ''
Write-Host 'Clipped prerequisite check'
Write-Host ''

$results = @(
    (Invoke-PrerequisiteCheck -Name 'Windows version' -Check {
            Test-WindowsVersion -MinimumBuild $MinimumWindowsBuild }),
    (Invoke-PrerequisiteCheck -Name 'Visual Studio C++ build tools' -Check {
            Test-VisualStudioBuildTools -VsWhere $VsWherePath }),
    (Invoke-PrerequisiteCheck -Name 'Windows SDK' -Check {
            Test-WindowsSdk -RegistryPath $WindowsKitsRegistryPath }),
    (Invoke-PrerequisiteCheck -Name 'Rust toolchain' -Check {
            Test-RustToolchain -Rustup $RustupCommand -PinnedVersion $pinnedRust -RepositoryPath $repositoryRoot }),
    (Invoke-PrerequisiteCheck -Name 'rustfmt and clippy' -Check {
            Test-RustComponents -Cargo $CargoCommand }),
    (Invoke-PrerequisiteCheck -Name 'Node.js' -Check {
            Test-Node -Node $NodeCommand -PinnedVersion $pinnedNode -Required $nodeRequired }),
    (Invoke-PrerequisiteCheck -Name 'GPU and driver' -Check {
            Test-GraphicsAdapter -MaximumAgeDays $MaximumDriverAgeDays -InventoryPath $GraphicsAdapterInventory }),
    (Invoke-PrerequisiteCheck -Name 'ffprobe' -Check {
            Test-Ffprobe -Ffprobe $FfprobeCommand })
)

foreach ($result in $results) { Write-CheckLine -Result $result }

$failures = @($results | Where-Object { $_.Status -eq 'Fail' })
$warnings = @($results | Where-Object { $_.Status -eq 'Warn' })

Write-Host ''

if ($warnings.Count -gt 0) {
    Write-Host "$($warnings.Count) optional prerequisite(s) need attention:" -ForegroundColor Yellow
    foreach ($warning in $warnings) {
        Write-Host "  $($warning.Name): $($warning.Detail)"
        if ($warning.Fix) { Write-Host "    $($warning.Fix)" }
    }
    Write-Host ''
}

if ($failures.Count -eq 0) {
    Write-Host 'All required prerequisites are present.' -ForegroundColor Green
    exit 0
}

Write-Host "$($failures.Count) required prerequisite(s) are missing:" -ForegroundColor Red
foreach ($failure in $failures) {
    Write-Host "  $($failure.Name): $($failure.Detail)" -ForegroundColor Red
    if ($failure.Fix) { Write-Host "    $($failure.Fix)" }
}
Write-Host ''
Write-Host "Full instructions: $script:InstallGuide"
exit 1
