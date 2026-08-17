# Audio routing

**Status: a recording of a window now assembles them — a game track, an
everything-else track and a microphone track, plus the compatibility mix.**
[Issue #19](https://github.com/wildware-uk/clipped/issues/19) built system audio
capture: `clipped-audio` can record the output device Windows is playing
through, as a continuous, timestamped stream of `f32` samples.
[Issue #20](https://github.com/wildware-uk/clipped/issues/20) added microphone
capture beside it, on the same engine, and
[issue #26](https://github.com/wildware-uk/clipped/issues/26) added the one this
product exists for: **everything one game's process tree plays, and nothing
else**. [Issue #29](https://github.com/wildware-uk/clipped/issues/29) added the
other end of the model — the **compatibility mix**, the single track a player
that takes one arbitrarily should take — which is the only place in Clipped where
sources are deliberately combined.
[Issue #27](https://github.com/wildware-uk/clipped/issues/27) added the
complement — everything the machine played *except* one process tree — and with
both modes present, `clipped_session::audio` now decides which captures a
recording opens and which track each one feeds.

**The rule is that the two scoped captures are opened together or not at all.**
A recording of a window opens the include mode against that window's process tree
and the exclude mode against the same tree, so every sample the machine played
lands on exactly one of the two tracks. Opening one alone would leave the other
half unrecorded; opening a scoped capture beside a whole-endpoint one would put
the game's audio on two tracks, which nobody discovers until they mute the game
in an editor and it is still audible. A recording with no process to scope to —
a monitor capture, or a window whose process has already exited — records the
whole endpoint on the other-system-audio track instead, which is coarser rather
than broken.

What is still to come is routing a *named* application to a track of its own
([issue #33](https://github.com/wildware-uk/clipped/issues/33)); the sections at
the end that describe it are unwritten because describing behaviour before it
is built produces a page that is wrong from the day it is committed (AGENTS.md
section 7).

So this document describes three captures, one process tree and one mixer: what
they do, the problems they exist to solve, what they convert, what happens when
the user changes their audio device mid-recording, how a game's process tree is
resolved and kept current, what goes into the compatibility mix and what it
refuses, and how to check any of it for yourself. Most of it is written about
system audio because that is where the machinery was built and where it is
easiest to describe; the [Microphone](#microphone) and
[The game's own audio](#the-games-own-audio) sections say what is different
about the other two, and everything not listed there is the same code. The
intended end state is SPEC.md sections 11 to 14, the constraints are AGENTS.md
section 21, and the decision that shapes the whole subsystem is
[ADR 0003](adr/0003-process-specific-audio-capture.md).

## What it does

```rust
let mut capture = SystemAudioCapture::open()?;
loop {
    match capture.read(Duration::from_millis(100))? {
        Capture::Samples(audio) => { /* audio.samples(), audio.timestamp() */ }
        Capture::Idle => {}
        Capture::FormatChanged(format) => { /* see "Device changes" */ }
    }
}
```

Every buffer carries interleaved `f32` samples, the format they are in, the
performance-counter position of the first frame, and whether the samples came
from the endpoint or were synthesised. Consecutive buffers are **exactly
contiguous**: each timestamp is the previous timestamp plus the previous
buffer's duration, with no gaps and no overlaps, for the whole life of the
capture. Nothing downstream has to reconcile anything.

## Why it exists in this shape

Two properties of WASAPI loopback drive almost every decision in
`crates/audio`.

### Loopback delivers nothing while the endpoint is silent

Not silent buffers — nothing. `GetNextPacketSize` returns zero for as long as
no application is rendering, and then packets resume as though no time had
passed. A capture that concatenates what it is handed therefore produces a
track shorter than its recording by exactly the amount of silence in it, and
every sound after the first quiet passage lands too early. The error is
cumulative: it looks perfect for the first minute and is minutes out by the end
of a long session. It is the single most common way loopback capture is got
wrong.

The fix is to fill the gap with silence, and the whole difficulty is deciding
how much. `crates/audio/src/timeline.rs` holds one anchor and one frame count,
and compares every packet's own reported position against where the timeline
expects it:

| The packet's position is | What happens |
| --- | --- |
| later than expected, by more than the deadband | that much silence is emitted in front of it |
| earlier than expected, by more than the deadband | the overlapping frames are trimmed off its front, or it is dropped if it is entirely inside covered time |
| within the deadband either way | it is emitted unchanged |

Because the comparison is always against the anchor rather than against the
previous packet, an ignored difference is still there next time; the deadband
bounds the offset instead of letting it accumulate. The deadband is 20 ms,
which is about two thousand times the packet-to-packet jitter measured on
Windows 11 build 26200 and well under a perceptible synchronisation error.

Left alone, a device whose sample clock genuinely differs from the performance
counter would be corrected in one 20 ms step roughly once an hour rather than
continuously — inside the deadband on every single packet, and still 20 ms out
an hour later, because the same small ignored error is added every time.
[Issue #30](https://github.com/wildware-uk/clipped/issues/30) removed that
step for a *steady* clock error: `Timeline::correction_ratio` measures the
rate the offset has been growing at since the timeline was last known to be
right, and `crate::resample::LinearResampler` applies it to every real
packet's own samples, by linear interpolation, before the packet ever reaches
the table above. A source running a few parts per million fast or slow is
therefore nudged continuously, by a resampling ratio too small to hear, and
the correction that used to be one 20 ms event an hour is now nothing a
listener could point to. The table above still governs — a real gap or a
device change is not a rate to track, it is silence to fill or samples to
trim — but a steady clock no longer needs either. The rate estimate itself
resets whenever the table above fires, because an ignored offset erased by a
step correction, or a gap, may describe a different piece of hardware than the
one the estimate was measuring; `crates/audio/src/timeline.rs` and
`crates/audio/src/resample.rs` are the arithmetic and the reasoning behind it,
respectively.

While an endpoint is quiet the timeline is topped up to `now - 60 ms` rather
than to `now`, because audio for the last few milliseconds may still be inside
the endpoint's buffer and filling over it would mean trimming real samples away
again. The silence is handed over in 100 ms instalments, so a consumer that
stalled for a minute does not cause a minute of zeroed samples to be allocated.

### The default endpoint can move without anything failing

A capture client keeps working when its endpoint stops being the default. That
is the trap. The user plugs in a headset, Windows moves the default, every
application follows — and a naive capture carries on recording the speakers,
which now receive nothing. No call fails. The recording is silent from that
moment and nothing says so.

So `crates/audio/src/windows/notifications.rs` registers an
`IMMNotificationClient` and watches for the console default on the side of the
audio stack this capture is on moving, for the captured device leaving the
active state, and for it being removed. A capture on a device the user chose
watches only the second and third of those: see [Microphone](#microphone).
Unplugging is also caught from the other side, because it invalidates the
client and the next WASAPI call returns `AUDCLNT_E_DEVICE_INVALIDATED`. Either
route ends the stream and opens a new one on whatever is default now.

## Timestamps

Every timestamp is a performance-counter reading **the audio device supplied**,
in nanoseconds: the QPC position `IAudioCaptureClient::GetBuffer` attaches to
each packet. `AudioTimestamp` has no `now()`, for the same reason
`clipped_capture::CaptureTimestamp` has none — a timestamp taken when a buffer
reaches this process encodes this recorder's scheduling jitter rather than the
endpoint's clock, and is worst exactly when the machine is busiest.

It is the same clock a captured video frame is stamped on, so audio and video
timestamps can be subtracted from one another directly.

Every buffer carries **two** accounts of the same moment, and the difference
between them is how far the track has slid against the reference clock.
`CapturedAudio::timestamp` is where the *track* puts it — the anchor plus every
frame emitted since, so the track is contiguous and as long as the recording —
and `CapturedAudio::device_timestamp` is where the *endpoint* said it belongs,
adjusted for any frames trimmed off the front of the packet. The first advances
at the endpoint's sample rate and the second at the performance counter's, so
the way the gap between them grows is the way the audio slides against the
picture. It is a difference and not an absolute: the track is anchored on the
first packet's own device position, so it says nothing about any constant offset
the two accounts already had. Synthesised silence has no `device_timestamp`,
because it covers a period the device never described.

[av-sync.md](av-sync.md) is the model that consumes this: which clock a
recording is timed against, where the conversion to a media time happens, what
happens on a gap or a step, and the drift measured on real hardware.

There is exactly one place that reads the counter itself: measuring how long
the endpoint has been saying nothing, which is a period the device will never
describe. Anything that estimates is reconciled against the device's own
position as soon as it speaks again.

One measurement worth knowing, taken on Windows 11 build 26200: a loopback
packet's reported position is about one device period (10 ms) *ahead* of the
moment it is read, because it is the time the endpoint will render the audio
rather than the time it was captured. The timeline anchors on the first real
packet, so this is a property of the stream rather than an offset this crate
introduces.

## Formats: what is converted, and what is not

**Sample format is converted. Sample rate and channel count are not.**

The endpoint's mix format is whatever `IAudioClient::GetMixFormat` says. On the
machines this was developed against that is 48 kHz stereo 32-bit float, and
nothing assumes it: 16-, 24- and 32-bit integer endpoints, 24 valid bits in a
32-bit container, 44.1 kHz, 5.1, and formats with no extensible part are all
handled, and anything else is refused with its numbers in the message rather
than reinterpreted. Reinterpreting sample data produces full-scale noise, not a
quiet mistake.

Every buffer leaves the crate as interleaved `f32` in `[-1.0, 1.0]`. The
integer conversions divide by negative full scale, so no sample can leave the
range and nothing downstream has to clip. The sample rate and channel count are
passed through untouched, and the endpoint's `dwChannelMask` is reported as a
`ChannelMask` so that a 5.1 recording is not labelled by guesswork.

**Sample rate and channel count are still untouched, but every source's own
samples are now nudged by a fraction of a percent to stay aligned with the
reference clock** (issue #30, [above](#loopback-delivers-nothing-while-the-endpoint-is-silent)).
That is a correction against drift within one source's declared rate, not a
conversion between two different declared rates: two sources captured at
genuinely different sample rates still cannot be combined without a real
resampling stage, which is why the compatibility mix still refuses one (see
[What it will not do](#the-compatibility-mix)). Downmixing is a decision about
what the user hears, which this crate is not entitled to make on its own
(AGENTS.md section 21).

## Device changes during a recording

The rule is that **a recording is worth more than the audio it is missing**. No
device event ends a capture (AGENTS.md sections 16 and 17); the only thing that
stops one is the caller.

Every line below carries `audio_source=system_audio` or
`audio_source=microphone`, which is how the two captures a recording runs are
told apart in one log. The field and its permitted values are `docs/logging.md`,
not words chosen here.

| What happens | What the capture does |
| --- | --- |
| The default output device changes to another device with the same sample rate and channel count | reopens on it, logs one `info` line naming the reason, and fills the outage with silence |
| The device being recorded is unplugged or disabled | the same, whether the news arrives as a notification or as `AUDCLNT_E_DEVICE_INVALIDATED` |
| There is no output device at all | logs a `warn`, produces silence, and looks again every two seconds until one appears |
| The default moves to a device with a **different** sample rate or channel count | logs a `warn`, reports `Capture::FormatChanged` once, and produces silence until the caller restarts the capture or the user selects a usable device again |
| Windows cannot open the new device | logs a `warn` and retries every two seconds |
| The device opens and then fails at once, over and over | logs a `warn` and leaves it alone for two seconds before trying again, rather than reopening it immediately |

That last row is not a nicety. A device that opens and then fails on the first
call — a sound card on its way out — is the one failure that can be reopened
faster than it fails, and reopening it with no delay is an `Activate`,
`Initialize`, `Start`, fail, repeat loop with nothing to end it: a read that
never returns, a core at 100%, and a log growing for as long as the recorder
runs. A stream that has been running for longer than half a second still
reopens with no delay, because that is the ordinary case and the delay would be
silence in somebody's recording.

The last row of the second kind — a different sample rate — is the one
compromise. A track's format is fixed when the capture opens, because a muxer
that has written a stream header cannot be handed 44.1 kHz halfway through, and
this crate has no rate-conversion resampler: issue #30 keeps one source's own
clock aligned with the reference clock over a long recording, which is a
different problem from converting between two endpoints that disagree about
the shape of a frame. Changing shape underneath the caller would be worse than
silence, and ending the recording over a headset would be worse still, so the
capture says what happened, keeps the timeline running, and waits.

Opening is the one asymmetry: `SystemAudioCapture::open` fails with
`AudioError::NoEndpoint` on a machine with no output device, because there is
then no format to give a track and no recording in progress to protect.

## Microphone

A microphone is a WASAPI *capture* endpoint rather than a render endpoint
recorded in loopback mode, and that is the entire technical difference. So it is
not a second implementation: `src/windows/endpoint_capture.rs` is the engine
described everywhere above — the mix format, the device clock, the silence that
keeps a track the same length as its recording, the endpoint that is unplugged
mid-recording, the endpoint that fails the instant it opens — and
`src/windows/loopback.rs` and `src/windows/microphone.rs` are the two public
captures that tell it which endpoint to open and with which stream flags
(AGENTS.md section 55).

```rust
for microphone in clipped_audio::windows::microphones()? {
    println!("{} {}", microphone.name(), microphone.id());
}

let mut capture = MicrophoneCapture::open(&MicrophoneSelection::SystemDefault)?;
// ... or MicrophoneSelection::device(id, name) for one the user chose.
```

`read`, `format`, `stats` and `close` behave exactly as they do for system
audio, and the buffers are the same contiguous, timestamped `f32` on the same
clock, so a microphone track can be compared with a video frame or with the
system audio track by subtracting timestamps.

Four things about a microphone are genuinely different.

**It may not be there at all.** Every machine that plays sound has a render
endpoint; plenty have no microphone. `MicrophoneCapture::open` therefore reports
`AudioError::NoMicrophone`, or `AudioError::MicrophoneUnavailable` naming the
device that was chosen — "the microphone Clipped is set to record (Shure MV7) is
not connected. Connect it, or choose a different microphone" — because a message
a user can act on is the difference AGENTS.md section 45 asks for.

**It goes away far more often than a speaker does.** A headset leaves the desk
with the person wearing it. That must not end a recording, and it does not: the
track becomes silence of exactly the right length, the capture keeps looking for
the device every two seconds, and the timeline stays contiguous across the
outage. Every row of the table in [Device changes during a
recording](#device-changes-during-a-recording) applies unchanged, on the input
side.

**A chosen microphone is waited for, not replaced.** `MicrophoneSelection::
SystemDefault` follows the default input device when Windows moves it, which is
what a user who has never opened the settings expects. A selection made with
`MicrophoneSelection::device` does not: unplugging a chosen headset makes
Windows promote whatever is left — often a webcam on the other side of the room
— and a track that silently became that would be worse than a silent one. The
identifier a choice is stored as is `Microphone::id`, which Windows keeps stable
across reboots; storing it between sessions belongs to the configuration API,
[issue #108](https://github.com/wildware-uk/clipped/issues/108), and is not done
yet.

**Windows can mute it.** A muted microphone still delivers packets — of silence,
flagged as such — so a recording of one looks perfectly healthy and contains
nothing. It is the commonest reason a microphone track is silent, and it is
invisible from the stream, so the capture activates the endpoint's volume
interface and `MicrophoneCapture::is_muted` reports the switch. It is `None`
when there is no device open or when Windows will not answer for it, which some
virtual devices do not. One `warn` line is logged when a capture opens on a
muted device; the switch is not polled during recording, because Windows tells
nobody when it moves and a COM call in the capture loop would cost more than it
is worth.

### Privacy

A microphone hears the room, so its samples are the most private thing Clipped
handles (AGENTS.md section 13). Nothing in `clipped-audio` writes them anywhere,
and no log line is derived from their values: the diagnostics count frames, name
devices and measure durations. `CapturedAudio` — the one type that carries the
samples out of the crate — prints as a description of the buffer rather than as
its contents, so the guarantee survives the first consumer that writes
`tracing::debug!(?buffer)` instead of depending on nobody ever doing so. The one
number that comes from the samples is the
peak level `examples/microphone_probe.rs` prints once a second, which exists so
that "the track is silent" can be told apart from "the track is quiet", is
thrown away as soon as it is printed, and is never logged.

The tests in `src/windows/microphone.rs` open the machine's real microphone for
a second or two at a time and assert on frame counts, timestamps and whether
silence is zero. None of them keeps a sample, writes one, or looks at what was
said.

## The game's own audio

**This is the feature.** Everything one game's process tree plays, captured on
its own, so that the game can be turned down in an edit days later without
touching the voice chat that was playing over it
([ADR 0003](adr/0003-process-specific-audio-capture.md)).

```rust
let mut capture = ProcessLoopbackCapture::open(game_process)?;
while capture.target_is_running() {
    match capture.read(Duration::from_millis(100))? {
        Capture::Samples(audio) => { /* the game, and only the game */ }
        Capture::Idle | Capture::FormatChanged(_) => {}
    }
}
capture.finish(); // then read until `NotOpen`: see "Ending a capture" below.
```

`read`, `format` and `stats` behave exactly as they do for system audio, and the
buffers are the same contiguous, timestamped `f32` on the same clock. Five
things are different.

**It is not on a device.** A process-scoped client is activated through
`ActivateAudioInterfaceAsync` against a *virtual* device no enumerator lists,
with the target process in an `AUDIOCLIENT_ACTIVATION_PARAMS` blob, and the call
is asynchronous: it returns an operation object and calls a completion handler
this crate implements. `crates/audio/src/windows/activation.rs` is that call and
nothing else.

One trap is worth stating because it costs an afternoon. The `PROPVARIANT` that
carries the blob **borrows** it, and windows-rs implements `Drop` for
`PROPVARIANT` as `PropVariantClear`, which for a `VT_BLOB` frees `pBlobData`.
With a stack blob that is `CoTaskMemFree` on a stack address: the process dies
with `STATUS_HEAP_CORRUPTION` the moment an activation succeeds. It is held in a
`ManuallyDrop` for that reason.

**Nobody says what shape the audio is.** There is no endpoint, so there is no
mix format — `GetMixFormat` is not available on this client and the format is
Clipped's choice, which the audio engine converts into. The choice is the
default output endpoint's sample rate and channel count as 32-bit float, so that
the game track can sit in one file beside the system-audio track without a
resampler (issue #30); 48 kHz stereo is asked for if the engine refuses that or
the machine has no output device. Whichever is accepted is **fixed for the life
of the capture**, so a stream reopened mid-recording cannot change a track's
shape underneath a muxer that has written a stream header.

**The target is a tree, and Windows takes one root.**
`PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE` includes the process named
and the ones it started — including ones started *after* the capture was opened,
which the isolation test asserts by starting the process that makes the noise
only once the capture is running. Clipped tracks the same membership from this
side with [the game's process tree](#the-games-process-tree), for the two
questions the activation cannot answer:

| What happens | What the capture does |
| --- | --- |
| A process of the game starts or exits | nothing: Windows follows the tree itself |
| The process the activation names exits with descendants still running | re-scopes onto a surviving member and activates again, logging one `info`; the outage is filled with silence |
| …and more than one member survives, in separate trees | re-scopes onto the lowest-numbered one and logs a `warn` naming the members whose audio is therefore not in this track ([issue #311](https://github.com/wildware-uk/clipped/issues/311)) |
| The game and everything it started exit | `target_is_running` becomes `false`, one `info` is logged, and the track continues as silence until the caller stops it |
| A process of the game cannot be opened | it is not a member — an unpinned identifier is not one to scope a capture to — and its name is logged at `debug` |

The tree is rooted at the game rather than at the launcher, for the reason
[The tree is rooted at the game](#the-tree-is-rooted-at-the-game-not-at-the-launch)
gives: Steam's notification chime does not belong in a track named after a game.

**Ending a capture drains it.** The audio engine holds up to 200 ms of captured
audio; closing a capture throws that away, which is the last fraction of a
second before somebody stopped recording — often the part they pressed the key
for. `finish()` therefore leaves the capture readable and stops it looking
forwards: the queued packets are handed over on the same timeline as everything
before them, and then the capture closes itself and `read` reports `NotOpen`.
The client is deliberately **not** stopped first. Measured on Windows 11 build
26200, a process-scoped stream stopped after a 150 ms stall reported no queued
packets at all, where the same stream drained before stopping produced the
150 ms.

**It may not be available at all.** Process loopback is documented from Windows
build 20348, which no shipping Windows 10 release reaches, and
`AudioError::ProcessLoopbackUnavailable` is what a machine below that floor
produces. It is its own error precisely so a caller can tell it apart and take
the documented fallback: **record one system-audio track and say that per-source
separation is unavailable on this machine.** What must never happen is a track
labelled "Game" that is really everything the machine played (ADR 0003's second
consequence, AGENTS.md section 27). The message names the build number and the
fallback, so a user whose tracks all came out identical can find out why.

The floor itself is still unconfirmed on real hardware — this has only been run
on Windows 11 build 26200, where it works — so
[prerequisites.md](prerequisites.md) does not yet state a minimum version for
it.

## The game's process tree

A game is not one process. Steam starts a launcher, the launcher starts the
game, an anti-cheat wrapper may sit between them, some titles re-execute
themselves once, and the process that renders the audio is often not the one
whose window is being captured. So "capture the game's audio" means capturing a
*set* of processes — and because "other system audio" is defined as the
complement of that set, a process missed from it does not merely lose its audio:
it puts game audio into the system track, where nobody notices until they open
the file in an editor days later
([ADR 0003](adr/0003-process-specific-audio-capture.md)).

The set lives in `clipped_windows::ProcessTree`, one layer below this crate.
That is where it belongs rather than a convenience: it holds no audio concept at
all — it opens process handles, reads the process table and compares creation
times — and the same facility is what a session needs to know a game is really
gone. `crates/windows/src/process_tree.rs` is the code, and this section is what
it is for.

### It is not the detection walk

`clipped-game-detection` also follows parent chains, for
[issue #41](https://github.com/wildware-uk/clipped/issues/41)'s launch debounce,
and the two are asking different questions. Detection asks **did a game start**:
it collects a burst of process starts into one launch, reports it, and stops
caring. This asks **which processes are it, right now** — membership with a
lifetime, maintained for the whole of a recording, answered as a list of
identifiers that goes to `ActivateAudioInterfaceAsync`. Neither answer is
derivable from the other, and the second one has to survive things the first
never sees: a helper started an hour in, and a launcher that exits while the
game it started carries on.

### A member is a handle, not a number

Windows reuses process identifiers, often within seconds on a busy machine. A
tree that remembered numbers would, over a long session, eventually scope a
game's audio to whatever inherited a dead helper's identifier.

So a tree holds an **open handle to every member**. The kernel keeps an
identifier reserved for as long as any handle to the process object exists —
even after the process has exited — so an identifier this tree is holding cannot
come to mean anything else. That one decision pays for three things at once:

- **Exits cost nothing to notice.** A wait of zero on a handle already held says
  whether the process has gone. No search of the process table, and no
  comparison of two lists.
- **A launcher can exit without taking its children out.** Windows does not
  re-parent orphans; it leaves them naming a process that no longer exists. A
  fresh walk from the root would lose them. Membership here is *sticky* and the
  dead parent's identifier is still pinned, so its orphans are still reachable —
  the exited member is kept as a ghost until nothing living descends from it,
  and only then is its handle released.
- **Adoption can be verified.** See below.

The handle is opened with `PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE` and
nothing more, which is the least Windows will answer both questions for.

### Identifier reuse, and the two comparisons that defeat it

Pinning protects identifiers the tree already holds. What is left is *adoption*:
the process table says process C's creator is member P, but the table is a copy,
and by the time C is opened it may be a different process wearing the same
number — or C's real creator may be a long-dead process that held P's number
before P did. The table records a creator's identifier and never says whether it
still means anything.

Both are settled by creation times, which Windows guarantees are ordered:

| Rule | Rejects |
| --- | --- |
| C started no later than the moment the process table was read | a process that took C's identifier after the table was copied |
| C started no earlier than the parent it claims | a process whose real creator held the parent's identifier before the parent did |

The moment is read *before* the table rather than after, which is the safe
direction: a process created while the table was being copied is refused and
looked at again next time, whereas the other order would admit an identifier
recycled during the copy. No rejection is permanent — nothing remembers a
refusal — so the cost of being wrong is one interval of one process's audio in
the wrong track rather than an answer that stays wrong. That matters because
creation times are on the system clock rather than a monotonic one; a clock
adjustment mid-session costs one interval, not a misattribution.

### What the catalogue's `child_processes` means here: nothing

The game catalogue carries a list of executable names a game is known to spawn,
and deliberately does not match on it
([game-detection.md](game-detection.md)). It is not a membership key here
either, and for a stronger reason. A name cannot say *which* process it means:
admitting every process called `anticheat-service.exe` would put a service
shared by several games — or anything a user renamed — into one game's audio
track, which is precisely the silent misattribution the comparisons above exist
to prevent. Membership is kernel parentage, verified, or it is nothing.

Where the list *is* useful is the session layer, which already uses it for what
it is good for: keeping a session open while a game's known helper is still
running ([sessions.md](sessions.md)). If a game turns out to produce audio from
a process genuinely not descended from it, the answer is a second tree rooted at
that process deliberately, not a name match.

### The tree is rooted at the game, not at the launch

`clipped-game-detection` reports a launcher, any wrapper and the game as one
group. A tree takes one root, and it should be the game: the launcher is the
game's *parent*, and rooting there would put Steam's notification chime and a
launcher's autoplaying video into the track labelled with the game's name.

### How a change is noticed, and how quickly

There is no thread. `ProcessTree::refresh` does its work on the calling thread
and is the only thing that changes membership, so nothing can scan behind a
caller's back, and a caller may hold the tree wherever it holds the capture.

A call reads the process table at most once per **rescan interval**, one second
by default; a call inside that window does nothing and costs about 25 ns. So a
caller can refresh as often as it likes — an audio thread waking every few
milliseconds may simply call it on every packet — and membership is at most one
interval stale in both directions. **One second is the documented pickup
latency**: a helper started mid-recording is in the tree within a second of
appearing, and a member's exit is noticed within a second of happening.

Do not refresh from a video capture thread. A scan is milliseconds, not
microseconds (below), which is several frames at 60 fps; an audio thread working
against a 200 ms buffer can absorb one, a frame loop cannot (AGENTS.md section
20).

### What it costs

| | |
| --- | --- |
| Machine | AMD Ryzen 9 9950X3D, 16 cores / 32 logical processors, Windows 11 Pro build 26200 |
| Processes running | 412 |
| Build | `--release` |
| Method | `examples/process_tree_probe.rs`: `Instant` around each scan, `GetProcessTimes` for the probe's own processor time over the run |
| Machine state | in ordinary use, with other work running — which is why the maxima are far from the medians |

| Rescan interval | Scan: min / median / max | Processor time, % of one core |
| --- | --- | --- |
| 500 ms | 8.6 / 11.9 / 51.4 ms | 2.81 |
| **1 s (the default)** | **8.3 / 12.4 / 33.5 ms** | **1.26** |
| 2 s | 8.5 / 13.0 / 40.0 ms | 0.77 |

30-second runs, except the default, which is 60 seconds.

**Almost all of a scan is `CreateToolhelp32Snapshot`.** Two processes were
opened in the whole of each run — the tree gains its two extra members half way
through — so twelve milliseconds is the cost of asking Windows for a list of 412
processes, not the cost of deciding anything about them.

**At the default that is 1.26% of one core.** On the machine above that is 0.04%
of it; on a four-core machine it is nearer 0.3%, spent for the whole length of
every recording, against SPEC.md section 38's 3% budget for the entire recorder.
That is affordable and it is not nothing, and the honest reading is that the
mechanism is more expensive than the job: reading a 412-row list should not take
twelve milliseconds.
[Issue #288](https://github.com/wildware-uk/clipped/issues/288) is the way out —
measuring `NtQuerySystemInformation` against the snapshot call — rather than
tuning the interval, because halving the cost by doubling the interval doubles
how long a helper's audio spends in the wrong track.

The numbers are not exactly inverse in the interval, and the machine being busy
is why: the two-second run should be about 0.6% and measured 0.77%. Read the
column as *roughly one per cent of one core at the default, give or take a
quarter*.

Taking the measurements again:

```text
cargo run --release -p clipped-windows --example process_tree_probe -- 60
cargo run --release -p clipped-windows --example process_tree_probe -- 30 500
```

The first argument is how long to run for in seconds, the second the rescan
interval in milliseconds. The probe starts a three-process chain of its own, so
it needs no game and measures a tree that gains members rather than one that
never changes.

### What it does not cover

- **A process Windows will not let Clipped open is not a member.** It cannot be
  pinned, and an unpinned identifier is not one to scope a capture to. In
  practice this is a game's anti-cheat or crash-reporting *service*, which runs
  as the system account and plays nothing; `TreeChange::refused` names them so
  that a case where it is plainly part of the game can be reported rather than
  guessed at. `ProcessLoopbackCapture` logs them at `debug`.
- **Where audio that belongs to no tree ends up** is not decided here. It is a
  question about the complement capture, and it belongs to
  [issue #27](https://github.com/wildware-uk/clipped/issues/27) with an
  observation behind it (ADR 0003's last consequence).
- **Nothing has been tried against a real game.** The behaviour is asserted
  against a chain of processes the tests start themselves, and against
  `test-apps/process-tree-audio`, whose child plays a known tone. Anti-cheat
  wrappers and launchers do things test fixtures do not; a session that records
  a real game is
  [issue #22](https://github.com/wildware-uk/clipped/issues/22), and that is
  where this meets one.

## The compatibility mix

**The one place sources are deliberately combined, and it combines copies.**

Several audio tracks is the product, and it is also a shape some players handle
badly: handed a file with four audio tracks, a player that takes one arbitrarily
can land on the microphone, and the recording sounds broken to somebody who only
double-clicked it. SPEC.md section 13's answer is a mix of everything on track 1,
carrying Matroska's default flag, so casual playback sounds right while an editor
still sees every source on its own. `clipped-muxer` already puts that track first
and flags it ([issue #28](https://github.com/wildware-uk/clipped/issues/28));
`clipped_audio::Mixer` is what fills it
([issue #29](https://github.com/wildware-uk/clipped/issues/29)).

```rust
let mut mix = Mixer::new(format).anchored_at(epoch);
let game = mix.add_source(AudioSource::Game, game_format, Level::UNITY)?;
let voice = mix.add_source(AudioSource::Microphone, microphone_format, level)?;

// per packet, from whichever source produced it
mix.contribute(game, audio.timestamp(), audio.samples())?;
while let Some(block) = mix.take() {
    // block.samples() → the compatibility mix track
}
```

Everything about it follows from AGENTS.md section 21, which forbids silently
combining sources the user expects to stay isolated.

**It cannot touch a source's own track.** `contribute` takes `&[f32]`. There is
no path through the mixer that writes to a caller's buffer, so a level, a mute or
the limiter is visible in the mix and nowhere else — which is what makes it safe
to move a level *during* a recording, and what makes the isolated tracks worth
having when somebody turns the game down to hear themselves talk.

**Sources are placed, not appended.** A contribution's timestamp decides which
frames of the mix it is added to, so a microphone opened half a second after the
game lands half a second in and two sources that overlap are summed over the
frames they share. This is not a nicety: every source is on its own clock and its
own thread, and a mixer that concatenated what it was handed would produce a mix
of the same session with every source sliding against every other. Audio that
arrives after the mix has already passed the moment it belongs to is **counted
and dropped** rather than placed where the mix happens to be — it is still on
that source's own track, in full, which is the point of having isolated tracks.

**A source that produces nothing does not silence the rest.** The mix cannot emit
a frame until every source has had its chance at it, so the slowest source sets
the latency and a source that has *stopped* — a microphone Windows muted, a
capture whose device never came back — would set it to for ever. So the mix waits
for the slowest source for half a second and then carries on without it. What is
deliberately not done is dividing by the number of sources: a mix 12 dB quieter
because four tracks were declared and three are silent is a recording somebody
turns up and then finds is noisy.

**Clipping is prevented rather than allowed.** Two sources at −6 dBFS are exactly
full scale and three are past it. The mix is held under 0.99 by a peak limiter —
one gain per frame, applied to every channel of that frame together, dropping
instantly to whatever the frame needs and recovering towards unity over 200 ms —
so a loud passage is turned *down* rather than having its peaks sliced off. The
alternative, clamping each sample at ±1.0, squares off the waveform and produces
broadband harmonic distortion at exactly the moments a recording matters most;
`tests/compatibility_mix.rs` measures the difference at the third harmonic rather
than asserting it. There is no look-ahead, deliberately: a true brickwall limiter
delays the signal by a few milliseconds, and a mix that is late against the
picture to avoid an artefact nobody can hear is a bad trade.

**What it will not do.** It does not convert between sample rates, so a source
captured at a rate the mix is not being written at is refused when it is
*added* — before the recording starts, with a message saying so — rather than
dropped from the mix during it. [Issue #30](https://github.com/wildware-uk/clipped/issues/30)
keeps a single source's own clock from drifting against the reference clock
over a long recording ([above](#loopback-delivers-nothing-while-the-endpoint-is-silent));
it does not convert between two sources that were never at the same rate to
begin with, which is a genuine resampling stage this mixer still does not
have. Channel layouts are handled for the cases a recording actually produces:
channel for channel, a mono source spread across every channel of the mix, and
any source folded into a mono mix. A genuine downmix — 5.1 into stereo — needs
a coefficient table, which is a decision about what the user hears, and is
refused the same way for the same reason.

**Where it runs.** A `Mixer` is owned by one thread and holds no lock; it is
`Send` and not `Sync`. The alternative would be a lock every capture thread takes
on every packet, which AGENTS.md section 20 rules out on a capture path. Its
memory is bounded — it buffers at most two seconds, and hands blocks over in
100 ms instalments — and its per-buffer work is a multiply-add per sample with no
allocation once the accumulator has reached its steady size.

**What is not built.** Nothing writes the mix to a file yet: `clipped-session`
opens the captures and writes their tracks, and giving the compatibility track
its samples — along with the setting that turns the track off, which SPEC.md
section 13 asks for and which belongs with the rest of the configuration — is the
remaining half of issue #29. Until then a recording still declares a mix track
only when a session declares one, and this build's session does not.

## Threading

**One capture, one thread, and this crate does not create it.**

`read` blocks until it has something to report, so the caller supplies the
thread — as `clipped-capture` does for video, so that the session owns its
threads and can give them the priority and lifetime it wants. A capture is
`Send` so it can be built anywhere and moved onto that thread, and is not
`Sync`. A recording that captures system audio and a microphone runs two of
them, on two threads, sharing nothing: neither notices anything about the
other's device, which the tests assert.

What that thread waits on is deliberately short. Inside `read` there is one
blocking call: a wait on the event handle WASAPI signals when a packet is
ready, bounded by the caller's timeout and by a 100 ms slice so that silence
keeps flowing while the endpoint is quiet. There is no lock shared with
anything that does work, no allocation once the buffers reach their steady
size, no file, no logging above `debug` in the per-packet path, and nothing
that talks to the rest of the recorder (AGENTS.md section 20).

Device notifications arrive on a thread inside the Windows audio service.
Those callbacks take a mutex for two field writes and return — no allocation
beyond a device identifier, no logging, no audio call — and the capture thread
acts on the flag between reads. Opening an endpoint is the one operation that
can take more than a moment, and it happens there, never in a callback.

Event-driven loopback is used where the audio engine accepts it, which it does
on Windows 11 build 26200. Where it does not, the capture falls back to looking
at the packet queue every 5 ms and says so in the log.

## Ownership

Every native resource has one owner and one release point (AGENTS.md
section 58).

| Resource | Owner | Released by |
| --- | --- | --- |
| `IAudioClient`, `IAudioCaptureClient` | `Stream` | `Stream::drop`, which stops the stream first |
| The event handle WASAPI signals | `Stream`'s `WakeEvent` | its own `Drop`, which runs after the audio clients above have been released |
| `IAudioEndpointVolume`, for a microphone's mute switch | `Stream`'s `EndpointMute` | its own `Drop` |
| The `IMMNotificationClient` registration | `EndpointNotifications` | its `Drop`, which unregisters |
| `IMMDeviceEnumerator` | `EndpointCapture` | its `Drop` |
| The `WAVEFORMATEX` from `GetMixFormat` | `MixFormat` | its `Drop`, with `CoTaskMemFree` |
| The multi-threaded COM apartment | the process | nothing, deliberately — `src/windows/apartment.rs` |

Closing a capture and dropping one do the same thing, so a thread that unwinds
releases exactly what a clean stop would.

## Buffering, and what happens when the consumer stalls

Nothing in this process grows. The only buffering is the 200 ms the audio
engine holds for the stream, fixed when it is opened; this crate keeps one
converted packet and one 100 ms silence buffer, and neither grows with how long
a consumer has been away.

When a consumer stalls for longer than 200 ms the audio engine discards the
oldest data and flags the discontinuity. That is real audio lost, and it is
reported as such — `CaptureStats::discontinuities` counts it — but the *time*
is not lost: the gap comes back as silence of exactly the right length, from
the device's own positions, so the track stays aligned with the video rather
than sliding forward by however long the consumer was away.

## How to run it

```text
cargo run -p clipped-audio --example loopback_probe -- 60
```

The probe opens the same capture the recorder will, installs Clipped's logging
so endpoint changes appear as they will in a session, and prints a line a
second: frames captured, seconds of audio, how much of it was synthesised
silence, endpoint changes, discontinuities, peak level and the device name.

The game's own track has one too, and it is how a machine is asked whether it
can do process-scoped capture at all:

```text
cargo run -p clipped-audio --example process_loopback_probe -- 30
cargo run -p clipped-audio --example process_loopback_probe -- 30 12345
```

With no second argument it scopes the capture to *itself*, which plays nothing,
so it makes no sound and still answers the questions that matter on a shared
machine: whether the activation works on this build, what shape the audio engine
accepted, whether packets arrive and whether their positions are
performance-counter readings. Give it a game's process identifier to watch a
real track — `peak` staying at zero while a game is plainly making a noise is
the failure it exists to find — and it ends by draining, printing how much audio
the engine still held.

The process tree has a probe of its own, which measures rather than watches:

```text
cargo run --release -p clipped-windows --example process_tree_probe -- 60
```

See [What it costs](#what-it-costs) for what it reports and what the numbers
were on the machine this was written on.

The loopback probe is the tool for the two behaviours no automated test can
reach on an ordinary machine, because they need a hand on a cable:

- **Unplug or switch off the output device.** The frame count must keep rising,
  `silence` must start growing, and `endpoint` must become `<none>`. Plug it
  back in and there should be one endpoint-change log line and the new device's
  name.
- **Stop everything that plays audio.** The frame count must keep rising at the
  sample rate — that is silence WASAPI is not delivering being synthesised.

A run in which the frame count stands still is a recording whose audio track
would be shorter than its video.

**This procedure has not been carried out yet.** It is written here because it
is what has to happen, not because it has happened: no output device has been
physically unplugged on a machine running this code, so Windows actually
dispatching to the registered `IMMNotificationClient` — as opposed to the
callbacks doing the right thing when they are called, which is unit tested — is
unverified. [Issue #141](https://github.com/wildware-uk/clipped/issues/141)
tracks doing it and recording what was seen. The
`AUDCLNT_E_DEVICE_INVALIDATED` route into the same reopen is the reason a
recording survives an unplug even if the notification never arrives.

The microphone has the same tool, and the same gap:

```text
cargo run -p clipped-audio --example microphone_probe -- 60
cargo run -p clipped-audio --example microphone_probe -- 60 "{0.0.1.00000000}.{…}"
```

It lists the machine's microphones with their identifiers, records the default
one — or, with a second argument, the one that identifier names — and prints the
same line a second with two more facts on it: whether Windows has the device
muted, and a peak level so that a silent track can be told from a quiet room.
Nothing it captures is written anywhere.

- **Unplug the microphone.** The frame count must keep rising, `silence` must
  start growing and `device` must become `<none>`. Plug it back in and there
  should be one device-change log line and the microphone's name again.
- **Mute the microphone in Windows.** `muted` becomes `yes` and `peak` falls to
  zero: the pair of facts that answers "why is my microphone track silent".
- **Change the default microphone in Windows' sound settings.** With no second
  argument the capture must move to the new device. With one, it must *not*:
  a chosen microphone is waited for, never replaced.

**These have been run only in the sense of watching the probe record.** What has
been seen here is a run against the default microphone and a run against a
chosen device by identifier, both producing audio at the sample rate with no
synthesised silence. No microphone has been physically unplugged on a machine
running this code, and Windows dispatching to the registered
`IMMNotificationClient` is unverified for the same reason it is for the output
side — see issue #141, which covers both.

## How to test it

```text
cargo test -p clipped-audio
```

- **The arithmetic**, in `src/timeline.rs`: silence of the exact length of the
  gap, jitter absorbed, ignored jitter not accumulating into drift,
  over-synthesised silence trimmed back out, buffers exactly contiguous across
  all of it, the drift-correction ratio staying at `1.0` until enough history
  has accrued to trust it and clamped rather than following a bad measurement
  once it is, and a real gap resetting the estimate rather than extending it.
  These run anywhere, including on a machine with no sound card.
- **The resampler**, in `src/resample.rs`: a ratio of `1.0` reproducing its
  input exactly, including across a packet boundary; a ratio below or above
  `1.0` producing correspondingly fewer or more frames; a long run at a steady
  ratio converging on `frames * ratio` rather than drifting away from it; a
  reset discarding carried state rather than blending it into the next
  packet. Also machine-independent.
- **The conversions**, in `src/format.rs` and `src/windows/endpoint.rs`: every
  endpoint sample format converting to the same amplitude, 24-bit sign
  extension, the mix format Windows actually reports here, a 44.1 kHz 5.1
  24-in-32 endpoint, and formats that are refused.
- **The real output endpoint**, in `src/windows/loopback.rs`: a contiguous
  timeline over two seconds of real capture; a 600 ms consumer stall producing
  bounded buffers and silence of the length the audio engine could not hold; an
  endpoint change not ending the recording; and an endpoint that fails as soon
  as it opens being backed off rather than reopened in a loop — that one reads
  on a second thread, because the regression it guards against is a `read` that
  never returns and a hung test says nothing.
- **The real microphone**, in `src/windows/microphone.rs`: a contiguous timeline
  over a second and a half of real capture, with synthesised silence asserted to
  be zero; a device change not ending the recording; a device that stops
  answering — which is what unplugging a USB microphone does — still producing a
  track of the right length, made of silence; that failure being *recognised*,
  so that `AUDCLNT_E_DEVICE_INVALIDATED` does not appear in the log as an
  unexplained fault while anything else does; a chosen device being reopened by
  its identifier rather than replaced; a microphone that is not connected being
  reported by name with something to do about it, which needs no audio hardware
  and so runs in CI; the list of microphones naming at most one default; and the
  microphone and system audio running at once on different devices without
  either disturbing the other.

  The two tests that need a device to fail open a real endpoint and then return
  a chosen `HRESULT` in place of the one
  `IAudioCaptureClient::GetNextPacketSize` returned, so the classification in
  `Stream::lost` and everything after it is the code a real unplug runs. What
  no test covers is Windows returning that `HRESULT` in the first place — the
  same gap as the notification callbacks, and the same issue: #141.
- **The game's own track**, in `src/windows/process_loopback.rs`. Four of these
  capture a process tree that **plays nothing** — this test process, or a
  `cmd.exe` chain the test starts — so they make no sound at all and are not
  suppressed by `CLIPPED_SKIP_AUDIO`; what they need is a Windows that can scope
  a capture to a process, and where it cannot they skip loudly rather than
  failing. **That is a property of the machine, not of CI** — this page used to
  say a GitHub runner cannot do it and that they skip there, which is false
  ([#441](https://github.com/wildware-uk/clipped/issues/441)): the CI failures
  behind [#341](https://github.com/wildware-uk/clipped/issues/341),
  [#387](https://github.com/wildware-uk/clipped/issues/387) and
  [#425](https://github.com/wildware-uk/clipped/issues/425) quote measured track
  lengths, which a skipped test cannot produce. They assert a contiguous
  timeline of the right length from a tree that
  is silent; that stopping a capture after a 150 ms stall hands over about
  150 ms of audio rather than losing it, and then reports `NotOpen`; that the
  game exiting is noticed and leaves the track running as silence; and that a
  game which exits the process it was launched as, leaving a descendant, is
  followed onto that descendant with the recording contiguous across the
  re-scoping.

  Beside them are the arithmetic that needs no machine at all: the
  `WAVEFORMATEXTENSIBLE` describing exactly the format the crate then converts
  by, the speaker mask filled in for an unlabelled stereo endpoint and not
  invented for anything else, the activation blob naming the process it scopes
  to, an activation that never completes being given up on, the completion
  handler signalling through its real vtable, and the check that decides whether
  a stream's reported positions are performance-counter readings at all.
- **The compatibility mix**, in two places, because it makes two different kinds
  of claim. `src/mix/` holds the mechanics — where a buffer is placed, what a
  level multiplies, which layouts and rates are refused, how far the mix may run
  before the slowest source has caught up, that a block never prints its samples
  — and `tests/compatibility_mix.rs` holds what is *audible*, because a mixer
  that summed nothing and emitted silence of exactly the right length would pass
  every one of the mechanical assertions. That file synthesises the three tones
  of AGENTS.md section 26 — 440 Hz game, 880 Hz other system audio, 1320 Hz
  microphone — feeds them in 10 ms packets the way WASAPI delivers them, and
  measures the result with the same Goertzel filter, through the same
  `clipped-media-validation::AudioContent`, that
  `crates/muxer/tests/multi_track_audio.rs` asserts a finished recording with. It
  asserts that every tone is in the mix; that each source's own buffer is
  bit-identical afterwards and still carries only its own tone; that a source
  which starts a second in is *heard* a second in; that turning one source down
  12 dB makes it four times quieter in the mix and moves nothing else; that a
  source driven past full scale is held under the ceiling with an order of
  magnitude less third-harmonic distortion than clamping produces; and that a
  source producing nothing does not take the others with it.

  None of it opens a device, renders anything or runs `ffprobe`, so it makes no
  sound and needs no sound card. What it does not cover is the end-to-end claim —
  that a *recording* plays correctly in a naive player — which needs the session
  wiring that is the remaining half of issue #29.
- **Isolation**, in `test-apps/process-tree-audio/tests/`, which is the
  acceptance criterion this feature stands or falls on and the one thing here
  that makes a noise. Two process trees play two tones at once — 997 Hz from a
  child the captured tree spawns *after* the capture is open, 1373 Hz from an
  unrelated process — and the recording has to contain the first as its
  strongest frequency, the second at least eight times down, and nothing at all
  during the window before the child started. Run it with:

  ```text
  cargo test -p clipped-process-tree-audio
  ```

  `CLIPPED_SKIP_AUDIO` skips it; `CLIPPED_REQUIRE_AUDIO` turns the skip into a
  failure.
- **What is not tested automatically**, and is the honest gap in issue #20: no
  test plays a known waveform *into* a microphone. Doing that needs a virtual
  input device, which is not installed on the machine this was written on and
  cannot be assumed on anybody else's (AGENTS.md section 25); the alternative —
  playing a tone through the speakers and hoping the microphone hears it —
  records whoever is in the room and asserts on the room's acoustics.
  [Issue #153](https://github.com/wildware-uk/clipped/issues/153) tracks doing
  it properly. What is verified against a synthetic signal today is every stage
  the microphone's samples pass through *except* the WASAPI capture call itself:
  the sample conversion, in `src/format.rs`, against synthetic bytes in each
  endpoint format, and the whole engine below the endpoint, by the tone test
  below — which runs the same `endpoint_capture.rs` code the microphone runs.
- **The tone**, in `tests/system_audio.rs`: a 997 Hz sine this crate
  synthesises, renders through WASAPI itself and captures back, asserted by
  Goertzel filter — it has to be present while it plays, absent afterwards, and
  the strongest frequency between 200 Hz and 2 kHz has to be 997. It is 997 Hz
  and not the more obvious 440 because 440 Hz is a musical A: music playing on
  the machine while the suite runs puts energy in exactly that bin, and the
  test failed here for that reason. It plays a
  quiet sound (about −28 dBFS) for under a second. The same file stalls a
  consumer through the public API and asserts that the silence invented to
  cover the gap is actual zeroes, and asserts that every endpoint buffer carries
  the position the device gave it — that synthesised silence carries none, and
  that the device's position and the track's are genuinely two different numbers
  rather than one copied twice, which is what makes drift observable at all.
- **The drift itself**, in `tests/capture/av_sync.rs`, which records this crate
  and a real video capture at the same time and measures how far apart they get.
  It belongs there rather than here because it needs both
  ([av-sync.md](av-sync.md)).

The process tree is tested in its own crate, and split the same way:

```text
cargo test -p clipped-windows
```

- **The membership rules**, in `crates/windows/src/process_tree.rs`: who is a
  candidate and who is a stranger; the two creation-time comparisons that refuse
  a recycled identifier, each shown refusing and then accepting either side of
  the boundary; a ghost kept while an orphan of it lives and released once none
  does; a process adopted after it had already exited never being announced
  either way. These are functions of numbers and are tested against process
  trees written down in the test.
- **The tree against real processes**, in `crates/windows/tests/process_tree.rs`:
  a chain of three processes the test starts, which spawns its descendants only
  when told to, so that "a game spawns a helper an hour in" happens in a second.
  A child started after the tree was built joins it and so does its own child; a
  root that is killed leaves its two descendants in the tree, orphaned, which is
  the launcher case; an unrelated process started by the same test is not a
  member; a refresh inside the rescan interval reads nothing and reports
  nothing, while the same tree told it may look again finds exactly the exit it
  had been holding back; and the tree empties when the chain does. None of it
  needs audio hardware, a game or a GPU.

  What no test covers is a process Windows refuses to open, which needs a
  process at a higher integrity level and cannot be arranged from a test running
  as the user. The classification that decides it — access denied means "never",
  anything else means "it exited a moment ago" — is unit tested against both
  error codes; that Windows returns the first one for an anti-cheat service is
  not.

Everything that touches an endpoint skips, loudly, on a machine without one —
which is why these are not in the pull-request CI job, since a GitHub Windows
runner has no audio device. Setting `CLIPPED_REQUIRE_AUDIO` turns those skips
into failures on a machine that is supposed to have one.

## Assumptions

- Windows decides the mix format and this crate does not argue with it; a
  format it cannot convert is refused rather than reinterpreted.
- The endpoint's reported positions and `QueryPerformanceCounter` are the same
  clock in the same units. `src/time.rs` asserts the conversion.
- Shared mode only. Exclusive mode would lock other applications out of the
  user's sound card, which a background recorder must never do.
- A process identifier stays reserved for as long as a handle to the process is
  open, and a process created later has a later creation time. Those two
  properties of Windows are what makes the game's process tree trustworthy; if
  either stopped holding, audio would be scoped by guesswork.
- The console role is what is recorded, on both sides. The communications role
  — which a headset may hold while speakers and a desk microphone hold the
  console role — is the one a chat application picks, and following it would
  mean a capture that changed device whenever a call started. Voice chat as its
  own track is [issue #27](https://github.com/wildware-uk/clipped/issues/27).
- **Windows follows a process tree as it grows.** A capture is scoped once, and
  a process the game starts afterwards is included without the client being
  activated again. That is what
  `PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE` is documented to do, and
  the isolation test asserts it by starting the process that makes the noise
  only after the capture is open — but it has been seen on one build of Windows,
  and a build where it does not hold would need the capture to activate again
  whenever the tree gains a member.
- **A process-scoped client reports performance-counter positions.** Measured on
  Windows 11 build 26200, where a silent tree produced exactly its sample rate
  in frames per second with no silence synthesised, which only happens if the
  positions are counter readings. It is not documented, so it is not trusted
  blind: the first packet of such a stream is compared with the counter, and a
  stream whose positions are not on it is timed by readings taken as each packet
  is read, with one `warn` saying so.

## What this document will cover

Written during M2, alongside the code:

- The track model as actually implemented, and how a configured set of tracks
  becomes a set of capture streams.
- The other direction of process-scoped capture: excluding a game's tree to
  obtain everything else
  ([issue #27](https://github.com/wildware-uk/clipped/issues/27)). Including one
  is written above.
- What happens to audio that cannot be attributed to a process tree, once there
  is a capture to observe it with. How the tree itself is resolved and kept
  current is written above.
- The optional preservation of a raw pre-processing microphone track beside the
  processed one (SPEC.md section 14,
  [issue #32](https://github.com/wildware-uk/clipped/issues/32)).
- Application-to-track routing configuration, how it is persisted, and how it
  behaves when a routed application is not running
  ([issue #33](https://github.com/wildware-uk/clipped/issues/33)).
- Where the compatibility mix is assembled in a *recording* — which thread owns
  the mixer, how its blocks reach the muxer's track 1, and the setting that turns
  the track off (SPEC.md section 13). What the mix does with what it is given is
  written above; the remaining half of
  [issue #29](https://github.com/wildware-uk/clipped/issues/29) is the wiring.
- A drift measurement taken from an actual multi-hour recording on real
  hardware, the way the numbers elsewhere on this page were taken, rather than
  from the synthetic packet sequences `crates/audio/src/timeline.rs` and
  `crates/audio/src/resample.rs` are unit tested against
  ([issue #30](https://github.com/wildware-uk/clipped/issues/30) — the
  continuous correction itself, and what it replaced, are described
  [above](#loopback-delivers-nothing-while-the-endpoint-is-silent)).
  Converting between two sources at genuinely different declared sample rates
  — as opposed to correcting one source's own clock against the reference
  clock, which is what is built — is a separate resampling stage neither this
  crate nor the compatibility mix has.
- Following an endpoint whose mix format differs from the one a recording
  started with, which today produces silence and a `Capture::FormatChanged`.
- Per-source processing — gain, mute, noise suppression, gate, compressor,
  limiter — and where in the chain each sits
  ([issue #31](https://github.com/wildware-uk/clipped/issues/31)).
- How to verify isolation across the whole track model at once
  ([issue #34](https://github.com/wildware-uk/clipped/issues/34)): a recording
  with a game track, a system track and a microphone track, each asserted by
  frequency to hold its own tone and none of the others. Two trees against one
  capture is asserted today, in
  `test-apps/process-tree-audio/tests/process_loopback_isolation.rs`.
- The Windows version requirements the subsystem depends on, measured rather
  than read off the documentation, and how it behaves on a machine that does not
  meet them. What is known today is above: the activation fails and
  `AudioError::ProcessLoopbackUnavailable` says so, and the floor itself is
  unconfirmed on hardware.
