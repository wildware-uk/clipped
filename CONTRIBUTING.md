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
short: a clean clone plus a stable Rust toolchain and the MSVC build tools
should be enough to run `cargo build --workspace`. If it is not, that is a bug
in the documentation — please raise an issue.

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
want published. Clipped's own logs are designed not to include window contents,
audio content or file contents ([docs/logging.md](docs/logging.md),
[docs/privacy.md](docs/privacy.md)), but an attachment you add by hand is your
own responsibility.
