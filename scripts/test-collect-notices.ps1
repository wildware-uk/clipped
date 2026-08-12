#Requires -Version 5.1

<#
.SYNOPSIS
    Tests scripts/collect-notices.ps1: what it refuses to produce, what it
    refuses to delete, and what the payload it does produce actually contains.

.DESCRIPTION
    A notices payload is only worth anything if it is complete and true, and
    both ways of getting that wrong are silent. A missing licence text produces
    a payload that looks finished; a dependency list that quietly included
    test-only crates, or quietly dropped shipped ones, would read exactly the
    same as a correct one. Each case here therefore asserts on the contents of
    what was written, not on the script having exited zero.

    Two properties are easy to assert uselessly, and are asserted here in the
    only form that can fail:

    - **Both workspaces are walked.** Clipped has two, and everything the
      desktop application links is in the second one. A case that names only
      crates the root workspace reaches would pass with the second workspace
      deleted from the script, so the cases below name `tauri`, `wry` and
      `webview2-com` - which appear in no root-workspace graph - and put a floor
      under the crate count.
    - **The notices are reproduced.** Grepping the whole file for "Permission is
      hereby granted" passes when one crate in three hundred still has its text.
      So the cases below take a named crate's own section and assert that
      crate's own licence text is inside it, and hold the number of crates
      rendered as "publishes no licence file" to a ceiling.

    The cases that need a licence text of Clipped's own run against a fixture
    repository root, so that removing a file from the fixture is how "what
    happens when it is missing" is asked. The cases that describe FFmpeg or the
    Rust dependency graph run against the real installed build and the real
    workspaces, because those are what a release would describe and a fixture
    would only prove the fixture.

    The exception is the licence refusal. Clipped may not ship a GPL FFmpeg
    (docs/adr/0004-ffmpeg-dependency-strategy.md), and that guard is the only
    thing standing between a mistaken FFMPEG_DIR and a payload asserting a
    licence position nobody chose - so it has to be tested, and testing it needs
    an FFmpeg that answers "GPL". Rather than requiring a second real build on
    every machine that runs this, the fixture is a stub `ffprobe.exe` compiled
    on the spot from a few lines of C#, which answers `-version` and `-L` the
    way FFmpeg's does. That is enough, because the script's whole knowledge of a
    build comes through those two invocations.

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

# The union of both workspaces is a little over 270 crates; the root workspace
# alone reaches about 80. A floor between the two by that margin cannot be met
# by one workspace, and does not have to be edited every time a dependency is
# added or removed - which is what would turn it back into a number nobody
# trusts.
$minimumCrateCount = 200

# Crates whose licence file the script could not read are rendered as a line of
# prose instead of a notice. A handful genuinely publish none; a jump means the
# reading broke, which is invisible in a 2 MB file. Eleven on the day this was
# written.
$maximumCratesWithoutALicenceFile = 20

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

