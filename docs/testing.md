# Testing capture against controlled test applications

**Status: the video test applications exist and one capture test drives them.**
`test-apps/video-pattern` and `test-apps/fullscreen-dx11` are built, documented
here, and exercised end to end by the tests in `tests/capture/`. The audio ones
AGENTS.md section 26 also names do not exist yet, and
[the last section](#what-is-not-built-yet) says why and where they are tracked.

The other half of testing a recorder is what comes *out* of it, which is
[validating produced media](#validating-produced-media) — one harness, used by
every crate that writes a file.

## Why these exist

A capture pipeline cannot be tested against a game. A game is not installed on
every machine, never renders the same thing twice, and cannot tell a test which
frame it just presented — so "did the recording drop a frame?" becomes a person
watching a video and forming an opinion. AGENTS.md sections 25 and 26 rule that
out and ask for controlled test applications instead:

> Manual testing with Spotify and Discord does not scale and is not
> deterministic.

So the subject brings the answer with it. Every frame `video-pattern` presents
carries its own frame number, drawn into its pixels, along with enough redundant
information to prove the frame is whole. A test captures the window, reads the
numbers back, and knows exactly which frames arrived, which never did, and which
arrived twice — with nobody watching.

## The applications

| Application | What it is | Run it |
| --- | --- | --- |
| `test-apps/video-pattern` | A window — bordered or borderless — presenting the deterministic pattern at a fixed rate | `cargo run -p clipped-video-pattern --bin video-pattern -- --help` |
| `test-apps/fullscreen-dx11` | The same pattern covering a whole display, exclusively where Windows allows it | `cargo run -p clipped-fullscreen-dx11 --bin fullscreen-dx11 -- --help` |

Both are ordinary workspace members, so `cargo build --workspace`,
`cargo clippy --workspace --all-targets` and `cargo fmt --all` cover them. A
test application nobody compiles is a capture test that has quietly stopped
running.

### Rules both of them follow

- **They go on a display that is not the primary one**, when the machine has
  one, and they are always-on-top everywhere except fullscreen. That is not
  politeness: an always-on-top window over somebody's work gets minimised,
  Windows Graphics Capture stops delivering frames for a minimised window, and
  the run stops measuring capture and starts measuring Alt-Tab. `--monitor
  primary` overrides it.
- **They never outlive whoever started them.** A run ends at its `--seconds`
  deadline, when standard input closes or carries `stop`, at Ctrl-C, or when the
  window is closed — and every one of those paths gives back the display and
  destroys the window before the process exits. A test that panics still closes
  the pipe, and the application notices.
- **They are per-monitor DPI aware**, so the pattern is drawn in the same
  physical pixels a capture backend sees. Without that, a display scaled above
  100% would have Windows stretch every cell of the pattern and the decoder
  would be reading a resampled image.
- **They say what they did on standard output**, in a line-based protocol, so
  that driving one from a test needs no guessing. Diagnostics go to standard
  error, so a driver parsing the protocol cannot be derailed by a warning.

## `video-pattern`

### What it draws

Every frame is drawn from the frame counter alone, in BGRA8:

```text
┌──────────────────────────────────────────────────────────┐
│ ■■■■□■□□■…  header row: magic, 32-bit counter, checksum   │  16 px tall
│                                                          │
│         ███  marker square, at x = f(counter)             │  64 px
│                                                          │
│   background: palette[counter % 8]                       │
└──────────────────────────────────────────────────────────┘
```

- **Magic**: four cells of fixed colours, which say "this is a Clipped test
  pattern" and let a decoder find the pattern inside a bigger frame.
- **Counter**: 32 cells, one per bit, least significant first, white for one and
  black for zero. This is the frame's identity.
- **Checksum**: eight more cells over the counter. A single misread cell turns
  frame 4 into frame 262,148, and without the checksum a test would report a
  gap of a quarter of a million frames instead of a decode failure.
- **Background and marker**: both are functions of the counter, and both are
  checked against it. They are redundant on purpose — they are what makes a
  *torn* frame, one assembled from two source frames, fail to decode rather than
  read as a good frame.

The exact geometry, the palette and the tolerances are in
`test-apps/video-pattern/src/pattern.rs`, which is the one place they are
defined: the renderer and the decoder are two functions in that file, and their
agreement is asserted by unit tests that need no GPU and run in CI.

### What it guarantees

- Frame *n* is drawn identically on every machine, every run.
- Consecutive frames always differ, so the compositor always has new content and
  never skips composing the window.
- A frame that decodes is whole: its header, background and marker agree — for
  any tear between two source frames less than the marker's period apart, which
  is 152 frames in a 1280-pixel-wide pattern. [What the pattern cannot
  see](#what-the-pattern-cannot-see) has the rest of that.
- The counter it draws is the count of frames it has presented, so the `stopped`
  line's frame count and the last counter captured are two independent accounts
  of the same run. That holds for every frame the application ever presents,
  including the warm-up frames DXGI needs before an exclusive fullscreen
  transition: they come from the same counter and are counted in the same total,
  so no counter is ever drawn twice and none goes unreported.
- **It is silent unless `--tone` is passed.** With it, the application also plays
  a 30 ms 997 Hz tone at about −28 dBFS at the moment it presents a named frame,
  and announces both moments — where the endpoint's own clock puts the tone, and
  the counter reading immediately after the frame went to the compositor. That is
  what gives a capture an event whose sound and picture were simultaneous at the
  source, and therefore an *absolute* A/V offset rather than a relative one
  ([av-sync.md](av-sync.md)). The `ready` line names the frames that carry a
  tone, so a test knows which ones to decode before the run starts.

### Running it by hand

```text
# The default: a borderless 1280x720 window at 30 fps on a non-primary display,
# for sixty seconds.
cargo run -p clipped-video-pattern --bin video-pattern

# An ordinary window with a title bar, at 60 fps, 2560x1440, for ten seconds.
cargo run -p clipped-video-pattern --bin video-pattern -- \
    --mode windowed --fps 60 --size 2560x1440 --seconds 10

# Started by a script with no standard input to give: without --ignore-stdin the
# application sees end-of-input immediately and stops.
cargo run -p clipped-video-pattern --bin video-pattern -- --ignore-stdin

# With sound: a quiet 997 Hz tone at the moment a named frame is presented,
# every five seconds. Silent without this.
cargo run -p clipped-video-pattern --bin video-pattern -- --tone --seconds 20
```

`--help` lists every option. A release build is not needed — a debug build
presents 2560x1440 at 60 fps on this project's development machine — but if a
run reports fewer frames than it was asked for, build with `--release` before
concluding anything about capture.

### The protocol a test drives it through

```text
ready hwnd=0x00000000003b0c62 client=2560x1440 fps=60 presentation=borderless exclusive=no monitor=\\.\DISPLAY1 tone=off
stopped frames=300 reason=deadline
```

`tone` is `off` when the run was not asked for one, `no` when it was and this
machine cannot play one, and `yes` followed by `tone-hz`, `tone-ms`,
`tone-first` and `tone-every` when it can — after which one `tone` line is
printed per tone as the frame it belongs to is presented:

```text
tone index=0 frame=60 onset=61420657101866 present=61420657564400 skew=462534
```

`onset` is where the endpoint's own clock put the tone and `present` the counter
reading just after the frame went to the compositor, both in nanoseconds on the
performance counter. A tone with no moment says which of the two reasons it has:
`onset=none` is one the render thread refused to place and did not play, and
`onset=pending` is one it had not reported by the time the frame was presented —
probably played, but with nothing to measure it from. They are separate states
because one is a missing sound and the other is a missing report, and a driver
that counted them together would report a scheduling hiccup as an unplayed tone.

The `ready` line comes after the window exists, the swap chain is presenting
and — for a fullscreen run — the display has been asked for. Capturing before
that line is capturing a window that may not be there. The `stopped` line is
printed after the display has been given back and the window destroyed, so a
driver can treat it as permission to stop worrying about the process.

`reason` is one of `deadline`, `stop-requested`, `interrupted` or
`window-closed`.

## `fullscreen-dx11`

The same pattern, the same protocol, over a whole display — as a borderless
window covering it (`--mode borderless`, what most modern games actually use) or
by asking DXGI for the display itself (`--mode exclusive`, the default).

It is a separate binary rather than a flag on `video-pattern` because taking a
display away from whoever is at the machine should be a decision, not a typo.

```text
cargo run -p clipped-fullscreen-dx11 --bin fullscreen-dx11 -- --seconds 10
```

**Windows usually refuses the exclusive transition.** `SetFullscreenState` needs
the foreground, and Windows does not grant the foreground to a process the user
has not interacted with — which includes anything a test started. The application
says which it got (`exclusive=yes` or `exclusive=no`), warns on standard error
with `DXGI_ERROR_NOT_CURRENTLY_AVAILABLE (0x887A0022)` when it was refused, and
carries on as a borderless window covering the display. Do not read
`exclusive=no` as a defect in the application, and do not read a passing test as
proof that exclusive fullscreen capture works — read the field.

It *can* be granted, and what decides it has been measured: a process that
synthesised an input event has to still be running when the transition is asked
for. Not the launch path, not how long the session has been idle, and not
whether the displays are powered on — all three were varied and none of them
moved the answer. The evidence is under "Exclusive fullscreen" in
[capture-pipeline.md](capture-pipeline.md) and the procedure is in
[tests/capture/README.md](../tests/capture/README.md). Running the application
or the test on its own does not produce a grant, however awake the machine is.

## The capture tests

`tests/capture/` holds the tests that point a real capture backend at these
applications:

| Test | What it decides |
| --- | --- |
| `wgc_video_pattern.rs` | That a borderless window and an ordinary bordered window are both captured frame for frame: dropped, duplicated, out-of-order and torn frames are counted *and* asserted on, and the checker that does it is itself tested without a GPU |
| `wgc_fullscreen_dx11.rs` | That an application covering a whole display is captured, that every frame that arrives is the pattern, and that the display is the shape it was afterwards — and, when Windows refused the exclusive transition, that the run says so and fails under `CLIPPED_REQUIRE_CAPTURE` rather than passing as if it had proved something |
| `av_sync.rs` | Two runs: that video and system audio captured at the same time stay within a documented tolerance of each other and by how much per minute they drift, and — against a subject playing a tone at the moment it presents a named frame — what the *absolute* A/V offset of a capture is ([av-sync.md](av-sync.md)) |
| `readback.rs` | Not a test: the helper that copies a captured GPU texture into system memory so the others can look at it |

## The end-to-end recorder tests

`tests/capture/` stops at the capture backend. The tests that drive the whole
recorder — capture, encoder and muxer, as a user gets it — live beside the
binary in `apps/recorder/tests/`, because what they exercise is the executable
rather than any one crate:

| Test | What it decides |
| --- | --- |
| `record_end_to_end.rs` | That `clipped-recorder record` against a real window produces media the harness accepts — and, in `ctrl_c_during_a_recording_leaves_a_playable_file`, that a real `CTRL_C_EVENT` mid-recording leaves a file that **plays**, with the three finalisation lines appearing in the diagnostics *in order*, so "the trailer was written and then the encoder was flushed" fails exactly as a missing line does. Also that a recording ends when its window closes, and still leaves valid media |
| `ipc_protocol.rs` | That `clipped-recorder serve` speaks the protocol in [ipc.md](ipc.md) over a real named pipe to a real child process: the handshake and a version it does not speak, a frame that is not a message, a length prefix that would allocate the machine, a client that vanishes mid-request, commands whose subsystem is not built, the connection cap and its slot coming back, a second recorder refusing to compete for the endpoint, a recording driven **entirely over the protocol** producing a playable file — and an export driven the same way, whose MP4 is **decoded** from first frame to last, holds the recording's coded bytes packet for packet, refuses a destination that already exists without touching it, and reports a refusal from the muxer in the muxer's own words |
| `ctrl_c.rs` | The half of Ctrl+C that needs no capture engine: that the finalisation hook runs exactly once and the process exits cleanly, against a real child sent a real console event. `record_end_to_end.rs` is what proves the resulting *file* plays |
| `supervision.rs` | That the recorder is a process with a lifetime of its own, against real processes ended with a real `TerminateProcess` — no signal, no destructors: that a recorder outlives the process that started it and keeps serving, that a second launch attaches rather than competing and one holding the instance name touches nothing at all, that a killed recorder is reported and replaced rather than left showing a stale state, and that a recorder which cannot start is given up on after a bounded number of attempts. Two of its tests need a GPU and record a real window: one kills the supervisor mid-recording and proves the recording carried on and the file plays, the other kills the *recorder* mid-recording and proves the file it left is playable and that the supervisor named it |
| `command_line.rs` | The command-line surface: that `record --help` documents a default for every option, that invalid values are usage errors and not panics, that a missing target names all three ways of giving one, that an existing recording is not overwritten without being asked, and that `list-windows` and `capabilities` report what they claim to. Also the one thing about `watch` that only the built program shows: that a settings file it cannot read produces the plain sentence on **standard error**, on a line of its own rather than only inside a log record. It starts `watch` over an unreadable file, waits for it to say it is watching, and stops it |
| `unreadable_settings.rs` | That a settings file `watch` cannot read is **reported** — once, as a warning, carrying the sentence that says the file was left alone — and that a file which reads cleanly, or is not there at all, is not worth a word. It is a binary of its own because observing a report means installing a subscriber, and a second subscriber in a process makes `tracing` abandon its cached per-callsite decisions; sharing a process with this crate's other tests made it fail about half the time. This is the **log** half of that report; the console half is in `command_line.rs`, because no in-process subscriber can see an `eprintln!`. What such a file does to a *recording* is `watch`'s own tests |

Most of these need a GPU and a display, so they skip themselves without one and
`CLIPPED_REQUIRE_CAPTURE` turns that skip into a failure. The command-line, the
settings-report and the Ctrl+C tests do not, and run anywhere; so does all of
`supervision.rs` except the two `#[ignore]`d tests that record a real window.

### Running one of them on its own

`supervision.rs` and `ctrl_c.rs` drive a fixture from `apps/recorder/examples/`,
and **selecting a single test target does not build examples**:

```text
cargo build -p clipped-recorder --examples
cargo test  -p clipped-recorder --test supervision
```

Without the first line, `cargo test --test supervision` after an edit to
`crates/ipc` builds a fresh test binary and runs it against a fixture compiled
from the code as it was before the edit — and passes. For a suite whose subject
is process supervision that is the worst failure mode it could have, so
`support::example_binary` refuses an example older than anything it was built
from and names the command above. `cargo test --workspace`, which is what CI
runs, builds examples itself and is unaffected.

### How a test drives an application

```rust
use std::time::Duration;
use clipped_video_pattern::harness::TestApp;
use clipped_video_pattern::pattern::{self, Surface};

let app = TestApp::start(
    env!("CARGO_BIN_EXE_video-pattern"),
    ["--mode", "borderless", "--fps", "30", "--seconds", "120"],
    Duration::from_secs(30),
)?;

// app.window() is the HWND to capture; app.client_size() is the exact size of
// the pattern to look for. Capture, then, for each frame:
//
//   let image = reader.read(frame.texture())?;
//   let surface = Surface::new(&image.pixels, image.stride, image.width, image.height)?;
//   let region = pattern::locate(&surface, width, height)  // once
//   let decoded = pattern::decode(&surface, region)?;      // every frame
//   decoded.index()  // the source's own frame number

let stopped = app.stop(Duration::from_secs(10))?;  // and it is gone
```

Three details worth knowing:

- `TestApp::start` returns only once the application has announced itself, and
  `TestApp`'s `Drop` closes standard input, waits, and kills the process. There
  is no path — panic included — on which the application outlives the test.
- `pattern::locate` is called once and its `Region` reused. A window capture
  includes the border and title bar, so the client area does not start at the
  frame's top-left corner: in a measured run of a 1280x720 window the frame was
  1282x752 and the pattern was at (1, 31). Searching every frame would work and
  would be slow.
- The readback copies the texture *while the frame is held*, because the texture
  belongs to the backend for exactly that long (`docs/capture-pipeline.md`).

### Running them

They are `#[ignore]`d. They need a GPU, a desktop session, a compositor and
about fifteen seconds, and the fullscreen one takes over a display — none of
which belongs in `cargo test --workspace`, and none of which a hosted CI runner
has (`tests/capture/README.md`, and the `Test` step in
`.github/workflows/ci.yml`).

```text
cargo test -p clipped-video-pattern --test wgc_video_pattern -- --ignored --nocapture --test-threads=1
cargo test -p clipped-fullscreen-dx11 --test wgc_fullscreen_dx11 -- --ignored --nocapture
cargo test -p clipped-video-pattern --test av_sync -- --ignored --nocapture --test-threads=1
```

`av_sync.rs` additionally needs an audio endpoint, runs for about ninety seconds
by default, and takes `CLIPPED_AV_SYNC_SECONDS` for the long runs the drift
figures in [av-sync.md](av-sync.md) come from. Set `CLIPPED_REQUIRE_AUDIO` when
the numbers are being recorded, so that a machine which delivers no endpoint
packets fails rather than printing `SKIPPED (av-sync): …` and passing.

Its two tests differ over sound. The drift one makes none: it holds a render
stream open so the endpoint's clock keeps running, and every buffer it hands the
audio engine is marked silent. The absolute one does make a sound, because a
measurement of where a recording puts a sound needs one — the subject is started
with `--tone` and plays a 30 ms tone at about −28 dBFS every five seconds.
[av-sync.md](av-sync.md) has both, and which command runs which.

`--nocapture` is worth typing: each test prints its frame accounting, which is
the evidence AGENTS.md section 53 asks to be recorded on the issue. A run on
this project's development machine (RTX 4090, 2560x1440 at 144 Hz) reads:

```text
=== wgc_video_pattern (borderless) ===
pattern found at    : 1280x720 at (0, 0)
frames delivered    : 181
frames decoded      : 181
acquisition timeouts: 0
source frames in run: 181 (counters 4 to 184)
dropped             : 0 (0.00%)
duplicated          : 0
out of order        : 0
backend said missed : 0
span on source clock: 6.00s
undecodable frames  : 0
```

Every one of those numbers is asserted on and not merely printed: the run fails
if the pattern was never found, if too few frames arrived to conclude anything,
if any frame did not decode, if any counter arrived out of order or twice, or if
more than 5% of the frames the source presented never arrived. The one tolerance
is the last: a busy machine can lose a frame, and nothing a machine does makes a
backend hand the same source frame over twice.

They are `#[ignore]`d rather than skipping themselves at runtime, deliberately.
A test that decides for itself that it could not run reads as a pass, and the
difference between "it ran and passed" and "it did not run" is exactly what this
file is for. Where a test *can* usefully skip — the capture unit tests inside
`clipped-capture` — the project has `CLIPPED_REQUIRE_CAPTURE` to turn a skip
into a failure on a machine that is supposed to be able to capture.

## Running the suite without making a noise

Some tests play sound. `crates/audio/tests/system_audio.rs` renders a quiet
997 Hz tone so it can capture it back and measure it, and the A/V sync tests
drive `video-pattern --tone`. That is the right way to test a capture path —
a real reference signal, not a mock — but `cargo test --workspace` is the
command CONTRIBUTING.md asks every contributor to run before review, and on a
machine with a sound card it makes noise. On a call, in headphones, or on the
tenth run of the afternoon, that is unwelcome.

```text
CLIPPED_SKIP_AUDIO=1 cargo test --workspace
```

Every test that opens an audio device or plays a tone then skips, reporting
`SKIPPED (audio): CLIPPED_SKIP_AUDIO is set` on stderr — loudly, like every
other skip here, so that a quiet run never looks like a passing one.

It is checked *before* a device is opened rather than after, because by the
time a test has discovered it cannot run it has already made whatever noise it
was going to make.

`CLIPPED_SKIP_AUDIO` is not the opposite of `CLIPPED_REQUIRE_AUDIO`. That one
is about whether a machine *can* run these tests; this one is about whether it
should right now. Setting both is a contradiction — one says they must not run,
the other says they must not be skipped — and fails with a message saying so,
rather than letting either win silently.

### Clearing up a run by hand, without stopping somebody else's

`TestApp`'s `Drop` stops the application on every path, so an ordinary run leaves
nothing behind. A run interrupted by hand — a killed shell, a debugger — can, and
the obvious clean-up is the wrong one:

```text
# Wrong: stops every checkout's copy, not yours.
Stop-Process -Name fullscreen-dx11, video-pattern

# Right: matches the executable that came out of this working tree.
Get-CimInstance Win32_Process |
    Where-Object { $_.ExecutablePath -like "$PWD*" } |
    ForEach-Object { Stop-Process -Id $_.ProcessId }
```

Every checkout of this repository builds test applications with the same file
names, so two working trees — two `git worktree`s, a clone beside a clone — run
processes called `video-pattern.exe` that are not the same program. Matching on
the name stops whichever one answers first, which during this project has meant
ending another contributor's measurement mid-run and leaving them to work out
why their numbers stopped. Match on `ExecutablePath`, and check what you are
about to stop before you stop it.

## What the pattern cannot see

- **A tear a whole marker period wide.** The background repeats every eight
  frames and the marker returns to the same x after `(width - 64) / 8` frames —
  152 in a 1280-pixel pattern, 312 in a 2560-pixel one — so a frame assembled
  from two source frames exactly that far apart reads as a good one. Every
  displacement below the period is caught, which is swept in full by a unit
  test, and the first blind one is pinned by another so the bound stays a stated
  number. At 30 fps it is a tear between frames five seconds apart, which is not
  a compositor handing over a half-composed frame.
- **HDR.** The pattern is drawn and decoded in BGRA8. On a display doing a
  colour conversion the decoder would fail rather than mislead, but it would
  have nothing useful to say. HDR capture is
  [issue #99](https://github.com/wildware-uk/clipped/issues/99).
- **Scaling.** The decoder samples the middle of each cell and compares colours
  with a tolerance, so a capture that resampled the image would fail to decode
  rather than report the resampling. Nothing in the pipeline does that today.
- **Anything about a real game.** A test application proves the pipeline handles
  a controlled subject. Games do things nothing here does — protected content,
  mode switches, driver resets — and that is what the capture compatibility
  matrix in [issue #96](https://github.com/wildware-uk/clipped/issues/96) exists
  for.

## Relationship to `wgc_probe`

`crates/capture/examples/wgc_probe.rs` also renders a Direct3D 11 window, and
the overlap is real: a window class, a swap chain, a paced present loop and a
non-primary monitor choice exist in both. They are separate because they answer
different questions — the probe *measures the capture backend* (pacing
percentiles, resource drift over half an hour, what happens when a window is
minimised or closed), and the test applications *are the subject* a test points
a capture at — and because the layering forbids the merge as things stand:
`clipped-capture` is layer 1 and `clipped-video-pattern` is layer 5, so the
probe cannot depend on the application without inverting the dependency
direction the workspace enforces.

Folding the two windows into one is
[issue #137](https://github.com/wildware-uk/clipped/issues/137). Until then, a
change to how a test window is created has to be made in both places, and the
two are already drifting: only the test application is per-monitor DPI aware,
and only the probe detects that its window was minimised.

## Validating produced media

**Status: built and in use.** `tests/media` is `clipped-media-validation`, the
harness every crate that writes a file checks its output with. AGENTS.md section
22 is why it exists:

> Generated media must be validated. Do not assume successful encoder/muxer
> calls mean the recording is valid.

A muxer that returns `Ok` has written a file, not a recording. Before this
harness each crate answered "is it really valid?" its own way — `crates/muxer`
had one `ffprobe` wrapper and the NVENC tests another — which meant a third way
arrived with every ticket and none of them had ever been pointed at a file that
was actually broken.

### What it can assert

| Expectation | Method | What it catches |
| --- | --- | --- |
| The container opens | `Media::open` | A recording that never got a header, or that is not media at all |
| The video stream is the one that was asked for | `.video(VideoStream::codec(…).resolution(…).pixel_format(…).frame_rate(…))` | Wrong codec, wrong size, wrong pixel format |
| The video *plays* | `.decoded_frames(n)` | A stream that is listed but does not decode: the count is what came out of a decoder, not what the container claims |
| The expected number of audio streams | `.audio_stream_count(n)` | The multi-track failure — sources that were supposed to be separate arriving as one track |
| Each audio track's codec, rate, channels, name, language, default flag | `.audio(index, AudioStream::…)` | A microphone track silently promoted to stereo, a track that lost its name |
| Every track got its own packets | `.packets(n)` on a stream | A writer that routed everything to the first stream |
| The duration is plausible | `.duration_seconds(expected, tolerance)` | A recording that stopped early, or one whose timeline is wrong |
| Timestamps increase, per stream | `.monotonic_timestamps()` | A clock that stepped backwards |
| The tracks are in sync | `.synchronised_within(bound)` | Tracks that start apart, and tracks that *drift* apart over the recording |
| The recording starts at zero | `.streams_start_at(0.0, tolerance)` | A writer that never rebased its timestamps: every track three seconds in, and in sync with itself |
| A track carries its own tone and none of the others | `.audio_tone(index, Tone::at(440.0).isolated_from(880.0))` | Audio isolation, which no amount of `ffprobe` output can see |

Nothing is asserted until `assert_valid()`, so one run reports every failed
expectation rather than the first:

```rust
use std::time::Duration;

use clipped_media_validation::{require_media_tools, AudioStream, Media, VideoStream};

let Some(_tools) = require_media_tools() else { return };  // a clean skip

Media::open(&path)
    .expect("the recording opens")
    .validate()
    .stream_count(3)
    .video(VideoStream::codec("h264").resolution(640, 360).decoded_frames(120))
    .audio_stream_count(2)
    .audio(0, AudioStream::codec("pcm_s16le").sample_rate(48_000).title("Compatibility Mix"))
    .duration_seconds(4.0, 0.1)
    .monotonic_timestamps()
    .synchronised_within(Duration::from_millis(40))
    .assert_valid();
```

`.check()` returns the report instead of panicking, and `.that(condition, ||
message)` carries a bespoke assertion into the same report, which is how
`crates/muxer/tests/abrupt_termination.rs` keeps its own arithmetic about how
much of a killed recording survived.

### What a failure says

"Media invalid" is useless at two in the morning. This is what
`a_missing_audio_track_is_reported_with_the_tracks_that_are_there` printed on
this project's development machine:

```text
2 expectations were not met for C:\Users\shaun\AppData\Local\Temp\clipped-media-missing-track-49276-2-1786399811931531800\recording.mkv:
  1. audio stream count: expected 3, found 2 (a:0 pcm_s16le 48000 Hz stereo, 94 decoded frames, 94 packets 'Game' (default); a:1 pcm_s16le 48000 Hz stereo, 94 decoded frames, 94 packets 'Other System Audio')
  2. a:2: the file has no such stream (v:0 h264 320x180 yuv420p 30/1 fps, 60 decoded frames, 60 packets 'Gameplay'; a:0 pcm_s16le 48000 Hz stereo, 94 decoded frames, 94 packets 'Game' (default); a:1 pcm_s16le 48000 Hz stereo, 94 decoded frames, 94 packets 'Other System Audio')
what the file actually holds:
  v:0 h264 320x180 yuv420p 30/1 fps, 60 decoded frames, 60 packets 'Gameplay'
  a:0 pcm_s16le 48000 Hz stereo, 94 decoded frames, 94 packets 'Game' (default)
  a:1 pcm_s16le 48000 Hz stereo, 94 decoded frames, 94 packets 'Other System Audio'
  container: matroska,webm, duration 2.000s
```

Every report ends with an inventory of the file, because the next question after
"what did I expect?" is always "what did I get?".

### It is tested against media that is broken

A validator only ever run against good input is not a validator, so
`tests/media/tests/invalid_media.rs` damages real recordings in the ways real
recorders damage them — the fixtures are written by the pinned FFmpeg build,
never by the crates being validated, and then taken apart at the byte level:

| The damage | What the harness says |
| --- | --- |
| Truncated before the track entries reached the disk | `Media::open` refuses it: `is not media that can be opened: ffprobe found no streams in it. The file is 173 bytes` |
| Truncated to half its length after it was finished | The duration still reads 2.000s — the segment header survived the cut — and `decoded frames: expected 60, found 30` is what catches it |
| An audio track that was never written | The report above |
| A cluster timestamped before the one in front of it | `timestamps: a:0 goes backwards — packet 40 of the file is at 0.000000s, after packet 38 at 0.491000s` |
| Audio half a second behind the video | `A/V synchronisation: the tracks start 0.500s apart, which is more than the stated 0.050s bound (a:0 starts at 0.500s, v:0 starts at 0.000s)` |
| Audio that begins with the video and then stops halfway | `A/V synchronisation: the tracks end 1.001s apart, which is more than the stated 0.050s bound (a:0 ends at 0.999s, v:0 ends at 2.000s)` — the start check passes on this file, which is what makes it a test of the *end* |
| A track another source bled into | `a:1 isolation: 440 Hz belongs to another source and must not be audible here, but it measures 0.1250 against this track's own 1320 Hz at 0.1250 — 1.0x apart` |
| A track nothing was ever routed into | `the track is silent (peak amplitude 0.00e0 over 2.00s of audio)` |
| A bare H.264 elementary stream, which has no timeline at all | `start time: v:0 reports no start time at all, so there is nothing to place it on the recording's timeline` — a missing field is a failure, never a silent 0.000s — and `A/V synchronisation: nothing to compare … this file has 1` |

### Why FFmpeg's programs rather than the linked libraries

Deliberately, and for three reasons. AGENTS.md section 22 names `ffprobe` for
exactly this and the pinned build ships it, so there is nothing extra to
install. A validator that read files through the same code that wrote them would
be marking its own homework. And the layering forbids the alternative anyway:
`crates/muxer` is layer 2 and owns the only FFmpeg linkage in the workspace, so
a harness the muxer's own tests can use has to sit below it.

`ffmpeg` is used for one thing — decoding an audio track to samples, for the
tone assertions. Both are test tools and nothing else: nothing in the recorder
shells out to FFmpeg ([docs/ffmpeg.md](ffmpeg.md)).

### Running it, and skipping it honestly

```text
cargo test -p clipped-media-validation
CLIPPED_REQUIRE_MEDIA=1 cargo test --workspace
```

`require_media_tools()` returns `None` after printing `SKIPPED (media): …` when
neither the pinned build nor `PATH` has FFmpeg, so a checkout without it reports
that it validated nothing instead of passing quietly. `CLIPPED_REQUIRE_MEDIA=1`
turns that skip into a failure, which is the same lever `CLIPPED_REQUIRE_CAPTURE`
and `CLIPPED_REQUIRE_ENCODER` give the other subsystems.

CI sets it on the `Test` step. These tests link nothing — they run `ffprobe.exe`
and `ffmpeg.exe` as subprocesses — so a fetch-script or cache regression that
left the libraries but not the programs would still compile, and without the
variable every test proving the harness *detects* anything would skip and leave
the run green.

### What it cannot do yet

- **Measure an A/V offset against the source, from a file.** `synchronised_within`
  compares the tracks against *each other* — where they start, and where they end
  after any drift — which is a container-level check. The real offset is measured
  in `tests/capture/av_sync.rs` instead, by reading the frame counter
  `video-pattern` draws into each frame and comparing it with the onset of the
  tone that application now plays at the moment it presents that frame
  ([av-sync.md](av-sync.md)); it uses this harness's `AudioContent::magnitude_at`
  for the tone analysis and everything else in it is capture timestamps rather
  than a file. Doing the same thing to a produced recording is
  [issue #151](https://github.com/wildware-uk/clipped/issues/151).
- **Replace the tone analysis in `crates/audio/tests/system_audio.rs`.** That
  test has its own Goertzel filter, written before this harness existed and
  measuring interleaved capture buffers rather than a decoded file. It is the
  same technique and should become one implementation:
  [issue #152](https://github.com/wildware-uk/clipped/issues/152).
- **Check the NVENC bitstreams.** `crates/encoder`'s hardware tests still run
  `ffprobe` themselves, over raw elementary streams rather than containers:
  [issue #154](https://github.com/wildware-uk/clipped/issues/154).

## What is not built yet

AGENTS.md section 26 names four test applications. Two exist. The other two are
audio, and they were left out on purpose rather than stubbed:

- `test-apps/audio-generator` and `test-apps/process-tree-audio` need
  `clipped-audio` to exist to be worth anything — an audio generator with no
  audio capture to test would be written against a guess at what M2 needs and
  rewritten by the first test that used it, and a directory of empty programs is
  worse than two good ones. They are
  [issue #136](https://github.com/wildware-uk/clipped/issues/136), in M2, with
  the tone plan from AGENTS.md section 26 (440 Hz, 880 Hz, 1320 Hz) written into
  its acceptance criteria.

Anything else this document describes is built. Where it describes something
that is not, it says so — a document that quietly describes intentions as facts
is worse than a short one (AGENTS.md section 7).
