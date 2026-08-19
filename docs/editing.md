# Editing

**Status: the document model, the operations that cut a clip up and the mix
exist and are tested, the editor screen draws a document it is given, and the
desktop application can now open a clip and save an edit back over the control
protocol — `library_clip_document` and `save_clip_document`
([#306](https://github.com/wildware-uk/clipped/issues/306),
[ipc.md](ipc.md)). What is still missing is a control per operation, and
nothing renders a clip to a file.** `crates/edit` defines what an edit *is*,
reads and writes it, converts an older one, answers the question an exporter
asks — "what is on screen at this moment, where does it come from, and how loud
is each track here?" — and performs the seven edits a user can make: trim start,
trim end, split and delete section
([#84](https://github.com/wildware-uk/clipped/issues/84)), and a level, a mute
and a pair of fades on any audio track
([#85](https://github.com/wildware-uk/clipped/issues/85)), all with undo and
redo. The editor's shell is [issue
#83](https://github.com/wildware-uk/clipped/issues/83) and is described in
[desktop-ui.md](desktop-ui.md#the-editor-screen), the remaining operations are
[#86](https://github.com/wildware-uk/clipped/issues/86) to
[#88](https://github.com/wildware-uk/clipped/issues/88), and the export engine
is [#89](https://github.com/wildware-uk/clipped/issues/89). This document is the
specification all of them are held to.

## The rule everything else follows from

A recording is irreplaceable. The game is over, the round will not happen again,
and no amount of engineering gets it back. So editing in Clipped never touches
one:

> Making, changing, exporting or deleting a clip does not modify, move,
> truncate or re-encode the recording it refers to.

That is AGENTS.md sections 56 and 57 and SPEC.md section 2, and it is not
implemented as care taken at each call site. It is implemented as an inability:
`crates/edit` performs **no file access at all**. It hands the caller a `String`
and takes one back, and `crates/edit/tests/sources_are_never_touched.rs`
asserts both that a checksummed file survives everything the crate can be asked
to do, and that the crate's own source contains no way to open a file in the
first place.

An export writes a new file. A deleted clip deletes a row. Neither has anything
to say about the recording.

## Two kinds of time

The moment an edit contains a cut or a speed change, "three seconds in" means
two different things. Getting this wrong is how an editor ends up with audio a
few frames adrift of its picture, so the model has two types and they are not
interchangeable:

| | Measured from | Used for |
| --- | --- | --- |
| **Source time** | The first frame of one recording | Which part of a recording a segment plays |
| **Output time** | The start of the clip | The playhead, overlay timing, fade lengths, the exported file's own timeline |

Both count **nanoseconds**, which is what the recorder already writes:
`crates/encoder` stamps packets on a `1/1_000_000_000` time base and
`crates/muxer` rescales them into the container. A source time therefore needs
no conversion to be looked up in the file. Nanoseconds also stay exact in the
editor, which is JavaScript and holds integers exactly only below 2^53 — a
hundred and four days of nanoseconds, against recordings that last hours.

Times are not frame indices. A frame index assumes a constant frame rate, which
a recording made while a game was dropping frames does not have, and it means
nothing at all once one clip refers to two recordings at different rates. The
document stores time; the exporter snaps to frames.

Every range in the document is **half-open**, `[start, end)`. The segment
ending at twelve seconds and the segment starting at twelve seconds do not both
claim the frame there.

## What a document looks like

```json
{
  "schema_version": 2,
  "title": "Round 12 ace",
  "aspect_ratio": { "width": 16, "height": 9 },
  "sources": [
    { "id": 0, "recording": "rec-2026-08-11-cs2" },
    { "id": 1, "recording": "rec-2026-08-11-cs2-b" }
  ],
  "segments": [
    {
      "source": 0,
      "span": { "start": 30000000000, "end": 38000000000 },
      "speed": { "numerator": 1, "denominator": 1 },
      "crop": null,
      "rotation": "none"
    },
    {
      "source": 0,
      "span": { "start": 92000000000, "end": 104000000000 },
      "speed": { "numerator": 2, "denominator": 1 },
      "crop": null,
      "rotation": "none"
    }
  ],
  "audio_tracks": [
    {
      "name": "Game",
      "inputs": [{ "source": 0, "stream": 0 }, { "source": 1, "stream": 0 }],
      "gain_db": -3.0,
      "muted": false,
      "fade_in": 0,
      "fade_out": 1000000000
    }
  ],
  "overlays": [
    {
      "text": "Round 12",
      "when": { "start": 0, "end": 3000000000 },
      "position": { "x": 0.5, "y": 0.85 },
      "height_percent": 7
    }
  ]
}
```

Nothing in it is a path. A recording is named by the library's identifier for
it, and `crates/library` is what reconciles identifiers against what is actually
on disk — users move folders, change drive letters and restore from backups, and
a clip that stops opening because a recording moved would be a clip Clipped
broke.

## The timeline

A document is a **list of segments**, laid end to end in the order they play.
Each names a source, a span of that recording, and how it is presented.

```text
  source A  ├────────────────────────────────────────────────┤
                 ╰── 30s–38s ──╯        ╰── 92s–104s ──╯
  source B  ├──────────────────────────────┤
                   ╰─ 5s–9s ─╯

  output    ├── segment 0 ──┤── segment 1 ──┤─ segment 2 ─┤
            0s              8s             14s           18s
```

That is the document above with a third segment appended from the second
recording. Segment 1 is twelve seconds of material at double speed, which is why
it occupies six seconds of the output and not twelve.

There are no gaps: a gap is either black frames nobody asked for or a second way
of writing the same edit.

### A cut is stored as its result, not as an instruction

AGENTS.md section 57 lists `cuts` as one of the things an edit is made of, and
the obvious shape — a source plus a list of removed ranges — was considered and
rejected.

It is order-dependent. "Remove 10s–20s, then remove 15s–25s" means one thing if
the second range is measured against the original recording and another if it is
measured against the result of the first removal, and every reader has to guess
the same way as every writer or the export does not match the preview. And it
assumes a single source, which [#88](https://github.com/wildware-uk/clipped/issues/88)
does not have.

So deleting a section turns one segment into two, and the cut *is* the boundary
between them. Reading the document is arithmetic rather than replay, and the
answer cannot depend on the order the user did things in.

### Reading it

`EditDocument::locate(OutputTime)` is the whole interface, and it is what both
the preview and the exporter must use, so that they cannot disagree:

```text
  segment_start[k] = Σ output_nanos(segment[i]) for i < k

  output_nanos(segment)  = span_nanos × denominator ÷ numerator
  source_time(at)        = span.start + (at − segment_start) × numerator ÷ denominator
```

All of it is integer arithmetic in 128 bits, truncating. Speed is an exact ratio
of two integers rather than a float for the same reason: `0.1` is not a tenth in
binary, and a preview and an export that round it at different moments drift
apart over a long clip. Two integers divide the same way on every machine, for
ever.

Truncation can leave a segment's last fraction of a nanosecond unaccounted for.
That is deliberate — rounding up produces a boundary one nanosecond past the end
of the material it came from — and it is far below the frame the exporter will
snap to anyway.

## Editing it

Four operations change what material a clip plays, and every one of them is
about the distinction above: the user points at a moment of **output** time, and
what changes is which **source** material the document names and where it lands
in the output afterwards. Three more change how the clip *sounds*, and are
described under [changing the mix](#changing-the-mix); they move nothing at all.

| | What it does | Source time | Output time |
| --- | --- | --- | --- |
| **Trim start** | Drops everything before the playhead | Unchanged | Everything kept moves earlier by the trim |
| **Trim end** | Drops everything from the playhead on | Unchanged | Unchanged |
| **Split** | Turns one segment into two | Unchanged | Unchanged |
| **Delete section** | Removes a selected range and joins what is left | Unchanged | Everything after the cut moves earlier by the length of the cut |
| **A change to the mix** | Sets a level, a mute or a fade on one track | Unchanged | Unchanged |

Deleting eight seconds out of the middle does not move a frame in any recording.
It moves every frame after the cut eight seconds earlier in the export, while
they stay exactly where they were in the file they come from. That row of the
table is the one an export bug is usually really about.

### All four are one piece of arithmetic

Put a boundary at an output time — splitting the segment that covers it, in
source time, at its own speed — and then keep some of what is either side:

```text
  before   ├──── segment 0 ────┼──── segment 1 ────┤
                      ▲ at
  divide   ├── 0a ────┼─ 0b ───┼──── segment 1 ────┤

  split         keep everything             →  0a 0b 1
  trim start    keep from the boundary      →  0b 1
  trim end      keep up to the boundary     →  0a
  delete        divide twice, drop the middle
```

There is one such division in the crate, and it is the same
`Speed::source_nanos` the exporter's `locate` uses, so a cut lands exactly where
the frame the user is looking at comes from.

A boundary that already exists is **found, not inserted**. Splitting where a
segment already begins returns the document unchanged rather than adding an
empty piece, which is what makes the operations order-independent: two splits in
either order, a split and a deletion of an earlier range in either order, and
two deletions in either order all produce the same document. That property is
the whole reason a cut is [stored as its result](#a-cut-is-stored-as-its-result-not-as-an-instruction),
and it is tested rather than asserted.

### Frames, and what happens at a keyframe

A boundary in the document is a time, and the arithmetic that produces it is
exact: the cut a user asked for at 12.5 seconds is stored as the source
nanosecond that output moment comes from, with **no tolerance at all**. The one
place a fraction is lost is the truncating division a non-integer speed does,
which is under a nanosecond — five orders of magnitude below a frame at 60fps.

Frames arrive at export. A cut almost never lands exactly on a frame boundary,
and never on a keyframe, so
[#89](https://github.com/wildware-uk/clipped/issues/89) has to decide what to do
about both. The rules it is held to are:

- **Snap outwards from the document, never inwards.** Ranges are half-open, so a
  segment's first exported frame is the first whose presentation time is at or
  after `span.start`, and its last is the last strictly before `span.end`. A
  frame therefore belongs to exactly one side of a cut: none is duplicated at a
  join and none is dropped.
- **A keyframe is a re-encode decision, not a timing one.** Stream-copying a
  segment can only begin at a keyframe, so a copy would have to move the cut back
  to the previous one — up to a whole group of pictures of material the user
  deleted. That is a visible difference from the preview, so it is not something
  an exporter may do quietly: the boundary in the document is what the output
  must show, and a segment whose start is not a keyframe is re-encoded from the
  cut. `Segment::is_untransformed` is the model's half of that question and
  answers only about the segment's own transformations; whether a copy is
  possible at all needs the file, and is #89's to answer.

The frame tolerance an *export* is measured against therefore belongs in that
ticket, with a measurement behind it. Nothing in this build writes a file, so
there is nothing here to measure and no number is claimed.

### What happens to everything else in the document

- **Overlays** are timed in output time, so both ends of an overlay's range go
  through the same mapping the material did. One that was only ever over deleted
  or trimmed material disappears with it; one that straddles a cut keeps the part
  that survived, and one that spans a deleted section is shortened by the length
  of the section so that it still covers the same frames either side of the join.
- **Fades** are lengths at the ends of the clip, so a clip that got shorter can
  be left with fades that no longer fit, which validation refuses. They are
  shortened rather than the edit being refused — a user who has faded a clip must
  still be able to trim it. The fade *in* is kept in preference to the fade out.
  A cut that leaves them fitting does not change them at all, which means the
  material under a fade moves while the fade does not: [see
  above](#fades).
- **Levels and mutes** are untouched by every operation. They belong to a track
  of the output and not to any stretch of material, so there is nothing for a
  cut to do to them.
- **Sources stay declared** even when nothing plays them any more. An unused
  source breaks nothing, dropping one would silently break any audio track fed
  from it, and undo has to be able to put the material back.
- **Segments keep their presentation.** Both halves of a split carry the speed,
  crop and rotation the segment had.

### What is refused

An operation returns either a new document or a refusal; the original is never
modified, so there is no half-applied state. Refused are: a time past the end of
the clip; a trim that would keep nothing, because trimming says which range is
*kept* and a kept range that ends where it starts is not a range; a change to
the mix of a track the clip does not have; and — as a last line of defence
rather than an expected outcome — any result that would not validate, since an
operation checks its own output before returning it. That last one is how a
level outside -60 dB to +12 dB and a pair of fades longer than the clip are
refused: there is one statement of what a document may say, and the operations
are held to it rather than repeating it.

Deleting *all* of a clip is allowed, and leaves the valid empty document
described under [what a document may not say](#what-a-document-may-not-say): a
user who selected everything and pressed delete has an empty clip, and undo is
one keystroke away.

### Undo and redo

`EditHistory` holds the document and the states it has been in, up to
`MAX_UNDO_STEPS` of them, oldest dropped first. Whole documents rather than
inverse operations: an inverse has to reproduce what its operation destroyed —
the overlay a deletion dropped, the fade a trim shortened — which is a second
implementation of every operation that has to stay in step with the first, and
the failure when it does not is a user pressing Ctrl+Z and getting *nearly*
their clip back. A document is a few hundred bytes; "restore exact prior state"
is worth more than the copy.

An operation with nothing to do — splitting where a boundary already is —
records no step, so undo never restores an identical document.

## Combining recordings

A document holds a *list* of sources, from the first version. Making that
decision now is the difference between
[#88](https://github.com/wildware-uk/clipped/issues/88) being a feature and
being a rewrite of everything built on top of the model; a single-recording clip
is simply a document with one entry.

What the model does **not** do is normalise. Resolutions, frame rates and audio
layouts that differ between two recordings are reconciled at export, by
[#89](https://github.com/wildware-uk/clipped/issues/89) against the rules
[#88](https://github.com/wildware-uk/clipped/issues/88) documents, using the
files themselves. The document deliberately holds no copy of a recording's
resolution or duration: a copy is a second answer that goes stale.

## Audio

An audio track in a document is a track of the **output**, and it lists which
stream of which recording feeds it:

```text
  "Game"       ← source 0 stream 0, source 1 stream 0
  "Microphone" ← source 0 stream 1
  "Discord"    ← source 0 stream 2
```

That indirection is what makes [#88](https://github.com/wildware-uk/clipped/issues/88)
work. Two sessions recorded on different days may carry Discord on different
stream indices, and joining them has to put both under one slider rather than
under two. It also means the model never guesses a track's meaning from its
name, which would be wrong for exactly the users who route their own
applications to their own tracks (SPEC.md section 12).

**Levels** are decibels, `0.0` meaning "as recorded", between -60 dB and +12 dB.
Decibels are what the user drags and what the document stores; what a renderer
needs is an amplitude multiplier, and the one conversion — `10^(dB/20)`, so -6 dB
is very nearly half — lives in `TrackOutput::amplitude` so that a preview and an
export cannot each round their own way.

### Solo is not a property of a track

A **level**, a **mute** and a **fade** describe the clip: they are what the
exported file must carry, so they are fields of the track, they are saved, and
they reach the export. **Solo is none of those things.** Soloing is how somebody
listens to part of their own mix while they work, and it is undone by clicking
it again ten seconds later.

So a solo is `Solo` — a value the editor holds *beside* the document — and not a
field of `AudioTrack`. Format version 1 stored `soloed` on each track, and
version 2 [drops it](#compatibility). Two things were wrong with it:

- **It could be exported.** A clip saved while the user was listening to Discord
  on its own is a clip whose document says every other track is silent. The
  export would honour that, and the user would get a file of one track and no
  game audio, for a button they pressed while editing.
- **"Two tracks are soloed" was expressible and undefined.** Any number of
  tracks could carry the flag, and each reader had to invent a rule for what
  several of them meant. A `Solo` names **at most one track**: pressing solo on
  another track moves it, pressing it again clears it, and there is no ambiguous
  state to arbitrate.

The consequence, stated plainly because it is a deliberate loss: **a solo does
not survive closing the clip.** Reopening it plays the whole mix. That is the
right trade against exporting a clip with most of its audio missing.

### Mute and solo together

| | Nothing soloed | This track soloed | Another track soloed |
| --- | --- | --- | --- |
| **Not muted** | heard | heard | silent in the preview, heard in the export |
| **Muted** | silent | silent | silent |

- **Mute wins**, including over a solo on the same track. Soloing a muted track
  does not unmute it: solo is a way of listening to part of a mix, not a second
  mute button with the opposite sense, and a control that quietly undoes another
  control is the surprise AGENTS.md section 27 is about.
- **Solo is exclusive**, and only in the preview. While a track is soloed every
  other track is silent *to the person listening*.
- **Solo does nothing when nothing is soloed**, so the ordinary case is just
  mute and level, and the preview and the export agree exactly.

Two methods, and the difference between them is the whole point:
`EditDocument::track_output(index)` is what an **export** reads and is never
given a solo; `EditDocument::monitored_output(index, solo)` is what the
**preview** reads. Both answer `Silent` or `Audible { gain_db }` — a type rather
than an `f64`, so that "silent" cannot be misread as "no gain applied", which is
the mistake that exports a muted microphone at full volume.

### Fades

A fade is a length at the start and a length at the end of the clip, in **output
time**. The curve is defined here so that preview and export cannot differ: the
multiplier rises **linearly in amplitude** from zero across `fade_in`, and falls
linearly to zero across `fade_out`.

```text
       ╱▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔╲
      ╱                     ╲
  0s ╯  fade_in     fade_out ╰ clip
```

`EditDocument::track_amplitude_at(index, at)` is that curve multiplied by the
track's level and its mute: one number, between `0.0` and `1.0` at a normal
level, for one track at one moment. It is the audio half of `locate`, and for
the same reason — an exporter and a preview that computed it separately would
disagree somewhere.

Three consequences worth stating, because each of them is a question somebody
will ask of an export that sounds wrong:

- **Fades are anchored to the ends of the clip, not to the material.** Material
  moves in output time every time a section is deleted, so a fade pinned to a
  frame would wander off the front of the clip the first time the user trimmed
  it. Deleting three seconds out of the middle of a clip with a six-second fade
  in therefore leaves the fade six seconds long, and the material that used to
  be past the end of it is now inside it and quieter than it was.
- **Fades may not overlap**, which is the same rule as "they may not add up to
  more than the clip lasts": a passage that is fading in and out at once is not
  something a user asked for. Where a document says it anyway — one that has
  never been validated — the two envelopes multiply, so the answer stays between
  zero and one rather than becoming a multiplier greater than the level.
- **A shorter clip shortens the fades**, and only when it has to. Trimming a
  clip whose fades no longer fit shortens them rather than refusing the trim,
  keeping the fade *in* in preference to the fade out. Setting a fade that does
  not fit is refused instead — the difference is that a trim is a user asking
  for something else and finding a fade in the way, while a fade that does not
  fit is the user asking for the impossible thing directly.

### Changing the mix

Lowering Discord is an **operation**, like trimming:

| | What it does |
| --- | --- |
| **Set track gain** | Plays one track at a different level |
| **Set track muted** | Silences one track, or lets it be heard again |
| **Set track fades** | Sets how long that track fades in and out for |

They go through `EditDocument::apply` and `EditHistory` for two reasons, both of
them a user pressing Ctrl+Z: undoing a level has to restore the exact level that
was there, and a level outside -60 dB to +12 dB, or a pair of fades longer than
the clip, is refused *when the slider is dragged* rather than when the user
saves. A mix change moves no material, so nothing on the timeline is remapped.

Soloing is deliberately not an operation. It changes no document, so there would
be nothing for undo to restore.

### What a mix costs an export

The model decides what [#89](https://github.com/wildware-uk/clipped/issues/89)
can copy and what it must produce sample by sample. `AudioTrack::is_unmixed` is
the model's half of the question — it answers about the mix only, exactly as
`Segment::is_untransformed` answers about a segment's own transformations —
and the rules are:

| The track | Can the recorded stream be copied? |
| --- | --- |
| One input, `0.0` dB, not muted, no fades | **Yes.** Nothing is done to a sample |
| A level other than `0.0` dB | **No.** Every sample is scaled, so the track is decoded, mixed and re-encoded |
| A fade in or out | **No.** Every sample the fade covers is scaled by a different amount |
| Muted | **No stream is needed at all** — see below |
| Soloed | **Not a question.** A solo never reaches an export |
| Two or more inputs | Not a mix question: it is a join, and [#88](https://github.com/wildware-uk/clipped/issues/88)'s |

Muting is the interesting row. A muted track contributes no samples from the
recording, so nothing has to be decoded for it — but the exported file still has
to have that track, or a clip with four sliders becomes a file with three, and
the user who unmutes it later finds the audio gone. **Producing silence needs an
encoder; omitting the track needs nothing.** Today `crates/export` produces
silence and names the mute as a reason a copy is impossible
([exporting.md](exporting.md#audio)); whether that or an omitted track is the
better answer is a decision for that engine, tracked in
[#325](https://github.com/wildware-uk/clipped/issues/325) so it is made
deliberately rather than by whichever crate got there first.

Nothing in this crate can answer whether a copy is possible *at all*: that
depends on the codecs in the file, which `crates/edit` never opens.

## Overlays and framing

Text is timed in **output** time — "three seconds into the clip" stays true when
the material behind it is trimmed or sped up — and positioned and sized as
fractions of the frame, so that the same clip exported at 1080p and at 720p
looks the same. Deliberately minimal: a line of text, a position, a height and a
range. Colour, fonts and animation are absent, and adding one later is a new
field and a new schema version.

Crop and rotation belong to a **segment**, because an edit may join a landscape
recording to a portrait one and one rectangle cannot mean the right thing in
both. Crop is fractions of the source frame and is applied *before* rotation, so
a rectangle drawn on the picture the user is looking at survives them rotating
it afterwards. The aspect ratio belongs to the **document**, because it
describes the file being written and there is only one of those. It is a ratio
and not a resolution: how many pixels to render is an export setting
([#90](https://github.com/wildware-uk/clipped/issues/90)) that may differ
between two exports of the same clip, while "this clip is vertical" is an edit
decision the user made.

## Where a document lives

**In SQLite, as text in a column.** AGENTS.md section 32 puts application
metadata in the database or in a documented sidecar, and section 55 says not to
build a second implementation of something that already exists — so an edit
document is not a file format with a directory and a locking story of its own.
It is a value, and [#55](https://github.com/wildware-uk/clipped/issues/55) owns
the schema that holds it, alongside the recordings and clips it refers to.

`crates/edit` is therefore an encoder and a decoder and nothing else. That has
three consequences worth stating:

- The database layer stores the text **opaquely**. `clipped-storage` does not
  depend on `clipped-edit` — they are both at layer 0 of README.md's dependency
  table — and does not need to: a `TEXT` column does not have to understand its
  contents. Interpreting a document is the job of whichever layer above is
  about to show or render it.
- A document crosses the IPC boundary as the same JSON, rather than being
  converted into a second representation for the desktop application.
- **Migration is the caller's write, not this crate's.** Reading a document
  written by an older build converts it in memory and reports that it did; the
  caller decides whether to store the result, and must keep the original when it
  does. Nothing is rewritten by the model.

  **The recorder is that caller.** `save_clip_document` copies the older text
  into `clips.edit_superseded` in the same transaction that replaces it
  (migration `0008`), once and never again — the column holds the only text
  this build could not have produced, so a later save must not overwrite it
  with text this build wrote. Reading a document writes nothing at all: a user
  who opens a clip made by an older Clipped and closes it again has changed
  nothing.

## Compatibility

These documents are user data that has to survive updates (AGENTS.md section
43), including the update the user has not installed yet on their other machine.
The rules are the ones `crates/game-detection`'s catalogue overlay already
follows:

| The document is | What happens |
| --- | --- |
| The current version | Read directly |
| Older | Converted in memory through the migration chain, then validated; the caller is told, and keeps the original if it stores the result |
| Newer | **Refused. Nothing is written back.** The message says to update Clipped and that nothing has been changed |
| Missing its version | Refused |
| Carrying a field this build does not know, at the current version | Refused |

That last row is the one worth arguing for. Every shape change bumps the
version, *including one that only adds a field* — these documents are written by
Clipped and never by hand, so bumping costs nothing. The alternative is an older
build opening a newer document, silently dropping the field it did not
understand, and writing that back the next time the user saves. Refusing to open
beats opening and quietly discarding.

The migration chain is followed by matching each step's `from` to the version
reached so far, not by assuming single increments, and a step that would
overshoot the version this build reads is not taken. A conversion that fails, or
whose result does not validate, refuses the document and leaves it exactly as it
was.

`SCHEMA_VERSION` is **2**, and one migration has shipped:

| Version | What changed | Converting a document from the version before it |
| --- | --- | --- |
| 1 | The first there has ever been ([#82](https://github.com/wildware-uk/clipped/issues/82)) | — |
| 2 | An audio track no longer carries `soloed` ([#85](https://github.com/wildware-uk/clipped/issues/85)) | The flag is dropped |

Dropping it is the only honest conversion. A solo describes a moment of
somebody's editing session rather than the clip, so there is nothing in a
version 2 document for it to become; turning it into mutes on the other tracks
would silence material the user never asked to lose, which is the opposite of
AGENTS.md section 56. Everything that *is* the edit — the level, the mute and
the fades of every track — comes through untouched, and the caller is told the
document was converted so it can decide whether to store the newer text (keeping
the original).

## What a document may not say

Validation runs on every read *and* every write. Refusing to write a broken
document is the more valuable half: it means nothing unreadable can reach the
database, so an edit made last year still opens.

- A segment may not play a source the document does not declare, have an empty
  or backwards span, have a zero in its speed, be cropped outside the frame, or
  be so short at its speed that it contributes no output at all.
- Two sources may not share an identifier, and a source must say which recording
  it is.
- An audio track needs a name, needs at least one input, and may not draw on an
  undeclared source. Names are unique, and one recorded stream may not feed two
  output tracks — the export would carry the same audio twice under two sliders.
- A level must be a real number between -60 dB and +12 dB. Fades may not exceed
  the clip.
- An overlay must say something, must be on screen for a real range that ends
  within the clip, must be positioned on the frame, and must be between 1% and
  50% of the frame's height.

An empty document — no sources, no segments — is **valid**. A user who deleted
everything has an empty clip, not a corrupt one.

The rule for the operations built on this model is that they keep the document
valid: trimming the end of a clip that has an overlay running past the new end
is [#84](https://github.com/wildware-uk/clipped/issues/84)'s problem to clamp,
not the model's to tolerate. Every operation validates its result before
returning it, so an edit can refuse but cannot produce something unwritable.

## Testing it

```text
cargo test -p clipped-edit
```

No hardware, no files, no fixtures on disk. The suite covers the timeline
arithmetic at and either side of every boundary, the mute/solo matrix, the fade
envelope at both ends and where two fades meet, every validation refusal, the
version and migration behaviour above, and each operation at a boundary, across
a boundary, over a whole segment, at the ends of the clip and where it would
leave nothing. Three whole-model tests carry the acceptance criteria of
[#82](https://github.com/wildware-uk/clipped/issues/82),
[#84](https://github.com/wildware-uk/clipped/issues/84) and
[#85](https://github.com/wildware-uk/clipped/issues/85):

- `tests/sources_are_never_touched.rs` — a checksummed file before and after
  everything the crate can do to a document that names it, **including trimming,
  splitting, deleting, remixing, undoing and redoing**, and a check that the
  crate's source contains no file access at all. Lowering a track is in that
  list because "changing the audio levels of a recording" is exactly the phrase
  a user would expect to mean the recording changed.
- `tests/an_edited_clip_plays_what_is_left.rs` — the clip is kept a second way,
  as a plain list of the moments of the original timeline that survived each
  operation, and every position of the edited clip must play what the
  corresponding element of that list played. The list knows nothing about
  segments or spans, so it cannot agree with the implementation by construction
  the way an assertion about segment spans does. The same file walks undo and
  redo across six operations — four cuts and two changes to the mix — and
  compares the *stored text* at every step.
- `tests/round_trip_is_identical_playback.rs` — the clip is *read* at
  one-tenth-second steps from before its start to past its end, recording which
  recording is on screen, which frame of it, which text is over it, what each
  audio track contributes and **how loud each track is at that moment**; that
  transcript must be identical after a save and a reload, and saving what was
  read must produce the same bytes. The level is sampled through the walk rather
  than read off the track because a fade is a different number at every step: a
  save that lost one leaves the curve flat, which a comparison of fields would
  have shown as a missing field and a comparison of playback shows as the clip
  sounding different.

Comparing two documents with `==` would have been the easy version of the
second, and would prove less: two documents can be equal and still be read
differently if the reading depends on anything outside them.

A round trip is also only worth what its fixture covers, which is a trap this
one fell into: the first version of it left `aspect_ratio` and the old `soloed`
flag at their defaults, so a build that discarded both on every save passed.
(That flag is gone — it is [not part of the
document](#solo-is-not-a-property-of-a-track) — and the check that caught it is
what the level and the fades now go through.) The fixture
holds a value other than the default for every field of every structure this
crate writes, and a second test enforces that by comparing it against a baseline
document built from the plain constructors, over the serialised text, field by
field. A field added to the model later arrives at that baseline's value on both
sides, compares equal, and is named in the failure — so extending the model
means extending the fixture, rather than quietly shipping a field nothing checks
survives a save.

The same file sweeps `deny_unknown_fields`: a key this build does not
understand is pushed into every object of a fully populated document in turn —
a segment, a span, a speed, a crop, a track, an input, an overlay, a position,
the aspect ratio and the document itself — and each must be refused by name. A
structure added later is swept without anybody adding it to a list.
