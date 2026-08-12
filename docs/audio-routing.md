# Audio routing

**Status: two streams exist, and they are not a track model yet.**
[Issue #19](https://github.com/wildware-uk/clipped/issues/19) built system audio
capture: `clipped-audio` can record the output device Windows is playing
through, as a continuous, timestamped stream of `f32` samples.
[Issue #20](https://github.com/wildware-uk/clipped/issues/20) added microphone
capture beside it, on the same engine. That is the foundation the track model is
built on, and it is all there is: nothing yet writes either stream into a file,
which is [issue #28](https://github.com/wildware-uk/clipped/issues/28). Routing
— the part this document is named after — is milestone M2, and the sections at
the end that describe it are still unwritten because describing behaviour before
it is built produces a page that is wrong from the day it is committed
(AGENTS.md section 7).

One piece of M2 is built and is described here in full: **which processes a game
consists of**, which is what "the game's audio" has to mean before anything can
capture it ([issue #25](https://github.com/wildware-uk/clipped/issues/25)). No
capture uses it yet — that is
[issue #26](https://github.com/wildware-uk/clipped/issues/26) for the game's own
track and [issue #27](https://github.com/wildware-uk/clipped/issues/27) for
everything else — so [The game's process tree](#the-games-process-tree) below
describes a facility with no consumer, deliberately said out loud.

So this document describes two captures and one process tree: what the captures
do, the two problems they exist to solve, what they convert, what happens when
the user changes their audio device mid-recording, how a game's process tree is
resolved and kept current, and how to check any of it for yourself. Most of it
is written about system audio because that is where the machinery was built and
where it is easiest to describe; the [Microphone](#microphone) section says what
is different about the other one, and everything not listed there is the same
code. The intended end state is SPEC.md sections 11 to 14, the constraints are
AGENTS.md section 21, and the decision that shapes the whole subsystem is
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
Windows 11 build 26200 and well under a perceptible synchronisation error. Its
cost, stated plainly, is that a device whose sample clock genuinely differs
from the performance counter is corrected in one 20 ms step roughly once an
hour rather than continuously. Removing that step needs resampling against a
reference clock, which is
[issue #30](https://github.com/wildware-uk/clipped/issues/30).

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

Resampling belongs to the stage that reconciles several capture clocks
(issue #30) and downmixing is a decision about what the user hears, which this
crate is not entitled to make on its own (AGENTS.md section 21).

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
this crate has no resampler yet (issue #30). Changing shape underneath the
caller would be worse than silence, and ending the recording over a headset
would be worse still, so the capture says what happened, keeps the timeline
running, and waits.

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
  guessed at. Nothing logs those names yet, because nothing yet consumes a tree.
- **Where audio that belongs to no tree ends up** is not decided here. It is a
  question about the complement capture, and it belongs to
  [issue #27](https://github.com/wildware-uk/clipped/issues/27) with an
  observation behind it (ADR 0003's last consequence).
- **Nothing has been tried against a real game.** The behaviour is asserted
  against a chain of processes the tests start themselves. Anti-cheat wrappers
  and launchers do things test fixtures do not, and the first real capture
  (#26) is where that meets a game.

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
  all of it. These run anywhere, including on a machine with no sound card.
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

## What this document will cover

Written during M2, alongside the code:

- The track model as actually implemented, and how a configured set of tracks
  becomes a set of capture streams.
- Process-scoped loopback capture in both directions: including a game's
  process tree, and excluding it to obtain everything else
  ([issue #26](https://github.com/wildware-uk/clipped/issues/26),
  [issue #27](https://github.com/wildware-uk/clipped/issues/27)).
- What happens to audio that cannot be attributed to a process tree, once there
  is a capture to observe it with. How the tree itself is resolved and kept
  current is written above.
- The optional preservation of a raw pre-processing microphone track beside the
  processed one (SPEC.md section 14,
  [issue #32](https://github.com/wildware-uk/clipped/issues/32)).
- Application-to-track routing configuration, how it is persisted, and how it
  behaves when a routed application is not running
  ([issue #33](https://github.com/wildware-uk/clipped/issues/33)).
- The compatibility mix: what is mixed into it, at what point, and how muting a
  source interacts with it
  ([issue #29](https://github.com/wildware-uk/clipped/issues/29)).
- Clock drift and sample-rate handling between independent capture clients, and
  how audio stays aligned with video over a multi-hour session
  ([issue #30](https://github.com/wildware-uk/clipped/issues/30)) — including
  what replaces the deadband correction described above.
- Following an endpoint whose mix format differs from the one a recording
  started with, which today produces silence and a `Capture::FormatChanged`.
- Per-source processing — gain, mute, noise suppression, gate, compressor,
  limiter — and where in the chain each sits
  ([issue #31](https://github.com/wildware-uk/clipped/issues/31)).
- How to verify isolation: the tone-generator system tests in `tests/audio`
  ([issue #34](https://github.com/wildware-uk/clipped/issues/34)), which assert
  by frequency rather than by ear, as `tests/system_audio.rs` already does for
  one source.
- The Windows version requirements the subsystem depends on, and how it behaves
  on a machine that does not meet them.
