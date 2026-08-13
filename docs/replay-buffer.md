# Replay buffer

Clipped keeps the last few minutes of a game in memory so that a hotkey pressed
*after* something interesting still produces a clip of it. This document covers
the part of that which exists today: the rolling buffer of encoded segments in
`crates/replay` ([issue #35](https://github.com/wildware-uk/clipped/issues/35)),
what it costs, what it does when it cannot have what it asked for, and — since
[issue #37](https://github.com/wildware-uk/clipped/issues/37) — how a leased
range becomes a playable file.

**Something asks for one now.** `clipped-recorder replay` records with a buffer
and binds `Ctrl`+`F10` to a save
([issue #38](https://github.com/wildware-uk/clipped/issues/38),
`docs/recorder-cli.md`); `start_recording` takes a `replay_seconds` and
`save_replay` saves from it over the protocol (`docs/ipc.md`).
`clipped_session::replay::ReplayRecording` is the join between the two — it is
what knows the video in the buffer well enough to write a container header for
it — and `docs/sessions.md` is where a saved clip is written down.

What still does not exist is a capture that keeps *only* the buffer: SPEC.md
section 4's Manual/Replay mode writes no continuous file, and this build has no
recording without one, which is
[issue #423](https://github.com/wildware-uk/clipped/issues/423). Spilling
segments to disk for long durations is
[issue #36](https://github.com/wildware-uk/clipped/issues/36), and the
"per-configuration ceiling" section below is the argument for why it has to
exist.

## Encoded segments, never raw frames

The rule SPEC.md section 16 states, in the numbers that make it a rule rather
than a preference. One 1080p60 BGRA frame is 1920 × 1080 × 4 = 7.9 MiB:

| Buffer | Raw BGRA frames | Encoded at 18.7 Mbit/s |
| --- | --- | --- |
| 30 seconds | 13.9 GiB | 67 MiB |
| 5 minutes | 139 GiB | 667 MiB |
| 30 minutes | 834 GiB | 3.9 GiB |

Memory here is in binary units throughout — MiB is 1024², GiB is 1024³ — which
is the quantity Windows Task Manager shows and labels MB and GB. Bitrates are in
decimal Mbit/s, as bitrates always are.

Any design that buffers frames before the encoder has already failed, whatever
its other merits. So `clipped-replay` sits after the encoder: `clipped-session`
drains each encoded packet into the Matroska writer and into the buffer, and
**there is one encode**. A recording and a replay buffer running at the same
time cost one extra `memcpy` per packet, not a second encoder session.

The consequence worth internalising is that a replay buffer's footprint depends
on the **bitrate and the duration** and not on the resolution or the frame rate.
Recording 4K instead of 1080p makes the buffer bigger only in so far as it makes
the bitrate bigger.

## The segment model

A segment is a run of packets that **begins on a keyframe**. That is the whole
of the design, and everything else follows from it:

- A coded picture references the pictures before it, so a stream can only be cut
  immediately before a keyframe. A segment that begins on one can be decoded on
  its own; one that does not can only be decoded from the previous keyframe.
- Therefore the buffer's unit of eviction is the segment. Dropping the oldest
  *packet* would leave every later packet in its group of pictures undecodable
  while still occupying memory.
- Therefore a save is bought at segment granularity: the clip begins at the
  keyframe at or before the requested start, and ends at the end of the segment
  containing the requested end.

Segments are stored as one `Vec<u8>` per segment with an index of packet
offsets, rather than a `Vec<u8>` per packet. At 60 frames a second a thirty
minute window is 108,000 packets, and 108,000 allocations for one buffer is a
cost paid on the thread that is also capturing (AGENTS.md section 18).

A segment reserves one segment's worth of bytes when it opens, and grows by that
same step rather than by doubling. Two things follow, and the first is the
smaller: filling a segment the encoder produced at the bitrate it was configured
for costs **one** allocation, and one more per further segment's worth of
overshoot. The measurements below do not support a stronger claim than that —
NVENC achieved 24.04 Mbit/s against a configured 18.66, so every segment in
those runs outgrew its reservation once. The second is why the step is fixed:
the memory ceiling is enforced against the allocation and not merely the
payload, and a capacity that doubled could carry a segment from just under the
ceiling to well over it inside a single push.

### Granularity, and what it costs

The segment length is a target; the encoder's keyframe interval is what the
buffer can actually act on, so the granularity is the larger of the two.
`clipped_encoder::KeyframeInterval::DEFAULT` is a keyframe every two seconds and
`clipped_replay::DEFAULT_SEGMENT` matches it.

At that default:

- A lease carries **up to two seconds of extra video before the requested
  start** and **up to two seconds after the requested end**.
  `SegmentLease::leading_slack` and `trailing_slack` report exactly how much.
  The save keeps the first and trims the second; "What a save gives you" below
  is the rule and the reason.
- The requested range is always covered in full when the buffer held it.
  `SegmentLease::is_complete` says whether it did, and `shortfall` says by how
  much it did not.

Shortening the segment length buys precision and costs compression: a keyframe
is several times the size of a predicted picture, so a buffer of half-second
segments holds noticeably fewer seconds for the same memory. Lengthening it does
the reverse. Two seconds is the balance the encoder already strikes for
recordings, and there is no reason for the buffer to disagree with the file.

## Retention

Segments are held oldest-first. The oldest is dropped only when the ones behind
it still reach back over the configured window from the newest picture in the
buffer, so a buffer that has been running longer than its window holds:

```text
window  ≤  what is held  <  window + segment length
```

Erring on the side of extra is deliberate. A buffer holding slightly less than
its window would fail the request it was configured for, and the slack costs two
seconds of video. The extra is however much of the oldest segment is not yet
needed, and it cannot be trimmed because a segment that does not begin on a
keyframe cannot be decoded.

The rule is applied on **every packet** rather than at every segment boundary,
because the newest picture is in the segment currently being written. A buffer
that only evicted at a boundary would hold a whole segment more than it was
asked for and would grow past its ceiling until the next keyframe.

Supported windows are 30 seconds to 30 minutes (`MINIMUM_WINDOW`,
`MAXIMUM_WINDOW`). A window outside that is refused when the buffer is
configured, not discovered when a clip comes out short.

## Saving while the buffer is evicting

The hard part is not selecting the segments. It is that a save reads them while
the encoder is still producing new ones and the buffer is still throwing old
ones away, on three different threads. A thirty-second buffer of two-second
segments turns over completely in thirty seconds, and writing a five-minute clip
to a slow disk takes longer than that — so **by the time a save finishes
reading, the buffer will often have evicted every segment it started with**.

This is solved by a reference count rather than by a rule:

```text
ReplayBuffer::lease(range)  ──▶  SegmentLease
        │                             │
   Arc<Segment>                  Arc<Segment>      (the same segments)
        │                             │
   eviction drops this          the save keeps this until it is done
```

Each sealed segment lives behind an `Arc`. The buffer holds one reference, a
lease holds another, and eviction drops only the buffer's. A segment the buffer
has evicted is alive, unchanged and complete for as long as a lease holds it,
and there is no window in which it could be otherwise, because a lease's
references are cloned under the same lock eviction takes. The buffer keeps
evicted-but-leased segments in a list of their own so the memory a save is
holding open is reported rather than invisible, and they are released when their
last reader drops.

`crates/replay/tests/save_during_eviction.rs` is what holds that to be true: a
writer thread turns the whole window over many times while a reader thread walks
every byte of a lease and checks each packet against the frame number written
into its first eight bytes. The test fails if a lease loses a segment, gains
one, or reads bytes that were pushed for a different frame — and it asserts that
eviction really did overtake the reader, so that it cannot pass by racing
nothing.

### The newest two seconds

The material somebody just pressed a hotkey about is in the segment still being
written, which cannot be shared while it is being appended to. A lease therefore
takes a **copy** of the open segment — one `memcpy` of at most one segment,
about 4.5 MiB at 1080p60, once per save. Without it every clip would end up to
two seconds before the moment it was saved for, which is the part that mattered.

### What a lease costs the capture thread

`ReplayBuffer` takes its own lock, and the capture thread holds it for the
`memcpy` of one packet. That is a deliberate, bounded exception to AGENTS.md
section 18's "no locks on capture threads", and it is the same cost the capture
thread already pays: `clipped-session`'s bounded queue to the muxing thread
takes a lock inside `SyncSender::send` for every packet. The lock is never held
across a filesystem call, an unbounded allocation, or a wait on another thread.

The one moment a reader holds it longer is the open-segment copy above. Measured
on the hardware below, taking a lease over a full 5-minute window took **0.77
ms** — under a twentieth of a frame interval at 60 fps, once per save.

## What a save gives you

`clipped_replay::save_clip` takes a lease, a destination and a video track
description, and writes a Matroska file. It is **not a second muxer**: a clip is
the same encoded packets a recording is made of, in the same container, so the
save is a loop over the lease driving `clipped_muxer::MkvWriter` — the writer a
recording is written by (AGENTS.md section 55). Everything the container already
does for a recording, a clip therefore gets: timestamps rebased onto the first
packet, decode timestamps forced to increase, and a file that stays playable if
the process is killed while it is being written.

That is why `clipped-replay` depends on `clipped-muxer` rather than sitting
beside it. The dependency points one way — nothing in the muxer knows a replay
buffer exists — and README.md's "Dependency direction" carries the argument.

### The two ends are not symmetrical

```text
 segments      │▓▓▓▓▓▓▓▓│▓▓▓▓▓▓▓▓│▓▓▓▓▓▓▓▓│▓▓▓▓▓▓▓▓│
 requested          ├──────────────────┤
 written        ├──────────────────────┤
```

**At the front the clip is longer than was asked for**, and it cannot be
otherwise. A coded picture references the pictures before it, so a stream can
only be cut immediately before a keyframe; a clip that began at the requested
instant would open with pictures nothing can decode. It therefore begins at the
keyframe at or before the requested start, which at the two-second default is up
to two seconds early. `SavedClip::leading_slack` reports how much a particular
clip carries, so a caller can say so rather than leaving somebody to notice.

**At the end it is trimmed to the request.** Nothing after the requested end has
to be there for what precedes it to decode, so it is not written. The trim is
made *in decode order* — everything up to and including the last packet
*presented* at or before the requested end — which is what keeps it safe for an
encoder that reorders: every picture a written packet references was decoded
before it, so it was written too. Trimming on the first packet past the end would
drop pictures that later ones need.

So the tolerance a caller may rely on is:

```text
requested length  ≤  clip length  <  requested length + segment length
```

and the extra is at the front. Measured on the fixture in
`crates/replay/tests/save_clip.rs` — 640×360 at 60 fps, H.264, two-second
keyframes — a request for the previous 60 seconds produced a clip of **61.983
seconds in 3,720 packets, 1.983 s of which is before the requested start**,
15,507,949 bytes of coded video. `ffprobe` decodes all 3,720 pictures of it.

A clip the buffer could not fill is still written: a hotkey pressed ten seconds
into a session asking for the last thirty produces the ten seconds there are,
with `SavedClip::is_complete` false and `shortfall` saying what was missing.
Refusing would be worse — there is a clip to be had, and it is the clip somebody
asked for.

### Audio: there is none, and that is now a gap rather than a consequence

A replay is video only. That used to follow from the pipeline — a recording had
no audio track at all — and it no longer does: since
[issue #180](https://github.com/wildware-uk/clipped/issues/180) a recording has
a system track and a microphone track, and **a clip saved out of the buffer has
neither**. The buffer takes `clipped_encoder::EncodedPacket`s, which are coded
*pictures*; captured audio reaches the file as PCM on the muxing thread and is
not offered to the buffer at all (`crates/session/src/muxing.rs`).

SPEC.md section 42 asks for M3 to "preserve all audio tracks", so this is an
unmet requirement of the milestone rather than a design decision, and it is
[issue #40](https://github.com/wildware-uk/clipped/issues/40) — which was
written as the *verification* of the audio path and now also owns building it.
`apps/recorder/tests/replay_clip.rs` asserts `audio_stream_count(0)` on a saved
clip, so the gap is stated in a test rather than assumed.

`save_clip` takes a `VideoTrack` rather than a `RecordingLayout` so that this is
a property of the signature and not a comment: a caller cannot hand it audio
tracks that would be silently dropped.

### Where the work happens

Not on the capture thread. `save_clip` never touches the buffer at all — it
reads a lease, whose segments are immutable and kept alive by their own reference
count — so the only moment a save spends under the buffer's lock is taking the
lease, measured at 0.77 ms for a five-minute window above. What that asks of a
caller is one thing: **take the lease wherever is convenient and call
`save_clip` on a thread that is not capturing** (AGENTS.md section 20).

`crates/replay/tests/save_clip.rs` holds that to be true rather than asserting
it: a thread stands in for the capture and encode loop, pushing real coded
pictures into the buffer *and* writing each of them to a recording of its own,
while a clip is saved from the same packets on another thread. It waits for
capture to advance while the save thread is still running, so a save that blocked
the buffer — or one that wrote nothing — ends the wait with nothing counted and
fails. Afterwards the buffered video is checked frame by frame against the frame
numbers it was pushed with, so a single picture lost across the save is a
failure, and both files are decoded.

### Names, and two saves at once

Nothing in `save_clip` invents a file name, and a path that is already taken is
refused rather than overwritten (AGENTS.md section 56). Deciding what a clip
should be called belongs to the layer that knows what it is of, which is the
**session**: a clip is `clipped-<session-id>-replay-<n>.mkv`, beside the
recording it came out of and numbered within its sitting, so everything one
session produced sorts together in a directory listing and no two saves can
collide (`clipped_session::automatic::Session::clip_path`). A caller that names
its own destination — `save_replay`'s `output` — gets that instead, and a
destination that already exists is still refused.

Two saves at once need nothing special: two leases are two independent sets of
references and two writers are two files. The test above takes two leases a
moment apart out of a buffer that is still being written to, writes both
concurrently, and checks that the buffer took every packet throughout and will
still serve a third clip afterwards.

The contention it is really about is eviction, so the test waits for that rather
than assuming it. It begins only once the buffer's own
`segments_evicted_for_window` has moved — the window is rolling, not still
filling — and while both saves are in flight it waits for that count to rise
*again* and for `segments_retained_for_a_save` to become non-zero, which is the
buffer reporting that the window has moved past a segment a save is still
reading. That is the moment the lease exists for: the segment is out of the
buffer and alive because a reader holds it.

## Per-configuration ceiling

The memory a buffer occupies is arithmetic, not a guess:

```text
expected bytes = bitrate / 8 × (window + segment length)
ceiling        = expected bytes × 1.5
```

The 50 % headroom is for a rate control that overshoots — the measurements below
show why it is not generous. `ReplayConfig::memory_ceiling` is that number and
`with_memory_ceiling` overrides it; a ceiling below what the window needs is
refused rather than accepted, because a buffer that silently keeps ten seconds
when it was asked for five minutes is worse than one that says the numbers do
not fit.

At the 18.7 Mbit/s a 1080p60 recording is given:

| Window | Expected | Ceiling |
| --- | --- | --- |
| 30 seconds | 71 MiB | 107 MiB |
| 1 minute | 138 MiB | 207 MiB |
| 5 minutes | 672 MiB | 1008 MiB |
| 10 minutes | 1.31 GiB | 1.96 GiB |
| 30 minutes | 3.91 GiB | 5.87 GiB |

**A save in progress sits outside that ceiling.** While a clip is being written
the process holds the ceiling *plus the clip*, bounded by the clip's own length,
so a save of the whole window at most doubles the figure. It is outside rather
than inside deliberately: counting a lease against the ceiling would make a save
evict the buffer's history to pay for itself, collapsing it to a single segment
for as long as the clip took to write and leaving the next hotkey press with
nothing.

### When the machine cannot provide it

The ceiling is enforced, not merely documented. If what the buffer owns exceeds
it — which happens when the encoder is producing more than the bitrate the
buffer was sized from — segments are evicted beyond the window until it is under
again. The consequences are visible rather than silent:

- The window shortens. `ReplayStats::segments_evicted_over_ceiling` counts every
  segment lost that way, separately from the ones the window discarded normally,
  and `ReplayStats::covered` says how much history is actually there.
- It is reported once at `warn`, naming the ceiling and what is held.
- One sealed segment is always kept, so a save is never impossible.
- The recording is never affected. A replay buffer cannot fail a recording: it
  copies bytes into memory it already owns, and reaching its ceiling costs it
  its own oldest segments rather than costing the file anything (AGENTS.md
  section 17).

### The segment being written is not exempt

Evicting sealed segments cannot bound the buffer on its own, because the segment
currently being written is not one of them. A segment is sealed by the *next*
keyframe, so an encoder whose keyframe interval is longer than the buffer's
window produces a segment that never seals at all: one keyframe followed by five
minutes of predicted pictures is a single segment, and a ceiling weighed only
against the sealed queue lets it grow without limit. Measured, on a 30-second /
18.66 Mbit/s configuration: **1,196,228,696 bytes held against a 111,974,400
byte ceiling** — 10.7× over, covering 300 s of a 30 s window.

Nothing in this workspace configures an encoder that way:
`KeyframeInterval::DEFAULT` is two seconds, far more often than any supported
window. But the keyframe interval belongs to `clipped-encoder`, so "it cannot
happen" would be a property of another crate's current settings rather than of
this design.

The ceiling is therefore checked **before** each packet is copied in, against
what that append would cost, and when evicting sealed segments cannot free
enough room for it the buffer:

1. **Seals the segment being written where it stands.** A segment is cut at its
   end, not at its front, so what is kept still begins on a keyframe and is
   still decodable on its own. Nothing already buffered is thrown away, and a
   save made during what follows gets real video.
2. **Discards packets until the encoder's next keyframe**, counted by
   `ReplayStats::packets_discarded_over_ceiling` and returned as
   `PushOutcome::DiscardedOverCeiling`. There is nowhere else to put them: a
   segment that does not begin on a keyframe cannot be decoded, so admitting
   them would mean holding pictures no save could use.
3. **Drops what it held from before the gap** when that keyframe arrives.
   `lease_last` measures "the last thirty seconds" back from the newest picture,
   so a buffer holding both sides of a gap would select across it and write one
   clip that silently jumps (AGENTS.md section 22). Material from before a gap
   cannot serve the request this buffer exists for.

`ReplayStats::segments_sealed_at_the_ceiling` counts step 1, and is zero for
every encoder in this workspace. Three alternatives were weighed:

| Instead | Why not |
| --- | --- |
| Seal early and carry on into a segment that does not begin on a keyframe | Such a segment decodes only behind the keyframe segment it continues, so a 30 s clip would have to drag in the five minutes back to that keyframe. It costs the crate's central invariant and still does not produce the clip. |
| Refuse the packet and leave the segment open | Bounds nothing: the open segment is what is over the ceiling. |
| Drop the open segment outright and resume at the next keyframe | Bounds memory equally well, but throws away decodable video that sealing keeps for free. |

None of this makes such a configuration work — no buffer can cut a 30-second
clip out of a stream with a keyframe every five minutes. What it does is keep
the memory where this page says it is, keep every byte handed to a save
decodable, and put the loss in the statistics where somebody can see it.

Nothing here reserves memory up front, so there is no allocation to fail at
start-up; and nothing grows without bound, so there is no allocation to fail
during a match. What a 30-minute window at 3.9 GiB does to a machine with 8 GB of
RAM is nevertheless a real problem, and the answer to it is issue #36:
disk-backed buffering, which trades the memory for a bounded amount of disk and
keeps the long durations SPEC.md section 16 asks for.

## Measured memory use

Real figures, from `crates/replay/examples/buffer_memory.rs`, which opens an
NVENC session and encodes into a live buffer. It is an example rather than a
test because it needs an NVIDIA GPU, takes minutes at the longer durations and
allocates gigabytes at the longest.

```text
cargo run --release --example buffer_memory -- --window 30
```

**Method.** 1920×1080 at 60 fps, H.264, NVIDIA NVENC, constant bitrate
18,662,400 bit/s — the rate `clipped-session` gives a 1080p60 recording — with a
keyframe every 2 seconds and 2-second segments. The encoder is fed frames of
pseudo-random noise, because a constant-bitrate encoder given easy content
spends less than it was configured to and a buffer that was never filled at the
stated bitrate measures nothing. Each run encodes 20 % more video than the
window holds, so the buffer is measured while it is evicting rather than while
it is still filling. Timestamps come from the frame number rather than a clock,
so a 30-minute buffer is filled with 30 minutes of *video* as fast as the
encoder will produce it rather than in 30 minutes of waiting; what the buffer
holds is identical either way, because retention is measured in media time.
Process memory is `GetProcessMemoryInfo`, sampled once per second of video,
reported as the growth from a baseline taken after the encoder session and the
source textures were allocated — so the figures are the buffer and not the
harness.

**Hardware.** Windows 11 Pro build 26200, NVIDIA GeForce RTX 4090 (driver's
NVENC, H.264), AMD Ryzen with 61 GB of RAM.

**Results.**

| Window | Held | Peak | Ceiling | Working set growth | Private growth | Wall clock |
| --- | --- | --- | --- | --- | --- | --- |
| 30 s | 91.7 MiB | 92.8 MiB | 106.8 MiB | 103.9 MiB | 132.1 MiB | 5.7 s |
| 5 min | 838.3 MiB | 839.4 MiB | 1007.8 MiB | 851.2 MiB | 879.6 MiB | 89.2 s |
| 30 min | 4985.0 MiB | 4985.9 MiB | 6013.4 MiB | 4999.8 MiB | 5043.5 MiB | 544.2 s |

Every figure in that table is from the run transcribed below it, or from the two
runs of the same binary made beside it; nothing is carried over from an earlier
build.

"Held" is the buffer's own accounting, "working set growth" is what the process
actually grew by, and the two agreeing to within about 3 % is the point of
measuring both. Private bytes run higher — 28 MiB above the working set at 30
seconds and 44 MiB at 30 minutes — which is the allocator holding pages the
buffer has handed back.

Each run held its window and one segment exactly as the retention rule says:
32.0 s, 302.0 s and 1802.0 s covered, for windows of 30 s, 300 s and 1800 s with
2 s segments. No run hit its ceiling, and no run cut a segment short.

**These figures are 24 % to 29 % above the arithmetic**, and the reason is worth
stating rather than smoothing over. The buffer was sized for 18.66 Mbit/s and
NVENC produced 24.04, 23.29 and 23.21 Mbit/s in the three runs: a
constant-bitrate encoder fed pure noise overshoots, because there is nothing to
predict and every macroblock costs. Real game footage compresses, so a buffer of
the same window recording a game will sit closer to the 71 MiB / 672 MiB /
3.91 GiB the table above predicts. What the runs demonstrate is the harder case:
**the 50 % ceiling headroom absorbed a bitrate overshoot of up to 29 % at every
duration, with no segment evicted over the ceiling.**

The 30-minute run encoded 129,600 frames — 36 minutes of video, the window plus
20 % — in 544 seconds of wall clock, because the timestamps come from the frame
number. It was not filled in real time and does not claim to have been. The wall
clock column is how fast this machine could encode noise while it was also
running a test suite, not a property of the buffer; an earlier run of the same
code did the 30-minute fill in 339 s.

A save of the whole window took **0.88 ms** at 30 minutes and 0.77 ms at 5
minutes: cloning 901 `Arc`s and copying one open segment. It is flat in the
window's length, which is what makes it safe to do from a hotkey while the
capture thread is running.

One transcript, verbatim, so the shape of the output is on the record. This is
the 30-second run in the table above, figure for figure; the other two differ
only in their numbers:

```text
> cargo run --release --example buffer_memory -- --window 30
configuration
  encoder          NVIDIA NVENC, H.264
  picture          1920x1080 at 60 fps
  bitrate          18.7 Mbit/s (constant)
  buffer           30s of 18.7 Mbit/s in 2s segments, at most 107 MiB
  keyframes        every 2 s
  ... 30% after 2 s
  ... 50% after 3 s
  ... 80% after 5 s

what was encoded
  frames submitted 2160
  media time       36.0 s
  wall clock       5.7 s
  achieved bitrate 24.04 Mbit/s

what the buffer holds
  segments         16
  packets          1920
  covered          32.0 s
  bytes held       91.7 MiB
  peak bytes held  92.8 MiB
  ceiling          106.8 MiB
  evicted (window) 2
  evicted (ceiling) 0
  cut short        0
  discarded (ceiling) 0

what the process holds
  working set before 78.9 MiB
  working set after  182.8 MiB
  working set growth 103.9 MiB
  private before     207.8 MiB
  private after      339.9 MiB
  private growth     132.1 MiB

a save of the whole window
  segments leased  16
  bytes leased     88.1 MiB
  complete         true
  time to lease    0.823 ms
  working set      188.3 MiB
```

**And this is the argument for issue #36.** A 30-minute window costs the better
part of 5 GiB of resident memory on a machine that is also running a game. That
is affordable on 32 GB and it is not affordable on 8 or 16, which is why
disk-backed buffering exists as a ticket rather than as an optimisation.

## Attached to a live recording

`clipped_session::record_with_replay` is `record` with a buffer to fill. The
caller owns the **handle** — `clipped_session::replay::ReplayRecording` — rather
than the buffer, and the difference is the point: a clip's container needs the
codec, the picture size and the parameter sets the encoder produced, and those
exist only once a recording has opened one. The handle is created with a window,
the recording fills in the rest when its encoder opens, and
`ReplayRecording::save_last` is what a caller calls. A save runs on another
thread while the recording carries on.

The filling-in is `clipped_session::start_buffer`, one function rather than two
statements inside the recording loop, because the two are only correct together:
the buffer is built from the track the recording will declare and the bitrate
its encoder was given, and the thing every packet is copied into has to be the
buffer *that* built. Doing the first and not the second is a recording keeping
an empty buffer; pushing into a buffer some other handle owns is a clip
declaring a track it does not contain. Both are silent until somebody presses
the key, and both are asserted in `apps/recorder/tests/replay_clip.rs` with
coded video instead of a graphics device.

**A saved clip is written down where the recording is.** `apps/recorder`'s
`replay` subcommand and its `serve` both go through one routine
(`apps/recorder/src/replay.rs`): write the clip, then enter it in the session's
own record — the sidecar `clipped-library` indexes — so a replay reaches the
library exactly as the recording beside it does, in the `clips` table that was
designed for it (`docs/sessions.md`, `docs/storage.md`). The order is the crash
safety: the file first, the record after it exists, so an interrupted recorder
leaves a clip nothing indexed rather than a library row for a file that was
never written.

`crates/session/examples/replay_probe.rs` is the same wiring with the buffer's
own accounting printed instead of a clip — `clipped-recorder replay` is what a
person uses. It records a window for a stated number of seconds with a buffer
attached and prints what the buffer ended up holding. Against
`test-apps/video-pattern` at 1920×1080 and 60 fps for 50 seconds, on an RTX 4090
encoding AV1 through NVENC:

```text
Recorded 2929 frames of 1920x1080 AV1 in 49.78s (NVIDIA NVENC, Windows Graphics
Capture, 58.8 fps sustained; 0 frames dropped). Stopped by request.

replay buffer after 50.0 s
  segments held      16
  packets held       1849
  bytes held         5048786
  peak bytes held    5052665
  ceiling            111974400
  covered            31.7 s
  evicted (window)   9
  evicted (ceiling)  0
  discarded pre-key  0

a save of the last 15 s
  segments leased    8
  beginning on a key 8
  packets            889
  bytes              137722
  requested          34.779s to 49.779s
  covered            34.412s to 49.779s
  complete           true
  leading slack      0.367 s
  trailing slack     0.000 s
```

The buffer held 31.7 s of a 30 s window in 16 segments and evicted 9, which is
the retention rule doing exactly what the arithmetic says. The bytes are small —
5 MB for 31.7 s, against a 107 MiB ceiling — because a test pattern is nearly
free to encode and AV1's rate control spends what the content needs rather than
what it was offered. That is the design working, not a measurement to quote for
a game: the buffer's footprint follows the bitrate the encoder actually
produces, which for a real game is the configured one.

## Threading

```text
 capture + encode thread                    save thread
 ───────────────────────                    ───────────
 acquire a frame
 submit it to the encoder
 drain packets ─┬─▶ bounded queue ─▶ MkvWriter
                │
                └─▶ ReplayBuffer::push        ReplayBuffer::lease(range)
                        (memcpy)                       │
                                                 SegmentLease
                                                       │
                                              save_clip ─▶ MkvWriter
```

One writer, any number of readers. The buffer takes the lock itself so that
neither side has to remember to, and a reader that panics while holding it does
not end the recording — the lock is taken through `PoisonError::into_inner`,
because the state behind it is a queue of immutable segments and a byte count,
and a wrong number in a report is a smaller failure than a recording that stops
(AGENTS.md section 17).

## What is not decided here

- **Audio.** A clip is video only, and SPEC.md section 42 asks for M3 to keep
  every track. See "Audio: there is none" above;
  [issue #40](https://github.com/wildware-uk/clipped/issues/40) is where the
  buffer learns to carry it.
- **Interaction with a full-session recording**, and with automatic highlight
  clipping in M10. Both consume the same packets and neither has been written.

Related decisions: [ADR 0001](adr/0001-mkv-archival-container.md), because
segments are standard containers rather than an application-specific format.
