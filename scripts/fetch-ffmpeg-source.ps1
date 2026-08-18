#Requires -Version 5.1

<#
.SYNOPSIS
    Assembles the corresponding source for the exact FFmpeg build Clipped ships.

.DESCRIPTION
    Clipped conveys FFmpeg's libraries as DLLs beside its own binaries, which
    makes FFmpeg's LGPL v3 obligations ours to discharge: a release has to offer
    the source of the exact build it carries, not "FFmpeg 8.1 from somewhere".
    docs/licensing.md lists the obligations; this script produces the artefact
    that discharges the source one, so that publishing it is a step somebody
    runs rather than a promise nobody kept.

    Two source trees are collected, because either alone is incomplete:

    - **FFmpeg itself**, at the exact commit the shipped build was made from.
      The commit is not guessed - the builder puts it in the asset name
      (ffmpeg-n8.1.2-34-g9b6c8969e0-...), and the libraries report the same
      string at run time through av_version_info(), which is what
      crates/muxer/tests/ffmpeg_linkage.rs asserts against the pin.
    - **The build recipe**, at the tag the artefact was published under. FFmpeg
      is configured with a long argument list and links a specific set of
      external libraries, all of which live in BtbN/FFmpeg-Builds rather than in
      FFmpeg. Without the recipe, the source of FFmpeg alone does not let anyone
      rebuild the library that actually shipped.

    Nothing here is pinned twice. The tag, asset and checksum are read out of
    scripts/fetch-ffmpeg.ps1, which is where the pin lives, so moving the pin
    moves this too and the two cannot disagree.

    Provenance comes from git rather than from a checksum. A GitHub source
    archive is generated on demand and its bytes are not promised to be stable,
    so a SHA-256 recorded here would eventually fail for a reason that is not a
    compromised download. A commit id is a hash of the tree it names: fetching
    it and checking that what arrived has that id is the stronger statement, and
    it is the one this script makes.

.PARAMETER Tag
    Release tag of the binary build. Defaults to the pin in
    scripts/fetch-ffmpeg.ps1.

.PARAMETER Asset
    File name of the binary asset, which is what names the FFmpeg commit.
    Defaults to the pin in scripts/fetch-ffmpeg.ps1.

.PARAMETER Sha256
    SHA-256 of the binary asset, recorded in the manifest so that the source and
    the binary it corresponds to are tied together. Defaults to the pin.

.PARAMETER Destination
    Directory the source archives and the manifest are written to. Defaults to
    third-party/ffmpeg/source, which is inside the gitignored directory the
    binary build is fetched into: this is a release input, not something to
    commit.

.PARAMETER FetchScript
    The script the pin is read from. Exposed for the tests, which point it at a
    fixture rather than at the real pin.

.PARAMETER PlanOnly
    Print what would be fetched and exit, touching no network. This is also how
    the tests exercise the pin reading and the commit derivation offline.

.PARAMETER Force
    Rebuild the archives even when they already match the pin.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts/fetch-ffmpeg-source.ps1

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts/fetch-ffmpeg-source.ps1 -PlanOnly

.OUTPUTS
    Exit code 0 when the corresponding source is present and verified, 1
    otherwise.
#>

