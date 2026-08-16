# Clipped

An open-source automatic game recorder for Windows. Clipped detects the game
you launched, records it with hardware encoding into separate editable audio
tracks, and stops when the game does — without scenes, sources or manual
routing.

Clipped is local-first: no account, no cloud service and no telemetry.

## Status

Early development. **There is no release to download** — no published build and
nothing signed — and a build from source is how you run Clipped today. There
will not be one until every milestone is finished, and the first will be
`v1.0.0`; [docs/releasing.md](docs/releasing.md) is that rule, and the release
build refuses to produce anything until it holds. What
follows is what that build does. See [SPEC.md](SPEC.md) for the product this is
being built towards, and the
[issue tracker](https://github.com/wildware-uk/clipped/issues) for what is being
worked on.

**The recorder records.** `clipped-recorder record --process <name>` captures a
window through Windows Graphics Capture, encodes it with the best hardware
encoder it finds, and writes Matroska with the system audio and the microphone
as separate uncompressed tracks. `clipped-recorder watch` does the same
automatically when a game in the catalogue launches, and stops when it exits.
`clipped-recorder capabilities` reports the adapters and encoders it found
([docs/encoder-capabilities.md](docs/encoder-capabilities.md)), and
`clipped-recorder list-windows` shows what can be captured.

**The desktop window runs**, from `npm run dev`. It starts and stops a recording,
lists what has been recorded, and **plays one** — with sound, and with any of its
audio tracks ([#304](https://github.com/wildware-uk/clipped/issues/304),
[ADR 0010](docs/adr/0010-what-the-webview-plays.md)). It cannot yet export one
([#322](https://github.com/wildware-uk/clipped/issues/322)), and the library
draws no thumbnails yet. Recordings land in `%USERPROFILE%\Videos\Clipped\` and
any player that handles Matroska will open them.

**`npm run build:app` builds an installer that records.** It carries the
recorder and the FFmpeg libraries beside the window, so an installed Clipped
finds and starts its recorder with nothing set by hand
([docs/packaging.md](docs/packaging.md)). It is not a shippable build: it is
unsigned, and it does not yet carry the licence texts and third-party notices a
distributed copy owes ([#123](https://github.com/wildware-uk/clipped/issues/123)).

Screenshots are pending.

## Supported platforms

Windows 11 and modern Windows 10, on x86_64. That is the only platform Clipped
builds for today, because capture, process-specific audio and hardware encoding
are all reached through Windows APIs.

Linux is not supported and is not currently being worked on. Platform-specific
code is kept in `clipped-windows` or in a `windows/` submodule of the crate that
owns the behaviour (AGENTS.md section 5, and the module documentation in
`crates/windows/src/lib.rs`), so that a second platform remains possible later
without unpicking the whole engine. SPEC.md section 3 sets Windows as the
initial target and asks that the architecture leave room for Linux.

## Building from source

You need:

- Rust, stable channel, 1.85 or newer, installed through
  [rustup](https://rustup.rs) with the `x86_64-pc-windows-msvc` target.
- The MSVC build tools and Windows SDK that the `msvc` target links against —
  in practice, Visual Studio Build Tools with the "Desktop development with
  C++" workload.
- LLVM, for the `libclang.dll` that generates the FFmpeg bindings at build time
  (`winget install LLVM.LLVM`).

[docs/prerequisites.md](docs/prerequisites.md) has the full list, including the
versions the project is tested against.

```text
git clone https://github.com/wildware-uk/clipped.git
cd clipped
powershell -ExecutionPolicy Bypass -File scripts/fetch-ffmpeg.ps1
cargo build --workspace
cargo test --workspace
```

The one step beyond the toolchain is that FFmpeg build. Clipped links against a
pinned, LGPL-only FFmpeg rather than vendoring or compiling one, so the script
downloads it and verifies its checksum into the gitignored `third-party/ffmpeg/`
— 67 MB to download and 168 MB on disk, plus 409 MB of DLLs copied beside the
binaries in `target/debug`. Nothing has to be set afterwards and no new shell is
needed: the committed `.cargo/config.toml` is what points Cargo at the result,
and an environment variable of the same name still overrides it if you build
against an FFmpeg of your own. [docs/ffmpeg.md](docs/ffmpeg.md) covers it, and
`scripts/check-prerequisites.ps1` reports whether it has been done.

## Development setup

Beyond the build, the checks a change is expected to pass locally are:

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace
cargo test --workspace
```

Formatting is not a matter of taste here. `rustfmt.toml` pins the formatter
settings, `.editorconfig` covers the non-Rust files, and `.gitattributes`
normalises line endings to LF so that `cargo fmt --check` behaves the same on
Windows as it does in CI.

The workspace lints `missing_docs`, `missing_debug_implementations`,
`unreachable_pub` and `clippy::undocumented_unsafe_blocks` are enabled in
`Cargo.toml`, so anything public needs a doc comment.

The desktop application is an npm workspace at the repository root, covering
`apps/desktop` and `packages/*`. The Node version is pinned in `.nvmrc`, which is
what CI installs from:

```text
npm install
npm run dev      # Vite plus the Tauri window, rebuilt on change
npm test         # the desktop and package suites
npm run lint     # eslint, prettier and tsc --noEmit
```

`npm run dev` builds the Rust side too, so the first run takes as long as a
`cargo build` of the desktop crate. The window loads its frontend from Vite on
port 5173 in development, so it needs that server — which `npm run dev` starts
for you.

## Repository layout

```text
apps/
    recorder/       The recording process, which runs independently of the UI
    desktop/        The Tauri desktop application, and its React frontend
crates/            The Rust libraries the recorder is assembled from
plugins/           The highlight plugins shipped with Clipped, one executable each
packages/          TypeScript packages consumed by the desktop application
tests/             Capture, audio, integration and performance suites
docs/              Architecture, subsystem documentation and ADRs
```

## Architecture

The recorder is a native Rust process that owns capture, encoding, muxing and
session state. The desktop application is a client of that process over IPC, not
a host for it, so closing or crashing the UI cannot interrupt a recording.
[docs/architecture.md](docs/architecture.md) describes the subsystems and how
they fit together, and significant decisions are recorded as ADRs under
[docs/adr/](docs/adr/) (AGENTS.md section 48). The crate-level documentation in
each `lib.rs` remains the authority on what an individual crate is and is not
responsible for.

### Dependency direction

The crates are layered. **A crate may depend only on crates in a lower layer.**
This is not a convention to be remembered — it is asserted against the real
dependency graph by `tests/integration/tests/workspace_layering.rs`, which
fails if a dependency points sideways or upwards, or if a new crate is added
without being placed in a layer.

| Layer | Crates | Depends on |
| --- | --- | --- |
| 0 | `clipped-windows`, `clipped-events`, `clipped-storage`, `clipped-logging`, `clipped-ipc`, `clipped-hotkeys`, `clipped-edit`, `clipped-media-validation`, `clipped-ffmpeg-runtime` | nothing in this workspace |
| 1 | `clipped-capture`, `clipped-audio`, `clipped-encoder`, `clipped-library`, `clipped-game-detection`, `clipped-plugins`, `clipped-waveform` | layer 0 |
| 2 | `clipped-muxer`, `clipped-league-plugin`, `clipped-cs2-plugin`, `clipped-dota2-plugin` (plugins, see below) | layers 0–1 |
| 3 | `clipped-replay`, `clipped-export` | layers 0–2 |
| 4 | `clipped-session` | layers 0–3 |
| 5 | `clipped-recorder` (binary), `clipped-workspace-tests` | layers 0–4 |
| 6 | `clipped-video-pattern` (test application) | layers 0–5 |
| 7 | `clipped-fullscreen-dx11` (test application) | layers 0–6 |

`clipped-league-plugin`, `clipped-cs2-plugin` and `clipped-dota2-plugin` are in
`plugins/`, not `crates/`: they are game integrations
([docs/plugin-api.md](docs/plugin-api.md)), which are executables the recorder
*starts* rather than crates anything links. Layering is not what governs them,
and no layer could: whichever one they sat at, every layer above would be free
to name them, and every layer below would be free to be named by them. The
two rules that actually apply are asserted directly, by
`nothing_in_the_workspace_depends_on_a_plugin` and
`a_plugin_names_only_the_plugin_contract_and_the_event_vocabulary` in
`tests/integration/tests/workspace_layering.rs`:

- **Nothing may depend on a plugin**, of any kind, including for tests. A plugin
  is reached by starting a process.
- **A plugin may name only `clipped-plugins` and `clipped-events`** — the
  contract and the vocabulary — plus `clipped-game-detection` and
  `clipped-logging`, which answer *where a game is installed* and *where
  Clipped's own directory is* and know nothing about a recording. A plugin that
  reached `clipped-session` would be a game's protocol inside the recording
  engine, which is what the process boundary exists to prevent. The allowlist is
  in `workspace_layering.rs`, one entry at a time with a reason each, and is
  deliberately not "anything at a lower layer".

Their layer is therefore only what the layer table needs in order to cover
every member, and every plugin sits on the same one so that adding the next is a
line in two places rather than a decision.

Layers 6 and 7 are the controlled test applications in `test-apps/`, which
capture tests point at instead of an installed game
([docs/testing.md](docs/testing.md)). They are at the top of the stack because
nothing in the product may depend on one.

`clipped-replay` sits above `clipped-muxer` rather than beside it, which is a
change made by [issue #37](https://github.com/wildware-uk/clipped/issues/37) and
worth stating plainly. A replay buffer exists to produce a clip, a clip is a
file, and the muxer is what writes files — so `clipped_replay::save_clip` drives
`MkvWriter` over the packets a lease holds rather than containing a second
Matroska implementation, and rather than leaving every caller to write the same
loop (AGENTS.md section 55). The dependency points one way only: the muxer still
knows nothing about a buffer, and a recording is written by exactly the code a
clip is.

`clipped-export` sits beside it rather than above it, and for the same reason
it is above the muxer at all. An export writes a file, `MkvWriter` is what
writes files here, and a cut-only edit is written from the coded packets the
recording already holds — so `clipped_export::export` drives the same writer a
recording and a replay clip are written by
([docs/exporting.md](docs/exporting.md)). It has nothing to do with
`clipped-replay` and does not depend on it. It does name `rusty_ffmpeg`
directly, to *read* the recordings an edit refers to: `clipped-muxer` writes
containers and remuxes whole files, and its container reader is private to it,
so there is no lower-layer route to reading a recording's packets. That is the
case ADR 0004's amendment ([issue #155](https://github.com/wildware-uk/clipped/issues/155))
permits and that `clipped-waveform` already relies on.

`clipped-ipc` sits at layer 0 for a reason worth stating: it is the protocol
the desktop application drives the recorder through
([docs/ipc.md](docs/ipc.md)), so both ends of the connection have to be able to
use it. It therefore depends on no other crate in this workspace, and it holds
no application logic — the recorder plugs its own subsystems into it, and a
client that only wants to send commands does not link the recording engine to
do so.

`clipped-hotkeys` sits there for the same shape of reason. A global hotkey is a
key combination plus a handler the *caller* supplies
([docs/hotkeys.md](docs/hotkeys.md)), so the dependency points into it: the
process that owns a recording session registers a handler, and a hotkey crate
that reached back into the session could be linked by neither the recorder nor
the desktop application.

`clipped-edit` sits at layer 0 for the reason `clipped-ipc` does: an edit
document is read by *both* ends of the application
([docs/editing.md](docs/editing.md)). The editor in the desktop process shows
one, the recorder process exports it, and `clipped-storage` keeps it as text
without understanding it — so a document model that reached into the recording
engine could not be linked by the half of the system that only wants to draw a
timeline. It holds no application logic and performs no file or database access
at all, which is also the cheapest guarantee that editing cannot damage a
recording: a crate that cannot open a file cannot rewrite one.

`clipped-media-validation` (`tests/media`) is a test-only package too, but it
sits at the *bottom* rather than the top, and deliberately: it is what every
crate that writes media checks its output with, so it has to be reachable from
all of them as a dev-dependency. It is never published, never linked into the
recorder, and depends on nothing in this workspace — which is what makes
sitting at layer 0 sound rather than convenient.

`clipped-ffmpeg-runtime` (`crates/ffmpeg-runtime`) sits at layer 0 for the same
kind of reason. It copies the pinned FFmpeg DLLs beside the binaries a build
produces, so that nothing has to be on `PATH`, and it is named only by the
`[build-dependencies]` of the crates that link FFmpeg — `clipped-muxer` and
`clipped-encoder`. No binary links it, and it depends on nothing at all.

`clipped-logging` owns where diagnostics go and how much is recorded: it
installs the process-wide `tracing` subscriber, resolves the log level from the
environment and a configuration file without a rebuild, writes bounded rotating
files under a documented per-user directory, and defines the standard context
fields as types rather than loose strings. It deliberately does not own logging
itself. The rule is that a crate wanting to emit events adds
`tracing.workspace = true` and calls the `tracing` macros directly, so no
diagnostic is routed through a Clipped-specific wrapper. `clipped-logging`,
`clipped-encoder` and `clipped-recorder` have taken that dependency so far; a
crate takes it in the change that gives it something to log, and most of
`crates/` is still documentation-only stubs.

It also answers where Clipped's per-user directory is, for the crates that keep
a file in it — the encoder's capability cache and the game catalogue's user
overlay as well as the logs. That is not a diagnostics concern; it lives here
because layer 0 is the lowest place all three can see, and one function saying
where the directory is beats three copies that can drift apart (issue #228).
`clipped-game-detection` takes the dependency for that reason alone.

Two rules matter most:

- **Nothing depends on the user interface.** `apps/desktop` and `packages/` are
  deliberately not Cargo packages, so the desktop application cannot be linked
  into the recorder even by accident. The UI is a client of the recorder over
  IPC, and closing or crashing it must not interrupt a recording.
- **Platform code stays at the bottom.** Windows APIs are reached through
  `clipped-windows` or through a `windows/` submodule of the crate that owns
  the behaviour, never scattered through unrelated modules.

Each crate's `lib.rs` documents what it is responsible for, what it explicitly
is not responsible for, and where it sits in this stack.

## Documentation

| Document | What it covers |
| --- | --- |
| [SPEC.md](SPEC.md) | The product being built, and the milestone order |
| [AGENTS.md](AGENTS.md) | Engineering standards every contribution is held to |
| [CONTRIBUTING.md](CONTRIBUTING.md) | Workflow, branches, commits, definition of done |
| [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) | Expected conduct and how to report a problem |
| [docs/prerequisites.md](docs/prerequisites.md) | Toolchains, SDKs and driver expectations |
| [docs/architecture.md](docs/architecture.md) | Subsystems, boundaries and ADRs |
| [docs/privacy.md](docs/privacy.md) | What leaves the machine, and what never does |
| [docs/licensing.md](docs/licensing.md) | What a release has to carry, and the LGPL obligations FFmpeg brings |
| [docs/releasing.md](docs/releasing.md) | When a release may happen, what a version means, and the gates a tag has to pass |
| [docs/packaging.md](docs/packaging.md) | What the installer carries beside the window, and how it gets there |
| [docs/testing.md](docs/testing.md) | The controlled test applications, and the capture tests that drive them |
| [docs/logging.md](docs/logging.md) | Log levels, log location and diagnostics |
| [docs/ipc.md](docs/ipc.md) | The protocol between the desktop application and the recorder |
| [docs/game-detection.md](docs/game-detection.md) | The game catalogue, its matching rules and how to add a game |
| [docs/editing.md](docs/editing.md) | What an edit is, the two kinds of time it is written in, where it is stored and how a document from an older build is read |
| [docs/storage-management.md](docs/storage-management.md) | What the library occupies, how accurate that figure is, and the limits configured against it |

Subsystem documents, each written beside the code it describes:

| Document | What it covers |
| --- | --- |
| [docs/capture-pipeline.md](docs/capture-pipeline.md) | The capture backends, and how one is chosen |
| [docs/encoder-pipeline.md](docs/encoder-pipeline.md) | Encoder selection, configuration and fallback |
| [docs/muxing.md](docs/muxing.md) | Containers, track ordering and what a recording carries |
| [docs/av-sync.md](docs/av-sync.md) | The clocks, and how audio is kept against video |
| [docs/audio-routing.md](docs/audio-routing.md) | Per-process audio scoping and the device graph |
| [docs/replay-buffer.md](docs/replay-buffer.md) | Retained segments, and saving from them |
| [docs/sessions.md](docs/sessions.md) | Sessions, and recording a game without being told to |
| [docs/library.md](docs/library.md) | The index, its reconciliation against the disk, and search |
| [docs/thumbnails.md](docs/thumbnails.md) · [docs/waveforms.md](docs/waveforms.md) | Generation and caching of each |
| [docs/screenshots.md](docs/screenshots.md) | Taking a still of the game on a hotkey, while it is being played |
| [docs/bookmarks.md](docs/bookmarks.md) | Marking a moment while the recording is being made |
| [docs/highlights.md](docs/highlights.md) | Turning events into clips |
| [docs/plugin-api.md](docs/plugin-api.md) | The event model, and the contract a highlight plugin meets |
| [docs/exporting.md](docs/exporting.md) | The copy-or-re-encode decision, and what an export guarantees |
| [docs/configuration.md](docs/configuration.md) | Settings, and how per-game overrides resolve |
| [docs/desktop-ui.md](docs/desktop-ui.md) | The window's screens and the design system |
| [docs/hotkeys.md](docs/hotkeys.md) | Global hotkeys, and what they cannot do |
| [docs/diagnostics.md](docs/diagnostics.md) | Metrics, the support bundle and what it redacts |
| [docs/recorder-cli.md](docs/recorder-cli.md) | Every recorder subcommand and its arguments |
| [docs/search.md](docs/search.md) | The query language for searching the library, and what of it runs today |
| [docs/storage.md](docs/storage.md) | The database behind the library: its schema, and how it is migrated |

## Contributing

Work is tracked as GitHub issues grouped into milestones `M0` to `M15`. An issue
is the source of truth for its own scope and acceptance criteria; `SPEC.md` is a
reference document, not a task list.

[CONTRIBUTING.md](CONTRIBUTING.md) explains the workflow, branch and commit
naming, and what counts as done. Engineering standards are in
[AGENTS.md](AGENTS.md) and apply to human and automated contributors alike.

Participation is covered by the [Code of Conduct](CODE_OF_CONDUCT.md).

## Licence

Mozilla Public License 2.0. The full text is in [LICENSE](LICENSE), and
`Cargo.toml` declares `license = "MPL-2.0"` for every crate in the workspace.

MPL-2.0 is file-level copyleft: changes to Clipped's own source files stay open,
while the licence still permits linking against LGPL FFmpeg and against the
permissive Rust ecosystem that a full GPL would have ruled out.

The practical consequence for contributors is that dependencies must be
MPL-2.0-compatible. MIT, Apache-2.0, BSD and ISC licensed crates are fine, as
are MPL-2.0 ones; GPL-only dependencies are not, and a dependency with unclear
licensing should not be added at all. See
[CONTRIBUTING.md](CONTRIBUTING.md#licensing-and-dependencies).

Third-party code that lives in this repository — the encoder bindings generated
from NVIDIA's, AMD's and Intel's headers, all MIT — is listed with the notices
its licence requires in
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).

Distributing Clipped is a separate question again, because a release ships
FFmpeg's LGPL v3 libraries and several hundred Rust crates that this repository
does not contain. What a release has to carry, which of it exists today, and how
the FFmpeg relinking permission was tested are in
[docs/licensing.md](docs/licensing.md).
