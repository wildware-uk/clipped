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
    that exists today; Node and the WebView2 runtime were both in that category
    until the desktop application landed, and are required now that it has.

.PARAMETER RustupCommand
    Name or path of the rustup executable.

.PARAMETER CargoCommand
    Name or path of the cargo executable, used to check that the rustfmt and
    clippy components are actually usable.

.PARAMETER NodeCommand
    Name or path of the node executable.

.PARAMETER FfprobeCommand
    Name or path of the ffprobe executable, used by the media tests.

.PARAMETER ClangCommand
    Name or path of the clang executable. Only its location is wanted: LLVM
    installs libclang.dll beside clang.exe, and libclang is what the FFmpeg
    binding's bindgen step loads.

.PARAMETER LibclangPath
    Directory holding libclang.dll, defaulting to the LIBCLANG_PATH environment
    variable that bindgen itself reads.

.PARAMETER LibclangSearchDirectory
    Further directories to look in for libclang.dll when neither LIBCLANG_PATH
    nor clang on PATH found it. These are the default LLVM install locations.

.PARAMETER CargoConfigFile
    Path to the workspace's .cargo/config.toml, whose [env] table is where the
    four FFmpeg variables come from on a machine where nobody has set them by
    hand. Defaults to the file in the repository root.

.PARAMETER FfmpegDir
    Root of the fetched FFmpeg build, defaulting to the FFMPEG_DIR environment
    variable. Left unset, the value is read from CargoConfigFile - the same
    order Cargo resolves it in, since an environment variable overrides an [env]
    entry.

.PARAMETER FfmpegIncludeDir
    FFmpeg header directory, defaulting to FFMPEG_INCLUDE_DIR and then to
    CargoConfigFile.

.PARAMETER FfmpegLibsDir
    FFmpeg import library directory, defaulting to FFMPEG_LIBS_DIR and then to
    CargoConfigFile.

.PARAMETER FfmpegLinkMode
    Link mode the FFmpeg binding is configured with, defaulting to
    FFMPEG_LINK_MODE and then to CargoConfigFile. Must be `dynamic`: see
    docs/adr/0004-ffmpeg-dependency-strategy.md.

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
    apps/desktop/package.json. Node and the WebView2 runtime are only required
    once this file exists; until then a missing one of either is a warning.

