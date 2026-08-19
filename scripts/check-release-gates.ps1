#Requires -Version 5.1

<#
.SYNOPSIS
    Decides whether a tag may become a release of Clipped, and refuses with a
    reason when it may not.

.DESCRIPTION
    A release is the only thing this project does that cannot be undone by a
    revert: once an installer is on somebody's machine, a version number nobody
    chose or a missing licence text is already on it. So the tag does not build
    a release on its own - it asks this script for permission first, and every
    check below is a way of saying no.

    Seven gates, each of which is a thing that has gone wrong for somebody else:

    1. Version.      The tag is the source of truth for the version, and every
                     version this repository declares has to agree with it. A
                     release built from a tag that disagrees ships an installer
                     reporting a version nobody chose, which is unfixable after
                     the fact because the number is what a bug report will
                     quote. Nothing here rewrites a file to agree: bumping the
                     version is a reviewed commit on `main`, not something a
                     release workflow does behind you.
    2. Branch.       The tagged commit has to be an ancestor of `origin/main`.
                     Tagging a branch is how a release ends up carrying work
                     nobody reviewed.
    3. CI.           CI has to have passed on that exact commit. "It was green
                     on the pull request" is a statement about a different
                     tree.
    4. Milestones.   Nothing is released until every milestone is finished. See
                     docs/releasing.md for what that means and who decides it.
    5. Licences.     The installer carries a pinned LGPL v3 FFmpeg, and
                     distributing it owes a notice, both licence texts and the
                     third-party notices (docs/licensing.md, issue #123). A
                     build that can publish before those ship is a build that
                     can break a licence by accident.
    6. Corresponding The other half of that obligation: the source of the exact
       source.       FFmpeg build being shipped, published with the binary
                     rather than promised. scripts/fetch-ffmpeg-source.ps1
                     assembles it and .github/workflows/release.yml attaches it
                     to the release; this gate is what notices when that step is
                     removed, fails, or assembles the source of a different
                     build than the one the pin now names. The notice inside
                     every installed copy says the source is published with the
                     release, so a release that publishes none makes an
                     installed file lie.
    7. Codec         Copyright licences are not patent licences, and the two
       patents.      gates above are entirely about copyright. ADR 0008 is the
                     patent position, and its sixth decision blocks the first
                     signed public release on four questions being put to
                     somebody qualified - not on a particular answer. This gate
                     is what makes that a block rather than a sentence: it
                     reads the answers section of that record and refuses while
                     it is unanswered. It cannot check that a lawyer was
                     consulted, only that the answers were written down where
                     everybody can read them, which is the most a script can
                     honestly claim here.

    Every gate is evaluated, always, even after one has refused. A release
    blocked by four things should say so once, rather than four times over four
    attempts.

    The three GitHub questions - which milestones exist, what has been
    released, whether CI passed - are answered by JSON this script is handed
    rather than by calls it makes. That is what lets every branch below be
    tested against a fixture instead of against the live repository, and it
    keeps the network in the workflow where a reader can see it.
    docs/releasing.md prints the three `gh api` commands that produce them.

.PARAMETER Tag
    The tag under consideration, leading `v` included - `v1.0.0`. Taken as the
    tag rather than as a version so that the rule for turning one into the other
    lives here and not in the workflow.

.PARAMETER CommitSha
    The commit the tag points at. Checked for ancestry of -MainRef, and quoted
    in the report.

.PARAMETER MilestonesJson
    Path to the output of `gh api "repos/{owner}/{repo}/milestones?state=all"`.

.PARAMETER ReleasesJson
    Path to the output of `gh api "repos/{owner}/{repo}/releases"`. What has
    already been published decides whether the milestone gate still applies.

.PARAMETER CiRunsJson
    Path to the output of
    `gh api "repos/{owner}/{repo}/actions/workflows/ci.yml/runs?head_sha={sha}"`.

.PARAMETER RepositoryRoot
    The checkout to read the version declarations and the bundle from. Defaults
    to the parent of this script.

.PARAMETER MainRef
    The ref the tagged commit must be an ancestor of. `origin/main` in the
    workflow; the tests point it at a local branch.

.PARAMETER CorrespondingSourceDirectory
    Where scripts/fetch-ffmpeg-source.ps1 assembled the corresponding source of
    the FFmpeg being shipped. Defaults to that script's own default,
    third-party/ffmpeg/source, so that a person running the gates by hand after
    running it needs no argument. release.yml passes a directory of its own,
    outside the cached third-party tree, for the reason given there.

.PARAMETER FetchScript
    The script the FFmpeg pin is read from, by running it with -PrintPin. The
    pin is asked for rather than parsed, so that this gate cannot come to a
    different conclusion about which build is shipped than the script that
    downloads it. The tests point it at a fixture.

.PARAMETER Rehearse
    Report on every gate and exit 0 whatever they say. For a rehearsal that
    cannot publish anything - `workflow_dispatch` in
    .github/workflows/release.yml - so that the gates can be read on a day when
    they all refuse. It is not an override: the workflow passes it on no path
    that reaches `gh release create`, and there is no way to reach one from a
    tag push. Changing what a release is allowed to do is a pull request
    against docs/releasing.md, not a flag.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts/check-release-gates.ps1 `
        -Tag v1.0.0 -CommitSha $sha `
        -MilestonesJson milestones.json -ReleasesJson releases.json `
        -CiRunsJson ci-runs.json

.OUTPUTS
    Exit code 0 when every gate passes, 1 when any refuses. Prints one section
    per gate, naming what is wrong and what would fix it.
#>

[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $Tag,
    [Parameter(Mandatory)] [string] $CommitSha,
    [Parameter(Mandatory)] [string] $MilestonesJson,
    [Parameter(Mandatory)] [string] $ReleasesJson,
    [Parameter(Mandatory)] [string] $CiRunsJson,
    [string] $RepositoryRoot = '',
    [string] $MainRef = 'origin/main',
    [string] $CorrespondingSourceDirectory = '',
    [string] $FetchScript = '',
    [switch] $Rehearse
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if (-not $RepositoryRoot) { $RepositoryRoot = Split-Path -Parent $PSScriptRoot }
if (-not $CorrespondingSourceDirectory) {
    $CorrespondingSourceDirectory = Join-Path $RepositoryRoot 'third-party\ffmpeg\source'
}
if (-not $FetchScript) { $FetchScript = Join-Path $RepositoryRoot 'scripts\fetch-ffmpeg.ps1' }

# Semantic versioning, with the pre-release and build-metadata parts it allows.
# Anchored, so `v1.2` and `v1.0.0.1` are refused by name rather than quietly
# accepted and turned into an installer version Windows rounds off.
$semanticVersion = '^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$'

# What ADR 0008's answers section carries until somebody has put the four
# questions to a lawyer. The record says this token is what the gate looks for,
# so a maintainer filling the section in is not guessing at a format.
$unansweredPlaceholder = '_UNANSWERED_'

function New-GateResult {
    <#
    .SYNOPSIS
        One gate's verdict and the lines it wants printed under its heading.
    #>
    param(
        [Parameter(Mandatory)] [string] $Name,
        [Parameter(Mandatory)] [bool] $Passed,
        [string[]] $Lines = @()
    )

    return [pscustomobject]@{
        Name   = $Name
        Passed = $Passed
        Lines  = @($Lines)
    }
}

function Read-JsonFile {
    <#
    .SYNOPSIS
        Reads one of the three GitHub answers, or explains why it could not.
    .DESCRIPTION
        A gate that cannot see its evidence must refuse, not pass. An empty file
        - which is what a failed `gh api` redirected into one leaves behind - is
        the case that would otherwise read as "no milestones, nothing to
        object to".
    #>
    param(
        [Parameter(Mandatory)] [string] $Path,
        [Parameter(Mandatory)] [string] $Produces
    )

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "$Path does not exist. It should hold the output of: $Produces"
    }

    $raw = Get-Content -LiteralPath $Path -Raw
    if (-not $raw -or -not $raw.Trim()) {
        throw "$Path is empty. It should hold the output of: $Produces"
    }

    try {
        $parsed = ConvertFrom-Json $raw
    } catch {
        throw "$Path is not JSON. It should hold the output of: $Produces"
    }

    # Windows PowerShell 5.1 turns an empty JSON array into $null rather than
    # into an empty array, and @($null) is a one-element list holding nothing.
    # Under Set-StrictMode that element throws on the first property read, in a
    # later gate, with a message about `state` and no mention of this file.
    if ($null -eq $parsed) { return @() }
    return $parsed
}

function Get-RelativePath {
    param(
        [Parameter(Mandatory)] [string] $Path
    )

    $full = (Resolve-Path -LiteralPath $Path).ProviderPath
    $root = (Resolve-Path -LiteralPath $RepositoryRoot).ProviderPath
    if ($full.StartsWith($root, [System.StringComparison]::OrdinalIgnoreCase)) {
        return $full.Substring($root.Length).TrimStart('\', '/')
    }
    return $full
}

function Test-IsBuildOutput {
    <#
    .SYNOPSIS
        True for a path under a directory nothing in this repository authors.
    .DESCRIPTION
        `target`, `node_modules`, `dist` and `third-party` all contain manifests
        with versions in them, and every one of those versions belongs to
        somebody else. Matched on path segments rather than as substrings, so a
        crate legitimately called `dist-something` is not skipped.

        Matched on the part *below* the repository root, too. Against the
        absolute path, a checkout that happens to live under a directory called
        `dist` - or `target`, or `.git`, none of which are far-fetched names for
        a directory somebody keeps checkouts in - skips every manifest in the
        tree. The version gate then finds no declaration anywhere and refuses a
        correct tag, telling whoever pushed it that this check has a bug. It
        does, and this is it.
    #>
    param(
        [Parameter(Mandatory)] [string] $Path
    )

    $segments = (Get-RelativePath $Path) -split '[\\/]'
    foreach ($segment in $segments) {
        if (@('target', 'node_modules', 'dist', 'third-party', '.git') -contains $segment) { return $true }
    }
    return $false
}

function Get-CargoVersionDeclaration {
    <#
    .SYNOPSIS
        Every version literal in a Cargo manifest that names Clipped's own.
    .DESCRIPTION
        Two shapes matter, and missing either one turns a version bump into a
        build failure several minutes later rather than a refusal here:

        - `[package] version` and `[workspace.package] version`, which are what
          a crate publishes as its own version;
        - the version requirement on a *path* dependency, as in
          `clipped-ipc = { path = "...", version = "0.1.0" }`. Cargo resolves
          the path but still enforces the requirement, so a workspace bumped to
          1.0.0 with these left at 0.1.0 does not build at all.

        A third-party dependency's `version = "1"` has no `path` beside it and
        is not Clipped's version to change, so it is not collected.

        Read line by line rather than with a TOML parser, because Windows
        PowerShell 5.1 ships none and this repository's manifests are written by
        rustfmt conventions rather than by hand-rolled multi-line tables. It errs
        towards finding a declaration: an extra one that already agrees costs a
        line of report, a missed one costs a failed build.
    #>
    param(
        [Parameter(Mandatory)] [string] $Path
    )

    $found = @()
    $section = ''
    foreach ($line in (Get-Content -LiteralPath $Path)) {
        $trimmed = $line.Trim()
        if ($trimmed -match '^\[([^\]]+)\]$') {
            $section = $Matches[1]
            continue
        }
        if ($trimmed.StartsWith('#')) { continue }

        if (($section -eq 'package' -or $section -eq 'workspace.package') -and
            $trimmed -match '^version\s*=\s*"([^"]+)"') {
            $found += [pscustomobject]@{ Where = "[$section] version"; Value = $Matches[1] }
            continue
        }

        # A path dependency carries the version it requires of a sibling crate.
        if ($trimmed -match 'path\s*=\s*"' -and $trimmed -match 'version\s*=\s*"([^"]+)"') {
            $value = $Matches[1]
            $name = '(dependency)'
            if ($trimmed -match '^([A-Za-z0-9_.-]+)\s*=') { $name = $Matches[1] }
            $found += [pscustomobject]@{ Where = "$name (path dependency)"; Value = $value }
        }
    }

    return $found
}

function Get-VersionDeclaration {
    <#
    .SYNOPSIS
        Every place in the tree that says what version Clipped is.
    .DESCRIPTION
        Discovered rather than listed, so that a package added next year is
        covered without anybody remembering to add it here. A hardcoded list is
        the version of this check that passes while the build carries a
        different number.
    #>

    $declarations = @()

    foreach ($manifest in (Get-ChildItem -LiteralPath $RepositoryRoot -Recurse -Filter 'Cargo.toml' -File -Force)) {
        if (Test-IsBuildOutput $manifest.FullName) { continue }
        foreach ($declaration in (Get-CargoVersionDeclaration -Path $manifest.FullName)) {
            $declarations += [pscustomobject]@{
                File  = (Get-RelativePath $manifest.FullName)
                Where = $declaration.Where
                Value = $declaration.Value
            }
        }
    }

    foreach ($manifest in (Get-ChildItem -LiteralPath $RepositoryRoot -Recurse -Filter 'package.json' -File -Force)) {
        if (Test-IsBuildOutput $manifest.FullName) { continue }
        $package = Get-Content -LiteralPath $manifest.FullName -Raw | ConvertFrom-Json
        $name = ''
        if ($package.PSObject.Properties.Name -contains 'name') { $name = [string]$package.name }
        # Somebody else's package.json under a directory of ours - a fixture, a
        # vendored example - is not this repository declaring its own version.
        if ($name -ne 'clipped' -and -not $name.StartsWith('@clipped/')) { continue }
        if ($package.PSObject.Properties.Name -notcontains 'version') { continue }
        $declarations += [pscustomobject]@{
            File  = (Get-RelativePath $manifest.FullName)
            Where = 'version'
            Value = [string]$package.version
        }
    }

    foreach ($manifest in (Get-ChildItem -LiteralPath $RepositoryRoot -Recurse -Filter 'tauri.conf.json' -File -Force)) {
        if (Test-IsBuildOutput $manifest.FullName) { continue }
        $config = Get-Content -LiteralPath $manifest.FullName -Raw | ConvertFrom-Json
        if ($config.PSObject.Properties.Name -notcontains 'version') { continue }
        $declarations += [pscustomobject]@{
            File  = (Get-RelativePath $manifest.FullName)
            Where = 'version'
            Value = [string]$config.version
        }
    }

    return $declarations
}

function Test-VersionGate {
    <#
    .SYNOPSIS
        The tag is a version, and the tree agrees with it everywhere.
    #>

    $lines = @()

    if (-not $Tag.StartsWith('v')) {
        return New-GateResult -Name 'Version' -Passed $false -Lines @(
            "The tag is '$Tag'. A release tag is a `v` followed by a semantic version, as in v1.0.0.",
            'Nothing was built. See docs/releasing.md for the tag format.'
        )
    }

    $version = $Tag.Substring(1)
    if ($version -notmatch $semanticVersion) {
        return New-GateResult -Name 'Version' -Passed $false -Lines @(
            "The tag is '$Tag', and '$version' is not a semantic version.",
            'Expected MAJOR.MINOR.PATCH, optionally with a pre-release such as v1.0.0-rc.1.',
            'A tag like v1.2 would become an installer version Windows and Cargo disagree about.'
        )
    }

    $declarations = @(Get-VersionDeclaration)
    if ($declarations.Count -eq 0) {
        return New-GateResult -Name 'Version' -Passed $false -Lines @(
            "No version declaration was found anywhere under $RepositoryRoot.",
            'That is a bug in this check, not a clean tree: the workspace Cargo.toml and',
            'apps/desktop/src-tauri/tauri.conf.json both carry one.'
        )
    }

    $disagreeing = @($declarations | Where-Object { $_.Value -ne $version })
    if ($disagreeing.Count -eq 0) {
        $lines += "The tag says $version, and so does every one of the $($declarations.Count) places this repository declares a version:"
        foreach ($declaration in ($declarations | Sort-Object File, Where)) {
            $lines += "    $($declaration.File)  ($($declaration.Where))"
        }
        return New-GateResult -Name 'Version' -Passed $true -Lines $lines
    }

    $lines += "The tag says $version. These do not:"
    $lines += ''
    foreach ($declaration in ($disagreeing | Sort-Object File, Where)) {
        $lines += ("    {0,-46} {1,-32} says {2}" -f $declaration.File, $declaration.Where, $declaration.Value)
    }
    $lines += ''
    $lines += 'Nothing here has been rewritten to agree. Which version Clipped is, is a'
    $lines += 'decision recorded in a reviewed commit on main; a release workflow that'
    $lines += 'edited these would be deciding it for you, and the tag would stop being'
    $lines += 'evidence of anything. Bump them on main, let CI go green, and tag that'
    $lines += 'commit instead. Remember Cargo.lock and package-lock.json: `cargo build`'
    $lines += 'and `npm install` update them, and the release build uses --locked and'
    $lines += '`npm ci`, which refuse a lockfile that disagrees.'

    return New-GateResult -Name 'Version' -Passed $false -Lines $lines
}

function Test-BranchGate {
    <#
    .SYNOPSIS
        The tagged commit is on main.
    #>

    $onMain = $false
    $reachable = $false

    Push-Location $RepositoryRoot
    try {
        $previous = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        try {
            & git cat-file -e "$CommitSha^{commit}" 2>&1 | Out-Null
            $reachable = ($LASTEXITCODE -eq 0)
            if ($reachable) {
                & git merge-base --is-ancestor $CommitSha $MainRef 2>&1 | Out-Null
                $onMain = ($LASTEXITCODE -eq 0)
            }
        } finally {
            $ErrorActionPreference = $previous
        }
    } finally {
        Pop-Location
    }

    if (-not $reachable) {
        return New-GateResult -Name 'Branch' -Passed $false -Lines @(
            "The tagged commit $CommitSha is not in this checkout, so where it sits cannot be established.",
            'The release workflow checks out with fetch-depth: 0 and fetches main for exactly this reason.'
        )
    }

    if (-not $onMain) {
        return New-GateResult -Name 'Branch' -Passed $false -Lines @(
            "$CommitSha is not an ancestor of $MainRef.",
            '',
            'A tag on a branch is how a release ships work nobody reviewed and nobody merged.',
            'Merge it first, then tag the commit on main.'
        )
    }

    return New-GateResult -Name 'Branch' -Passed $true -Lines @(
        "$CommitSha is an ancestor of $MainRef, so the tag names reviewed, merged work."
    )
}

function Test-ContinuousIntegrationGate {
    <#
    .SYNOPSIS
        CI passed on this exact commit.
    #>

    $runs = Read-JsonFile -Path $CiRunsJson -Produces "gh api `"repos/{owner}/{repo}/actions/workflows/ci.yml/runs?head_sha=$CommitSha`""

    $all = @()
    if ($null -ne $runs -and $runs.PSObject.Properties.Name -contains 'workflow_runs') {
        $all = @($runs.workflow_runs | Where-Object { $null -ne $_ })
    }

    $successful = @($all | Where-Object { $_.status -eq 'completed' -and $_.conclusion -eq 'success' })
    if ($successful.Count -gt 0) {
        $run = $successful[0]
        return New-GateResult -Name 'Continuous integration' -Passed $true -Lines @(
            "CI passed on ${CommitSha}: run $($run.run_number), $($run.html_url)"
        )
    }

    $lines = @("No successful CI run exists for $CommitSha.")
    $lines += ''
    if ($all.Count -eq 0) {
        $lines += 'No run of ci.yml has ever been recorded against that commit. A commit that'
        $lines += 'reached main has been through CI, so this usually means the tag names a'
        $lines += 'commit that never did.'
    } else {
        $lines += "$($all.Count) run(s) exist against it, and none of them is a completed success:"
        foreach ($run in $all) {
            $lines += ("    run {0,-6} {1,-12} {2,-12} {3}" -f $run.run_number, $run.status, $run.conclusion, $run.html_url)
        }
        $lines += ''
        $lines += 'A red or unfinished CI run is not a commit anybody should be shipping. If'
        $lines += 'the failure is a known flake, re-run it and let it go green, so that what'
        $lines += 'is recorded against the released commit is a pass rather than an argument.'
    }

    return New-GateResult -Name 'Continuous integration' -Passed $false -Lines $lines
}

