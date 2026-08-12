#Requires -Version 5.1

<#
.SYNOPSIS
    Tests scripts/fetch-ffmpeg-source.ps1, and in particular that it reads the
    pin rather than carrying its own copy of it.

.DESCRIPTION
    The corresponding source a release publishes has to be the source of the
    build that release ships. Two things could break that quietly: the script
    could stop reading the pin out of scripts/fetch-ffmpeg.ps1 and fall back to
    something of its own, or it could derive the wrong revision from the asset
    name. Neither would fail visibly - it would produce a source archive, just
    of the wrong tree - so both are tested here.

    Every case runs the real script as a child process with -PlanOnly, so the
    suite touches no network and needs no git. The plan is the resolved pin and
    the revision derived from it, which is exactly the part that has to be
    right before anything is fetched.

    Fixtures are stand-in fetch scripts with pins that are deliberately not the
    real one. A case that passed against the real pin by coincidence would
    prove nothing (AGENTS.md section 25), which is why the fixture pin names a
    version of FFmpeg this project has never used.

    Written as a plain script rather than as Pester tests for the same reason
    scripts/test-check-prerequisites.ps1 is: the only Pester on a stock Windows
    install is 3.4.0, whose syntax is incompatible with the Pester 5 a
    contributor would install.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts/test-fetch-ffmpeg-source.ps1

.OUTPUTS
    Exit code 0 when every case passes, 1 otherwise.
#>

[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$sourceScript = Join-Path $PSScriptRoot 'fetch-ffmpeg-source.ps1'
$realFetchScript = Join-Path $PSScriptRoot 'fetch-ffmpeg.ps1'
$fixtureRoot = Join-Path ([System.IO.Path]::GetTempPath()) "clipped-ffmpeg-source-fixtures-$PID"
$failureCount = 0

function New-FixtureFetchScript {
    <#
    .SYNOPSIS
        A stand-in fetch script carrying one pin.
    .DESCRIPTION
        Only the parameter block matters: the script under test reads the
        defaults of -Tag, -Asset and -Sha256 out of the parsed file, so a
        fixture is a param block and nothing else.
    #>
    param(
        [Parameter(Mandatory)] [string] $Name,
        [Parameter(Mandatory)] [string] $Tag,
        [Parameter(Mandatory)] [string] $Asset,
        [Parameter(Mandatory)] [string] $Sha256
    )

    $path = Join-Path $fixtureRoot "$Name.ps1"
    $content = @"
param(
    [string] `$Tag = '$Tag',
    [string] `$Asset = '$Asset',
    [string] `$Sha256 = '$Sha256',
    [switch] `$Force
)
"@
    Set-Content -LiteralPath $path -Value $content -Encoding UTF8
    return $path
}

function Invoke-Case {
    <#
    .SYNOPSIS
        Runs the script under test and reports what it said and how it exited.
    #>
    param([Parameter(Mandatory)] [string[]] $Arguments)

    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $output = & powershell -ExecutionPolicy Bypass -File $sourceScript @Arguments 2>&1 | Out-String
        $code = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previous
    }

    return [pscustomobject]@{
        Output   = $output
        ExitCode = $code
    }
}

function Assert-Case {
    param(
        [Parameter(Mandatory)] [string] $Name,
        [Parameter(Mandatory)] $Result,
        [Parameter(Mandatory)] [int] $ExpectedExitCode,
        [Parameter(Mandatory)] [string[]] $Contains
    )

    $problems = @()
    if ($Result.ExitCode -ne $ExpectedExitCode) {
        $problems += "exit code was $($Result.ExitCode), expected $ExpectedExitCode"
    }
    foreach ($expected in $Contains) {
        if ($Result.Output -notlike "*$expected*") {
            $problems += "output does not mention '$expected'"
        }
    }

    if ($problems.Count -eq 0) {
        Write-Host "  PASS  $Name"
        return
    }

    Write-Host "  FAIL  $Name" -ForegroundColor Red
    foreach ($problem in $problems) { Write-Host "        $problem" -ForegroundColor Red }
    Write-Host '        it printed:' -ForegroundColor Red
    foreach ($line in ($Result.Output -split "`r?`n")) { Write-Host "        | $line" -ForegroundColor DarkGray }
    $script:failureCount++
}

New-Item -ItemType Directory -Path $fixtureRoot -Force | Out-Null

