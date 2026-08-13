#Requires -Version 5.1

<#
.SYNOPSIS
    Writes the body of a Clipped release: what the download is, that nothing
    signed it, and the SHA-256 to check it against.

.DESCRIPTION
    Two things a reader of an unsigned release needs, and neither can be left to
    whoever is drafting it at the time.

    The first is the warning. Clipped's installer is not code-signed, so Windows
    SmartScreen shows "Windows protected your PC" and offers only "Don't run"
    until the reader finds "More info". Somebody who has not been told that is
    left to decide, alone, whether they have been handed malware - and the
    honest answer, "this is what Windows shows for any installer without a paid
    certificate", belongs in the release rather than in a forum reply three days
    later.

    The second is the checksum, which is what makes the warning survivable: it
    is the only thing that lets a reader tell the file they downloaded from a
    file somebody else uploaded. It is computed here, from the installer being
    released, rather than pasted in - a hash that disagrees with its asset is
    worse than no hash, because it is trusted.

.PARAMETER Tag
    The release tag, `v` included. Used in the title and in the documentation
    links, so that they point at the tree the installer was built from rather
    than at whatever main says later.

.PARAMETER InstallerPath
    The installer being released. Its name and its SHA-256 go into the notes.

.PARAMETER OutFile
    Where to write the notes. `gh release create --notes-file` reads it.

.PARAMETER FfmpegPinPath
    The pin record scripts/fetch-ffmpeg.ps1 writes beside the installed FFmpeg
    (.clipped-ffmpeg-pin.json). When it is there, the notes name the exact
    FFmpeg build shipped, which is the fact the corresponding-source obligation
    turns on. Omitted rather than guessed when it is not.

.PARAMETER RepositoryUrl
    Base URL the documentation links are built from.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts/write-release-notes.ps1 `
        -Tag v1.0.0 -InstallerPath .\Clipped_1.0.0_x64-setup.exe -OutFile notes.md

.OUTPUTS
    Exit code 0 when the notes were written, 1 otherwise. Prints the SHA-256 it
    computed, so a build log records it independently of the release.
#>

[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $Tag,
    [Parameter(Mandatory)] [string] $InstallerPath,
    [Parameter(Mandatory)] [string] $OutFile,
    [string] $FfmpegPinPath = '',
    [string] $RepositoryUrl = 'https://github.com/wildware-uk/clipped'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if (-not (Test-Path -LiteralPath $InstallerPath -PathType Leaf)) {
    Write-Host ''
    Write-Host 'Release notes cannot be written.' -ForegroundColor Red
    Write-Host ''
    Write-Host "  Missing: the installer the notes are about"
    Write-Host "  Looked in: $InstallerPath"
    Write-Host ''
    Write-Host '  The SHA-256 in the notes is computed from the file being released. Notes'
    Write-Host '  written without it would publish a checksum of nothing.'
    Write-Host ''
    exit 1
}

$installer = Get-Item -LiteralPath $InstallerPath
$hash = (Get-FileHash -LiteralPath $installer.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
$megabytes = [math]::Round($installer.Length / 1MB, 1)

$ffmpeg = $null
if ($FfmpegPinPath -and (Test-Path -LiteralPath $FfmpegPinPath -PathType Leaf)) {
    $ffmpeg = Get-Content -LiteralPath $FfmpegPinPath -Raw | ConvertFrom-Json
}

$lines = @()
$lines += "Clipped $Tag for Windows x64."
$lines += ''
$lines += '## This build is not signed'
$lines += ''
$lines += 'Clipped is not code-signed. Windows SmartScreen will show **"Windows protected'
$lines += 'your PC"** and offer only **Don''t run**; the option to continue is behind'
$lines += '**More info** and then **Run anyway**.'
$lines += ''
$lines += 'That warning is what Windows shows for any installer without a paid'
$lines += 'code-signing certificate. It is not a statement that this file is malicious,'
$lines += 'and it is not a statement that it is safe either - nothing has vouched for it.'
$lines += 'What you can check yourself is that the file you downloaded is the file this'
$lines += 'release published:'
$lines += ''
$lines += '| Asset | Size | SHA-256 |'
$lines += '| --- | --- | --- |'
$lines += ("| ``{0}`` | {1} MB | ``{2}`` |" -f $installer.Name, $megabytes, $hash)
$lines += ''
$lines += '```powershell'
$lines += ("Get-FileHash .\{0} -Algorithm SHA256" -f $installer.Name)
$lines += '```'
$lines += ''
$lines += 'If that does not print the hash above, do not run the installer.'
$lines += ''
$lines += '## What is in it'
$lines += ''
$lines += 'Built from this tag by'
$lines += ("[.github/workflows/release.yml]({0}/blob/{1}/.github/workflows/release.yml) " -f $RepositoryUrl, $Tag)
$lines += 'on a GitHub-hosted Windows runner, from a commit CI had already passed on.'
$lines += 'The installer carries the desktop application, the recorder process it drives,'
$lines += 'and the FFmpeg libraries the recorder links.'
if ($ffmpeg -and $ffmpeg.PSObject.Properties.Name -contains 'asset') {
    $lines += ''
    $lines += ("FFmpeg build shipped: ``{0}``" -f $ffmpeg.asset)
}
$lines += ''
$lines += '## Licensing'
$lines += ''
$lines += ("Clipped is [MPL-2.0]({0}/blob/{1}/LICENSE). The installer also carries FFmpeg's" -f $RepositoryUrl, $Tag)
$lines += 'LGPL v3 libraries and several hundred permissively licensed Rust crates, and it'
$lines += 'installs the licence texts and third-party notices those require alongside the'
$lines += 'application.'
$lines += ("[docs/licensing.md]({0}/blob/{1}/docs/licensing.md) sets out what a release" -f $RepositoryUrl, $Tag)
$lines += 'carries and why, including where the corresponding source of the exact FFmpeg'
$lines += 'build shipped here can be obtained.'
$lines += ''
$lines += 'No patent licence comes with this download. Distributing software that encodes'
$lines += 'H.264 or HEVC has patent-pool implications no copyright licence addresses;'
$lines += ("[ADR 0008]({0}/blob/{1}/docs/adr/0008-codec-patent-position.md) is the project's" -f $RepositoryUrl, $Tag)
$lines += 'position.'
$lines += ''

$directory = Split-Path -Parent $OutFile
if ($directory -and -not (Test-Path -LiteralPath $directory -PathType Container)) {
    New-Item -ItemType Directory -Path $directory -Force | Out-Null
}

# UTF-8 without a BOM: the bytes become the release body verbatim, and a BOM
# would appear as stray characters at the top of the first heading.
[System.IO.File]::WriteAllText($OutFile, (($lines -join "`n") + "`n"), (New-Object System.Text.UTF8Encoding $false))

Write-Host ''
Write-Host "Release notes for $Tag written to $OutFile"
Write-Host ''
Write-Host ("  Asset:   {0}" -f $installer.Name)
Write-Host ("  Size:    {0:N0} bytes" -f $installer.Length)
Write-Host ("  SHA-256: {0}" -f $hash)
Write-Host ''
exit 0
