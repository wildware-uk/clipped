#Requires -Version 5.1

<#
.SYNOPSIS
    Assembles the licence texts and third-party notices an installed Clipped
    has to carry.

.DESCRIPTION
    Clipped ships FFmpeg's LGPL v3 libraries beside its own binaries and links
    several hundred permissively licensed Rust crates into them. Both carry
    conditions that are discharged by what is *installed*, not by what is in
    this repository: the LGPL wants its notice, both licence texts and an offer
    of source with each copy, and MIT, BSD and ISC each want their copyright
    line and permission notice carried with the binary. docs/licensing.md is the
    whole list; this script produces the directory that satisfies the parts of
    it that are files.

    It is deliberately not a document somebody keeps up to date. Every fact in
    the output is read from something authoritative at the moment it runs:

    - the FFmpeg version, licence and configuration come from the installed
      build itself, by running its own ffprobe, so they describe the DLLs that
      will actually be copied rather than what the pin said months ago;
    - the pin - tag, asset and checksum - comes from the record
      scripts/fetch-ffmpeg.ps1 wrote beside those DLLs;
    - the Rust dependencies come from `cargo metadata`, and each crate's notice
      is the licence file published inside that crate, read out of the source
      Cargo unpacked.

    Which Rust crates are listed is a question with a wrong answer that is easy
    to reach. The list here is the normal-dependency closure of both workspaces:
    starting at Clipped's own crates and following only ordinary dependency
    edges, never `dev-dependencies` and never `build-dependencies`. Test-only
    and build-time crates are reached over no edge this walk follows, so they
    are absent; `deny.toml` and CI still hold them to the licence allow-list,
    which is a different question.

    That closure is a *superset* of what is linked into the binaries, and it is
    named as one rather than as the linked set, because the two are not the
    same. A procedural macro is an ordinary dependency of the crate that uses
    it - `serde_derive` of `serde`, `thiserror-impl` of `thiserror` - and so are
    `syn`, `quote` and `proc-macro2` underneath it, but all of them run in the
    compiler and none of them is in a shipped binary, exactly as `bindgen` is
    not. Separating them out would mean deciding, per crate, which edges lead
    only into the compiler, and being wrong in the direction that drops a crate
    that *is* linked. Listing more than is linked reproduces a permission notice
    for something a user did not receive, which costs a reader a paragraph;
    listing less omits one they did, which is the failure that matters. So the
    rule is the one that cannot under-report, and the payload says which rule it
    is rather than claiming the narrower one.

    Clipped's own crates are not listed either: they are covered by LICENSE at
    the root of the payload, which is the same file. "Clipped's own" means a
    member of *either* workspace, which is not the same as a member of the
    workspace currently being walked - `clipped-ipc` is a member of the root one
    and a path dependency of the desktop one.

.PARAMETER Destination
    Directory the payload is written to. Defaults to target/licences, which is
    gitignored: this is a build output, and committing it would be a snapshot
    that starts rotting the next time a dependency moves.

    A directory that already holds a payload this script wrote is replaced. One
    that holds anything else is refused, because the release checklist's next
    step is "include the payload in the installer" and the obvious mistake is to
    point this at a staging or install directory that already has something in
    it. The payload is also assembled beside the destination and moved into
    place only once it is complete, so a run that fails part-way through - cargo
    metadata failing, a crate that cannot be read - leaves what was there
    before intact rather than deleted and half-replaced.

.PARAMETER Force
    Write into a -Destination that holds files this script did not write,
    deleting them. For a caller that has decided the directory is theirs to
    empty; nothing in the release checklist needs it.

.PARAMETER FFmpegDir
    The installed FFmpeg prefix to describe. Defaults to FFMPEG_DIR from the
    environment, then to third-party/ffmpeg/current, which is where
    scripts/fetch-ffmpeg.ps1 installs the pinned build.

.PARAMETER RepositoryRoot
    The checkout to read LICENSE, THIRD-PARTY-NOTICES.md, licences/GPL-3.0.txt
    and the two Cargo manifests from. Exposed for the tests.

