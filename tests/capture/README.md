# tests/capture

System tests that exercise real video capture against controlled test
applications rather than against installed games, so that results are
reproducible on any machine (AGENTS.md sections 25 and 26).

| File | What it is |
| --- | --- |
| `wgc_video_pattern.rs` | Captures `test-apps/video-pattern` through the Windows Graphics Capture backend, borderless and bordered, and accounts for every frame the source presented |
| `wgc_fullscreen_dx11.rs` | Captures `test-apps/fullscreen-dx11`, which covers a whole display, and checks the display is given back |
| `readback.rs` | Shared helper: copies a captured GPU texture into system memory so a test can read the pattern out of it |

The tests belong to the packages that own the applications they start — Cargo
only sets `CARGO_BIN_EXE_…` for a test in the binary's own package — so they are
declared as `[[test]]` targets in `test-apps/*/Cargo.toml` with their sources
here, beside the other system tests.

These tests depend on GPU and display hardware, so they are not part of the
pull-request CI job: they are `#[ignore]`d, and

```text
cargo test -p clipped-video-pattern --test wgc_video_pattern -- --ignored --nocapture --test-threads=1
cargo test -p clipped-fullscreen-dx11 --test wgc_fullscreen_dx11 -- --ignored --nocapture
```

is how they are run. [docs/testing.md](../../docs/testing.md) explains what each
test application draws, what it guarantees, how to run one by hand, and how a
test drives it.
