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

## Building from source

Requires the Rust toolchain pinned by `rust-toolchain.toml` and the Windows
build prerequisites.

```text
cargo build --workspace
cargo test --workspace
```

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

## Dependency direction

The crates are layered. **A crate may depend only on crates in a lower layer.**
This is not a convention to be remembered — it is asserted against the real
dependency graph by `tests/integration/tests/workspace_layering.rs`, which
fails if a dependency points sideways or upwards, or if a new crate is added
without being placed in a layer.

| Layer | Crates | Depends on |
| --- | --- | --- |
| 0 | `clipped-windows`, `clipped-events`, `clipped-storage` | nothing in this workspace |
| 1 | `clipped-capture`, `clipped-audio`, `clipped-encoder`, `clipped-library`, `clipped-game-detection`, `clipped-plugins` | layer 0 |
| 2 | `clipped-muxer` | layers 0–1 |
| 3 | `clipped-session` | layers 0–2 |
| 4 | `clipped-recorder` (binary) | layers 0–3 |

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

## Contributing

Work is tracked as GitHub issues grouped into milestones. Engineering
standards for this repository are in [AGENTS.md](AGENTS.md) and apply to human
and automated contributors alike.

## Licence

Mozilla Public License 2.0. See [LICENSE](LICENSE).
