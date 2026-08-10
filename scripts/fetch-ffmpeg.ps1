#Requires -Version 5.1

<#
.SYNOPSIS
    Downloads and extracts the FFmpeg build that Clipped links against.

.DESCRIPTION
    Clipped links dynamically against a prebuilt, LGPL-only FFmpeg rather than
    compiling FFmpeg itself. The build is pinned to one immutable release asset
    and verified against a SHA-256 checksum recorded in this script, so every
    machine and every CI run links against the same bytes. See
    docs/adr/0004-ffmpeg-dependency-strategy.md for why, and docs/ffmpeg.md for
    how to move the pin.

    The script is idempotent. A second run over an intact installation checks
    the recorded pin and exits without touching the network, which is what makes
    it usable as an unconditional step in CI and in a contributor's shell alike.

    Nothing here is interactive and nothing is written outside the destination
    directory, except that -PersistEnvironment writes the FFmpeg environment
    variables to the user environment and, under GitHub Actions, they are
    appended to the file named by GITHUB_ENV so later steps inherit them.

    The upstream project publishes no checksums of its own. The checksum
    defaulted here was computed from the asset that was reviewed when the pin
    was chosen, and the release tag it names is a dated, immutable one rather
    than the rolling "latest" tag, so a changed checksum means the artefact
    changed underneath us and is worth stopping for.

.PARAMETER Tag
    Release tag in the builds repository. Must be a dated autobuild tag such as
    autobuild-2026-08-09-13-03, never "latest": "latest" moves daily, which
    would make the checksum below wrong tomorrow and the build irreproducible.

.PARAMETER Asset
    File name of the release asset. Must be a win64 "lgpl-shared" build. A
    "gpl" build would put GPL components into a binary Clipped distributes
    under MPL-2.0, and a non-shared build would defeat the LGPL relinking
    permission that dynamic linking is relied on to satisfy.

.PARAMETER Sha256
    Expected SHA-256 of the asset, as lowercase or uppercase hex.

.PARAMETER BaseUri
    Release download root. Exposed so the fetch can be pointed at a mirror or,
    in the script's own failure-path check, at a local file:// URI.

.PARAMETER Destination
    Directory the build is extracted into, defaulting to third-party/ffmpeg in
    the repository. Each pinned build lands in its own subdirectory named after
    the asset, so switching pins does not overwrite a working installation and
    CI can cache this directory keyed on the asset name.

.PARAMETER Force
    Re-download and re-extract even when the pinned build is already present.

.PARAMETER PersistEnvironment
    Also write the FFmpeg environment variables to the current user's
    environment, so new shells find them without re-running this script. Off by
    default because writing to a contributor's environment behind their back is
    rude.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts/fetch-ffmpeg.ps1

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts/fetch-ffmpeg.ps1 -PersistEnvironment

.OUTPUTS
    Exit code 0 when the pinned build is present and verified, 1 otherwise.
#>

