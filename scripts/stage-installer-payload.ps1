#Requires -Version 5.1

<#
.SYNOPSIS
    Puts the recorder and the FFmpeg runtime libraries where the installer will
    collect them, and refuses to let an installer be built without them.

.DESCRIPTION
    The desktop application is a client of a separate recorder process: it looks
    for clipped-recorder.exe beside its own executable and reports honestly when
    it is not there (ADR 0002, ADR 0006). An installer that carries only the
    window therefore installs a Clipped that records nothing, every time
    (issue #226). This script is what puts the second executable - and the
    libraries it links - into the bundle.

    It stages rather than merely checking, because two different kinds of file
    have to end up in one directory beside clipped-desktop.exe:

    - clipped-recorder.exe, built by `cargo build --release -p clipped-recorder`;
    - the FFmpeg DLLs from the pinned build that scripts/fetch-ffmpeg.ps1
      installs. The recorder links FFmpeg dynamically (ADR 0004), and Windows
      resolves a DLL from the directory of the executable that needs it, so
      without them the installed recorder does not start at all.

    Everything in the FFmpeg build's `bin` with a .dll extension is staged, for
    the reason crates/ffmpeg-runtime gives: libavformat loads its siblings, and
    the set changes with the FFmpeg version, so a hand-written list here would
    be a second copy of the pin to keep in step and would fail as a missing-DLL
    dialogue on a user's machine rather than as a build error here. The programs
    in that directory - ffmpeg.exe, ffplay.exe, ffprobe.exe - are deliberately
    not staged: nothing in Clipped shells out to them, they are test tools
    (docs/ffmpeg.md), and shipping a program nobody runs is a licence obligation
    taken on for nothing.

    It does not stage the licence texts or the third-party notices. Those are
    issue #123, and scripts/collect-notices.ps1 already produces them.

.NOTES
    Why the payload is staged into a directory of its own, rather than
    tauri.conf.json naming target\release\clipped-recorder.exe and the FFmpeg
    `bin` directly:

    `tauri-build` copies every declared resource into the Cargo target directory
    from the window crate's build script, so it runs on *every* `cargo build` of
    that crate - including `cargo clippy`, `cargo test`, and the two of those CI
    runs in its Desktop UI job. A declared resource that does not exist is a hard
    error there, and a glob that matches nothing is a hard error too
    (tauri_utils::resources::ResourcePaths). Naming the recorder or the FFmpeg
    `bin` directly would therefore make `cargo test --manifest-path
    apps/desktop/src-tauri/Cargo.toml` require a release build of the recorder
    and a fetched FFmpeg, neither of which that job has or needs.

    A directory that exists and is empty is the one shape that degrades safely:
    ResourcePaths skips it. So the window crate's build script creates this
    directory, an ordinary `cargo build` of the window finds it empty and copies
    nothing, and `tauri build` fills it first through
    tauri.conf.json's beforeBuildCommand - which is where this script runs, so
    that the refusal below reaches anybody who builds an installer, not only
    somebody who typed `npm run build:app`.

.PARAMETER RecorderExecutable
    The recorder to stage. Defaults to target\release\clipped-recorder.exe in
    this repository - a release build, because that is what an installer ships.

.PARAMETER FfmpegDir
    Root of the FFmpeg build whose `bin\*.dll` travel with the recorder.
    Defaults to the FFMPEG_DIR environment variable when it is set, so that a
    machine pointed at an FFmpeg of its own stages that one, and otherwise to
    third-party\ffmpeg\current, which is where scripts/fetch-ffmpeg.ps1 installs
    the pin and what .cargo/config.toml names.

.PARAMETER PayloadDirectory
    Where to stage them. Defaults to apps\desktop\src-tauri\installer-payload,
    which tauri.conf.json's `bundle.resources` maps to the installation
    directory.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts/stage-installer-payload.ps1

.OUTPUTS
    Exit code 0 when the payload is staged, 1 when something it needs is
    missing. Prints one line per staged file, so a build log records exactly
    what the installer was given.
#>

[CmdletBinding()]
param(
    [string] $RecorderExecutable,
    [string] $FfmpegDir,
    [string] $PayloadDirectory
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repositoryRoot = Split-Path -Parent $PSScriptRoot

if (-not $RecorderExecutable) {
    $RecorderExecutable = Join-Path $repositoryRoot 'target\release\clipped-recorder.exe'
}

if (-not $FfmpegDir) {
    $FfmpegDir = if ($env:FFMPEG_DIR) {
        $env:FFMPEG_DIR
    } else {
        Join-Path $repositoryRoot 'third-party\ffmpeg\current'
    }
}

if (-not $PayloadDirectory) {
    $PayloadDirectory = Join-Path $repositoryRoot 'apps\desktop\src-tauri\installer-payload'
}

function Write-Refusal {
    <#
    .SYNOPSIS
        Says what is missing, where it was looked for and what produces it.
    .DESCRIPTION
        Three separate facts, because any one of them alone leaves somebody
        guessing: a name is not a path, a path is not a remedy, and a remedy
        without the path cannot be checked (AGENTS.md section 15).
    #>
    param(
        [Parameter(Mandatory)] [string] $Missing,
        [Parameter(Mandatory)] [string] $LookedIn,
        [Parameter(Mandatory)] [string] $Remedy,
        [Parameter(Mandatory)] [string] $Consequence
    )

    Write-Host ''
    Write-Host 'The Clipped installer cannot be built.' -ForegroundColor Red
    Write-Host ''
    Write-Host "  Missing: $Missing"
    Write-Host "  Looked in: $LookedIn"
    Write-Host ''
    Write-Host '  Run this first:'
    Write-Host ''
    Write-Host "      $Remedy"
    Write-Host ''
    Write-Host "  $Consequence"
    Write-Host ''
}

# The recorder first, because it is the one an installer has never carried and
# the one whose absence issue #226 is about.
if (-not (Test-Path -LiteralPath $RecorderExecutable -PathType Leaf)) {
    Write-Refusal `
        -Missing 'clipped-recorder.exe, the recording process the desktop application starts' `
        -LookedIn $RecorderExecutable `
        -Remedy 'cargo build --release -p clipped-recorder' `
        -Consequence ('An installer without it installs a Clipped that reports the recorder missing and records nothing.')
    exit 1
}

$ffmpegBin = Join-Path $FfmpegDir 'bin'
$ffmpegLibraries = @()
if (Test-Path -LiteralPath $ffmpegBin -PathType Container) {
    # By extension rather than by -Filter: the FileSystem provider's filter is
    # the Win32 one, which also matches a file's 8.3 short name, so '*.dll'
    # there can match something that is not a DLL at all.
    $ffmpegLibraries = @(
        Get-ChildItem -LiteralPath $ffmpegBin -File |
            Where-Object { $_.Extension -eq '.dll' } |
            Sort-Object Name
    )
}

if ($ffmpegLibraries.Count -eq 0) {
    Write-Refusal `
        -Missing 'the FFmpeg runtime libraries (bin\*.dll) the recorder links against' `
        -LookedIn $ffmpegBin `
        -Remedy 'powershell -ExecutionPolicy Bypass -File scripts/fetch-ffmpeg.ps1' `
        -Consequence ('The recorder links FFmpeg dynamically, so without these beside it the installed recorder does not start at all.')
    exit 1
}

# Staged afresh every time. A DLL that the pin dropped, or a recorder from an
# older build, would otherwise sit here and be shipped: this directory is a
# build output, and the only thing that should be in it is what this run put
# there.
if (Test-Path -LiteralPath $PayloadDirectory) {
    Get-ChildItem -LiteralPath $PayloadDirectory -Force | Remove-Item -Recurse -Force
} else {
    New-Item -ItemType Directory -Path $PayloadDirectory -Force | Out-Null
}

$staged = @()
foreach ($source in @($RecorderExecutable) + $ffmpegLibraries.FullName) {
    $name = Split-Path -Leaf $source
    Copy-Item -LiteralPath $source -Destination (Join-Path $PayloadDirectory $name) -Force
    $staged += Get-Item -LiteralPath (Join-Path $PayloadDirectory $name)
}

Write-Host "Staged for the installer, beside clipped-desktop.exe, in $PayloadDirectory"
Write-Host ''
Write-Host ("  {0,-24} {1,12}  {2}" -f 'File', 'Bytes', 'From')
foreach ($file in $staged) {
    $from = if ($file.Name -eq (Split-Path -Leaf $RecorderExecutable)) {
        Split-Path -Parent $RecorderExecutable
    } else {
        $ffmpegBin
    }
    Write-Host ("  {0,-24} {1,12:N0}  {2}" -f $file.Name, $file.Length, $from)
}

# Which FFmpeg this is, recorded by the fetch script when it installed the pin.
# A build log that says only "seven DLLs" does not say which build shipped, and
# that is the question the corresponding-source obligation turns on
# (docs/licensing.md, issue #123).
$pinFile = Join-Path $FfmpegDir '.clipped-ffmpeg-pin.json'
if (Test-Path -LiteralPath $pinFile -PathType Leaf) {
    $pin = Get-Content -LiteralPath $pinFile -Raw | ConvertFrom-Json
    Write-Host ''
    Write-Host "  FFmpeg build: $($pin.asset)"
}

Write-Host ''
exit 0
