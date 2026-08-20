<#
.SYNOPSIS
    Cases for scripts/check-landed-issues.ps1.

.DESCRIPTION
    Each case runs the real script as a child process against a fixture: a git
    repository whose commit titles follow the convention, and JSON standing in
    for what GitHub says is open. Exit code and output are both asserted.

    The properties worth guarding are the ones that would make this report
    quietly useless rather than loudly broken:

    - **It refuses an empty open-issue list.** Every candidate is found by
      intersecting with that set, so an empty one reports nothing and reads
      exactly like a repository with no drift in it.
    - **It refuses a history that names no issues.** The convention is
      `Title (#issue) (#pr)`; if it changes, this script silently finds nothing
      rather than saying it can no longer read what it was written for.
    - **It does not treat a pull request number as an issue.** A squash merge
      appends `(#pr)`, so a title with one number names no issue at all, and
      counting it would report every merged pull request as a candidate.

    Written as a plain script rather than as Pester tests for the reason
    scripts/test-check-release-gates.ps1 is: the only Pester on a stock Windows
    install is 3.4.0, whose syntax is incompatible with the Pester 5 a
    contributor would install.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts/test-check-landed-issues.ps1
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

$script = Join-Path $PSScriptRoot 'check-landed-issues.ps1'
$fixtureRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("clipped-landed-" + $PID)
if (Test-Path -LiteralPath $fixtureRoot) { Remove-Item -LiteralPath $fixtureRoot -Recurse -Force }
New-Item -ItemType Directory -Path $fixtureRoot -Force | Out-Null

$failures = 0

function New-HistoryFixture {
    <#
    .SYNOPSIS
        A repository whose commit titles are the ones given.
    #>
    param(
        [Parameter(Mandatory)] [string] $Name,
        [Parameter(Mandatory)] [string[]] $Titles
    )

    $root = Join-Path $fixtureRoot $Name
    New-Item -ItemType Directory -Path $root -Force | Out-Null
    & git -C $root init --quiet
    & git -C $root config user.email 'fixture@example.invalid'
    & git -C $root config user.name 'Fixture'
    foreach ($title in $Titles) {
        & git -C $root commit --quiet --allow-empty -m $title
    }
    # The script reads a ref; a fixture has no remote, so its cases name HEAD.
    return $root
}

function New-OpenIssuesFile {
    param(
        [Parameter(Mandatory)] [string] $Name,
        [Parameter(Mandatory)] [AllowEmptyCollection()] [array] $Issues
    )

    $path = Join-Path $fixtureRoot "$Name.json"
    # `ConvertTo-Json` on one element produces an object rather than an array,
    # which the script would then fail to enumerate - so the shape is forced.
    $json = if ($Issues.Count -eq 0) { '[]' } else { ConvertTo-Json -InputObject $Issues -Depth 4 }
    Set-Content -LiteralPath $path -Value $json -Encoding utf8
    return $path
}

