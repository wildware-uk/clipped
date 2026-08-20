#Requires -Version 5.1

<#
.SYNOPSIS
    Tests scripts/stage-installer-payload.ps1: that it refuses rather than
    staging a payload that cannot record, and that what it stages is exactly the
    recorder and the FFmpeg libraries.

.DESCRIPTION
    Two failures this guards against are silent by construction, which is why
    they are tested here rather than left to a build going wrong later.

    The first is an installer built without the recorder. That is issue #226
    itself: nothing crashes, no error is printed, and the installed application
    reports the recorder missing forever. The cases below run the real script
    against a directory where the recorder is not, and assert both that it stops
    and that it staged nothing - a script that refused *after* copying would
    leave a stale recorder from a previous run to be shipped.

    The second is the wrong set of files. Everything in the FFmpeg build's `bin`
    with a .dll extension has to travel with the recorder, because libavformat
    loads its siblings; the programs in that same directory - ffmpeg.exe,
    ffprobe.exe, ffplay.exe - must not, because nothing in Clipped runs them.
    Both halves are asserted, against a fixture and then against the FFmpeg this
    repository actually carries.

    Every case runs the real script as a child process and inspects the
    directory it left behind, so what is under test is the script an installer
    build runs, with its own argument parsing and its own exit code. What
    *calls* it is asserted elsewhere and not here: tauri.conf.json's
    beforeBuildCommand runs it, and .github/workflows/ci.yml runs
    `npm run build:app` in a checkout with no recorder in it and requires that to
    fail.

    Written as a plain script rather than as Pester tests for the same reason
    scripts/test-check-prerequisites.ps1 is: the only Pester on a stock Windows
    install is 3.4.0, whose syntax is incompatible with the Pester 5 a
    contributor would install.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts/test-stage-installer-payload.ps1

.OUTPUTS
    Exit code 0 when every case passes, 1 otherwise.
#>

[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$stageScript = Join-Path $PSScriptRoot 'stage-installer-payload.ps1'
$repositoryRoot = Split-Path -Parent $PSScriptRoot
$realFfmpegDir = Join-Path $repositoryRoot 'third-party\ffmpeg\current'
$fixtureRoot = Join-Path ([System.IO.Path]::GetTempPath()) "clipped-installer-payload-fixtures-$PID"
$failureCount = 0

function New-Fixture {
    <#
    .SYNOPSIS
        A directory tree standing in for a built workspace.
    .DESCRIPTION
        The script under test copies files and looks at their names; it never
        loads one. So a fixture "recorder" and a fixture "DLL" are small files
        with the right names, which is what lets these cases run in a fraction
        of a second and on a machine that has never built Clipped.
    #>
    param(
        [Parameter(Mandatory)] [string] $Name,
        [switch] $WithRecorder,
        [string[]] $Libraries = @(),
        [string[]] $Programs = @(),
        # Every fixture has its notices, because every real build does: they are
        # what `scripts/collect-notices.ps1` leaves behind and what an installed
        # copy is obliged to carry. The switch is for the one case that proves
        # the obligation is enforced (issue #123).
        [switch] $WithoutLicences
    )

    $root = Join-Path $fixtureRoot $Name
    $ffmpegBin = Join-Path $root 'ffmpeg\bin'
    New-Item -ItemType Directory -Path $ffmpegBin -Force | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $root 'payload') -Force | Out-Null

    $recorder = Join-Path $root 'clipped-recorder.exe'
    if ($WithRecorder) {
        Set-Content -LiteralPath $recorder -Value 'a recorder, for the purposes of this test' -Encoding Ascii
    }

    foreach ($library in $Libraries) {
        Set-Content -LiteralPath (Join-Path $ffmpegBin $library) -Value $library -Encoding Ascii
    }
    foreach ($program in $Programs) {
        Set-Content -LiteralPath (Join-Path $ffmpegBin $program) -Value $program -Encoding Ascii
    }

    $licences = Join-Path $root 'licences'
    if (-not $WithoutLicences) {
        New-Item -ItemType Directory -Path (Join-Path $licences 'ffmpeg') -Force | Out-Null
        Set-Content -LiteralPath (Join-Path $licences 'LICENSE.txt') -Value 'MPL-2.0, for the purposes of this test' -Encoding Ascii
        Set-Content -LiteralPath (Join-Path $licences 'THIRD-PARTY-NOTICES.md') -Value '# Notices' -Encoding Ascii
        Set-Content -LiteralPath (Join-Path $licences 'ffmpeg\LGPL-3.0.txt') -Value 'LGPL' -Encoding Ascii
    }

    return [pscustomobject]@{
        Recorder  = $recorder
        FfmpegDir = (Join-Path $root 'ffmpeg')
        Payload   = (Join-Path $root 'payload')
        Licences  = $licences
    }
}

