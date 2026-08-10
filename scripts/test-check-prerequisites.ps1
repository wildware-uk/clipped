#Requires -Version 5.1

<#
.SYNOPSIS
    Tests scripts/check-prerequisites.ps1, including its missing-dependency and
    its present-but-wrong paths.

.DESCRIPTION
    The value of the prerequisite check is what it does when something is wrong,
    and that is the case nobody exercises by running it on a working machine.
    Each case here runs the real script as a child process and asserts on its
    exit code and on the text a contributor would read.

    Every case is driven entirely by fixtures: stand-in commands, fixture pin
    files, a registry key under HKCU, a fixture FFmpeg tree, and a JSON
    description of the display adapters. Nothing is read from the machine the
    tests happen to run on, so a case that fails is a defect in the check rather
    than a statement about the developer's toolchain (AGENTS.md section 25).
    That matters most on a clean CI runner, where an outcome that depends on
    installed toolchains or on real hardware would fail for reasons that have
    nothing to do with the change.

    Some of the check's parameters default to environment variables rather than
    to a command name, and a case that wants one of those absent expresses it by
    omitting the parameter. Windows PowerShell drops an empty string argument on
    its way to a native command, so passing "" would not mean what it looks
    like. The variables are therefore cleared in this process before any case
    runs, and restored afterwards, so what a child process inherits is decided
    here and not by whoever is running the suite.

    Two shapes of wrongness are covered, because they are detected by different
    code and only one of them is easy:

    - absence, produced by pointing a probe at a command, path or registry key
      that genuinely does not exist;
    - present but wrong, produced by pointing a probe at a stand-in that answers
      the way the real tool answers in that state - vswhere printing "[]" for a
      Visual Studio install with no C++ workload, node reporting a different
      major version, rustup refusing a toolchain that is not installed, rustup
      reporting a toolchain that overrides the pin.

    The parsing and reporting under test are the production code in both cases.

    Written as a plain script rather than as Pester tests because the only
    Pester on a stock Windows install is 3.4.0, whose syntax is incompatible
    with the Pester 5 a contributor would install; requiring one or the other
    would trade a missing-dependency error in the checker for a missing-
    dependency error in the checker's tests.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts/test-check-prerequisites.ps1

.OUTPUTS
    Exit code 0 when every case passes, 1 otherwise.
#>

