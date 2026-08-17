# 0015. Capture holds the display awake, and says so when a source goes quiet anyway

- Status: Accepted
- Date: 2026-08-17
- Issue: [#461](https://github.com/wildware-uk/clipped/issues/461)

## Context

Windows powers the displays down on an idle timer — fifteen minutes on this
project's development machine, and the default on mains power for most of them.
A display that has been powered down is not a dark display. It stops being a
source.

Measured on 2026-08-17 with
`cargo run -p clipped-capture --example duplication_probe`, which drives raw DXGI
and takes `clipped-capture` out of the path entirely. Both passes are three
seconds per output; the first was taken with the session idle for 5,765 seconds
against a 900-second display timeout, the second about a minute later after one
synthetic mouse-move event, which is the only thing that changed:

| Output | Pass | Displays off | Displays on |
| --- | --- | ---: | ---: |
| `\\.\DISPLAY2` | idle desktop | 0 frames, 12 timeouts | 496 frames, 0 timeouts |
| `\\.\DISPLAY2` | window repainting | **0 frames, 12 timeouts** | **542 frames, 0 timeouts** |
| `\\.\DISPLAY1` | idle desktop | 0 frames, 12 timeouts | 3 frames, 12 timeouts |
| `\\.\DISPLAY1` | window repainting | **0 frames, 12 timeouts** | **541 frames, 0 timeouts** |

The repainting rows are the ones that matter. A window drawing in alternating
colours is a real present, not a redraw of the same pixels, so a source that
delivers nothing for it is not an idle source. The `DISPLAY1` idle row with the
displays on is the behaviour this must never be confused with: a screen where
nothing is changing legitimately produces timeouts, and that is correct.

Throughout the dark pass `DuplicateOutput` succeeded, `AttachedToDesktop` was
true and `WmiMonitorBasicDisplayParams` reported `Active=True`. Every
`AcquireNextFrame` answered `DXGI_ERROR_WAIT_TIMEOUT` — the same value the
backend returns for an idle desktop, which is a normal and expected condition.
So **the recorder cannot tell "nothing is happening on screen" from "this screen
has stopped existing as far as I am concerned"**, and those want different
responses.

Three constraints shape the answer.

**Windows Graphics Capture is not a refuge.** Issue #461 assumed it was, on a
measurement of 363 frames at 59.99 fps with the displays down.
[capture-pipeline.md](../capture-pipeline.md) records the opposite from an
earlier session on the same machine: with both displays off the desktop
compositor drops to about 4 Hz, and `wgc_probe` delivered 40 frames and 80
timeouts in ten seconds against 597 and 0 with the displays awake. Whichever
figure is right, preferring Windows Graphics Capture is not a policy that makes
this go away, so the answer cannot be one that only covers the fallback.

**The pipeline writes the gap faithfully, and nothing downstream invents
anything.** Nothing in `clipped-encoder` or `clipped-muxer` duplicates or repeats
a frame; a stretch with no frames is a large jump in presentation timestamps and
a Matroska file that declares no nominal frame rate. That is
[av-sync.md](../av-sync.md)'s rule 5 working as designed — *a gap in a source is
filled, not closed* — and it is the right behaviour. It also means a recording of
a dark hour is a file whose timestamps say an hour and which holds four frames,
with nothing anywhere saying why.

**Silence is already, correctly, not treated as failure.**
`CaptureFallback` will not fall back on it, because a capture that produces no
frames is indistinguishable from a source producing none and the commonest cause
is a minimised window. So whatever is done here must not turn an idle desktop
into an error.

Out of scope: waking a display that is already off, which nothing a background
process can call does; and what an armed replay buffer holds across such a gap,
which turns out to be a separate defect (see Consequences).

## Decision

**A capture holds the display awake for exactly as long as it is open**, and
**a recording states the longest stretch in which its source produced nothing.**

The first half is `SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED)`,
taken in `clipped_session::recording::record` on the thread that runs the capture
loop and released when that scope ends —
`clipped_capture::DisplayAwake`, whose platform half is
`crates/capture/src/windows/display_required.rs`. It is a property the capture
backends need rather than an application preference, which is why it lives in
`clipped-capture` beside them.

Three details a contributor has to know.

- **The state is per thread**, and Windows drops it when that thread ends.
  `DisplayAwake` is therefore deliberately not `Send`: the hold cannot be taken
  on one thread and released on another, and it cannot be parked somewhere its
  owning thread outlives.
- **`ES_SYSTEM_REQUIRED` is not set.** Keeping somebody's computer out of sleep
  is a much larger decision than keeping a monitor lit, and a machine that
  suspends ends the capture anyway — which is a thing a recording can report
  rather than a thing it has to survive.
- **It cannot wake a display that is already off.** Neither can
  `WM_SYSCOMMAND`/`SC_MONITORPOWER`; only a real input event does, which is what
  the measurement above had to use. This prevents a capture going dark and does
  nothing for one that started dark.

That last point is why the second half is not belt and braces. The
`Acquisition::Timeout` arm of the recording loop was empty; it now accumulates
the acquisition timeout, keeps the longest unbroken stretch, says so once at
`warn` past `SILENT_SOURCE_THRESHOLD` (thirty seconds, the same figure
`CaptureFallback::note_silence` uses), and puts
`RecordingReport::longest_source_silence` on the report and in the sentence the
user reads. A minimised window is *not* counted there — it has its own count —
for the same reason `Acquisition` splits the two: "nothing new to show" and
"nothing to show until somebody acts" are different facts about a recording.

There is **no setting** for the hold. See the alternatives.

## Alternatives

### Do nothing, and rely on the selection policy

Issue #461's third question. Windows Graphics Capture is preferred, Desktop
Duplication is reached only when the newer API is unavailable or has declined a
target, and it is worth knowing how many real machines get there before spending
anything on it.

Rejected because the premise does not hold. The repository's own measurement has
Windows Graphics Capture at about 4 Hz with the displays off, which is a ten-fold
loss rather than a total one but is not "unaffected" — and a replay buffer whose
last thirty seconds holds four frames is no more useful than one holding none.
More importantly, the cost of doing nothing is not bounded by how rare the
fallback is: a machine that reaches it records nothing and says nothing, for
however long the screen stays dark.

### Surface the timeouts, and do not touch power state at all

Issue #461's second question taken alone: count the silence, log it, put it on
the report, and leave the display to Windows. It is the cheaper half, it is
entirely inside the recorder, and it imposes nothing on anybody's power settings.

Rejected as the *whole* answer, though it is half of what was chosen. It
converts a silent data loss into a reported one, which is a real improvement and
is why it is in the decision — but the recording still contains nothing. A
recorder that has been asked to be ready, watches the screen go dark, records an
empty file and then explains itself afterwards has not done the job. This is the
alternative that would win if the hold turned out to be unacceptable to users;
the machinery is already independent of it.

### A user setting, defaulting to on

Issue #461 asks for this in terms: `ES_DISPLAY_REQUIRED` "is a real imposition on
somebody's power settings, so it should be a decision rather than a default
somebody discovers". The cost is not trivial — a `SettingKey`, a `Preferences`
field, six read/write arms in `config/document.rs`, an `applies` arm in the
recorder and a control in the desktop settings deck, which
`settingsConformance.test.ts` enforces.

Rejected **for now**, and this is the closest of the alternatives. The imposition
is bounded by something the user did: there is no capture in this build that runs
without a recording, an armed replay buffer *is* a running recording writing a
continuous file, and a recording is a thing somebody started and can see. Holding
the display for the length of a recording is what every recorder does, and adding
a switch whose only effect is to let a recording silently become empty invites
exactly the failure this record exists to close.

What would make it win: [issue #423](https://github.com/wildware-uk/clipped/issues/423),
a buffer-only capture that writes no continuous file. The moment a capture can be
armed in the background with nothing on screen to show for it, an all-day hold on
somebody's monitors stops being "while you are recording" and becomes a
background imposition, and it needs a switch. Whoever implements #423 should add
the key at the same time.

### Have the backend report a long run of timeouts as a distinct `Acquisition`

A fourth variant beside `Timeout`, `TargetMinimised` and `SizeChanged` — say
`SourceDark` — so that the backend, which knows it is duplicating a display,
could name the condition instead of leaving the session to infer it from
elapsed time.

Rejected because the backend does not know it either. That is the whole finding:
`DuplicateOutput` succeeds, `AttachedToDesktop` is true, WMI says the monitor is
active, and the only signal is the absence of frames — which is exactly what an
idle desktop produces. A variant the backend could only ever raise on a timer is
a variant that belongs to the caller counting the timer, which is where it now
is. `Acquisition`'s existing split is between things the backend can genuinely
distinguish, and this would have broken that rule.

### Fabricate a repeat frame to keep the timeline full

Submit the last frame again on a long timeout, so a recording of a dark hour is
an hour of a frozen picture rather than an hour-long file with four frames in it.
It would make every player's seek bar behave, and it is what a fixed-rate
pipeline would do naturally.

Rejected: it contradicts [av-sync.md](../av-sync.md) rule 5 and
`Acquisition::Timeout`'s own documentation — "a backend never invents a frame" —
and it would make an empty recording *look* like a successful one, which is the
opposite of what issue #461 is about. It also costs real bitrate for pictures
nobody presented. The existing behaviour, a faithful gap, is right; what was
missing was saying it happened.

## Consequences

**What becomes easy.** A recording left running while somebody walks away keeps
recording. A `watch` session that starts a game at the end of a long idle
stretch holds the screen for the length of the game rather than for the length
of the idle timer. And a recording that does end up with a hole in it now carries
the size of the hole in its report, its summary sentence and its log, so
"why is my ten-minute recording four frames long?" is answered by the recorder
rather than by ffprobe.

**What becomes hard.** Somebody's monitors do not sleep while Clipped is
recording, and the desktop window asks for a replay buffer on every recording it
starts, so "recording" can mean all day if they leave it running. That is
visible to them through Windows' own `powercfg /requests`, and it is the trade
this record accepts. It also means a machine left recording will not idle-sleep
its displays for burn-in purposes; an OLED owner who leaves an eight-hour
recording running is now paying for that in a way they were not before.

**What is not fixed, and it is worse than issue #461 assumed.** #461 says a dark
stretch leaves a replay buffer that "is empty for as long as the display was
dark". It is not empty. `ReplayBuffer::lease_last` measures its window back from
the newest *picture* rather than from a clock — `crates/replay/src/buffer.rs`,
`plan_lease` and `latest_presentation` — and eviction runs only when a packet
arrives. So after two hours of nothing, "save the last thirty seconds" returns
the thirty seconds from *before* the stall, reports `is_complete()`, and carries
nothing anywhere saying the footage is two hours old. The buffer already has the
defence for the other cause of a gap — `resume_after_any_gap` discards material
across a gap the ceiling created, citing AGENTS.md section 22 — and no equivalent
for packets that never arrived, because it cannot tell "the encoder produced
nothing" from "no time has passed". That is a distinct defect with its own
decision to make, it predates and outlives this one, and it is raised separately.

**What has to be watched.** Two numbers.
`display_held=false` on the `capture started` log line means
`SetThreadExecutionState` was refused and every recording on that machine is one
idle timeout away from recording nothing. A `longest_source_silence` of minutes
with `display_held=true` means either a genuinely still source or a screen that
was already dark when the recording began, and the two are still not
distinguishable from inside the process — which is the part of issue #461 that no
decision closes.

**What this creates.** A settings key when
[issue #423](https://github.com/wildware-uk/clipped/issues/423) lands, per the
third alternative. Nothing else: `CaptureFallback::note_silence` and
`silent_for` already existed and are still not called by the session, because
threading the fallback through the recording loop is
[issue #97](https://github.com/wildware-uk/clipped/issues/97)'s remaining
mid-recording work rather than this one's.
