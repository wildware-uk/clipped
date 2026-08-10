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
track's first buffer landed 258 ms before the first video frame, because the
audio thread opens its endpoint while the capture backend is still initialising.
An unsigned media time would turn that 258 ms lead into eighteen billion seconds
of lag, silently. `clipped-muxer` clamps a negative timestamp to the start of the file
and counts it (`docs/muxing.md`), which is the right thing to do with a packet
that genuinely precedes the recording.

The epoch does not have to be the earliest moment in the recording, and nothing
in the model assumes it is.

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

That offset is the audio/video offset in the finished file. Not an estimate of
it: video timestamps *are* performance-counter readings, so how far the audio
track has slid from the counter is how far it has slid from the picture.

Both halves are on every buffer `clipped-audio` hands over —
`CapturedAudio::timestamp` is the track's account, `CapturedAudio::device_timestamp`
is the endpoint's — and `clipped_capture::DriftEstimator` turns a stream of the
pairs into a rate.

### The measurement

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

Measured on the development machine (Windows 11 build 26200, Razer BlackShark V2
Pro 2.4 GHz wireless headset as the default endpoint at 48 kHz, capture of a
1280×720 30 fps pattern window on a non-primary display):

| Run | Endpoint buffers | Peak A/V offset | Final A/V offset | Drift rate | Corrections |
| --- | --- | --- | --- | --- | --- |
| 60 s | 6,016 | −0.809 ms | −0.240 ms | −3.854 ppm (−0.231 ms/min) | 0 |
| 70 s | 7,000 | −0.845 ms | −0.282 ms | −3.752 ppm (−0.225 ms/min) | 0 |
| **30 min** | **180,009** | **−7.871 ms** | **−7.323 ms** | **−4.061 ppm (−0.244 ms/min)** | **0** |

The thirty-minute run captured 53,992 video frames over 1799.752 s alongside
1800.090 s of audio, with no synthesised silence, no dropped frames and no
subject restarts.

The interpretation of −4.061 ppm: the endpoint's clock runs about four parts per
million *fast* relative to the performance counter, so the audio track gains
about a quarter of a millisecond per minute against the picture. That is exactly
what the run measured — 7.3 ms of lead after half an hour — and it extrapolates
to roughly 29 ms over a two-hour recording, which is inside the tolerance below
and, on this endpoint, means the deadband correction may never fire at all. A
different endpoint will measure differently; the number to generalise is the
*method*, not the value.

The rate is small, but the point of measuring it is that it is *not zero and not
random*. A quarter of a millisecond a minute is invisible for the first ten
minutes and is a third of the tolerance budget by the end of a long session, and
the only reason it is knowable in advance is that it was fitted over thousands of
observations rather than eyeballed.

The same run also measures the video side against the reference clock: the
pattern's frame counter is decoded from one frame a second and fitted against
the capture timestamps, which gave 33.3334 ms between the source's frames
against a nominal 33.3333 ms at 30 fps — the video source and the reference
clock agree to one part in three hundred thousand, which is what "video
timestamps are performance-counter readings" looks like when it is checked
rather than asserted.

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
more than the magnitude when diagnosing, because a track running ahead of the
clock has been given too many samples and one running behind has been given too
few, and those have different causes.

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
| The default audio endpoint changing mid-recording | `clipped-audio` | The capture follows it; the track continues. | A recording is worth more than the audio it is missing (AGENTS.md sections 16 and 17). Note that the new endpoint is a **different crystal**, so the drift rate measured before the change does not apply after it — the estimator sees this as a discontinuity, which is the correct reading. |

The cost of the deadband, stated plainly: an endpoint whose sample clock
genuinely differs from the performance counter is corrected in one 20 ms step
rather than continuously. At the −4.061 ppm measured here, that step would arrive
about once every 82 minutes — and the thirty-minute run above never reached it,
which is why it recorded no corrections at all. Removing the step needs
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
- **It does not own a session's threads.** `CaptureClock` is `Copy` and
  stateless once built, so each thread holds its own copy and no capture thread
  waits on a lock to time a packet (AGENTS.md section 20). Who creates the clock
  and when is `clipped-session`'s decision.

## Running the measurement

```text
# About ninety seconds.
cargo test -p clipped-video-pattern --test av_sync -- --ignored --nocapture --test-threads=1

# The long run: thirty minutes.
CLIPPED_AV_SYNC_SECONDS=1800 cargo test -p clipped-video-pattern --test av_sync \
    -- --ignored --nocapture --test-threads=1
```

It needs a GPU, a display and an audio endpoint, so it is `#[ignore]`d and is not
part of the pull-request CI job. It puts a borderless window on a non-primary
display for the length of the run and makes no sound.

Half an hour is long enough for the machine to be used, and the subject is a
topmost window on somebody's display: it can be closed, and the session can lock.
The test therefore starts the subject again when its window goes away, up to
three times, and says so in its output. After the third loss it gives up on the
video side and lets the audio side — which is where the offset is measured — run
to the deadline; the run then fails unless video covered at least half of it. The
first thirty-minute attempt at this measurement was lost exactly that way, twelve
minutes in, which is why the restart is there.

The pure arithmetic underneath it — the epoch conversion, the rate fit, what a
discontinuity does, what an empty estimator reports — is unit-tested in
`crates/capture/src/time.rs` and runs everywhere, including on a machine with no
hardware at all. The per-buffer plumbing is asserted against a real endpoint in
`crates/audio/tests/system_audio.rs`.

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