function New-FixtureFFmpegPrefix {
    <#
    .SYNOPSIS
        An FFmpeg prefix whose ffprobe reports the licence a case needs it to.
    .DESCRIPTION
        Everything collect-notices.ps1 knows about an FFmpeg build it learns by
        running `ffprobe -version` and `ffprobe -L` in that build's own bin
        directory, so a program that answers those two the way FFmpeg does is
        indistinguishable from one to the code under test.

        It has to be a real executable - PowerShell will not run a batch file
        named .exe, and the script looks for bin\ffprobe.exe by name - so it is
        compiled here. Add-Type reaches the C# compiler that ships with the
        .NET Framework, which is a Windows component and is on every machine
        that has Windows PowerShell 5.1 to run this suite with.

        The alternative was to require a second, GPL FFmpeg build on every
        machine, which would mean the refusal was tested nowhere.
    #>
    param(
        [Parameter(Mandatory)] [string] $Name,
        [Parameter(Mandatory)] [string] $Version,
        [Parameter(Mandatory)] [string] $LicenceBanner
    )

    $prefix = Join-Path $fixtureRoot $Name
    New-Item -ItemType Directory -Path (Join-Path $prefix 'bin') -Force | Out-Null

    # The script asserts the prefix carries a licence text before it describes
    # the build, so the fixture carries one too. Which text it is does not
    # matter to the case: what is on trial is what the libraries report.
    Copy-Item -LiteralPath (Join-Path $ffmpegDir 'LICENSE.txt') -Destination (Join-Path $prefix 'LICENSE.txt')

    $source = @"
using System;

public static class Program
{
    public static int Main(string[] args)
    {
        foreach (string argument in args)
        {
            if (argument == "-L")
            {
                Console.WriteLine("$LicenceBanner");
                return 0;
            }
        }

        Console.WriteLine("ffprobe version $Version Copyright (c) 2000-2026 the FFmpeg developers");
        Console.WriteLine("configuration: --enable-shared --enable-version3");
        Console.WriteLine("libavutil      60. 26.100 / 60. 26.100");
        Console.WriteLine("libavformat    62. 12.100 / 62. 12.100");
        return 0;
    }
}
"@

    Add-Type -TypeDefinition $source -OutputAssembly (Join-Path $prefix 'bin\ffprobe.exe') -OutputType ConsoleApplication
    return $prefix
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

function Get-CrateSection {
    <#
    .SYNOPSIS
        One crate's entry in the generated notices, heading and body.
    .DESCRIPTION
        The file is a sequence of `## name version` headings, so a crate's own
        notice is the text between its heading and the next one. Asserting
        inside that slice rather than over the whole file is the difference
        between "some crate somewhere still has a licence text" and "this crate
        has its own".

        The version is not part of the lookup. A dependency bump must not turn
        a case that guards the notices into one that quietly stops finding the
        crate it was written for.
    #>
    param(
        [Parameter(Mandatory)] [string] $Path,
        [Parameter(Mandatory)] [string] $Crate
    )

    $sections = [regex]::Split((Get-Content -LiteralPath $Path -Raw), '(?m)^## ')
    $pattern = '^' + [regex]::Escape($Crate) + ' [0-9]'
    return @($sections | Where-Object { $_ -match $pattern })[0]
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

    Write-Host 'Refusing an FFmpeg Clipped may not ship'

    # These two are the guard docs/licensing.md and the ADR both rest on. The
    # licence Clipped ships under depends on FFmpeg answering "LGPL", and the
    # answer is asked of the build rather than read off a directory name
    # precisely because a GPL build of the same commit reports the same version.
    Test-Case -Name 'a GPL FFmpeg is refused, by name, and nothing is written' -Body {
        $fixture = New-FixtureRepository -Name 'gpl-build'
        $prefix = New-FixtureFFmpegPrefix -Name 'gpl-ffmpeg' -Version 'n8.1.2-34-g9b6c8969e0-20260809' -LicenceBanner @'
ffprobe is free software; you can redistribute it and/or modify it under the terms of the GNU General Public License as published by the Free Software Foundation; either version 3 of the License, or (at your option) any later version.
'@
        $destination = Join-Path $fixture 'out'

        $result = Invoke-Collect -Arguments @(
            '-RepositoryRoot', $fixture, '-FFmpegDir', $prefix,
            '-Destination', $destination, '-SkipRustDependencies')

        $problems = @()
        if ($result.ExitCode -ne 1) { $problems += "exit code was $($result.ExitCode), expected 1" }
        if ($result.Output -notlike '*reports its licence as GPL, not LGPL*') { $problems += 'the message does not say the build reported GPL' }
        if ($result.Output -notlike '*may not ship a GPL FFmpeg*') { $problems += 'the message does not say why that is refused' }
        if (Test-Path -LiteralPath $destination) { $problems += 'a payload describing a GPL FFmpeg was written anyway' }
        $problems
    }

    Test-Case -Name 'an FFmpeg whose licence is unrecognised is refused too' -Body {
        $fixture = New-FixtureRepository -Name 'unknown-licence-build'
        $prefix = New-FixtureFFmpegPrefix -Name 'unknown-ffmpeg' -Version 'n8.1.2-34-g9b6c8969e0-20260809' -LicenceBanner @'
ffprobe is proprietary software supplied under a licence agreement with somebody.
'@
        $destination = Join-Path $fixture 'out'

        $result = Invoke-Collect -Arguments @(
            '-RepositoryRoot', $fixture, '-FFmpegDir', $prefix,
            '-Destination', $destination, '-SkipRustDependencies')

        $problems = @()
        if ($result.ExitCode -ne 1) { $problems += "exit code was $($result.ExitCode), expected 1" }
        if ($result.Output -notlike '*reports its licence as unrecognised, not LGPL*') { $problems += 'the message does not say the licence was not recognised' }
        if (Test-Path -LiteralPath $destination) { $problems += 'a payload was written for a build whose licence is unknown' }
        $problems
    }

    Write-Host 'What it will and will not overwrite'

    # -Destination is documented, and release-checklist step 5 points the
    # payload at an installer's staging directory. Emptying whatever it is
    # given is therefore a thing this script must not do.
    Test-Case -Name 'a destination holding files this script did not write is refused' -Body {
        $fixture = New-FixtureRepository -Name 'foreign-destination'
        $destination = Join-Path $fixture 'staging'
        New-Item -ItemType Directory -Path $destination -Force | Out-Null
        $bystander = Join-Path $destination 'clipped.exe'
        Set-Content -LiteralPath $bystander -Value 'a build somebody staged here' -Encoding UTF8

        $result = Invoke-Collect -Arguments @(
            '-RepositoryRoot', $fixture, '-FFmpegDir', $ffmpegDir,
            '-Destination', $destination, '-SkipRustDependencies')

        $problems = @()
        if ($result.ExitCode -ne 1) { $problems += "exit code was $($result.ExitCode), expected 1" }
        if ($result.Output -notlike '*did not write*') { $problems += 'the message does not say the directory was not its own' }
        if (-not (Test-Path -LiteralPath $bystander)) { $problems += 'the file that was already there was deleted' }
        elseif ((Get-Content -LiteralPath $bystander -Raw) -notlike '*a build somebody staged here*') { $problems += 'the file that was already there was overwritten' }
        if (Test-Path -LiteralPath (Join-Path $destination 'LICENSE.txt')) { $problems += 'part of a payload was written into it anyway' }
        $problems
    }

    Test-Case -Name 'a payload from a previous run is replaced without complaint' -Body {
        $fixture = New-FixtureRepository -Name 'rerun'
        $destination = Join-Path $fixture 'out'

        $first = Invoke-Collect -Arguments @(
            '-RepositoryRoot', $fixture, '-FFmpegDir', $ffmpegDir,
            '-Destination', $destination, '-SkipRustDependencies')
        $problems = @()
        if ($first.ExitCode -ne 0) { $problems += "the first run failed: $($first.Output)"; return $problems }

        # Something left behind by the first run that the second must clear:
        # replacing a payload means replacing it, not merging into it.
        $stale = Join-Path $destination 'ffmpeg\STALE.md'
        Set-Content -LiteralPath $stale -Value 'left over from an older payload' -Encoding UTF8

        $second = Invoke-Collect -Arguments @(
            '-RepositoryRoot', $fixture, '-FFmpegDir', $ffmpegDir,
            '-Destination', $destination, '-SkipRustDependencies')

        if ($second.ExitCode -ne 0) { $problems += "the second run failed: $($second.Output)" }
        if (Test-Path -LiteralPath $stale) { $problems += 'a file from the previous payload survived the replacement' }
        if (-not (Test-Path -LiteralPath (Join-Path $destination 'ffmpeg\NOTICE.md'))) { $problems += 'the replacement payload is missing its FFmpeg notice' }
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
    $rustNotices = Join-Path $full 'THIRD-PARTY-NOTICES-RUST.md'

    Test-Case -Name 'crates in the binaries are listed and build-time-only crates are not' -Body {
        $problems = @()
        if ($withCrates.ExitCode -ne 0) { $problems += "the run failed: $($withCrates.Output)"; return $problems }

        $listed = @(Select-String -Path $rustNotices -Pattern '^## (?<name>\S+) ' |
                ForEach-Object { $_.Matches[0].Groups['name'].Value })

        # rusty_ffmpeg is linked into the recorder, so its notice has to travel
        # with it. bindgen is rusty_ffmpeg's own build-dependency: it generates
        # the FFI at build time and no part of it is in the binary, so listing
        # it would claim something untrue about what a user was given.
        if ($listed -notcontains 'rusty_ffmpeg') { $problems += 'rusty_ffmpeg is missing, and it is linked into the recorder' }
        if ($listed -contains 'bindgen') { $problems += 'bindgen is listed, and it is a build-dependency that is not in any binary' }

        # clipped-ipc is the Clipped crate this can actually get wrong: it is a
        # member of the root workspace and an ordinary path dependency of the
        # desktop one, so the desktop walk reaches it as it reaches any crate
        # from crates.io. clipped-muxer is a member of the only workspace that
        # names it and could not be listed by accident; it is here so that the
        # rule reads as a rule rather than as one crate's exception.
        if ($listed -contains 'clipped-ipc') { $problems += "clipped-ipc is listed as third-party, and it is Clipped's own crate, covered by LICENSE.txt" }
        if ($listed -contains 'clipped-muxer') { $problems += "clipped-muxer is listed, and Clipped's own crates are covered by LICENSE.txt" }
        $problems
    }

    Test-Case -Name 'the desktop workspace is walked, not only the root one' -Body {
        $problems = @()
        if ($withCrates.ExitCode -ne 0) { return @('the run failed; see the case above') }

        $listed = @(Select-String -Path $rustNotices -Pattern '^## (?<name>\S+) ' |
                ForEach-Object { $_.Matches[0].Groups['name'].Value })

        # Clipped has two Cargo workspaces and everything the desktop
        # application links is in the second. These three are reachable from
        # apps/desktop/src-tauri and from nowhere in the root graph, so they are
        # absent from the payload the moment that second walk stops happening -
        # which is otherwise a change that takes the file from 275 notices to 80
        # and reads exactly the same.
        foreach ($crate in @('tauri', 'wry', 'webview2-com')) {
            if ($listed -notcontains $crate) {
                $problems += "$crate is missing; it is reachable only through apps/desktop/src-tauri, so the second workspace was not walked"
            }
        }

        if ($listed.Count -lt $minimumCrateCount) {
            $problems += "$($listed.Count) crates are listed, which is below the floor of $minimumCrateCount; one workspace alone reaches about 80"
        }
        $problems
    }

    Test-Case -Name 'each named crate reproduces its own licence text under its own heading' -Body {
        $problems = @()
        if ($withCrates.ExitCode -ne 0) { return @('the run failed; see the case above') }

        # Two crates, chosen because they are reached from different workspaces
        # and carry different licence texts, and asserted on their own sections
        # rather than on the file: a whole-file match for "Permission is hereby
        # granted" is satisfied by any one of nearly three hundred crates and
        # says nothing about the other 274.

        # serde, from both workspaces, publishes the full Apache 2.0 text - the
        # long one, and so the first casualty of anything that drops large
        # files.
        $serde = Get-CrateSection -Path $rustNotices -Crate 'serde'
        if (-not $serde) {
            $problems += 'serde has no section at all'
        } else {
            if ($serde -notlike '*### LICENSE-APACHE*') { $problems += 'serde does not carry its LICENSE-APACHE' }
            if ($serde -notlike '*TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION*') { $problems += 'serde carries no Apache 2.0 licence text' }
            if ($serde -notlike '*Permission is hereby granted*') { $problems += 'serde carries no MIT permission notice, and it is dual-licensed' }
        }

        # tokio, from the desktop workspace, is MIT and its file carries the
        # copyright line - which is the part MIT requires to travel with the
        # binary and the part a list of licence identifiers would lose.
        $tokio = Get-CrateSection -Path $rustNotices -Crate 'tokio'
        if (-not $tokio) {
            $problems += 'tokio has no section at all'
        } else {
            if ($tokio -notlike '*Permission is hereby granted*') { $problems += 'tokio carries no MIT permission notice' }
            if ($tokio -notlike '*Copyright (c) Tokio Contributors*') { $problems += "tokio carries no copyright line, which is the part MIT requires to travel" }
        }
        $problems
    }

    Test-Case -Name 'crates with no licence file of their own stay a small minority' -Body {
        $problems = @()
        if ($withCrates.ExitCode -ne 0) { return @('the run failed; see the case above') }

        $withoutALicenceFile = @(Select-String -Path $rustNotices -SimpleMatch 'This crate publishes no licence file').Count
        if ($withoutALicenceFile -gt $maximumCratesWithoutALicenceFile) {
            $problems += "$withoutALicenceFile crates are rendered as publishing no licence file, above the ceiling of $maximumCratesWithoutALicenceFile; the notices are being dropped rather than reproduced"
        }
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
