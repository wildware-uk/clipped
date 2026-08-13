#Requires -Version 5.1

<#
.SYNOPSIS
    Tests scripts/check-release-gates.ps1: that each gate refuses what it exists
    to refuse, that it names what is wrong, and that it does not fix anything
    itself.

.DESCRIPTION
    Every gate this exercises guards something that cannot be undone once it has
    gone wrong. A release built from a mismatched tag ships a version number a
    bug report will quote forever; a release published before the licence texts
    ship is a licence breached against everybody who downloaded it. There is no
    safe way to find out in production whether these work, and no way to find
    out in staging either, because the failure mode of a broken gate is that a
    release succeeds.

    So each case here runs the real script as a child process against a fixture
    tree - a checkout with manifests and a git history in it, and JSON standing
    in for the three answers GitHub gives - and asserts on its exit code and on
    what it said. Two properties beyond "it refused" are asserted deliberately:

    - **It names the file.** A refusal that says "the version does not match" is
      a refusal somebody has to go and investigate; the point of the gate is
      that it hands over the answer.
    - **It changes nothing.** The version cases assert the manifests are
      byte-identical afterwards, because a workflow that helpfully rewrote them
      to agree with the tag would make the tag evidence of nothing at all.

    The case that a gate *passes* when it should matters just as much: a script
    that refuses everything is not a gate, it is an outage, and it would be
    discovered on the one day the project wanted to release.

    Written as a plain script rather than as Pester tests for the same reason
    scripts/test-stage-installer-payload.ps1 is: the only Pester on a stock
    Windows install is 3.4.0, whose syntax is incompatible with the Pester 5 a
    contributor would install.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts/test-check-release-gates.ps1

.OUTPUTS
    Exit code 0 when every case passes, 1 otherwise.
#>