.PARAMETER WebView2RegistryPath
    Registry keys where an installed WebView2 runtime registers itself, tried in
    order. Three, because the runtime can be installed per machine or per user
    and a 32-bit Windows does not have the WOW6432Node redirection.

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
    [string] $ClangCommand = 'clang',
    [string] $LibclangPath = $env:LIBCLANG_PATH,
    [string[]] $LibclangSearchDirectory = @(
        'C:\Program Files\LLVM\bin',
        'C:\Program Files (x86)\LLVM\bin'
    ),
    [string] $CargoConfigFile = '',
    [string] $FfmpegDir = $env:FFMPEG_DIR,
    [string] $FfmpegIncludeDir = $env:FFMPEG_INCLUDE_DIR,
    [string] $FfmpegLibsDir = $env:FFMPEG_LIBS_DIR,
    [string] $FfmpegLinkMode = $env:FFMPEG_LINK_MODE,
    [string] $VsWherePath = (Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'),
    [string] $WindowsKitsRegistryPath = 'HKLM:\SOFTWARE\Microsoft\Windows Kits\Installed Roots',
    # {F3017226-...} is the Evergreen runtime's fixed product code, which is how
    # Microsoft's own documented detection finds it.
    [string[]] $WebView2RegistryPath = @(
        'HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
        'HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
        'HKCU:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
    ),
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
if (-not $CargoConfigFile) { $CargoConfigFile = Join-Path $repositoryRoot '.cargo\config.toml' }

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

function Test-WebView2 {
    <#
    .SYNOPSIS
        Checks that the Evergreen WebView2 runtime is installed.
    .DESCRIPTION
        The desktop application is a Tauri window, which is a WebView2 host.
        Without the runtime it does not open: apps/desktop/src-tauri/src/main.rs
        panics with that sentence, because by then there is no interface left to
        report it in. Saying so here instead is the difference between a missing
        prerequisite and a crash several minutes into a first "npm run dev".

        Windows 11 ships it and Windows Update pushes it to Windows 10, so this
        usually passes without anybody installing anything - which is exactly why
        it is worth checking rather than assuming.

        Three keys are tried because the runtime registers under a different one
        depending on how it was installed: per machine on 64-bit Windows, per
        machine on 32-bit Windows, or per user. An uninstalled runtime can leave
        its key behind with a `pv` of 0.0.0.0, which Microsoft's own detection
        guidance treats as absent, so this does too.
    #>
    param(
        [Parameter(Mandatory)] [string[]] $RegistryPath,
        [Parameter(Mandatory)] [bool] $Required
    )

    $name = 'WebView2 runtime'
    $fix = 'Install the Evergreen WebView2 Runtime from https://developer.microsoft.com/microsoft-edge/webview2/. Windows 11 ships it, so a machine without it is unusual.'

    foreach ($path in $RegistryPath) {
        if (-not (Test-Path -LiteralPath $path)) { continue }

        $version = ''
        try {
            $version = [string] (Get-ItemProperty -LiteralPath $path).pv
        } catch {
            $version = ''
        }

        if ($version -and $version -ne '0.0.0.0') {
            return New-CheckResult -Name $name -Status 'Pass' -Detail "$version in $path"
        }
    }

    $status = 'Warn'
    if ($Required) { $status = 'Fail' }
    New-CheckResult -Name $name -Status $status `
        -Detail 'no runtime registered under any of the WebView2 keys' -Fix $fix
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

function Test-Libclang {
    <#
    .SYNOPSIS
        Checks for the libclang.dll that the FFmpeg binding's bindgen step loads.
    .DESCRIPTION
        Clipped contains no C or C++, but crates/muxer links FFmpeg through
        `rusty_ffmpeg`, which generates its FFI from FFmpeg's own headers while
        the workspace builds. That is bindgen, and bindgen loads libclang.dll at
        run time. Without it, `cargo build --workspace` fails several minutes in
        with "Unable to find libclang", which names neither FFmpeg nor LLVM.

        Three places are looked at, in the order bindgen itself would resolve
        them: LIBCLANG_PATH, the directory clang.exe is in - LLVM ships the DLL
        beside the executable - and the default install locations. The first hit
        is reported, because that is the one that will be used.
    .PARAMETER Clang
        Name or path of clang.exe, used only to locate its directory.
    .PARAMETER ConfiguredPath
        Value of LIBCLANG_PATH, which may name the DLL itself or its directory.
    .PARAMETER SearchDirectory
        Fallback directories to look in.
    #>
    param(
        [Parameter(Mandatory)] [string] $Clang,
        [string] $ConfiguredPath = '',
        [string[]] $SearchDirectory = @()
    )

    $name = 'LLVM (libclang)'
    $fix = 'Run "winget install LLVM.LLVM". If libclang.dll lives somewhere else - inside another toolchain, say - set LIBCLANG_PATH to the directory containing it.'

    # LIBCLANG_PATH is documented as a directory but bindgen also accepts the
    # file, so a contributor who set it either way is told the truth about what
    # they have rather than being sent to reinstall LLVM.
    if ($ConfiguredPath) {
        if ((Test-Path -LiteralPath $ConfiguredPath -PathType Leaf) -and
            ([System.IO.Path]::GetFileName($ConfiguredPath) -ieq 'libclang.dll')) {
            return New-CheckResult -Name $name -Status 'Pass' `
                -Detail "$ConfiguredPath (LIBCLANG_PATH)"
        }

        $configuredDll = Join-Path $ConfiguredPath 'libclang.dll'
        if (Test-Path -LiteralPath $configuredDll -PathType Leaf) {
            return New-CheckResult -Name $name -Status 'Pass' `
                -Detail "$configuredDll (LIBCLANG_PATH)"
        }

        return New-CheckResult -Name $name -Status 'Fail' `
            -Detail "LIBCLANG_PATH is set to $ConfiguredPath, which holds no libclang.dll" `
            -Fix $fix
    }

    $candidates = @()
    $clangProbe = Invoke-Probe -Command $Clang -Arguments @('--version')
    if ($clangProbe.Found) { $candidates += (Split-Path -Parent $clangProbe.Path) }
    $candidates += $SearchDirectory

    foreach ($directory in $candidates) {
        if (-not $directory) { continue }
        $dll = Join-Path $directory 'libclang.dll'
        if (Test-Path -LiteralPath $dll -PathType Leaf) {
            return New-CheckResult -Name $name -Status 'Pass' -Detail $dll
        }
    }

    $looked = @($candidates | Where-Object { $_ }) -join ', '
    if (-not $looked) { $looked = 'nowhere - clang is not on PATH and no search directory was given' }

    New-CheckResult -Name $name -Status 'Fail' `
        -Detail "no libclang.dll found (looked in $looked)" -Fix $fix
}

function Get-CargoConfiguredEnvironment {
    <#
    .SYNOPSIS
        Reads the [env] table out of a Cargo configuration file.
    .DESCRIPTION
        The FFmpeg variables live in the workspace's .cargo/config.toml, which
        is what lets `cargo build` work in the shell the fetch script was run
        from. This check has to agree with Cargo about their values, so it reads
        the same file rather than repeating the paths.

        Only the two shapes that file uses are understood - a bare string, and
        an inline table with a `value` and an optional `relative` - because
        anything else in it would be a change somebody made deliberately, and
        silently guessing at it would be worse than reporting nothing. A
        `relative` value is resolved against the directory holding `.cargo`,
        exactly as Cargo resolves it, so what comes back is comparable with what
        the build will see.

        Returns an empty hashtable when the file is absent or has no [env]
        table; the caller reports that as the missing prerequisite it is.
    .PARAMETER Path
        The configuration file to read.
    #>
    param([Parameter(Mandatory)] [string] $Path)

    $values = @{}
    if (-not (Test-Path -LiteralPath $Path)) { return $values }

    $configurationRoot = Split-Path -Parent (Split-Path -Parent $Path)
    $inEnvironmentTable = $false

    foreach ($line in (Get-Content -LiteralPath $Path)) {
        $trimmed = $line.Trim()
        if ($trimmed -match '^\[(.+)\]$') {
            $inEnvironmentTable = $Matches[1] -eq 'env'
            continue
        }
        if (-not $inEnvironmentTable) { continue }
        if ($trimmed -notmatch '^([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.+)$') { continue }

        $variable = $Matches[1]
        $assigned = $Matches[2]

        if ($assigned -match '^"([^"]*)"') {
            $values[$variable] = $Matches[1]
            continue
        }
        if ($assigned -match 'value\s*=\s*"([^"]*)"') {
            $value = $Matches[1]
            if ($assigned -match 'relative\s*=\s*true') {
                $value = Join-Path $configurationRoot $value
            }
            $values[$variable] = $value
        }
    }

    $values
}

function Test-FfmpegBuild {
    <#
    .SYNOPSIS
        Checks that the pinned FFmpeg build has been fetched and is linkable.
    .DESCRIPTION
        crates/muxer links against a prebuilt FFmpeg that
        scripts/fetch-ffmpeg.ps1 downloads
        (docs/adr/0004-ffmpeg-dependency-strategy.md). Four variables tell the
        build where it is and how to link it: three the binding reads, and
        FFMPEG_DIR, which is Clipped's own and is read by
        crates/muxer/build.rs. They are set by the workspace's
        .cargo/config.toml, and an environment variable of the same name
        overrides that file - so this check resolves them in that order too.

        Not fetching the build is the single most likely reason a clean clone
        fails to build, and what it fails with is
        "!!!!!!! rusty_ffmpeg: No linking method set!", or a missing header,
        from inside a dependency's build script that names nothing anybody can
        run.

        The directories are checked for the files that are actually needed, not
        merely for existing: a half-deleted build, or a variable left pointing
        at a build that has since been removed, is a state worth telling
        someone about before the linker does.

        FFMPEG_LINK_MODE is checked as strictly as the rest because it is not a
        build detail. `dynamic` is how Clipped satisfies the LGPL's relinking
        requirement; the binding's default is static, which would quietly change
        the licence position of every binary produced from that machine.
    .PARAMETER CargoConfig
        Path to the .cargo/config.toml the four values come from when the
        environment does not override them.
    .PARAMETER Prefix
        FFMPEG_DIR from the environment, if it is set there.
    .PARAMETER IncludeDirectory
        FFMPEG_INCLUDE_DIR from the environment, if it is set there.
    .PARAMETER LibrariesDirectory
        FFMPEG_LIBS_DIR from the environment, if it is set there.
    .PARAMETER LinkMode
        FFMPEG_LINK_MODE from the environment, if it is set there.
    #>
    param(
        [Parameter(Mandatory)] [string] $CargoConfig,
        [string] $Prefix = '',
        [string] $IncludeDirectory = '',
        [string] $LibrariesDirectory = '',
        [string] $LinkMode = ''
    )

    $name = 'FFmpeg libraries'
    $fix = 'Run "powershell -ExecutionPolicy Bypass -File scripts/fetch-ffmpeg.ps1" from the repository root. See docs/ffmpeg.md.'

    $configured = Get-CargoConfiguredEnvironment -Path $CargoConfig
    $overridden = @()
    foreach ($pair in @(
            @{ Variable = 'FFMPEG_DIR'; Value = $Prefix },
            @{ Variable = 'FFMPEG_INCLUDE_DIR'; Value = $IncludeDirectory },
            @{ Variable = 'FFMPEG_LIBS_DIR'; Value = $LibrariesDirectory },
            @{ Variable = 'FFMPEG_LINK_MODE'; Value = $LinkMode })) {
        if ($pair.Value) {
            $configured[$pair.Variable] = $pair.Value
            $overridden += $pair.Variable
        }
    }

    # An override says where the reader wants the build found; the file is the
    # only thing that makes an un-overridden build work at all, so its absence
    # is reported as itself rather than as a missing directory.
    $unset = @('FFMPEG_DIR', 'FFMPEG_INCLUDE_DIR', 'FFMPEG_LIBS_DIR', 'FFMPEG_LINK_MODE') |
        Where-Object { -not $configured.ContainsKey($_) }
    if ($unset) {
        return New-CheckResult -Name $name -Status 'Fail' `
            -Detail "$CargoConfig does not set $($unset -join ', ')" `
            -Fix 'That file is what points Cargo at the fetched FFmpeg, and it is committed to the repository. Restore it ("git checkout -- .cargo/config.toml") or set the four variables in your shell. See docs/ffmpeg.md.'
    }

    # One header, one import library and the runtime library directory: enough
    # to tell a fetched build from a path pointing at nothing, without restating
    # the fetch script's own layout check.
    $required = [ordered]@{
        "$($configured.FFMPEG_INCLUDE_DIR)\libavformat\avformat.h" = 'FFMPEG_INCLUDE_DIR'
        "$($configured.FFMPEG_LIBS_DIR)\avformat.lib"              = 'FFMPEG_LIBS_DIR'
        "$($configured.FFMPEG_DIR)\bin"                            = 'FFMPEG_DIR'
    }

    $missing = @()
    foreach ($path in $required.Keys) {
        if (-not (Test-Path -LiteralPath $path)) { $missing += "$path ($($required[$path]))" }
    }

    if ($missing.Count -gt 0) {
        $source = 'the pinned FFmpeg build has not been fetched'
        if ($overridden) {
            $source = "the FFmpeg variables set in this shell ($($overridden -join ', ')) point at a build that is not there"
        }
        return New-CheckResult -Name $name -Status 'Fail' `
            -Detail "$($source): $($missing -join '; ')" -Fix $fix
    }

    if ($configured.FFMPEG_LINK_MODE -ne 'dynamic') {
        return New-CheckResult -Name $name -Status 'Fail' `
            -Detail "FFMPEG_LINK_MODE is '$($configured.FFMPEG_LINK_MODE)', not 'dynamic'" `
            -Fix 'Clipped links FFmpeg dynamically to satisfy the LGPL (docs/adr/0004-ffmpeg-dependency-strategy.md). Unset FFMPEG_LINK_MODE in this shell and the workspace .cargo/config.toml sets it correctly.'
    }

    # The build directory has a fixed name, so what is in it is a question for
    # the pin record the fetch script left there.
    $installed = ''
    $pin = Join-Path $configured.FFMPEG_DIR '.clipped-ffmpeg-pin.json'
    if (Test-Path -LiteralPath $pin) {
        try { $installed = (Get-Content -LiteralPath $pin -Raw | ConvertFrom-Json).asset } catch { $installed = '' }
    }
    if (-not $installed) { $installed = Split-Path -Leaf $configured.FFMPEG_DIR }

    New-CheckResult -Name $name -Status 'Pass' -Detail "$installed, linked dynamically"
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

# Node and the WebView2 runtime are both needed to build and run the desktop
# application and neither is needed for anything else, so the same fact decides
# whether each is required. Testing for the manifest rather than hard-coding
# "yes" is what let that promotion happen on its own the day the shell landed.
$desktopApplicationExists = Test-Path -LiteralPath $DesktopManifest

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
    (Invoke-PrerequisiteCheck -Name 'LLVM (libclang)' -Check {
            Test-Libclang -Clang $ClangCommand -ConfiguredPath $LibclangPath `
                -SearchDirectory $LibclangSearchDirectory }),
    (Invoke-PrerequisiteCheck -Name 'FFmpeg libraries' -Check {
            Test-FfmpegBuild -CargoConfig $CargoConfigFile -Prefix $FfmpegDir `
                -IncludeDirectory $FfmpegIncludeDir -LibrariesDirectory $FfmpegLibsDir `
                -LinkMode $FfmpegLinkMode }),
    (Invoke-PrerequisiteCheck -Name 'Node.js' -Check {
            Test-Node -Node $NodeCommand -PinnedVersion $pinnedNode -Required $desktopApplicationExists }),
    (Invoke-PrerequisiteCheck -Name 'WebView2 runtime' -Check {
            Test-WebView2 -RegistryPath $WebView2RegistryPath -Required $desktopApplicationExists }),
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