[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$checkScript = Join-Path $PSScriptRoot 'check-prerequisites.ps1'
$failureCount = 0

$fixtureRoot = Join-Path ([System.IO.Path]::GetTempPath()) "clipped-prerequisite-fixtures-$PID"
$fixtureRegistryRoot = 'HKCU:\SOFTWARE\ClippedPrerequisiteCheckTests'

# Fictional versions, so that a stand-in genuinely has to answer for a case to
# pass. If a fixture were pinned to whatever this machine has installed, a case
# could pass by accident against the real tool.
$fixtureRustPin = '1.42.0'
$fixtureNodePin = '20.11.1'

# Cleared for the duration of the suite; see the description above.
$inheritedEnvironment = [ordered]@{}
foreach ($variable in @('LIBCLANG_PATH', 'FFMPEG_DIR', 'FFMPEG_INCLUDE_DIR', 'FFMPEG_LIBS_DIR', 'FFMPEG_LINK_MODE')) {
    $inheritedEnvironment[$variable] = [Environment]::GetEnvironmentVariable($variable)
    Remove-Item -LiteralPath "env:$variable" -ErrorAction SilentlyContinue
}

function New-Fixture {
    <#
    .SYNOPSIS
        Writes a fixture file under the fixture root and returns its path.
    #>
    param(
        [Parameter(Mandatory)] [string] $Name,
        [Parameter(Mandatory)] [string] $Content
    )

    $path = Join-Path $fixtureRoot $Name
    Set-Content -LiteralPath $path -Value $Content -Encoding Ascii
    $path
}

function New-StubCommand {
    <#
    .SYNOPSIS
        Writes a stand-in executable that answers the way a real tool would.
    .DESCRIPTION
        A .cmd file, because the check resolves probes with
        "Get-Command -CommandType Application" specifically so that a PowerShell
        function cannot stand in for a real tool. Honouring that here keeps the
        test on the same path as production: the check really does spawn a
        process, capture both its streams and read its exit code.
    #>
    param(
        [Parameter(Mandatory)] [string] $Name,
        [Parameter(Mandatory)] [string[]] $Lines,
        [string] $Directory = ''
    )

    if (-not $Directory) { $Directory = $fixtureRoot }
    $path = Join-Path $Directory "$Name.cmd"
    Set-Content -LiteralPath $path -Value (@('@echo off') + $Lines) -Encoding Ascii
    $path
}

function Invoke-Check {
    <#
    .SYNOPSIS
        Runs the prerequisite check in a child process and captures its report.
    #>
    param([string[]] $Arguments = @())

    $ErrorActionPreference = 'Continue'
    $output = & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $checkScript @Arguments 2>&1 | Out-String

    [pscustomobject]@{
        ExitCode = $LASTEXITCODE
        Output   = $output
    }
}

function Assert-Case {
    <#
    .SYNOPSIS
        Asserts an exit code and the presence or absence of report text.
    .PARAMETER ExpectedText
        Substrings that must all appear in the output. These are the sentences a
        contributor relies on, so they are part of the contract, not decoration.
    .PARAMETER UnexpectedText
        Substrings that must not appear. Used where the defect is noise rather
        than a missing fact.
    #>
    param(
        [Parameter(Mandatory)] [string] $Name,
        [Parameter(Mandatory)] [int] $ExpectedExitCode,
        [Parameter(Mandatory)] [string[]] $ExpectedText,
        [string[]] $UnexpectedText = @(),
        [string[]] $Arguments = @()
    )

    $result = Invoke-Check -Arguments $Arguments
    $problems = @()

    if ($result.ExitCode -ne $ExpectedExitCode) {
        $problems += "expected exit code $ExpectedExitCode but got $($result.ExitCode)"
    }

    # Plain substring matching, not -like: the status labels contain square
    # brackets, which -like would treat as a character class.
    foreach ($expected in $ExpectedText) {
        if (-not $result.Output.Contains($expected)) {
            $problems += "expected the output to contain '$expected'"
        }
    }

    foreach ($unexpected in $UnexpectedText) {
        if ($result.Output.Contains($unexpected)) {
            $problems += "expected the output not to contain '$unexpected'"
        }
    }

    if ($problems.Count -eq 0) {
        Write-Host "PASS  $Name" -ForegroundColor Green
        return
    }

    $script:failureCount++
    Write-Host "FAIL  $Name" -ForegroundColor Red
    foreach ($problem in $problems) { Write-Host "        $problem" -ForegroundColor Red }
    Write-Host '      --- output ---'
    Write-Host $result.Output
    Write-Host '      --------------'
}

try {
    New-Item -ItemType Directory -Path $fixtureRoot -Force | Out-Null

    # --- pins, manifests and paths -----------------------------------------

    $pinFile = New-Fixture -Name 'rust-toolchain.toml' -Content @"
[toolchain]
channel = "$fixtureRustPin"
"@
    $nvmrcFile = New-Fixture -Name '.nvmrc' -Content $fixtureNodePin
    $desktopManifest = New-Fixture -Name 'package.json' -Content '{ "name": "clipped-desktop" }'

    $absentPath = Join-Path $fixtureRoot 'no-such-directory\no-such-file'

    # --- a Windows SDK registration, under HKCU so no elevation is needed ---

    $sdkRoot = Join-Path $fixtureRoot 'WindowsKits\10\'
    New-Item -ItemType Directory -Path (Join-Path $sdkRoot 'Include\10.0.26100.0') -Force | Out-Null

    $sdkRegistryPath = Join-Path $fixtureRegistryRoot 'Installed Roots'
    New-Item -Path $sdkRegistryPath -Force | Out-Null
    New-ItemProperty -LiteralPath $sdkRegistryPath -Name 'KitsRoot10' -Value $sdkRoot `
        -PropertyType String -Force | Out-Null

    # --- an LLVM install, and an FFmpeg build that has been fetched ---------

    # Only the file's name and location matter to the check: it reports where
    # bindgen would load libclang.dll from, and never opens it.
    $llvmDirectory = Join-Path $fixtureRoot 'LLVM\bin'
    New-Item -ItemType Directory -Path $llvmDirectory -Force | Out-Null
    Set-Content -LiteralPath (Join-Path $llvmDirectory 'libclang.dll') `
        -Value 'not a real DLL' -Encoding Ascii

    # A clang that answers from the same directory as the DLL, which is how a
    # real LLVM install is laid out and the path the check takes when
    # LIBCLANG_PATH is not set.
    $clangBesideLibclang = New-StubCommand -Name 'clipped-stub-clang' -Directory $llvmDirectory -Lines @(
        'echo clang version 20.1.8',
        'exit /b 0'
    )

    # The shape scripts/fetch-ffmpeg.ps1 leaves behind: a prefix with bin,
    # include and lib. The files are empty because the check only asks whether
    # the build is there, never what is in it.
    $ffmpegPrefix = Join-Path $fixtureRoot 'ffmpeg-n0.0-fixture-win64-lgpl-shared'
    New-Item -ItemType Directory -Path (Join-Path $ffmpegPrefix 'bin') -Force | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $ffmpegPrefix 'include\libavformat') -Force | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $ffmpegPrefix 'lib') -Force | Out-Null
    Set-Content -LiteralPath (Join-Path $ffmpegPrefix 'include\libavformat\avformat.h') `
        -Value '/* fixture */' -Encoding Ascii
    Set-Content -LiteralPath (Join-Path $ffmpegPrefix 'lib\avformat.lib') `
        -Value 'fixture' -Encoding Ascii

    # --- display adapters ---------------------------------------------------

    # Driver dates are written relative to now on purpose. A fixed date would
    # make "current driver" mean something different every year; an offset makes
    # both outcomes independent of when the suite runs.
    $currentDriverDate = (Get-Date).AddDays(-30).ToString('yyyy-MM-ddTHH:mm:ss')
    $staleDriverDate = (Get-Date).AddDays(-1200).ToString('yyyy-MM-ddTHH:mm:ss')

    $currentAdapters = New-Fixture -Name 'adapters-current.json' -Content @"