function Test-MilestoneGate {
    <#
    .SYNOPSIS
        Nothing is released until every milestone is finished.
    .DESCRIPTION
        Two conditions per milestone, and both are needed:

        - it is *closed*, which only somebody with write access can do, and
          which is the human saying the acceptance criteria that need a person
          at a keyboard were actually met;
        - it has *no open issues*, which is the part a script can check and the
          part a person forgets.

        Closing a milestone on GitHub does not require its issues to be closed,
        so neither condition implies the other.

        The gate retires itself once anything has been published, because after
        the first release the question stops being "is the product finished"
        and becomes ordinary semantic versioning. A draft does not retire it: a
        draft is not something anybody has been given.
    #>

    $releases = Read-JsonFile -Path $ReleasesJson -Produces 'gh api "repos/{owner}/{repo}/releases"'
    $published = @(@($releases) | Where-Object { $_ -and $_.draft -eq $false })

    if ($published.Count -gt 0) {
        $first = @($published | Sort-Object published_at)[0]
        return New-GateResult -Name 'Milestones' -Passed $true -Lines @(
            "Retired. $($first.tag_name) has been published, so this repository has released before.",
            'Which version comes next is semantic versioning over what changed, not a',
            'question about milestones. See docs/releasing.md.'
        )
    }

    $milestones = @(Read-JsonFile -Path $MilestonesJson -Produces 'gh api "repos/{owner}/{repo}/milestones?state=all"')

    if ($milestones.Count -eq 0) {
        return New-GateResult -Name 'Milestones' -Passed $false -Lines @(
            'This repository has no milestones at all, and nothing has been released.',
            'That is not the finished state - it is a query that returned nothing, which is',
            'the one answer that must not be read as permission.'
        )
    }

    # release.yml asks for one page of a hundred, because `gh api --paginate`
    # concatenates pages into a stream of JSON arrays rather than into one. A
    # full page therefore means "there may be more", and an unseen milestone is
    # exactly the thing that would let this gate pass when it should not.
    if ($milestones.Count -ge 100) {
        return New-GateResult -Name 'Milestones' -Passed $false -Lines @(
            "$($milestones.Count) milestones were supplied, which is a full page.",
            'There may be more that this gate has not seen, and one of them may be',
            'unfinished. Paginate the query in .github/workflows/release.yml before',
            'releasing again.'
        )
    }

    $unfinished = @($milestones | Where-Object { $_.state -ne 'closed' -or $_.open_issues -gt 0 })
    if ($unfinished.Count -eq 0) {
        return New-GateResult -Name 'Milestones' -Passed $true -Lines @(
            "All $($milestones.Count) milestones are closed with no open issues, and nothing has been released yet.",
            'This is the first release the project has ever been allowed to make.'
        )
    }

    $lines = @(
        'Nothing has been released yet, and Clipped does not release until every',
        'milestone is finished. These are not:',
        ''
    )
    foreach ($milestone in ($unfinished | Sort-Object title)) {
        $reason = @()
        if ($milestone.state -ne 'closed') { $reason += 'still open' }
        if ($milestone.open_issues -gt 0) { $reason += "$($milestone.open_issues) open issue(s)" }
        $lines += ("    {0,-34} {1}" -f $milestone.title, ($reason -join ', '))
    }
    $lines += ''
    $lines += ("$($unfinished.Count) of $($milestones.Count) milestones. A milestone is finished when every issue in it is")
    $lines += 'closed *and* a maintainer has closed the milestone on GitHub - the second'
    $lines += 'being the part no script can do, because several issues here are open'
    $lines += 'precisely because a criterion needs a human at a keyboard. docs/releasing.md'
    $lines += 'has the rule and the reasoning.'

    return New-GateResult -Name 'Milestones' -Passed $false -Lines $lines
}

