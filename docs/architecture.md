# Architecture

This document is the entry point for understanding how Clipped is put
together. It answers the questions AGENTS.md section 47 requires a subsystem to
answer — what this does, why it exists, where it sits, how to run it, how to
test it, and what it assumes — for the system as a whole. Each subsystem
document listed under [Subsystem documentation](#subsystem-documentation)
answers them again for its own area.

Clipped is at the start of its life. Most of what is described below is
designed but not built, and this document marks which is which rather than
describing behaviour that does not exist (AGENTS.md section 7). Anything not
explicitly marked as existing today should be read as intent.

## What it does

Clipped records Windows games without being told to. It watches for a game
launching, starts the capture mode configured for that game, encodes on the
GPU, captures the game's audio, the rest of the system's audio and the
microphone as *separate* tracks in one file, stops when the game exits, and
files the result under that game in a local library.

The product this is being built towards is described in
[SPEC.md](../SPEC.md); the milestone plan is SPEC.md section 42 and the
[issue tracker](https://github.com/wildware-uk/clipped/issues) is the source of
truth for what has actually been done.

## Why it exists in this shape

Three product requirements drive nearly every structural decision, and it is
worth stating them before the diagrams, because most of the architecture is
downstream of them.

**Recording must survive the rest of the application.** A user is in a game;
they will not notice that the recorder died, and they cannot be asked to check.
So a UI crash must not stop a recording ([ADR 0002](adr/0002-separate-recorder-process.md)),
an abrupt power loss must not cost the whole session
([ADR 0001](adr/0001-mkv-archival-container.md)), and a metadata database
failure must not touch the video files (AGENTS.md section 17).

**Audio sources that the user expects to stay separate must stay separate.**
Recording the desktop mix and the microphone is what every other recorder does
and it is not what this product is. Independent tracks are the reason to use
Clipped at all, and capturing them requires scoping capture to a process tree
([ADR 0003](adr/0003-process-specific-audio-capture.md)).

**The recorder runs alongside a game and must be close to free.** SPEC.md
section 38 sets the target at under 3% CPU at 1080p60. That rules out
frame-by-frame CPU work, allocation in capture loops, and blocking a capture
thread on a database, the UI, a plugin or the network (AGENTS.md sections 18
and 20).

## Where the pieces sit

### Process boundary

Clipped is two processes. This is the single most important structural fact
about the system.

```text
┌──────────────────────────────┐        ┌──────────────────────────────┐
│ Desktop application          │        │ Recorder process             │
│ apps/desktop (Tauri + React) │        │ apps/recorder                │
│                              │        │                              │
│ tray, settings, games,       │  IPC   │ game detection, sessions,    │
│ library, clip editor         │ ─────► │ capture, audio, encode, mux  │
│                              │        │                              │
│ may be closed or crash       │        │ owns every recording         │
└──────────────────────────────┘        └──────────────────────────────┘
```

The UI is a *client*. It sends commands and receives status; it never holds the
capture pipeline, and it is not required for recording to start, continue or
finalise. The reasoning and the alternatives considered are in
[ADR 0002](adr/0002-separate-recorder-process.md).

One consequence is enforced in the repository: `apps/desktop` and `packages/`
are deliberately **not** Cargo packages, so the UI cannot be linked into the
recorder even by mistake. The test `no_crate_depends_on_the_desktop_application`
in `tests/integration/tests/workspace_layering.rs` fails if a `Cargo.toml`
appears in any of them.

A second consequence is a convention held up by review, not by a check: no crate
under `crates/` may refer to UI concerns at all. `clipped-session`, the top
library layer, documents this in its own crate docs, but nothing fails if a
crate ignores it — a reviewer has to notice.

The IPC protocol between the two is designed in
[issue #49](https://github.com/wildware-uk/clipped/issues/49) and supervision of
the recorder by the UI in
[issue #106](https://github.com/wildware-uk/clipped/issues/106), both in M5.
Nothing of it exists yet. Until then the recorder is driven from its own command
line.

### Module split

SPEC.md section 5 splits the system into an application service over a native
capture engine. That split is realised as the Cargo workspace: each module in
the specification maps to a crate, or to a part of a crate that owns the
surrounding responsibility.

| SPEC.md section 5 module | Lives in | Status |
| --- | --- | --- |
| Game Detector | `clipped-game-detection` | M4 |
| Session Manager, Capture Coordinator | `clipped-session` | M1 onwards |
| Replay Buffer | `clipped-session` with segment writing in `clipped-muxer` | M3 |
| Audio Router | `clipped-audio` | M1–M2 |
| Encoder Manager | `clipped-encoder` | M1 |
| Event/Highlight Engine | `clipped-events` (vocabulary), `clipped-session` (rules) | M8–M10 |
| Media Library | `clipped-library` | M6 |
| Storage Manager | `clipped-storage` (persistence); policy undecided | M6, M12 |
| Export Engine | not yet created; `clipped-muxer` for remux | M11 |
| Plugin Manager | `clipped-plugins` | M9 |
| Game/Screen Capture | `clipped-capture` | M1 |
| Audio Capture | `clipped-audio` | M1–M2 |
| Hardware Encoder | `clipped-encoder` | M1 |
| Media Muxer | `clipped-muxer` | M1 |
| Platform APIs | `clipped-windows` | M1 |

Some cells deserve explanation. The Replay Buffer and the Session Manager are
not separate crates because the buffer is a mode of a recording session rather
than an independent subsystem: it needs the same capture, encode and mux
pipeline and differs only in what it keeps (SPEC.md section 16). The Export
Engine has no crate yet because nothing exports; creating an empty crate for it
now would be speculative structure, and the M11 issues that build it will place
it. Storage policy — quotas, retention, favourite protection — is listed as
undecided for the same reason: the mechanism (SQLite, on-disk layout) is clearly
`clipped-storage`, but no crate's documented remit claims the policy, and
`clipped-library` explicitly claims indexing, search, favourites and tags
instead. Where it lives is an M12 decision that
[issue #93](https://github.com/wildware-uk/clipped/issues/93) and
[issue #111](https://github.com/wildware-uk/clipped/issues/111) will make.

### Dependency direction

The crates are layered, and a crate may depend only on crates in a strictly
lower layer. The layer table lives in the "Dependency direction" section of
[README.md](../README.md) and is deliberately not duplicated here — one copy is
already one more than the number that can drift.

The rule is not a convention. `tests/integration/tests/workspace_layering.rs`
reads the real dependency graph from `cargo metadata` and fails if a dependency
points sideways or upwards, or if a new crate is added without being placed in
a layer. Adding a crate therefore means deciding, explicitly and in review,
where it sits.

Platform code stays at the bottom: Windows APIs are reached through
`clipped-windows`, or through a `windows/` submodule of the crate that owns the
behaviour, and never from unrelated modules (AGENTS.md section 5). A future
Linux port then has a marked surface to reimplement rather than a search
problem.

### What exists today

Being blunt about it, because the gap between this section and the ones above is
large:

- The workspace, the eleven `clipped-*` crates, the recorder binary, the
  desktop and web placeholders, and the four test suites exist.
- Most crates under `crates/` contain **module documentation only**: no types,
  no functions and no capture code. Three are further on. `clipped-logging` has
  the subscriber setup and the typed logging context. `clipped-capture` has the
  capture backend interface and the selection policy — the trait a backend
  implements, the frame and timestamp vocabulary, and the pure function that
  picks a backend and reports the choice — but no backend implements it, so
  nothing in this repository can produce a frame yet. `clipped-windows` has
  window and monitor enumeration and the rules that resolve a user's selector to
  one window, which is where a capture target comes from. Each
  `lib.rs` states that crate's responsibilities, what it is explicitly not
  responsible for, and where it sits in the stack; those doc comments are the
  authoritative statement of a crate's remit and this document defers to them.
- `apps/recorder` has a command line: `record` parses and validates every
  argument and installs its Ctrl+C handler, then reports that this build has no
  capture engine and exits 3; `list-windows` lists what could be captured; and
  `capabilities` reports the graphics adapters and encoders it found. See
  [recorder-cli.md](recorder-cli.md).
- `apps/desktop` and `packages/` are README placeholders.
- The tests assert the behaviour that exists: the workspace layering test,
  `clipped-logging`'s unit and integration tests, `clipped-capture`'s tests for
  the selection policy and the frame and timestamp types, and
  `clipped-windows`' tests for target selection and for enumeration against
  windows the test creates. None of them captures, encodes or writes anything,
  because nothing yet can.

Everything else in this document is a plan with an issue number attached.

## How to run it

```text
cargo build --workspace
cargo run -p clipped-recorder
```

With no arguments that prints the help and exits 2. It has three subcommands.
`list-windows` lists the windows that could be captured, and works today.
`capabilities` reports the graphics adapters and encoders it found, which is
detection rather than encoding
([encoder-capabilities.md](encoder-capabilities.md)). `record` validates its
arguments and then reports that this build cannot record, because the capture
engine does not exist yet. The first useful invocation — capture a window,
encode it, produce a playable MKV — is the M1 milestone:

```text
clipped-recorder capabilities
clipped-recorder record --window <window>
```

The desktop application is not runnable and will not be until the M5 scaffold
([issue #48](https://github.com/wildware-uk/clipped/issues/48)). When it is, it
is started separately and connects to the recorder; there is no combined "run
the app" command by design, because in production the two processes have
independent lifetimes.

Toolchain and platform prerequisites are in [prerequisites.md](prerequisites.md).

## How to test it

The four commands every change must pass:

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace
cargo test --workspace
```

There is no CI yet — [issue #4](https://github.com/wildware-uk/clipped/issues/4)
sets up the Windows workflow — so today those four commands are run by hand
before opening a pull request. `cargo test --workspace` is intended to be the
pull-request gate once #4 lands. It must stay fast, deterministic and runnable
on a machine with no GPU, no game and no audio hardware, which is why the suites
are split:

| Suite | Contains | Intended for CI (#4) |
| --- | --- | --- |
| Unit tests, in each crate | Isolated logic: replay ranges, cleanup rules, configuration resolution, game matching | yes |
| `tests/integration` | Workspace-wide invariants and subsystem interaction: encoder + muxer, session + database | yes |
| `tests/media` | The harness every media-producing crate validates its output with, and its own tests against media that is broken | yes |
| `tests/capture` | Real capture against controlled test applications | no — needs GPU and display |
| `tests/audio` | Recording known tone generators and asserting the tracks stay isolated | no — needs audio hardware |
| `tests/performance` | Benchmarks and soak tests against the SPEC.md section 38 targets | no — long-running |

Two rules that matter more here than in most projects. Media output is not
trusted because the encoder and muxer returned success: generated files are
inspected — container opens, expected streams present, timestamps monotonic,
A/V drift acceptable — by `tests/media`, the one harness every crate that writes
a file uses, and `ffprobe` is fair game in tests (AGENTS.md section 22). And
audio isolation is proved with generated tones at known frequencies rather than
by playing Spotify and listening, because the assertion has to be
machine-checkable and repeatable (AGENTS.md section 26).

## Assumptions

These hold across the system. Where one stops holding, the architecture, and
not just some code, needs revisiting.

- **Windows first.** Windows 11 and modern Windows 10 (SPEC.md section 3), with
  a caveat: the process-scoped loopback API that separate audio tracks depend on
  is documented from build 20348, which no consumer Windows 10 release reaches.
  On current information separated audio is Windows 11 only and Windows 10 falls
  back to a single mixed track; see
  [ADR 0003](adr/0003-process-specific-audio-capture.md). Linux is left room
  for by the layering, but no Linux implementation is planned.
- **A GPU that can encode video.** Hardware encoding is the normal path;
  software encoding exists as a fallback, not as an equal option
  (SPEC.md section 9).
- **Local-first, and offline.** No account, no cloud service, no telemetry. New
  network communication is never introduced silently (AGENTS.md section 14,
  SPEC.md section 39). See [privacy.md](privacy.md).
- **The user's files stay the user's files.** Recordings are ordinary media
  files in a documented container; the database holds references and metadata,
  never media blobs. Uninstalling Clipped must not cost anyone a recording
  (AGENTS.md sections 31 and 32).
- **Everything external fails.** Games crash, GPU drivers reset, encoders
  disappear, audio devices are unplugged mid-session, drives fill and monitors
  vanish. Recovery is designed per case rather than assumed
  (AGENTS.md section 16, SPEC.md section 35).
- **The recorder runs for days.** It is a background process with a long
  lifetime, so leaked handles, encoder sessions and GPU textures are serious
  bugs rather than untidiness (AGENTS.md sections 58 and 59).
- **Game integrations never touch a game's memory.** Official APIs, telemetry,
  logs and Game State Integration only. No injection, nothing that could look
  like a cheat to an anti-cheat system (AGENTS.md section 34).

## Subsystem documentation

Complex subsystems get their own document under `docs/` (AGENTS.md section 7).
This document covers the system; those cover one area each in the depth a
contributor working in it needs.

| Document | Covers | Filled in by |
| --- | --- | --- |
| [capture-pipeline.md](capture-pipeline.md) | Frame capture, backend selection and fallback, the capture clock, the path from frame to encoded packet | M1 |
| [encoder-capabilities.md](encoder-capabilities.md) | Adapter and encoder detection, what is measured against what is inferred, the capability cache, what "Automatic" chooses | M1 |
| [muxing.md](muxing.md) | Writing MKV, the track layout, timestamp handling and monotonicity, and what a recording killed mid-write costs | M1 |
| [av-sync.md](av-sync.md) | Which clock a recording is timed against, how every source is expressed against it, what happens on a gap or a step, and the measured drift | M1 |
| [audio-routing.md](audio-routing.md) | Per-source capture, application-to-track routing, drift correction, the compatibility mix | M2 |
| [replay-buffer.md](replay-buffer.md) | The rolling segmented buffer, retention and clip construction | M3 |
| [plugin-api.md](plugin-api.md) | The `HighlightProvider` contract, plugin discovery and supervision, event translation | M9 |

All but [capture-pipeline.md](capture-pipeline.md),
[encoder-capabilities.md](encoder-capabilities.md), [muxing.md](muxing.md) and
[av-sync.md](av-sync.md) are stubs today, stating what they will cover and which
milestone writes them. `capture-pipeline.md` is
written as far as the code goes: the capture backend interface and the selection
policy exist, so the interface, the ownership and threading rules, the timestamp
model and the selection policy are documented there, and the sections that would
describe an unwritten encoder path are listed at the end of it as still to be
written. `encoder-capabilities.md` covers detection, which is written, and says
plainly what detection cannot tell you until an encoder backend exists.
`muxing.md` covers the container writer, which is written, and ends with what
has not been exercised because nothing yet produces the packets for it.
`av-sync.md` decides which clock a recording is timed against and records the
drift measured between the two capture paths that exist — video and system audio
— over a thirty-minute run; it names what it deliberately leaves to M2, which is
correcting that drift rather than measuring it. The rest
stay stubs on purpose: describing a capture pipeline that has not been written
produces documentation that is wrong on the day it is committed.

Supporting documents that are not subsystems:

- [prerequisites.md](prerequisites.md) — toolchain and platform requirements.
- [privacy.md](privacy.md) — the privacy and network access policy.
- [logging.md](logging.md) — structured logging and log configuration.
- [adr/](adr/) — architecture decision records.

### Keeping it true

Documentation is updated in the change that alters the behaviour, not
afterwards. A pull request that changes how a subsystem behaves and leaves its
document describing the old behaviour is incomplete, in the same way that one
leaving a test failing is incomplete (AGENTS.md sections 7 and 52).

In practice:

- Change behaviour, update the subsystem document in the same pull request.
- Change how crates relate to each other, update this document and the layer
  table in README.md.
- Make a decision that constrains future work, write an ADR.
- Replace a stub with real content in the milestone that builds the subsystem.
  The stub is a placeholder for a document, not a substitute for one.

Documentation that describes behaviour the code does not have is worse than no
documentation, because it is trusted. Deleting a paragraph that has gone stale
is always allowed.

## Architecture decision records

Decisions that constrain later work are recorded in [adr/](adr/) with their
context, the alternatives that were genuinely considered, and the consequences
accepted. They are not a changelog: an ADR records why a door was closed, so
that reopening it is a deliberate act.

| ADR | Decision |
| --- | --- |
| [0001](adr/0001-mkv-archival-container.md) | MKV is the archival recording container |
| [0002](adr/0002-separate-recorder-process.md) | The recorder runs as an independent process from the desktop UI |
| [0003](adr/0003-process-specific-audio-capture.md) | Process-specific audio capture is the basis for track separation |

[adr/0000-template.md](adr/0000-template.md) is the template. `adr/README.md`
describes when to write one and how they are numbered.
