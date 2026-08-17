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

One consequence is enforced in the repository, by three tests in
`tests/integration/tests/workspace_layering.rs`:

- `the_javascript_side_never_becomes_a_cargo_package` — the interface itself,
  `packages/ui` and `packages/shared` are deliberately **not** Cargo packages,
  and it fails if a `Cargo.toml` appears in any of them.
- `no_crate_depends_on_the_desktop_application` — `apps/desktop/src-tauri` *is*
  a Cargo package, the Tauri binary that owns the window, in a workspace of its
  own. It fails if any crate of this workspace names `clipped-desktop`.
- `the_desktop_application_links_nothing_of_this_workspace_but_the_protocol` —
  the same rule facing the other way, so the window reaches the recorder over
  IPC rather than by linking capture or encoding into its own process.
  `clipped-ipc` is the one exception: a webview cannot open a named pipe, so the
  Tauri host is the protocol's client, and the alternative would be a second
  implementation of the handshake inside the window
  ([ADR 0006](adr/0006-recorder-lifetime-and-supervision.md)).
- `the_crate_the_desktop_application_may_link_drags_nothing_else_in` — what
  makes that exception sound: `clipped-ipc` must depend on no other crate of the
  workspace, or the allowance would be linking the recording engine into the
  window transitively.

A second consequence is a convention held up by review, not by a check: no crate
under `crates/` may refer to UI concerns at all. `clipped-session`, the top
library layer, documents this in its own crate docs, but nothing fails if a
crate ignores it — a reviewer has to notice.

