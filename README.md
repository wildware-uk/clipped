# Clipped

An open-source automatic game recorder for Windows. Clipped detects the game
you launched, records it with hardware encoding into separate editable audio
tracks, and stops when the game does — without scenes, sources or manual
routing.

Clipped is local-first: no account, no cloud service and no telemetry.

## Status

Early development. Nothing is installable yet. See [SPEC.md](SPEC.md) for the
product this is being built towards, and the
[issue tracker](https://github.com/wildware-uk/clipped/issues) for what is
actually being worked on.

`cargo build --workspace` produces a `clipped-recorder` binary. It cannot record
yet — the recording engine is milestone M1 — but
`clipped-recorder capabilities` reports the graphics adapters and hardware
encoders it found on your machine
([docs/encoder-capabilities.md](docs/encoder-capabilities.md)).

Installation instructions and screenshots are pending a shippable build.

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

The desktop application is not buildable yet: `apps/desktop` and `packages/`
are placeholders until milestone M5, and there is no npm workspace to install.

## Repository layout

```text
apps/
    recorder/       The recording process, which runs independently of the UI
    desktop/        The Tauri desktop application (placeholder until M5)
crates/            The Rust libraries the recorder is assembled from
packages/          TypeScript packages consumed by the desktop application
tests/             Capture, audio, integration and performance suites
docs/              Architecture, subsystem documentation and ADRs
```

## Architecture

The recorder is a native Rust process that owns capture, encoding, muxing and
session state. The desktop application is a client of that process over IPC, not
a host for it, so closing or crashing the UI cannot interrupt a recording.
[docs/architecture.md](docs/architecture.md) will describe the subsystems and
how they fit together, and significant decisions are to be recorded as ADRs
under `docs/adr/` (AGENTS.md section 48). Neither exists yet: both arrive with
issue #6, and until then the crate-level documentation in each `lib.rs` is the
authority.

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
| 2 | `clipped-muxer` | layers 0–1 |
| 3 | `clipped-replay`, `clipped-export` | layers 0–2 |
| 4 | `clipped-session` | layers 0–3 |
| 5 | `clipped-recorder` (binary), `clipped-workspace-tests` | layers 0–4 |
| 6 | `clipped-video-pattern` (test application) | layers 0–5 |
| 7 | `clipped-fullscreen-dx11` (test application) | layers 0–6 |

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
| [docs/testing.md](docs/testing.md) | The controlled test applications, and the capture tests that drive them |
| [docs/logging.md](docs/logging.md) | Log levels, log location and diagnostics |
| [docs/ipc.md](docs/ipc.md) | The protocol between the desktop application and the recorder |
| [docs/game-detection.md](docs/game-detection.md) | The game catalogue, its matching rules and how to add a game |
| [docs/editing.md](docs/editing.md) | What an edit is, the two kinds of time it is written in, where it is stored and how a document from an older build is read |
| [docs/storage-management.md](docs/storage-management.md) | What the library occupies, how accurate that figure is, and the limits configured against it |

The `docs/` entries are written under issues #3, #6, #8, #5, #23, #49, #42, #82
and #93 and are listed here so those tickets do not each have to edit this
table.

## Contributing

Work is tracked as GitHub issues grouped into milestones `M0` to `M14`. An issue
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