function Get-BundledFile {
    <#
    .SYNOPSIS
        Every file the installer will collect, according to tauri.conf.json.
    .DESCRIPTION
        Asked of `bundle.resources` rather than of one directory, so that this
        gate does not assume how the licence payload gets into the bundle. It is
        issue #123's decision whether the notices are staged into
        installer-payload or collected from somewhere else; either way they end
        up under a declared resource, and either way this gate sees them.
    #>

    $config = Join-Path $RepositoryRoot 'apps\desktop\src-tauri\tauri.conf.json'
    if (-not (Test-Path -LiteralPath $config -PathType Leaf)) {
        throw "$config does not exist, so what the installer bundles cannot be established."
    }

    $bundleRoot = Split-Path -Parent $config
    $parsed = Get-Content -LiteralPath $config -Raw | ConvertFrom-Json

    $sources = @()
    if ($parsed.PSObject.Properties.Name -contains 'bundle' -and
        $parsed.bundle.PSObject.Properties.Name -contains 'resources') {
        $resources = $parsed.bundle.resources
        if ($resources -is [System.Array]) {
            $sources = @($resources)
        } else {
            $sources = @($resources.PSObject.Properties.Name)
        }
    }

    $files = @()
    foreach ($source in $sources) {
        $path = Join-Path $bundleRoot ($source.TrimEnd('/', '\'))
        if (Test-Path -LiteralPath $path -PathType Container) {
            $files += @(Get-ChildItem -LiteralPath $path -Recurse -File -Force)
        } elseif (Test-Path -LiteralPath $path -PathType Leaf) {
            $files += @(Get-Item -LiteralPath $path)
        } else {
            # A glob, or a resource that has not been produced yet. Resolve-Path
            # answers both without throwing.
            $files += @(Resolve-Path -Path $path -ErrorAction SilentlyContinue |
                    ForEach-Object { Get-Item -LiteralPath $_.ProviderPath } |
                    Where-Object { -not $_.PSIsContainer })
        }
    }

    return [pscustomobject]@{
        Declared = @($sources)
        Files    = @($files)
    }
}

function Test-LicenceGate {
    <#
    .SYNOPSIS
        The installer carries what distributing it obliges Clipped to carry.
    .DESCRIPTION
        The file names are the ones scripts/collect-notices.ps1 writes, and the
        obligations they discharge are set out in docs/licensing.md. This gate
        checks the artefact rather than the issue tracker: #123 being closed is
        somebody's opinion, whereas a bundle without GPL-3.0.txt in it is a
        licence breach whoever thinks what.
    #>

    $required = @(
        [pscustomobject]@{ Name = 'LICENSE.txt'; Why = "Clipped's own MPL-2.0 text" }
        [pscustomobject]@{ Name = 'THIRD-PARTY-NOTICES.md'; Why = 'third-party material inside Clipped''s own source' }
        [pscustomobject]@{ Name = 'THIRD-PARTY-NOTICES-RUST.md'; Why = 'the notice every linked Rust crate requires to travel with the binary' }
        [pscustomobject]@{ Name = 'NOTICE.md'; Why = 'LGPL v3 section 4(a): which FFmpeg is shipped and that it is LGPL' }
        [pscustomobject]@{ Name = 'LGPL-3.0.txt'; Why = 'LGPL v3 section 4(b): the LGPL text itself' }
        [pscustomobject]@{ Name = 'GPL-3.0.txt'; Why = 'LGPL v3 section 4(b): the GPL text the LGPL is written on top of' }
    )

    $bundle = Get-BundledFile
    $names = @($bundle.Files | ForEach-Object { $_.Name })
    $missing = @($required | Where-Object { $names -notcontains $_.Name })

    if ($missing.Count -eq 0) {
        return New-GateResult -Name 'Licences' -Passed $true -Lines @(
            "All six required texts are among the $($bundle.Files.Count) files the installer bundles.",
            'docs/licensing.md is what they discharge; nothing here checks their contents.'
        )
    }

    $declared = '`bundle.resources` names ' + ($bundle.Declared -join ', ')
    if ($bundle.Declared.Count -eq 0) { $declared = 'tauri.conf.json declares no `bundle.resources` at all' }

    $lines = @('The installer would not carry what distributing it obliges Clipped to carry:', '')
    foreach ($file in $missing) {
        $lines += ("    {0,-30} {1}" -f $file.Name, $file.Why)
    }
    $lines += ''
    if ($bundle.Files.Count -eq 0) {
        $lines += "It would carry nothing at all. $declared,"
        $lines += 'and no file was found under it. If this ran before the build, the payload'
        $lines += 'has not been staged yet: scripts/stage-installer-payload.ps1 does that, and'
        $lines += 'tauri.conf.json runs it from beforeBuildCommand.'
    } else {
        $lines += "It would carry $($bundle.Files.Count) files, none of which are those. $declared."
    }
    $lines += ''
    $lines += 'This is issue #123, and it is the one gate here that is about somebody else'
    $lines += 'rather than about you: the installer ships a pinned LGPL v3 FFmpeg, and'
    $lines += 'conveying it owes a notice, both licence texts and the corresponding source.'
    $lines += 'scripts/collect-notices.ps1 already produces every file above; what is'
    $lines += 'missing is putting the payload into the bundle. Until that is done, an'
    $lines += 'installer built from this tree is not one that may be distributed, and this'
    $lines += 'gate is what stops a workflow distributing it by accident.'

    return New-GateResult -Name 'Licences' -Passed $false -Lines $lines
}

function Get-PinnedFFmpeg {
    <#
    .SYNOPSIS
        The FFmpeg build this tree ships, asked of the script that downloads it.
    .DESCRIPTION
        The pin is a set of parameter defaults in scripts/fetch-ffmpeg.ps1, and
        this gate must not carry a second copy of it or a regular expression
        over it: either would let the gate approve source for one build while
        the release job downloaded another. `-PrintPin` exists so that anything
        needing the pin can ask the pin, and this is the second caller of it
        after ci.yml's cache key.
    #>

    if (-not (Test-Path -LiteralPath $FetchScript -PathType Leaf)) {
        throw "$FetchScript does not exist, so which FFmpeg build this release would ship cannot be established."
    }

    # Windows PowerShell wraps a native command's stderr in ErrorRecords, and
    # under 'Stop' that terminates before the exit code has been read. The exit
    # code is what decides here.
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $printed = & powershell -ExecutionPolicy Bypass -File $FetchScript -PrintPin 2>&1 | Out-String
        $code = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previous
    }

    if ($code -ne 0) {
        throw "$FetchScript -PrintPin exited $code, so the FFmpeg pin could not be read:`n$printed"
    }

    $pin = @{}
    foreach ($line in ($printed -split "`r?`n")) {
        if ($line -match '^(?<key>[A-Za-z0-9_]+)=(?<value>.+)$') {
            $pin[$Matches['key'].ToLowerInvariant()] = $Matches['value'].Trim()
        }
    }

    foreach ($key in @('tag', 'asset', 'sha256')) {
        if (-not $pin.ContainsKey($key)) {
            throw "$FetchScript -PrintPin printed no $key, so the FFmpeg pin could not be read:`n$printed"
        }
    }

    return $pin
}

function Get-ZipEntryCount {
    <#
    .SYNOPSIS
        How many entries an archive holds, or -1 if it is not an archive.
    .DESCRIPTION
        A file of the right name and a source archive are different things, and
        the difference is invisible in a directory listing. A truncated
        download, an error page saved with a .zip name and an empty file all
        pass a "does it exist" check and are all useless to whoever is owed the
        source.
    #>
    param([Parameter(Mandatory)] [string] $Path)

    Add-Type -AssemblyName System.IO.Compression.FileSystem -ErrorAction SilentlyContinue

    $zip = $null
    try {
        $zip = [System.IO.Compression.ZipFile]::OpenRead($Path)
        return $zip.Entries.Count
    } catch {
        return -1
    } finally {
        if ($zip) { $zip.Dispose() }
    }
}

function Test-CorrespondingSourceGate {
    <#
    .SYNOPSIS
        The source of the exact FFmpeg being shipped is assembled and ready to
        publish, and is the source of *this* build.
    .DESCRIPTION
        The installed NOTICE.md tells every recipient that the source of these
        libraries "is published with the Clipped release that carries these
        files". That sentence is either true on the day of the release or it is
        a false statement inside an artefact nobody can recall, so it is checked
        here rather than left to whoever is drafting at the time.

        Four things, and each of them has a way of being wrong that the others
        do not catch:

        - the manifest is there at all, which is what fails when the step that
          assembles the source is removed from the workflow or errors out;
        - it names the pinned asset and its SHA-256, which is what fails when
          the pin moves and a directory assembled for the previous build is
          left behind - the case where every file exists and every one of them
          is the source of something else;
        - it promises two archives rather than one. FFmpeg without the build
          recipe is not the corresponding source of these DLLs: the configure
          arguments and the versions of every external library compiled into
          them live in BtbN/FFmpeg-Builds;
        - every archive the manifest promises exists and opens as an archive,
          which is what fails on a truncated fetch.

        Deliberately not checked: that the archives contain FFmpeg. The commit
        ids in the manifest are what tie them to the build, they were verified
        against the remote when the archives were made
        (scripts/fetch-ffmpeg-source.ps1), and a gate that tried to recognise
        FFmpeg by its file names would refuse a correct release the first time
        upstream renamed something.
    #>

    $manifestName = 'CORRESPONDING-SOURCE.md'
    $manifest = Join-Path $CorrespondingSourceDirectory $manifestName

    # Caught here rather than left to the outer handler, which would report
    # "a gate could not be evaluated" without saying which one or why. Not
    # knowing which FFmpeg is shipped is a refusal either way: the question this
    # gate asks is whether the source matches it.
    try {
        $pin = Get-PinnedFFmpeg
    } catch {
        return New-GateResult -Name 'Corresponding source' -Passed $false -Lines @(
            'Which FFmpeg build this release would ship could not be established, so',
            'whether the corresponding source matches it cannot be either.',
            '',
            "  $($_.Exception.Message)"
        )
    }

    $how = @(
        '',
        'scripts/fetch-ffmpeg-source.ps1 assembles it - two archives and a manifest',
        'naming the FFmpeg commit and the build recipe the DLLs were produced from - and',
        '.github/workflows/release.yml runs it before this gate and attaches what it',
        'produced to the release. docs/licensing.md is the whole obligation; issue #123',
        'is the work.'
    )

    if (-not (Test-Path -LiteralPath $manifest -PathType Leaf)) {
        return New-GateResult -Name 'Corresponding source' -Passed $false -Lines (@(
                "No $manifestName in $CorrespondingSourceDirectory,",
                'so this release has no corresponding source to publish. The installer would',
                'ship',
                '',
                "    $($pin.asset)",
                '',
                'whose libraries are LGPL v3. Conveying them obliges this project to give',
                'whoever received them the source of that exact build, and the NOTICE.md inside',
                'the installer tells them it is published with the release. Publishing the',
                'installer without it would make that sentence false on somebody else''s machine.'
            ) + $how)
    }

    $recorded = Get-Content -LiteralPath $manifest -Raw
    if (-not $recorded) { $recorded = '' }

    if (-not ($recorded.Contains($pin.asset) -and $recorded.Contains($pin.sha256))) {
        return New-GateResult -Name 'Corresponding source' -Passed $false -Lines (@(
                "$(Get-RelativePath $manifest) is not the source of the build this release would ship.",
                '',
                "    the pin names   $($pin.asset)",
                "    SHA-256         $($pin.sha256)",
                '',
                'and the manifest does not record both of those. A directory left over from a',
                'previous pin looks exactly like a current one: every file is present, every',
                'archive opens, and all of it is the source of a build nobody is shipping.',
                'Re-assemble it, or delete it and let the workflow assemble it again.'
            ) + $how)
    }

    # Every archive the manifest promises. Taken from the manifest rather than
    # from a directory listing, because the manifest is the document a recipient
    # reads: a file it names and the release does not carry is the failure, and
    # an extra file beside it is not. The pinned binary asset is named in there
    # too - it is what the source corresponds to - and is not one of ours to
    # find on disk.
    $named = @([regex]::Matches($recorded, '`(?<file>[^`]+\.zip)`') |
            ForEach-Object { $_.Groups['file'].Value } |
            Where-Object { $_ -ne $pin.asset } |
            Sort-Object -Unique)

    if ($named.Count -lt 2) {
        return New-GateResult -Name 'Corresponding source' -Passed $false -Lines (@(
                "$(Get-RelativePath $manifest) names $($named.Count) source archive(s), and the corresponding",
                'source is two: FFmpeg at the commit the binary was built from, and the build',
                'recipe it was configured and compiled by. FFmpeg alone does not describe the',
                'artefact - the configure arguments and every external library compiled into it',
                'live in BtbN/FFmpeg-Builds - so half of it is not the corresponding source of',
                'anything.'
            ) + $how)
    }

    # Small enough that a real source archive cannot be mistaken for a stub, and
    # large enough that no plausible source tree is under it: the two archives
    # this produces hold ten thousand entries and three hundred.
    $minimumEntries = 25

    $problems = @()
    $present = @()
    foreach ($archive in $named) {
        $path = Join-Path $CorrespondingSourceDirectory $archive
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            $problems += ("    {0,-52} promised by the manifest, not in the directory" -f $archive)
            continue
        }

        $entries = Get-ZipEntryCount -Path $path
        $bytes = (Get-Item -LiteralPath $path).Length
        if ($entries -lt 0) {
            $problems += ("    {0,-52} {1:N0} bytes, and not a readable archive" -f $archive, $bytes)
            continue
        }
        if ($entries -lt $minimumEntries) {
            $problems += ("    {0,-52} opens, but holds only {1} entries" -f $archive, $entries)
            continue
        }

        $present += ("    {0,-52} {1,7:N1} MB, {2:N0} entries" -f $archive, ($bytes / 1MB), $entries)
    }

    if ($problems.Count -gt 0) {
        return New-GateResult -Name 'Corresponding source' -Passed $false -Lines (@(
                'The corresponding source is incomplete:',
                ''
            ) + $problems + @(
                '',
                'A file of the right name is not a source archive, and the difference does not',
                'show in a directory listing. Whoever is owed this source is owed something',
                'they can build.'
            ) + $how)
    }

    return New-GateResult -Name 'Corresponding source' -Passed $true -Lines (@(
            'The release publishes the source of the FFmpeg it ships:',
            '',
            "    build       $($pin.asset)",
            "    assembled   $(Get-RelativePath $CorrespondingSourceDirectory)",
            ''
        ) + $present + @(
            "    $manifestName" + (' ' * 33) + 'which binary they are the source of, by commit id'
        ))
}

