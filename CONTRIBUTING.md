# Contributing to Clipped

Thanks for taking an interest. This document covers how work is tracked, how
changes are named, and what has to be true before a change is considered done.

[AGENTS.md](AGENTS.md) holds the engineering standards themselves. It applies to
human and automated contributors alike — an agent-authored change is reviewed
against exactly the same bar as a hand-written one, and "an agent wrote it" is
not an explanation for anything.

Participation is covered by the [Code of Conduct](CODE_OF_CONDUCT.md). Its
enforcement contact is still an unfilled `[INSERT CONTACT METHOD]` placeholder;
until the maintainer replaces it, raise conduct concerns with the repository
maintainer directly.

## Getting set up

[README.md](README.md) has the build, and
[docs/prerequisites.md](docs/prerequisites.md) has the full toolchain list. In
short: a clean clone, a stable Rust toolchain, the MSVC build tools, LLVM, and
one run of `scripts/fetch-ffmpeg.ps1` should be enough to run
`cargo build --workspace` — in that shell, with nothing else to set. If it is
not, that is a bug in the documentation — please raise an issue.

## Issues and milestones

GitHub issues are the source of truth for all work:

```text
https://github.com/wildware-uk/clipped/issues
```

- **Start from an issue.** If nothing covers the work you want to do, raise one
  first and agree the scope there. This applies to bug fixes as much as to
  features.
- **The acceptance criteria are the ticket.** They define what "finished" means.
  If they turn out to be wrong or incomplete, say so on the issue and change
  them deliberately, rather than quietly building something different.
- **Milestones `M0` to `M14`** group issues and follow the milestone order in
  `SPEC.md` section 42, from project foundations through the recording engine to
  performance hardening. Do not implement later-milestone work to satisfy an
  earlier ticket; capture compatibility work does not belong in the ticket that
  first opens a capture session.
- **`SPEC.md` is a reference document, not a task list.** It describes the
  product and is not updated to track progress.
- **`area:` labels** identify the subsystems involved, and `size:` labels give a
  rough scale. Linked issues record dependencies between tickets.

Work you discover mid-ticket that the acceptance criteria do not require belongs
in a new issue, not in the current change. Substantial `TODO` comments must name
the issue they depend on, in the form `TODO(#184): …`.

### Adding a game needs no Rust

The smallest useful contribution to Clipped is a game the catalogue does not
know yet, and it is a one-file pull request: append a `[[game]]` block to
[`crates/game-detection/data/games.toml`](crates/game-detection/data/games.toml).
Nothing has to be registered anywhere, no Rust changes, and the file's own header
is the field reference — [docs/game-detection.md](docs/game-detection.md) has the
detail if you want it. A bad entry fails the build with a message naming the file
and the entry, so it is not a change you can get quietly wrong.

## Branches and commits

Branch from `main`, and name the branch after the issue:

```text
<type>/<issue-number>-<short-description>

feat/12-window-capture-backend
fix/48-audio-drift-on-device-change
docs/6-architecture-overview
```

`<type>` is one of `feat`, `fix`, `docs`, `refactor`, `test`, `perf` or `chore`.

Commit messages explain **why** the change exists; the diff already says what it
does. The subject line is imperative, fits in about 72 characters and ends with
the issue number:

```text
Create the Cargo workspace and repository layout (#1)

Eleven library crates with documented responsibilities, the recorder
binary, placeholders for the desktop application and web packages, and
the four test suites. Layering is enforced by a test that reads the real
dependency graph from cargo metadata.

Closes #1.
```

Put `Closes #<N>.` in the body of the commit or pull request that completes the
issue, so history explains itself later without a trip to the tracker.

### Commit scope

Keep each change focused, and avoid combining unrelated refactors, formatting,
feature work, dependency upgrades and bug fixes into one commit unless there is a
genuine reason (AGENTS.md section 39). Small coherent changes are easier to
review, and far easier to revert when a capture regression only shows up on
somebody else's GPU.

The same applies to pull requests: one issue per pull request wherever
practical.

## What "done" means