[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$gateScript = Join-Path $PSScriptRoot 'check-release-gates.ps1'
$fixtureRoot = Join-Path ([System.IO.Path]::GetTempPath()) "clipped-release-gate-fixtures-$PID"
$failureCount = 0

# The six texts docs/licensing.md requires an installed build to carry, and the
# names scripts/collect-notices.ps1 writes them under.
$licencePayload = @(
    'LICENSE.txt',
    'THIRD-PARTY-NOTICES.md',
    'THIRD-PARTY-NOTICES-RUST.md',
    'ffmpeg\NOTICE.md',
    'ffmpeg\LGPL-3.0.txt',
    'ffmpeg\GPL-3.0.txt'
)

function Write-Fixture {
    param(
        [Parameter(Mandatory)] [string] $Path,
        [Parameter(Mandatory)] [AllowEmptyString()] [string] $Content
    )

    $directory = Split-Path -Parent $Path
    if (-not (Test-Path -LiteralPath $directory -PathType Container)) {
        New-Item -ItemType Directory -Path $directory -Force | Out-Null
    }
    # WriteAllText rather than Set-Content, so that a fixture asking for an
    # empty file gets a zero-byte one - which is what a failed `gh api`
    # redirected into a file actually leaves, and the case that has to refuse.
    [System.IO.File]::WriteAllText($Path, $Content)
}

function New-Fixture {
    <#
    .SYNOPSIS
        A checkout to run the gates against.
    .DESCRIPTION
        Small, but the same shapes the real repository has: a Cargo workspace
        whose members inherit the version and whose path dependencies restate
        it, two npm packages, and a Tauri configuration whose bundle.resources
        names a payload directory. A fixture that had only one version
        declaration in it would not exercise the case that matters, which is
        one file out of several disagreeing.

        The git history is real, because the branch gate asks git a question
        about ancestry that only git can answer. `main` and `side` diverge, so
        both verdicts can be produced.
    #>
    param(
        [Parameter(Mandatory)] [string] $Name,
        [string] $Version = '1.0.0',
        [hashtable] $Disagreeing = @{},
        [switch] $WithLicences
    )

    $root = Join-Path $fixtureRoot $Name
    New-Item -ItemType Directory -Path $root -Force | Out-Null

    function Resolve-FixtureVersion {
        param([string] $File)
        if ($Disagreeing.ContainsKey($File)) { return $Disagreeing[$File] }
        return $Version
    }

    Write-Fixture -Path (Join-Path $root 'Cargo.toml') -Content @"
[workspace]
resolver = "2"
members = ["crates/muxer"]

[workspace.package]
version = "$(Resolve-FixtureVersion 'Cargo.toml')"
edition = "2021"

[workspace.dependencies]
clipped-muxer = { path = "crates/muxer", version = "$(Resolve-FixtureVersion 'Cargo.toml.dependency')" }
serde = { version = "1", features = ["derive"] }
"@

    Write-Fixture -Path (Join-Path $root 'crates\muxer\Cargo.toml') -Content @"
[package]
name = "clipped-muxer"
version.workspace = true
edition.workspace = true

[dependencies]
serde = { workspace = true }
"@

    Write-Fixture -Path (Join-Path $root 'package.json') -Content @"
{
  "name": "clipped",
  "version": "$(Resolve-FixtureVersion 'package.json')",
  "private": true
}
"@

    Write-Fixture -Path (Join-Path $root 'apps\desktop\package.json') -Content @"
{
  "name": "@clipped/desktop",
  "version": "$(Resolve-FixtureVersion 'apps/desktop/package.json')",
  "private": true
}
"@

    # Somebody else's manifest under our tree. Its version is not ours to
    # require agreement from, and a gate that failed on it would be a gate
    # nobody could get past.
    Write-Fixture -Path (Join-Path $root 'test-apps\fixture\package.json') -Content @"
{
  "name": "not-ours",
  "version": "9.9.9"
}
"@

    Write-Fixture -Path (Join-Path $root 'apps\desktop\src-tauri\Cargo.toml') -Content @"
[package]
name = "clipped-desktop"
version = "$(Resolve-FixtureVersion 'apps/desktop/src-tauri/Cargo.toml')"
edition = "2021"

[dependencies]
clipped-ipc = { path = "../../../crates/ipc", version = "$(Resolve-FixtureVersion 'apps/desktop/src-tauri/Cargo.toml.dependency')" }
"@

    Write-Fixture -Path (Join-Path $root 'apps\desktop\src-tauri\tauri.conf.json') -Content @"
{
  "productName": "Clipped",
  "version": "$(Resolve-FixtureVersion 'apps/desktop/src-tauri/tauri.conf.json')",
  "bundle": {
    "resources": {
      "installer-payload/": ""
    }
  }
}
"@

    # The payload the installer would collect. A recorder and a DLL always, so
    # that "carries files, but not the licence texts" is distinguishable from
    # "carries nothing"; the licence texts only when the case says so.
    $payload = Join-Path $root 'apps\desktop\src-tauri\installer-payload'
    Write-Fixture -Path (Join-Path $payload 'clipped-recorder.exe') -Content 'a recorder'
    Write-Fixture -Path (Join-Path $payload 'avcodec-62.dll') -Content 'a library'
    if ($WithLicences) {
        foreach ($file in $licencePayload) {
            Write-Fixture -Path (Join-Path $payload $file) -Content "the text of $file"
        }
    }

    Push-Location $root
    try {
        $previous = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        try {
            & git init -q 2>&1 | Out-Null
            & git add -A 2>&1 | Out-Null
            & git -c user.name=Fixture -c user.email=fixture@example.invalid commit -q -m 'the tree' 2>&1 | Out-Null
            & git branch -M main 2>&1 | Out-Null
            $onMain = (& git rev-parse HEAD).Trim()

            & git checkout -q -b side 2>&1 | Out-Null
            Set-Content -LiteralPath (Join-Path $root 'unreviewed.txt') -Value 'work nobody merged' -Encoding Ascii
            & git add -A 2>&1 | Out-Null
            & git -c user.name=Fixture -c user.email=fixture@example.invalid commit -q -m 'on a branch' 2>&1 | Out-Null
            $onSide = (& git rev-parse HEAD).Trim()

            & git checkout -q main 2>&1 | Out-Null
        } finally {
            $ErrorActionPreference = $previous
        }
    } finally {
        Pop-Location
    }

    return [pscustomobject]@{
        Root          = $root
        MainCommit    = $onMain
        BranchCommit  = $onSide
        Payload       = $payload
        MilestonesAll = (New-Milestones -Path (Join-Path $root '.milestones-finished.json') -Finished)
        MilestonesOpen = (New-Milestones -Path (Join-Path $root '.milestones-open.json'))
        ReleasesNone  = (New-Json -Path (Join-Path $root '.releases-none.json') -Value '[]')
        CiPassed      = (New-CiRuns -Path (Join-Path $root '.ci-passed.json') -Conclusion 'success')
        CiFailed      = (New-CiRuns -Path (Join-Path $root '.ci-failed.json') -Conclusion 'failure')
        CiNone        = (New-Json -Path (Join-Path $root '.ci-none.json') -Value '{"total_count":0,"workflow_runs":[]}')
    }
}

function New-Json {
    param(
        [Parameter(Mandatory)] [string] $Path,
        [Parameter(Mandatory)] [AllowEmptyString()] [string] $Value
    )
    Write-Fixture -Path $Path -Content $Value
    return $Path
}

function New-Milestones {
    param(
        [Parameter(Mandatory)] [string] $Path,
        [switch] $Finished
    )

    if ($Finished) {
        return New-Json -Path $Path -Value @'
[
  { "title": "M0 - Project Foundations", "state": "closed", "open_issues": 0 },
  { "title": "M1 - Recording Engine", "state": "closed", "open_issues": 0 }
]
'@
    }

    return New-Json -Path $Path -Value @'
[
  { "title": "M0 - Project Foundations", "state": "closed", "open_issues": 0 },
  { "title": "M1 - Recording Engine", "state": "open", "open_issues": 14 },
  { "title": "M9 - Highlight Plugin API", "state": "closed", "open_issues": 3 }
]
'@
}

function New-CiRuns {
    param(
        [Parameter(Mandatory)] [string] $Path,
        [Parameter(Mandatory)] [string] $Conclusion
    )

    return New-Json -Path $Path -Value @"
{
  "total_count": 1,
  "workflow_runs": [
    {
      "run_number": 412,
      "status": "completed",
      "conclusion": "$Conclusion",
      "html_url": "https://github.com/wildware-uk/clipped/actions/runs/1"
    }
  ]
}
"@
}

function Invoke-Gates {
    <#
    .SYNOPSIS
        Runs the script under test as a child process, as CI does.
    #>
    param(
        [Parameter(Mandatory)] $Fixture,
        [Parameter(Mandatory)] [string] $Tag,
        [string] $CommitSha,
        [string] $MilestonesJson,
        [string] $ReleasesJson,
        [string] $CiRunsJson,
        [switch] $Rehearse
    )

    if (-not $CommitSha) { $CommitSha = $Fixture.MainCommit }
    if (-not $MilestonesJson) { $MilestonesJson = $Fixture.MilestonesAll }
    if (-not $ReleasesJson) { $ReleasesJson = $Fixture.ReleasesNone }
    if (-not $CiRunsJson) { $CiRunsJson = $Fixture.CiPassed }

    $arguments = @(
        '-ExecutionPolicy', 'Bypass', '-File', $gateScript,
        '-Tag', $Tag,
        '-CommitSha', $CommitSha,
        '-MilestonesJson', $MilestonesJson,
        '-ReleasesJson', $ReleasesJson,
        '-CiRunsJson', $CiRunsJson,
        '-RepositoryRoot', $Fixture.Root,
        '-MainRef', 'main'
    )
    if ($Rehearse) { $arguments += '-Rehearse' }

    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $output = & powershell @arguments 2>&1 | Out-String
        $code = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previous
    }

    return [pscustomobject]@{
        Output   = $output
        ExitCode = $code
    }
}

