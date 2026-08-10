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

`cargo build --workspace` produces a `clipped-recorder` binary, but it has no
capture commands yet; the recording engine is milestone M1.

Installation instructions and screenshots are pending a shippable build.

## Supported platforms

Windows 11 and modern Windows 10, on x86_64. That is the only platform Clipped
builds for today, because capture, process-specific audio and hardware encoding
are all reached through Windows APIs.

Linux is not supported and is not currently being worked on. Platform-specific
code is kept in `clipped-windows` or in a `windows/` submodule of the crate that
owns the behaviour, so that a second platform remains possible later without
unpicking the whole engine (SPEC.md section 3).

## Building from source

You need:

- Rust, stable channel, 1.85 or newer, installed through
  [rustup](https://rustup.rs) with the `x86_64-pc-windows-msvc` target.
- The MSVC build tools and Windows SDK that the `msvc` target links against —
  in practice, Visual Studio Build Tools with the "Desktop development with
  C++" workload.

[docs/prerequisites.md](docs/prerequisites.md) has the full list, including the
versions the project is tested against.

```text
git clone https://github.com/wildware-uk/clipped.git
cd clipped
cargo build --workspace
cargo test --workspace
```

No environment variables, local configuration or generated files are required
to build: a clean clone plus the toolchain above is enough.

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
[docs/architecture.md](docs/architecture.md) describes the subsystems and how
they fit together; architecture decisions are recorded as ADRs under
`docs/adr/`.

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
| 4 | `clipped-recorder` (binary) | layers 0–3 |

`clipped-logging` is the shared structured-logging setup: the tracing
subscriber, the standard context fields and the log configuration every other
crate initialises through, rather than each crate choosing its own.

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
