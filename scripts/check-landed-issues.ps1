<#
.SYNOPSIS
    Lists issues whose work has landed on `main` and which are still open.

.DESCRIPTION
    The pull request title convention cannot close an issue
    ([#511](https://github.com/wildware-uk/clipped/issues/511)): closing keywords
    are forbidden in a title or body, deliberately, so closing is left to
    whoever remembers. This makes the drift visible.

    It is not untidiness. `check-release-gates.ps1` refuses a tag while any
    milestone has an open issue, so an issue left open after its work lands
    blocks a release with bookkeeping rather than with work.

    **These are candidates, not conclusions.** A commit citing an issue does not
    mean the issue is finished: several in this repository are trackers for a
    whole screen, or for work with a documented second half. This script says
    "somebody should look at these", and the looking is reading the acceptance
    criteria against the source. Run on 2026-08-20 it produced twenty
    candidates, of which four were genuinely complete (#101, #136, #166, #588)
    and the rest were not.

    That ratio is why this refuses to close anything itself, and why it exits 0
    when it finds candidates. It is a report, not a gate. A gate here would
    either close live work or be turned off.

.PARAMETER Root
    The repository. Defaults to the parent of this script's directory.

.PARAMETER OpenIssuesJson
    Path to the output of
    `gh issue list --state open --limit 500 --json number,title`.

    A file rather than a call, for the reason `check-release-gates.ps1` takes
    its inputs as files: the script is then runnable offline and testable
    against fixtures.

.PARAMETER Ref
    Which history to read. Defaults to `origin/main`.

.OUTPUTS
    Exit code 0 when it ran, 1 when it could not read what it needs. Prints one
    line per candidate with the commit that landed against it.

.EXAMPLE
    gh issue list --state open --limit 500 --json number,title > open.json
    powershell -File scripts/check-landed-issues.ps1 -OpenIssuesJson open.json
#>
[CmdletBinding()]
param(
    [string] $Root,
    [Parameter(Mandatory)] [string] $OpenIssuesJson,
    [string] $Ref = 'origin/main'
)

$ErrorActionPreference = 'Stop'

if (-not $Root) { $Root = Split-Path -Parent $PSScriptRoot }

if (-not (Test-Path -LiteralPath $OpenIssuesJson -PathType Leaf)) {
    Write-Host "The open-issue list was not found at $OpenIssuesJson." -ForegroundColor Red
    Write-Host 'Produce it with: gh issue list --state open --limit 500 --json number,title'
    exit 1
}

$open = @{}
foreach ($issue in (Get-Content -LiteralPath $OpenIssuesJson -Raw -Encoding utf8 | ConvertFrom-Json)) {
    $open[[int]$issue.number] = [string]$issue.title
}

# Empty is not an answer. Every candidate below is found by intersecting with
# this set, so an empty one reports nothing and looks like a clean repository.
if ($open.Count -eq 0) {
    Write-Host 'The open-issue list is empty.' -ForegroundColor Red
    Write-Host 'That is not "nothing is open" - it is a query that returned nothing, and this'
    Write-Host 'script would then report no candidates however many there are.'
    exit 1
}

$titles = & git -C $Root log $Ref --format=%s 2>$null
if ($LASTEXITCODE -ne 0 -or -not $titles) {
    Write-Host "Could not read the history of $Ref in $Root." -ForegroundColor Red
    Write-Host 'Fetch it first, or name another ref with -Ref.'
    exit 1
}

# `Title (#issue) (#pr)`. A squash merge appends the pull request, so the last
# number is always that and anything before it is an issue the work was for. A
# title with one number names no issue - it is the pull request alone.
$landed = @{}
foreach ($title in $titles) {
    $numbers = [regex]::Matches($title, '\(#(\d+)\)') | ForEach-Object { [int]$_.Groups[1].Value }
    if (@($numbers).Count -lt 2) { continue }
    foreach ($issue in @($numbers)[0..(@($numbers).Count - 2)]) {
        if (-not $landed.ContainsKey($issue)) { $landed[$issue] = $title }
    }
}

if ($landed.Count -eq 0) {
    Write-Host "No commit on $Ref names an issue." -ForegroundColor Red
    Write-Host 'The convention is `Title (#issue) (#pr)`, so finding none means this script no'
    Write-Host 'longer reads what it was written to read.'
    exit 1
}

$candidates = @($landed.Keys | Where-Object { $open.ContainsKey($_) } | Sort-Object)

Write-Host ("Read $($titles.Count) commits on $Ref naming $($landed.Count) distinct issues.")
Write-Host ("$($open.Count) issues are open.")
Write-Host ''

if ($candidates.Count -eq 0) {
    Write-Host 'No open issue has work landed against it.'
    exit 0
}

Write-Host "$($candidates.Count) open issue(s) have work landed against them:" -ForegroundColor Yellow
foreach ($issue in $candidates) {
    Write-Host ''
    Write-Host ("  #{0}  {1}" -f $issue, $open[$issue])
    Write-Host ("        landed: {0}" -f $landed[$issue])
}

Write-Host ''
Write-Host 'These are candidates, not conclusions. A commit citing an issue does not mean the'
Write-Host 'issue is finished - a tracker for a whole screen, or work with a second half, looks'
Write-Host 'exactly the same from here. Read the acceptance criteria against the source before'
Write-Host 'closing any of them.'
exit 0