function Assert-Case {
    param(
        [Parameter(Mandatory)] [string] $Name,
        [Parameter(Mandatory)] $Result,
        [Parameter(Mandatory)] [int] $ExpectedExitCode,
        [string[]] $Contains = @(),
        [string[]] $DoesNotContain = @()
    )

    $problems = @()
    if ($Result.ExitCode -ne $ExpectedExitCode) {
        $problems += "exit code was $($Result.ExitCode), expected $ExpectedExitCode"
    }
    foreach ($expected in $Contains) {
        if ($Result.Output -notlike "*$expected*") {
            $problems += "output does not mention '$expected'"
        }
    }
    foreach ($unexpected in $DoesNotContain) {
        if ($Result.Output -like "*$unexpected*") {
            $problems += "output mentions '$unexpected' and should not"
        }
    }

    if ($problems.Count -eq 0) {
        Write-Host "  PASS  $Name"
        return
    }

    Write-Host "  FAIL  $Name" -ForegroundColor Red
    foreach ($problem in $problems) { Write-Host "        $problem" -ForegroundColor Red }
    Write-Host '        it printed:' -ForegroundColor Red
    foreach ($line in ($Result.Output -split "`r?`n")) { Write-Host "        | $line" -ForegroundColor DarkGray }
    $script:failureCount++
}

