# Releasing Clipped

**Clipped has never been released, and will not be until every milestone is
finished.** This page is the rule that says so, and
[`.github/workflows/release.yml`](../.github/workflows/release.yml) is the
mechanism that enforces it. Both exist now, while nothing is at stake, so that
the first real release is boring.

Three questions this answers, in order of how easy they are to get wrong:

1. [What a version is, and what a milestone is not](#a-milestone-is-not-a-version)
2. [Who decides a milestone is finished](#who-decides-a-milestone-is-finished)
3. [What a tag is allowed to do](#the-seven-gates)

## A milestone is not a version

`M9` complete does not mean `0.9.0`. No milestone number maps to a version
number, and none ever will.

[ADR 0014](adr/0014-a-milestone-is-not-a-version.md) is the decision, with the
alternatives that were considered and rejected — milestone-numbered versions,
`0.x` previews, calendar versioning — and what would make the `0.x` one win
later. This page is the procedure that follows from it; that page is where to
argue with it.

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
- push a tag once the rehearsal reports all seven gates passing.

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

## The seven gates

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
| **Corresponding source** | the source of the FFmpeg build the installer would ship is not assembled, is incomplete, or is the source of a different build | see below |
| **Codec patents** | the four questions in [ADR 0008](adr/0008-codec-patent-position.md) have not been asked and their answers written into that record | see below |

The milestone gate **retires itself** once anything has been published. After
the first release, which version comes next is semantic versioning, and a newly
opened milestone must not lock the project out of shipping a fix. A *draft* does
not retire it — nobody has been given a draft.

### The licence gate

The installer carries a pinned LGPL v3 FFmpeg. Conveying it owes a notice, both
licence texts, the corresponding source and the relinking permission;
[docs/licensing.md](licensing.md) sets out the whole list, and
[#123](https://github.com/wildware-uk/clipped/issues/123) is the work.

A workflow able to publish an installer without that paperwork is a workflow
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

The paths above are where
[`scripts/collect-notices.ps1`](../scripts/collect-notices.ps1) writes them
today. The gate matches on the **file name** rather than on the path, anywhere
under a declared resource, so that it does not assume how
[#123](https://github.com/wildware-uk/clipped/issues/123) is discharged — staged
into `installer-payload`, or declared as a second resource, either satisfies it.
It checks that the six texts are *present*, and nothing more: not their
contents, not that `NOTICE.md` is FFmpeg's rather than somebody else's. Whoever
publishes the draft is still the one who has read them.

**This gate passes as of [#538](https://github.com/wildware-uk/clipped/pull/538)**,
which put the payload into the bundle and was the remaining half of
[#123](https://github.com/wildware-uk/clipped/issues/123). It started passing on
its own, with no change here, which is what checking the artefact rather than
the issue tracker buys. Confirmed on 2026-08-17 by listing the contents of an
installer built from `edb36d8`: all six texts are in it, under `licences\` and
`licences\ffmpeg\`.

The gate checks the artefact rather than the issue tracker deliberately. #123
being closed is somebody's opinion; a bundle without `GPL-3.0.txt` in it is a
licence breach whatever anybody thinks.

One obligation it does **not** check, because it is not a file in a bundle, and
which whoever publishes the draft has to satisfy by hand:

- **The relinking permission**, tested by substituting the FFmpeg DLLs in the
  installed application and confirming it still records. docs/licensing.md,
  "Replacing the FFmpeg libraries", is the procedure.

Note that `bundle.licenseFile` in `tauri.conf.json` does not discharge anything
here. NSIS *displays* that text during installation; it does not install it.

### The corresponding source gate

The licence texts are the half of the FFmpeg obligation that fits inside an
installer. The other half is **the source of the exact build being shipped**,
and until [#123](https://github.com/wildware-uk/clipped/issues/123) was finished
this document, `docs/licensing.md` and the `NOTICE.md` inside every installed
copy all described a thing the workflow did not do:
[`scripts/fetch-ffmpeg-source.ps1`](../scripts/fetch-ffmpeg-source.ps1) existed,
nothing ran it, and the release job uploaded an installer and a `.sha256` and
nothing else. The notice a user would have received says the source "is
published with the Clipped release that carries these files" — a sentence that
would have been false on somebody else's machine, unrecallably.

**What happens now.** The gate job assembles the corresponding source, into a
directory outside the cached FFmpeg tree so that it is a fetch verified during
this run rather than bytes restored from a cache. This gate then refuses unless:

- `CORRESPONDING-SOURCE.md` is there at all — which is what fails if the step
  that assembles it is removed or errors;
- it records the asset name and SHA-256 that `scripts/fetch-ffmpeg.ps1 -PrintPin`
  reports, so that source assembled for a previous pin is caught. That is the
  failure a directory listing cannot show: every file present, every archive
  intact, all of it the source of a build nobody is shipping;
- every archive the manifest promises exists, opens as an archive and holds more
  than a token number of entries — a truncated fetch has the right name and the
  wrong contents;
- there are two of them. FFmpeg without
  [BtbN/FFmpeg-Builds](https://github.com/BtbN/FFmpeg-Builds) is not the
  corresponding source of these DLLs: the `configure` arguments and the versions
  of every external library compiled into them live in the recipe.

The release job then downloads what the gate approved and attaches it in the
same `gh release create` call as the installer. Same page, same moment: whoever
can download the object code can download its source, with no request to make
and nobody to ask. **A written offer was considered and not taken** — it is
permitted, but it commits the project to answering requests for years from
anybody holding a copy, and this one costs about 22 MB per release and is
discharged the moment the draft exists. Which of the two is right is the
repository owner's call; changing it means changing this section, `NOTICE.md`
in `scripts/collect-notices.ps1`, and the workflow together, because the
installed notice is what a recipient relies on.

The gate does not check that the archives contain FFmpeg. The commit ids in the
manifest are what tie them to the build and they were verified against the
remote when the archives were made; a gate that tried to recognise FFmpeg by its
file names would refuse a correct release the first time upstream renamed
something.

### The codec patent gate

The two gates above are entirely about **copyright**, and a copyright licence is
not a patent licence. The installer ships an FFmpeg whose libraries encode H.264
through a statically linked `libopenh264`, and the default codec resolves to
HEVC on every machine whose GPU has no AV1 encoder. Both are patent-pool
standards, and nothing in the licence payload grants anything over them.

[ADR 0008](adr/0008-codec-patent-position.md) is the position, and it is
`Accepted`: AV1 first, AVC and HEVC only on silicon a vendor already licensed,
no second software encoder for a pool codec. Its sixth decision blocks the first
signed public release on four questions being put to somebody qualified — **not
on a particular answer, on somebody having answered.** Until this gate existed,
that block was a sentence in a document. This gate is what makes it a block.

It reads the "The answers" section of that record and refuses while any of its
six fields — who answered, when, and one per question — is still the
`_UNANSWERED_` placeholder, blank, whitespace, or too short to be an answer.
Filling it in is what discharges the gate: put the questions to a lawyer, write
what they said into the record, commit it.

Three things it deliberately does not do:

- **It does not check that anybody was consulted**, because no script can. It
  checks that the answers were written down where a contributor and a user can
  read them, which is the most it can honestly claim.
- **It does not ask for a particular answer.** Passing it is not this project
  saying a release is safe; ADR 0008 is explicit that nothing available here can
  establish that.
- **It does not retire after the first release**, the way the milestone gate
  does. Answers once written stay written, so it costs a later release nothing;
  what it goes on catching is that section being deleted or emptied.

It reads one Markdown file out of the checkout, so it costs the gate job
nothing.

## Making a release

1. **Finish the milestones.** Every issue closed, every milestone closed by a
   maintainer.
2. **Confirm the relinking permission by hand.** Everything else in
   [#123](https://github.com/wildware-uk/clipped/issues/123) is carried by the
   workflow — the texts in the installer, the source on the release — but
   substituting the FFmpeg DLLs in an installed copy and confirming it still
   records is a person at a keyboard. docs/licensing.md, "Replacing the FFmpeg
   libraries", is the procedure.
3. **Ask the four codec patent questions and write the answers down.** Also a
   person, and the longest lead time on this list — it needs somebody outside
   the project. [ADR 0008](adr/0008-codec-patent-position.md) has the questions
   written to be answerable and the section the answers go in;
   [the codec patent gate](#the-codec-patent-gate) refuses until they are there.
   Start this before step 1, not after step 7.
4. **Bump the version** in one reviewed pull request — every declaration in the
   table above, plus both lockfiles — and merge it.
5. **Wait for CI to pass on `main`** at that commit. The gate requires it, and
   a tag pushed before CI finishes is refused for a reason that would have gone
   away on its own.
6. **Rehearse.** Run the workflow from the Actions tab with the tag you are
   about to push. It reports every gate without creating anything.
7. **Tag the merge commit and push it**:

   ```text
   git tag -a v1.0.0 -m "Clipped 1.0.0"
   git push origin v1.0.0
   ```

8. **Read the draft.** The workflow attaches the installer, a `.sha256`
   sidecar, the corresponding FFmpeg source — two archives and
   `CORRESPONDING-SOURCE.md` — and notes that state the build is unsigned, that
   SmartScreen will warn, the installer's SHA-256 and what the source assets
   are. Check the hash against the asset yourself before publishing — the notes
   are generated from the file, but you are the last person who can catch it if
   something upstream replaced it. Check the source assets are on the draft too:
   the gates refuse without them, but the assets are what a recipient actually
   gets.
9. **Publish it.** That is the last gate, and it is a person.

### Rehearsing

`workflow_dispatch` on the Release workflow takes a tag and reports on all seven
gates without publishing anything. It exists because the gates all refuse today,
and a refusal nobody can watch working is a refusal nobody trusts. The rehearsal
assembles the corresponding source and gates it exactly as a tag push does,
which is how that path can be exercised without a release existing.

Ticking **build** additionally builds the installer, hashes it and renders the
notes into the run summary, then leaves the file on the runner. It creates no
release and uploads no installer, deliberately: an installer built today is one
that [may not be distributed](#the-licence-gate), and a workflow artefact on a
public repository is a distribution.

The corresponding source *is* uploaded as a run artefact, on a rehearsal as on a
tag push, and that is the one thing here that may be distributed — it is LGPL
v3 source that anybody may redistribute, and publishing it is the obligation
rather than the risk. It is uploaded because the job that drafts the release
attaches the bytes the gate approved rather than a second copy it assembled for
itself.

You can run the gates locally against a checkout, which is what CI does:

```powershell
$sha = git rev-parse HEAD
gh api "repos/wildware-uk/clipped/milestones?state=all&per_page=100" | Out-File milestones.json -Encoding utf8
gh api "repos/wildware-uk/clipped/releases?per_page=100" | Out-File releases.json -Encoding utf8
gh api "repos/wildware-uk/clipped/actions/workflows/ci.yml/runs?head_sha=$sha" | Out-File ci-runs.json -Encoding utf8

# The corresponding-source gate reads what this assembles. Skip it and that gate
# refuses, correctly - there would be nothing to publish.
powershell -ExecutionPolicy Bypass -File scripts/fetch-ffmpeg-source.ps1

powershell -ExecutionPolicy Bypass -File scripts/check-release-gates.ps1 `
    -Tag v1.0.0 -CommitSha $sha `
    -MilestonesJson milestones.json -ReleasesJson releases.json -CiRunsJson ci-runs.json
```

`fetch-ffmpeg-source.ps1` writes to `third-party/ffmpeg/source`, which is where
the gate looks by default; the workflow passes `-CorrespondingSourceDirectory`
because it keeps it out of the cached tree. Running it takes a shallow fetch of
two commits and about 22 MB, and a second run over an intact directory fetches
nothing.

The three GitHub answers are handed to the script rather than fetched by it, so
that every branch in it can be tested against a fixture instead of against the
live repository. [`scripts/test-check-release-gates.ps1`](../scripts/test-check-release-gates.ps1)
is that test suite, and CI runs it on every pull request.

## What has been proven, and what has not

The point of building this early is that nobody is relying on it yet, so this
section is worth more than the rest of the page. It distinguishes what has been
*run* from what has only been *read*, and it is the section to update after the
first real tag.

**`.github/workflows/release.yml` has never executed.** Not once, on any event.
`gh api repos/wildware-uk/clipped/actions/workflows/release.yml/runs` returned
`total_count: 0` on 2026-08-17; the repository has no tags and no releases. An
earlier version of this page claimed the gates had been proven "on a throwaway
tag pushed and deleted", and that was not true — no tag has ever been pushed.
Nothing below rests on the workflow having run.

Verified by running, on 2026-08-17, against the tree at `edb36d8`:

- **The gate script refuses this repository, for the right reasons.** Run with
  the three `gh api` answers above and `-Tag v1.0.0`: Version, Continuous
  integration and Milestones refuse and Branch passes, each naming what is
  wrong. Its own suite (`scripts/test-check-release-gates.ps1`) and
  `scripts/test-write-release-notes.ps1` both pass.
- **The codec patent gate refuses this repository, and passes when answered.**
  Re-run on 2026-08-19 against `94aa069` with the licence payload collected and
  the corresponding source assembled: two of seven gates refuse, Milestones and
  Codec patents, and the other five pass. With plausible answers written into
  ADR 0008 by hand, the codec gate passed and named who gave them, leaving
  Milestones as the only refusal; the answers were then reverted, so what is
  committed is the unanswered state. Each of that gate's checks was removed in
  turn and its cases went red each time, and renaming the record's `### The
  answers` heading turned the case that copies the real record in red as well —
  which is what ties the gate to the document rather than to a fixture of its
  own shape. Suite: 48 cases.
- **The corresponding source is assembled, and the gate reads it.** Run against
  the real pin on 2026-08-17, `scripts/fetch-ffmpeg-source.ps1` fetched
  `9b6c8969e05b4f0b29f0f85cd501be6b3e582e6b` from FFmpeg and
  `2437e7b868da3c11872367b15f3c613b87c24819` from BtbN/FFmpeg-Builds and wrote
  21.2 MB and 0.2 MB of archives beside a manifest. With that directory in
  place the gate passed and listed the assets; with the directory deleted it
  refused, naming the FFmpeg build and `fetch-ffmpeg-source.ps1`; with one
  archive truncated to a third of its length it refused with "not a readable
  archive". Each of the gate's checks was also removed in turn from the script
  and the suite failed each time, so none of them is a check nothing tests.
- **The licence gate now passes**, which it did not when this page was written.
  [#123](https://github.com/wildware-uk/clipped/issues/123) landed in
  [#538](https://github.com/wildware-uk/clipped/pull/538), and an installer
  built from this tree carries all six texts — confirmed by listing the
  contents of the built `.exe`, not by reading the configuration.
- **The build the workflow performs works, end to end.** `npm ci`,
  `cargo build --release --locked -p clipped-recorder`,
  `scripts/collect-notices.ps1` and `npm run build:app` produced exactly one
  installer, at exactly the path the workflow's "Find the installer" step
  expects. `scripts/write-release-notes.ps1` rendered notes from it, and the
  SHA-256 it published matched `Get-FileHash` on the asset.
- **The workflow is well-formed.** `actionlint` reports nothing on it. `zizmor`
  reported six high-severity findings, of which the template injections and the
  cache poisoning on the publishing path have been fixed; what remains is
  argued in the comments at each site.

Read and reasoned, but **not** run:

- **`gh release create` has never executed here.** That the draft appears, with
  the installer, its checksum and the three corresponding-source assets attached
  and `--verify-tag` accepting the tag, is inference from the documented
  behaviour of `gh` and of the `contents: write` permission.
- **The corresponding source has never made the trip between the two jobs.**
  `actions/upload-artifact` in the gate job and `actions/download-artifact` in
  the release job have not run here. Within one workflow run neither needs a
  token or an extra permission, which is documented rather than observed. If the
  download were to arrive empty, the release job refuses before writing any
  notes — that step exists precisely because "the installer went up and the
  source did not" is the failure that looks like success. The first rehearsal
  with **build** ticked exercises the whole path except `gh release create`.
- **No tag push has ever triggered the workflow**, so the `on.push.tags` match,
  the `needs: gate` refusal actually preventing the build, and the branch gate
  running against a real tag ref are all unobserved. They are also the cheapest
  things to observe: the first rehearsal from the Actions tab exercises every
  one of them except the last step.
- **The gate job predicts what the installer will carry; it does not inspect
  it.** It stages the payload on a different runner with a stand-in recorder,
  and the release job then builds the real installer. The two check out the same
  commit and run the same scripts, so they should not disagree — but "should
  not" is the honest strength of that claim, and the end-to-end confirmation
  above was done by hand rather than by the workflow.
- **Nothing here signs anything.** The installer is unsigned, SmartScreen will
  warn about it, and the release notes say so rather than leaving somebody to
  guess. Code signing is not in scope of any milestone yet.

The first rehearsal should be run from the Actions tab with **build** ticked,
and this section updated with what it actually did.