[CmdletBinding()]
param(
    [string] $Tag = '',
    [string] $Asset = '',
    [string] $Sha256 = '',
    [string] $Destination = '',
    [string] $FetchScript = '',
    [switch] $PlanOnly,
    [switch] $Force
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repositoryRoot = Split-Path -Parent $PSScriptRoot
if (-not $FetchScript) { $FetchScript = Join-Path $PSScriptRoot 'fetch-ffmpeg.ps1' }
if (-not $Destination) { $Destination = Join-Path $repositoryRoot 'third-party\ffmpeg\source' }

# Made absolute before anything uses it. The archives are written with
# `git -C <throwaway clone> archive --output=...`, and git resolves a relative
# output path against the directory it was pointed at rather than against this
# script's: a caller passing `-Destination ffmpeg-source` - as
# .github/workflows/release.yml does, to keep the source out of the directory
# the FFmpeg cache restores - would have git try to write into the clone that
# is deleted at the end of the run. Resolving it here makes a relative path
# mean what the caller meant.
if (-not [System.IO.Path]::IsPathRooted($Destination)) {
    $Destination = Join-Path (Get-Location).ProviderPath $Destination
}
$Destination = [System.IO.Path]::GetFullPath($Destination)

[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$ProgressPreference = 'SilentlyContinue'

# Where each half of the corresponding source comes from. Both are read-only
# clones of public repositories; neither is a mirror we control, which is
# exactly why the manifest records commit ids rather than URLs alone.
$ffmpegRepository = 'https://github.com/FFmpeg/FFmpeg.git'
$buildsRepository = 'https://github.com/BtbN/FFmpeg-Builds.git'

$manifestFile = Join-Path $Destination 'CORRESPONDING-SOURCE.md'

function Write-Step {
    # AllowEmptyString because a blank line between sections is written through
    # the same function as everything else, rather than through a second one.
    param([Parameter(Mandatory)] [AllowEmptyString()] [string] $Message)
    Write-Host $Message
}

function Get-PinnedParameter {
    <#
    .SYNOPSIS
        Reads one parameter default out of the fetch script.
    .DESCRIPTION
        The pin is a set of parameter defaults in scripts/fetch-ffmpeg.ps1, and
        this script must not carry a second copy of it: two copies is how a
        release ends up offering the source of a build it did not ship.

        The defaults are read from the parsed script rather than by matching a
        regular expression over its text, so a reformatted parameter block or a
        changed quoting style cannot silently produce the wrong answer. A
        default that is not a plain literal is refused rather than guessed at.
    #>
    param(
        [Parameter(Mandatory)] [string] $Script,
        [Parameter(Mandatory)] [string] $Name
    )

    if (-not (Test-Path -LiteralPath $Script)) {
        throw "Cannot read the FFmpeg pin: $Script does not exist."
    }

    $errors = $null
    $ast = [System.Management.Automation.Language.Parser]::ParseFile($Script, [ref] $null, [ref] $errors)
    if ($errors -and $errors.Count -gt 0) {
        throw "Cannot read the FFmpeg pin: $Script does not parse ($($errors[0].Message))."
    }

    $parameters = @()
    if ($ast.ParamBlock) { $parameters = @($ast.ParamBlock.Parameters) }

    foreach ($parameter in $parameters) {
        if ($parameter.Name.VariablePath.UserPath -ne $Name) { continue }
        if (-not $parameter.DefaultValue) {
            throw "Cannot read the FFmpeg pin: -$Name in $Script has no default value."
        }
        if ($parameter.DefaultValue -isnot [System.Management.Automation.Language.StringConstantExpressionAst]) {
            throw "Cannot read the FFmpeg pin: the default of -$Name in $Script is not a plain string."
        }
        return $parameter.DefaultValue.Value
    }

    throw "Cannot read the FFmpeg pin: $Script has no -$Name parameter."
}

function Get-FFmpegRevision {
    <#
    .SYNOPSIS
        The FFmpeg revision an asset name identifies.
    .DESCRIPTION
        The builder names its artefacts after `git describe` of the tree it
        built: ffmpeg-n8.1.2-34-g9b6c8969e0-win64-lgpl-shared-8.1.zip is 34
        commits past the n8.1.2 tag, at commit 9b6c8969e0. That commit is the
        precise answer and is preferred.

        An artefact built exactly at a tag has no commit in its name -
        ffmpeg-n8.1.2-win64-lgpl-shared-8.1.zip - and for those the tag is the
        answer and is just as exact, because a tag in FFmpeg's own repository
        resolves to one commit.

        Anything else is refused. Deriving nothing is a script that stops with a
        sentence naming the asset; deriving the wrong thing is a release that
        offers source for a build nobody shipped.
    #>
    param([Parameter(Mandatory)] [string] $AssetName)

    if ($AssetName -match '-g(?<commit>[0-9a-f]{7,40})-win64') {
        return [pscustomobject]@{
            Revision = $Matches['commit']
            Kind     = 'commit'
        }
    }

    if ($AssetName -match '^ffmpeg-(?<tag>n[0-9][^-]*)-win64') {
        return [pscustomobject]@{
            Revision = $Matches['tag']
            Kind     = 'tag'
        }
    }

    throw @"
Cannot tell which FFmpeg source $AssetName was built from.

The builder names an artefact after 'git describe' of the tree it built, so the
name carries either a commit (ffmpeg-n8.1.2-34-g9b6c8969e0-win64-...) or a
release tag (ffmpeg-n8.1.2-win64-...). This name has neither, so the
corresponding source cannot be identified from it - and offering the wrong
source would be worse than stopping here. Pass -Asset explicitly if the pin is
being read from somewhere unexpected.
"@
}

function Assert-GitPresent {
    <#
    .SYNOPSIS
        Fails with a named prerequisite rather than a command-not-found error.
    #>
    if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
        throw @"
git is not on PATH, and the corresponding source is fetched with it.

git is what makes this verifiable: a commit id is a hash of the tree it names,
so fetching a commit and checking the id of what arrived proves the source
matches the build. Install git and run this again.
"@
    }
}

function Invoke-Git {
    <#
    .SYNOPSIS
        Runs git, failing loudly with everything it printed.
    .DESCRIPTION
        A failed fetch is the interesting case and git says why on stderr, so
        the output is captured and included rather than left to scroll past.

        $ErrorActionPreference is relaxed for the duration of the call. Windows
        PowerShell wraps each stderr line of a native command in an ErrorRecord,
        and under 'Stop' that turns git's ordinary progress chatter into a
        terminating error before its exit code has been looked at. The exit code
        is what decides success here.
    #>
    param([Parameter(Mandatory)] [string[]] $Arguments)

    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $output = & git @Arguments 2>&1 | Out-String
        $code = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previous
    }

    if ($code -ne 0) {
        throw "git $($Arguments -join ' ') failed:`n$output"
    }
    return $output
}