.PARAMETER SkipRustDependencies
    Leave out the generated Rust dependency notices. `cargo metadata` resolves
    two large graphs, which takes a few seconds and wants a network on a cold
    cache; the switch exists so that the FFmpeg half can be checked without
    them. A payload produced this way is incomplete and says so in its own
    README, and the script says so on the way out.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts/collect-notices.ps1

.OUTPUTS
    Exit code 0 when the payload was written, 1 otherwise.
#>

[CmdletBinding()]
param(
    [string] $Destination = '',
    [string] $FFmpegDir = '',
    [string] $RepositoryRoot = '',
    [switch] $SkipRustDependencies,
    [switch] $Force
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if (-not $RepositoryRoot) { $RepositoryRoot = Split-Path -Parent $PSScriptRoot }
if (-not $Destination) { $Destination = Join-Path $RepositoryRoot 'target\licences' }
if (-not $FFmpegDir) {
    $FFmpegDir = if ($env:FFMPEG_DIR) { $env:FFMPEG_DIR } else { Join-Path $RepositoryRoot 'third-party\ffmpeg\current' }
}

# The graph is evaluated for the one target Clipped is built for, matching
# deny.toml's [graph] section: a cfg-gated dependency that is never compiled
# into anything we ship is not something a user has been given.
$target = 'x86_64-pc-windows-msvc'

# Names a published crate uses for the file carrying its licence. Matched
# case-insensitively against the root of the unpacked crate; anything deeper is
# a licence for something the crate itself vendored, which is its own notice
# file's business rather than ours.
$licenceFilePatterns = @('LICENSE*', 'LICENCE*', 'COPYING*', 'NOTICE*', 'UNLICENSE*')

# A guard rather than a policy: a crate's licence file is a few kilobytes, and
# anything enormous matched by the patterns above is not one.
$maximumLicenceFileBytes = 128KB

# The sentence every payload's README carries, and the only thing that tells a
# directory this script wrote from a directory somebody else's build filled.
$payloadMarker = 'Generated by `scripts/collect-notices.ps1`'

function Write-Step {
    param([Parameter(Mandatory)] [AllowEmptyString()] [string] $Message)
    Write-Host $Message
}

function Assert-Present {
    param(
        [Parameter(Mandatory)] [string] $Path,
        [Parameter(Mandatory)] [string] $What
    )
    if (-not (Test-Path -LiteralPath $Path)) {
        throw "$What is not at $Path, so the notices would be incomplete. Nothing was written."
    }
}

function Get-InstalledBuild {
    <#
    .SYNOPSIS
        Describes the FFmpeg build that will be shipped, from the build itself.
    .DESCRIPTION
        Read from the artefact rather than from the pin, because the pin is what
        was asked for and this has to describe what is there. The two agree on
        a machine where the fetch script ran and disagree on one where somebody
        pointed FFMPEG_DIR at an FFmpeg of their own - and in that case the
        notices must describe theirs.

        `ffprobe -version` is the source: it prints the same strings the
        libraries report through av_version_info(), avutil_license() and
        avutil_configuration(), which is what crates/muxer's linkage module
        reads at run time.
    #>
    param([Parameter(Mandatory)] [string] $Prefix)

    $ffprobe = Join-Path $Prefix 'bin\ffprobe.exe'
    Assert-Present -Path $ffprobe -What 'The FFmpeg build to describe'

    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $output = & $ffprobe -hide_banner -version 2>&1 | Out-String
        $code = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previous
    }
    if ($code -ne 0) {
        throw "$ffprobe -version failed with exit code $code, so the FFmpeg build cannot be described."
    }

    $lines = $output -split "`r?`n"

    $version = ''
    if ($lines[0] -match '^ffprobe version (?<version>\S+)') { $version = $Matches['version'] }
    if (-not $version) {
        throw "Could not read a version out of $ffprobe -version. It printed:`n$output"
    }

    $configuration = ''
    foreach ($line in $lines) {
        if ($line -match '^configuration:\s*(?<configuration>.*)$') {
            $configuration = $Matches['configuration']
            break
        }
    }

    $libraries = @($lines | Where-Object { $_ -match '^lib(av|sw)\w+\s' } | ForEach-Object { $_.Trim() })

    # The libraries are asked what licence they are under rather than told. A
    # build carrying GPL components answers differently, and a notice claiming
    # LGPL over one of those would be wrong in the direction that matters.
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $licenceOutput = & $ffprobe -hide_banner -L 2>&1 | Out-String
    } finally {
        $ErrorActionPreference = $previous
    }
    $licence = if ($licenceOutput -match 'GNU Lesser General Public License') {
        'LGPL'
    } elseif ($licenceOutput -match 'GNU General Public License') {
        'GPL'
    } else {
        'unrecognised'
    }

    return [pscustomobject]@{
        Version       = $version
        Configuration = $configuration
        Libraries     = $libraries
        Licence       = $licence
        LicenceText   = $licenceOutput.Trim()
    }
}

