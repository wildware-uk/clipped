# Releasing Clipped

**Clipped has never been released, and will not be until every milestone is
finished.** This page is the rule that says so, and
[`.github/workflows/release.yml`](../.github/workflows/release.yml) is the
mechanism that enforces it. Both exist now, while nothing is at stake, so that
the first real release is boring.

Three questions this answers, in order of how easy they are to get wrong:

1. [What a version is, and what a milestone is not](#a-milestone-is-not-a-version)
2. [Who decides a milestone is finished](#who-decides-a-milestone-is-finished)
3. [What a tag is allowed to do](#the-five-gates)

## A milestone is not a version

`M9` complete does not mean `0.9.0`. No milestone number maps to a version
number, and none ever will.

Milestones `M0` to `M15` are groupings of scope, numbered in the order the work
is planned (`SPEC.md` section 42). Version numbers are promises to whoever
installed the previous one. The two are different kinds of thing, and treating
the first as the second breaks at the first collision with reality: `M15 -
Signal Engine` was added on 2026-08-12, after `M0` to `M14` had been numbered,
for design that is not in `SPEC.md` at all. A scheme in which the milestone
number is the version would have had to answer what version that is.

**The rule.**

- **The first release of Clipped is `v1.0.0`**, and it is the release that
  happens when every milestone is finished. There are no `0.x` releases,
  because a `0.x` release would be a version number for a product that has not
  yet done the thing it exists to do.
- **Before that, nothing is released at all.** Not a milestone tag, not a
  preview, not an alpha. Finishing a milestone produces a closed milestone, not
  a version.
- **After `v1.0.0`, versions are ordinary [semantic
  versioning](https://semver.org) over what changed** — a breaking change to
  the configuration, the plugin API, the recording metadata or the command line
  is a major; a user-visible feature is a minor; a fix is a patch. Which one a
  change is, is a judgement about compatibility, not a lookup from a milestone.
  New milestones will keep being added; none of them will imply a version.
- **Pre-releases are allowed and are governed identically.** `v1.0.0-rc.1` is a
  tag like any other and passes exactly the same gates. A release candidate is
  something a stranger installs, so every obligation a release carries, it
  carries too.

### The tree says `0.1.0`, and that is not a version

Every manifest in this repository declares `0.1.0`. That is a placeholder for
"unreleased", not a version anybody chose, and nothing has ever been published
under it.

Raising it to `1.0.0` is a reviewed commit on `main` like any other, and it has
to change **every** declaration at once. There are more of them than there
appear to be — as of this page, 29 across seven files:

| | |
| --- | --- |
| `Cargo.toml` | `[workspace.package] version`, and the `version = "…"` requirement on each of the 21 `clipped-*` path dependencies |
| `apps/desktop/src-tauri/Cargo.toml` | `[package] version`, and the requirement on `clipped-ipc` |
| `apps/desktop/src-tauri/tauri.conf.json` | `version` — the one a user sees, in the installer and in Add or Remove Programs |
| `package.json`, `apps/desktop/package.json`, `packages/shared/package.json`, `packages/ui/package.json` | `version` |

A path dependency's version requirement is not decoration: Cargo enforces it,
so a workspace bumped to `1.0.0` with those left at `0.1.0` does not build.
`Cargo.lock` and `package-lock.json` have to follow as well; `cargo build` and
`npm install` update them, and the release build uses `--locked` and `npm ci`,
which refuse a lockfile that disagrees with its manifest.

The release build does not do any of this for you. It reads the tag, compares,
and refuses while naming every file that disagrees — because a workflow that
edited the tree to match the tag would be choosing the version itself, and the
tag would stop being evidence of anything.

## Who decides a milestone is finished

**A maintainer, by closing the milestone on GitHub.** Not a script, and not an
agent.

"Every issue in it is closed" is checkable, and it is necessary — but it is not
the same statement. Several issues in this repository are open precisely
because an acceptance criterion needs a human at a keyboard: a capture path
exercised against a real game, an installer run on a machine that has never
built Clipped, a recording checked by watching it. Closing the last issue in a
milestone is a claim; closing the milestone is somebody accepting it.

So the release gate asks for both, of every milestone:

- the milestone is **closed**, which needs write access to the repository and
  is a deliberate act;
- it has **no open issues**, which is the half a script can check and the half a
  person forgets. GitHub is happy to close a milestone with fourteen open
  issues in it.

Neither implies the other, which is why both are asked. As of writing, `M0 -
Project Foundations` has no open issues and is still open — the mechanical half
is satisfied and nobody has said so yet.

This also means the rule maintains itself. Opening a new milestone for work
nobody has done re-locks the first release, automatically, with no workflow
change.

## What an agent may do

An agent may:

- run the rehearsal (below) and report which gates refuse;
- open a pull request bumping the version declarations, when the gates say
  everything else is ready;
- push a tag once the rehearsal reports all five gates passing.

An agent may **not** publish a release. The workflow always produces a
**draft**, and a draft is not a distribution: nothing is downloadable and
nothing has been announced until a human opens it, reads the notes and presses
publish.

The reason an agent may push a tag at all is that pushing one early is
harmless: the gates refuse it, no installer is built and nothing is created.
There is no flag that gets past them. Releasing earlier than this page says
means changing this page in a pull request, on purpose, with somebody else
reading it — which is the correct amount of friction for a decision that cannot
be reverted once somebody has downloaded the result.

## The five gates

Pushing a tag matching `v*` runs
[`.github/workflows/release.yml`](../.github/workflows/release.yml). Its first
job answers one question — may this tag become a release? — by running
[`scripts/check-release-gates.ps1`](../scripts/check-release-gates.ps1), and the
build job does not start unless the answer is yes. Every gate is evaluated even
after one has refused, so a tag that is wrong in four ways is told so once.

| Gate | Refuses when | Because |
| --- | --- | --- |
| **Version** | the tag is not `v` + a semantic version, or any version declaration in the tree disagrees with it | an installer reporting a version nobody chose cannot be corrected afterwards; the number is what every bug report will quote |
| **Branch** | the tagged commit is not an ancestor of `origin/main` | a tag on a branch ships work nobody reviewed and nobody merged |
| **Continuous integration** | no successful `ci.yml` run exists for that exact commit | "it was green on the pull request" is a statement about a different tree |
| **Milestones** | anything has yet to be released and any milestone is open or has open issues | the rule at the top of this page |
| **Licences** | the installer would not bundle the licence texts and third-party notices | see below |

The milestone gate **retires itself** once anything has been published. After
the first release, which version comes next is semantic versioning, and a newly
opened milestone must not lock the project out of shipping a fix. A *draft* does
not retire it — nobody has been given a draft.

### The licence gate

The installer carries a pinned LGPL v3 FFmpeg. Conveying it owes a notice, both
licence texts, the corresponding source and the relinking permission;
[docs/licensing.md](licensing.md) sets out the whole list, and
[#123](https://github.com/wildware-uk/clipped/issues/123) is the work.

Today an installer built from this repository carries none of the paperwork,
and `README.md` says so. A workflow able to publish that installer is a workflow
able to break a licence by accident, against everybody who downloads it, without
anybody noticing — so the gate asks what `bundle.resources` in
`tauri.conf.json` would actually collect and requires six files to be among
them:

| File | What it discharges |
| --- | --- |
| `LICENSE.txt` | Clipped's own MPL-2.0 text |
| `THIRD-PARTY-NOTICES.md` | third-party material vendored into Clipped's own source |
| `THIRD-PARTY-NOTICES-RUST.md` | the notice MIT, BSD and ISC each require to travel with the binary |
| `ffmpeg/NOTICE.md` | LGPL v3 section 4(a) — which FFmpeg is shipped, and that it is LGPL |
| `ffmpeg/LGPL-3.0.txt` | LGPL v3 section 4(b) |
| `ffmpeg/GPL-3.0.txt` | LGPL v3 section 4(b) — the GPL text the LGPL is written on top of |

[`scripts/collect-notices.ps1`](../scripts/collect-notices.ps1) already produces
every one of them. What is missing is putting the payload into the bundle, which
is #123's remaining work; when that lands, this gate starts passing on its own,
with no change here.

The gate checks the artefact rather than the issue tracker deliberately. #123
being closed is somebody's opinion; a bundle without `GPL-3.0.txt` in it is a
licence breach whatever anybody thinks.

Two obligations it does **not** check, because they are not files in a bundle,
and which whoever publishes the draft has to satisfy by hand:

- **The corresponding source of the exact FFmpeg build** has to be attached to
  the release or mirrored somewhere that will outlive it.
  [`scripts/fetch-ffmpeg-source.ps1`](../scripts/fetch-ffmpeg-source.ps1)
  assembles it. Pointing at BtbN's release page is not enough: the obligation
  runs to whoever received the binary, and those builds are deleted after a few
  months.
- **The relinking permission**, tested by substituting the FFmpeg DLLs in the
  installed application and confirming it still records. docs/licensing.md,
  "Replacing the FFmpeg libraries", is the procedure.

Note that `bundle.licenseFile` in `tauri.conf.json` does not discharge anything
here. NSIS *displays* that text during installation; it does not install it.

## Making a release

1. **Finish the milestones.** Every issue closed, every milestone closed by a
   maintainer.
2. **Discharge [#123](https://github.com/wildware-uk/clipped/issues/123)** so
   that the installer carries its paperwork.
3. **Bump the version** in one reviewed pull request — every declaration in the
   table above, plus both lockfiles — and merge it.
4. **Wait for CI to pass on `main`** at that commit. The gate requires it, and
   a tag pushed before CI finishes is refused for a reason that would have gone
   away on its own.
5. **Rehearse.** Run the workflow from the Actions tab with the tag you are
   about to push. It reports every gate without creating anything.
6. **Tag the merge commit and push it**:

   ```text
   git tag -a v1.0.0 -m "Clipped 1.0.0"
   git push origin v1.0.0
   ```

7. **Read the draft.** The workflow attaches the installer, a `.sha256`
   sidecar, and notes that state the build is unsigned, that SmartScreen will
   warn, and the installer's SHA-256. Check the hash against the asset yourself
   before publishing — the notes are generated from the file, but you are the
   last person who can catch it if something upstream replaced it.
8. **Attach the corresponding FFmpeg source**, or the link that will outlive
   the release.
9. **Publish it.** That is the last gate, and it is a person.

### Rehearsing

`workflow_dispatch` on the Release workflow takes a tag and reports on all five
gates without publishing anything. It exists because the gates all refuse today,
and a refusal nobody can watch working is a refusal nobody trusts.

Ticking **build** additionally builds the installer, hashes it and renders the
notes into the run summary, then leaves the file on the runner. It uploads
nothing and creates nothing, deliberately: an installer built today is one that
[may not be distributed](#the-licence-gate), and a workflow artefact on a public
repository is a distribution.

You can run the gates locally against a checkout, which is what CI does:

```powershell
gh api "repos/wildware-uk/clipped/milestones?state=all&per_page=100" > milestones.json
gh api "repos/wildware-uk/clipped/releases?per_page=100" > releases.json
gh api "repos/wildware-uk/clipped/actions/workflows/ci.yml/runs?head_sha=$(git rev-parse HEAD)" > ci-runs.json

powershell -ExecutionPolicy Bypass -File scripts/check-release-gates.ps1 `
    -Tag v1.0.0 -CommitSha (git rev-parse HEAD) `
    -MilestonesJson milestones.json -ReleasesJson releases.json -CiRunsJson ci-runs.json
```

The three GitHub answers are handed to the script rather than fetched by it, so
that every branch in it can be tested against a fixture instead of against the
live repository. [`scripts/test-check-release-gates.ps1`](../scripts/test-check-release-gates.ps1)
is that test suite, and CI runs it on every pull request.

## What has not been proven

Honesty about the mechanism, since the point of building it early is that
nobody is relying on it yet:

- **No release has ever been created by this workflow**, because creating one
  would mean tagging a version this project has not reached. Everything up to
  and including building the installer, hashing it and rendering the notes can
  be rehearsed and has been; `gh release create` itself has not run.
- **The gates have been proven by refusing**, on fixtures in the test suite, on
  this repository as it stands, and on a throwaway tag pushed and deleted. The
  path where all five pass has been exercised against fixtures only, because
  the real repository cannot currently satisfy them — which is the intended
  state.
- **Nothing here signs anything.** The installer is unsigned, SmartScreen will
  warn about it, and the release notes say so rather than leaving somebody to
  guess. Code signing is not in scope of any milestone yet.
