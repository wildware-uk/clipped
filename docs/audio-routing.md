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

So this document describes two captures: what they do, the two problems they
exist to solve, what they convert, what happens when the user changes their
audio device mid-recording, and how to check any of it for yourself. Most of it
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
timestamps can be subtracted from one another directly. A/V synchronisation
proper is [issue #22](https://github.com/wildware-uk/clipped/issues/22).

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
devices and measure durations. The one number that comes from the samples is the
peak level `examples/microphone_probe.rs` prints once a second, which exists so
that "the track is silent" can be told apart from "the track is quiet", is
thrown away as soon as it is printed, and is never logged.

The tests in `src/windows/microphone.rs` open the machine's real microphone for
a second or two at a time and assert on frame counts, timestamps and whether
silence is zero. None of them keeps a sample, writes one, or looks at what was
said.

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

It is the tool for the two behaviours no automated test can reach on an
ordinary machine, because they need a hand on a cable:

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
  track of the right length, made of silence; a chosen device being reopened by
  its identifier rather than replaced; a microphone that is not connected being
  reported by name with something to do about it, which needs no audio hardware
  and so runs in CI; the list of microphones naming exactly one default; and the
  microphone and system audio running at once on different devices without
  either disturbing the other.
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
  cover the gap is actual zeroes.

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
- How a game's process tree is resolved and kept current as children start and
  exit ([issue #25](https://github.com/wildware-uk/clipped/issues/25)), and what
  happens to audio that cannot be attributed to a tree.
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
