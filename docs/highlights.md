# Highlights and virtual clips

**Status: the virtual clip model, the highlight rules and generation exist and
are tested; nothing stores or lists a clip yet.**
`crates/library/src/virtual_clip.rs` defines what a clip *is* before anything
has been exported, `crates/session/src/highlights/` decides which moments
deserve one ([#75]), and `crates/session/src/highlights/generate.rs` turns those
moments into the clips themselves ([#76]). What is still missing is either end
of that: nothing delivers a game event to a session and nothing can persist a
clip that has no file ([#269]), so a generated highlight does not yet reach a
user's library. The two capture modes built on the same rules are [#77] and
[#78], and creating a clip by hand from the timeline is [#91]. This document is
the specification all of them are held to.

[#75]: https://github.com/wildware-uk/clipped/issues/75
[#290]: https://github.com/wildware-uk/clipped/issues/290
[#76]: https://github.com/wildware-uk/clipped/issues/76
[#77]: https://github.com/wildware-uk/clipped/issues/77
[#78]: https://github.com/wildware-uk/clipped/issues/78
[#35]: https://github.com/wildware-uk/clipped/issues/35
[#55]: https://github.com/wildware-uk/clipped/issues/55
[#56]: https://github.com/wildware-uk/clipped/issues/56
[#59]: https://github.com/wildware-uk/clipped/issues/59
[#82]: https://github.com/wildware-uk/clipped/issues/82
[#84]: https://github.com/wildware-uk/clipped/issues/84
[#88]: https://github.com/wildware-uk/clipped/issues/88
[#89]: https://github.com/wildware-uk/clipped/issues/89
[#91]: https://github.com/wildware-uk/clipped/issues/91
[#93]: https://github.com/wildware-uk/clipped/issues/93
[#111]: https://github.com/wildware-uk/clipped/issues/111
[#269]: https://github.com/wildware-uk/clipped/issues/269

## What a virtual clip is

A range of a recording that behaves like a clip without a file existing. It can
be offered, listed, titled, tagged and played before anything is exported, and
it costs nothing until the user asks for a file (SPEC.md sections 19, 20 and
44).

That is the whole product argument for automatic highlights. A session produces
twenty interesting moments; nineteen of them will never be watched twice. Any
design that renders them costs disk and encoder time for footage the user
already has, so Clipped stores the *description* and renders on export.

## Is it the same thing as an edit document?

[#82] landed `crates/edit`: a document referencing sources with in and out
points, cuts, overlays and audio levels, distinguishing source time from output
time (`docs/editing.md`). A virtual clip is very close to a one-source edit
document with an in and an out, so this ticket had to answer whether it *is*
one, is a special case of one, or is genuinely different. Building a second
model beside it would be exactly what AGENTS.md section 55 forbids; forcing an
ill-fitting one would be worse.

**The answer: a virtual clip is an edit document plus a reason.**

```text
  VirtualClip
    ├── edit: EditDocument   what plays, and which parts of what
    ├── origin: ClipOrigin   why it exists
    └── tags                 what it is filed under
```

The range half is not a special case of the edit model — it is the edit model.
`EditDocument::from_recording` already means "one recording, one span, nothing
rendered", written for SPEC.md section 20, and every question a virtual clip is
asked about time is one the document already answers: how long is it, what is on
screen at this moment, which part of which recording is that. So nothing about
spans, sources, the two kinds of time, validation or the stored format is
written twice.

What the document deliberately cannot hold is the second field, for two
reasons:

- **Layering.** Provenance is game-event vocabulary — a kill, reported by a
  plugin, at a moment on the recording's timeline. `clipped-edit` and
  `clipped-events` are both at layer 0 of the table in README.md and cannot name
  each other. `clipped-library` is the lowest crate that can see both, so that
  is where the model lives.
- **Meaning.** An export must produce the same file whether the range was
  dragged by hand or generated from a kill. "Why" is not an instruction for
  rendering; it is library metadata, and putting it in the document would put
  something in front of the exporter that the exporter must ignore.

### "Virtual" is not a state of the model

There is no `is_virtual` flag and no conversion from a virtual clip into a real
one. Virtual means *no exported file exists yet*, which is a fact about the
library's row rather than about the clip. Exporting ([#89]) writes a new file
and fills in a path; the document is still what the clip is, so the same clip
can be exported twice at different qualities without becoming two clips. It also
means the editor needs no import step: opening a generated highlight ([#91],
[#84]) opens the document it already had.

### What a saved replay is

The replay buffer writes a file at the moment the hotkey is pressed, because
the packets it is made of are in memory and about to be evicted
(`docs/replay-buffer.md`). So a saved replay is a clip with a file from the
start — and it is still modelled here, as `ClipOrigin::ReplayBuffer`, because
its range and its reason are the same shape as the other two. A library that
modelled saved replays separately would ask every screen, every filter and every
deletion rule to handle two kinds of clip.

## Why a clip exists

`ClipOrigin` is a closed vocabulary of three, not free text, because the library
filters on it — "what did Clipped generate" is a different question from "what
did I save" — and because a generated clip must be traceable to the event that
caused it, which a label cannot do.

| Origin | Produced by | Has a file at creation |
| --- | --- | --- |
| `manual` | the user dragging a range ([#91]) | no |
| `replay-buffer` | the replay hotkey (`docs/replay-buffer.md`) | yes |
| `highlight` | generation from game events ([#76]) | no |

A `highlight` carries a `HighlightCause`: what happened, when it happened on the
recording's timeline, and who reported it. Three fields, and deliberately not
the whole `GameEvent` — the payload can be kilobytes of a plugin's own detail
and would be a second copy of a row that event persistence already owns, and the
confidence is what the rules filtered on before deciding to generate at all.

The moment it carries is the moment the event *describes*, never the moment it
was reported. `clipped_events::EventTiming` keeps those apart precisely because
a plugin observes a game telling it something rather than the thing itself, and
a clip built around the arrival time is a clip built around the wrong second.

## Which moments are worth a clip

The rules live in `crates/session/src/highlights/`, which is where the layer
table in [architecture.md](architecture.md) puts the highlight engine's policy
half: `clipped-events` is the vocabulary, `clipped-session` is what decides
anything about it. They take a list of `GameEvent`s and produce the ranges that
would be worth keeping. The rules themselves create nothing — no clip, no file,
no row, no title; making the clips is `generate.rs`, below — which is why the
whole of this behaviour is tested against constructed event streams with no
game, no GPU and no recording.

A rule set answers three questions per event kind, and two about the set:

| Setting | Per | What it means |
| --- | --- | --- |
| `enabled` | kind | Whether events of this kind are worth a clip at all |
| `lead_seconds` | kind | How much of the recording before the event to keep |
| `trail_seconds` | kind | How much after it |
| `minimum_confidence` | kind | How sure the source has to be that it happened |
| `merge_gap_seconds` | set | How close two windows have to be to become one |
| `maximum_length_seconds` | set | How long merging across a gap may make a clip |

### What Clipped ships

Fifteen seconds before a kill and ten after (SPEC.md section 7); ten and five
for a death, because the interesting part of dying is what led to it. On by
default: kill, death, assist, score, goal, achievement and a win — the things
the player did that they would watch again. Off: everything that is a boundary
rather than a moment — a game, match or round starting or ending — and a loss,
because a lost match is a scoreboard. Each of them still carries a real window,
so switching one on is one change rather than four.

**A plugin's own invention is off by default**, and that is the more important
half of the table. `EventKind::Custom` is how a plugin says something the
vocabulary does not cover ([plugin-api.md](plugin-api.md)), and a plugin that
could put clips in a user's library by inventing a name would be deciding what
that library contains.

An event kind this build has never heard of is off for the same reason — but a
*rule* for one still works. A newer Clipped's `objective_taken` rule reads back
as `EventKind::Unrecognised("objective_taken")` and applies to events carrying
that tag, because both are keyed by the string that arrived. Nothing about that
needed code; it is what the open vocabulary in `clipped-events` is for.

### Merging is the substance

A kill streak is one moment that produced five events. Five clips of the same
twenty seconds is not a feature — it is the library becoming useless silently,
with nothing to fail and nothing to report. So the guarantee is stated as an
invariant and asserted over every scenario the tests construct: **no two
highlights overlap**.

Two rules produce it, and where they disagree the answer is written down rather
than left to the order of the `if`s:

- **Windows that touch always join**, whatever the maximum length says. Two
  clips of the same footage is the failure being prevented, and a user who set a
  short maximum asked for shorter clips, not for duplicates of the same seconds.
- **A gap is bridged only while the result stays inside the maximum.** This is
  where the ceiling bites: a burst that keeps going after a lull gets a second
  clip rather than one that swallows the round.

Nothing is truncated and nothing is dropped. A rule whose own window is longer
than the maximum produces that window and merges nothing into it; truncating it
would be the recorder deciding that the user's fifteen seconds of lead were
really nine (AGENTS.md section 27). Every selected event is a cause of exactly
one highlight, and the causes of each are in the order things happened, which is
not the order their windows opened.

Events the rules do not select take no part: a round ending in the middle of a
firefight neither extends that firefight's clip nor splits it, because a rule
that is off is off.

### Certainty and precision are different questions

`clipped_events::Confidence` is how sure a source is *that* an event happened;
`EventTiming::precision` is how well it knows *when*. They come apart in both
directions — Game State Integration is certain and imprecise, a detector
watching a kill feed is precise and unsure — so **a rule filters on the first
and pads its window with the second**. A source polled every two seconds knows
the moment to within a second and says so; a window built from the nominal time
alone would cut a second before the kill it is a clip of.

A confidence read back from storage may be outside 0 to 1, because
`clipped-events` keeps what is already in a user's library verbatim rather than
destroying the event over it. There is no honest comparison to make against such
a value, so the event is skipped and the reason says so, rather than a number
being invented for it.

### Per-game rules, and where they are not yet

Resolution is `crate::config`'s three-layer fold, used rather than reimplemented
(AGENTS.md sections 30 and 55): the shipped defaults, then the global rules,
then one game's, each layer overriding only what it mentions. Inheritance is per
*field* and not per rule, so a game that wants five more seconds after a kill
says only that and keeps following the global lead. Every answer carries which
layer supplied it, which is what a settings screen's Reset control needs.

**What does not exist yet is the section in the file.** `HighlightRules::read`
and `HighlightRules::write` are the format — `merge_gap_seconds`,
`maximum_length_seconds` and an `events` object keyed by event kind — and
`config::Configuration` does not hold it, so today every caller resolves the
shipped defaults and a user cannot change a rule by editing `settings.json`.
That wiring is [#290], and it was kept out of this change because it is entirely
inside the configuration module and is a change to the settings file format.
Until it lands, a build with the rules and a build without can still exchange a
settings file: the older one keeps the whole `highlights` section among the
top-level keys it does not recognise and writes it back.

The migration path for the section itself is that there is nothing to migrate
from. No build has written it, so the only older shape is its absence, and
absence means every rule is inherited — which is exactly what an unconfigured
user gets. It carries no version of its own, because the settings file has one.

## Generating a session's highlights

`clipped_session::highlights::HighlightGeneration` ([#76]) is the end of the
detection chain: the step that turns the merged moments into clips. For each
moment the rules chose it produces exactly this and writes nothing:

```rust
let window = window_around(highlight.start(), Duration::ZERO, highlight.duration(), &recorded)?;
let clip = VirtualClip::of_range(title, recording, window, ClipOrigin::Highlight(cause))
    .with_tag(event.kind().as_str());
```

It takes three things and asks the library for nothing: the rules resolved for
the game being recorded, the recordings the session has **finished**, and the
clips the library already holds for it. Everything below follows from those
three.

### It writes no file, and automatic generation never will

This is the decision the ticket turns on, and the reason is not the encoder
time. A virtual clip costs zero bytes, so a session's twenty interesting moments
cost twenty rows of metadata rather than twenty renders of footage the user
already has. Writing them out would also do two things nothing asked for:

- **It spends the user's disk.** The storage quota ([#93]) is the budget they
  set for the footage they chose to keep, and automatic cleanup ([#111]) deletes
  the oldest unprotected recordings once that budget is reached. A recorder that
  wrote a gigabyte of highlights after every session would fill the budget with
  copies and then delete the originals to make room for them.
- **It renders what nobody will watch.** Nineteen of the twenty will never be
  opened twice. Rendering is [#89]'s, at the moment somebody asks for a file and
  at the quality they ask for; a generated highlight is already an edit
  document, so exporting one needs no import step.

`crates/session/tests/generating_highlights_writes_nothing.rs` holds it to that
three ways: it scans `generate.rs` for any means of opening a file or waiting on
anything, compares a directory byte for byte before and after a session's clips
are generated in it, and measures the cost.

### Which source a clip comes from

**A file the session finished writing, and never the replay buffer.**

A highlight is detected while the game is being played, so the material may
still be in the rolling buffer ([#35]) rather than on disk. Taking it from there
is not a cheaper version of this: the packets are in memory and about to be
evicted, so keeping one means *writing a file at that moment*
([replay-buffer.md](replay-buffer.md)) — which is the thing generation
deliberately does not do. That save is a capture mode rather than generation:
Highlights Only ([#77]) is the ticket whose whole purpose is that the buffer is
all there is.

So a moment no finished file of the session covers produces no clip, and the
reason says which of the five cases it was. **And that is also the answer to
"what if the buffer has already evicted it":** nothing is generated. By the time
anything could ask, the memory has been overwritten; there is no file to point
at, and pointing at one that does not contain the kill it claims would be a
marker the user cannot check (AGENTS.md section 27). A session running the
buffer alone reports `NothingRecorded` for every moment it heard.

A merged window can reach past the end of the file it is cut from — into the gap
before the session's next recording. It is clamped to that file rather than
split across two, because a clip drawing on several recordings is [#88]; the
events outside it are still recorded as its causes. The file chosen is the one
holding the moment the clip is *named* after, which is the earliest event in it.

### When it runs, and why nothing can stall the recording

After a recording has been finished, on whatever thread the caller likes, and
never on the one that is capturing (AGENTS.md section 20). That is not a
convention that could erode: the input is a list of `RecordedSegment`s, and a
segment is a file *and the span it covers*, which is only known once the file
has been closed. A session halfway through its second recording therefore
generates the first one's highlights now and the second one's when that file
ends — which is what "during or after a session" means here.

Generation opens nothing, takes no lock and waits on nothing, so it cannot cost
a recording a frame even if a caller runs it on the wrong thread. The test above
asserts that against the module's source rather than against this paragraph.

### Titles and tags

A title is made of what the events say and nothing more: which kinds happened,
how many of each, and where in the file the clip starts.

```text
Kill ×3 at 20:05
Kill ×2, assist at 0:45
Objective taken, flag captured at 0:45
```

The kinds are in the order things happened, so the first one named is the moment
the clip opens on. A plugin's own name is namespaced — `acme-cs2.flag_captured`
— and the namespace is how the vocabulary stays collision-free rather than
something to show somebody, so a title uses the part after it and an underscore
reads as a space. Nothing is inferred beyond counting: three kills close
together is `Kill ×3` and not `Triple kill`, because Clipped does not know that
game's word for it and inventing one would put a claim in the user's library
that nothing checked. A title is capped at 96 characters, because an event kind
read back from storage is whatever text was stored.

The **tags** are the wire spelling of every distinct kind in the clip —
`kill`, `acme-cs2.flag_captured` — because a tag is what a search filters on
rather than what a person reads, and a clip of three kills is tagged `kill`
once.

### Re-running it produces nothing

Generation is idempotent against the clips a session already has: hand back what
it produced last time and it produces nothing new, however many times it runs.
Two rules do it, and both are needed.

- **An event is clipped once.** A moment one of whose causes is already the
  reason a generated clip exists is `AlreadyGenerated`. This is what covers an
  event that arrives late and joins a firefight that already has a clip.
- **Generated clips of a recording never overlap.** When the rules have changed
  between runs and the same events now merge differently, a range covering
  seconds an existing generated clip covers is `OverlapsAnExistingClip`. This is
  the merge's own invariant — no two highlights overlap — extended across runs,
  and it is what stops "regenerate" from being the way a library fills with
  near-identical clips.

Neither rule deletes or rewrites anything. A clip the user already has is
theirs, and regenerating after changing a rule leaves it alone (AGENTS.md
section 56); getting the new window means deleting the old clip first. A clip
the user made **by hand** takes no part in either rule: it is not a generated
clip, and the library filters on `ClipOrigin` for exactly this kind of question.

### What is not generated is reported

Every moment the rules chose either becomes a clip or appears in `withheld()`
with the event it would have been named after and the reason there is no clip.
A caller can therefore say "four of these five kills are on this file and the
fifth happened before the recording started" rather than quietly producing four
(AGENTS.md sections 15 and 27). Events the *rules* did not select never reach
generation at all — `ResolvedHighlightRules::decision_for` is what says why for
one of those, and it says it in four different ways.

### What generation costs

Measured on the maintainer's machine, debug build
(`cargo test -p clipped-session --test generating_highlights_writes_nothing --
--nocapture`), over a busy three-hour session — 720 events in bursts of four a
minute apart, which the merge turns into 180 clips:

| | |
| --- | --- |
| Generating 180 clips from 720 events | 2.4 ms, **13 µs each** |
| The same run again, against those 180 clips | 0.3 ms |
| Bytes of media written | **0** |
| The 180 clip documents, as text | 101 kB, ~560 bytes each |

The re-run is the more interesting figure, because it is the path that checks
every moment against every clip the library holds.

### Where the conversion lives

`window_around` is the one place an event's time becomes an edit's time. They
are the same quantity against the same zero — nanoseconds from the recording's
epoch, which is the timestamp of its first kept video frame — but they are types
in two layer 0 crates that cannot name each other, so the conversion is written
once, here, with tests, rather than at each call site. It is the fourth copy
[issue #253](https://github.com/wildware-uk/clipped/issues/253) warns about
being avoided.

Two things it does that a subtraction at the call site gets wrong:

- **It measures from the file, not from the session.** For a whole recording
  those are the same number. For a saved replay they are not: the file's first
  packet is a keyframe some way down the session's timeline, so a kill twenty
  minutes into the session is ten seconds into that file. It takes a
  `clipped_events::RecordedSpan` and asks it, rather than subtracting.
- **It clamps rather than failing.** A kill four seconds into a recording with a
  fifteen-second lead still deserves a clip; it just starts at the beginning.
  `None` means the window and the file do not overlap at all — the recording
  does not cover the moment, so there is nothing to offer, and inventing one
  would be a marker the user cannot check (AGENTS.md section 27).

It is arithmetic, not policy. Which events are worth a clip, how much lead and
trail each kind gets, and how a burst of them is merged into one clip are the
rules above, which produce the `lead` and `trail` passed in — as
`ResolvedHighlightRules::highlights`, whose `Highlight` carries the merged range
and the events that caused it. Generation's loop is therefore over highlights
rather than over events, and its `cause` comes from
`HighlightCause::of(highlight.primary())`; the window it passes is the merged
one, as a lead of nothing and a trail of the whole highlight, because both ends
have already been decided.

## What a clip costs

Measured on the maintainer's machine, debug build
(`cargo test -p clipped-library -- --nocapture`):

| | |
| --- | --- |
| Ten thousand highlights over a three-hour recording | 9.1 ms, **0.9 µs each** |
| Bytes of media written | **0** |
| One clip, as the text it is stored as | ~570 bytes |

A clip of two seconds and a clip of three hours are the same handful of fields;
the only difference between their documents is the extra digits of the end time.
`crates/library/tests/a_virtual_clip_costs_nothing.rs` asserts all three: the
measurement, a byte-for-byte comparison of a directory before and after a
thousand clips are made in it, and a scan of the module's own source for any
means of opening a file at all.

For storage accounting ([#93]) a virtual clip contributes **zero bytes**. The
bytes belong to the recording it points at, counted once, there.

## When the source recording goes away

A virtual clip has no file of its own, so it cannot outlive its source the way a
saved replay can: deleting the recording deletes the only copy of the material.
The behaviour, which `SourceDeletion` states as a type rather than as prose:

- **Automatic cleanup never deletes a referenced recording.** The storage
  manager ([#111]) skips it exactly as it skips a favourite. This is what "clips
  count towards the storage protection of their source" means: not that the
  clip's own bytes are counted — it has none — but that its existence protects
  the recording's.
- **A person may still delete it, having been told what it costs.** Refusing
  outright would leave a user unable to reclaim their own disk. The deletion is
  confirmed rather than blocked, with the number of clips that will stop playing
  stated first (AGENTS.md section 45).
- **The clips are kept either way.** The recording goes to the trash (SPEC.md
  section 28), its clips become `ClipState::SourceInTrash`, and restoring the
  recording restores them. Nothing is deleted on the user's behalf (AGENTS.md
  section 56).
- **A missing file is a marked clip, not an error.** A recording the user moved
  or deleted outside Clipped leaves its clips `ClipState::SourceMissing`: listed,
  marked and unplayable until the file comes back. The library is a view over
  what is actually on disk, and a clip that vanished because a drive was
  unplugged would be Clipped destroying user data.

A clip that draws on more than one recording ([#88]) takes the worst answer of
its sources, because a clip that needs two recordings and has one does not play.

## Persistence, which does not exist yet

**A virtual clip cannot currently be stored, and this ticket did not add the
ability.** The `clips` table from [#55] requires `path TEXT NOT NULL UNIQUE`,
which is precisely the column a virtual clip does not have, and it holds no
edit document and no origin. Storing one therefore needs a migration, and that
migration is [#269] rather than part of this change: `docs/storage.md`'s rule is
that a table that is wrong is worse than a table that is missing, and columns
that nothing writes and nothing reads would be a guess at a shape two open
issues are still deciding.

The shape [#269] should add, recorded here so the model and the schema are
argued in one place:

- `path` becomes nullable — a clip with no file is the normal case, and a file
  is what an export adds.
- an `edit` column holding `EditDocument::write`'s text, which `clipped-storage`
  keeps without understanding, exactly as it keeps settings JSON.
- `origin` and `origin_detail`, mirroring the `(kind, detail)` pair
  `session_events` already uses. `ClipOrigin` serialises as
  `{"origin":"highlight","kind":"kill","at":600000000000,"source":"acme-cs2"}`,
  so the tag is the column and the rest is the detail.
- `source_recording_id` stays, because a virtual clip's dependency on a
  recording has to be a foreign key the database can answer questions about —
  "what depends on this recording" is asked before every deletion, and scanning
  every clip's document to answer it would not scale.

Until that lands, a clip does not survive a restart and does not appear in the
library or in search; the library index itself is [#56] and search is [#59],
both M6 and both open.

## Where the events themselves live

A `HighlightCause` is three fields and deliberately not the whole `GameEvent`,
"because the payload … would be a second copy of a row that event persistence
already owns". That row is [#71]'s, and this section is what it is owed —
recorded here, beside the model that depends on it, for the same reason the
`clips` columns above are.

### Placing an event in a file

`crates/library/src/events.rs` is the conversion a timeline needs, and the
companion of `window_around`: that one turns a moment into a *range* for a clip
to be cut from, this one turns it into a *point* for a timeline to draw. A
session is a list of `RecordedSegment`s — a recording, and the
`clipped_events::RecordedSpan` its file covers — and a moment is either inside
exactly one of them or inside none:

```text
  session timeline   ├──────────────────────────────────────────────┤
  recordings              ├── #1 ───┤        ├───── #2 ─────┤
  events               ✕      ✓          ✕          ✓             ✕
```

All five answers are ordinary rather than errors, and they are told apart
rather than reduced to a boolean, because they are five different things to
tell somebody:

| Where the moment is | What `place` answers |
| --- | --- |
| Inside a recording | `In { recording, at }`, measured **from that file** |
| Before the first | `BeforeTheFirstRecording` — the recorder started after the game |
| In a gap | `BetweenRecordings` — a window destroyed and recreated |
| After the last | `AfterTheLastRecording` |
| No file at all | `NothingRecorded` — see below |

Two rules hold it to AGENTS.md section 27. **Nothing is dropped**: `marks`
answers with every event it was given, in the order things happened, each
carrying where it belongs or why it belongs nowhere, so a caller can say "four
of these five are on this file and the fifth happened before it started" rather
than quietly showing four. **Nothing is invented**: a moment no file covers is
never pinned to the nearest frame, which is why `RecordedSpan::position_of`
answers `None` rather than clamping.

Placement never reads an event's kind. That is the property that makes a kind
this build has never met — `EventKind::Unrecognised`, or a plugin's namespaced
`Custom` — a mark on the timeline exactly like a kill, rather than one that
vanishes on an older build.

### A replay-buffer-only session

A session with the buffer running and nothing being written to disk has no
segments, so every event it hears places as `NothingRecorded`. The events are
still the session's — they are what a saved replay is offered *for* — and the
moment the hotkey writes a clip, that clip is a segment whose span starts at
the keyframe the buffer began with, and the events inside it place in it,
rebased onto the clip. Nothing about the model changes between the two cases;
the list of segments does.

### The table, which does not exist yet

**Nothing stores a game event.** `session_events` is not it and says so: the
`0001` migration reserves that table for the session's own vocabulary and
records the exclusion, `docs/storage.md` repeats it under "What is deliberately
absent", and three things make writing game events there wrong rather than
merely untidy — its `at` is RFC 3339 text where an `EventTime` is signed
nanoseconds on the media timeline, it has no `recording_id`, and
`clipped_library::index::ingest` rewrites every one of a session's rows on each
reconciliation.

So M9 owes a migration, and the shape it should add is recorded here so that
the model and the schema are argued in one place:

```sql
CREATE TABLE game_events (
    game_event_id INTEGER PRIMARY KEY,
    session_id    TEXT NOT NULL REFERENCES sessions (session_id) ON DELETE CASCADE,
    recording_id  INTEGER REFERENCES recordings (recording_id) ON DELETE SET NULL,
    at_nanos      INTEGER NOT NULL,
    kind          TEXT NOT NULL,
    source        TEXT NOT NULL,
    document      TEXT NOT NULL,
    CHECK (kind <> '')
) STRICT;
```

- **`document` is the authority**, and holds the whole `StoredEvent` JSON. That
  is what makes [#68]'s forward compatibility survive storage: a field a newer
  build added is inside it, `ReadEvent::to_json` puts it back, and an older
  build that re-indexes a library does not delete what it could not name
  (AGENTS.md section 56). The other columns are indexes into that text rather
  than a second copy of the model; spreading the envelope across columns would
  lose everything that did not fit one.
- **`kind` carries no `CHECK`.** The vocabulary is open by design, and a
  constraint here would refuse exactly the events that must still be drawn.
- **`recording_id` is nullable**, because an event a replay-buffer-only session
  heard belongs to the session and to no file, and `ON DELETE SET NULL` keeps
  it when the recording goes.

Until that migration lands, nothing writes a game event and nothing reads one
back: `crates/library/src/events.rs` places events it is *handed*. Drawing them
is a second gap and a separate one — the desktop window can neither read the
library nor ask the recorder for a row of it ([#329], [#301]) — so the editor's
event lane says "nobody asked" rather than "there were none".

[#68]: https://github.com/wildware-uk/clipped/issues/68
[#71]: https://github.com/wildware-uk/clipped/issues/71
[#301]: https://github.com/wildware-uk/clipped/issues/301
[#329]: https://github.com/wildware-uk/clipped/issues/329
