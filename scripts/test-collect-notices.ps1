#Requires -Version 5.1

<#
.SYNOPSIS
    Tests scripts/collect-notices.ps1: what it refuses to produce, and what the
    payload it does produce actually contains.

.DESCRIPTION
    A notices payload is only worth anything if it is complete and true, and
    both ways of getting that wrong are silent. A missing licence text produces
    a payload that looks finished; a dependency list that quietly included
    test-only crates, or quietly dropped shipped ones, would read exactly the
    same as a correct one. Each case here therefore asserts on the contents of
    what was written, not on the script having exited zero.

    The cases that need a licence text of Clipped's own run against a fixture
    repository root, so that removing a file from the fixture is how "what
    happens when it is missing" is asked. The cases that describe FFmpeg or the
    Rust dependency graph run against the real installed build and the real
    workspaces, because those are what a release would describe and a fixture
    would only prove the fixture.

    Nothing here is skipped when the machine is not set up: the FFmpeg build is
    a documented prerequisite (docs/ffmpeg.md), and a suite that passes by
    skipping is the failure mode AGENTS.md section 54 is about.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts/test-collect-notices.ps1

.OUTPUTS
    Exit code 0 when every case passes, 1 otherwise.
#>

[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repositoryRoot = Split-Path -Parent $PSScriptRoot
$collectScript = Join-Path $PSScriptRoot 'collect-notices.ps1'
$fixtureRoot = Join-Path ([System.IO.Path]::GetTempPath()) "clipped-notices-fixtures-$PID"
$failureCount = 0

$ffmpegDir = if ($env:FFMPEG_DIR) { $env:FFMPEG_DIR } else { Join-Path $repositoryRoot 'third-party\ffmpeg\current' }
if (-not (Test-Path -LiteralPath (Join-Path $ffmpegDir 'bin\ffprobe.exe'))) {
    Write-Host "No FFmpeg build at $ffmpegDir. Run scripts/fetch-ffmpeg.ps1 first; these tests describe a real build." -ForegroundColor Red
    exit 1
}

function New-FixtureRepository {
    <#
    .SYNOPSIS
        A checkout-shaped directory holding only what the script reads from one.
    #>
    param([Parameter(Mandatory)] [string] $Name)

    $root = Join-Path $fixtureRoot $Name
    New-Item -ItemType Directory -Path (Join-Path $root 'licences') -Force | Out-Null
    Copy-Item -LiteralPath (Join-Path $repositoryRoot 'LICENSE') -Destination (Join-Path $root 'LICENSE')
    Copy-Item -LiteralPath (Join-Path $repositoryRoot 'THIRD-PARTY-NOTICES.md') -Destination (Join-Path $root 'THIRD-PARTY-NOTICES.md')
    Copy-Item -LiteralPath (Join-Path $repositoryRoot 'licences\GPL-3.0.txt') -Destination (Join-Path $root 'licences\GPL-3.0.txt')
    return $root
}

function Invoke-Collect {
    param([Parameter(Mandatory)] [string[]] $Arguments)

    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $output = & powershell -ExecutionPolicy Bypass -File $collectScript @Arguments 2>&1 | Out-String
        $code = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previous
    }

    return [pscustomobject]@{ Output = $output; ExitCode = $code }
}

function Test-Case {
    param(
        [Parameter(Mandatory)] [string] $Name,
        [Parameter(Mandatory)] [scriptblock] $Body
    )

    $problems = @(& $Body)
    if ($problems.Count -eq 0) {
        Write-Host "  PASS  $Name"
        return
    }

    Write-Host "  FAIL  $Name" -ForegroundColor Red
    foreach ($problem in $problems) { Write-Host "        $problem" -ForegroundColor Red }
    $script:failureCount++
}

New-Item -ItemType Directory -Path $fixtureRoot -Force | Out-Null