function Get-CodecPatentAnswer {
    <#
    .SYNOPSIS
        The six fields of ADR 0008's "The answers", and what is written in each.
    .DESCRIPTION
        Parsed out of the record rather than out of a file of its own, because
        the answers belong with the questions: a maintainer writing them down is
        editing the document they were reading, and a contributor reading the
        record finds them without knowing this gate exists. A separate
        machine-readable file would be a second place to forget.

        Read line by line and ended at the next heading of the same level or
        above, so that prose can be added around the answers without the parse
        drifting onto it. Windows PowerShell 5.1 ships no Markdown parser and
        this needs none: the headings are the contract, and the section says so
        in its own text rather than leaving a reader to infer it from here.

        Returns $null when the section is not there at all, which is a different
        failure from an unanswered one and gets a different refusal.
    #>
    param([Parameter(Mandatory)] [string] $Path)

    # UTF-8 named, because Windows PowerShell 5.1 otherwise reads a file with no
    # byte-order mark in the system ANSI codepage. The parse survives that - the
    # headings and the placeholder are ASCII - but the refusal prints the record
    # back, and a message that renders an em dash or a name with an accent as
    # mojibake is one somebody stops reading.
    $lines = @(Get-Content -LiteralPath $Path -Encoding UTF8)

    $start = -1
    for ($index = 0; $index -lt $lines.Count; $index++) {
        if ($lines[$index] -match '^###\s+The answers\s*$') {
            $start = $index
            break
        }
    }
    if ($start -lt 0) { return $null }

    $fields = @()
    $current = $null
    for ($index = $start + 1; $index -lt $lines.Count; $index++) {
        $line = $lines[$index]

        # A heading at this level or above is the end of the section.
        if ($line -match '^#{1,3}\s') { break }

        if ($line -match '^####\s+(?<heading>Answer\s+(?<number>\d+)\b.*)$') {
            $current = [pscustomobject]@{
                Field   = "Answer $($Matches['number'])"
                Heading = $Matches['heading'].Trim()
                Body    = @()
            }
            $fields += $current
            continue
        }

        # The two attribution fields sit above the first answer heading, so they
        # are only read while no answer is open. Inside one they are prose.
        if ($null -eq $current -and $line -match '^-\s+(?<name>Answered by|Date)\s*:(?<value>.*)$') {
            $describes = 'who answered the four questions'
            if ($Matches['name'] -eq 'Date') { $describes = 'when they answered them' }
            $fields += [pscustomobject]@{
                Field   = $Matches['name']
                Heading = $describes
                Body    = @($Matches['value'])
            }
            continue
        }

        if ($null -ne $current) { $current.Body += $line }
    }

    return @($fields)
}