New-Item -ItemType Directory -Path $fixtureRoot -Force | Out-Null

try {
    Write-Host 'A tag that agrees with the tree, on main, green, with everything finished'

    $good = New-Fixture -Name 'ready' -WithLicences
    Assert-Case `
        -Name 'every gate passes, and the release is allowed' `
        -Result (Invoke-Gates -Fixture $good -Tag 'v1.0.0') `
        -ExpectedExitCode 0 `
        -Contains @('All 5 gates pass', 'v1.0.0 may be built and drafted') `
        -DoesNotContain @('REFUSED')

    Write-Host ''
    Write-Host 'The tag is the source of truth for the version'

    foreach ($case in @(
            @{ File = 'Cargo.toml'; Label = 'the workspace Cargo.toml' },
            @{ File = 'package.json'; Label = 'the root package.json' },
            @{ File = 'apps/desktop/package.json'; Label = "the desktop application's package.json" },
            @{ File = 'apps/desktop/src-tauri/tauri.conf.json'; Label = 'the Tauri configuration' },
            @{ File = 'apps/desktop/src-tauri/Cargo.toml'; Label = 'the desktop crate' }
        )) {
        $fixture = New-Fixture -Name ("disagrees-" + ($case.File -replace '[\\/.]', '-')) -WithLicences -Disagreeing @{ $case.File = '0.1.0' }
        Assert-Case `
            -Name "a tag that disagrees with $($case.Label) is refused, and that file is named" `
            -Result (Invoke-Gates -Fixture $fixture -Tag 'v1.0.0') `
            -ExpectedExitCode 1 `
            -Contains @('REFUSED', 'Version', ($case.File -replace '/', '\'), 'says 0.1.0')
    }

    # A path dependency's version requirement is not decoration: Cargo enforces
    # it, so a workspace bumped to 1.0.0 with these left behind does not build.
    # Left out of the gate, that becomes a cryptic resolver error twenty minutes
    # into a release build instead of a named file here.
    $staleDependency = New-Fixture -Name 'stale-path-dependency' -WithLicences -Disagreeing @{ 'Cargo.toml.dependency' = '0.1.0' }
    Assert-Case `
        -Name "a path dependency left at the old version is refused, and named as one" `
        -Result (Invoke-Gates -Fixture $staleDependency -Tag 'v1.0.0') `
        -ExpectedExitCode 1 `
        -Contains @('Cargo.toml', 'clipped-muxer (path dependency)', 'says 0.1.0')

    # The refusal has to leave the tree alone. A workflow that edited these to
    # agree with the tag would be choosing the version itself, and the tag would
    # stop being evidence of anything.
    $untouched = New-Fixture -Name 'not-rewritten' -WithLicences -Disagreeing @{ 'package.json' = '0.1.0' }
    $before = @{}
    foreach ($file in @('Cargo.toml', 'package.json', 'apps\desktop\package.json', 'apps\desktop\src-tauri\tauri.conf.json')) {
        $before[$file] = (Get-FileHash -LiteralPath (Join-Path $untouched.Root $file) -Algorithm SHA256).Hash
    }
    $rewriteResult = Invoke-Gates -Fixture $untouched -Tag 'v1.0.0'
    $rewritten = @()
    foreach ($file in $before.Keys) {
        $after = (Get-FileHash -LiteralPath (Join-Path $untouched.Root $file) -Algorithm SHA256).Hash
        if ($after -ne $before[$file]) { $rewritten += $file }
    }
    if ($rewritten.Count -eq 0) {
        Write-Host '  PASS  a refused release leaves every manifest byte-identical'
    } else {
        Write-Host "  FAIL  a refused release rewrote $($rewritten -join ', ')" -ForegroundColor Red
        $failureCount++
    }

    $wellFormed = New-Fixture -Name 'tag-shapes' -WithLicences
    Assert-Case `
        -Name 'a tag that is not a semantic version is refused rather than rounded off' `
        -Result (Invoke-Gates -Fixture $wellFormed -Tag 'v1.2') `
        -ExpectedExitCode 1 `
        -Contains @('is not a semantic version', 'MAJOR.MINOR.PATCH')

    Assert-Case `
        -Name 'a tag without the leading v is refused by name' `
        -Result (Invoke-Gates -Fixture $wellFormed -Tag '1.0.0') `
        -ExpectedExitCode 1 `
        -Contains @("A release tag is a")

    $prerelease = New-Fixture -Name 'prerelease' -Version '1.0.0-rc.1' -WithLicences
    Assert-Case `
        -Name 'a pre-release tag is a version like any other, and passes when the tree agrees' `
        -Result (Invoke-Gates -Fixture $prerelease -Tag 'v1.0.0-rc.1') `
        -ExpectedExitCode 0 `
        -Contains @('All 5 gates pass')

    Write-Host ''
    Write-Host 'A tag on a branch is not a release'

    Assert-Case `
        -Name 'a commit that is not an ancestor of main is refused' `
        -Result (Invoke-Gates -Fixture $good -Tag 'v1.0.0' -CommitSha $good.BranchCommit) `
        -ExpectedExitCode 1 `
        -Contains @('is not an ancestor of main', 'ships work nobody reviewed and nobody merged')

    Assert-Case `
        -Name 'a commit that is not in the checkout at all is refused, not assumed' `
        -Result (Invoke-Gates -Fixture $good -Tag 'v1.0.0' -CommitSha '0123456789012345678901234567890123456789') `
        -ExpectedExitCode 1 `
        -Contains @('is not in this checkout')

    Write-Host ''
    Write-Host 'A commit CI has not passed is not a release'

    Assert-Case `
        -Name 'a commit with no CI run against it is refused' `
        -Result (Invoke-Gates -Fixture $good -Tag 'v1.0.0' -CiRunsJson $good.CiNone) `
        -ExpectedExitCode 1 `
        -Contains @('No successful CI run exists', 'has ever been recorded')

    Assert-Case `
        -Name 'a commit whose CI run failed is refused, and the run is linked' `
        -Result (Invoke-Gates -Fixture $good -Tag 'v1.0.0' -CiRunsJson $good.CiFailed) `
        -ExpectedExitCode 1 `
        -Contains @('No successful CI run exists', 'actions/runs/1', 'known flake')

    Write-Host ''
    Write-Host 'Nothing is released until every milestone is finished'

    Assert-Case `
        -Name 'an open milestone stops the release, and is named with its open issue count' `
        -Result (Invoke-Gates -Fixture $good -Tag 'v1.0.0' -MilestonesJson $good.MilestonesOpen) `
        -ExpectedExitCode 1 `
        -Contains @('M1 - Recording Engine', 'still open', '14 open issue(s)')

    # Closing a milestone on GitHub does not require its issues to be closed, so
    # "closed" alone is not the finished state. M9 in the fixture is exactly
    # that shape, and it has to be caught.
    Assert-Case `
        -Name 'a closed milestone with open issues in it still stops the release' `
        -Result (Invoke-Gates -Fixture $good -Tag 'v1.0.0' -MilestonesJson $good.MilestonesOpen) `
        -ExpectedExitCode 1 `
        -Contains @('M9 - Highlight Plugin API', '3 open issue(s)')

    $noMilestones = New-Json -Path (Join-Path $good.Root '.milestones-empty.json') -Value '[]'
    Assert-Case `
        -Name 'no milestones at all is refused rather than read as nothing to object to' `
        -Result (Invoke-Gates -Fixture $good -Tag 'v1.0.0' -MilestonesJson $noMilestones) `
        -ExpectedExitCode 1 `
        -Contains @('no milestones at all', 'must not be read as permission')

    # A full page means the query may have truncated, and an unseen milestone is
    # the one that would let this gate pass when it should not. Every milestone
    # here is finished, so a gate that did not notice the page would pass.
    $fullPage = New-Json -Path (Join-Path $good.Root '.milestones-full-page.json') -Value (
        '[' + ((1..100 | ForEach-Object { "{`"title`":`"M$_`",`"state`":`"closed`",`"open_issues`":0}" }) -join ',') + ']')
    Assert-Case `
        -Name 'a full page of milestones is refused, because there may be more' `
        -Result (Invoke-Gates -Fixture $good -Tag 'v1.0.0' -MilestonesJson $fullPage) `
        -ExpectedExitCode 1 `
        -Contains @('which is a full page', 'may be more that this gate has not seen')

    # The gate is about the *first* release. Once something has been published,
    # which version comes next is semantic versioning, and a new milestone
    # opening must not lock the project out of shipping a fix.
    $released = New-Json -Path (Join-Path $good.Root '.releases-one.json') -Value @'