function Get-PinRecord {
    <#
    .SYNOPSIS
        The pin scripts/fetch-ffmpeg.ps1 recorded beside the installed build.
    .DESCRIPTION
        Absent when somebody is building against an FFmpeg of their own, which
        is allowed. The payload then says the build was not the pinned one
        rather than inventing an asset name for it.
    #>
    param([Parameter(Mandatory)] [string] $Prefix)

    $pinFile = Join-Path $Prefix '.clipped-ffmpeg-pin.json'
    if (-not (Test-Path -LiteralPath $pinFile)) { return $null }

    try {
        return Get-Content -LiteralPath $pinFile -Raw | ConvertFrom-Json
    } catch {
        return $null
    }
}

function Get-ShippedDlls {
    param([Parameter(Mandatory)] [string] $Prefix)

    $binary = Join-Path $Prefix 'bin'
    Assert-Present -Path $binary -What "The FFmpeg build's bin directory"
    return @(Get-ChildItem -LiteralPath $binary -Filter '*.dll' | Sort-Object Name)
}

function Get-ShippedCrates {
    <#
    .SYNOPSIS
        Every third-party crate compiled into what Clipped ships.
    .DESCRIPTION
        `cargo metadata` reports the resolved graph and, for each edge, whether
        it is a normal, dev or build dependency. Walking only normal edges from
        the workspace's own packages is what separates "in the binary" from
        "used to test or build it".

        Two workspaces are walked, because Clipped has two: the root one and
        apps/desktop/src-tauri, which is detached for the reasons its own
        manifest gives. The union is what an installation contains.

        Clipped's own crates are removed from that union at the end rather than
        as each workspace is walked, because a workspace's member list only
        describes that workspace. `clipped-ipc` is a member of the root one and
        an ordinary path dependency of the desktop one, so the desktop walk
        reaches it as it reaches any other dependency and has nothing local to
        recognise it by. Filtering afterwards, against the members of every
        workspace walked, is what keeps Clipped's own code out of a file whose
        first line calls everything in it third-party.
    #>
    param([Parameter(Mandatory)] [string[]] $Manifests)

    $crates = @{}
    $own = [System.Collections.Generic.HashSet[string]]::new()

    foreach ($manifest in $Manifests) {
        Assert-Present -Path $manifest -What 'A Cargo manifest to read dependencies from'
        Write-Step "  resolving $manifest"

        $previous = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        try {
            $json = & cargo metadata --format-version 1 --all-features --filter-platform $target --manifest-path $manifest 2>$null | Out-String
            $code = $LASTEXITCODE
        } finally {
            $ErrorActionPreference = $previous
        }
        if ($code -ne 0) {
            throw "cargo metadata failed for $manifest with exit code $code."
        }

        $metadata = $json | ConvertFrom-Json

        $packages = @{}
        foreach ($package in $metadata.packages) { $packages[$package.id] = $package }

        $nodes = @{}
        foreach ($node in $metadata.resolve.nodes) { $nodes[$node.id] = $node }

        foreach ($member in $metadata.workspace_members) {
            if (-not $packages.ContainsKey($member)) { continue }
            $package = $packages[$member]
            [void] $own.Add("$($package.name) $($package.version)")
        }

        $seen = [System.Collections.Generic.HashSet[string]]::new()
        $queue = [System.Collections.Generic.Queue[string]]::new()
        foreach ($member in $metadata.workspace_members) { $queue.Enqueue($member) }

        while ($queue.Count -gt 0) {
            $id = $queue.Dequeue()
            if (-not $seen.Add($id)) { continue }
            if (-not $nodes.ContainsKey($id)) { continue }

            foreach ($dependency in $nodes[$id].deps) {
                # kind is null for a normal dependency and the string "dev" or
                # "build" otherwise. One dependency can be more than one kind at
                # once - a crate used both in the library and in its tests - and
                # a normal edge anywhere in that list is enough to ship it.
                $normal = @($dependency.dep_kinds | Where-Object { $null -eq $_.kind })
                if ($normal.Count -eq 0) { continue }
                $queue.Enqueue($dependency.pkg)
            }
        }

        foreach ($id in $seen) {
            if (-not $packages.ContainsKey($id)) { continue }
            $package = $packages[$id]
            $crates["$($package.name) $($package.version)"] = $package
        }
    }

    foreach ($key in $own) { $crates.Remove($key) }

    return $crates
}