function Get-AnswerState {
    <#
    .SYNOPSIS
        Whether one field of that section holds an answer.
    .DESCRIPTION
        Three ways of not holding one, and they are worth telling apart in the
        refusal because they are three different things somebody did:

        - the placeholder is still there, meaning nobody has been asked;
        - it is blank or whitespace, which is what deleting the placeholder and
          writing nothing leaves, and which must not read as answered - an empty
          answer is the exact shape of this gate being defeated by accident;
        - it is too short to be an answer. ADR 0008 asks for the sentence rather
          than the word, and the floor is low enough that no real answer is
          under it and high enough that a character typed to get past this is.
          If somebody wants past it badly enough to write two dozen characters
          of nonsense, they have deleted the gate, which no gate can prevent.
    #>
    param(
        [Parameter(Mandatory)] [AllowEmptyString()] [string] $Value,
        [Parameter(Mandatory)] [int] $Minimum
    )

    $text = ($Value -replace '\s+', ' ').Trim()
    if (-not $text) { return 'blank' }
    if ($text.Contains($unansweredPlaceholder)) { return 'unanswered' }
    if ($text.Length -lt $Minimum) { return 'too short to be an answer' }
    return 'answered'
}

function Test-CodecPatentGate {
    <#
    .SYNOPSIS
        The four codec patent questions have been asked and the answers written
        into ADR 0008.
    .DESCRIPTION
        ADR 0008's sixth decision blocks the first signed public release on
        those questions being put to somebody qualified - not on a particular
        answer, on somebody having answered. Until this gate existed that block
        was a sentence in a document, which is the defect this repository keeps
        finding: something decided, believed, and wired to nothing.

        What can and cannot be checked here is worth being plain about. No
        script can establish that a lawyer was consulted, or judge what they
        said. What it can establish is that the answers have been *recorded*,
        in the record that asked the questions, where a contributor and anybody
        reading the repository can see them. That is the whole of it, and it is
        deliberately not a check that shipping is safe - ADR 0008 says nothing
        available to this project can establish that.

        Cheap on purpose: one Markdown file read from the checkout. The gate job
        compiles nothing and this adds nothing to it.

        It does not retire after the first release, the way the milestone gate
        does. Answers once written stay written, so it costs a passing release
        nothing; what it goes on catching is the section being deleted or
        emptied later.
    #>

    # Small enough that no answer a person would write is under it, large enough
    # that a full stop typed into the field is not one. Named here rather than
    # inlined because ADR 0008's section states the same rule in words, and the
    # two have to agree.
    $minimumAnswer = 24

    $relative = 'docs/adr/0008-codec-patent-position.md'
    $record = Join-Path $RepositoryRoot 'docs\adr\0008-codec-patent-position.md'

    $why = @(
        '',
        'ADR 0008 is this project''s codec patent position, and its sixth decision blocks',
        'the first signed public release on four questions being put to somebody',
        'qualified. Not on a particular answer - on somebody having answered. The',
        'installer ships an FFmpeg whose libraries encode H.264 through a statically',
        'linked libopenh264, and the default codec resolves to HEVC on every machine',
        'whose GPU has no AV1 encoder. Both are patent-pool standards, and no copyright',
        'licence in the payload grants anything over them; a release is the moment a',
        'pool''s claim would attach, and it is the one act here that cannot be undone.',
        '',
        'This gate is deliberate, and what discharges it is deliberate too: put the four',
        'questions under "The four questions for a lawyer" to a lawyer, write what they',
        'said into "The answers" in that record, and commit it. Nothing here asks for a',
        'particular answer, and passing this gate is not anybody saying the release is',
        'safe - that record is explicit that nothing available to this project can',
        'establish that. It is somebody qualified having looked, before the binaries are',
        'on other people''s machines rather than after.',
        '',
        "The record is $relative; docs/releasing.md is what a release is allowed to do."
    )

    if (-not (Test-Path -LiteralPath $record -PathType Leaf)) {
        return New-GateResult -Name 'Codec patents' -Passed $false -Lines (@(
                "$relative is not in this checkout, so whether the codec patent questions",
                'have been answered cannot be established. A gate that cannot see its evidence',
                'refuses; deleting the record is not how the block it carries is lifted.'
            ) + $why)
    }

    $fields = Get-CodecPatentAnswer -Path $record
    if ($null -eq $fields) {
        return New-GateResult -Name 'Codec patents' -Passed $false -Lines (@(
                "$relative has no `"### The answers`" section.",
                '',
                'That section is what this gate reads, and it was in the record when the gate',
                'was written. Either it has been removed - in which case put it back rather',
                'than releasing without it - or its heading has been renamed, in which case',
                'this gate and the record have to be changed together.'
            ) + $why)
    }

    $expected = @('Answered by', 'Date', 'Answer 1', 'Answer 2', 'Answer 3', 'Answer 4')
    $problems = @()
    $answered = @()

    foreach ($name in $expected) {
        $field = @($fields | Where-Object { $_.Field -eq $name })
        if ($field.Count -eq 0) {
            $problems += ("    {0,-14} the record has no such field" -f $name)
            continue
        }

        $minimum = $minimumAnswer
        if ($name -eq 'Answered by' -or $name -eq 'Date') { $minimum = 1 }

        $value = ($field[0].Body -join ' ')
        $state = Get-AnswerState -Value $value -Minimum $minimum
        if ($state -eq 'answered') {
            $answered += $field[0]
            continue
        }
        $problems += ("    {0,-14} {1,-26} {2}" -f $name, $state, $field[0].Heading)
    }

    if ($problems.Count -gt 0) {
        $lines = @(
            "The four codec patent questions in $relative have not been answered:",
            ''
        ) + $problems
        if ($answered.Count -gt 0) {
            $lines += ''
            $lines += "$($answered.Count) of the $($expected.Count) fields do hold something. Partly answered is not answered:"
            $lines += 'the release is blocked on all four questions, because the one left blank is'
            $lines += 'the one nobody wanted to ask.'
        }
        return New-GateResult -Name 'Codec patents' -Passed $false -Lines ($lines + $why)
    }

    $by = @($fields | Where-Object { $_.Field -eq 'Answered by' })[0].Body -join ' '
    $on = @($fields | Where-Object { $_.Field -eq 'Date' })[0].Body -join ' '

    return New-GateResult -Name 'Codec patents' -Passed $true -Lines @(
        "All four questions in $relative are answered.",
        '',
        "    answered by   $($by.Trim())",
        "    dated         $($on.Trim())",
        '',
        'This says the questions were asked and the answers recorded, which is what ADR',
        '0008 decision 6 blocks a release on. It is not this gate saying the release is',
        'safe, and it never was: read the answers.'
    )
}