function Resolve-AbbreviatedCommit {
    <#
    .SYNOPSIS
        Expands the short commit in an asset name to a full object id.
    .DESCRIPTION
        The builder's asset names carry `git describe` output, which abbreviates
        the commit to ten characters. Git's fetch protocol will not accept that:
        a client may ask for an object by its full id or for a named ref, and
        nothing else, so `git fetch origin 9b6c8969e0` fails with "couldn't find
        remote ref" no matter how the server is configured.

        Expanding it needs something that can see the repository. GitHub's API
        resolves an abbreviated commit to the full one, and that is all it is
        used for: the answer is checked against the abbreviation it came from,
        and the fetch that follows still proves the id, so a wrong answer here
        cannot become a source archive for the wrong tree.
    #>
    param(
        [Parameter(Mandatory)] [string] $ApiRepository,
        [Parameter(Mandatory)] [string] $Commit
    )

    if ($Commit.Length -eq 40) { return $Commit }

    $uri = "https://api.github.com/repos/$ApiRepository/commits/$Commit"
    Write-Step "  expanding $Commit through $uri"

    $headers = @{
        'User-Agent' = 'clipped-fetch-ffmpeg-source'
        'Accept'     = 'application/vnd.github+json'
    }

    # Unauthenticated, this API allows sixty calls an hour per address, and a
    # GitHub-hosted runner shares its address with every other runner on it. One
    # call per release is not the problem; being told to come back in an hour on
    # the day of a release is. $GITHUB_TOKEN is present in Actions and absent on
    # a developer's machine, where sixty an hour is nobody's constraint, so it is
    # used when it is there and not required when it is not. The answer is
    # checked against the abbreviation either way, and the fetch that follows
    # still proves the id.
    if ($env:GITHUB_TOKEN) {
        $headers['Authorization'] = "Bearer $($env:GITHUB_TOKEN)"
        Write-Step '  (authenticated with $GITHUB_TOKEN)'
    }

    try {
        $response = Invoke-RestMethod -Uri $uri -Headers $headers -UseBasicParsing
    } catch {
        throw @"
Could not expand the abbreviated commit $Commit through $uri : $($_.Exception.Message)

The asset name abbreviates the commit and git will only fetch a full object id,
so this lookup is how the two are joined. If the API is unreachable or rate
limited, pass the full 40-character commit as -Asset's commit by hand:

  git ls-remote $ffmpegRepository | Select-String $Commit
"@
    }

    $resolved = "$($response.sha)".ToLowerInvariant()
    if (-not $resolved.StartsWith($Commit.ToLowerInvariant())) {
        throw "Asked $uri for $Commit and it answered $resolved, which does not begin with it."
    }

    return $resolved
}

