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
```

`--help` lists every option. A release build is not needed — a debug build
presents 2560x1440 at 60 fps on this project's development machine — but if a
run reports fewer frames than it was asked for, build with `--release` before
concluding anything about capture.

### The protocol a test drives it through

```text
ready hwnd=0x00000000003b0c62 client=2560x1440 fps=60 presentation=borderless exclusive=no monitor=\\.\DISPLAY1
stopped frames=300 reason=deadline
```

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

**Windows often refuses the exclusive transition.** `SetFullscreenState` needs
the foreground, and Windows does not grant the foreground to a process the user
has not interacted with — which includes anything a test started, and anything
started from a terminal that is not itself in the foreground. The application
says which it got (`exclusive=yes` or `exclusive=no`), warns on standard error
when it was refused, and carries on as a borderless window covering the display.
Every measured run on this project's machine so far has been refused with
`DXGI_ERROR_NOT_CURRENTLY_AVAILABLE`, which is the same result
`docs/capture-pipeline.md` records for the capture probe. Do not read
`exclusive=no` as a defect in the application, and do not read a passing test as
proof that exclusive fullscreen capture works — read the field.

## The capture tests

`tests/capture/` holds the tests that point a real capture backend at these
applications:

| Test | What it decides |
| --- | --- |
| `wgc_video_pattern.rs` | That a borderless window and an ordinary bordered window are both captured frame for frame: dropped, duplicated, out-of-order and torn frames are counted *and* asserted on, and the checker that does it is itself tested without a GPU |
| `wgc_fullscreen_dx11.rs` | That an application covering a whole display is captured, that every frame that arrives is the pattern, and that the display is the shape it was afterwards |
| `readback.rs` | Not a test: the helper that copies a captured GPU texture into system memory so the others can look at it |

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
```

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
| A track another source bled into | `a:1 isolation: 440 Hz belongs to another source and must not be audible here, but it measures 0.1250 against this track's own 1320 Hz at 0.1250 — 1.0x apart` |
| A track nothing was ever routed into | `the track is silent (peak amplitude 0.00e0 over 2.00s of audio)` |

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

### What it cannot do yet

- **Measure an A/V offset against the source.** `synchronised_within` compares
  the tracks against *each other* — where they start, and where they end after
  any drift — which is a container-level check. Measuring the real offset means
  reading the frame counter `video-pattern` draws into each frame and comparing
  it with a tone onset, and that needs an audio generator to record alongside
  it: the applications for that are [issue
  #136](https://github.com/wildware-uk/clipped/issues/136), and the measurement
  itself is [issue #151](https://github.com/wildware-uk/clipped/issues/151).
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
