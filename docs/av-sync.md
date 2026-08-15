# The capture clock and audio/video synchronisation

Every source in a recording has its own idea of what time it is. This document
decides which one the recording believes, how the others are expressed against
it, where the conversion happens, what is done when two of them disagree, and
how far apart they were measured to drift on real hardware.

It is written down because synchronisation is easy to design once and very hard
to retrofit. The symptom of getting it wrong — audio a second out by the end of
a two-hour recording — is invisible in a thirty-second test, so it is not
something a later milestone will notice and fix. Multi-track muxing
([issue #28](https://github.com/wildware-uk/clipped/issues/28)) and drift
correction ([issue #30](https://github.com/wildware-uk/clipped/issues/30)) both
build directly on what is below.

## The model in five sentences

1. A recording is timed against **one monotonic reference clock**, named by the
   video source. On Windows that is the high-resolution performance counter.
2. The recording's **epoch** is the timestamp of the first video frame it keeps,
   and every timestamp in the file is nanoseconds from that epoch — a
   `MediaTime`, which is signed, because a source that was already running can
   describe a moment before the epoch.
3. **Every source converts once**, at the boundary of `clipped-capture`, through
   `CaptureClock`. A source that cannot name the reference clock is refused
   rather than subtracted.
4. **Nothing is resampled and nothing is shifted to make two sources agree.**
   Each source is placed where its own hardware said it happened; where they
   disagree, the disagreement is measured, bounded and reported.
5. **A gap in a source is filled, not closed.** Silence goes into an audio gap
   and the video timeline simply has no frame; neither timeline is ever pulled
   backwards to remove the hole, because that is what makes an error cumulative.

The types are in `crates/capture/src/time.rs` and the model's assumptions are
listed at the end of this document.

## Why the reference is the video source's clock

There are three candidate clocks in a recording and only one of them can be
authoritative.

| Candidate | What it is | Why it is not the reference |
| --- | --- | --- |
| Wall-clock time | `SystemTime::now()` | Not monotonic. It steps when NTP corrects it and when the user changes the time zone, and a step in the middle of a recording moves every subsequent packet. `SourceClock` deliberately has no variant for it, so a backend has nowhere to declare one. |
| The audio device's sample clock | A crystal on the sound card, counting frames | It is the one clock in the system that can be adjusted without anybody noticing, by adding or removing samples — which makes it the clock that gets *corrected*, not the one that does the correcting. It is also per-device: a recording with a microphone and a headset has two of them. |
| The video source's clock | `Direct3D11CaptureFrame::SystemRelativeTime`, `DXGI_OUTDUPL_FRAME_INFO::LastPresentTime` | **This one.** |

The argument for video is that video is the source that cannot be adjusted.
Frames exist at the moments the compositor made them; a recorder that moved one
would be inventing frame pacing that the game did not have, and the result looks
like stutter that is not in the game. Audio, by contrast, can absorb a
correction: adding twenty milliseconds of silence to a quiet passage or dropping
twenty milliseconds from a loud one is at worst a faint tick, and resampling it
properly is inaudible. So the source that cannot bend is the reference, and the
source that can is expressed against it.

There is a second, practical reason, and it is why this is cheap on Windows:
both Windows capture APIs stamp frames with performance-counter readings, and
WASAPI reports a performance-counter position with every captured packet. The
reference clock is therefore one both sides can read directly, and the "cross
clock conversion" that this document might have been about does not exist on
this platform. What remains is not conversion but *rate*: see
[what actually drifts](#what-actually-drifts).

## The epoch, and `MediaTime`

A performance counter counts from system boot. A recording started on a machine
that has been up for a year begins at about 3.1 × 10^16 nanoseconds, which is a
number no container and no player has a use for, and which overflows a
64-bit product in the muxer's rescaling if it is passed through unrebased.

`CaptureClock::start_at(first_frame_timestamp)` fixes the epoch.
`CaptureClock::media_time` then converts any timestamp on the same clock into a
`MediaTime`: nanoseconds from the epoch, **signed**.

Signed, because a packet older than the epoch is normal rather than a fault. The
audio endpoint was already running when the first video frame arrived, so the
audio capture's first packet routinely describes a moment before the epoch —
measured on the development machine over the thirty-minute run below, the audio
track's first buffer landed 293 ms before the first video frame, because the
audio thread opens its endpoint while the capture backend is still initialising.
An unsigned media time would turn that 293 ms lead into eighteen billion seconds
of lag, silently.

The epoch does not have to be the earliest moment in the recording, and nothing
in the model assumes it is.

### One epoch per recording, one timeline per session

`MediaTime` is per **recording**: a `CaptureClock` is started for each one, so a
session that writes three files has three epochs, and each file's timestamps
count from its own.

Events are not on that axis. `clipped_events::EventTime` is a moment on the
**session's** timeline — one zero for the whole sitting — because placing an
event means asking which of a session's files covers it, and a set of segments
each measured from its own zero cannot be sorted or searched
(`clipped_library::events`, `docs/highlights.md`). A session's second recording
therefore occupies a span starting at a positive `EventTime` rather than at
zero.

The two coincide only for the first recording. **A later recording's
`MediaTime` readings are not `EventTime`s**, and converting one means adding
where that recording starts on the session's timeline;
`EventTime::from_media_nanos` takes a reading already on that timeline and
cannot rebase, because it is handed a bare `i64`. Getting this wrong is silent:
every event of the second file lands in the first, and no assertion anywhere
fails.

The recorder keeps this rule by holding one epoch per session
([issue #488](https://github.com/wildware-uk/clipped/issues/488)). A
`SessionPlugins` is still created per recording — a plugin does not outlive the
recording that started it — but the second one is handed the session's zero
rather than taking its own, and the driver lets that zero go when the session
ends. Holding it past the end would stamp the *next* session's events against
the previous session's first frame, which is the same failure an hour further
out.

### Start-time alignment: what happens to audio before the epoch

This needs a stated rule rather than whatever falls out of the component that
happens to see the packet last, because the quantity is not small and it is not
random. A quarter of a second of audio arriving before the first video frame is
a *fixed* error at the head of every recording, its size decided by how much
earlier the audio thread happened to open its endpoint — and a constant error is
exactly what the drift measurement below cannot see.

**The rule: audio that precedes the epoch is trimmed at the epoch, to the
sample, by whoever assembles the recording.** Frames describing a moment before
the first kept video frame are dropped; the first frame at or after the epoch
keeps the media time its own hardware gave it. Nothing is shifted, and the track
handed to the writer starts at the recording rather than before it.

The two alternatives, and why not:

| Instead | Why not |
| --- | --- |
| **Leave it to the writer**, which clamps any pre-origin timestamp to the start of the file (`docs/muxing.md`) | The writer's origin is set by whichever packet reaches it first, so this decides the head of every recording by a race. If the first video frame is written first, the pre-epoch audio behind it is stacked on the file's first instant — 293 ms of samples in the measurement below. If an audio packet gets there first, the file's origin moves back instead and the picture starts hundreds of milliseconds in. Either way the head of the recording is misaligned by however early the audio thread happened to open its endpoint, and nothing in the pipeline measures it. The clamp is the right *last* resort for a packet that reaches a writer, not a policy. |
| **Move the epoch back** to the first audio packet | The epoch is the first video frame by construction (point 2 above), and moving it means the file starts with sound and no picture: a black lead-in of unpredictable length in every recording, and an epoch that is not knowable until every source has delivered something, which delays the first conversion. |

**Not implemented here.** `clipped-capture` gives a session the signed
`MediaTime` the rule needs, and `crates/capture/src/time.rs` is where the epoch
is fixed, but applying the rule belongs to whoever owns the recording's sources —
`clipped-session`, which exists and records video but has no audio source to
align against yet ([issue #180](https://github.com/wildware-uk/clipped/issues/180)).
[Issue #174](https://github.com/wildware-uk/clipped/issues/174) tracks doing it.
Until then a recording assembled from these parts inherits the muxer's clamp,
which is a known error at the head of the file rather than a hidden one.

## Where a conversion happens

Once per packet, at the boundary of `clipped-capture`, and nowhere else.

```text
     Windows Graphics Capture            WASAPI loopback
     Desktop Duplication                        │
              │                                 │
     CaptureTimestamp                     AudioTimestamp
     (+ SourceClock)                (nanoseconds, performance counter)
              │                                 │
              └──────────► CaptureClock ◄───────┘
                   media_time      media_time_on
                                │
                            MediaTime
                    (signed nanoseconds from the epoch)
                                │
                         PacketTimestamp ──► MKV, rescaled to 1 ms
```

`CaptureClock::media_time` takes a `CaptureTimestamp`, which carries the
`SourceClock` it counts on, and refuses — `ClockMismatch` — if that is not the
recording's clock. `CaptureClock::media_time_on` is the same conversion for a
source whose timestamps are not `CaptureTimestamp`s, and it makes the caller
name the clock explicitly, so the claim being made is one line a reviewer can
check.

### Why audio timestamps are a different type

`clipped_audio::AudioTimestamp` and `clipped_capture::CaptureTimestamp` count
the same nanoseconds on the same counter and would be better as one type. They
are two because `clipped-capture` and `clipped-audio` are both layer 1 of the
dependency table in `README.md` — neither may depend on the other — and there is
no shared vocabulary crate below them to put a timestamp in.

Issue #22 considered creating one and did not, because a new workspace crate for
a single 30-line type is a larger and more disruptive change than the
duplication it removes, and the duplication is bounded: both types are
constructed only from a value their source supplied, both expose `as_nanos`, and
`CaptureClock::media_time_on` is the single named bridge between them. If a
third crate needs the same vocabulary — the session's own clock, or a
non-Windows capture backend with a different `SourceClock` — that is the point at
which a `clipped-time` crate earns its place.

## What actually drifts

Because video timestamps and audio packet positions are both performance-counter
readings, the interesting quantity is not a conversion error. It is the **audio
device's sample clock**.

An audio endpoint counts frames with its own oscillator. Nominally 48,000 of
them a second; actually 48,000 × (1 ± a few tens of parts per million), and the
error is a property of the part rather than of the machine's load. `clipped-audio`
builds its track by counting samples — the track has to be contiguous, so buffer
*n*'s timestamp is the anchor plus every frame emitted before it — and the
performance counter counts real time. Those two accounts of "now" separate at
exactly the endpoint's rate error:

```text
offset(t) = (track position at t) − (performance-counter position at t)
```

Video timestamps *are* performance-counter readings, so the way that offset
moves is the way the audio moves against the picture: its slope is the rate at
which a recording comes apart, and that rate is what everything below is about.

What it is *not* is a reading of the audio/video offset a finished file
contains. It is a measurement of a change, taken from timestamps rather than
from a file, and
[what the measurement can and cannot see](#what-the-measurement-can-and-cannot-see)
sets out what stands between the two. Read that before quoting a number from
here.

Both halves are on every buffer `clipped-audio` hands over —
`CapturedAudio::timestamp` is the track's account, `CapturedAudio::device_timestamp`
is the endpoint's — and `clipped_capture::DriftEstimator` turns a stream of the
pairs into a rate.

### The measurement: how far they move apart

`tests/capture/av_sync.rs` captures the `video-pattern` test application through
the real Windows Graphics Capture backend and the system audio endpoint through
real WASAPI loopback, at the same time, on separate threads, and feeds every
endpoint buffer's pair of positions to a `DriftEstimator`. It holds a render
stream open for the length of the run — every buffer released with
`AUDCLNT_BUFFERFLAGS_SILENT`, so it makes no sound at all — because loopback
delivers nothing while the endpoint is idle, and a period the device never
described has no position to measure against.

Two things it does *not* do, because both would measure the test rather than the
subject. It does not read every frame's pixels back: one frame a second is copied
into system memory and decoded, which is enough to prove the frames being timed
are the pattern's and enough to fit the source's presentation interval, without
fifty thousand GPU readbacks competing with the capture. And it does not compare
audio against a single video frame: the offset is measured at every endpoint
buffer, thousands of times over the run, so the result is a trend rather than a
sample.

### What the measurement can and cannot see

The number this produces is quoted in issues and will be built on, so it is
worth being exact about what it is a number *of*.

**It is relative.** `clipped-audio` anchors its track on the first packet's own
device position, so the first observation of a run is zero by construction and
every later one is measured from there. A constant error that was already there
when the capture started — an endpoint whose reported positions are a fixed
period away from the samples they describe, or a session that starts the audio
at a different moment from the video — does not appear in these numbers at any
size. What is measured is the change. Measuring the constant needs a subject
whose sound and picture are known to be simultaneous at the source, which is
[the second measurement](#the-absolute-offset-what-the-drift-measurement-cannot-see)
below and is a different run with a different subject.

**No file is written.** These are the timestamps the pipeline produces, which is
what a writer is handed, not what a writer wrote. `clipped-muxer` rescales media
times to 1 ms container ticks and clamps anything before the file's origin to the
start of it (`docs/muxing.md`), so the offset a finished recording contains is
this plus the rescale's rounding plus whatever happened at the head of the file —
see [start-time alignment](#start-time-alignment-what-happens-to-audio-before-the-epoch).

**It has an error bar, and the error bar is not the interesting uncertainty.**
The rate is a least-squares slope, so `DriftEstimator::rate_standard_error`
reports its standard error and the test prints it beside the rate. Over half an
hour and 180,000 observations that error is under a thousandth of a part per
million — while repeat runs against the same endpoint vary by about a *third* of
a part per million, three hundred times more. So the fit is not where the
uncertainty in this number lives: it lives in the two paragraphs above and in the
run-to-run spread recorded below, none of which more observations reduce.

**It is not physical synchronisation** — see
[what this model deliberately does not do](#what-this-model-deliberately-does-not-do).

Measured on the development machine (Windows 11 build 26200, Razer BlackShark V2
Pro 2.4 GHz wireless headset as the default endpoint at 48 kHz, capture of a
1280×720 30 fps pattern window on a non-primary display):

| Run | Endpoint buffers | Peak A/V offset | Final A/V offset | Drift rate | Standard error of the fit | Corrections |
| --- | --- | --- | --- | --- | --- | --- |
| 90 s | 9,008 | −0.947 ms | −0.361 ms | −4.250 ppm (−0.255 ms/min) | 0.0649 ppm | 0 |
| **30 min** | **180,009** | **−8.415 ms** | **−7.833 ms** | **−4.346 ppm (−0.261 ms/min)** | **0.0007 ppm** | **0** |

The thirty-minute run captured 53,993 video frames covering 1799.713 s of it
alongside 1800.090 s of audio, with no synthesised silence, no dropped frames and
no subject restarts.

**Repeatability, which matters more than either figure.** Thirty-minute runs
against this same endpoint have measured −4.061, −4.241, −4.345 and −4.346 ppm;
ninety-second runs, −4.250 and −4.414 ppm. That is a spread of about 0.35 ppm
between runs, two to three orders of magnitude wider than any single run's
standard error, so **the honest precision of this measurement is a tenth of a
part per million at best**, and quoting the fourth digit of one run would be
quoting noise. A crystal's rate does move with its temperature, and this is a
wireless headset; what generalises is "about four parts per million slow, on this
endpoint, on the order of a quarter of a millisecond a minute", and the method
that produced it.

The interpretation of that sign: the endpoint's sample clock runs about four
parts per million **slow** against the performance counter. It delivers slightly
fewer than 48,000 frames per real second, so the track — which is built by
counting frames — falls behind real time, and each sound is stamped about a
quarter of a millisecond per minute *earlier* than the moment it happened. The
audio therefore **leads** the picture by a growing amount, which is what the run
measured: 7.8 ms of lead after half an hour, and `SyncState::Ahead` is the state
a negative offset classifies as.

The sign is worth being exact about, because drift correction
([issue #30](https://github.com/wildware-uk/clipped/issues/30)) reads it: a
negative rate is removed by making the audio track *longer*, and a positive one
by making it shorter. A corrector that took this endpoint for a fast clock would
resample in the direction that doubles the error.

Left alone, that rate would reach 40 ms — the lead half of the tolerance below —
after about two and a half hours (the thirty-minute run's own figure is 153
minutes). It never gets there, because `clipped-audio`'s 20 ms deadband arrives
first: at a quarter of a millisecond a minute the timeline crosses it after about
eighty minutes and puts the offset back in one step. So what a two-hour recording
on this endpoint contains is one correction and an offset the deadband bounds at
20 ms, rather than the ~30 ms a straight extrapolation of the rate suggests. The
thirty-minute run is well short of the first step, which is why it recorded no
corrections at all. A different endpoint will measure differently; the number to
generalise is the *method*, not the value.

The rate is small, but the point of measuring it is that it is *not zero and not
random*. A quarter of a millisecond a minute is invisible for the first ten
minutes and is half the lead budget by the end of a long session, and the only
reason it is knowable in advance is that it was fitted over thousands of
observations rather than eyeballed.

The same run also measures the video side against the reference clock: the
pattern's frame counter is decoded from one frame a second and fitted against
the capture timestamps, which gave 33.3333 ms between the source's frames
against a nominal 33.3333 ms at 30 fps — agreement to within the 0.0001 ms the
line prints, three parts per million, which is what "video timestamps are
performance-counter readings" looks like when it is checked rather than
asserted. (The ninety-second run printed 33.3336 ms for the same quantity: a fit
over eighty-nine sampled frames of an application's own timer, rather than a
measurement of the reference clock.)

## The absolute offset: what the drift measurement cannot see

Everything above is a *change*. It has to be, because the subject it is measured
against makes no sound: there is no moment in a capture of it whose audio and
video halves are known to have happened together, so there is nothing to measure
a constant against, and the first observation of a run is zero by construction.
An endpoint whose reported positions sit a fixed period from the samples they
describe, or a pipeline that stamps a frame with a present time where it meant a
compose time, would be invisible above at any size.

So the subject was given a sound. `video-pattern --tone` plays a 997 Hz tone of
30 ms at about −28 dBFS — quiet, and once every five seconds rather than
continuously — placed at the moment it presents a *named* frame, and announces
both halves of that event on standard output:

```text
tone index=0 frame=60 onset=31415926535 present=31415928111 skew=1576
```

`onset` is where the endpoint's own clock (`IAudioClock`) puts the tone's
half-amplitude point, `present` is the performance counter immediately after
that frame was handed to the compositor, and `skew` is the difference: how far
apart the two halves were **at the source**. It is announced rather than assumed
to be zero, because nothing makes a thread present a frame at exactly the moment
an endpoint plays a sample. Measured over the runs below it stays inside a
millisecond, which is what asking for each tone six frames ahead of the frame it
belongs to — against the moment that frame is actually about to be presented —
buys over a schedule laid down at the start of a run.
`test-apps/video-pattern/src/tone.rs` is how a sample is placed at a moment, and
why the moment named is the attack's midpoint.

`tests/capture/av_sync.rs` then finds both halves in what was captured — the tone
by its frequency, in a Goertzel envelope measured every 0.25 ms over 2 ms
windows, and the frame by the counter in its pixels — and reports

```text
offset = (audio in the recording − audio at the source)
       − (video in the recording − video at the source)
```

so that the source skew cancels rather than being ignored. Positive is sound
behind picture.

### What that number contains

The offset is one path minus the other, and each path is one pair of moments —
where the recording puts the event, minus where the source said it happened:

| Path | What it is | What is inside it | Measured here |
| --- | --- | --- | --- |
| **Video** | The capture's timestamp for the frame, minus the moment the subject handed that frame to the compositor | The compositor's present-to-compose latency, **and** whatever `clipped-capture` does between the timestamp Windows attaches to a frame and the one it reports | +13.8 and +14.3 ms on average over the two runs below; 6.7 to 28.5 ms tone to tone |
| **Audio** | Where the recording's audio track puts the tone, minus the moment the endpoint's own clock played it | The audio engine's render-to-loopback latency, **and** however `clipped-audio` anchors its track — 3.4 ms of this one is the constant [issue #188](https://github.com/wildware-uk/clipped/issues/188) is about | −2.4 and −2.3 ms on average, and steady: under 0.5 ms of spread across a run |
| **Their difference** | The A/V offset this measurement reports | Everything above, and nothing here separates one from another | −16.2 and −16.6 ms |

**Which part of it is the operating system's, stated exactly.** The dominant
term in each path is Windows': a compositor holds an application's frame until
it composes it, which is up to a display refresh and is the reason the video
path is both large and scattered, and the loopback tap reports a sample some
fixed distance from where `IAudioClock` says the endpoint played it, which is
the reason the audio path is small and steady. A recording made on Windows
contains both whatever the recorder does, and neither is Clipped's to remove.

What this measurement cannot do is *prove* that division. It has a single
account of each of the four moments, so a constant the recorder itself adds on
either side sits inside the same figure and no arithmetic here can lift it out;
separating them would need a second, independent account of when the frame was
composed and when the sample was played. The one term that has been separated is
the 3.4 ms [below](#what-it-found), by measuring the audio path a second time
against the endpoint's own reported positions — and even that is not yet
attributed to either side. So the row above that governs is the third one: what
is bounded is the total, and the total is what a viewer of the recording gets.

### The numbers

Measured on the development machine (Windows 11 build 26200, the same Razer
BlackShark V2 Pro 2.4 GHz wireless headset as the default endpoint at 48 kHz,
a 1280×720 30 fps pattern window on a non-primary display), two 90-second runs:

| Run | Tones measured | Mean A/V offset | Range | Standard deviation | Video path | Audio path |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | 18 of 18 | **−16.176 ms** | −30.755 to −9.166 ms | 5.749 ms | +13.775 ms | −2.401 ms |
| 2 | 18 of 18 | **−16.587 ms** | −24.337 to −10.971 ms | 4.290 ms | +14.327 ms | −2.260 ms |

Sound **ahead** of picture by about 16 ms, and the reason is in the last two
columns: the compositor holds a frame for a few milliseconds longer than the
audio engine holds a sample. The two runs agree to 0.4 ms, which flatters the
figure — two earlier runs of the same measurement, before the match window was
narrowed to the frames a run decodes around a tone, gave −15.9 and −14.5 ms. A
millisecond or so is the honest spread between runs.

The scatter *within* a run is the compositor's. Every tone in both runs was
measured on the tone's own frame, and the video path of individual tones ranged
from 6.7 to 28.5 ms while the audio path of the same tones moved by less than
half a millisecond. The detector's own resolution — a quarter of a millisecond,
on a burst whose peak is 0.0401 against a floor of zero — is nowhere near
either.

That is well inside `SyncTolerance::default()`, and the tolerance is the right
one to judge it by even though most of what it is spending is very likely
Windows': a recording is watched, not audited, and 16 ms of lead is 16 ms of
lead however it got there. The test therefore asserts the mean against 40 ms of
lead and 60 ms of lag, and separately asserts that the tones of a run agree with
each other to within 15 ms — because a mean of readings that disagree is not a
measurement of a constant.

### What it found

**The track this recorder builds sits about 3.4 ms ahead of the endpoint's own
positions for the same samples, and it is there within the first seconds of a
run.** The test reports each tone twice: once against `CapturedAudio::timestamp`,
the track `clipped-audio` builds by counting samples, and once against
`CapturedAudio::device_timestamp`, the position WASAPI attached to the packet the
samples came from. In both runs above the two accounts were 3.43 ms apart at the
first tone and 3.79 ms apart at the last — 0.36 ms of separation over 85
seconds, which is the drift rate the same run measured (−4.1 ppm, a quarter of a
millisecond a minute) and nothing more. So it is a step, taken early, and not
the rate.

**The relative measurement is not blind to it, and saying otherwise would be
wrong.** The same runs' drift lines read `first −0.002 ms, last −3.792 ms, peak
−4.368 ms, in tolerance (8971 observations, 1 discontinuities)` and `first
+0.000 ms, last −3.792 ms, peak −4.370 ms … 1 discontinuities`: the step happens
*after* the track's anchor, so it is inside the interval those numbers cover, and
the estimator reports it as its latest and peak offset and counts it as a
discontinuity. The class of error that measurement genuinely cannot see is one
already present when the capture started — before the anchor, where the first
observation is zero by construction — and this is not that.

**The silent runs do not show it at all**, which is the third thing worth
recording and came out of running both measurements on this machine within
minutes of each other. A thirty-second drift run taken while writing this
reported `first +0.000 ms, last −0.128 ms, peak −0.697 ms … 0 discontinuities`,
and the runs in the drift table further up say the same: the ninety-second one
peaked at −0.947 ms, and the thirty-minute one at −8.415 ms with no
discontinuity at all — which is a quarter of a millisecond a minute accumulating
for half an hour, not a step. The difference between those runs and these is
what the endpoint is being given: `AUDCLNT_BUFFERFLAGS_SILENT` buffers, against
a real 997 Hz tone. So whatever the 3.4 ms is, it turns up when the endpoint has
audio to play. That is a clue for #188 rather than an explanation, and it is
here because it is easy to observe now and hard to reconstruct later.

What the absolute measurement adds, then, is a moment known to be simultaneous
at the source to hold both accounts against, so the disagreement can be read as
a placement rather than as a movement, and so that the offset each account gives
can be quoted: run 1's mean was −16.176 ms with the track and −12.561 ms with
the endpoint's own positions, run 2's −16.587 and −12.983 ms. Both are well
inside the tolerance, the difference is *inside* the audio path figure in the
table above rather than on top of it, and nothing is wrong with a recording
because of it. It is written down because a number nobody has explained should
be, and because which of the two accounts a recording ought to be built from is
a question worth an answer. Whether it is the anchor, the position WASAPI
reports for a packet, or the endpoint's own buffering is
[issue #188](https://github.com/wildware-uk/clipped/issues/188).

### What it still does not do

- **No file is written.** As with the drift measurement, these are the timestamps
  the pipeline produces, not what a writer wrote. It is the half of
  [issue #173](https://github.com/wildware-uk/clipped/issues/173) that is not
  done, and it is not done because it cannot be yet: a recording with an audio
  track in it needs
  [#126](https://github.com/wildware-uk/clipped/issues/126) to wire capture,
  encode and mux together and
  [#180](https://github.com/wildware-uk/clipped/issues/180) to route audio into
  the same file. Measuring this offset from a produced recording, with the media
  harness reading the pattern's counter out of the video track and the tone out
  of the audio track, is
  [#151](https://github.com/wildware-uk/clipped/issues/151). The stages that
  could change the number between here and there are named in `docs/muxing.md`:
  the rescale to 1 ms container ticks, and the clamp of anything before the
  file's origin.
- **It is not physical synchronisation.** Whether the sound leaving the speakers
  and the light leaving the panel are simultaneous still needs a microphone and a
  photodiode; the endpoint's own output latency — which for a wireless headset is
  tens of milliseconds — is downstream of everything measured here and is not in
  these numbers.
- **It cannot attribute what it measures.** See the table above.

## Tolerance: how far out is too far

`SyncTolerance` holds two limits, and they are deliberately asymmetric, because
perception is. Sound arriving *after* the picture is what a listener expects from
anything more than a few metres away; sound arriving *before* it is a thing that
never happens in nature and is noticed at roughly half the error.

| Standard | Audio ahead of picture | Audio behind picture |
| --- | --- | --- |
| ITU-R BT.1359-1, detectability | 45 ms | 125 ms |
| ITU-R BT.1359-1, acceptability | 90 ms | 185 ms |
| EBU R37, production chains | 40 ms | 60 ms |
| **`SyncTolerance::default()`** | **40 ms** | **60 ms** |

The default is EBU R37 rather than the more forgiving ITU figures because a
recorder is the *start* of somebody's production chain: whatever it spends, the
editor, the transcode and the player have to fit inside what is left.

That is the reportable limit, not the design target. `clipped-audio` holds its
track to within **20 ms** of the reference clock by construction — its timeline
compares every packet against the position the anchor and the frame count
predict, and corrects anything beyond a 20 ms deadband — so the tolerance above
has two to three times the headroom over what the pipeline should ever produce.
A recording that reaches 60 ms has a fault in it, not bad luck.

`SyncState` says which way: `InTolerance`, `Ahead` or `Behind`. Which way matters
more than the magnitude when diagnosing, because a track that is `Ahead` — sound
before picture — has been given *fewer* samples than the elapsed real time
accounts for, and one that is `Behind` has been given more. A track's timestamps
are its anchor plus its frame count, so the count is the diagnosis, and the two
directions have different causes: a slow endpoint crystal or lost packets on one
side, a fast crystal or over-synthesised silence on the other.

## What happens when they disagree

Nothing here shifts a timeline to hide a disagreement. Each case is handled where
it arises, and every one of them is counted rather than silently absorbed.

| What happened | Where it is handled | What is done | Why |
| --- | --- | --- | --- |
| Video frames the machine could not keep up with | `clipped-capture` | Nothing is inserted. The next frame carries its own later timestamp, and `CapturedFrame::frames_missed` reports how many went by. | The gap is real: those frames were never composed. Duplicating the previous frame to fill it would add latency to everything after it and lie about the game's pacing. |
| A video source timestamp that does not advance | `clipped-muxer` | The decode timestamp is nudged one tick past its predecessor, and counted in `RecordingSummary`. | The container requires increasing decode timestamps. Dropping the packet loses picture; refusing the write loses the session (AGENTS.md section 17). |
| The audio endpoint delivering nothing (silence, no default device, an unplugged headset) | `clipped-audio` | Silence of exactly the length of the gap, measured against the device's clock, or against a counter reading where the device has nothing to say. | This is the classic loopback bug: a track that concatenates what it is given is shorter than its recording by the amount of silence in it, and every sound after the first quiet passage lands early — cumulatively, all session. |
| An audio packet arriving with a position later than the track expects, by more than 20 ms | `clipped-audio` | That much silence is emitted in front of it. | Same as above, at packet granularity. |
| An audio packet arriving with a position *earlier* than the track expects, by more than 20 ms | `clipped-audio` | The overlapping frames are trimmed off its front, or it is dropped if it lies entirely inside covered time. | Silence has already been synthesised over that period; emitting both would make the track longer than the recording, which is the same bug in the other direction. |
| An audio packet within 20 ms of where the track expects it | `clipped-audio` | Emitted unchanged. | Consecutive packets' reported positions vary by tens of microseconds. Correcting that would insert or trim a frame in almost every packet of a recording, audibly, for no benefit. Because the comparison is always against the anchor and never against the previous packet, an ignored difference is still there next time: the deadband **bounds** the offset rather than letting it accumulate. |
| A source clock that steps | `clipped-capture`'s `DriftEstimator` | The step ends the current rate fit and starts a new one, and is counted as a discontinuity. The peak and latest offsets still span the whole run. | A step is not drift. Fitting a line across one measures the step rather than the crystal — a single 20 ms correction in a half-hour run reads as 0.7 ms/min of drift that does not exist. But the recording still contains the error, so the peak is not reset with the fit. |
| A timestamp on a different clock | `CaptureClock` | Refused: `ClockMismatch`. | Subtracting two unrelated counters produces a number, and a recording built on it is wrong by an unpredictable amount that looks fine until somebody watches it. |
| The default audio endpoint changing mid-recording | `clipped-audio` | The capture follows it if the new endpoint presents the same shape of audio, and the track continues. If it does not — a different sample rate or channel count, which would need resampling that does not exist yet — the track continues as synthesised silence and the caller is told once (`Capture::FormatChanged`). | A recording is worth more than the audio it is missing (AGENTS.md sections 16 and 17), and changing shape underneath a writer that has already declared a stream is worse than silence. Note that a new endpoint is a **different crystal**, so the drift rate measured before the change does not apply after it — the estimator sees this as a discontinuity, which is the correct reading, and silence has no device position to observe at all. |

The cost of the deadband, stated plainly: an endpoint whose sample clock
genuinely differs from the performance counter is corrected in one 20 ms step
rather than continuously. At the four parts per million measured here, that step
would arrive about once every eighty minutes — and the thirty-minute run above
never reached it, which is why it recorded no corrections at all. Removing the step needs
resampling against the reference clock, which is issue #30.

## What this model deliberately does not do

- **It does not resample.** Correcting drift continuously, so that the deadband
  step never happens, is
  [issue #30](https://github.com/wildware-uk/clipped/issues/30) in M2. What is
  built here is the model, the measurement and the plumbing that makes that
  correction possible: the two accounts of every buffer's position, and an
  estimator that turns them into a rate a corrector can act on.
- **It does not decide the track layout.** How many audio tracks a recording
  has, and what goes in each, is
  [issue #28](https://github.com/wildware-uk/clipped/issues/28) and M2 routing.
  Every one of them is placed on the timeline described here.
- **It does not measure physical synchronisation.** Whether the sound leaving
  the speakers and the light leaving the panel are simultaneous depends on the
  audio endpoint's output latency and the display's, and measuring it needs a
  microphone and a photodiode. The recorder's responsibility is the timestamp
  domain: to place each source at the moment its own hardware said it happened.
- **It does not measure a file.** Both measurements are of the timestamps the
  pipeline produces, which is what a writer is handed; no recording is written or
  read back.
  [What the measurement can and cannot see](#what-the-measurement-can-and-cannot-see)
  is the full statement for the drift figures, and the drift measurement's other
  blind spot — a constant offset — is what
  [the absolute measurement](#the-absolute-offset-what-the-drift-measurement-cannot-see)
  exists for. Anything quoting the *rate* from this document is quoting a drift,
  not a verdict on a recording; the offset a recording contains is the second
  measurement's figure.
- **It does not apply the start-time alignment rule.** The rule is stated
  [above](#start-time-alignment-what-happens-to-audio-before-the-epoch); the
  component that would apply it, `clipped-session`, records video with no audio
  track beside it, so there is nothing yet to align
  ([issue #174](https://github.com/wildware-uk/clipped/issues/174)).
- **It does not own a session's threads.** `CaptureClock` is `Copy` and
  stateless once built, so each thread holds its own copy and no capture thread
  waits on a lock to time a packet (AGENTS.md section 20). Who creates the clock
  and when is `clipped-session`'s decision, and it makes it in
  `crates/session/src/recording.rs`: the clock starts at the timestamp of the
  first frame the recording keeps.

## Running the measurement

There are two tests in `tests/capture/av_sync.rs` and they are separate runs, so
each command below names the one it wants.

```text
# The drift: about ninety seconds, and silent.
CLIPPED_REQUIRE_AUDIO=1 cargo test -p clipped-video-pattern --test av_sync \
    -- --ignored --nocapture --test-threads=1 av_offset_stays

# The long drift run: thirty minutes, and silent.
CLIPPED_AV_SYNC_SECONDS=1800 CLIPPED_REQUIRE_AUDIO=1 \
    cargo test -p clipped-video-pattern --test av_sync \
    -- --ignored --nocapture --test-threads=1 av_offset_stays

# The absolute offset: about ninety seconds, and it plays a tone.
# CLIPPED_AV_SYNC_TONE_SECONDS lengthens it.
CLIPPED_REQUIRE_AUDIO=1 cargo test -p clipped-video-pattern --test av_sync \
    -- --ignored --nocapture --test-threads=1 the_absolute
```

Both need a GPU, a display and an audio endpoint, so both are `#[ignore]`d and
neither is part of the pull-request CI job. Both put a borderless window on a
non-primary display for the length of the run.

**The drift run makes no sound**: it holds a render stream open so that the
endpoint's clock keeps running, and every buffer it hands the audio engine is
marked `AUDCLNT_BUFFERFLAGS_SILENT`. **The absolute run does make a sound**,
because a measurement of where a recording puts a sound needs one — a 30 ms tone
at about −28 dBFS every five seconds, played by the subject rather than by the
test. Neither run needs the machine to be quiet: the detector looks for 997 Hz,
which is the frequency digital audio has used for this for decades because no
instrument plays it.

`CLIPPED_REQUIRE_AUDIO` is in all of the commands deliberately. Without it, a machine
whose default endpoint refuses a render stream, or which delivers no packets at
all, prints `SKIPPED (av-sync): …` and the run still passes — the code under test
is not at fault for either. With it, both become failures. Anybody collecting
evidence should set it, because a green run without it is not on its own proof
that an offset was measured.

Half an hour is also long enough for an unattended machine to put its displays
to sleep, and a display going away takes the window on it with it: the run then
loses the subject, starts it again onto a display that is still asleep, loses it
within seconds, and gives up on the video side. That is the failure to expect
from starting a long run and walking away, and the fix is to keep the machine
awake for the length of it — `SetThreadExecutionState` with
`ES_DISPLAY_REQUIRED`, which is what any recorder does while it records — rather
than anything in the test.

Half an hour is long enough for the machine to be used, and the subject is a
topmost window on somebody's display: it can be closed, and the session can lock.
The test therefore starts the subject again when its window goes away, up to
three times, and says so in its output. After the third loss it gives up on the
video side and lets the audio side — which is where the offset is measured — run
to the deadline; the run then fails unless video *covered* at least half of it —
the sum of what each subject was up for, not the interval from the first frame to
the last, so that time with nothing being captured cannot pass for coverage. The
first thirty-minute attempt at this measurement was lost exactly that way, twelve
minutes in, which is why the restart is there.

The restart matters to the absolute run as well, and for a reason worth stating:
each subject announces the frames its own tones belong to, so a run that has to
start a second one picks up that subject's plan rather than carrying the dead
one's. The second of the two runs recorded above lost its window 42 seconds in
and still measured all nineteen of its tones.

The pure arithmetic underneath it — the epoch conversion, the rate fit, what a
discontinuity does, what an empty estimator reports — is unit-tested in
`crates/capture/src/time.rs` and runs everywhere, including on a machine with no
hardware at all. The per-buffer plumbing is asserted against a real endpoint in
`crates/audio/tests/system_audio.rs`, and the arithmetic that places a tone at a
moment — the stream-index conversion, what the announced moment means, and what
happens to a tone that cannot be placed at the moment it was asked for — is
unit-tested in `test-apps/video-pattern/src/tone.rs` and needs no sound card.

## Assumptions

Where one of these stops holding, this model needs revisiting rather than
patching.

- **The video source and the audio device report positions on the same monotonic
  clock.** True on Windows for both capture APIs and for WASAPI. A platform
  where it is not true needs a conversion between two clocks, which is a
  measurable rate of its own and would belong in `CaptureClock`.
- **The reference clock does not step.** `QueryPerformanceCounter` is monotonic
  and continuous across sleep on supported hardware. A step would be seen as a
  discontinuity by the estimator, which is the right reading, but the recording
  would contain it.
- **One video source per recording.** Capturing two windows at once is not in
  the capture interface and is not planned; a second video source would be a
  second candidate reference clock and this document would need a rule for
  choosing.
- **The audio endpoint's rate error is constant over a recording.** True of a
  crystal to within its temperature coefficient, which is far below the
  resolution of anything here. Not true across a device change, which is why
  that is treated as a discontinuity.

## Related

- [capture-pipeline.md](capture-pipeline.md) — where video timestamps come from
  and why they are never read from a clock.
- [audio-routing.md](audio-routing.md) — the audio timeline, the deadband and
  how silence is synthesised.
- [muxing.md](muxing.md) — how a `MediaTime` becomes a container timestamp, and
  what the writer corrects.
- [testing.md](testing.md) — the controlled test applications the measurement
  runs against.
