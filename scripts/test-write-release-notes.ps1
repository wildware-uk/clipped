#Requires -Version 5.1

<#
.SYNOPSIS
    Tests scripts/write-release-notes.ps1: that the checksum it publishes is the
    checksum of the file being released, and that the notes say what an unsigned
    download obliges them to say.

.DESCRIPTION
    A published SHA-256 is trusted, which is what makes a wrong one worse than
    none: a reader who checks and matches has concluded something false, and a
    reader who checks and does not match concludes the mirror is compromised.
    The first case below is therefore not "a hash appears" but "the hash in the
    notes is the hash of these bytes", computed independently and asserted
    against a file whose contents the test chose.

    The SmartScreen paragraph is asserted for the same reason it exists. Windows
    will tell whoever downloads this that it protected them from it, and a
    release that does not explain why leaves somebody deciding alone whether
    they have been handed malware. It is not decoration that can quietly go
    missing in an edit.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts/test-write-release-notes.ps1

.OUTPUTS
    Exit code 0 when every case passes, 1 otherwise.
#>

[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$notesScript = Join-Path $PSScriptRoot 'write-release-notes.ps1'
$fixtureRoot = Join-Path ([System.IO.Path]::GetTempPath()) "clipped-release-notes-fixtures-$PID"
$failureCount = 0

function Invoke-Notes {
    param(
        [Parameter(Mandatory)] [string] $Tag,
        [Parameter(Mandatory)] [string] $InstallerPath,
        [Parameter(Mandatory)] [string] $OutFile,
        [string] $FfmpegPinPath = '',
        [string] $CorrespondingSourceDirectory = ''
    )

    $arguments = @(
        '-ExecutionPolicy', 'Bypass', '-File', $notesScript,
        '-Tag', $Tag,
        '-InstallerPath', $InstallerPath,
        '-OutFile', $OutFile
    )
    if ($FfmpegPinPath) { $arguments += @('-FfmpegPinPath', $FfmpegPinPath) }
    if ($CorrespondingSourceDirectory) { $arguments += @('-CorrespondingSourceDirectory', $CorrespondingSourceDirectory) }

    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $output = & powershell @arguments 2>&1 | Out-String
        $code = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previous
    }

    $notes = ''
    if (Test-Path -LiteralPath $OutFile -PathType Leaf) {
        $notes = Get-Content -LiteralPath $OutFile -Raw
    }

    return [pscustomobject]@{
        Output   = $output
        ExitCode = $code
        Notes    = $notes
    }
}

function Assert-That {
    param(
        [Parameter(Mandatory)] [string] $Name,
        [Parameter(Mandatory)] [bool] $Condition,
        [string] $Detail = ''
    )

    if ($Condition) {
        Write-Host "  PASS  $Name"
        return
    }
    Write-Host "  FAIL  $Name" -ForegroundColor Red
    if ($Detail) { Write-Host "        $Detail" -ForegroundColor Red }
    $script:failureCount++
}

New-Item -ItemType Directory -Path $fixtureRoot -Force | Out-Null