function Invoke-Stage {
    <#
    .SYNOPSIS
        Runs the script under test against a fixture and reports what it left.
    #>
    param(
        [Parameter(Mandatory)] $Fixture,
        [string] $FfmpegDir
    )

    if (-not $FfmpegDir) { $FfmpegDir = $Fixture.FfmpegDir }

    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $output = & powershell -ExecutionPolicy Bypass -File $stageScript `
            -RecorderExecutable $Fixture.Recorder `
            -FfmpegDir $FfmpegDir `
            -PayloadDirectory $Fixture.Payload `
            -LicenceDirectory $Fixture.Licences 2>&1 | Out-String
        $code = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previous
    }

    $staged = @()
    if (Test-Path -LiteralPath $Fixture.Payload) {
        $staged = @(Get-ChildItem -LiteralPath $Fixture.Payload -File -Force | Select-Object -ExpandProperty Name | Sort-Object)
    }

    return [pscustomobject]@{
        Output   = $output
        ExitCode = $code
        Staged   = $staged
    }
}

function Assert-Case {
    param(
        [Parameter(Mandatory)] [string] $Name,
        [Parameter(Mandatory)] $Result,
        [Parameter(Mandatory)] [int] $ExpectedExitCode,
        [string[]] $Contains = @(),
        [string[]] $ExpectedStaged
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
    if ($PSBoundParameters.ContainsKey('ExpectedStaged')) {
        $expected = (@($ExpectedStaged) | Sort-Object) -join ', '
        $actual = (@($Result.Staged) | Sort-Object) -join ', '
        if ($expected -ne $actual) {
            $problems += "staged [$actual], expected [$expected]"
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
    Write-Host 'Refusing to build an installer that cannot record'

    $noRecorder = New-Fixture -Name 'no-recorder' -Libraries @('avcodec-62.dll', 'avutil-60.dll')
    Assert-Case `
        -Name 'a missing recorder stops the build, naming the file and where it was looked for' `
        -Result (Invoke-Stage -Fixture $noRecorder) `
        -ExpectedExitCode 1 `
        -Contains @(
        'clipped-recorder.exe',
        $noRecorder.Recorder,
        'cargo build --release -p clipped-recorder'
    ) `
        -ExpectedStaged @()

    # A refusal that has already copied something is not a refusal: the next
    # `tauri build` would collect whatever a previous run left here. So the
    # payload directory is required to be untouched, stale contents and all,
    # when the script stops.
    $staleThenNoRecorder = New-Fixture -Name 'stale-then-no-recorder' -Libraries @('avcodec-62.dll')
    Set-Content -LiteralPath (Join-Path $staleThenNoRecorder.Payload 'clipped-recorder.exe') -Value 'from a previous run' -Encoding Ascii
    Assert-Case `
        -Name 'a refusal leaves the payload alone rather than half-staging it' `
        -Result (Invoke-Stage -Fixture $staleThenNoRecorder) `
        -ExpectedExitCode 1 `
        -ExpectedStaged @('clipped-recorder.exe')

    # Issue #690: existence was the whole check, and it is not enough. `tauri
    # build` compiles clipped-desktop and never clipped-recorder — they are
    # separate Cargo workspaces — so a bundle succeeds without consulting the
    # recorder's sources at all. That shipped an installer in 47 seconds
    # carrying a recorder seven merged pull requests old, reported as a success.
    #
    # The fixture recorder is dated to 2020 rather than the sources being
    # touched, because the comparison is against this repository's real `.rs`
    # files: a test that wrote to `crates/` to make a point would be editing the
    # tree it is being run from.
    $staleRecorder = New-Fixture -Name 'stale-recorder' -WithRecorder -Libraries @('avcodec-62.dll')
    (Get-Item -LiteralPath $staleRecorder.Recorder).LastWriteTime = [datetime]'2020-01-01 00:00'
    Assert-Case `
        -Name 'a recorder older than the sources stops the build, rather than being shipped' `
        -Result (Invoke-Stage -Fixture $staleRecorder) `
        -ExpectedExitCode 1 `
        -Contains @(
        'a current clipped-recorder.exe',
        'cargo build --release -p clipped-recorder'
    ) `
        -ExpectedStaged @()

    # The other direction, which is what stops the check above being a refusal
    # nobody can clear: a recorder newer than every source stages exactly as it
    # did before. Without this the previous case passes just as well against a
    # script that refuses every build.
    $freshRecorder = New-Fixture -Name 'fresh-recorder' -WithRecorder -Libraries @('avcodec-62.dll')
    Assert-Case `
        -Name 'a recorder newer than the sources is staged, so the check is not a blanket refusal' `
        -Result (Invoke-Stage -Fixture $freshRecorder) `
        -ExpectedExitCode 0 `
        -ExpectedStaged @('clipped-recorder.exe', 'avcodec-62.dll')

    $noLibraries = New-Fixture -Name 'no-ffmpeg' -WithRecorder
    Assert-Case `
        -Name 'an FFmpeg build with no DLLs in it stops the build, naming the fetch script' `
        -Result (Invoke-Stage -Fixture $noLibraries) `
        -ExpectedExitCode 1 `
        -Contains @(
        (Join-Path $noLibraries.FfmpegDir 'bin'),
        'scripts/fetch-ffmpeg.ps1'
    ) `
        -ExpectedStaged @()

    $noFfmpegAtAll = New-Fixture -Name 'no-ffmpeg-at-all' -WithRecorder
    Assert-Case `
        -Name 'an FFmpeg directory that does not exist stops the build too' `
        -Result (Invoke-Stage -Fixture $noFfmpegAtAll -FfmpegDir (Join-Path $fixtureRoot 'nowhere')) `
        -ExpectedExitCode 1 `
        -Contains @('scripts/fetch-ffmpeg.ps1') `
        -ExpectedStaged @()

    # Issue #123: an installed copy carries FFmpeg's LGPL libraries and the
    # notices of several hundred crates, and distributing the first without the
    # second is not something the licences permit. So it is a refusal, in the
    # same shape as a missing recorder, rather than a warning nobody reads.
    $noLicences = New-Fixture -Name 'no-licences' -WithRecorder -Libraries @('avcodec-62.dll') -WithoutLicences
    Assert-Case `
        -Name 'missing notices stop the build, naming the script that produces them' `
        -Result (Invoke-Stage -Fixture $noLicences) `
        -ExpectedExitCode 1 `
        -Contains @(
        $noLicences.Licences,
        'scripts/collect-notices.ps1'
    ) `
        -ExpectedStaged @()

    Write-Host 'What travels with the recorder'

    $complete = New-Fixture -Name 'complete' `
        -WithRecorder `
        -Libraries @('avcodec-62.dll', 'avformat-62.dll', 'avutil-60.dll', 'swresample-6.dll') `
        -Programs @('ffmpeg.exe', 'ffplay.exe', 'ffprobe.exe', 'README.txt')

    Assert-Case `
        -Name 'the recorder and every DLL are staged, and the FFmpeg programs are not' `
        -Result (Invoke-Stage -Fixture $complete) `
        -ExpectedExitCode 0 `
        -ExpectedStaged @(
        'clipped-recorder.exe',
        'avcodec-62.dll',
        'avformat-62.dll',
        'avutil-60.dll',
        'swresample-6.dll'
    )

    # A DLL dropped from a moved pin, or a recorder renamed, would otherwise sit
    # in this directory and be shipped by every later build. The directory is a
    # build output and each run owns all of it.
    $stale = New-Fixture -Name 'stale' -WithRecorder -Libraries @('avcodec-62.dll')
    Set-Content -LiteralPath (Join-Path $stale.Payload 'avresample-4.dll') -Value 'from an older pin' -Encoding Ascii
    Set-Content -LiteralPath (Join-Path $stale.Payload 'notes.txt') -Value 'left by hand' -Encoding Ascii
    Assert-Case `
        -Name 'a file left by an earlier run is removed rather than shipped' `
        -Result (Invoke-Stage -Fixture $stale) `
        -ExpectedExitCode 0 `
        -ExpectedStaged @('clipped-recorder.exe', 'avcodec-62.dll')

    Write-Host 'Against the FFmpeg this repository carries'

    # The fixture cases above would pass against a filter that matched, say,
    # only files beginning with "av". This one runs against the real pinned
    # build, and what it expects is read out of that build rather than written
    # down here, so moving the pin moves the case with it instead of switching
    # it off.
    if (-not (Test-Path -LiteralPath (Join-Path $realFfmpegDir 'bin') -PathType Container)) {
        Write-Host '  FAIL  the pinned FFmpeg build is not installed, so the case that checks the real one cannot run' -ForegroundColor Red
        Write-Host "        expected it in $realFfmpegDir" -ForegroundColor Red
        Write-Host '        run scripts/fetch-ffmpeg.ps1' -ForegroundColor Red
        $failureCount++
    } else {
        $realLibraries = @(
            Get-ChildItem -LiteralPath (Join-Path $realFfmpegDir 'bin') -File |
                Where-Object { $_.Extension -eq '.dll' } |
                Select-Object -ExpandProperty Name
        )

        $againstTheRealBuild = New-Fixture -Name 'real-ffmpeg' -WithRecorder
        Assert-Case `
            -Name 'every DLL of the pinned build is staged, and nothing else from it is' `
            -Result (Invoke-Stage -Fixture $againstTheRealBuild -FfmpegDir $realFfmpegDir) `
            -ExpectedExitCode 0 `
            -ExpectedStaged (@('clipped-recorder.exe') + $realLibraries)
    }

    Write-Host 'What an installed copy is obliged to carry'

    $withLicences = New-Fixture -Name 'with-licences' -WithRecorder -Libraries @('avcodec-62.dll')
    $carried = Invoke-Stage -Fixture $withLicences
    $stagedLicences = Join-Path $withLicences.Payload 'licences'
    $problems = @()
    if ($carried.ExitCode -ne 0) { $problems += "exit code was $($carried.ExitCode), expected 0" }
    foreach ($expected in @('LICENSE.txt', 'THIRD-PARTY-NOTICES.md', 'ffmpeg\LGPL-3.0.txt')) {
        if (-not (Test-Path -LiteralPath (Join-Path $stagedLicences $expected) -PathType Leaf)) {
            $problems += "the installer payload has no $expected"
        }
    }
    if ($problems.Count -eq 0) {
        Write-Host '  PASS  the licence texts and notices reach the payload'
    } else {
        Write-Host '  FAIL  the licence texts and notices reach the payload' -ForegroundColor Red
        foreach ($problem in $problems) { Write-Host "        $problem" -ForegroundColor Red }
        $script:failureCount++
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
