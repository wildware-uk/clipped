# 0010. A replay save never reaches across a gap, and one that lands entirely inside one is refused

- Status: Accepted
- Date: 2026-08-17
- Issue: [#574](https://github.com/wildware-uk/clipped/issues/574)

## Context

`ReplayBuffer::lease_last` resolves "the last thirty seconds" against the newest
**picture** in the buffer, not against a clock, and eviction runs only when a
packet arrives. Nothing in `clipped-replay` reads a wall clock, deliberately, so
that a buffer behaves identically whether a test pushes an hour of video in a
millisecond or a recording takes an hour to produce it (AGENTS.md section 25,
`crates/replay/src/range.rs`).

A capture backend hands over a frame when its source's content *changes*, so a
source can stop delivering frames for an unbounded stretch without anything
being wrong:

- **A minimised window.** Alt-tabbing out of an exclusive fullscreen game
  minimises it, and the recording loop deliberately waits that out rather than
  ending the session over a keypress
  ([#383](https://github.com/wildware-uk/clipped/issues/383)). This is the
  common case by a wide margin.
- **A display that has powered down**, which stops Desktop Duplication
  delivering frames while every handle it holds stays valid
  ([#461](https://github.com/wildware-uk/clipped/issues/461)).

Two failures follow, and neither announces itself.

**A save taken during such a stretch returned the video from before it, marked
complete.** Thirty seconds of gameplay from two hours ago, `is_complete()` true,
`shortfall()` zero, `ReplayStats::covered` reporting the pre-stall range, and no
`LeaseError` capable of firing: `Empty` needs no keyframe to have ever arrived,
and `OutsideBuffer` cannot fire because the range is derived from the newest
picture and therefore always overlaps.

**A save taken after video resumed spanned the gap.** The selection reaches back
from the newest picture, so it picks up the last segment from before the stall
as well; the clip is then as long as the stall — a two-hour file where thirty
seconds were asked for — and `is_complete()` is still true. That breaks the
tolerance `docs/replay-buffer.md` states as a guarantee, `requested length ≤
clip length < requested length + segment length`.

The buffer already defends against the *other* cause of a gap.
`Inner::resume_after_any_gap` drops everything from before a gap the memory
ceiling created, citing AGENTS.md section 22 for the reason: a clip that jumps
without saying so. The reasoning was already right and already written down; it
was reachable only from the ceiling path, because the buffer could not tell "the
encoder produced nothing" from "no time has passed".

What has to remain true afterwards: no clock inside `clipped-replay`; a clip that
is short says so in the vocabulary that already exists (`is_complete`,
`shortfall`) rather than a third one; and a replay buffer may never cost somebody
their recording (AGENTS.md section 17).

Out of scope: keeping the display awake while a buffer is armed, which is
[#461](https://github.com/wildware-uk/clipped/issues/461)'s first question and a
real imposition on somebody's power settings; and surfacing the silence in
`RecordingReport`, which belongs with the rest of that measurement.

## Decision

A stretch with no picture for longer than **one segment** is a gap. A lease never
selects across one, and "the last N seconds" is measured from *now* rather than
from the newest picture.

In practice:

1. **The buffer detects a resumption itself**, from the packets, in media time: a
   picture more than a segment beyond the newest one held. It then takes the
   three steps the ceiling already takes — seal the open segment where it stands,
   discard packets until the encoder's next keyframe, drop what was held from
   before the gap when that keyframe opens a segment.
   `Inner::resume_after_any_gap` is shared between both causes.
2. **Whoever is capturing reports a silence that is still going on**, through
   `ReplayBuffer::note_source_silence(elapsed)`. `crates/session/src/recording.rs`
   calls it on every acquisition that found no frame and on every one that found
   the window minimised. The buffer stores it, forgets it on the next packet, and
   adds it to the newest picture to get "now".
3. **A save is answered in the existing vocabulary.** Reaching back over a gap
   gives the resumed video only, `is_complete()` false and `shortfall()` the part
   that predates it. Reaching into a silence that is still going on gives what
   there is, short by the silence.
4. **A save whose whole request predates the gap is refused**, with a new
   `LeaseError::SourceSilent` naming how long nothing has been captured and what
   is actually held.
5. **A named range (`lease`, not `lease_last`) is unaffected.** A caller that
   named two instants asked for those instants.

One segment is the threshold because it is where the promise in
`docs/replay-buffer.md` breaks: a stretch without pictures inside the selection
adds its own length to the clip, so anything longer than a segment puts the clip
outside `requested length + segment length`. Shorter than that is inside the
slack a clip already carries.

## Alternatives

### Serve the stale footage and carry the age through to the user

The clip is written as before and something — `SavedClip`, the notification, the
library row — says "this is 2 hours old". Its case is real and it is the option
that loses the least: thirty seconds of gameplay from before the stall may well
be the most interesting thirty seconds in the buffer, and the user is the one who
knows whether they want it. It is also the least disruptive change, since no
existing save starts failing.

Rejected because the age has to survive every hop to be worth anything, and it
does not. A clip's age would have to cross `SegmentLease`, `SavedClip`,
`ReplaySaveError`, the IPC protocol, the desktop's notification and the library
row, and the failure mode of every one of those hops is that the clip arrives
looking exactly like a good one. That is the same class of defect as the bug —
a confident wrong answer — with more places to go wrong. Worse, it needs a new
word for "old" alongside `is_complete` and `shortfall`, when the case is already
expressible: a request for thirty seconds that contains none of the last thirty
seconds is not incomplete by some amount, it is entirely unmet.

### Keep both sides of the gap and let the clip jump

Do nothing to the selection; simply mark the lease incomplete when it spans a
gap. It keeps the most video, which is the thing a replay buffer exists to keep.

Rejected because the clip is then as long as the gap: a two-hour file, of which
all but a few seconds is a frozen picture, when thirty seconds were asked for. It
breaks the length tolerance callers were told to rely on, it is unbounded in size
on disk, and `resume_after_any_gap` already rejected exactly this argument for
the ceiling case. Two answers to one question would also mean the buffer behaves
differently depending on which cause produced the gap, which nothing downstream
could explain.

### Give the buffer a clock

Have `ReplayBuffer` read `Instant::now()`, so that "now" needs no caller to
report it and nothing can be forgotten. That is genuinely tempting: the
notification path is the one part of this that a future caller can omit, and the
"check the consumer, not only the producer" failure is a real one in this
codebase.

Rejected because it destroys the crate's testability. Media time and wall time
are the same thing only in a live recording; every test here pushes an hour of
video in a millisecond, and `crates/replay/examples/buffer_memory.rs` measures a
thirty-minute window by encoding as fast as the GPU will go. A buffer that read a
clock would report a thirty-minute silence during its own memory measurement.
AGENTS.md section 25 asks for exactly this, and the packet-derived half of the
decision recovers most of what the clock would have given: the resumption case
needs no caller at all, and the reporting path is asserted where it is made, in
`crates/session/src/recording.rs`'s own tests.

### Refuse every save that touches a gap at all

Simpler to state, and impossible to get wrong. If any part of the request
predates a gap, refuse.

Rejected because it throws away clips that are worth having. Ten seconds after a
window is restored, a save of the last thirty is ten real seconds of what just
happened — the same case as a hotkey pressed ten seconds into a session, which
this crate already answers by writing the clip there is and saying it was short.
Refusing would make the buffer useless for the first window's-worth of every
resumption, which is precisely when somebody who has just alt-tabbed back in is
most likely to press the key.

### Detect the gap only at lease time, keeping the pre-gap segments

Record where the discontinuity is and refuse to *select* across it, leaving the
older material in the buffer for an explicit `lease(range)`. It is less
destructive, and a false positive would cost nothing permanent.

Rejected because it does not work at segment granularity. The resumption packet
is not necessarily a keyframe, so the gap can fall in the middle of the open
segment; a selection clamped to segment boundaries would still carry the pre-gap
half of it, which is the whole two hours. Making it work would mean trimming
packets off the front of a segment, and a segment that does not begin on a
keyframe cannot be decoded — the crate's central invariant. Sealing at the gap
and waiting for a keyframe is what the ceiling path already does, and it costs at
most one keyframe interval of resumed video.

## Consequences

**A save can now fail where it used to succeed.** `LeaseError::SourceSilent` is a
new refusal reaching `ReplaySaveError::NothingBuffered`, the IPC error and the
desktop. It is the correct answer, but it is a behaviour change for anyone whose
source goes quiet, and the message is doing real work: it has to leave somebody
understanding that their window stopped drawing rather than that Clipped is
broken (AGENTS.md section 45). Worth watching once this is in front of people.

**A stall costs the window.** After a gap the buffer starts again from empty, so
saves are short for one window's-worth of resumed video and say so. That is the
price of not carrying material across a gap and it is paid every time somebody
alt-tabs out for more than two seconds — which, in a build where minimising is
routine, will be often. If it turns out to annoy in practice, the alternative
that wins next is not "keep both sides" but the second one above: carry the age
through, with the plumbing done properly.

**Every caller that owns a buffer now owes it a silence report.** A future
recording loop, or a Manual/Replay-mode capture with no continuous file
([#423](https://github.com/wildware-uk/clipped/issues/423)), that never calls
`note_source_silence` reintroduces the during-the-stall half of this defect and
nothing in `clipped-replay` will notice. That is why the hand-off is asserted in
`crates/session/src/recording.rs` rather than only around it, and why the
packet-derived half exists at all: the resumption case cannot be forgotten, so at
worst a forgetful caller is wrong for the length of the stall rather than for
ever.

**Up to one keyframe interval of resumed video is discarded**, counted as
`ReplayStats::packets_discarded_after_a_source_gap`. It is zero whenever video
resumes on a keyframe, which is what an encoder producing them on a timer does
after any real stall — every encoder in this workspace — so the counter being
non-zero is the thing to watch.

**The threshold is a judgement and is not measured.** One segment is derived from
a documented tolerance rather than from data about how long real sources go quiet
for. It must stay above an encoder's reordering depth, which is why a small fixed
value would be wrong: the reordering fixture in `crates/replay/src/save.rs` hops
200 ms between consecutive presentation times with no gap existing, and lowering
the threshold to 100 ms fails that test. A segment is a keyframe interval, which
is comfortably above it. If a source is found that legitimately pauses for
minutes — a menu nobody is touching — this decision costs it its window, and the
number to revisit is this one.

**`ReplayStats::covered` must now be read beside `source_silence`.** On its own it
still reports a range a save can honestly produce, but it says nothing about how
old that range is. Any future diagnostics screen showing one without the other
would report a healthy buffer holding nothing anybody could use.
