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
while recording {
    match capture.read(Duration::from_millis(100))? {
        Capture::Samples(audio) => { /* audio.samples(), audio.timestamp() */ }
        Capture::Idle => {}
        Capture::FormatChanged(format) => { /* see "Device changes" */ }
    }
}
capture.finish(); // then read until `NotOpen`: see "Ending a capture".
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

### What an hour of real drift correction measures

Everything above was argued and unit tested against synthetic packet sequences.
[Issue #30](https://github.com/wildware-uk/clipped/issues/30) asks for the
number a real recording produces, so here it is: one uninterrupted hour, on real
hardware, with the correction running.

**The conditions**, because a drift figure without them is a number about
nothing. Windows 11 build 26200. The default render endpoint was a Razer
BlackShark V2 Pro 2.4 GHz wireless headset, presenting a 48 kHz mix format, and
it was recorded through WASAPI loopback — so the clock being measured is that
headset's crystal. The reference is the performance counter, which is the clock
Windows Graphics Capture stamps video frames with and the clock WASAPI reports
buffer positions on, so "how far the audio has moved against the picture" and
"how far the audio has moved against the performance counter" are the same
question (`docs/av-sync.md`). The subject was a 1280×720 30 fps pattern window
on a non-primary display. The measurement is `tests/capture/av_sync.rs` with
`CLIPPED_AV_SYNC_SECONDS=3600`; `docs/testing.md` has the command.

**Sampled at every endpoint buffer, which is every 10 ms — 360,006 times over
the hour** — and reported both as one least-squares fit over all of them and as
sixty separate fits, one per minute. The per-minute report is the part that
answers a question the single fit cannot: whether the drift is a rate or an
event.

**Two hours were run, not one**, because a single number from a single hour
cannot be told apart from an accident of that hour.

| | First hour | Second hour |
| --- | --- | --- |
| Audio captured | 3600.062 s, 172,802,996 frames, **0 synthesised** | 3600.093 s, 172,804,454 frames, **0 synthesised** |
| Video captured | 3599.687 s, 107,989 frames, 0 restarts, 0 missed | 3599.703 s, 107,993 frames, 0 restarts, 0 missed |
| Observations | 360,006, **0 discontinuities**, 0 step corrections | 360,009, **0 discontinuities**, 0 step corrections |
| A/V offset after an hour | **−2.387 ms** (audio leading the picture) | **−2.780 ms** |
| Worst it reached | −2.956 ms | −3.357 ms |
| Fitted drift rate | **−0.656 ppm, −0.039 ms/min** (standard error 0.0003 ppm) | **−0.787 ppm, −0.047 ms/min** (standard error 0.0003 ppm) |
| Tolerance | −40 ms to +60 ms (EBU R37), 17 hours away at this rate | 14 hours away |

The two agree to 0.13 ppm, which is well inside the third-of-a-part-per-million
spread `docs/av-sync.md` records between repeat runs on this endpoint, and both
are six or seven times smaller than what the same endpoint measured before the
correction existed. The second hour was deliberately run with the machine
compiling and running tests throughout: it still synthesised no silence, missed
no video frame and recorded no discontinuity, which is worth knowing separately
from the rate.

**It is linear, not stepped.** In both hours every one of the sixty per-minute
fits has the same sign, and they sit between −0.43 and −0.91 ppm in the first
and between −0.31 and −0.87 ppm in the second, against a per-minute standard
error of 0.12 ppm — so the minute-to-minute variation is barely outside the
noise of measuring a minute. The offset walks steadily to its final value with
no jump anywhere in it, and neither run recorded a discontinuity or a deadband
correction. That is a residual crystal error being tracked, not an event: it is
the shape resampling is the right answer to, and the shape that says nothing
else went wrong for an hour.

**What it is worth against the correction being off.** The same endpoint,
measured before the continuous correction existed, drifted at −4.35 ppm
(−0.261 ms/min) — `docs/av-sync.md` records that run and the three others
around it. Uncorrected, an hour of it is about −15.7 ms and the 20 ms deadband
fires after roughly eighty minutes, putting the whole accumulated error back in
one step. Corrected, the hour ends 2.4 ms out with nothing to step. So the
correction removes about **six sevenths** of this endpoint's drift, and what is
left is a seventh of a frame of video.

**What this does not say.** It is two hours on one crystal in one machine, and
the run-to-run spread on this endpoint is about a third of a part per million —
a three-minute run on the same build measured −0.75 ppm — so the honest
precision is a tenth of a part per million and the fourth digit of any single
run is noise. It is also a measurement of the *timestamps the pipeline
produces*, not of a finished file; what a muxer does with them afterwards is
`docs/muxing.md`. And it says nothing about a second machine, which is the
obvious next measurement and has not been taken.

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

**A capture's declared sample rate and channel count are still untouched, but
every source's own samples are now nudged by a fraction of a percent to stay
aligned with the reference clock** (issue #30,
[above](#loopback-delivers-nothing-while-the-endpoint-is-silent)). That is a
correction against drift within one source's declared rate. Converting between
two *different* declared rates is a separate thing, and it happens in exactly
one place: the compatibility mix, on the copy it holds, so that a 44.1 kHz
microphone is in the default track of a recording made on a 48 kHz endpoint (see
[Sources at different sample rates](#the-compatibility-mix)). No track a
recording contains is resampled between rates — a 44.1 kHz capture is a 44.1 kHz
track. Downmixing is a decision about what the user hears, which this crate is
not entitled to make on its own (AGENTS.md section 21).

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
that has written a stream header cannot be handed 44.1 kHz halfway through.

There is a rate converter in this crate now
([issue #30](https://github.com/wildware-uk/clipped/issues/30), see
[Sources at different sample rates](#the-compatibility-mix)), and it is
deliberately **not** wired in here. The compatibility mix converts a source's
rate because the mix is a derived track that is entitled to be a combination of
its sources; a capture's own track is not, and running a device change through a
resampler would mean that half of somebody's microphone track was the samples
their microphone produced and half was samples this crate invented, with nothing
in the file to say where the join was. Doing that quietly is what AGENTS.md
section 22 rules out. Changing shape underneath the caller would be worse than
silence, ending the recording over a headset would be worse still, so the
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

`read`, `format`, `stats`, `finish` and `close` behave exactly as they do for
system audio, and the buffers are the same contiguous, timestamped `f32` on the
same clock, so a microphone track can be compared with a video frame or with the
system audio track by subtracting timestamps. Ending one is
[the same two calls with a read loop between them](#ending-a-capture), and here
it is the difference between keeping and losing the last words somebody said
before they pressed stop.

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
| The process the activation names exits with descendants still running | re-scopes onto a surviving member and activates again, logging one `info`; the outage is filled with silence. The other side of a pair follows it onto the same process — see below |
| …and more than one member survives, in separate trees | re-scopes onto the lowest-numbered one and logs a `warn` naming the members whose audio is therefore not in this track ([issue #311](https://github.com/wildware-uk/clipped/issues/311)) |
| The game and everything it started exit | `target_is_running` becomes `false`, one `info` is logged, and the including side continues as silence until the caller stops it. The excluding side becomes [everything the machine plays](#what-the-excluding-side-does-after-the-game-exits), because the set it excludes is now empty |
| A process of the game cannot be opened | it is not a member — an unpinned identifier is not one to scope a capture to — and its name is logged at `debug` |

The tree is rooted at the game rather than at the launcher, for the reason
[The tree is rooted at the game](#the-tree-is-rooted-at-the-game-not-at-the-launch)
gives: Steam's notification chime does not belong in a track named after a game.

**Windows offers both sides, and a recording takes both or neither.**
`PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE` is everything the machine
played *except* the tree — the other-system-audio track. A recording with a
`Game` track scoped to the tree and a system track that is the whole endpoint
has the game's audio on **two** tracks, which is worse than one honest track and
a note saying separation was unavailable (ADR 0003). So:

```rust
// What a recording opens. Two captures, one thread each, one agreement.
let (game, other) = ProcessLoopbackCapture::open_pair(game_process)?;
```

That is `clipped_session::audio::open`, and naming the caller is deliberate:
this page said "what a recording opens" for a week while no recording opened it
([issue #581](https://github.com/wildware-uk/clipped/issues/581)). The session
opened `open` and `open_excluding` two lines apart, so issue #27's agreement
existed and never happened, with the code and the prose both reading as though
it did. What stops that returning is that
`clipped_session::audio::PlannedSource::ScopedPair` is **one** plan item for the
two captures — the session can no longer name one side without the other — and
that a test drives the session's own `open` and asserts the two captures it
returns report the same `ScopeAgreement`.

`open` and `open_excluding` are still there for a caller that wants one side,
but a recording that opened both of them separately would carry a defect that
only appears when a game's launcher exits partway through. Each capture then
resolves the surviving tree from its **own** `ProcessTree`, refreshed on its own
schedule, and nothing makes them land on the same process: a process in one
side's tree and not the other's has its audio on both tracks, or on neither.

`open_pair` gives the two captures one `AtomicU32` naming the process both are
scoped to. An atomic rather than a shared tree behind a lock, because both sides
are read on capture threads and a capture thread waits on nothing (AGENTS.md
section 20). The rule, in `decide_scope`:

| What the cell says | What the capture does |
| --- | --- |
| something other than what this capture names | follows it, even if this side's tree has not listed that process yet — being briefly scoped to something invisible is a moment of silence, disagreeing is the wrong audio on a track |
| what this capture names, and it is alive | nothing |
| what this capture names, it is dead, and this capture is *following* | nothing: a follower does not lead. This is what makes the rule terminate — without it, two captures whose trees disagree would move away from each other for ever |
| what this capture names, it is dead, and this capture is not following | publishes the lowest-numbered living member. If the other side published first, in the window between the read and the write, that one wins |

A capture opened on its own owns its cell, always reads back what it wrote, and
therefore behaves exactly as it did before pairs existed.

The two sides are distinct in the log: `audio_source` is `game` for the
including side and `other_system` for the excluding one, and the `device` field
names which side it is. Two captures whose lines were indistinguishable would
make the one question worth asking of them — which track did this audio go to —
unanswerable (AGENTS.md section 35).

### What the excluding side does after the game exits

**It carries everything the machine plays, which is what that track is for**
([issue #563](https://github.com/wildware-uk/clipped/issues/563)). Once the tree
is empty the set being excluded is empty, so the exclusion selects nothing and
the capture is ordinary system audio for the rest of the recording. That is the
case the track exists for: the game closes, the user keeps recording, and a
browser or a voice call is still playing.

It used not to be. `open_stream` refused an empty tree on **both** sides, so an
already-open stream carried on but any *reopen* from that point — an endpoint
change, a re-scope — produced no stream at all, and the track was synthesised
silence for the rest of the recording.

Whether Windows accepts an exclude-mode activation naming a process that has
exited is undocumented, so it was measured rather than reasoned about. On
**Windows 11 Pro build 26200**, with a 997 Hz tone playing from the measuring
process and a `cmd.exe` that plays nothing as the game:

| Activation | `ActivateAudioInterfaceAsync`, `Initialize`, `Start` | 997 Hz measured |
| --- | --- | --- |
| exclude, live identifier | all succeed | 0.02687 |
| exclude, identifier of a process that has exited | all succeed | 0.02690 |
| exclude, identifier that has never existed | all succeed | 0.02717 |
| include, identifier of a process that has exited | all succeed | 0.00000 |
| either side, identifier `0` | activation refused, `E_INVALIDARG` | — |

A dead identifier is not a special case to Windows: it excludes a tree with no
members, which is everything. The include row is the control — it says the
filter really is being applied rather than every activation returning the
endpoint — and the exclude rows are within a fraction of a percent of the live
baseline.

The other candidate was to fall back to `SystemAudioCapture` on the whole
endpoint for the remainder. It was rejected: it is a different capture with a
different activation, so it would need a changeover that is seamless in the
timeline and that cannot double if the tree becomes non-empty again, and the
measurement says none of that machinery buys anything. What is built is the same
activation with the same client and the same fixed format, so there is no
changeover at all.

The **including** side still refuses. There is nothing left to include, so the
track is silence either way, and a `Game` track that quietly became everything
the machine plays is exactly ADR 0003's cardinal sin — muting the game in an
editor would not silence it.

**Which dead identifier it reopens against is the pair's, not this side's own.**
The two mechanisms meet here. When the tree empties, `decide_scope` returns
`Ended` on both sides and *neither* publishes, so the shared cell still names
the last process the pair agreed on and each capture stays activated on it —
which is what `the_game_ending_does_not_split_the_pair` asserts. A reopen from
that point names that agreed identifier rather than whichever survivor this side
happened to resolve on its own, which is the difference the pair makes: before
it, a game that exited its launcher and then ended entirely could leave the two
sides having reopened against two different dead identifiers. It changes nothing
about *what is captured* — excluding a tree with no members is everything the
machine plays whichever dead identifier is named, which is what the table above
measures — but it is the reason the identifier is now determined rather than
raced for.

What this costs is process-identifier reuse. Once the tree is empty,
`ProcessTree` has released the handle that was pinning the identifier — that is
what makes an empty tree empty — so a reopen long afterwards can name an
identifier Windows has since given to something else, and that process's audio
would be missing from this track. It is bounded, it is the same exposure
[issue #311](https://github.com/wildware-uk/clipped/issues/311) already
describes, and the alternative it replaces is the whole track silent. Closing it
needs `clipped-windows` to lend out the handle that pins an identifier.

### Proving the contents are separated

What none of this proves on its own is that the *contents* are separated. That is
a measurement on real hardware, and it is
[`tests/audio/track_isolation.rs`](../tests/audio/track_isolation.rs)
([issue #34](https://github.com/wildware-uk/clipped/issues/34)): it records a
window whose process tree is holding one tone while another process holds a
second, and measures both frequencies on both tracks. Measured on this project's
development machine, each track's own tone reads 0.0565 and the other tree's
reads 0.00003 — about 1,900 times apart, against a rejection threshold of eight.

`cargo run -p clipped-audio --example process_loopback_probe` is the same claim
by hand: it opens the pair and prints a peak level per side, so a game tone and a
browser tone should raise one column each and never both at once.

**Ending a capture drains it**, here and on the other two — see
[Ending a capture](#ending-a-capture). The client is deliberately **not**
stopped first: measured on Windows 11 build 26200, a process-scoped stream
stopped after a 150 ms stall reported no queued packets at all, where the same
stream drained before stopping produced the 150 ms.

**It may not be available at all.** Process loopback is documented from Windows
build 20348, which no shipping Windows 10 release reaches, and
`AudioError::ProcessLoopbackUnavailable` is what a machine below that floor
produces. It is its own error precisely so a caller can tell it apart and take
the documented fallback, and [the section below](#when-the-game-cannot-be-separated)
is what that caller now does. What must never happen is a track labelled "Game"
that is really everything the machine played (ADR 0003's second consequence,
AGENTS.md section 27).

The floor itself is still unconfirmed on real hardware — this has only been run
on Windows 11 build 26200, where it works — so
[prerequisites.md](prerequisites.md) does not yet state a minimum version for
it.

### When the game cannot be separated

A machine that cannot scope a capture to a process records **one audio track
holding everything it played**, rather than failing. The alternative is no
recording at all on every shipping Windows 10 installation, and a degraded
recording is worth more than none (AGENTS.md section 17).

Until [issue #604](https://github.com/wildware-uk/clipped/issues/604) that
sentence was true only of the error message. `crates/session/src/audio/mod.rs`
mapped every failure of `ProcessLoopbackCapture::open_pair` to
`SessionError::Audio` and propagated it, so the message promised a fallback that
nothing implemented. It is the shape this project keeps finding: something
described accurately in one place and absent from the code
(`crates/session/src/audio/mod.rs::open` is where it lives now).

**The track is called `System Audio`, and that is the decision rather than the
fallback.** Not `Game`, which would be everything the machine played under the
name of one process tree. Not `Other System Audio` either: SPEC.md section 11
defines that track as all system audio *minus* the game's tree, so a track
carrying the game as well is not that track under a different name — somebody
who muted it in an editor expecting to still hear the game would hear nothing,
which is precisely the failure AGENTS.md section 21 exists to prevent. A
recording has the pair or the undivided track and never both, and `System Audio`
sits where `Game` would have been so that the microphone stays where it is in
every file.

| | Separated | Undivided |
| --- | --- | --- |
| Tracks | Compatibility Mix, Game, Other System Audio, Microphone | Compatibility Mix, **System Audio**, Microphone |
| Muting the game silences | the game | everything |

Which failures take this path, and which still refuse:

| `AudioError` | What a recording does | Why |
| --- | --- | --- |
| `ProcessLoopbackUnavailable` | Records one `System Audio` track | This machine will never scope a capture to a process. Refusing means no recording, for ever, on this machine |
| `ProcessUnavailable` | Records one `System Audio` track | The game has exited or runs elevated, so there is no tree to scope to. `ProcessLoopbackCapture::open_excluding` already documented this answer for a tree that is empty before the capture opens |
| `NoEndpoint` | Refuses | The fallback opens that same endpoint and would fail again a moment later, with a vaguer message |
| `UnsupportedFormat` | Refuses | As above: the shape the endpoint presents does not change because the capture stopped being scoped |
| `Platform` | Refuses | Nothing classified this failure. Recording a different track layout because of one nobody understood is guessing (AGENTS.md section 27) |

**It is never silent.** Four places say so, and the first is the one a user
meets without being told to look:

1. **The file.** Its track is named `System Audio`, which any editor shows.
2. **The recorder's own summary**, which gains a sentence naming the track and
   the reason (`RecordingReport`).
3. **A `warn` line**, carrying `audio_fallback`, the game's process and what
   Windows said.
4. **The session's record.** `clipped-<session>.session.json` gains an
   `audio_fallback` object on the recording — `cause`, `detail` and `track` —
   because a file whose audio layout differs from the settings written beside it,
   with nothing recording why, is a support question nobody can answer months
   later. [Issue #61](https://github.com/wildware-uk/clipped/issues/61) is the
   same gap for a substituted encoder, still unfilled; this is deliberately not a
   second instance of it. `docs/sessions.md` has the key.

**Measuring it on hardware that cannot produce it.** Every machine in this
project is far past build 20348, so the path can never be reached here by
waiting for it.
[`tests/audio/system_audio_fallback.rs`](../tests/audio/system_audio_fallback.rs)
forces it with `CLIPPED_FORCE_AUDIO_SCOPING_FAILURE`, read by
`clipped_session::audio`. What the variable injects is the **error**, not the
outcome: a genuine `AudioError` comes back from the same call a Windows 10
machine's failure comes back from, and everything after it — the classification,
the endpoint capture, the track declaration, the mixer, the encoder, the
Matroska writer — is the product's own. It is the same trick
`crates/session/src/recording.rs`'s `ScriptedFactory` plays for capture, one
layer down. The values are `unavailable`, `process-gone` and `platform`; anything
else refuses the recording rather than being ignored, because a typo that quietly
restored the ordinary behaviour would make a run that proved nothing look exactly
like one that proved the fallback.

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

**The obvious way out was measured and does not exist.**
[Issue #288](https://github.com/wildware-uk/clipped/issues/288) proposed
`NtQuerySystemInformation` — the call the snapshot is built on, documented as
subject to change, and expected to be several times cheaper. Asked the same
question, interleaved with the snapshot over 150 rounds against 448 processes,
it came out at **0.88x the speed**: slightly slower, repeated to two decimal
places across three runs. `crates/windows/examples/process_table_apis.rs` is
that measurement, and it checks the two answers row by row as well as timing
them — they agree on every identifier, parent and name.

The same output says what the query is paying for. `SystemProcessInformation`
describes every *thread* of every process, 13,155 of them in 1.28 MB, and gives
no way to decline them. Whether the snapshot pays for that same walk is the
reading that fits — the calls are related and the timings are close — but only
the query's side was counted, so it is an explanation rather than a second
measurement.

So the twelve milliseconds is the price of the question, not of the door it is
asked through, and a cheaper process tree has to ask less often or ask for less.
Tuning the interval remains the thing not to do: halving the cost by doubling
the interval doubles how long a helper's audio spends in the wrong track.

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

**Sources at different sample rates.** A 44.1 kHz headset microphone beside a
48 kHz render endpoint is ordinary hardware, so the mix converts a source whose
rate is not its own rather than refusing it. It used to refuse it, and the
consequence was worse than it sounds: the source was left out of the
compatibility mix, so the one track a player that takes a track arbitrarily
takes had no microphone in it, and the only sign was a log line.

The conversion is a windowed-sinc interpolator in
`crates/audio/src/mix/rate.rs`, 64 taps with a Blackman window and 256
interpolated fractional positions, and it is worth being exact about what it
costs:

| | |
| --- | --- |
| Passband | flat to within 0.01 dB from 100 Hz to 18 kHz, 44.1 kHz to 48 kHz, measured by `the_passband_is_flat_across_everything_a_person_can_hear` |
| Images | more than 60 dB down. Linear interpolation — which is what `src/resample.rs` does, for a different job — leaves the image of a 10 kHz tone about 23 dB down, which is plainly audible |
| Aliasing when converting *down* | the cutoff follows the lower of the two rates, so content above the output's Nyquist frequency is removed rather than folded back into the audible band |
| Delay | 32 input frames, which is a constant and is therefore subtracted: a converted source lands where its capture stamped it, to within a tenth of a millisecond |
| Work | 64 multiply-accumulates per output frame per channel — about 6 million a second for a stereo source at 48 kHz — paid **only** by a source whose rate differs from the mix's |

**What it does not touch is the source's own track.** The conversion happens on
the copy the mixer holds; `Mixer::contribute` takes `&[f32]` and the borrow
checker is what enforces that. A 44.1 kHz microphone is still written to its own
track as 44.1 kHz samples, unresampled and unmixed, which is what AGENTS.md
section 22 is about. The compatibility mix is a combination by definition, and
this is the one place a combination is allowed to happen.

That is a different problem from
[the clock correction](#loopback-delivers-nothing-while-the-endpoint-is-silent),
which keeps a single source's own clock from drifting against the reference
clock and applies to every capture whether or not any rate conversion is
happening.

**What it will not do.** Channel layouts are handled for the cases a recording
actually produces:
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

## Ending a capture

All three captures end the same way, and it is **two calls with a read loop
between them**:

```rust
capture.finish();
loop {
    match capture.read(Duration::from_millis(100)) {
        Ok(Capture::Samples(audio)) => { /* the tail, on the same timeline */ }
        // The drain has handed over everything and closed the capture itself.
        Ok(Capture::Idle | Capture::FormatChanged(_)) => break,
        Err(AudioError::NotOpen) => break,
        Err(error) => { /* a drain does not fail */ }
    }
}
capture.close();
```

`close()` alone throws away whatever the audio engine is still holding, which is
up to the 200 ms it buffers for the stream — the last fraction of a second
before somebody pressed stop, which is the part they were watching. `finish()`
closes nothing: it stops the capture looking forwards, so the queued packets
come back through `read` on the same timeline as everything before them, and
once they run out the capture closes itself and the next read reports
`NotOpen`.

**The read loop is not optional, and leaving it out is silent.** A caller that
calls `finish()` and then `close()` has thrown the audio away exactly as a bare
close would, and nothing says so — no error, no log line, only a track that ends
early. `clipped-session` did precisely that for as long as `finish` existed, so
every recording lost its tail on every track while this page said otherwise
([issue #320](https://github.com/wildware-uk/clipped/issues/320)).

A drain **waits for nothing**. It reads what is queued and stops; nothing is
reopened, no silence is synthesised for time passing, and a device that has been
unplugged ends it on the first look. That matters most for a microphone, which
Windows shows an in-use indicator for as long as anything holds.

What that is worth, measured on Windows 11 Pro build 26200:

| Measurement | Result |
| --- | --- |
| A microphone capture finished after a 500 ms stall | 9,600 frames — 0.2000 s, the whole of the engine's buffer — every one of them from the device, none synthesised, in 13 ms of wall clock |
| A system-audio capture finished after a 500 ms stall, with a 997 Hz tone playing | 0.200 s of endpoint audio in 4 ms; 997 Hz measures 0.040 in it against 0.000 of background, so what came back is the sound that was playing rather than silence of the right length |
| A whole recording of a window, stopped normally (`tests/audio/track_isolation.rs`) | its tracks end 0.005–0.008 s apart, against 0.007–0.009 s on a build that drained nothing — the engine is holding almost nothing when the reader has been keeping up, and the 200 ms above is what a reader that fell behind leaves |

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
- **The rate conversion**, in `src/mix/rate.rs`: a tone keeping its frequency
  and its amplitude across 44.1 kHz to 48 kHz, the image a linear interpolator
  would leave at 13.9 kHz measuring 60 dB down instead, content above the
  output's Nyquist frequency being removed rather than folded back into the
  audible band, a constant staying constant at every fractional position, the
  two channels of a stereo source staying out of each other, the frame count
  over ten minutes tracking the ratio rather than its own rounding, and a reset
  starting from silence. Every one of them measures the samples rather than
  counting them, and all of them run on a machine with no sound card.
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
  never returns and a hung test says nothing. `tests/system_audio.rs` adds the
  one that needs a sound to measure: a capture finished after a 500 ms stall,
  with a 997 Hz tone playing, has to hand over that tone — found by sweeping the
  spectrum rather than assumed — rather than silence of the right length, and
  has to close itself while doing it (issue #320).
- **A drained microphone**, in `src/windows/microphone.rs`: a capture finished
  after a 500 ms stall hands over the audio the engine was holding, all of it
  from the device rather than synthesised, on the same timeline as the
  recording, closes itself, and takes long enough to be measured in milliseconds
  — which is the acceptance criterion about Windows' microphone-in-use
  indicator, since a drain that waited for a device that had gone would hold it
  open (issue #320).
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
  suppressed by `CLIPPED_SKIP_AUDIO` (the fifth, below, does make a sound and
  is); what they need is a Windows that can scope
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

  One of them **does** make a sound, and it is the one that measures
  [what the excluding side does after the game exits](#what-the-excluding-side-does-after-the-game-exits)
  (issue #563). It holds a 997 Hz tone in the test process, opens both sides
  against a `cmd.exe` that plays nothing, kills it, forces a reopen through the
  same path an unplugged headset takes, and measures the tone on both tracks
  either side of that. The other-system track has to carry it and the game's
  track has to carry nothing from a client at all — a build that stopped
  refusing an empty tree on *both* sides would put the whole machine into the
  track named after the game, and that assertion is what catches it.
  `CLIPPED_SKIP_AUDIO` skips it; `CLIPPED_REQUIRE_AUDIO` turns the skip into a
  failure.

  One more opens a real pair and a real lone capture and asserts what
  `ScopeAgreement` claims: the two sides of one `open_pair` report the same
  agreement, and a capture opened by itself reports a different one. It makes no
  sound, and it is what stops the session's check below from being satisfied by
  an agreement that said "same" about everything.

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
  during the window before the child started.

  Beside it, `mid_recording_joiner.rs` asks the same question of a tree that is
  **already making a noise**: a third tone, 1699 Hz, from a second child started
  a second into the run, with both sides of the tree open from one `open_pair`.
  It measures when that tone reaches the tree's track, that it does not also
  reach the complement's, and what the join costs the audio already flowing —
  which is where the click above was found. It is `#[ignore]`d and prints its
  measurements. Run them with:

  ```text
  cargo test -p clipped-process-tree-audio
  cargo test -p clipped-process-tree-audio --test mid_recording_joiner -- --ignored --nocapture
  ```

  `CLIPPED_SKIP_AUDIO` skips them; `CLIPPED_REQUIRE_AUDIO` turns the skip into a
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

And the consumer, which is where issue #581 was:

```text
cargo test -p clipped-session
```

- **That a recording opens the pair**, in `crates/session/src/audio/tests.rs`.
  The planning tests decide *which* captures a recording opens and need no
  machine; `pairing` is the other half and needs one. It creates an off-screen
  window of this process, builds the settings a recording of a window is made
  with, calls the session's own `open` — the same call `crate::recording` makes
  — and asserts that the two captures it gets back report the **same**
  `ScopeAgreement`.

  It is that and not `scoped_to`, because two captures opened separately name
  the same process for as long as nothing has exited: the assertion that fails
  on the wrong build has to be over the identity of the cell, not its contents.
  Reverting `open` to `ProcessLoopbackCapture::open` and `open_excluding` fails
  it with two addresses and one process identifier, which is exactly the defect
  as it shipped. It plays nothing and reads no packet; where Windows will not
  scope a capture it skips loudly, and `CLIPPED_REQUIRE_AUDIO` turns that into a
  failure.

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
  it is now measured rather than assumed: `mid_recording_joiner.rs` starts a
  process in a tree that is **already audible** and times how long its tone takes
  to appear on that tree's track. **50–75 ms** from the moment the root was asked
  to start it, which includes starting the process and opening its render
  stream, and 30–45 ms from the moment it said the stream was open. It arrives
  on that track *only*: on the complement's it measures 0.00016 against 0.029,
  so the audio moves rather than doubling. Still one build of Windows (11 Pro
  26200), and a build where it does not hold would need the capture to activate
  again whenever the tree gains a member.

  **The join is not free**, which is the half of
  [issue #27](https://github.com/wildware-uk/clipped/issues/27)'s second
  acceptance criterion that is not met. Each one costs the track **1,504 frames
  — 31.33 ms — of exact digital zeros**, delivered inside ordinary packets whose
  flags are `0`: no `AUDCLNT_BUFFERFLAGS_SILENT`, no
  `AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY`, and `CaptureStats::discontinuities`
  at zero throughout. Both edges step straight from signal to zero and back,
  which is what makes it audible: two clicks with a dropout between them.

  It was first read as a *splice at full amplitude*, because the 25 ms window
  holding it measured 0.008 of a 0.0400 tone while its peak stayed at 0.0400.
  That peak is the 5 ms of the window the zeros do not cover — 0.00801/0.04000
  is 0.2003, and the window's root-mean-square falls by exactly its square root.
  Dumping the samples shows 1,504 consecutive `0.0` between one at −0.0215 and
  the next at +0.0308. The three shapes originally reported are one shape.

  **It belongs to whichever tap's stream set changed**, and a stream *leaving*
  costs the same as one joining — so every application that starts playing and
  every one that stops costs the other-system-audio track 31 ms. **The
  whole-endpoint tap is immune**: an ordinary `AUDCLNT_STREAMFLAGS_LOOPBACK`
  capture watched across the same join and leave produced no run of zeros longer
  than a millisecond. This is process-scoped taps specifically.

  **Nothing on the client side of WASAPI avoids it.** Polling instead of
  event-driven, buffer durations from 10 ms to 1,000 ms, `AUTOCONVERTPCM`,
  `SRC_DEFAULT_QUALITY` and a release build all produce the same 1,504 frames;
  `NOPERSIST`, `RATEADJUST` and `CROSSPROCESS` are refused outright with
  `AUDCLNT_E_INVALID_STREAM_FLAG`. Two `IAudioClient`s activated separately
  against the same tree produce **sample-identical** tracks — a sum of squared
  differences of exactly 0.0 over 6,000 aligned samples — with holes at the same
  sample, so there is no second copy to splice over the first. Every activation
  also costs 1,504 frames at the *front* of its track, which is the same rebuild
  seen from the other end.
  [Issue #626](https://github.com/wildware-uk/clipped/issues/626) is the defect,
  and the tests pin its size so it cannot grow unnoticed.

  **What is counted, since the hole cannot be closed.**
  `CaptureStats::unflagged_dropouts` counts the runs and
  `CaptureStats::unflagged_dropout_frames` the frames they held, so a recording
  can say "31 ms lost, fourteen times" where it used to say nothing: the engine
  flags none of this, and `discontinuities` reads zero through all of it. Both
  figures rather than either, because the two together give the average run
  length, and 31 ms is what says *this* defect rather than another. They reach
  a recording's `AudioTrackReport`, the line the recorder prints when a
  recording ends, and the `an audio source finished` log line. Carrying them to
  the Diagnostics window is
  [issue #633](https://github.com/wildware-uk/clipped/issues/633), which waits in
  turn on the `metrics` stream
  ([issue #100](https://github.com/wildware-uk/clipped/issues/100)).

  A run is recognised as loss when it is **bounded by delivered audio on both
  sides** and lasts between 5 ms and 50 ms. Both conditions are needed and
  neither is sufficient. A tap whose processes are all quiet produces exact
  zeros legitimately and indefinitely, and it is being audible either side of
  the run that distinguishes a rebuild — which lands while the tap's other
  streams carry on playing — from a source that simply stopped. Zeros at the
  front of a track are therefore never counted, even though an activation costs
  the same 1,504 frames, because from inside the capture that is
  indistinguishable from a tap that had not started making a sound. Silence
  this crate synthesised is never any part of it: it is a different failure with
  a different cause and its own counter, and every interruption resolves in the
  direction of counting nothing.

  It is a heuristic and `crates/audio/src/dropout.rs` says so: a game that
  writes 20 ms of exact zeros between two sound effects with its stream open is
  counted, and a loss that fell outside the window is not. The count is a
  diagnostic and nothing in a recording decides anything by it.

  Measured on this machine, examining one 480-frame stereo packet — a device
  period — costs **360 ns in a release build**, 0.0036% of the 10 ms that packet
  represents, and 7.8 µs in debug. `docs/testing.md` has the command.
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
- A drift measurement over longer than an hour, and on a second machine. What
  has been measured is [one hour on one
  endpoint](#what-an-hour-of-real-drift-correction-measures)
  ([issue #30](https://github.com/wildware-uk/clipped/issues/30)); what that
  cannot say is whether a second crystal behaves the same way, and only a
  second machine can.
- Following an endpoint whose mix format differs from the one a recording
  started with, which today produces silence and a `Capture::FormatChanged`.
- Per-source processing — gain, mute, noise suppression, gate, compressor,
  limiter — and where in the chain each sits
  ([issue #31](https://github.com/wildware-uk/clipped/issues/31)).
- Verifying isolation across the whole track model at once, **including the
  microphone** ([issue #34](https://github.com/wildware-uk/clipped/issues/34)).
  A recording's game track, complement track and compatibility mix are asserted
  by frequency today, against real endpoints and real processes, in
  `tests/audio/track_isolation.rs`; two trees against one capture are asserted in
  `test-apps/process-tree-audio/tests/process_loopback_isolation.rs`. The
  microphone is not, and cannot be without a virtual capture device to render a
  known tone into, so it stays in the manual procedure in `docs/testing.md`.
- The Windows version requirements the subsystem depends on, measured rather
  than read off the documentation, and how it behaves on a machine that does not
  meet them. What is known today is above: the activation fails and
  `AudioError::ProcessLoopbackUnavailable` says so, and the floor itself is
  unconfirmed on hardware.