try {
    Write-Host 'The published checksum is the checksum of the published file'

    # Bytes chosen here rather than by the script, so that the hash asserted
    # below is arrived at independently of anything the script did.
    $installer = Join-Path $fixtureRoot 'Clipped_1.0.0_x64-setup.exe'
    [System.IO.File]::WriteAllBytes($installer, [byte[]](1..250))
    $expectedHash = (Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash.ToLowerInvariant()

    $outFile = Join-Path $fixtureRoot 'notes.md'
    $result = Invoke-Notes -Tag 'v1.0.0' -InstallerPath $installer -OutFile $outFile

    Assert-That -Name 'it writes the notes and exits 0' -Condition ($result.ExitCode -eq 0) -Detail $result.Output
    Assert-That `
        -Name 'the notes carry the SHA-256 of the installer being released' `
        -Condition ($result.Notes -like "*$expectedHash*") `
        -Detail "expected $expectedHash in the notes"
    Assert-That `
        -Name 'the build log records the same hash independently of the release' `
        -Condition ($result.Output -like "*$expectedHash*")
    Assert-That `
        -Name 'the asset is named, so the hash is attached to something' `
        -Condition ($result.Notes -like '*Clipped_1.0.0_x64-setup.exe*')
    Assert-That `
        -Name 'the reader is told how to check it' `
        -Condition ($result.Notes -like '*Get-FileHash*' -and $result.Notes -like '*do not run the installer*')

    # The hash has to be of the installer, not of something correlated with it.
    # A second file one byte different must produce different notes; a script
    # that hashed the wrong thing would pass every assertion above.
    $other = Join-Path $fixtureRoot 'Clipped_1.0.1_x64-setup.exe'
    [System.IO.File]::WriteAllBytes($other, [byte[]](1..249))
    $otherResult = Invoke-Notes -Tag 'v1.0.1' -InstallerPath $other -OutFile (Join-Path $fixtureRoot 'notes-other.md')
    Assert-That `
        -Name 'a different installer produces a different checksum' `
        -Condition ($otherResult.Notes -notlike "*$expectedHash*")

    Write-Host ''
    Write-Host 'The notes say what an unsigned build obliges them to say'

    Assert-That `
        -Name 'they say the build is not signed' `
        -Condition ($result.Notes -like '*not code-signed*')
    Assert-That `
        -Name 'they say SmartScreen will warn, and what the warning looks like' `
        -Condition ($result.Notes -like '*SmartScreen*' -and $result.Notes -like '*Windows protected*')
    Assert-That `
        -Name 'they say how to get past it, rather than leaving somebody stuck' `
        -Condition ($result.Notes -like '*More info*' -and $result.Notes -like '*Run anyway*')
    Assert-That `
        -Name 'they do not claim the file is safe' `
        -Condition ($result.Notes -like '*nothing has vouched for it*')
    Assert-That `
        -Name 'they point at the licensing the installer carries' `
        -Condition ($result.Notes -like '*docs/licensing.md*' -and $result.Notes -like '*LGPL v3*')
    Assert-That `
        -Name 'the documentation links point at the tag, not at whatever main says later' `
        -Condition ($result.Notes -like '*/blob/v1.0.0/docs/licensing.md*')

    Write-Host ''
    Write-Host 'Facts it does not have, it does not invent'

    Assert-That `
        -Name 'with no pin record, no FFmpeg build is named' `
        -Condition ($result.Notes -notlike '*FFmpeg build shipped*')

    $pin = Join-Path $fixtureRoot '.clipped-ffmpeg-pin.json'
    Set-Content -LiteralPath $pin -Value '{"asset":"ffmpeg-n8.1.2-34-g9b6c8969e0-win64-lgpl-shared-8.1.zip"}' -Encoding Ascii
    $withPin = Invoke-Notes -Tag 'v1.0.0' -InstallerPath $installer -OutFile (Join-Path $fixtureRoot 'notes-pin.md') -FfmpegPinPath $pin
    Assert-That `
        -Name 'with a pin record, the exact FFmpeg build is named' `
        -Condition ($withPin.Notes -like '*ffmpeg-n8.1.2-34-g9b6c8969e0-win64-lgpl-shared-8.1.zip*')

    Write-Host ''
    Write-Host 'The source published beside the installer is named in the notes'

    # A release page with an installer and two zips on it, and notes that do not
    # say what the zips are, leaves the reader who is owed the source to guess.
    # The obligation is discharged by publishing it *and* by the recipient being
    # able to find it.
    $sourceDirectory = Join-Path $fixtureRoot 'ffmpeg-source'
    New-Item -ItemType Directory -Path $sourceDirectory -Force | Out-Null
    Set-Content -LiteralPath (Join-Path $sourceDirectory 'CORRESPONDING-SOURCE.md') -Value '# the manifest' -Encoding Ascii
    [System.IO.File]::WriteAllBytes((Join-Path $sourceDirectory 'ffmpeg-9b6c8969e0-source.zip'), (New-Object byte[] 2048))
    [System.IO.File]::WriteAllBytes((Join-Path $sourceDirectory 'ffmpeg-builds-autobuild-2026-08-09-13-03-source.zip'), (New-Object byte[] 1024))

    $withSource = Invoke-Notes -Tag 'v1.0.0' -InstallerPath $installer -OutFile (Join-Path $fixtureRoot 'notes-source.md') -CorrespondingSourceDirectory $sourceDirectory
    Assert-That `
        -Name 'the source assets are named, so a reader can find them among the downloads' `
        -Condition ($withSource.Notes -like '*ffmpeg-9b6c8969e0-source.zip*' -and
            $withSource.Notes -like '*ffmpeg-builds-autobuild-2026-08-09-13-03-source.zip*' -and
            $withSource.Notes -like '*CORRESPONDING-SOURCE.md*')
    Assert-That `
        -Name 'and the notes say they are on this page rather than obtainable somewhere' `
        -Condition ($withSource.Notes -like '*published as assets on this release*')

    # Facts it does not have, it does not invent - the same rule the pin record
    # follows. Notes claiming the source is attached when it is not would be a
    # false statement on a page nobody can recall.
    Assert-That `
        -Name 'with no source directory, the notes do not claim the source is attached' `
        -Condition ($result.Notes -notlike '*published as assets on this release*')

    $emptyDirectory = Join-Path $fixtureRoot 'ffmpeg-source-empty'
    New-Item -ItemType Directory -Path $emptyDirectory -Force | Out-Null
    $emptySource = Invoke-Notes -Tag 'v1.0.0' -InstallerPath $installer -OutFile (Join-Path $fixtureRoot 'notes-empty-source.md') -CorrespondingSourceDirectory $emptyDirectory
    Assert-That `
        -Name 'a directory without the manifest in it is not read as source having been published' `
        -Condition ($emptySource.Notes -notlike '*published as assets on this release*')

    Write-Host ''
    Write-Host 'No installer, no notes'

    # A release whose notes were written without the installer would publish a
    # checksum of nothing, which is the one failure that looks exactly like
    # success from the outside.
    $missingOut = Join-Path $fixtureRoot 'notes-missing.md'
    $missing = Invoke-Notes -Tag 'v1.0.0' -InstallerPath (Join-Path $fixtureRoot 'absent.exe') -OutFile $missingOut
    Assert-That -Name 'a missing installer is a refusal' -Condition ($missing.ExitCode -eq 1)
    Assert-That `
        -Name 'the refusal names the file it looked for' `
        -Condition ($missing.Output -like '*absent.exe*' -and $missing.Output -like '*checksum of nothing*')
    Assert-That -Name 'and no notes are left behind' -Condition (-not (Test-Path -LiteralPath $missingOut))
} finally {
    Remove-Item -LiteralPath $fixtureRoot -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host ''
if ($failureCount -gt 0) {
    Write-Host "$failureCount case(s) failed." -ForegroundColor Red
    exit 1
}

Write-Host 'Every case passed.' -ForegroundColor Green
exit 0
