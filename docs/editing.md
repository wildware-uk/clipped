# Editing

**Status: the document model exists and is tested; nothing edits or renders one
yet.** `crates/edit` defines what an edit *is*, reads and writes it, converts an
older one, and answers the question an exporter asks — "what is on screen at
this moment, and where does it come from?". The editor itself is
[issue #83](https://github.com/wildware-uk/clipped/issues/83), the operations
are [#84](https://github.com/wildware-uk/clipped/issues/84) to
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
  "schema_version": 1,
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
      "soloed": false,
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

**Mute and solo**, which [#85](https://github.com/wildware-uk/clipped/issues/85)
asks to be predictable and documented, follow the rules every mixing desk uses:

- **Mute wins**, including over solo on the same track. Soloing a muted track
  does not unmute it. Solo is a way of listening to part of a mix, not a second
  mute button with the opposite sense.
- **Solo is exclusive.** If any track in the document is soloed, every track
  that is not soloed is silent.
- **Solo does nothing when nothing is soloed**, so the ordinary case is just
  mute and gain.

`EditDocument::track_output` resolves all of that into `Silent` or
`Audible { gain_db }` — a type rather than an `f64`, so that "silent" cannot be
misread as "no gain applied", which is the mistake that exports a muted
microphone at full volume.

**Fades** are a length at the start and a length at the end of the clip, in
output time. The curve is defined here so that preview and export cannot differ:
the multiplier rises **linearly in amplitude** from zero to the track's level
across `fade_in`, and falls linearly to zero across `fade_out`. Fades may not
add up to more than the clip lasts.

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

`SCHEMA_VERSION` is 1 and the shipped migration list is **empty**, correctly:
version 1 is the first version there has ever been, so no older document exists
anywhere and writing a migration from one would be inventing history. The
machinery that runs them is built and tested now, against conversions the tests
supply themselves, so that the first time a migration runs is not also the first
time the code around it does.

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
not the model's to tolerate.

## Testing it

```text
cargo test -p clipped-edit
```

No hardware, no files, no fixtures on disk. The suite covers the timeline
arithmetic at and either side of every boundary, the mute/solo matrix, every
validation refusal, and the version and migration behaviour above. Two
whole-model tests carry the acceptance criteria of
[#82](https://github.com/wildware-uk/clipped/issues/82):

- `tests/sources_are_never_touched.rs` — a checksummed file before and after
  everything the crate can do to a document that names it, and a check that the
  crate's source contains no file access at all.
- `tests/round_trip_is_identical_playback.rs` — the clip is *read* at
  one-tenth-second steps from before its start to past its end, recording which
  recording is on screen, which frame of it, which text is over it and what each
  audio track contributes; that transcript must be identical after a save and a
  reload, and saving what was read must produce the same bytes.

Comparing two documents with `==` would have been the easy version of the
second, and would prove less: two documents can be equal and still be read
differently if the reading depends on anything outside them.

A round trip is also only worth what its fixture covers, which is a trap this
one fell into: the first version of it left `aspect_ratio` and `soloed` at their
defaults, so a build that discarded both on every save passed. The fixture now
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
