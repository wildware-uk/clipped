# tests/capture

System tests that exercise real video capture against controlled test
applications rather than against installed games, so that results are
reproducible on any machine (AGENTS.md sections 25 and 26).

| File | What it is |
| --- | --- |
| `wgc_video_pattern.rs` | Captures `test-apps/video-pattern` through the Windows Graphics Capture backend, borderless and bordered, and accounts for every frame the source presented |
| `wgc_fullscreen_dx11.rs` | Captures `test-apps/fullscreen-dx11`, which takes a whole display exclusively, and checks the display is given back |
| `av_sync.rs` | Captures `test-apps/video-pattern` and the system audio endpoint at the same time: how far the two clocks drift apart, and — against a subject playing a tone at the moment it presents a named frame — the absolute A/V offset ([docs/av-sync.md](../../docs/av-sync.md)) |
| `screenshot.rs` | Photographs `test-apps/video-pattern` through the real capture backend, saves the file, and reads the pattern back out of the picture that was saved ([docs/screenshots.md](../../docs/screenshots.md)) |
| `screenshot_fullscreen.rs` | The same, of `test-apps/fullscreen-dx11` holding a whole display — the third of the presentations issue #67 names, which `screenshot.rs` cannot reach from the other package |
| `screenshot_during_recording.rs` | Records `test-apps/video-pattern` and takes screenshots out of the running recording, measuring that it went on capturing and encoding across every one of them |
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
cargo test -p clipped-video-pattern --test av_sync -- --ignored --nocapture --test-threads=1 av_offset_stays
cargo test -p clipped-video-pattern --test av_sync -- --ignored --nocapture --test-threads=1 the_absolute
cargo test -p clipped-video-pattern --test screenshot -- --ignored --nocapture --test-threads=1
cargo test -p clipped-fullscreen-dx11 --test screenshot_fullscreen -- --ignored --nocapture
cargo test -p clipped-video-pattern --test screenshot_during_recording -- --ignored --nocapture
```

is how they are run.

## Wake the displays first

**A run taken while Windows has the displays powered off is worthless, and looks
like a capture defect rather than a sleeping machine.** Windows Graphics Capture
delivers what the desktop compositor composes, and a desktop nobody is looking at
is composed at about 4 Hz — so a 60 fps subject arrives at 3.97 fps and every one
of these tests fails its frame count. The numbers either side of that are in
[docs/capture-pipeline.md](../../docs/capture-pipeline.md).

How to tell before you spend an afternoon on it: the frame count, which is an
order of magnitude out rather than a few percent. `powercfg /q SCHEME_CURRENT
SUB_VIDEO VIDEOIDLE` gives the display idle timeout and `GetLastInputInfo` gives
how long the session has been idle — `wgc_fullscreen_dx11.rs` prints the latter
with its result — but a long idle time is *not* on its own evidence that the
displays are off. Measured: 1,024 seconds idle on a machine whose timeout is 900,
and the run still delivered 301 of a possible 300 frames. Running these tests
puts a fullscreen application on that display, which is one of the things that
resets the display's own timer.

Waking them needs an input event. `SetThreadExecutionState(ES_DISPLAY_REQUIRED)`
does not turn a display back on, and neither does
`WM_SYSCOMMAND`/`SC_MONITORPOWER` on Windows 11 build 26200 — both were tried and
left the compositor at 3.97 fps. Move the mouse; and if the run is scripted, hold
`ES_CONTINUOUS | ES_DISPLAY_REQUIRED` for the length of it so the displays do not
go off underneath a long measurement.

Even short of that, a machine nobody has touched winds itself down: the same
`wgc_fullscreen_dx11.rs` run that delivers 272 of 300 frames thirty seconds after
an input event delivered 229 of 300 ten minutes after one, with the subject
itself presenting 278 frames rather than its usual 324. Take measurements on a
machine that is awake.

## Exclusive fullscreen: how to get a run that means anything

`wgc_fullscreen_dx11.rs` is the only test that exercises it. It asks
`test-apps/fullscreen-dx11` for the display through
`IDXGISwapChain::SetFullscreenState`, and **Windows decides** — so the test reads
the `exclusive` field from the application's `ready` line and reports which it
got. It cannot create the state Windows wants, and it does not try.

On this project's development machine (Windows 11 Pro build 26200, RTX 4090)
exactly one thing changes the answer:

> **A process that has synthesised an input event must still be running when
> the subject calls `SetFullscreenState`.**

That is a measured rule, not a documented one. Recency is not what matters and
neither is lineage: an event five seconds old is refused if the process that made
it has exited, an event twenty minutes old is granted if that process is still
alive, and a subject started through `Win32_Process::Create` — parented to
`WmiPrvSE`, no relation to the injector — is granted along with everything else.
Displays powered on or off did not move it either; the full table is under
"Exclusive fullscreen" in
[docs/capture-pipeline.md](../../docs/capture-pipeline.md).

**So running the test on its own does not produce a grant, however awake the
machine is.** Every run measured with no such process alive was refused, five of
them on displays that were awake with the compositor at full rate and 273 to 300
of 300 frames delivered. Whether a *person* moving a real mouse would satisfy
Windows was not measured — there is no live process behind a real mouse either,
so on the rule above it should not, but nobody has sat at the machine and
checked. What is known to work is the shell that starts the test being the thing
that synthesised the event, and staying open for the run.

```powershell
# In an interactive PowerShell, and then run cargo test from the SAME shell:
# this process is the one that has to still be there when the subject asks for
# the display.
Add-Type -MemberDefinition @'
[DllImport("user32.dll")] public static extern void mouse_event(uint f, int dx, int dy, uint d, IntPtr e);
'@ -Name Input -Namespace Win32

# A zero-delta move: it moves no cursor and types into no window. It also wakes
# a display the idle timeout has turned off, which matters for the separate
# reason above.
[Win32.Input]::mouse_event(1, 0, 0, 0, [IntPtr]::Zero)

$env:CLIPPED_REQUIRE_CAPTURE = "1"
cargo test --release -p clipped-fullscreen-dx11 --test wgc_fullscreen_dx11 -- --ignored --nocapture
```

`CLIPPED_REQUIRE_CAPTURE` is not optional when the result is being recorded.
Without it, a run Windows refused prints `NOT EXERCISED` and still passes — which
is the correct default for somebody who just ran the suite, and useless as
evidence. With it, that run fails and says why.

`av_sync.rs` also needs an audio endpoint, and holds two tests of about ninety
seconds each — so name the one you want rather than running both. The drift one
(`av_offset_stays`) makes no sound and takes `CLIPPED_AV_SYNC_SECONDS`; the
figures in [docs/av-sync.md](../../docs/av-sync.md) come from
`CLIPPED_AV_SYNC_SECONDS=1800`, because a drift of a few parts per million is not
visible in ninety seconds and is exactly what the model has to be right about.
The absolute one (`the_absolute`) starts the subject with `--tone`, so it does
make a sound — a 30 ms tone at about −28 dBFS every five seconds — and takes
`CLIPPED_AV_SYNC_TONE_SECONDS`. Set `CLIPPED_REQUIRE_AUDIO` with either: without
that variable a machine whose endpoint delivers no packets prints
`SKIPPED (av-sync): …` and still passes, and a run whose numbers are being
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
