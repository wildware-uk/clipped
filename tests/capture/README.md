# tests/capture

System tests that exercise real video capture against controlled test
applications rather than against installed games, so that results are
reproducible on any machine (AGENTS.md sections 25 and 26).

| File | What it is |
| --- | --- |
| `wgc_video_pattern.rs` | Captures `test-apps/video-pattern` through the Windows Graphics Capture backend, borderless and bordered, and accounts for every frame the source presented |
| `wgc_fullscreen_dx11.rs` | Captures `test-apps/fullscreen-dx11`, which takes a whole display exclusively, and checks the display is given back |
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

## Wake the displays first

**A run taken while Windows has the displays powered off is worthless, and looks
like a capture defect rather than a sleeping machine.** Windows Graphics Capture
delivers what the desktop compositor composes, and a desktop nobody is looking at
is composed at about 4 Hz — so a 60 fps subject arrives at 3.97 fps, every one of
these tests fails its frame count, and `SetFullscreenState` is refused with
`DXGI_ERROR_NOT_CURRENTLY_AVAILABLE (0x887A0022)` into the bargain. The numbers
either side of that are in [docs/capture-pipeline.md](../../docs/capture-pipeline.md).

How to tell before you spend an afternoon on it: `powercfg /q SCHEME_CURRENT
SUB_VIDEO VIDEOIDLE` gives the display idle timeout, and `GetLastInputInfo` gives
how long the session has been idle. Idle for longer than the timeout means the
displays are off.

Waking them needs an input event; `SetThreadExecutionState(ES_DISPLAY_REQUIRED)`
resets the idle timer but does not turn a display back on, and neither does
`WM_SYSCOMMAND`/`SC_MONITORPOWER` on Windows 11 build 26200 — both were tried and
left the compositor at 3.97 fps. Move the mouse, or if the run is scripted, hold
`ES_CONTINUOUS | ES_DISPLAY_REQUIRED` for the length of it so the displays do not
go off underneath a long measurement.

## Exclusive fullscreen

`wgc_fullscreen_dx11.rs` is the only test that exercises it. It asks
`test-apps/fullscreen-dx11` for the display through
`IDXGISwapChain::SetFullscreenState`, and Windows decides — so the test reads the
`exclusive` field from the application's `ready` line and says which it got. On
an awake display it is granted, and the run then means what it says; on a display
that has been powered off it is refused, and the test prints `NOT EXERCISED` at
the end so that a green run is not mistaken for evidence.

`av_sync.rs` also needs an audio endpoint, and takes about ninety seconds by
default. `CLIPPED_AV_SYNC_SECONDS` lengthens the run — the drift figures in
[docs/av-sync.md](../../docs/av-sync.md) come from `CLIPPED_AV_SYNC_SECONDS=1800`
— because a drift of a few parts per million is not visible in ninety seconds
and is exactly what the model has to be right about. Set `CLIPPED_REQUIRE_AUDIO`
with it: without that variable a machine whose endpoint delivers no packets
prints `SKIPPED (av-sync): …` and still passes, and a run whose numbers are being
recorded should fail instead.

`wgc_video_pattern.rs` also holds tests of its own frame accounting — that a
counter arriving twice is counted as a duplicate and fails the run, that a run
missing half the source's frames fails, that a healthy run passes. Those need
neither a GPU nor a display and do run in the pull-request job, deliberately: the
capture tests above rest entirely on that checker, and a checker only exercised
on a machine with a display is a checker nobody has watched fail (AGENTS.md
section 54).

[docs/testing.md](../../docs/testing.md) explains what each test application
draws, what it guarantees, how to run one by hand, and how a test drives it.