function Get-SourceArchive {
    <#
    .SYNOPSIS
        Produces a zip of one repository at one revision, and returns its id.
    .DESCRIPTION
        A shallow fetch of the single revision, rather than a clone: FFmpeg's
        history is hundreds of megabytes and none of it is the corresponding
        source of anything.

        The resolved commit id is returned and recorded in the manifest. For a
        revision given as a commit it is also checked against what was asked
        for, which is the verification this script offers in place of a checksum.
    #>
    param(
        [Parameter(Mandatory)] [string] $Repository,
        [Parameter(Mandatory)] [string] $Revision,
        [Parameter(Mandatory)] [string] $WorkingDirectory,
        [Parameter(Mandatory)] [string] $ArchivePath
    )

    New-Item -ItemType Directory -Path $WorkingDirectory -Force | Out-Null

    Invoke-Git -Arguments @('-C', $WorkingDirectory, 'init', '--quiet') | Out-Null
    Invoke-Git -Arguments @('-C', $WorkingDirectory, 'remote', 'add', 'origin', $Repository) | Out-Null

    Write-Step "  fetching $Revision from $Repository"
    Invoke-Git -Arguments @('-C', $WorkingDirectory, 'fetch', '--quiet', '--depth', '1', 'origin', $Revision) | Out-Null

    $resolved = (Invoke-Git -Arguments @('-C', $WorkingDirectory, 'rev-parse', 'FETCH_HEAD')).Trim()

    if ($Revision -match '^[0-9a-f]{7,40}$' -and -not $resolved.StartsWith($Revision)) {
        throw @"
Fetched $Revision from $Repository but got commit $resolved.

A commit id is a hash of the tree it names, so this cannot happen to an honest
server. Nothing was written.
"@
    }

    Write-Step "  resolved to $resolved"
    Invoke-Git -Arguments @('-C', $WorkingDirectory, 'archive', '--format=zip', "--output=$ArchivePath", 'FETCH_HEAD') | Out-Null

    return $resolved
}

function Write-Manifest {
    <#
    .SYNOPSIS
        Writes the file that ties the source archives to the binary they are for.
    .DESCRIPTION
        The archives are two zips with commit ids in their names, and a person
        who downloads them a year from now has no way to tell which binary they
        correspond to. The manifest is that statement, and it is also what the
        release notes point at.
    #>
    param(
        [Parameter(Mandatory)] [string] $FFmpegCommit,
        [Parameter(Mandatory)] [string] $BuildsCommit,
        [Parameter(Mandatory)] [string] $FFmpegArchive,
        [Parameter(Mandatory)] [string] $BuildsArchive
    )

    $generated = (Get-Date).ToUniversalTime().ToString('yyyy-MM-dd')

    $lines = @(
        '# Corresponding source for the FFmpeg build Clipped ships',
        '',
        "Generated by ``scripts/fetch-ffmpeg-source.ps1`` on $generated.",
        '',
        'Clipped links dynamically against a prebuilt, LGPL v3 FFmpeg and ships its',
        'DLLs unmodified beside its own binaries. This directory is the source of that',
        'exact build, which the LGPL requires a release to offer. See',
        '`docs/licensing.md` for the whole set of obligations and',
        '`docs/adr/0004-ffmpeg-dependency-strategy.md` for why FFmpeg is linked this',
        'way at all.',
        '',
        '## The binary this is the source for',
        '',
        '| | |',
        '| --- | --- |',
        "| Release tag | ``$Tag`` |",
        "| Asset | ``$Asset`` |",
        "| SHA-256 | ``$Sha256`` |",
        "| Published by | https://github.com/BtbN/FFmpeg-Builds |",
        '',
        'The libraries report the same build for themselves at run time, through',
        '`av_version_info()`. `crates/muxer/tests/ffmpeg_linkage.rs` asserts it.',
        '',
        '## What is here',
        '',
        '| File | Repository | Commit |',
        '| --- | --- | --- |',
        "| ``$FFmpegArchive`` | $ffmpegRepository | ``$FFmpegCommit`` |",
        "| ``$BuildsArchive`` | $buildsRepository | ``$BuildsCommit`` |",
        '',
        'FFmpeg is the Library the LGPL is about. The build recipe is here because',
        'FFmpeg alone does not describe the artefact: the `configure` arguments and',
        'the versions of every external library compiled into it live in',
        'BtbN/FFmpeg-Builds, and the binary reports those arguments through',
        '`avutil_configuration()`.',
        '',
        '## Publishing this',
        '',
        'Both archives and this file are attached to the Clipped release that carries',
        'the DLLs, or mirrored somewhere the release notes link to. The obligation is',
        'to the recipient of the binary, so a pointer to a third party that may remove',
        'the artefact is not enough on its own.'
    )

    Set-Content -LiteralPath $manifestFile -Value $lines -Encoding UTF8
}

