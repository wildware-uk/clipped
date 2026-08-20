<#
.SYNOPSIS
    Every "what has to exist first" row names at least one issue that is open.

.DESCRIPTION
    Three screens and the playback module draw a table of what they cannot show
    yet, and each row names the work that would land it. The rows are read by
    users, in the product, under a heading that says these things are coming.

    A row whose issues are *all* closed is telling somebody that finished work
    is tracked when nothing tracks it. That is not a documentation nicety: the
    Library screen asked for a waveform under each sound track and named issue
    #66, which closed months earlier having done a different thing (generating
    peaks, which works). Nobody was going to build the row's actual subject,
    and the screen said otherwise to every user who read it.

    A row may cite closed issues freely - most do, as satisfied dependencies:
    "marking works on the Library screen (#58)" is a true and useful sentence
    about work that is done. What this refuses is a row where *nothing* cited is
    still open, because then there is no work item behind the promise.

    That distinction is why this is not simply "no closed issues in a row". A
    check that flagged every closed citation would flag the majority of rows and
    be turned off within a week.

.PARAMETER Root
    The repository root. Defaults to the parent of this script's directory.

.PARAMETER ClosedIssuesJson
    Path to the output of
    `gh issue list --state closed --limit 1000 --json number`.

    A file rather than a call, for the reason check-release-gates.ps1 takes its
    inputs as files: the script is then runnable offline, testable against
    fixtures, and does not need a token to be exercised.

.OUTPUTS
    Exit code 0 when every row names an open issue, 1 when any does not. Prints
    the file, the row and what it cites.

.EXAMPLE
    gh issue list --state closed --limit 1000 --json number > closed.json
    powershell -File scripts/check-waiting-rows.ps1 -ClosedIssuesJson closed.json
#>
[CmdletBinding()]
param(
    [string] $Root,
    [Parameter(Mandatory)] [string] $ClosedIssuesJson
)

$ErrorActionPreference = 'Stop'

# In the body rather than as a parameter default: `$PSScriptRoot` is not bound
# while defaults are evaluated, which is why every other script here does the
# same (scripts/stage-installer-payload.ps1).
if (-not $Root) { $Root = Split-Path -Parent $PSScriptRoot }

if (-not (Test-Path -LiteralPath $ClosedIssuesJson -PathType Leaf)) {
    Write-Host "The closed-issue list was not found at $ClosedIssuesJson." -ForegroundColor Red
    Write-Host 'Produce it with: gh issue list --state closed --limit 1000 --json number'
    exit 1
}

$closed = [System.Collections.Generic.HashSet[int]]::new()
foreach ($issue in (Get-Content -LiteralPath $ClosedIssuesJson -Raw -Encoding utf8 | ConvertFrom-Json)) {
    [void]$closed.Add([int]$issue.number)
}

# Empty is not an answer. A query that returned nothing would make every row
# below look fine, which is the failure this whole script exists to prevent.
if ($closed.Count -eq 0) {
    Write-Host 'The closed-issue list is empty.' -ForegroundColor Red
    Write-Host 'That is not "nothing is closed" - it is a query that returned nothing, and every'
    Write-Host 'row would pass against it.'
    exit 1
}

$sourceRoot = Join-Path $Root 'apps\desktop\src'
$sources = @(Get-ChildItem -LiteralPath $sourceRoot -Recurse -File -Include '*.ts', '*.tsx' |
        Where-Object { $_.Name -notlike '*.test.*' })

if ($sources.Count -lt 20) {
    Write-Host "Only $($sources.Count) source files were found under $sourceRoot." -ForegroundColor Red
    Write-Host 'This check read almost nothing, which is not the same as finding nothing wrong.'
    exit 1
}

# `shows:` on the screens, `does:` in the editor. Both are followed by `needs:`,
# and it is the `needs:` half that names the work.
$pattern = '(?s)\{\s*(?:shows|does):\s*(.*?)needs:\s*(.*?)\n\s*\},'

$rows = @()
foreach ($source in $sources) {
    # UTF-8 explicitly: the rows are full of curly apostrophes and Get-Content
    # defaults to the system codepage, which turns them into mojibake in the
    # refusal a person has to read.
    $text = Get-Content -LiteralPath $source.FullName -Raw -Encoding utf8
    foreach ($match in [regex]::Matches($text, $pattern)) {
        $subject = ($match.Groups[1].Value -replace '\s+', ' ').Trim().Trim("',")
        $cited = @([regex]::Matches($match.Groups[2].Value, '#(\d+)') |
                ForEach-Object { [int]$_.Groups[1].Value } | Sort-Object -Unique)
        $rows += [pscustomobject]@{
            File    = $source.Name
            Subject = $subject
            Cited   = $cited
        }
    }
}

if ($rows.Count -eq 0) {
    Write-Host 'No waiting-on rows were found at all.' -ForegroundColor Red
    Write-Host 'They exist on the Home, Library, playback and editor surfaces, so finding none'
    Write-Host 'means this check no longer reads what it was written to read.'
    exit 1
}

$stale = @($rows | Where-Object {
        $_.Cited.Count -gt 0 -and -not ($_.Cited | Where-Object { -not $closed.Contains($_) })
    })

Write-Host "Read $($rows.Count) waiting-on rows across $($sources.Count) files."

if ($stale.Count -eq 0) {
    Write-Host 'Every row names at least one open issue.'
    exit 0
}

Write-Host ''
Write-Host "$($stale.Count) row(s) name only closed issues:" -ForegroundColor Red
foreach ($row in $stale) {
    Write-Host ''
    Write-Host "  $($row.File): $($row.Subject)"
    Write-Host "    cites $(($row.Cited | ForEach-Object { "#$_" }) -join ', ') - all closed"
}
Write-Host ''
Write-Host 'These rows are drawn to a user under "what has to exist first". Naming only closed'
Write-Host 'issues tells them the work is tracked when nothing tracks it.'
Write-Host ''
Write-Host 'Either the work is done, in which case remove the row and whatever sentence'
Write-Host 'introduces it; or it is not, in which case an open issue has to own it. Citing a'
Write-Host 'closed issue as a satisfied dependency is fine - just not as the only citation.'
exit 1