[CmdletBinding()]
param(
    [string] $Tag = 'autobuild-2026-08-09-13-03',
    [string] $Asset = 'ffmpeg-n8.1.2-34-g9b6c8969e0-win64-lgpl-shared-8.1.zip',
    [string] $Sha256 = '2936e5449886641b4279ca3fc554b678c8e9a2d20dd0c0a34fe7208b254a0905',
    [string] $BaseUri = 'https://github.com/BtbN/FFmpeg-Builds/releases/download',
    [string] $Destination = '',
    [switch] $Force,
    [switch] $PersistEnvironment
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Windows PowerShell does not populate $PSScriptRoot while binding parameter
# defaults, so the repository-relative default is filled in here instead.
$repositoryRoot = Split-Path -Parent $PSScriptRoot
if (-not $Destination) { $Destination = Join-Path $repositoryRoot 'third-party\ffmpeg' }

# Windows PowerShell 5.1 defaults to a TLS version GitHub no longer accepts,
# and renders download progress so slowly that it dominates the transfer.
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$ProgressPreference = 'SilentlyContinue'

$buildName = [System.IO.Path]::GetFileNameWithoutExtension($Asset)
$buildRoot = Join-Path $Destination $buildName
$pinFile = Join-Path $buildRoot '.clipped-ffmpeg-pin.json'
$downloadUri = "$BaseUri/$Tag/$Asset"

# One header, one library and one import library, named exactly as the build
# lays them out. Their presence is what "extracted successfully" means: an
# archive that unpacked but is missing the import libraries would otherwise be
# discovered as a linker error much later.
$requiredPaths = @(
    'include\libavformat\avformat.h',
    'include\libavcodec\avcodec.h',
    'include\libavutil\avutil.h',
    'lib\avformat.lib',
    'lib\avcodec.lib',
    'lib\avutil.lib',
    'LICENSE.txt'
)

function Write-Step {
    param([Parameter(Mandatory)] [string] $Message)
    Write-Host $Message
}

function Test-InstalledPin {
    <#
    .SYNOPSIS
        Reports whether the pinned build is already extracted and intact.
    .DESCRIPTION
        Both halves matter. The recorded pin proves the directory holds the
        build this script is currently pinned to rather than a previous one,
        and the file check proves a half-extracted or partly deleted directory
        is not mistaken for a good installation.

        The whole of the record's interpretation sits inside the try, not just
        the parse. Set-StrictMode makes reading a property a pin record does not
        have a terminating error, so a record written by an older version of
        this script - or truncated by a machine that lost power mid-write -
        would otherwise abort the run instead of being treated as what it is: a
        reason to fetch again.
    #>
    if (-not (Test-Path -LiteralPath $pinFile)) { return $false }

    try {
        $recorded = Get-Content -LiteralPath $pinFile -Raw | ConvertFrom-Json
        if ($recorded.tag -ne $Tag -or $recorded.asset -ne $Asset) { return $false }
        if ($recorded.sha256 -ne $Sha256.ToLowerInvariant()) { return $false }
    } catch {
        Write-Step "Ignoring unusable pin record at $pinFile ($($_.Exception.Message)); fetching again."
        return $false
    }

    foreach ($relative in $requiredPaths) {
        if (-not (Test-Path -LiteralPath (Join-Path $buildRoot $relative))) {
            Write-Step "Existing installation is missing $relative; fetching again."
            return $false
        }
    }

    return $true
}

function Get-PinnedArchive {
    <#
    .SYNOPSIS
        Downloads the asset to a temporary file and verifies its checksum.
    .OUTPUTS
        Path to the verified archive. Throws, leaving nothing behind, when the
        checksum does not match.
    #>
    param([Parameter(Mandatory)] [string] $StagingDirectory)

    $archive = Join-Path $StagingDirectory $Asset

    Write-Step "Downloading $Asset"
    Write-Step "  from $downloadUri"
    try {
        Invoke-WebRequest -Uri $downloadUri -OutFile $archive -UseBasicParsing
    } catch {
        throw "Downloading $downloadUri failed: $($_.Exception.Message)"
    }

    $actual = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    $expected = $Sha256.ToLowerInvariant()
    if ($actual -ne $expected) {
        Remove-Item -LiteralPath $archive -Force -ErrorAction SilentlyContinue
        throw @"
SHA-256 mismatch for $Asset.
  expected $expected
  actual   $actual
  from     $downloadUri

The downloaded file was deleted. A dated release tag is immutable, so a
mismatch means either the download was corrupted - retry once - or the artefact
is not the one this pin was verified against, which is not something to build
on. If the pin is being moved deliberately, update -Tag, -Asset and -Sha256
together in scripts/fetch-ffmpeg.ps1 and in the expectations under
crates/muxer/tests, as docs/ffmpeg.md describes.
"@
    }

    Write-Step "Checksum verified: $actual"
    return $archive
}

function Expand-PinnedArchive {
    <#
    .SYNOPSIS
        Extracts the archive and moves its single root directory into place.
    .DESCRIPTION
        Extraction goes to a staging directory first so that an interrupted or
        failed run cannot leave a partial tree where a later run would find the
        pin record absent but the directory present. The archive's own root
        directory is discovered rather than assumed, so the layout stays a
        property of the artefact instead of a second thing to keep in step.
    #>
    param(
        [Parameter(Mandatory)] [string] $Archive,
        [Parameter(Mandatory)] [string] $StagingDirectory
    )

    $unpacked = Join-Path $StagingDirectory 'unpacked'
    Write-Step "Extracting into $buildRoot"

    # ZipFile is used in preference to Expand-Archive because Windows
    # PowerShell 5.1's implementation takes minutes over an archive this size.
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [System.IO.Compression.ZipFile]::ExtractToDirectory($Archive, $unpacked)

    $roots = @(Get-ChildItem -LiteralPath $unpacked -Force)
    if ($roots.Count -ne 1 -or -not $roots[0].PSIsContainer) {
        throw "Expected $Asset to contain exactly one root directory, found $($roots.Count) entries."
    }

    if (Test-Path -LiteralPath $buildRoot) {
        Remove-Item -LiteralPath $buildRoot -Recurse -Force
    }
    New-Item -ItemType Directory -Path $Destination -Force | Out-Null
    Move-Item -LiteralPath $roots[0].FullName -Destination $buildRoot
}

function Assert-ExtractedLayout {
    <#
    .SYNOPSIS
        Fails loudly when the extracted build is not the shape we link against.
    #>
    $missing = @($requiredPaths | Where-Object { -not (Test-Path -LiteralPath (Join-Path $buildRoot $_)) })
    if ($missing.Count -gt 0) {
        throw @"
$Asset extracted, but these expected files are absent:
  $($missing -join "`n  ")

This is the shape of a shared "lgpl-shared" build. A static or differently
packaged asset will not link. Check the -Asset name.
"@
    }
}

function Write-PinRecord {
    <#
    .SYNOPSIS
        Records what was installed, so a later run can skip the network.
    #>
    $record = [ordered]@{
        tag        = $Tag
        asset      = $Asset
        sha256     = $Sha256.ToLowerInvariant()
        source     = $downloadUri
        fetchedUtc = (Get-Date).ToUniversalTime().ToString('o')
    }
    $record | ConvertTo-Json | Set-Content -LiteralPath $pinFile -Encoding UTF8
}

function Publish-FfmpegEnvironment {
    <#
    .SYNOPSIS
        Reports the variables the build needs and, where asked to, persists them.
    .DESCRIPTION
        `rusty_ffmpeg` takes one variable per question rather than one prefix:
        FFMPEG_INCLUDE_DIR is where bindgen reads the headers,
        FFMPEG_LIBS_DIR is where the linker finds the import libraries, and
        FFMPEG_LINK_MODE selects dynamic linking - which is not the default, and
        is the whole basis of Clipped's LGPL position
        (docs/adr/0004-ffmpeg-dependency-strategy.md).

        FFMPEG_DIR is Clipped's own: it names the prefix, and
        crates/muxer/build.rs reads it to find the runtime DLLs in bin/ and copy
        them beside the binaries. All four are derived here from one path so
        they cannot disagree.

        PATH is deliberately left alone; the DLL copying is what makes it
        unnecessary.
    #>
    $variables = [ordered]@{
        FFMPEG_DIR         = $buildRoot
        FFMPEG_INCLUDE_DIR = (Join-Path $buildRoot 'include')
        FFMPEG_LIBS_DIR    = (Join-Path $buildRoot 'lib')
        FFMPEG_LINK_MODE   = 'dynamic'
    }

    foreach ($name in $variables.Keys) {
        Set-Item -LiteralPath "env:$name" -Value $variables[$name]
    }

    if ($PersistEnvironment) {
        foreach ($name in $variables.Keys) {
            [Environment]::SetEnvironmentVariable($name, $variables[$name], 'User')
        }
        Write-Step 'Wrote the FFmpeg variables to the user environment; new shells will pick them up.'
    }

    if ($env:GITHUB_ENV) {
        foreach ($name in $variables.Keys) {
            Add-Content -LiteralPath $env:GITHUB_ENV -Value "$name=$($variables[$name])"
        }
        Write-Step 'Exported the FFmpeg variables through GITHUB_ENV for later workflow steps.'
    }

    Write-Host ''
    Write-Host 'FFmpeg is ready. Set these before building, if they are not set already:'
    Write-Host ''
    foreach ($name in $variables.Keys) {
        Write-Host "    `$env:$name = '$($variables[$name])'"
    }
    Write-Host ''
    if (-not $PersistEnvironment) {
        Write-Host 'Re-run with -PersistEnvironment to write them to your user environment once and stop thinking about it.'
    }
}

function Write-StaleBuildNotice {
    <#
    .SYNOPSIS
        Names previously fetched builds that the current pin has left behind.
    #>
    if (-not (Test-Path -LiteralPath $Destination)) { return }

    $stale = @(Get-ChildItem -LiteralPath $Destination -Directory -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -ne $buildName })
    if ($stale.Count -eq 0) { return }

    Write-Host ''
    Write-Host "Older FFmpeg builds are still taking up space in $Destination and can be deleted:"
    foreach ($directory in $stale) { Write-Host "    $($directory.Name)" }
}

# Failures below are contributor-facing rather than programmer-facing, so they
# are reported as the message and nothing else. A bare `throw` from a script
# renders the PowerShell exception header, the offending line and a CategoryInfo
# block around the text, and the reader has to find the sentence that matters
# inside that.
try {
    if ((Test-InstalledPin) -and -not $Force) {
        Write-Step "$buildName is already present and matches the pin; nothing to download."
        Publish-FfmpegEnvironment
        Write-StaleBuildNotice
        exit 0
    }

    $staging = Join-Path $Destination ".staging-$([System.Guid]::NewGuid().ToString('n'))"
    New-Item -ItemType Directory -Path $staging -Force | Out-Null
    try {
        $archive = Get-PinnedArchive -StagingDirectory $staging
        Expand-PinnedArchive -Archive $archive -StagingDirectory $staging
    } finally {
        Remove-Item -LiteralPath $staging -Recurse -Force -ErrorAction SilentlyContinue
    }

    Assert-ExtractedLayout
    Write-PinRecord

    Write-Step "Installed $buildName"
    Publish-FfmpegEnvironment
    Write-StaleBuildNotice
    exit 0
} catch {
    Write-Host ''
    Write-Host $_.Exception.Message -ForegroundColor Red
    exit 1
}
