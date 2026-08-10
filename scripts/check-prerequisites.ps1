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
    default, so the missing-dependency path can be exercised for real (by
    pointing a probe at something that genuinely is not there) rather than
    simulated. scripts/test-check-prerequisites.ps1 does exactly that.

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
# oldest Windows 10 release Microsoft still services and it comfortably
# includes the Windows Graphics Capture behaviour (build 19041 and later) the
# capture backend depends on.

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
    #>
    param(
        [Parameter(Mandatory)] [string] $Command,
        [string[]] $Arguments = @()
    )

    $resolved = Get-Command -Name $Command -CommandType Application -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if (-not $resolved) {
        return [pscustomobject]@{ Found = $false; ExitCode = $null; Output = ''; Path = '' }
    }

    # Native tools report version information on either stream, and Windows
    # PowerShell turns redirected native stderr into error records, so failures
    # are collected here rather than being allowed to terminate the run.
    $ErrorActionPreference = 'Continue'
    $global:LASTEXITCODE = 0
    $output = & $resolved.Source @Arguments 2>&1 | Out-String

    [pscustomobject]@{
        Found    = $true
        ExitCode = $LASTEXITCODE
        Output   = $output.Trim()
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

    $installations = @()
    if ($probe.Output) {
        try {
            $installations = @($probe.Output | ConvertFrom-Json)
        } catch {
            return New-CheckResult -Name $name -Status 'Fail' `
                -Detail "could not read the Visual Studio installation list: $($_.Exception.Message)" -Fix $fix
        }
    }

    if ($installations.Count -eq 0) {
        return New-CheckResult -Name $name -Status 'Fail' `
            -Detail 'Visual Studio is installed but no instance has the MSVC x64 C++ toolset' -Fix $fix
    }

    $installation = $installations[0]
    New-CheckResult -Name $name -Status 'Pass' `
        -Detail "$($installation.displayName) $($installation.installationVersion)"
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
        Checks that rustup is present and serving the pinned compiler.
    .DESCRIPTION
        rustc is invoked through the rustup shim from the repository, so this
        reports the version the build will actually use, pin included, rather
        than whichever toolchain happens to be the default.
    #>
    param(
        [Parameter(Mandatory)] [string] $Rustup,
        [Parameter(Mandatory)] [string] $PinnedVersion
    )

    $name = 'Rust toolchain'
    $fix = 'Install rustup from https://rustup.rs, then run "rustup toolchain install" from the repository root to fetch the pinned toolchain.'

    $rustupProbe = Invoke-Probe -Command $Rustup -Arguments @('--version')
    if (-not $rustupProbe.Found) {
        return New-CheckResult -Name $name -Status 'Fail' `
            -Detail "$Rustup is not on PATH" -Fix $fix
    }

    $rustcProbe = Invoke-Probe -Command $Rustup -Arguments @('run', $PinnedVersion, 'rustc', '--version')
    if ($rustcProbe.ExitCode -ne 0) {
        return New-CheckResult -Name $name -Status 'Fail' `
            -Detail "rustup cannot run the pinned toolchain $PinnedVersion`: $($rustcProbe.Output)" `
            -Fix "Run `"rustup toolchain install $PinnedVersion`"."
    }

    $installedVersion = ''
    if ($rustcProbe.Output -match 'rustc\s+(\d+\.\d+\.\d+)') {
        $installedVersion = $Matches[1]
    }

    if ($installedVersion -ne $PinnedVersion) {
        return New-CheckResult -Name $name -Status 'Fail' `
            -Detail "rustup reports '$($rustcProbe.Output)' for the pinned channel $PinnedVersion" `
            -Fix "Run `"rustup toolchain install $PinnedVersion`" and confirm rust-toolchain.toml is intact."
    }

    New-CheckResult -Name $name -Status 'Pass' -Detail "rustc $installedVersion (pinned by rust-toolchain.toml)"
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

function Test-GraphicsAdapter {
    <#
    .SYNOPSIS
        Reports the graphics adapters and the hardware encoder each implies.
    .DESCRIPTION
        Whether a specific codec is available cannot be answered from an adapter
        name - that needs the encoder itself, which the recorder queries at
        runtime - so this reports what is present and flags a driver old enough
        to be worth updating before blaming Clipped for an encoder failure.
    #>
    param([Parameter(Mandatory)] [int] $MaximumAgeDays)

    $name = 'GPU and driver'

    $adapters = @()
    try {
        $adapters = @(Get-CimInstance -ClassName Win32_VideoController -ErrorAction Stop)
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

    if ($stale.Count -gt 0) {
        return New-CheckResult -Name $name -Status 'Warn' `
            -Detail "$detail - driver older than $MaximumAgeDays days: $($stale -join ', ')" `
            -Fix 'Update the graphics driver from the GPU vendor before reporting hardware encoding problems.'
    }

    if (($encoders | Where-Object { $_ -ne 'software encoding only' }).Count -eq 0) {
        return New-CheckResult -Name $name -Status 'Warn' `
            -Detail "$detail - no NVENC, AMF or Quick Sync capable adapter recognised" `
            -Fix 'Recording will fall back to software encoding, which costs game performance.'
    }

    New-CheckResult -Name $name -Status 'Pass' -Detail $detail
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
    (Test-WindowsVersion -MinimumBuild $MinimumWindowsBuild),
    (Test-VisualStudioBuildTools -VsWhere $VsWherePath),
    (Test-WindowsSdk -RegistryPath $WindowsKitsRegistryPath),
    (Test-RustToolchain -Rustup $RustupCommand -PinnedVersion $pinnedRust),
    (Test-RustComponents -Cargo $CargoCommand),
    (Test-Node -Node $NodeCommand -PinnedVersion $pinnedNode -Required $nodeRequired),
    (Test-GraphicsAdapter -MaximumAgeDays $MaximumDriverAgeDays),
    (Test-Ffprobe -Ffprobe $FfprobeCommand)
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