try {
    Write-Host 'Reading the pin'

    $fixture = New-FixtureFetchScript `
        -Name 'ordinary' `
        -Tag 'autobuild-1999-01-01-00-00' `
        -Asset 'ffmpeg-n4.2.9-77-gdeadbeef01-win64-lgpl-shared-4.2.zip' `
        -Sha256 'ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef0123456789'

    Assert-Case `
        -Name 'the tag, asset and checksum come from the fetch script, not from here' `
        -Result (Invoke-Case -Arguments @('-PlanOnly', '-FetchScript', $fixture)) `
        -ExpectedExitCode 0 `
        -Contains @(
        'autobuild-1999-01-01-00-00',
        'ffmpeg-n4.2.9-77-gdeadbeef01-win64-lgpl-shared-4.2.zip',
        'abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789'
    )

    Assert-Case `
        -Name 'a commit in the asset name is the revision fetched' `
        -Result (Invoke-Case -Arguments @('-PlanOnly', '-FetchScript', $fixture)) `
        -ExpectedExitCode 0 `
        -Contains @('deadbeef01 (from the asset name, as a commit)')

    $atTag = New-FixtureFetchScript `
        -Name 'at-a-tag' `
        -Tag 'autobuild-1999-01-02-00-00' `
        -Asset 'ffmpeg-n4.2.9-win64-lgpl-shared-4.2.zip' `
        -Sha256 '0000000000000000000000000000000000000000000000000000000000000000'

    Assert-Case `
        -Name 'an artefact built at a release tag names the tag instead' `
        -Result (Invoke-Case -Arguments @('-PlanOnly', '-FetchScript', $atTag)) `
        -ExpectedExitCode 0 `
        -Contains @('n4.2.9 (from the asset name, as a tag)')

    Write-Host 'Refusing to guess'

    $unreadable = New-FixtureFetchScript `
        -Name 'unreadable-asset' `
        -Tag 'autobuild-1999-01-03-00-00' `
        -Asset 'some-other-media-library.zip' `
        -Sha256 '0000000000000000000000000000000000000000000000000000000000000000'

    Assert-Case `
        -Name 'an asset name with no revision in it stops, naming the asset' `
        -Result (Invoke-Case -Arguments @('-PlanOnly', '-FetchScript', $unreadable)) `
        -ExpectedExitCode 1 `
        -Contains @('Cannot tell which FFmpeg source some-other-media-library.zip was built from')

    $missing = Join-Path $fixtureRoot 'no-such-fetch-script.ps1'
    Assert-Case `
        -Name 'a missing fetch script is reported as the pin being unreadable' `
        -Result (Invoke-Case -Arguments @('-PlanOnly', '-FetchScript', $missing)) `
        -ExpectedExitCode 1 `
        -Contains @('Cannot read the FFmpeg pin', 'no-such-fetch-script.ps1')

    $noParameter = Join-Path $fixtureRoot 'no-asset-parameter.ps1'
    Set-Content -LiteralPath $noParameter -Value "param([string] `$Tag = 'autobuild-1999-01-04-00-00')" -Encoding UTF8
    Assert-Case `
        -Name 'a fetch script without an -Asset parameter stops rather than inventing one' `
        -Result (Invoke-Case -Arguments @('-PlanOnly', '-FetchScript', $noParameter)) `
        -ExpectedExitCode 1 `
        -Contains @('has no -Asset parameter')

    Write-Host 'Against the real pin'

    # The pin this repository actually carries, read the same way. This case is
    # the one that fails if the parameter block of scripts/fetch-ffmpeg.ps1 is
    # rewritten into a form the AST reader does not understand - a change that
    # would otherwise be noticed only when a release published source for a
    # build it did not ship.
    $realAsset = 'ffmpeg-n8.1.2-34-g9b6c8969e0-win64-lgpl-shared-8.1.zip'
    if (-not (Select-String -Path $realFetchScript -SimpleMatch $realAsset -Quiet)) {
        Write-Host "  SKIP  the pin has moved since this case was written; it expects $realAsset" -ForegroundColor Yellow
    } else {
        Assert-Case `
            -Name 'the current pin resolves to the FFmpeg commit its asset names' `
            -Result (Invoke-Case -Arguments @('-PlanOnly')) `
            -ExpectedExitCode 0 `
            -Contains @($realAsset, '9b6c8969e0 (from the asset name, as a commit)')
    }
} finally {
    Remove-Item -LiteralPath $fixtureRoot -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host ''
if ($failureCount -gt 0) {
    Write-Host "$failureCount case(s) failed." -ForegroundColor Red
    exit 1
}

Write-Host 'All cases passed.' -ForegroundColor Green
exit 0