function Test-AlreadyAssembled {
    <#
    .SYNOPSIS
        Whether this directory already holds the source for this pin.
    .DESCRIPTION
        The manifest names the asset it was written for, so a second run over an
        intact directory can tell "already done" from "the pin moved" without
        going near the network - the same property that makes the binary fetch
        script safe to run unconditionally.
    #>
    if (-not (Test-Path -LiteralPath $manifestFile)) { return $false }

    $recorded = Get-Content -LiteralPath $manifestFile -Raw
    return $recorded.Contains($Asset) -and $recorded.Contains($Sha256)
}

try {
    if (-not $Tag) { $Tag = Get-PinnedParameter -Script $FetchScript -Name 'Tag' }
    if (-not $Asset) { $Asset = Get-PinnedParameter -Script $FetchScript -Name 'Asset' }
    if (-not $Sha256) { $Sha256 = Get-PinnedParameter -Script $FetchScript -Name 'Sha256' }
    $Sha256 = $Sha256.ToLowerInvariant()

    $ffmpeg = Get-FFmpegRevision -AssetName $Asset
    $ffmpegArchiveName = "ffmpeg-$($ffmpeg.Revision)-source.zip"
    $buildsArchiveName = "ffmpeg-builds-$Tag-source.zip"

    Write-Step 'Corresponding source for the pinned FFmpeg build'
    Write-Step "  binary asset   $Asset"
    Write-Step "  release tag    $Tag"
    Write-Step "  SHA-256        $Sha256"
    Write-Step "  FFmpeg         $($ffmpeg.Revision) (from the asset name, as a $($ffmpeg.Kind))"
    Write-Step "  build recipe   $Tag in BtbN/FFmpeg-Builds"
    Write-Step "  destination    $Destination"

    if ($PlanOnly) {
        Write-Step ''
        Write-Step 'Nothing was fetched: -PlanOnly.'
        exit 0
    }

    if ((Test-AlreadyAssembled) -and -not $Force) {
        Write-Step ''
        Write-Step "The corresponding source for this pin is already in $Destination; nothing to fetch."
        exit 0
    }

    Assert-GitPresent

    New-Item -ItemType Directory -Path $Destination -Force | Out-Null
    $staging = Join-Path $Destination ".staging-$([System.Guid]::NewGuid().ToString('n'))"

    try {
        Write-Step ''
        Write-Step 'FFmpeg'
        $ffmpegRevision = $ffmpeg.Revision
        if ($ffmpeg.Kind -eq 'commit') {
            $ffmpegRevision = Resolve-AbbreviatedCommit -ApiRepository 'FFmpeg/FFmpeg' -Commit $ffmpeg.Revision
        }
        $ffmpegCommit = Get-SourceArchive `
            -Repository $ffmpegRepository `
            -Revision $ffmpegRevision `
            -WorkingDirectory (Join-Path $staging 'ffmpeg') `
            -ArchivePath (Join-Path $Destination $ffmpegArchiveName)

        Write-Step 'Build recipe'
        $buildsCommit = Get-SourceArchive `
            -Repository $buildsRepository `
            -Revision $Tag `
            -WorkingDirectory (Join-Path $staging 'builds') `
            -ArchivePath (Join-Path $Destination $buildsArchiveName)
    } finally {
        # A shallow FFmpeg checkout is over a hundred megabytes and is of no use
        # once it has been archived, so it does not survive the run either way.
        Remove-Item -LiteralPath $staging -Recurse -Force -ErrorAction SilentlyContinue
    }

    Write-Manifest `
        -FFmpegCommit $ffmpegCommit `
        -BuildsCommit $buildsCommit `
        -FFmpegArchive $ffmpegArchiveName `
        -BuildsArchive $buildsArchiveName

    Write-Step ''
    Write-Step "Corresponding source assembled in $Destination"
    Write-Step '  Attach both archives and CORRESPONDING-SOURCE.md to the release that ships these DLLs (docs/licensing.md).'
    exit 0
} catch {
    Write-Host ''
    Write-Host $_.Exception.Message -ForegroundColor Red
    exit 1
}