A ticket is not complete because the code exists (AGENTS.md sections 52 and 53).
Before asking for review, all four of these must pass on your machine:

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace
cargo test --workspace
```

Some of those tests play a quiet tone through your speakers, because measuring
a real capture needs a real reference signal. If that is unwelcome right now,
`CLIPPED_SKIP_AUDIO=1 cargo test --workspace` skips every test that touches an
audio device, reporting each skip on stderr. See
[docs/testing.md](docs/testing.md#running-the-suite-without-making-a-noise).

Beyond that, a change is done when it is implemented, builds, is covered by
tests where tests are meaningful, has had its behaviour actually verified,
handles its failure cases, updates the documentation it invalidates, and
introduces no known regression.

Record verification evidence on the issue and in the pull request: what was
built, what was tested, what was measured, and what was verified by hand. Be
specific.

```text
Verification

- cargo test --workspace passes (34 tests)
- recorded a 30-second session against the test pattern application
- ffprobe confirms one AV1 video stream and three audio streams
- manually verified each audio track can be muted independently
```

"Everything works" is not verification evidence.

If part of the ticket could not be completed, say so explicitly in the pull
request and on the issue. Partial honest work is welcome; a ticket closed over
an unverified claim is not. Never disable a test, delete an assertion or
substitute mock data to make a change look finished (AGENTS.md section 54).

## Continuous integration

Every pull request, and every push to `main`, runs
[`.github/workflows/ci.yml`](.github/workflows/ci.yml) on a GitHub-hosted
**Windows** runner. Clipped is a Windows application, so a green build anywhere
else would not tell us much.

Three jobs run in parallel:

| Job | What it runs |
| --- | --- |
| **Rust (format, lint, build, test)** | `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo build --workspace`, `cargo test --workspace` |
| **Dependencies (licences and advisories)** | `cargo deny --all-features check` |
| **Desktop UI (lint and build)** | `npm ci`, `npm run lint`, `npm run build` in `apps/desktop` (once `apps/desktop` exists — see below) |

`main` is **not** branch-protected at the moment, so nothing mechanically blocks
a red pull request from being merged. A red build is still a blocker: it is
enforced by review rather than by the platform, and "CI was failing but the
failure looked unrelated" is a claim to make on the pull request and have
agreed, not one to act on alone. Protection was considered and deliberately
declined while the initial build-out is in flight — requiring branches to be up
to date would force a rebase and a full CI re-run on every other open pull
request after each merge. Issue #4 records the decision, the exact command that
turns it on, and the condition for doing so.

The Rust job runs exactly the four commands in [What "done"
means](#what-done-means), so there is nothing CI checks that you cannot check
first. It also runs `scripts/check-prerequisites.ps1` on the runner: a
GitHub-hosted image is a machine that has never built Clipped, so it is the
honest test of whether [docs/prerequisites.md](docs/prerequisites.md) is still
accurate, and its output is copied into the run summary.

The compiler is not installed by the workflow. CI runs `rustup toolchain
install`, which reads `rust-toolchain.toml`, so CI and your machine use the same
compiler by construction.

### Steps that skip themselves

Parts of the project do not exist yet, and the workflow says so rather than
passing silently. The desktop UI steps skip until `apps/desktop/package.json`
appears, printing a `SKIPPED - …` line into the log and the run summary
explaining what is missing. When you add the missing piece, the steps start
running on their own — but read the skip notice first, because a step that has
never executed has never been tested either.

Hardware-dependent capture and encoder tests are **not** run in CI. A hosted
runner has no GPU and nothing to record, so those tests would be measuring the
runner. They stay on a documented manual path.

### Dependency licences and advisories

[`deny.toml`](deny.toml) is the machine-readable form of the policy in the next
section. It holds the list of licences a dependency may carry, and
[`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny) rejects anything
that is not on it — so GPL, AGPL and any other unlisted licence fail the check
without anyone having to notice. The same run checks the RustSec advisory
database, wildcard version requirements, and that every crate came from
crates.io.

Run it yourself before adding a dependency:

```text
cargo install cargo-deny --locked
cargo deny --all-features check
```

Adding a licence to the allow-list is a licensing decision about the project,
not a way to get a build green. Raise it on the issue.

## Releases and versioning

**Nothing is released until every milestone is finished**, and the first release
will be `v1.0.0`. A milestone number is not a version number: `M9` complete does
not mean `0.9.0`, and no milestone implies a version at all. The `0.1.0` in the
manifests is a placeholder for "unreleased".

[docs/releasing.md](docs/releasing.md) is the rule in full — what a version is,
who may decide a milestone is finished (a maintainer, by closing it on GitHub),
what an agent may and may not do, and the five gates a tag has to pass before
[`.github/workflows/release.yml`](.github/workflows/release.yml) will build
anything. Read it before tagging.