[{ "Name": "NVIDIA GeForce RTX 4090", "DriverVersion": "32.0.16.1074", "DriverDate": "$currentDriverDate" }]
"@
    $staleAdapters = New-Fixture -Name 'adapters-stale.json' -Content @"
[{ "Name": "NVIDIA GeForce GTX 1060", "DriverVersion": "27.21.14.5671", "DriverDate": "$staleDriverDate" }]
"@
    $staleSoftwareAdapters = New-Fixture -Name 'adapters-stale-software.json' -Content @"
[{ "Name": "Microsoft Basic Display Adapter", "DriverVersion": "10.0.19041.1", "DriverDate": "$staleDriverDate" }]
"@

    # --- stand-in commands --------------------------------------------------

    $vswherePresentJson = New-Fixture -Name 'vswhere-present.json' `
        -Content '[{ "instanceId": "fixture", "displayName": "Visual Studio Build Tools 2022", "installationVersion": "17.14.37411.7" }]'

    $vswherePresent = New-StubCommand -Name 'clipped-stub-vswhere-present' -Lines @(
        "type `"$vswherePresentJson`""
    )

    # Real vswhere prints an empty JSON array, and exits 0, when no installation
    # satisfies -requires. That is the state of a machine with Visual Studio but
    # without the "Desktop development with C++" workload.
    $vswhereNoCxx = New-StubCommand -Name 'clipped-stub-vswhere-no-cxx' -Lines @(
        'echo []'
    )

    $rustupSatisfied = New-StubCommand -Name 'clipped-stub-rustup' -Lines @(
        'if "%1"=="--version" goto version',
        'if "%1"=="show" goto active',
        "echo rustc $fixtureRustPin (0000000000 2026-01-01)",
        'exit /b 0',
        ':version',
        'echo rustup 1.28.2 (0000000000 2026-01-01)',
        'exit /b 0',
        ':active',
        "echo $fixtureRustPin-x86_64-pc-windows-msvc (overridden by 'C:\clipped\rust-toolchain.toml')",
        'exit /b 0'
    )

    $rustupPinMissing = New-StubCommand -Name 'clipped-stub-rustup-pin-missing' -Lines @(
        'if "%1"=="--version" goto version',
        "echo error: toolchain '$fixtureRustPin-x86_64-pc-windows-msvc' is not installed 1>&2",
        'exit /b 1',
        ':version',
        'echo rustup 1.28.2 (0000000000 2026-01-01)',
        'exit /b 0'
    )

    # RUSTUP_TOOLCHAIN and a directory override both beat rust-toolchain.toml,
    # and neither is visible in the file.
    $rustupOverridden = New-StubCommand -Name 'clipped-stub-rustup-overridden' -Lines @(
        'if "%1"=="--version" goto version',
        'if "%1"=="show" goto active',
        "echo rustc $fixtureRustPin (0000000000 2026-01-01)",
        'exit /b 0',
        ':version',
        'echo rustup 1.28.2 (0000000000 2026-01-01)',
        'exit /b 0',
        ':active',
        'echo 1.70.0-x86_64-pc-windows-msvc (overridden by environment variable RUSTUP_TOOLCHAIN)',
        'exit /b 0'
    )

    $cargoSatisfied = New-StubCommand -Name 'clipped-stub-cargo' -Lines @(
        "echo %1 $fixtureRustPin",
        'exit /b 0'
    )

    $cargoWithoutRustfmt = New-StubCommand -Name 'clipped-stub-cargo-no-rustfmt' -Lines @(
        'if "%1"=="fmt" goto missing',
        "echo clippy 0.1.42",
        'exit /b 0',
        ':missing',
        'echo error: no such command: fmt 1>&2',
        'exit /b 101'
    )

    $nodePinned = New-StubCommand -Name 'clipped-stub-node' -Lines @(
        "echo v$fixtureNodePin",
        'exit /b 0'
    )

    $nodeWrongMajor = New-StubCommand -Name 'clipped-stub-node-wrong-major' -Lines @(
        'echo v18.20.4',
        'exit /b 0'
    )

    $ffprobePresent = New-StubCommand -Name 'clipped-stub-ffprobe' -Lines @(
        'echo ffprobe version 7.1-fixture',
        'exit /b 0'
    )

    function New-Arguments {
        <#
        .SYNOPSIS
            Builds a full fixture argument list, with named overrides applied.
        .DESCRIPTION
            Every case passes every parameter, so a case only differs from a
            satisfied machine in the one thing it is about. Leaving a parameter
            at its default would let the machine decide part of the outcome.

            An override of $null omits the parameter instead of passing it,
            which is how a case expresses "this environment variable is not
            set"; the variables themselves were cleared before any case ran.
        #>
        param([hashtable] $Override = @{})

        $settings = [ordered]@{
            RustupCommand            = $rustupSatisfied
            CargoCommand             = $cargoSatisfied
            NodeCommand              = $nodePinned
            FfprobeCommand           = $ffprobePresent
            # LIBCLANG_PATH is the only thing satisfying libclang by default, so
            # a case that removes it is genuinely testing absence rather than
            # falling through to whatever LLVM this machine has.
            ClangCommand             = 'clipped-no-such-clang'
            LibclangPath             = $llvmDirectory
            LibclangSearchDirectory  = $absentPath
            FfmpegDir                = $ffmpegPrefix
            FfmpegIncludeDir         = (Join-Path $ffmpegPrefix 'include')
            FfmpegLibsDir            = (Join-Path $ffmpegPrefix 'lib')
            FfmpegLinkMode           = 'dynamic'
            VsWherePath              = $vswherePresent
            WindowsKitsRegistryPath  = $sdkRegistryPath
            RustToolchainFile        = $pinFile
            NvmrcFile                = $nvmrcFile
            # Node is only required once the desktop application exists, and it
            # does not, so the default fixture leaves it absent.
            DesktopManifest          = $absentPath
            GraphicsAdapterInventory = $currentAdapters
            # Any Windows the check can run on clears a floor of 1, so the
            # supported-build path is exercised without asserting anything about
            # this machine.
            MinimumWindowsBuild      = '1'
            MaximumDriverAgeDays     = '730'
        }

        foreach ($key in @($Override.Keys)) { $settings[$key] = $Override[$key] }

        $arguments = @()
        foreach ($key in $settings.Keys) {
            if ($null -eq $settings[$key]) { continue }
            $arguments += "-$key"
            $arguments += [string] $settings[$key]
        }
        $arguments
    }

    # === absence ============================================================

    # A machine that satisfies the prerequisites must be reported as satisfying
    # them; a check that failed for everyone would be no more useful than one
    # that passed for everyone.
    Assert-Case -Name 'satisfied machine exits 0' `
        -Arguments (New-Arguments) `
        -ExpectedExitCode 0 `
        -ExpectedText @(
        'All required prerequisites are present.',
        '[ OK ] Rust toolchain',
        '[ OK ] Visual Studio C++ build tools',
        'Visual Studio Build Tools 2022 17.14.37411.7',
        "$fixtureRustPin-x86_64-pc-windows-msvc"
    )

    # The case the ticket exists for: none of the toolchain is present, and the
    # report has to name each missing piece and how to install it.
    Assert-Case -Name 'missing toolchain exits 1 and explains each item' `
        -Arguments (New-Arguments @{
            RustupCommand           = 'clipped-no-such-rustup'
            CargoCommand            = 'clipped-no-such-cargo'
            NodeCommand             = 'clipped-no-such-node'
            FfprobeCommand          = 'clipped-no-such-ffprobe'
            VsWherePath             = 'C:\clipped-no-such-directory\vswhere.exe'
            WindowsKitsRegistryPath = 'HKLM:\SOFTWARE\Clipped\NoSuchWindowsKits'
            DesktopManifest         = $desktopManifest
        }) `
        -ExpectedExitCode 1 `
        -ExpectedText @(
        '[FAIL] Rust toolchain',
        '[FAIL] Visual Studio C++ build tools',
        '[FAIL] Windows SDK',
        '[FAIL] Node.js',
        '[WARN] ffprobe',
        'required prerequisite(s) are missing:',
        'https://rustup.rs',
        'Desktop development with C++',
        'developer.microsoft.com/windows/downloads/windows-sdk',
        'https://nodejs.org',
        'Full instructions: docs/prerequisites.md'
    )

    # Node is only a hard requirement once the desktop application exists, so the
    # same missing Node must be a warning while apps/desktop/package.json does not.
    Assert-Case -Name 'missing Node is a warning until the desktop app exists' `
        -Arguments (New-Arguments @{ NodeCommand = 'clipped-no-such-node' }) `
        -ExpectedExitCode 0 `
        -ExpectedText @('[WARN] Node.js', 'optional prerequisite(s) need attention', 'https://nodejs.org')

    # Neither LLVM nor a fetched FFmpeg is on a fresh machine, and both are
    # needed before crates/muxer will build. Without these two checks the
    # contributor meets "Unable to find libclang" or "No linking method set!"
    # from inside a dependency's build script, several minutes in, with no
    # mention of LLVM or of the script that fetches FFmpeg.
    Assert-Case -Name 'missing LLVM and missing FFmpeg are both named, with what to run' `
        -Arguments (New-Arguments @{
            LibclangPath     = $null
            FfmpegDir        = $null
            FfmpegIncludeDir = $null
            FfmpegLibsDir    = $null
            FfmpegLinkMode   = $null
        }) `
        -ExpectedExitCode 1 `
        -ExpectedText @(
        '[FAIL] LLVM (libclang)',
        'no libclang.dll found',
        'winget install LLVM.LLVM',
        '[FAIL] FFmpeg libraries',
        'FFMPEG_DIR, FFMPEG_INCLUDE_DIR, FFMPEG_LIBS_DIR, FFMPEG_LINK_MODE not set in this shell',
        'scripts/fetch-ffmpeg.ps1',
        '2 required prerequisite(s) are missing:'
    )

    # LLVM installed the ordinary way puts libclang.dll beside clang.exe, and
    # nobody in that state has LIBCLANG_PATH set. Finding it there is what makes
    # the check pass for the majority of contributors.
    Assert-Case -Name 'libclang beside clang on PATH is found without LIBCLANG_PATH' `
        -Arguments (New-Arguments @{
            LibclangPath = $null
            ClangCommand = $clangBesideLibclang
        }) `
        -ExpectedExitCode 0 `
        -ExpectedText @('[ OK ] LLVM (libclang)', 'libclang.dll', 'All required prerequisites are present.')

    # === present, but wrong =================================================

    # LIBCLANG_PATH pointing at the wrong directory is a different fault to LLVM
    # being absent, and it has a different fix: nothing needs installing.
    Assert-Case -Name 'LIBCLANG_PATH pointing somewhere without the DLL is reported as itself' `
        -Arguments (New-Arguments @{ LibclangPath = $fixtureRoot }) `
        -ExpectedExitCode 1 `
        -ExpectedText @(
        '[FAIL] LLVM (libclang)',
        "LIBCLANG_PATH is set to $fixtureRoot, which holds no libclang.dll",
        'set LIBCLANG_PATH to the directory containing it'
    )

    # An FFMPEG_DIR left over from a pin whose directory has since been deleted
    # is set, so the "not set" branch does not catch it, and the failure it
    # produces later is a linker error listing unresolved av* symbols.
    Assert-Case -Name 'FFmpeg variables pointing at a build that is not there are reported' `
        -Arguments (New-Arguments @{
            FfmpegDir        = $absentPath
            FfmpegIncludeDir = $absentPath
            FfmpegLibsDir    = $absentPath
        }) `
        -ExpectedExitCode 1 `
        -ExpectedText @(
        '[FAIL] FFmpeg libraries',
        'the FFmpeg variables are set but these are missing',
        'avformat.h (FFMPEG_INCLUDE_DIR)',
        'avformat.lib (FFMPEG_LIBS_DIR)',
        'scripts/fetch-ffmpeg.ps1'
    )

    # Static is the binding's default, so this is the state of anyone who set
    # the variables by hand. It builds, and it quietly changes the licence
    # position of every binary that machine produces (ADR 0004), which is why it
    # is a failure and not a warning.
    Assert-Case -Name 'FFmpeg linked statically is a failure, not a preference' `
        -Arguments (New-Arguments @{ FfmpegLinkMode = 'static' }) `
        -ExpectedExitCode 1 `
        -ExpectedText @(
        '[FAIL] FFmpeg libraries',
        "FFMPEG_LINK_MODE is 'static', not 'dynamic'",
        'docs/adr/0004-ffmpeg-dependency-strategy.md'
    )


    # Visual Studio installed without the "Desktop development with C++"
    # workload is one of the most common states on a contributor machine, and it
    # is the state the whole check is pointed at. Real vswhere answers "[]" and
    # exits 0, which is not absence and not success. Windows PowerShell turns
    # that empty array into a single property-less object, so a count-based
    # guard reads 1 rather than 0; before this was fixed, the check indexed that
    # object, Set-StrictMode made it a terminating error, and the run aborted
    # with a PowerShell stack trace before printing a single status line.
    Assert-Case -Name 'Visual Studio without the C++ workload is reported, not crashed' `
        -Arguments (New-Arguments @{ VsWherePath = $vswhereNoCxx }) `
        -ExpectedExitCode 1 `
        -ExpectedText @(
        '[ OK ] Windows version',
        '[FAIL] Visual Studio C++ build tools',
        'Visual Studio is installed but no instance has the MSVC x64 C++ toolset',
        'Desktop development with C++',
        '1 required prerequisite(s) are missing:',
        'Full instructions: docs/prerequisites.md'
    ) `
        -UnexpectedText @('cannot be found on this object', 'FullyQualifiedErrorId')

    # Node on the wrong major version is not a missing Node, and once the
    # desktop application exists it is a failure rather than advice: the Tauri
    # tooling and native modules are built against a major.
    Assert-Case -Name 'Node on the wrong major version fails once the desktop app exists' `
        -Arguments (New-Arguments @{
            NodeCommand     = $nodeWrongMajor
            DesktopManifest = $desktopManifest
        }) `
        -ExpectedExitCode 1 `
        -ExpectedText @(
        '[FAIL] Node.js',
        "Node 18.20.4 is a different major version to the pinned $fixtureNodePin",
        'https://nodejs.org'
    )

    # The same wrong major is only advice while the desktop application does not
    # exist, because nothing in the repository runs Node yet.
    Assert-Case -Name 'Node on the wrong major version is a warning before the desktop app exists' `
        -Arguments (New-Arguments @{ NodeCommand = $nodeWrongMajor }) `
        -ExpectedExitCode 0 `
        -ExpectedText @('[WARN] Node.js', "is a different major version to the pinned $fixtureNodePin")

    # rustup present but the pinned toolchain never fetched. The message must be
    # what rustup said, not what PowerShell made of it: redirected native stderr
    # arrives as error records whose default rendering prefixes the executable
    # name and appends a CategoryInfo block.
    Assert-Case -Name 'a pinned toolchain that is not installed is reported in rustup words' `
        -Arguments (New-Arguments @{ RustupCommand = $rustupPinMissing }) `
        -ExpectedExitCode 1 `
        -ExpectedText @(
        '[FAIL] Rust toolchain',
        "the pinned toolchain $fixtureRustPin is not installed",
        "error: toolchain '$fixtureRustPin-x86_64-pc-windows-msvc' is not installed",
        "Run `"rustup toolchain install $fixtureRustPin`""
    ) `
        -UnexpectedText @('clipped-stub-rustup-pin-missing.cmd :', 'CategoryInfo')

    # The pin being installed is not the same as the pin being in effect. A
    # directory override or RUSTUP_TOOLCHAIN silently wins over
    # rust-toolchain.toml, and the only symptom is lint output nobody else can
    # reproduce.
    Assert-Case -Name 'a toolchain overriding the pin is reported' `
        -Arguments (New-Arguments @{ RustupCommand = $rustupOverridden }) `
        -ExpectedExitCode 1 `
        -ExpectedText @(
        '[FAIL] Rust toolchain',
        "is 1.70.0-x86_64-pc-windows-msvc, not the pinned $fixtureRustPin",
        'RUSTUP_TOOLCHAIN',
        'rustup override unset'
    )

    # A toolchain installed before rust-toolchain.toml existed will not carry
    # the components it names, and the symptom is "no such subcommand" part way
    # through a verification run.
    Assert-Case -Name 'a toolchain without rustfmt is reported' `
        -Arguments (New-Arguments @{ CargoCommand = $cargoWithoutRustfmt }) `
        -ExpectedExitCode 1 `
        -ExpectedText @(
        '[FAIL] rustfmt and clippy',
        'cargo fmt not available',
        'rustup component add rustfmt clippy'
    )

    # === reported facts that are not failures ===============================

    # An unsupported Windows build must be refused rather than left to fail later
    # against a missing API.
    Assert-Case -Name 'unsupported Windows build exits 1' `
        -Arguments (New-Arguments @{ MinimumWindowsBuild = '99999999' }) `
        -ExpectedExitCode 1 `
        -ExpectedText @('[FAIL] Windows version', 'is below the minimum supported build 99999999', 'Settings > Windows Update')

    # A stale graphics driver is advice, not a blocker: recording still works, it
    # is just the first thing to rule out when hardware encoding misbehaves.
    Assert-Case -Name 'stale graphics driver warns without failing' `
        -Arguments (New-Arguments @{ GraphicsAdapterInventory = $staleAdapters }) `
        -ExpectedExitCode 0 `
        -ExpectedText @(
        '[WARN] GPU and driver',
        'NVIDIA GeForce GTX 1060 [NVENC, 27.21.14.5671',
        'driver older than 730 days',
        'Update the graphics driver'
    )

    # Both facts are independent and a machine can have both. Software encoding
    # is the more consequential of the two, so it must not be swallowed by the
    # driver advice.
    Assert-Case -Name 'a stale driver and no hardware encoder are both reported' `
        -Arguments (New-Arguments @{ GraphicsAdapterInventory = $staleSoftwareAdapters }) `
        -ExpectedExitCode 0 `
        -ExpectedText @(
        '[WARN] GPU and driver',
        'no NVENC, AMF or Quick Sync capable adapter recognised',
        'driver older than 730 days',
        'Recording will fall back to software encoding',
        'Update the graphics driver'
    )

    # === a broken checkout, rather than a missing prerequisite ==============

    # A missing pin file is a broken checkout rather than a missing prerequisite,
    # so it is reported distinctly instead of being blamed on the toolchain.
    Assert-Case -Name 'unreadable Rust pin is reported separately' `
        -Arguments (New-Arguments @{ RustToolchainFile = 'C:\clipped-no-such-directory\rust-toolchain.toml' }) `
        -ExpectedExitCode 2 `
        -ExpectedText @('Could not read the pinned Rust channel')
} finally {
    Remove-Item -LiteralPath $fixtureRoot -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $fixtureRegistryRoot -Recurse -Force -ErrorAction SilentlyContinue

    foreach ($variable in $inheritedEnvironment.Keys) {
        $value = $inheritedEnvironment[$variable]
        if ($null -eq $value) { Remove-Item -LiteralPath "env:$variable" -ErrorAction SilentlyContinue }
        else { Set-Item -LiteralPath "env:$variable" -Value $value }
    }
}

Write-Host ''
if ($failureCount -eq 0) {
    Write-Host 'All prerequisite check cases passed.' -ForegroundColor Green
    exit 0
}

Write-Host "$failureCount prerequisite check case(s) failed." -ForegroundColor Red
exit 1