function Invoke-Check {
    param(
        [Parameter(Mandatory)] [string] $Root,
        [Parameter(Mandatory)] [string] $OpenIssues
    )

    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $output = & powershell -ExecutionPolicy Bypass -File $script `
            -Root $Root -OpenIssuesJson $OpenIssues -Ref 'HEAD' 2>&1 | Out-String
        $code = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previous
    }
    return [pscustomobject]@{ Output = $output; Code = $code }
}

function Assert-Case {
    param(
        [Parameter(Mandatory)] [string] $Name,
        [Parameter(Mandatory)] $Result,
        [Parameter(Mandatory)] [int] $ExpectedExitCode,
        [string[]] $Contains = @(),
        [string[]] $Excludes = @()
    )

    $problems = @()
    if ($Result.Code -ne $ExpectedExitCode) {
        $problems += "exit code $($Result.Code), expected $ExpectedExitCode"
    }
    foreach ($needle in $Contains) {
        if ($Result.Output -notlike "*$needle*") { $problems += "did not say '$needle'" }
    }
    foreach ($needle in $Excludes) {
        if ($Result.Output -like "*$needle*") { $problems += "said '$needle' and should not have" }
    }

    if ($problems.Count -eq 0) {
        Write-Host "  PASS  $Name"
    } else {
        Write-Host "  FAIL  $Name" -ForegroundColor Red
        foreach ($problem in $problems) { Write-Host "        $problem" -ForegroundColor Red }
        Write-Host $Result.Output
        $script:failures++
    }
}

Write-Host 'What the report finds'

$history = New-HistoryFixture -Name 'ordinary' -Titles @(
    'Do a thing (#101) (#315)',
    'Do another thing (#202) (#316)',
    'A pull request with no issue behind it (#317)'
)
$open = New-OpenIssuesFile -Name 'ordinary' -Issues @(
    @{ number = 101; title = 'A thing that landed' },
    @{ number = 999; title = 'A thing nothing has touched' }
)
Assert-Case `
    -Name 'an open issue with work landed against it is a candidate, named with its commit' `
    -Result (Invoke-Check -Root $history -OpenIssues $open) `
    -ExpectedExitCode 0 `
    -Contains @('#101', 'A thing that landed', 'Do a thing (#101) (#315)') `
    -Excludes @('#999')

# The other direction, and what stops the case above passing against a script
# that listed every open issue.
$quiet = New-OpenIssuesFile -Name 'quiet' -Issues @(
    @{ number = 999; title = 'A thing nothing has touched' }
)
Assert-Case `
    -Name 'an open issue nothing landed against is not a candidate' `
    -Result (Invoke-Check -Root $history -OpenIssues $quiet) `
    -ExpectedExitCode 0 `
    -Contains @('No open issue has work landed against it') `
    -Excludes @('#999')

# A squash merge appends the pull request, so a title with one number names no
# issue. Counting it would make every merged pull request a candidate.
$prOnly = New-OpenIssuesFile -Name 'pr-only' -Issues @(
    @{ number = 317; title = 'A number that is a pull request' }
)
Assert-Case `
    -Name 'a lone number is read as a pull request and not as an issue' `
    -Result (Invoke-Check -Root $history -OpenIssues $prOnly) `
    -ExpectedExitCode 0 `
    -Contains @('No open issue has work landed against it') `
    -Excludes @('#317  A number')

Write-Host 'What it refuses rather than reporting nothing'

Assert-Case `
    -Name 'an empty open-issue list is refused, not reported as nothing to do' `
    -Result (Invoke-Check -Root $history -OpenIssues (New-OpenIssuesFile -Name 'empty' -Issues @())) `
    -ExpectedExitCode 1 `
    -Contains @('The open-issue list is empty')

$noIssues = New-HistoryFixture -Name 'no-issues' -Titles @(
    'A pull request with no issue behind it (#317)',
    'Another one (#318)'
)
# Matched without the word before it: the console wraps the refusal, and a needle
# spanning that break never matches however right the message is.
Assert-Case `
    -Name 'a history naming no issue at all is refused, because the convention may have changed' `
    -Result (Invoke-Check -Root $noIssues -OpenIssues $open) `
    -ExpectedExitCode 1 `
    -Contains @('longer reads what it was written to read')

Assert-Case `
    -Name 'a missing open-issue file is refused, naming how to produce one' `
    -Result (Invoke-Check -Root $history -OpenIssues (Join-Path $fixtureRoot 'nowhere.json')) `
    -ExpectedExitCode 1 `
    -Contains @('gh issue list --state open')

Write-Host ''
if ($failures -eq 0) {
    Write-Host 'All cases passed.'
    Remove-Item -LiteralPath $fixtureRoot -Recurse -Force -ErrorAction SilentlyContinue
    exit 0
}

Write-Host "$failures case(s) failed." -ForegroundColor Red
exit 1