Two things worth knowing even if you never make a release:

- **The tag is the source of truth for the version.** Twenty-nine declarations
  across seven files have to agree with it, and the release build refuses while
  naming every one that does not, rather than editing them to match.
- **The workflow cannot publish while the licence obligations in
  [docs/licensing.md](docs/licensing.md) are unmet.** The installer carries a
  pinned LGPL v3 FFmpeg, and until it also carries the notices and licence texts
  that conveying it owes ([#123](https://github.com/wildware-uk/clipped/issues/123)),
  a release built from this tree may not be distributed.

## Licensing and dependencies

Clipped is licensed under the [Mozilla Public License 2.0](LICENSE). It was
chosen because MPL-2.0 is file-level copyleft: modifications to Clipped's own
source files must stay open, which is the point of the project, while the
licence still permits linking against LGPL FFmpeg and against the permissive
Rust ecosystem. A full GPL would have kept the source open too, but would have
ruled out parts of that ecosystem the recorder needs.

The practical consequence when adding a dependency:

- MIT, Apache-2.0, BSD-2/3-Clause, ISC and MPL-2.0 crates are fine.
- GPL-only and AGPL dependencies are not.
- Dependencies with unclear, missing or unusual licensing should not be added.
  If a licence needs explaining, document it alongside the dependency.

State the licence of any new dependency in the pull request. Every crate in the
workspace declares `license = "MPL-2.0"` through `Cargo.toml`, and new crates
should inherit it with `license.workspace = true` rather than restating it.

Dependencies are not free even when their licence is (AGENTS.md section 10):
each one adds security surface, compile time, binary size and maintenance. Prefer
the standard library and existing workspace crates before adding another.

When adapting code from another project, verify the licence first, preserve the
attribution it requires, and note the source and any significant modifications in
a comment. Do not present third-party code as original.

### Dependencies and vendored source are two different things

The rules above are about *dependencies*: crates Cargo resolves and fetches,
recorded in `Cargo.lock` and checked on every pull request by `cargo deny`
against [deny.toml](deny.toml). If you add one with a licence outside the
allow-list, CI tells you.

*Vendored source* is third-party material committed into this repository:
generated FFI bindings, a copied header, a transcribed constant. **Nothing
checks it automatically.** Cargo does not know it exists and `cargo deny` never
sees it, so the obligations are yours to discharge deliberately, and a reviewer
has no tool that would catch you failing to. The encoder backends are the
worked examples — `crates/encoder/src/windows/{nvenc,amf,quicksync}/sys.rs` are
each generated from a vendor's headers.

When you commit third-party source, whether written by hand or generated by a
tool:

- **Carry its notices in the file itself.** The copyright line and the
  permission notice the licence requires go at the top of the committed file,
  not only in a separate document. Someone reading that file must be able to
  see what governs it.
- **Record it in [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)** with: where
  it lives in this tree, what it is, the upstream project with the exact tag or
  commit it came from, the licence, and how it was produced — the generation
  command if a tool made it, and anything modified afterwards. Enough that a
  contributor can regenerate it and get the same file, and that a user can see
  what they have been given.
- **Say so in the pull request**, with the licence named. This is the one place
  a human has to notice, because no check will.

Generated bindings are still a derivative of the headers they were generated
from: the type names, field names and constant values are the vendor's. A
licence that permits it is required, and its notices travel with the output.

## Pull requests

Open the pull request against `main`, and include:

- `Closes #<N>.` at the top, so the issue and the change stay linked.
- What changed and why, at a level a reviewer can follow without reading the
  whole diff.
- Each acceptance criterion from the issue, with the evidence that satisfies it.
- Anything left incomplete, stated plainly.

Do not merge your own pull request until it has been reviewed.

## Reporting bugs

Use the issue templates. Capture bugs in particular are hard to reproduce
without the environment they happened in, so the bug template asks up front for
your Windows version, GPU and driver version, and the capture backend and
encoder in use. Please fill those in — they are the first three questions
anybody triaging a capture problem will ask.

Never attach recordings, logs or screenshots containing anything you would not
want published. An attachment you add by hand is your own responsibility, so
read it before you upload it. Keeping window contents, audio content and file
contents out of Clipped's own logs is an acceptance criterion on issue #5 and
will be documented in [docs/logging.md](docs/logging.md) and
[docs/privacy.md](docs/privacy.md); it is an intention recorded there, not yet a
property you should rely on.