try {
    Write-Host 'Refusing to write an incomplete payload'

    Test-Case -Name 'a missing GPL v3 text stops the run and writes nothing' -Body {
        $fixture = New-FixtureRepository -Name 'no-gpl'
        Remove-Item -LiteralPath (Join-Path $fixture 'licences\GPL-3.0.txt')
        $destination = Join-Path $fixture 'out'

        $result = Invoke-Collect -Arguments @(
            '-RepositoryRoot', $fixture, '-FFmpegDir', $ffmpegDir,
            '-Destination', $destination, '-SkipRustDependencies')

        $problems = @()
        if ($result.ExitCode -ne 1) { $problems += "exit code was $($result.ExitCode), expected 1" }
        if ($result.Output -notlike '*GNU GPL v3 text*') { $problems += 'the message does not say what was missing' }
        if (Test-Path -LiteralPath $destination) { $problems += 'a payload directory was written anyway' }
        $problems
    }

    Test-Case -Name 'an FFmpeg prefix with no licence text stops the run' -Body {
        $fixture = New-FixtureRepository -Name 'no-ffmpeg-licence'
        $emptyPrefix = Join-Path $fixture 'ffmpeg'
        New-Item -ItemType Directory -Path (Join-Path $emptyPrefix 'bin') -Force | Out-Null
        $destination = Join-Path $fixture 'out'

        $result = Invoke-Collect -Arguments @(
            '-RepositoryRoot', $fixture, '-FFmpegDir', $emptyPrefix,
            '-Destination', $destination, '-SkipRustDependencies')

        $problems = @()
        if ($result.ExitCode -ne 1) { $problems += "exit code was $($result.ExitCode), expected 1" }
        if ($result.Output -notlike "*FFmpeg build's licence text*") { $problems += 'the message does not name the missing licence text' }
        if (Test-Path -LiteralPath $destination) { $problems += 'a payload directory was written anyway' }
        $problems
    }

    Write-Host 'What the payload carries'

    $fixture = New-FixtureRepository -Name 'payload'
    $payload = Join-Path $fixture 'out'
    $ffmpegOnly = Invoke-Collect -Arguments @(
        '-RepositoryRoot', $fixture, '-FFmpegDir', $ffmpegDir,
        '-Destination', $payload, '-SkipRustDependencies')

    Test-Case -Name 'both licence texts the LGPL requires are in the payload' -Body {
        $problems = @()
        if ($ffmpegOnly.ExitCode -ne 0) { $problems += "the run failed: $($ffmpegOnly.Output)"; return $problems }

        $lgpl = Join-Path $payload 'ffmpeg\LGPL-3.0.txt'
        $gpl = Join-Path $payload 'ffmpeg\GPL-3.0.txt'
        if (-not (Test-Path -LiteralPath $lgpl)) { $problems += 'the LGPL text is missing' }
        elseif ((Get-Content -LiteralPath $lgpl -Raw) -notmatch 'GNU LESSER GENERAL PUBLIC LICENSE') { $problems += 'LGPL-3.0.txt is not the LGPL' }
        if (-not (Test-Path -LiteralPath $gpl)) { $problems += 'the GPL text is missing' }
        elseif ((Get-Content -LiteralPath $gpl -Raw) -notmatch 'GNU GENERAL PUBLIC LICENSE') { $problems += 'GPL-3.0.txt is not the GPL' }
        $problems
    }

    Test-Case -Name 'the notice describes the build that is actually installed' -Body {
        $problems = @()
        $notice = Join-Path $payload 'ffmpeg\NOTICE.md'
        if (-not (Test-Path -LiteralPath $notice)) { return @('ffmpeg/NOTICE.md is missing') }

        # Read out of the build by this test rather than written down here, so
        # that a payload describing some other FFmpeg cannot pass.
        $reported = & (Join-Path $ffmpegDir 'bin\ffprobe.exe') -hide_banner -version 2>&1 | Out-String
        if ($reported -notmatch '^ffprobe version (?<version>\S+)') { return @('could not read a version out of the installed ffprobe') }
        $version = $Matches['version']

        # Newlines collapsed before matching: the notice is wrapped prose, so a
        # phrase in it can be split across two lines and a literal match would
        # be asserting on where the wrapping happened to fall.
        $text = (Get-Content -LiteralPath $notice -Raw) -replace '\s+', ' '
        if ($text -notlike "*$version*") { $problems += "the notice does not name the installed build $version" }
        if ($text -notlike '*Lesser General Public License*') { $problems += 'the notice does not say FFmpeg is under the LGPL' }
        if ($text -notlike '*--enable-version3*') { $problems += 'the notice does not carry the configuration the build reports' }
        $problems
    }

    Test-Case -Name 'a payload without the dependency notices says it is incomplete' -Body {
        $readme = Get-Content -LiteralPath (Join-Path $payload 'README.md') -Raw
        if ($readme -notlike '*Not generated*') { @('README.md does not say the payload is incomplete') } else { @() }
    }

    Write-Host 'The Rust dependency notices'

    $full = Join-Path $fixture 'full'
    $withCrates = Invoke-Collect -Arguments @(
        '-RepositoryRoot', $repositoryRoot, '-FFmpegDir', $ffmpegDir, '-Destination', $full)

    Test-Case -Name 'crates in the binaries are listed and build-time-only crates are not' -Body {
        $problems = @()
        if ($withCrates.ExitCode -ne 0) { $problems += "the run failed: $($withCrates.Output)"; return $problems }

        $listed = @(Select-String -Path (Join-Path $full 'THIRD-PARTY-NOTICES-RUST.md') -Pattern '^## (?<name>\S+) ' |
                ForEach-Object { $_.Matches[0].Groups['name'].Value })

        # rusty_ffmpeg is linked into the recorder, so its notice has to travel
        # with it. bindgen is rusty_ffmpeg's own build-dependency: it generates
        # the FFI at build time and no part of it is in the binary, so listing
        # it would claim something untrue about what a user was given. Clipped's
        # own crates are covered by LICENSE.txt in the same payload.
        if ($listed -notcontains 'rusty_ffmpeg') { $problems += 'rusty_ffmpeg is missing, and it is linked into the recorder' }
        if ($listed -contains 'bindgen') { $problems += 'bindgen is listed, and it is a build-dependency that is not in any binary' }
        if ($listed -contains 'clipped-muxer') { $problems += "clipped-muxer is listed, and Clipped's own crates are covered by LICENSE.txt" }
        $problems
    }

    Test-Case -Name 'each notice reproduces the licence text the crate publishes' -Body {
        $text = Get-Content -LiteralPath (Join-Path $full 'THIRD-PARTY-NOTICES-RUST.md') -Raw
        $problems = @()
        # The permission notice is the thing MIT and BSD require to be carried;
        # a list of crate names and licence identifiers would not discharge it.
        if ($text -notlike '*Permission is hereby granted*') { $problems += 'no MIT permission notice was reproduced anywhere in the file' }
        if ($text -notlike '*Copyright*') { $problems += 'no copyright line was reproduced anywhere in the file' }
        $problems
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
