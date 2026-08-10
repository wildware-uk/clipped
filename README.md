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
| 0 | `clipped-windows`, `clipped-events`, `clipped-storage`, `clipped-logging` | nothing in this workspace |
| 1 | `clipped-capture`, `clipped-audio`, `clipped-encoder`, `clipped-library`, `clipped-game-detection`, `clipped-plugins` | layer 0 |
| 2 | `clipped-muxer` | layers 0–1 |
| 3 | `clipped-session` | layers 0–2 |
| 4 | `clipped-recorder` (binary), `clipped-workspace-tests` | layers 0–3 |

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
| [docs/logging.md](docs/logging.md) | Log levels, log location and diagnostics |

The four `docs/` entries are written under issues #3, #6, #8 and #5 and are
listed here so those tickets do not each have to edit this table. The links
resolve once milestone M0 is complete.

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

Third-party code that lives in this repository — currently the NVENC bindings
generated from NVIDIA's MIT-licensed `nvEncodeAPI.h` — is listed with the
notices its licence requires in
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