$gates = @()
$failure = $null

try {
    $gates += Test-VersionGate
    $gates += Test-BranchGate
    $gates += Test-ContinuousIntegrationGate
    $gates += Test-MilestoneGate
    $gates += Test-LicenceGate
    $gates += Test-CorrespondingSourceGate
    $gates += Test-CodecPatentGate
} catch {
    $failure = $_
}

Write-Host ''
if ($Rehearse) {
    Write-Host 'REHEARSAL. Reporting on every gate; this run cannot produce a release.' -ForegroundColor Yellow
    Write-Host ''
}
Write-Host "Release gates for $Tag ($CommitSha)"
Write-Host ''

foreach ($gate in $gates) {
    $verdict = 'REFUSED'
    $colour = 'Red'
    if ($gate.Passed) {
        $verdict = 'passed '
        $colour = 'Green'
    }
    Write-Host "  [$verdict] $($gate.Name)" -ForegroundColor $colour
    foreach ($line in $gate.Lines) {
        Write-Host "            $line"
    }
    Write-Host ''
}

if ($failure) {
    Write-Host 'A gate could not be evaluated, which is a refusal:' -ForegroundColor Red
    Write-Host "  $failure"
    Write-Host ''
    if ($Rehearse) { exit 0 }
    exit 1
}

$refused = @($gates | Where-Object { -not $_.Passed })
if ($refused.Count -eq 0) {
    Write-Host "All $($gates.Count) gates pass. $Tag may be built and drafted." -ForegroundColor Green
    Write-Host ''
    exit 0
}

Write-Host "$($refused.Count) of $($gates.Count) gates refuse $Tag`: $(($refused | ForEach-Object { $_.Name }) -join ', ')." -ForegroundColor Red
Write-Host 'No installer was built and no release was created.'
Write-Host ''
if ($Rehearse) { exit 0 }
exit 1