function Get-LicenceFiles {
    <#
    .SYNOPSIS
        The licence files a crate publishes inside itself.
    .DESCRIPTION
        This is the part that makes the output a notices file rather than a
        list: MIT, BSD and ISC all require the copyright line and the permission
        notice to travel with the binary, and the copyright line is in the
        crate's own licence file, not in its metadata.
    #>
    param([Parameter(Mandatory)] $Package)

    $root = Split-Path -Parent $Package.manifest_path
    if (-not (Test-Path -LiteralPath $root)) { return @() }

    $files = @()
    foreach ($pattern in $licenceFilePatterns) {
        $files += @(Get-ChildItem -LiteralPath $root -Filter $pattern -File -ErrorAction SilentlyContinue)
    }

    return @($files |
        Sort-Object -Property Name -Unique |
        Where-Object { $_.Length -le $maximumLicenceFileBytes })
}

function Write-Payload {
    # AllowEmptyString as well as AllowEmptyCollection: a blank line between
    # paragraphs is an empty element, and a mandatory [string[]] rejects one.
    param(
        [Parameter(Mandatory)] [string] $Path,
        [Parameter(Mandatory)] [AllowEmptyCollection()] [AllowEmptyString()] [string[]] $Lines
    )
    $directory = Split-Path -Parent $Path
    New-Item -ItemType Directory -Path $directory -Force | Out-Null
    Set-Content -LiteralPath $Path -Value $Lines -Encoding UTF8
}

function Assert-DestinationIsReplaceable {
    <#
    .SYNOPSIS
        Stops before deleting a directory this script did not fill.
    .DESCRIPTION
        The last thing this script does is replace -Destination wholesale, and
        -Destination is a documented parameter whose most likely value after the
        default is wherever an installer stages its files. An empty directory,
        a missing one, and one holding a payload from a previous run are all
        this script's to replace. Anything else is somebody's data, and a
        licence tool is not a thing that should be able to delete it.
    #>
    param([Parameter(Mandatory)] [string] $Path)

    if (-not (Test-Path -LiteralPath $Path)) { return }

    if (-not (Test-Path -LiteralPath $Path -PathType Container)) {
        throw "$Path is a file, not a directory, so the notices payload cannot be written there. Nothing was written."
    }

    $existing = @(Get-ChildItem -LiteralPath $Path -Force)
    if ($existing.Count -eq 0) { return }

    # String.Contains rather than -like: the marker is full of backticks, and
    # backtick is the escape character of PowerShell's wildcard patterns, so
    # -like would quietly be matching a different string to the one written.
    $readme = Join-Path $Path 'README.md'
    if (Test-Path -LiteralPath $readme -PathType Leaf) {
        $recorded = Get-Content -LiteralPath $readme -Raw
        if ($recorded -and $recorded.Contains($payloadMarker)) { return }
    }

    throw @"
$Path already holds $($existing.Count) item(s) that this script did not write.

Writing the payload replaces the destination wholesale, and a destination this
script did not fill is somebody else's - a staging directory or an installed
application, which is where a release checklist points things. Nothing was
written. Give it an empty directory, or pass -Force if deleting what is there
is what you meant.
"@
}

