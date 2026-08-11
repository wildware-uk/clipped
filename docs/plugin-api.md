# Plugin API

**Status: half built.** The *event model* — what a plugin reports, how it is
timed, and how it stays readable for years — exists and is documented below
([issue #68](https://github.com/wildware-uk/clipped/issues/68),
`crates/events`). The *plugin contract* — the `HighlightProvider` interface,
how a plugin is discovered, started, supervised and isolated — does not
([issue #69](https://github.com/wildware-uk/clipped/issues/69)); `crates/plugins`
is still module documentation and no code, and the second half of this document
is the list of what will go there.

That split is deliberate rather than an accident of scheduling. A plugin API is
a compatibility surface: once it is published, third-party plugins depend on it
and it cannot be changed casually (AGENTS.md section 43). The event model is the
part five other issues wait on — the three game integrations
([#70](https://github.com/wildware-uk/clipped/issues/70),
[#72](https://github.com/wildware-uk/clipped/issues/72),
[#73](https://github.com/wildware-uk/clipped/issues/73)), persisting events and
drawing them on a timeline ([#71](https://github.com/wildware-uk/clipped/issues/71)),
and the automatic highlight rules of M10 — so it is settled first, and
described here as it is rather than as it might be.

- [What the model is for](#what-the-model-is-for)
- [The event](#the-event)
- [The vocabulary](#the-vocabulary)
- [Custom events](#custom-events)
- [Confidence, and what it is not](#confidence-and-what-it-is-not)
- [Timing](#timing)
- [Positions in a file, and in a replay clip](#positions-in-a-file-and-in-a-replay-clip)
- [The stored form](#the-stored-form)
- [Compatibility policy](#compatibility-policy)
- [How the three planned integrations map](#how-the-three-planned-integrations-map)
- [What this document will still cover](#what-this-document-will-still-cover)

## What the model is for

Counter-Strike 2 posts a JSON blob to a local HTTP endpoint whenever the state
it was configured to watch changes. League of Legends answers an HTTPS request
on `127.0.0.1:2999` with a list of events measured in seconds since the match
began. Dota 2 posts its own Game State Integration payload, shaped like neither.
Other games have a log file, or a replay you can only read afterwards, or
nothing at all.

The rule (AGENTS.md section 33) is that **a plugin exposes events through a
stable abstraction rather than forcing the core application to understand every
game's native protocol.** So all of that stops at the plugin. What comes out is
one shape:

```json
{"schema":1,"kind":"kill","at":61500000000,"precision":0,"latency":480000000,
 "source":"counter-strike-2","confidence":1.0,"data":{"weapon":"ak47"}}
```

and nothing above `crates/events` — not the session, not the timeline, not the
highlight rules — can tell which game produced it. That is the design
constraint, and every decision below follows from it. `crates/events` sits at
layer 0 of the dependency table in [README.md](../README.md) and depends on no
other crate in the workspace, because a vocabulary that needs the session or the
library in order to be understood is not shared.

## The event

`clipped_events::GameEvent`. Five parts — a kind, a moment, a source, a
confidence and a payload — which are seven fields on the wire, because the
moment takes three. The wire names are frozen: a stored event outlives the build
that wrote it, and everything in
[the compatibility policy](#compatibility-policy) rests on the envelope staying
readable forever.

| Field | Type | What it is |
| --- | --- | --- |
| `kind` | string | What happened. A [standard tag](#the-vocabulary), or a [namespaced custom name](#custom-events). |
| `at` | integer, signed nanoseconds | When it happened, on the recording's timeline. See [Timing](#timing). |
| `precision` | integer, nanoseconds | How far either side of `at` the true moment may lie. `0` means the source timed it exactly. Required. |
| `latency` | integer, nanoseconds | How much later than `at` the report arrived. Omitted when zero. |
| `source` | string | Who reported it: a plugin identifier, or `clipped` for the application itself. |
| `confidence` | number, 0 to 1 | How sure the source is that it happened at all. See [below](#confidence-and-what-it-is-not). |
| `data` | object | Whatever the source wanted to attach. Omitted when empty. Nothing above the plugin interprets it. |

`data` is where a game's own vocabulary lives — `weapon`, `championKilled`,
`hero_id` — and it is deliberately opaque above the plugin that wrote it. A
consumer that finds itself switching on a payload key to decide what an event
*means* has moved a game's protocol back into the core, and the answer is a new
`kind`, not a special case. It is capped at
`clipped_events::MAX_PAYLOAD_BYTES` (4 KiB of JSON), checked when a producer
builds one: a plugin is another program's output, every event is stored, and an
event stream is not a transport for something else.

## The vocabulary

The kinds are `SPEC.md` section 21's list, and they are a closed set. A variant
is a concept the *application* acts on — something a clip can be cut around, a
marker drawn for, a rule written against — not a concept a particular game
happens to have.

| `kind` | Meaning |
| --- | --- |
| `game_started`, `game_ended` | A game the application recognises started or stopped running. |
| `match_started`, `match_ended` | A match, game or session inside the game began or finished. |
| `round_started`, `round_ended` | A round, wave or life inside a match. |
| `kill`, `death`, `assist` | The player killed someone, died, or helped. |
| `win`, `loss` | How it ended for the player or their team. |
| `score`, `goal` | Points scored; an objective completed. |
| `achievement` | The game awarded something. |

`game_started` and `game_ended` are the two the application can already report
itself, from the process watcher
([#41](https://github.com/wildware-uk/clipped/issues/41),
[#46](https://github.com/wildware-uk/clipped/issues/46)); they are in the shared
vocabulary because a plugin that can do better — a game that says when the
*game* rather than the *process* started — should report the same kind rather
than inventing one.

## Custom events

`Custom` is how a plugin says something this list does not cover. An open
variant in a shared vocabulary is also how that vocabulary rots into a bag of
strings, so exactly one rule holds it back:

> **A custom name is namespaced. A standard tag is not.**

```text
kill                                  a standard event
round_started                         a standard event
acme-cs2.flag_captured                somebody's own word
```

Syntax: two or more `.`-separated segments; each segment starts with an ASCII
lowercase letter and continues with lowercase letters, digits, `-` or `_`; 64
bytes in total; the namespace `clipped` is reserved for the project. Lowercase
is enforced rather than folded, because `Flag_Captured` beside `flag_captured`
is two marks on a timeline that the user believes is one.

Three things follow, and they are why the rule is worth having:

1. **A plugin can never shadow or pre-empt a standard event.** It cannot emit
   `kill`, because that name has no namespace to put in front of it, and it
   cannot claim `objective_taken` before the project defines it.
2. **A name says who is answerable for it.** An unexplained mark on a timeline
   is traceable to the plugin whose namespace it carries, with no registry to
   consult. By convention the namespace is the plugin's own identifier.
3. **Both halves can grow independently.** A build that has never heard of
   `objective_taken` reads it as an unrecognised *standard* kind and keeps it; a
   build that has never heard of `acme-cs2.flag_captured` still reads it as a
   custom event, because the rule is syntactic and needs no table of known
   names.

**Promotion.** When a custom name turns out to be universal — two integrations
independently reporting the same thing — it becomes a standard kind in a later
release. Events already stored under the custom name stay exactly as they are
and stay readable; they do not migrate, because a stored event records what the
plugin actually said. The plugin starts emitting the standard kind from the
version it adopts, and a rule that cares about both matches both. This is the
one place the model expects to change shape, and it costs no schema bump.

## Confidence, and what it is not

`confidence` is how sure the source is that the event **happened**.
`precision` is how sure it is about **when**. They come apart in both
directions, which is why they are separate fields rather than one number:

- Game State Integration is polled, and says exactly what happened. Certain,
  imprecise.
- A detector watching the screen for a kill feed
  (`SPEC.md` section 24) knows which frame it looked at and is guessing about
  the kill. Precise, unsure.

A highlight rule filters on the first and pads a clip with the second.

There is no default. An integration reading an authoritative feed reports
`1.0`; a detector that computes a score reports the score it computed. Nothing
in between should be invented (AGENTS.md section 27). `NaN` is refused at
construction, because a `NaN` confidence compares false against every threshold
and would make an event silently invisible rather than merely uncertain.

## Timing

An event's position in a recording is the whole of its usefulness, and it is the
part a plugin cannot supply directly.

**The recording's timeline is `MediaTime`** — signed nanoseconds from the
capture clock's epoch, which is the timestamp of the first video frame the
recording keeps ([docs/av-sync.md](av-sync.md)). A plugin knows none of that. It
knows a game clock ("14:22 remaining"), or a wall clock, or a moment measured in
seconds since a match began. So:

> **The plugin reports in the terms it has; whoever attaches the plugin to a
> session converts once, through the recording's `CaptureClock`, and the result
> enters the model through `EventTime::from_media_nanos`.**

The conversion happens in one place, named, for the same reason every other
clock conversion in this codebase does: it is a claim about which timeline a
number came from, and a reviewer should have one line to check
([docs/av-sync.md](av-sync.md), "Where a conversion happens"). Nanoseconds from
any other zero produce an event in the wrong place, and nothing downstream can
detect it.

`EventTime` is signed, like `MediaTime`, because a moment before the epoch is
normal rather than a fault: a plugin attached to a game that was already running
reports the match it joined, and the first video frame Clipped kept may be later
than the thing being described.

### An event does not move because it arrived late

A plugin observes a game *telling it something*, and the two are not
simultaneous. So an event carries three numbers rather than one:

- **`at`** — the moment it describes. This is where it is drawn, and the only
  field a timeline uses.
- **`precision`** — how far either side of `at` the truth may lie. A source
  polled every two seconds knows the moment to within a second: it places `at`
  in the middle of the window it is sure about and says `precision` is one
  second. Zero is the claim "I timed this exactly", and it is required on the
  wire precisely so that a document which never made that claim cannot start
  making it by omission.
- **`latency`** — how much later than `at` the report arrived. This is **not** a
  correction; the event does not move. It is what tells a consumer whether
  reacting was possible at all: the replay buffer holds a fixed window
  ([docs/replay-buffer.md](replay-buffer.md)), and an event whose latency
  exceeds it describes a moment the buffer has already thrown away.

Two consequences for anything that consumes events:

- **Sort by `at`.** Events arrive in whatever order their transports allow, from
  several sources at once. A consumer that appends them draws them in arrival
  order, which is not time order.
- **An event can arrive for a moment already written to disk.** That is normal
  and it is not an error: the file is not rewritten, the event is stored against
  the recording with the position it always had, and a timeline drawn later puts
  it where it belongs.

## Positions in a file, and in a replay clip

An event's `at` is a moment in the *recording*. A player seeks by position in a
*file*. For a recording written from its start those are the same number, and it
is tempting to leave them conflated — until the replay buffer, where they are
not.

A saved replay clip is cut from a buffer that has been running for as long as
the game has. Its first packet is a keyframe some way down the recording's
timeline, the file begins there, and an event twenty minutes into the session is
ten seconds into the clip.

`clipped_events::RecordedSpan` is that subtraction, written once:

```rust
let clip = RecordedSpan::new(start_of_clip, end_of_clip)?;   // media times
let position = event.position_in(&clip);                     // Option<Duration>
```

- `start` and `end` are the media times of the file's first and last packets,
  which is what whoever wrote the file knows: `RecordedSpan::from_epoch` for a
  whole recording, and the keyframe a `SegmentLease` began with for a replay
  clip — including the leading slack the buffer could not trim
  ([docs/replay-buffer.md](replay-buffer.md), "Granularity, and what it costs").
- A moment the file does not contain gives `None`, **not** a clamp. A marker
  pinned to the first frame of a clip that does not contain the kill it claims
  is worse than no marker: it is a lie the user has no way to check (AGENTS.md
  section 27). The caller decides what to do — a timeline omits it, a highlight
  rule looks for a different clip.

**The assumption, stated:** that the file's own zero is the span's `start`. The
muxer sets a file's origin from the first packet it is given and clamps anything
earlier to it ([docs/muxing.md](muxing.md)), so this holds when the packet that
opens the file is the one whose media time is `start`. Audio captured before the
first video frame is the known exception and
[issue #174](https://github.com/wildware-uk/clipped/issues/174) is the fix;
until it lands, positions computed here inherit the same head-of-file error the
rest of the pipeline has.

## The stored form

Events are persisted against sessions and recordings
([#71](https://github.com/wildware-uk/clipped/issues/71)). The document is the
envelope plus one field:

```json
{"schema":1,"kind":"kill","at":61500000000,"precision":0,"latency":480000000,
 "source":"counter-strike-2","confidence":1.0}
```

`schema` travels **with each event** rather than with the file or the table that
holds it, so an event copied out of one and into another — exported, attached to
a bug report, moved between a session's sidecar and the library database — is
still self-describing. `clipped_events::schema::read` is the reading path: it
reports which schema wrote the document and upgrades it if it needs upgrading.

## Compatibility policy

This is [docs/ipc.md](ipc.md)'s compatibility policy applied to data at rest
rather than data on a wire, and it is deliberately the same policy, because the
alternative is two answers to one question. There is one difference, and it
follows from the change of medium: **nothing here ever refuses to read a stored
event.** A refusal on a wire ends a connection that can be made again; a refusal
at rest destroys something the user cannot get back (AGENTS.md section 56).

**The envelope is frozen.** `schema`, `kind`, `at`, `precision`, `latency`,
`source`, `confidence`, `data`: these names and their meanings do not change.
Everything below rests on that.

| What a build meets | What it does |
| --- | --- |
| **A field it does not know** | Ignores it. Adding a field costs no version bump — this is `serde`'s default behaviour, and the reason the version stays still for years. |
| **A `kind` it does not know, unnamespaced** | Keeps it verbatim as `EventKind::Unrecognised`. It is a kind added to the vocabulary after this build shipped: still a mark it can place, attribute and draw, and still exactly what it was when written back. |
| **A `kind` it does not know, namespaced** | Keeps it as `EventKind::Custom`. A plugin's vocabulary works on every build, because the rule is syntactic. |
| **A `schema` newer than its own** | Reads it, and flags it. The envelope is frozen, so the times and the source are exactly what they say they are; what a bump can change is the meaning of what lies *inside*, so `ReadEvent::is_understood` is false and a consumer that wants to interpret `data` knows not to. |
| **A `schema` older than its own** | Upgrades it, through `schema::upgrade`. |
| **A `schema` field that is missing** | Refuses, by name. Every document this crate writes has one, and a document without one cannot be interpreted at all — this is the one case where guessing would be worse than failing. |
| **An envelope it cannot read at all** | Refuses, saying so. An event with no time is not a mark on any timeline. |

The catch-alls are the part that has to be *implemented* rather than inherited.
An unknown field costs nothing to ignore; an unknown *tag* in a tagged union
fails the whole document it is part of, which would take a mark off somebody's
timeline over a word this build had not learned. `EventKind::Unrecognised` and
`EventKind::Custom` are what make the table above true rather than aspirational,
and `crates/events`'s tests assert it in both directions — including that an
event read from a future schema and written back again survives unchanged, which
is what "survive" has to mean for a library that gets re-indexed.

**When the version changes.** Adding a kind, a field, a source or a custom name
does *not* bump `SchemaVersion`. Removing one, renaming one, or changing what
one means does. Since the envelope is frozen, in practice a bump can only be
about the interpretation of a payload or of an existing kind.

`SchemaVersion` is a closed enumeration rather than a bare integer, and
`schema::upgrade` matches on it exhaustively. Adding a version therefore stops
the crate compiling until the step that migrates the documents already on disk
is written — a schema can be bumped, but it cannot be bumped quietly, leaving
events on somebody's disk to be discovered unreadable a release later. Today
there is one version and no upgrade step, because there is nothing yet to
upgrade from.

## How the three planned integrations map

**None of these exist.** They are the three shapes the model was designed
against, and they are recorded here because the design's only real test is
whether it absorbs their differences without any of them reaching the core.

| | Counter-Strike 2 ([#70](https://github.com/wildware-uk/clipped/issues/70)) | League of Legends ([#72](https://github.com/wildware-uk/clipped/issues/72)) | Dota 2 ([#73](https://github.com/wildware-uk/clipped/issues/73)) |
| --- | --- | --- | --- |
| Native shape | Game State Integration: a JSON state blob posted to a local endpoint on change | Live Client Data: a polled local HTTPS API returning a list of events with a match-relative time | Game State Integration: a different JSON state blob, posted the same way |
| What an event is natively | A *difference* between two state blobs — the previous round score and this one | An entry in an array, with `EventID` and `EventTime` in seconds since the match began | A difference between two state blobs, with a different shape again |
| Becomes | `kill`, `death`, `assist`, `round_started`, `round_ended`, `match_started`, `match_ended`, `win` | the same set, from a different derivation | the same set again |
| `at` | when the state changed, which is bounded by how often the game posts | the match-relative time, anchored to the media time of `match_started` | as CS2 |
| `precision` | the posting interval the plugin configured | whatever the match-relative clock's resolution is, plus the error in the anchor | as CS2 |
| `latency` | transport and parse | the poll interval, worst case | transport and parse |
| `data` | `weapon`, `headshot`, and the rest of the game's own words | `KillerName`, `VictimName`, … | Dota's own words |

The third row is the point: three unrelated derivations produce the same seven
kinds, and the differences that survive are two integers and an opaque object.

Whatever the technique, AGENTS.md section 34 is absolute: official APIs, local
telemetry, game logs, Game State Integration, documented IPC and supported
replay files only. Nothing that resembles injection or memory inspection, no
matter what it would enable. A user's game account is worth more than a
highlight.

## What this document will still cover

Written during M9 alongside the code, and treated as the reference a third-party
plugin author works from:

- The `HighlightProvider` contract as implemented: its operations, its
  lifecycle, and what a provider may assume about the session it is attached to
  ([issue #69](https://github.com/wildware-uk/clipped/issues/69)).
- Discovery, loading, supervision and isolation: where plugins live, what
  happens when one crashes, hangs or floods the event channel, and why that must
  not affect a recording.
- The permitted integration techniques in detail, and the explicitly forbidden
  ones (AGENTS.md section 34).
- Network access by plugins, which must be visible and documented rather than
  incidental (`SPEC.md` section 39).
- Versioning of the *contract*, as distinct from the versioning of the event
  schema settled above, and what a plugin built against an older version can
  expect.
- How to write and test a plugin, using the Counter-Strike 2 integration
  ([issue #70](https://github.com/wildware-uk/clipped/issues/70)) as the worked
  reference.