[
  { "tag_name": "v1.0.0", "draft": false, "prerelease": false, "published_at": "2026-09-01T00:00:00Z" }
]
'@
    Assert-Case `
        -Name 'once something has been published the milestone gate retires itself' `
        -Result (Invoke-Gates -Fixture $good -Tag 'v1.0.1' -MilestonesJson $good.MilestonesOpen -ReleasesJson $released) `
        -ExpectedExitCode 1 `
        -Contains @('Retired', 'v1.0.0 has been published') `
        -DoesNotContain @('REFUSED] Milestones')

    # A draft is not something anybody has been given, so it must not retire the
    # gate. Getting this backwards would mean one accidental draft unlocked
    # every future release.
    $draftOnly = New-Json -Path (Join-Path $good.Root '.releases-draft.json') -Value @'
[
  { "tag_name": "v1.0.0", "draft": true, "prerelease": false, "published_at": null }
]
'@
    Assert-Case `
        -Name 'a draft release does not retire the milestone gate' `
        -Result (Invoke-Gates -Fixture $good -Tag 'v1.0.0' -MilestonesJson $good.MilestonesOpen -ReleasesJson $draftOnly) `
        -ExpectedExitCode 1 `
        -Contains @('does not release until every', 'M1 - Recording Engine')

    Write-Host ''
    Write-Host 'The installer may not go out without what conveying it obliges'

    $noLicences = New-Fixture -Name 'no-licences'
    Assert-Case `
        -Name 'a bundle without the licence texts is refused, naming each one and issue #123' `
        -Result (Invoke-Gates -Fixture $noLicences -Tag 'v1.0.0') `
        -ExpectedExitCode 1 `
        -Contains @(
        'LICENSE.txt',
        'THIRD-PARTY-NOTICES-RUST.md',
        'NOTICE.md',
        'LGPL-3.0.txt',
        'GPL-3.0.txt',
        'issue #123',
        'not one that may be distributed'
    )

    # "Carries nothing" and "carries the wrong things" are different mistakes
    # with different remedies, and the message has to tell them apart.
    $emptyPayload = New-Fixture -Name 'empty-payload'
    Get-ChildItem -LiteralPath $emptyPayload.Payload -File | Remove-Item -Force
    Assert-Case `
        -Name 'an unstaged payload says so, rather than only listing what is missing' `
        -Result (Invoke-Gates -Fixture $emptyPayload -Tag 'v1.0.0') `
        -ExpectedExitCode 1 `
        -Contains @('carry nothing at all', 'stage-installer-payload.ps1')

    # Deleting `bundle.resources` would make the installer carry nothing at all
    # while every other check still passed, so the gate has to notice the
    # declaration is gone rather than reporting an empty list of resource names.
    $noResources = New-Fixture -Name 'no-resources' -WithLicences
    $stripped = Join-Path $noResources.Root 'apps\desktop\src-tauri\tauri.conf.json'
    (Get-Content -LiteralPath $stripped -Raw).Replace('"resources": {
      "installer-payload/": ""
    }', '"active": true') | Set-Content -LiteralPath $stripped -Encoding Ascii
    Assert-Case `
        -Name 'a bundle declaring no resources at all is refused, and says so' `
        -Result (Invoke-Gates -Fixture $noResources -Tag 'v1.0.0') `
        -ExpectedExitCode 1 `
        -Contains @('It would carry nothing at all. tauri.conf.json declares no')

    # Robust to how issue #123 is discharged: the gate asks what the *bundle*
    # collects, not what one directory holds, so a payload declared as a second
    # resource satisfies it too.
    $secondResource = New-Fixture -Name 'licences-elsewhere'
    foreach ($file in $licencePayload) {
        Write-Fixture -Path (Join-Path $secondResource.Root "apps\desktop\src-tauri\licences\$file") -Content "the text of $file"
    }
    $config = Join-Path $secondResource.Root 'apps\desktop\src-tauri\tauri.conf.json'
    (Get-Content -LiteralPath $config -Raw).Replace('"installer-payload/": ""', '"installer-payload/": "", "licences/": ""') |
        Set-Content -LiteralPath $config -Encoding Ascii
    Assert-Case `
        -Name 'licences collected through a second declared resource satisfy the gate' `
        -Result (Invoke-Gates -Fixture $secondResource -Tag 'v1.0.0') `
        -ExpectedExitCode 0 `
        -Contains @('All 5 gates pass')

    Write-Host ''
    Write-Host 'Evidence that is missing is a refusal, not a pass'

    $absent = Join-Path $good.Root '.no-such-file.json'
    Assert-Case `
        -Name 'a missing evidence file refuses and says what should have produced it' `
        -Result (Invoke-Gates -Fixture $good -Tag 'v1.0.0' -MilestonesJson $absent) `
        -ExpectedExitCode 1 `
        -Contains @('does not exist', 'gh api')

    # What a failed `gh api` redirected into a file leaves behind. Read as JSON
    # it is nothing at all, which is indistinguishable from "no open
    # milestones" unless it is caught here.
    $emptyEvidence = New-Json -Path (Join-Path $good.Root '.empty.json') -Value ''
    Assert-Case `
        -Name 'an empty evidence file refuses rather than reading as an empty list' `
        -Result (Invoke-Gates -Fixture $good -Tag 'v1.0.0' -MilestonesJson $emptyEvidence) `
        -ExpectedExitCode 1 `
        -Contains @('is empty')

    Write-Host ''
    Write-Host 'Reporting'

    # Four things wrong at once should be four things said once, not one thing
    # said four times over four attempts.
    $everything = New-Fixture -Name 'everything-wrong' -Disagreeing @{ 'package.json' = '0.2.0' }
    Assert-Case `
        -Name 'every gate is evaluated, so one refusal does not hide the next three' `
        -Result (Invoke-Gates -Fixture $everything -Tag 'v1.0.0' -CommitSha $everything.BranchCommit -MilestonesJson $everything.MilestonesOpen -CiRunsJson $everything.CiFailed) `
        -ExpectedExitCode 1 `
        -Contains @('5 of 5 gates refuse', 'Version, Branch, Continuous integration, Milestones, Licences')

    # Rehearsal exists so the gates can be read on a day when they refuse. It
    # reports the same verdicts and changes none of them.
    Assert-Case `
        -Name 'a rehearsal reports the refusals and still exits 0, because it can publish nothing' `
        -Result (Invoke-Gates -Fixture $everything -Tag 'v1.0.0' -MilestonesJson $everything.MilestonesOpen -Rehearse) `
        -ExpectedExitCode 0 `
        -Contains @('REHEARSAL', 'cannot produce a release', 'REFUSED')
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