The IPC protocol between the two is a Windows named pipe carrying versioned JSON
messages: [ipc.md](ipc.md) specifies it and
[ADR 0005](adr/0005-named-pipe-control-protocol.md) records why the transport is
a pipe rather than a loopback socket. `clipped-recorder serve` speaks it against
real recording sessions. The desktop application does not drive it yet, because
the TypeScript view of the messages is
[issue #209](https://github.com/wildware-uk/clipped/issues/209); deciding that no
recorder is running and starting one is
[issue #106](https://github.com/wildware-uk/clipped/issues/106), also in M5. The
command line stays the way capture is driven without a UI, which is a reason to
keep it good rather than scaffolding to be removed.

### Module split

SPEC.md section 5 splits the system into an application service over a native
capture engine. That split is realised as the Cargo workspace: each module in
the specification maps to a crate, or to a part of a crate that owns the
surrounding responsibility.

| SPEC.md section 5 module | Lives in | Status |
| --- | --- | --- |
| Game Detector | `clipped-game-detection` | M4 |
| Session Manager, Capture Coordinator | `clipped-session` (`automatic` is the session manager) | M1 onwards |
| Replay Buffer | `clipped-replay`, attached to a recording by `clipped-session` | M3 |
| Audio Router | `clipped-audio` | M1–M2 |
| Encoder Manager | `clipped-encoder` | M1 |
| Event/Highlight Engine | `clipped-events` (vocabulary), `clipped-session` (rules) | M8–M10 |
| Media Library | `clipped-library` | M6 |
| Storage Manager | `clipped-storage` (persistence); `clipped-library::accounting` (measurement and limits) | M6, M12 |
| Export Engine | `clipped-export`; `clipped-edit` is the edit document it renders, `clipped-muxer` for remux | M11 |
| Plugin Manager | `clipped-plugins` | M9 |
| Game/Screen Capture | `clipped-capture` | M1 |
| Audio Capture | `clipped-audio` | M1–M2 |
| Hardware Encoder | `clipped-encoder` | M1 |
| Media Muxer | `clipped-muxer` | M1 |
| Platform APIs | `clipped-windows` | M1 |

Some cells deserve explanation. The Replay Buffer is its own crate but not its
own pipeline: it is a *second consumer of the encoder's packets*, so
`clipped-session` drains each packet into the Matroska writer and into the
buffer, and a recording with a replay buffer running encodes once (SPEC.md
section 16, [replay-buffer.md](replay-buffer.md)). It is a crate rather than a
module of `clipped-session` because retention, range selection and holding
segments against eviction are self-contained logic with no need of capture, and
because `clipped-session` is the top layer — nothing else could reuse it there.
The Export
Engine is `clipped-export`: the plan, the renderer and the progress and
cancellation model. Nothing depends on that crate yet — no binary runs an
export — so it is written and tested at the edge of the tree rather than wired
into one. What it renders is `clipped-edit`: the
non-destructive edit document — which recordings to play, which parts of them,
in which order, how loud each track is and what text goes over the picture
([editing.md](editing.md)) — together with the operations that change one, which
since [issue #84](https://github.com/wildware-uk/clipped/issues/84) are trim
start, trim end, split, delete section, undo and redo. It holds no exporter and
no editor. It sits at layer 0 beside `clipped-ipc` because both ends of the
application read a document, and it performs no file or database access at all,
so a clip cannot damage the recording it refers to. The Storage Manager is split
between two crates because the mechanism and the policy are different jobs. The
mechanism — SQLite, migrations, on-disk layout — is `clipped-storage`. The
policy that had no home was placed by
[issue #93](https://github.com/wildware-uk/clipped/issues/93): storage
*accounting* is `clipped-library::accounting`, because measuring what is on disk
and attributing it to games and sessions is a view over the library, which is
what `clipped-library` has claimed from the start
([storage-management.md](storage-management.md)). It measures and judges limits
and deletes nothing; acting on a breached limit is
[issue #111](https://github.com/wildware-uk/clipped/issues/111), which is where
the retention and favourite-protection rules will land.

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

- The workspace, the `clipped-*` crates listed in README.md's layer table, the
  recorder binary, the desktop application's shell, and the four test suites
  exist. (The count was written down here once and went stale; the layer table
  is the one place it is kept.)
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
- `apps/recorder` has a command line, and `record` records: it resolves the
  target, captures the window, encodes its frames and writes them into a
  Matroska file, and finishes that file when Ctrl+C stops it
  ([#126](https://github.com/wildware-uk/clipped/issues/126)). A recording has a
  video track and no audio track. `watch` does the same thing without being
  asked: it waits on the process watcher, matches what launched against the game
  catalogue, and records a game from its launch to its exit
  ([#46](https://github.com/wildware-uk/clipped/issues/46),
  [sessions.md](sessions.md)). `list-windows` lists what could be captured
  and `capabilities` reports the graphics adapters and encoders it found. See
  [recorder-cli.md](recorder-cli.md).
- `clipped-session` is the crate that joins them, and the only place in the
  workspace holding `clipped-capture`, `clipped-encoder` and `clipped-muxer` at
  once. It owns the recording loop and the thread split: capture and encoding
  share the calling thread, because a captured texture may not outlive the
  acquisition that produced it, and only encoded packets cross a bounded queue
  to the thread that writes the file — which is what keeps the capture thread
  off the filesystem (AGENTS.md section 20). `record_with_replay` additionally
  copies each packet into a `clipped-replay` buffer, so a rolling window of the
  last few minutes is there to save from; `clipped_replay::save_clip` turns a
  window of it into a file
  ([#37](https://github.com/wildware-uk/clipped/issues/37)), and
  `clipped_session::replay` is what a recording and a save meet through:
  `clipped-recorder replay` turns a buffer on and a hotkey asks for a clip
  ([#38](https://github.com/wildware-uk/clipped/issues/38)). It also owns
  the **session manager**: `clipped_session::automatic` joins
  `clipped-game-detection`'s watcher and catalogue to that recording loop, and is
  the policy that decides when a session starts, when it stops, what a fast
  restart or a second game or a suspend means, and what a session contains
  ([#46](https://github.com/wildware-uk/clipped/issues/46)). It is a state
  machine over watcher events and a wall-clock reading — it opens no window and
  starts no thread — so every one of those rules is tested without a game, a GPU
  or any waiting; `apps/recorder`'s `watch` is the driver that carries out what
  it decides. A session's record is a JSON sidecar beside its recordings, and
  M6's [#55](https://github.com/wildware-uk/clipped/issues/55) owns the real
  store. Per-game settings are a later milestone and are not there.
- `apps/desktop` is the application shell, and the supervision behind it: a
  Tauri 2 window hosting a React interface, with its layout, navigation, design
  tokens and accessibility baseline, drawn from `packages/ui` and typed by
  `packages/shared`. It runs — `npm run dev` opens the window — and it shows no
  data it does not have: six of the seven screens are written and the last,
  Trash, is the placeholder that names the issue building it
  (`apps/desktop/src/Shell.tsx`), and the recorder status block shows what its
  link with the recorder reports, which is one wording per link state and no
  guessing ([#106](https://github.com/wildware-uk/clipped/issues/106),
  [ADR 0006](adr/0006-recorder-lifetime-and-supervision.md)).
  [desktop-ui.md](desktop-ui.md) covers it.
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

With no arguments that prints the help and exits 2. It has four subcommands.
`list-windows` lists the windows that could be captured. `capabilities` reports
the graphics adapters and encoders it found, which is detection rather than
encoding ([encoder-capabilities.md](encoder-capabilities.md)). `record` captures
a window, encodes it and produces a playable MKV, and Ctrl+C stops it leaving a
finished file. `serve` is the shape the recorder runs in beside a UI: it listens
on the named pipe and takes its instructions over the protocol in
[ipc.md](ipc.md) instead of from arguments:

```text
clipped-recorder capabilities
clipped-recorder list-windows
clipped-recorder record --window <window>
clipped-recorder serve
```

The desktop application is an npm workspace at the repository root and is
started separately:

```text
npm install
npm run dev
```

There is no combined "run the app" command, by design: in production the two
processes have independent lifetimes. The window does not connect to the
recorder yet, because the IPC protocol
([issue #49](https://github.com/wildware-uk/clipped/issues/49)) is not written,
and it says so rather than pretending otherwise. `npm run dev` compiles the Rust
side, so the first run takes several minutes and needs the WebView2 runtime as
well as the recorder's toolchain; `npm run dev:web` opens the interface alone in
a browser and needs neither. [desktop-ui.md](desktop-ui.md) has the rest.

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
| [game-detection.md](game-detection.md) | The game catalogue and how a process is matched against it; watching for processes starting and stopping, why the source is a subscription rather than a poll, how a launcher and the game it starts become one launch, and what detection costs while nothing is happening | M4 |
| [sessions.md](sessions.md) | What a session is; how a launch becomes a recording; what happens on a crash, a fast restart, a second game and a suspend; the one capture mode this build has; and where a session is written down before M6's database exists | M4 |
| [search.md](search.md) | The local search language: its syntax, what each term means, every message a malformed query produces, the limits of its text matching, and how a database-backed executor consumes the parsed query | M6 |
| [waveforms.md](waveforms.md) | Per-track audio peaks for the timeline and the clip editor: what is stored and at what resolutions, the sidecar cache and its invalidation and cleanup rules, and where generation runs so that it cannot compete with a recording | M8 |
| [thumbnails.md](thumbnails.md) | The picture every screen that lists a recording shows for it: which frame is chosen and why it is not the first, the size and format stored, the sidecar cache and its invalidation and cleanup, how a missing one degrades, and the measured cost per thumbnail | M6 |
| [bookmarks.md](bookmarks.md) | Marking a moment while it is being recorded: when a bookmark is stamped and why it is not at the key press, how accurate that is, why nothing about it touches capture, and the sidecar it is written to | M8 |
| [plugin-api.md](plugin-api.md) | The `HighlightProvider` contract, plugin discovery and supervision, event translation | M9 |
| [ipc.md](ipc.md) | The recorder control protocol: transport, framing, the handshake, the compatibility policy, the commands and events, and the security a local endpoint does and does not promise | M5 |
| [desktop-ui.md](desktop-ui.md) | The window: the Tauri and React shell, its layout and navigation, the design tokens, the accessibility baseline, and why the Tauri crate is its own Cargo workspace | M5 |
| [editing.md](editing.md) | What an edit is and what it deliberately is not: the document model, the two kinds of time it is written in, why a cut is stored as its result, where a document lives, and how one written by an older build is read | M11 |

All but [capture-pipeline.md](capture-pipeline.md),
[encoder-capabilities.md](encoder-capabilities.md), [muxing.md](muxing.md),
[av-sync.md](av-sync.md), [desktop-ui.md](desktop-ui.md), [ipc.md](ipc.md),
[game-detection.md](game-detection.md), [sessions.md](sessions.md),
[editing.md](editing.md), [search.md](search.md), [waveforms.md](waveforms.md),
[thumbnails.md](thumbnails.md)
[bookmarks.md](bookmarks.md) and [editing.md](editing.md)
are stubs today, stating what they will cover and which
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
correcting that drift rather than measuring it. `desktop-ui.md` covers the shell
that exists and is explicit that no feature screen behind it does. `ipc.md` is a
specification rather than a description: it is the schema both ends are written
against, and it says which of the commands it defines this build refuses and
where each is being built. `game-detection.md` covers the two halves of
detection that are written — the game catalogue and the process watcher — and it
records what the watcher costs while idle, which is more than was expected and is
the reason for
[issue #230](https://github.com/wildware-uk/clipped/issues/230).
`sessions.md` covers what joins them: the session manager, every rule it applies,
and the sidecar a session is written to until M6's database exists. Both are
explicit about what is not built, which for sessions is three of the four capture
modes and every per-game setting. `editing.md` is now mostly description rather
than specification: the document model and every operation on it exist in
`clipped-edit`, and the export engine exists in `clipped-export`. What is still
specification is the joining up — nothing depends on `clipped-export`, so no
export runs, and the desktop editor screen cannot yet open a clip, draw a frame
or read a waveform ([issue #306](https://github.com/wildware-uk/clipped/issues/306)).
That is the shape the fourteen remaining M11 tickets are held to. `search.md`
is the same shape again: the query language, its parser and its matcher exist in
`clipped-library`, so the syntax, every error message and the measured cost of
matching are written down, and the document is explicit that nothing indexes a
real library yet — that is M6's issues #55 and #56, and the document says what
they have to consume. `waveforms.md` covers the peak generator, which is written, and is explicit that
nothing draws its output and nothing hosts its background worker yet — the
timeline and the clip editor are the consumers, and neither exists.
`thumbnails.md` is the same shape for the picture beside a recording: the frame
rule, the sidecar cache and the background worker exist in
`clipped-library::thumbnail` and the measured cost per thumbnail is written down,
and it says plainly that the library screen that would draw one is #52 and that
nothing hosts the worker. The rest
stay stubs on purpose: describing a capture pipeline that has not been written

The rest stay stubs on purpose: describing a capture pipeline that has not been written
produces documentation that is wrong on the day it is committed.

Supporting documents that are not subsystems:

- [prerequisites.md](prerequisites.md) — toolchain and platform requirements.
- [packaging.md](packaging.md) — what the installer puts beside the window, and why.
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
| [0004](adr/0004-ffmpeg-dependency-strategy.md) | FFmpeg is a pinned LGPL build, linked dynamically through a sys binding |
| [0005](adr/0005-named-pipe-control-protocol.md) | A named pipe carries the control protocol between the UI and the recorder |
| [0006](adr/0006-recorder-lifetime-and-supervision.md) | The desktop application starts a detached recorder and supervises it, and neither stops the other |

[adr/0000-template.md](adr/0000-template.md) is the template. `adr/README.md`
describes when to write one and how they are numbered.
