# Highlights and virtual clips

**Status: the virtual clip model exists and is tested; nothing creates, stores
or lists one yet.** `crates/library/src/virtual_clip.rs` defines what a clip
*is* before anything has been exported. The rules that decide which moments
deserve one are [issue #75], generating them from a session's events is [#76],
the two capture modes built on them are [#77] and [#78], and creating one by
hand from the timeline is [#91]. This document is the specification all of them
are held to.

[issue #75]: https://github.com/wildware-uk/clipped/issues/75
[#76]: https://github.com/wildware-uk/clipped/issues/76
[#77]: https://github.com/wildware-uk/clipped/issues/77
[#78]: https://github.com/wildware-uk/clipped/issues/78
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

## What #76 will produce

For each event the rules select, generation produces exactly this and writes
nothing:

```rust
let window = window_around(event.timing().at(), lead, trail, &recorded)?;
let clip = VirtualClip::of_range(title, recording, window, ClipOrigin::Highlight(cause))
    .with_tag(event.kind().as_str());
```

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
trail each kind gets, and how a burst of them is merged into one clip are [#75]'s
rules, which produce the `lead` and `trail` passed in.

## What it costs

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
