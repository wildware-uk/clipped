#Requires -Version 5.1

<#
.SYNOPSIS
    Tests scripts/check-prerequisites.ps1, including its missing-dependency path.

.DESCRIPTION
    The value of the prerequisite check is what it does when something is
    absent, and that is the case nobody exercises by running it on a working
    machine. Each case here runs the real script as a child process and asserts
    on its exit code and on the text a contributor would read.

    Absence is produced by pointing a probe at something that genuinely is not
    installed - a command name that does not exist, a path that does not exist,
    a registry key that does not exist - so the detection code under test is the
    same code that runs in production. Nothing is stubbed or simulated.

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
        Asserts an exit code and the presence of expected report text.
    .PARAMETER ExpectedText
        Substrings that must all appear in the output. These are the sentences a
        contributor relies on, so they are part of the contract, not decoration.
    #>
    param(
        [Parameter(Mandatory)] [string] $Name,
        [Parameter(Mandatory)] [int] $ExpectedExitCode,
        [Parameter(Mandatory)] [string[]] $ExpectedText,
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

# A machine that satisfies the prerequisites must be reported as satisfying
# them; a check that failed for everyone would be no more useful than one that
# passed for everyone.
Assert-Case -Name 'satisfied machine exits 0' `
    -ExpectedExitCode 0 `
    -ExpectedText @('All required prerequisites are present.', '[ OK ] Rust toolchain')

# The case the ticket exists for: none of the toolchain is present, and the
# report has to name each missing piece and how to install it.
$missingArguments = @(
    '-RustupCommand', 'clipped-no-such-rustup',
    '-CargoCommand', 'clipped-no-such-cargo',
    '-NodeCommand', 'clipped-no-such-node',
    '-FfprobeCommand', 'clipped-no-such-ffprobe',
    '-VsWherePath', 'C:\clipped-no-such-directory\vswhere.exe',
    '-WindowsKitsRegistryPath', 'HKLM:\SOFTWARE\Clipped\NoSuchWindowsKits',
    '-DesktopManifest', $checkScript
)
Assert-Case -Name 'missing toolchain exits 1 and explains each item' `
    -Arguments $missingArguments `
    -ExpectedExitCode 1 `
    -ExpectedText @(
    '[FAIL] Rust toolchain',
    '[FAIL] Visual Studio C++ build tools',
    '[FAIL] Windows SDK',
    '[FAIL] Node.js',
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
    -Arguments @(
    '-NodeCommand', 'clipped-no-such-node',
    '-DesktopManifest', 'C:\clipped-no-such-directory\package.json'
) `
    -ExpectedExitCode 0 `
    -ExpectedText @('[WARN] Node.js', 'optional prerequisite(s) need attention', 'https://nodejs.org')

# An unsupported Windows build must be refused rather than left to fail later
# against a missing API.
Assert-Case -Name 'unsupported Windows build exits 1' `
    -Arguments @('-MinimumWindowsBuild', '99999999') `
    -ExpectedExitCode 1 `
    -ExpectedText @('[FAIL] Windows version', 'is below the minimum supported build 99999999', 'Settings > Windows Update')

# A stale graphics driver is advice, not a blocker: recording still works, it
# is just the first thing to rule out when hardware encoding misbehaves.
Assert-Case -Name 'stale graphics driver warns without failing' `
    -Arguments @('-MaximumDriverAgeDays', '0') `
    -ExpectedExitCode 0 `
    -ExpectedText @('[WARN] GPU and driver', 'driver older than 0 days', 'Update the graphics driver')

# A missing pin file is a broken checkout rather than a missing prerequisite,
# so it is reported distinctly instead of being blamed on the toolchain.
Assert-Case -Name 'unreadable Rust pin is reported separately' `
    -Arguments @('-RustToolchainFile', 'C:\clipped-no-such-directory\rust-toolchain.toml') `
    -ExpectedExitCode 2 `
    -ExpectedText @('Could not read the pinned Rust channel')

Write-Host ''
if ($failureCount -eq 0) {
    Write-Host 'All prerequisite check cases passed.' -ForegroundColor Green
    exit 0
}

Write-Host "$failureCount prerequisite check case(s) failed." -ForegroundColor Red
exit 1
