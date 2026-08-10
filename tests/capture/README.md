# tests/capture

System tests that exercise real video capture against controlled test
applications rather than against installed games, so that results are
reproducible on any machine (AGENTS.md sections 25 and 26).

| File | What it is |
| --- | --- |
| `wgc_video_pattern.rs` | Captures `test-apps/video-pattern` through the Windows Graphics Capture backend, borderless and bordered, and accounts for every frame the source presented |
| `wgc_fullscreen_dx11.rs` | Captures `test-apps/fullscreen-dx11`, which covers a whole display, and checks the display is given back |
| `av_sync.rs` | Captures `test-apps/video-pattern` and the system audio endpoint at the same time and measures how far the two clocks drift apart ([docs/av-sync.md](../../docs/av-sync.md)) |
| `readback.rs` | Shared helper: copies a captured GPU texture into system memory so a test can read the pattern out of it |

The tests belong to the packages that own the applications they start — Cargo
only sets `CARGO_BIN_EXE_…` for a test in the binary's own package — so they are
declared as `[[test]]` targets in `test-apps/*/Cargo.toml` with their sources
here, beside the other system tests.

The capture tests themselves depend on GPU and display hardware, so they are not
part of the pull-request CI job: they are `#[ignore]`d, and

```text
cargo test -p clipped-video-pattern --test wgc_video_pattern -- --ignored --nocapture --test-threads=1
cargo test -p clipped-fullscreen-dx11 --test wgc_fullscreen_dx11 -- --ignored --nocapture
cargo test -p clipped-video-pattern --test av_sync -- --ignored --nocapture --test-threads=1
```

is how they are run.

`av_sync.rs` also needs an audio endpoint, and takes about ninety seconds by
default. `CLIPPED_AV_SYNC_SECONDS` lengthens the run — the drift figures in
[docs/av-sync.md](../../docs/av-sync.md) come from `CLIPPED_AV_SYNC_SECONDS=1800`
— because a drift of a few parts per million is not visible in ninety seconds
and is exactly what the model has to be right about.

`wgc_video_pattern.rs` also holds tests of its own frame accounting — that a
counter arriving twice is counted as a duplicate and fails the run, that a run
missing half the source's frames fails, that a healthy run passes. Those need
neither a GPU nor a display and do run in the pull-request job, deliberately: the
capture tests above rest entirely on that checker, and a checker only exercised
on a machine with a display is a checker nobody has watched fail (AGENTS.md
section 54).

[docs/testing.md](../../docs/testing.md) explains what each test application
draws, what it guarantees, how to run one by hand, and how a test drives it.