# Declared out here so that the failure path can remove a half-built payload it
# may never have got as far as naming.
$assembly = ''

try {
    $licence = Join-Path $RepositoryRoot 'LICENSE'
    $notices = Join-Path $RepositoryRoot 'THIRD-PARTY-NOTICES.md'
    $gpl = Join-Path $RepositoryRoot 'licences\GPL-3.0.txt'
    $ffmpegLicence = Join-Path $FFmpegDir 'LICENSE.txt'

    Assert-Present -Path $licence -What "Clipped's own licence"
    Assert-Present -Path $notices -What 'The repository third-party notices'
    Assert-Present -Path $gpl -What 'The GNU GPL v3 text the LGPL requires alongside its own'
    Assert-Present -Path $ffmpegLicence -What "The FFmpeg build's licence text"
    if (-not $Force) { Assert-DestinationIsReplaceable -Path $Destination }

    Write-Step "Describing the FFmpeg build in $FFmpegDir"
    $build = Get-InstalledBuild -Prefix $FFmpegDir
    $pin = Get-PinRecord -Prefix $FFmpegDir
    # @() because a prefix with no DLLs in it returns nothing rather than an
    # empty array, and asking $null for a Count is a crash instead of the
    # sentence about the licence that this build was about to be refused with.
    $dlls = @(Get-ShippedDlls -Prefix $FFmpegDir)
    Write-Step "  FFmpeg $($build.Version), reporting $($build.Licence), $($dlls.Count) libraries"

    if ($build.Licence -ne 'LGPL') {
        throw @"
The FFmpeg build in $FFmpegDir reports its licence as $($build.Licence), not LGPL.

Clipped is MPL-2.0 and may not ship a GPL FFmpeg
(docs/adr/0004-ffmpeg-dependency-strategy.md). Nothing was written: a notices
payload describing this build would be a licence position nobody chose.
"@
    }

    # Everything is assembled beside the destination and moved onto it at the
    # end. The alternative - empty the destination first and write into it -
    # means that `cargo metadata` failing, or a crate whose licence file cannot
    # be read, leaves the previous payload deleted and a partial one in its
    # place, which is the state hardest to notice: the directory exists and has
    # files in it.
    $assembly = "$Destination.assembling-$([System.Guid]::NewGuid().ToString('n'))"
    New-Item -ItemType Directory -Path $assembly -Force | Out-Null

    Copy-Item -LiteralPath $licence -Destination (Join-Path $assembly 'LICENSE.txt')
    Copy-Item -LiteralPath $notices -Destination (Join-Path $assembly 'THIRD-PARTY-NOTICES.md')

    $ffmpegDirectory = Join-Path $assembly 'ffmpeg'
    New-Item -ItemType Directory -Path $ffmpegDirectory -Force | Out-Null
    Copy-Item -LiteralPath $ffmpegLicence -Destination (Join-Path $ffmpegDirectory 'LGPL-3.0.txt')
    Copy-Item -LiteralPath $gpl -Destination (Join-Path $ffmpegDirectory 'GPL-3.0.txt')

    $pinLines = if ($pin) {
        @(
            "| Release tag | ``$($pin.tag)`` |",
            "| Asset | ``$($pin.asset)`` |",
            "| SHA-256 | ``$($pin.sha256)`` |"
        )
    } else {
        @('| Pin | This build was not installed by `scripts/fetch-ffmpeg.ps1`, so no pin record describes it. |')
    }

    Write-Payload -Path (Join-Path $ffmpegDirectory 'NOTICE.md') -Lines (@(
            '# FFmpeg',
            '',
            'This application uses libraries from the FFmpeg project under the GNU Lesser',
            'General Public License version 3 or later (LGPL v3+). The libraries are',
            'unmodified, they are shipped as separate files alongside this application, and',
            'the application links against them dynamically.',
            '',
            'The full text of the LGPL v3 is in `LGPL-3.0.txt` beside this file. The LGPL is',
            'written as a set of additional permissions on top of the GNU General Public',
            'License version 3, so that text is required too and is in `GPL-3.0.txt`.',
            '',
            'FFmpeg is https://ffmpeg.org. Clipped is not endorsed by or affiliated with the',
            'FFmpeg project.',
            '',
            '## The build shipped here',
            '',
            '| | |',
            '| --- | --- |',
            "| Version | ``$($build.Version)`` |",
            "| Licence reported by the build | $($build.Licence) |"
        ) + $pinLines + @(
            '',
            'Library versions:',
            '',
            '```text'
        ) + $build.Libraries + @(
            '```',
            '',
            'Configured with:',
            '',
            '```text',
            $build.Configuration,
            '```',
            '',
            'Files:',
            '',
            '```text'
        ) + @($dlls | ForEach-Object { '{0}  {1:N0} bytes' -f $_.Name, $_.Length }) + @(
            '```',
            '',
            '## Source',
            '',
            'The LGPL requires the source of the exact libraries shipped here to be',
            'available to whoever received them. It is published as assets on the Clipped',
            'release that carries these files, on the same page as the installer and',
            'downloadable by anyone who can download that: an archive of FFmpeg at the commit',
            'this build was made from, an archive of the recipe it was configured and',
            'compiled by, and a `CORRESPONDING-SOURCE.md` naming both commits and the binary',
            'they correspond to. Nothing has to be requested and nobody has to be asked.',
            '',
            '`scripts/fetch-ffmpeg-source.ps1` in the Clipped repository assembles those',
            'files and `.github/workflows/release.yml` publishes them. It is not left to',
            'whoever drafts the release: the release gates refuse to build one at all unless',
            'that source is assembled and is the source of the build named above',
            '(`scripts/check-release-gates.ps1`).',
            '',
            'The Clipped repository is https://github.com/wildware-uk/clipped, and',
            '`docs/licensing.md` there sets out the whole set of obligations these files',
            'discharge.',
            '',
            '## Replacing these libraries',
            '',
            'The LGPL reserves the right to run this application against a modified version',
            'of the library. Nothing here prevents that: replace the DLLs above with an',
            'interface-compatible FFmpeg build - the same major version of each library, as',
            'the file names carry - and the application will use them. See',
            '`docs/licensing.md` in the Clipped repository for how this was verified.'
        ))

    $rustNoticeFile = Join-Path $assembly 'THIRD-PARTY-NOTICES-RUST.md'
    $crateCount = 0

    if ($SkipRustDependencies) {
        Write-Step 'Skipping the Rust dependency notices (-SkipRustDependencies).'
    } else {
        Write-Step 'Collecting the Rust dependencies compiled into the binaries'
        $crates = Get-ShippedCrates -Manifests @(
            (Join-Path $RepositoryRoot 'Cargo.toml'),
            (Join-Path $RepositoryRoot 'apps\desktop\src-tauri\Cargo.toml')
        )
        $crateCount = $crates.Count
        Write-Step "  $crateCount third-party crates"

        $lines = @(
            '# Rust dependency notices',
            '',
            "Generated by ``scripts/collect-notices.ps1`` on $((Get-Date).ToUniversalTime().ToString('yyyy-MM-dd')).",
            '',
            'These are the third-party Rust crates this application is built from, with the',
            'licence notice each one publishes.',
            '',
            'The list is the normal-dependency closure of both of Clipped''s Cargo',
            "workspaces, resolved for ``$target`` with all features enabled.",
            '`dev-dependencies` and `build-dependencies` are excluded: nothing reached only',
            'over those edges - test harnesses, `bindgen`, `cc`, `tauri-build` - is in a',
            'binary at all. Clipped''s own crates are excluded too; they are covered by',
            '`LICENSE.txt` beside this file.',
            '',
            'That closure is a superset of what is linked, and is named as one rather than',
            'as the linked set. A procedural macro is an ordinary dependency of the crate',
            'that uses it, and so are the crates underneath it - `serde_derive`,',
            '`thiserror-impl`, `syn`, `quote`, `proc-macro2` - yet all of them run in the',
            'compiler and none is in a shipped binary. Telling those apart per crate risks',
            'dropping one that is linked, so this list keeps them: a notice for something',
            'you did not receive costs you a paragraph, a missing notice costs you a right.',
            '',
            'Where a crate publishes no licence file of its own, the licence it declares in',
            'its manifest is named and there is nothing further to reproduce.',
            '',
            "$crateCount crates.",
            ''
        )

        foreach ($key in ($crates.Keys | Sort-Object)) {
            $package = $crates[$key]
            $declared = if ($package.license) { $package.license } elseif ($package.license_file) { "see $($package.license_file)" } else { 'not declared' }

            $lines += "## $($package.name) $($package.version)"
            $lines += ''
            $lines += "- Licence: $declared"
            if ($package.repository) { $lines += "- Repository: $($package.repository)" }
            $lines += ''

            # @() because PowerShell unrolls a one-element array on return, and
            # a lone FileInfo has no Count to ask about.
            $licenceFiles = @(Get-LicenceFiles -Package $package)
            if ($licenceFiles.Count -eq 0) {
                $lines += 'This crate publishes no licence file; the licence above is the one it'
                $lines += 'declares in its manifest.'
                $lines += ''
                continue
            }

            foreach ($file in $licenceFiles) {
                $lines += "### $($file.Name)"
                $lines += ''
                $lines += '```text'
                $lines += @(Get-Content -LiteralPath $file.FullName)
                $lines += '```'
                $lines += ''
            }
        }

        Write-Payload -Path $rustNoticeFile -Lines $lines
    }

    Write-Payload -Path (Join-Path $assembly 'README.md') -Lines @(
        '# Licences and notices',
        '',
        'Everything in this directory is installed with Clipped, because the licences of',
        'what Clipped ships require it. `docs/licensing.md` in the Clipped repository',
        'explains which obligation each file discharges.',
        '',
        '| File | What it is |',
        '| --- | --- |',
        '| `LICENSE.txt` | Clipped itself, under the Mozilla Public License 2.0. |',
        '| `THIRD-PARTY-NOTICES.md` | Third-party material inside Clipped''s own source. |',
        $(if ($SkipRustDependencies) { '| `THIRD-PARTY-NOTICES-RUST.md` | **Not generated.** This payload was produced with -SkipRustDependencies and is incomplete. |' } else { '| `THIRD-PARTY-NOTICES-RUST.md` | Every third-party Rust crate compiled into the binaries, with its notice. |' }),
        '| `ffmpeg/NOTICE.md` | Which FFmpeg is shipped, under what licence, and where its source is. |',
        '| `ffmpeg/LGPL-3.0.txt` | The GNU Lesser General Public License v3, FFmpeg''s licence. |',
        '| `ffmpeg/GPL-3.0.txt` | The GNU General Public License v3, which the LGPL is written on top of. |',
        '',
        "$payloadMarker on $((Get-Date).ToUniversalTime().ToString('yyyy-MM-dd')). Do not edit by hand: run it again."
    )

    # The payload is complete, so it becomes the destination. The window in
    # which neither the old payload nor the new one is in place is one rename
    # wide, rather than the whole run.
    if (Test-Path -LiteralPath $Destination) {
        Remove-Item -LiteralPath $Destination -Recurse -Force
    }
    Move-Item -LiteralPath $assembly -Destination $Destination
    $assembly = ''

    Write-Step ''
    Write-Step "Notices payload written to $Destination"
    if ($SkipRustDependencies) {
        Write-Step '  Incomplete: the Rust dependency notices were skipped, and an installer must not ship this.'
    }
    exit 0
} catch {
    # Whatever had been assembled is incomplete by definition, and leaving it
    # beside the destination would be litter that looks like a payload.
    if ($assembly -and (Test-Path -LiteralPath $assembly)) {
        Remove-Item -LiteralPath $assembly -Recurse -Force -ErrorAction SilentlyContinue
    }
    Write-Host ''
    Write-Host $_.Exception.Message -ForegroundColor Red
    exit 1
}
