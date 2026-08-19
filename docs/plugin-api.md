# Plugin API

**Status: three plugins exist, and nothing in the recorder starts any of
them.** Both
halves of the contract exist. The *event model* — what a plugin reports, how it
is timed, and how it stays readable for years — is `crates/events`
([issue #68](https://github.com/wildware-uk/clipped/issues/68)). The *plugin
contract* — what a plugin is, how one is found, started, supervised and kept
away from a recording — is `crates/plugins`
([issue #69](https://github.com/wildware-uk/clipped/issues/69)). The
integrations written against it are `plugins/league`
([issue #72](https://github.com/wildware-uk/clipped/issues/72)), described in
[The League of Legends plugin](#the-league-of-legends-plugin), `plugins/cs2`
([issue #70](https://github.com/wildware-uk/clipped/issues/70)) —
Counter-Strike 2, through Game State Integration — described in
[The reference plugin](#the-reference-plugin), and `plugins/dota2`
([issue #73](https://github.com/wildware-uk/clipped/issues/73)) — Dota 2,
through Game State Integration as well — described in
[The Dota 2 plugin](#the-dota-2-plugin-and-what-it-shares-with-counter-strike-2).

What is still missing is the wiring: **nothing in the recorder attaches a
supervisor to a session**, so no plugin runs during a real recording, and both
plugins are programs that have to be started by hand to see them work. That
is [What is not built](#what-is-not-built), and it is stated here as well
because it is the thing most likely to be misread (AGENTS.md section 7).

The worked example in this document is
`crates/plugins/examples/example_plugin.rs`, which is a real plugin that a real
supervisor runs in the crate's tests.

The order was deliberate rather than an accident of scheduling. A plugin API is
a compatibility surface: once it is published, third-party plugins depend on it
and it cannot be changed casually (AGENTS.md section 43). The event model is the
part five other issues wait on, so it was settled first; the contract around it
follows, and both are described here as they are rather than as they might be.

The event model:

- [What the model is for](#what-the-model-is-for)
- [The event](#the-event)
- [The vocabulary](#the-vocabulary)
- [Custom events](#custom-events)
- [Confidence, and what it is not](#confidence-and-what-it-is-not)
- [Timing](#timing)
- [Positions in a file, and in a replay clip](#positions-in-a-file-and-in-a-replay-clip)
- [The stored form](#the-stored-form)
- [Compatibility policy](#compatibility-policy)
- [How the three integrations map](#how-the-three-integrations-map)

The contract:

- [What a plugin is](#what-a-plugin-is)
- [The manifest](#the-manifest)
- [Network access, consent, and what enforcement can promise](#network-access-consent-and-what-enforcement-can-promise)
- [Filesystem access, and what changed in contract 2](#filesystem-access-and-what-changed-in-contract-2)
- [Discovery](#discovery)
- [The lifecycle](#the-lifecycle)
- [The wire](#the-wire)
- [How long ago, not when](#how-long-ago-not-when)
- [What a misbehaving plugin costs a recording](#what-a-misbehaving-plugin-costs-a-recording)
- [Supervision and restart](#supervision-and-restart)
- [Writing a plugin](#writing-a-plugin)
- [What a plugin may not do](#what-a-plugin-may-not-do)
- [Versioning the contract](#versioning-the-contract)

The plugins written against it, in the order they appear below:

- [The League of Legends plugin](#the-league-of-legends-plugin)
- [The reference plugin](#the-reference-plugin) — Counter-Strike 2
- [Writing into somebody else's game](#writing-into-somebody-elses-game)
- [Deriving events from state](#deriving-events-from-state)
- [The Dota 2 plugin, and what it shares with Counter-Strike 2](#the-dota-2-plugin-and-what-it-shares-with-counter-strike-2)
- [What is not built](#what-is-not-built)

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
| `at` | integer, signed nanoseconds | When it happened, on the session's timeline. See [Timing](#timing). |
| `precision` | integer, nanoseconds | How far either side of `at` the true moment may lie. `0` means the source timed it exactly. Required. |
| `latency` | integer, nanoseconds | How much later than `at` the report arrived. Omitted when zero. |
| `source` | string | Who reported it: a plugin identifier, or `clipped` for the application itself. `clipped` is reserved — `EventSource::plugin` refuses it. |
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

**Checked when produced, not when read.** The range is enforced where a producer
creates a confidence, and *not* again when one is read back out of a library —
for the same reason the payload size limit is not
([the compatibility policy](#compatibility-policy)). A stored `1.5` is read as
it was written and reported by `Confidence::is_usable`, because failing the
document would lose the event's kind, time and payload over one number. The same
applies to `source`: `EventSource::plugin` enforces the identifier syntax,
`EventSource::is_well_formed` reports whether what was read obeys it, and
nothing on the read path refuses a document over it.

## Timing

An event's position in a recording is the whole of its usefulness, and it is the
part a plugin cannot supply directly.

**The session's timeline is `MediaTime`** — signed nanoseconds from the
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
`ReadEvent::to_json` is the way back, and keeps everything the reading build did
not understand — see [the compatibility policy](#compatibility-policy).
`StoredEvent::new` is for an event this build produced, and writes only what the
model holds.

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
| **A field it does not know** | Ignores it, **and keeps it**. Adding a field costs no version bump. Ignoring it is `serde`'s default; keeping it is not, and is why `ReadEvent::to_json` exists — a build that reads a library and writes it back would otherwise delete every field it had not learned. |
| **A `source` or `confidence` its own types would refuse** | Reads it as it was stored. Those rules are enforced where a producer creates an event; enforcing them again on the way out of a database would fail the whole document over one field, and lose the kind, the time and the payload with it. `EventSource::is_well_formed` and `Confidence::is_usable` are how a consumer asks. |
| **A `kind` it does not know, unnamespaced** | Keeps it verbatim as `EventKind::Unrecognised`. It is a kind added to the vocabulary after this build shipped: still a mark it can place, attribute and draw, and still exactly what it was when written back. |
| **A `kind` it does not know, namespaced** | Keeps it as `EventKind::Custom`. A plugin's vocabulary works on every build, because the rule is syntactic. |
| **A `schema` it does not know** | Reads it, and flags it — in practice this is a file written by a newer Clipped. The envelope is frozen, so the times and the source are exactly what they say they are; what a bump can change is the meaning of what lies *inside*, so `ReadEvent::is_understood` is false and a consumer that wants to interpret `data` knows not to. It is reported as unknown rather than as *newer*, because a number below the current one would land here too and calling that "newer" would be a claim the reader cannot support. Writing it back keeps the number it arrived with: this build never re-encoded the payload, so stamping the current version on it would assert a meaning it did not read. |
| **A `schema` older than its own** | Upgrades it, through `schema::upgrade`, and writes it back at the current version — which is what upgrading it means. |
| **A `schema` field that is missing** | Refuses, by name. Every document this crate writes has one, and a document without one cannot be interpreted at all — this is the one case where guessing would be worse than failing. |
| **A `schema` field that is not a number** | Refuses, saying what it found. `"1"`, `1.0` and `-1` are not versions, and a document carrying one was not written by this crate. Reported separately from a missing field, because sending somebody to look for a field that is in front of them is worse than no error at all. |
| **An envelope it cannot read at all** | Refuses, saying so. An event with no time is not a mark on any timeline. |

The catch-alls are the part that has to be *implemented* rather than inherited.
An unknown field costs nothing to ignore; an unknown *tag* in a tagged union
fails the whole document it is part of, which would take a mark off somebody's
timeline over a word this build had not learned. `EventKind::Unrecognised`,
`EventKind::Custom` and `ReadEvent`'s kept unknown fields are what make the table
above true rather than aspirational.

**Reading and writing back.** `ReadEvent::to_json` is the write path for an
event that came off a disk, and `crates/events`'s tests compare the **whole
document** across it — not the fields the current model happens to know, which
is a comparison that cannot see the loss it is looking for. A document from a
schema this build has never met, carrying a kind it cannot name, fields it has
no names for and a payload it must not interpret, comes back out as the same
document. The one thing that is not byte-identical is a field this build *does*
understand: a document that spells out `"latency":0` or `"data":{}` gets them
back omitted, exactly as this build omits them when it writes an event of its
own. That is what "survive" has to mean for a library that gets re-indexed by
whichever build the user happens to be running.

**When the version changes.** Adding a kind, a field, a source or a custom name
does *not* bump `SchemaVersion`. Removing one, renaming one, or changing what
one means does. Since the envelope is frozen, in practice a bump can only be
about the interpretation of a payload or of an existing kind.

`SchemaVersion` is a closed enumeration rather than a bare integer, and two
guards make adding one a change that cannot be made quietly:

- `schema::upgrade` matches on it exhaustively, so a new version does not
  compile until the step that migrates the documents already on disk is written.
- `SchemaVersion::position` matches on it exhaustively too, and a constant
  asserts at compile time that `SchemaVersion::ALL` lists every version in order
  and ends at `CURRENT`. Without it, a version could be added — with a number,
  and with a migration — that `from_number` still answered `None` to, so every
  document the build wrote would read back as one from an unknown schema on the
  machine that wrote it.

The first is about a version the build cannot *migrate*; the second about one it
cannot *recognise*. Today there is one version and no upgrade step, because
there is nothing yet to upgrade from — and `crates/events`'s golden documents
are keyed by version and asserted to cover `ALL`, so the first bump cannot be
made without a version-1 document being read by the version-2 build.

## How the three integrations map

**All three of these now exist** — Counter-Strike 2 (`plugins/cs2`), League of
Legends (`plugins/league`) and Dota 2 (`plugins/dota2`). They are the three
shapes the model was designed against, and they are recorded here because the
design's only real test is whether it absorbs their differences without any of
them reaching the core. Every column is now a description of code rather than a
plan. League turned out to be what this table predicted, which is the most that
can be claimed for a prediction; Counter-Strike 2 and Dota 2 did not, and
[The reference plugin](#the-reference-plugin) and
[The Dota 2 plugin](#the-dota-2-plugin-and-what-it-shares-with-counter-strike-2)
say what each does instead — which is the more useful of the two results.

| | Counter-Strike 2 ([#70](https://github.com/wildware-uk/clipped/issues/70)) | League of Legends ([#72](https://github.com/wildware-uk/clipped/issues/72)) | Dota 2 ([#73](https://github.com/wildware-uk/clipped/issues/73)) |
| --- | --- | --- | --- |
| Native shape | Game State Integration: a JSON state blob posted to a local endpoint on change | Live Client Data: a polled local HTTPS API returning a list of events with a match-relative time | Game State Integration: a different JSON state blob, posted the same way |
| What an event is natively | A *difference* between two state blobs — the previous round score and this one | An entry in an array, with `EventID` and `EventTime` in seconds since the match began | A difference between two state blobs, with a different shape again |
| Becomes | `kill`, `death`, `assist`, `round_started`, `round_ended`, `match_started`, `match_ended`, `win` | the same set, from a different derivation | `kill`, `death`, `assist`, `match_started`, `match_ended`, `win`, `loss` — **and no rounds**, because Dota has none — plus `dota-2.kill_streak` |
| `at` | when the state changed, which is bounded by how often the game posts | the match clock in the same payload, minus the event's match-relative time: how long ago it happened, which is what the wire carries | the middle of the interval between the payload that changed and the one before it |
| `precision` | the posting interval the plugin configured | the request's measured round trip, plus an assumed bound on how precisely the two times are reported | half that interval, **measured rather than configured**: a game that has been paused posts when it posts |
| `latency` | transport and parse | how long ago it happened, which for an event from an earlier poll is longer than the poll interval | transport and parse |
| `data` | `weapon`, `headshot`, and the rest of the game's own words | `KillerName`, `VictimName`, … | `match_id`, `clock_time`, `hero`, and the running total the event came from |

The third row is the point, and the two integrations that exist have now tested
it: three unrelated derivations produce overlapping subsets of the same closed
vocabulary,
and the differences that survive are two integers and an opaque object. The
subsets are *not* identical, which is the more useful half of the result — Dota
has no rounds, and the answer to that was to report no rounds rather than to
find something round-shaped to map onto them. A vocabulary a game only partly
uses is working; a vocabulary every game fills in completely would be one whose
kinds had stopped meaning anything in particular.

Whatever the technique, AGENTS.md section 34 is absolute: official APIs, local
telemetry, game logs, Game State Integration, documented IPC and supported
replay files only. Nothing that resembles injection or memory inspection, no
matter what it would enable. A user's game account is worth more than a
highlight.

## What a plugin is

**A directory containing a manifest and an executable.** Clipped starts the
executable when a game it declares support for is launched, tells it about the
session on its standard input, and reads the events it prints on its standard
output, one JSON object per line.

```text
plugins/counter-strike-2/
    plugin.json              what it is, what it supports, and what it will do
                             with the network
    clipped-cs2-plugin.exe   a program that prints events
```

That is the decision issue #69 had to make, and it is worth recording what was
rejected, because this is a compatibility surface.

`SPEC.md` section 22 sketches a `HighlightProvider` interface —
`supports(process)`, `attach(session)`, `events()`, `detach()` — which reads
naturally as a Rust trait implemented inside the recorder. Three requirements
rule that out:

- **A crashing plugin must not touch a recording** (AGENTS.md sections 16 and
  17). In process, a panic can be caught; an abort, a stack overflow or a
  corrupted heap cannot, and a plugin fault takes the recorder and the recording
  with it. Across a process boundary, a plugin crash is an exit code. This is
  the argument [ADR 0002](adr/0002-separate-recorder-process.md) already made
  for keeping the recorder out of the window's process, applied one level down.
- **A hanging plugin must be reclaimable.** The recorder runs for days
  (AGENTS.md section 59), the likeliest failure of code that talks to a game
  over a socket is waiting for an answer that never comes, and **a hung thread
  cannot be killed**. A hung process can. `crates/plugins` tests exactly this:
  a plugin that says hello and then stops answering is terminated, *and the
  thread that was reading it ends*, because that thread was blocked on a pipe
  the dead process was holding.
- **A network declaration must be able to mean something.**
  [privacy.md](privacy.md) requires a plugin's network access to be declared and
  consented to before it runs, and says plainly that an in-process native plugin
  can never be held to such a declaration. A child process can be — not today,
  but the mechanism exists and
  [issue #280](https://github.com/wildware-uk/clipped/issues/280) is where it is
  applied.

So **there is no `HighlightProvider` trait**. One would be the contract for
plugins linked into the recorder, which is the model that was rejected, and an
abstraction with a single implementation whose only real use is the thing it is
meant to prevent (AGENTS.md section 1). The four operations are all here, as a
lifecycle rather than a vtable:

| SPEC.md section 22 | Here | When |
| --- | --- | --- |
| `supports(process)` | `InstalledPlugin::supports` | Answered from the manifest, before anything runs |
| `attach(session)` | `PluginSupervisor::attach` | Starts the process, writes `attach` to it |
| `events()` | `EventReceiver` | A bounded queue the recording drains |
| `detach()` | `PluginSupervisor::detach` | Writes `detach`, closes its input, kills it if it stays |

The cost is paid honestly: a process per plugin, a pipe, and a line of JSON per
event. What it buys is a contract that a plugin written in any language can
meet, and a failure mode for every kind of misbehaviour that ends at a queue.

The decision belongs in an ADR as well as here —
[issue #279](https://github.com/wildware-uk/clipped/issues/279) — because an ADR
is where a contributor looks before proposing in-process plugins again.

## The manifest

`plugin.json`, in the plugin's own directory:

```json
{
  "contract": 2,
  "id": "counter-strike-2",
  "name": "Counter-Strike 2",
  "version": "0.1.0",
  "description": "Reports kills, deaths and rounds from Game State Integration.",
  "executable": "clipped-cs2-plugin.exe",
  "supports": { "executables": ["cs2.exe"] },
  "network": [
    {
      "class": "loopback",
      "direction": "listen",
      "endpoint": "127.0.0.1:3212",
      "purpose": "receives Counter-Strike 2 game state"
    }
  ],
  "filesystem": [
    {
      "scope": "game-installation",
      "access": "write",
      "purpose": "writes the Game State Integration configuration Counter-Strike 2 reads at start-up"
    }
  ]
}
```

| Field | What it is |
| --- | --- |
| `contract` | Which version of *this* contract the plugin was written against. Not the event schema version; see [Versioning the contract](#versioning-the-contract). |
| `id` | Who it is — and the `source` every event it reports is stamped with, so a mark on a timeline is traceable to the plugin that made it. The syntax is `EventSource`'s, and `clipped` is refused. |
| `name`, `description` | What the user is shown. One line each, bounded, no control characters: a manifest is another program's data and it is rendered. |
| `version` | The plugin's own version, for the user and for a bug report. Clipped never compares two of them, because Clipped does not update plugins. |
| `executable` | One file name, inside the plugin's own directory. Not a path: `..\..\Windows\System32\cmd.exe` would make a plugin directory a way to run anything on the machine under a name the user consented to. |
| `supports` | The executables this plugin has an integration for, compared without regard to case. |
| `network` | What it will do with the network. Absent means none, which means none is permitted. |
| `filesystem` | What it will do with the filesystem, beyond running its own executable from its own directory. Absent means none, which means none is permitted — the same rule `network` follows. Contract 2 and later only; see [Filesystem access, and what changed in contract 2](#filesystem-access-and-what-changed-in-contract-2). |

Two rules about reading it are worth stating because they are the **opposite**
of the event model's:

- **An unknown field refuses the whole manifest.** A stored event is read
  leniently because refusing destroys something a user cannot get back. A
  manifest is a permission document, and a build that ignored a field it had not
  learned would run a plugin under a narrower declaration than the plugin was
  written to — a user consenting to the part of it this build happened to
  understand.
- **A `contract` this build does not speak is reported as exactly that**, and
  before anything else in the file is interpreted. A manifest written against a
  later contract will very likely also carry an unknown field, and sending
  somebody to look for a typo in a file that is simply newer than their Clipped
  is the wrong error.

`supports` is answered here rather than by asking the plugin, which is the one
place this contract deviates from SPEC.md's shape in substance rather than in
form. Starting a process to ask whether it cares about Notepad would mean every
launch on the machine starting every installed plugin; and a question answered
in a file is one the user can see the answer to.

## Network access, consent, and what enforcement can promise

[privacy.md](privacy.md)'s plugin section is the policy; this is how it is
implemented.

**Declared.** Each grant is a class (`loopback` or `outbound`), a direction
(`listen` or `connect`), an endpoint and a one-line purpose. The class has to
match the endpoint: a `loopback` grant naming `0.0.0.0` is refused, because
binding a wildcard address exposes the socket to the local network and
privacy.md calls that "outbound access wearing a disguise". An `outbound` grant
naming `127.0.0.1` is refused too — a declaration the user learns to distrust is
worse than none.

**Rendered as sentences**, not a permissions grid. `NetworkAccess::summary`
produces one line per grant:

```text
Listens on 127.0.0.1:3212 (this machine only) — receives Counter-Strike 2 game state
```

**Consented to as a value.** `ConsentToken` is the canonical text of a
declaration, and it is what the user's consent is recorded as. An
`InstalledPlugin` cannot be started; `InstalledPlugin::enable` takes the token
and returns an `EnabledPlugin` only if it still matches what the plugin
declares. So a plugin that adds outbound access in an update stops being
startable until the user is asked again — privacy.md's "the consent lapses",
enforced by a type rather than by a check somebody has to remember. Grants are
sorted into the token, so reordering a manifest does not lapse consent and
changing one does.

**And what that does not promise.** A child process can call the operating
system whatever its manifest says. `NetworkAccess::ENFORCEMENT` is the sentence
the plugin manager shows, and it does not overstate the position:

> Clipped shows what a plugin declares and refuses to start one whose
> declaration has changed since you allowed it. It cannot yet stop a plugin from
> using the network in ways it did not declare.

Making that stronger is [issue #280](https://github.com/wildware-uk/clipped/issues/280):
a job object or an AppContainer around the child, which is possible *because* a
plugin is a process. When it lands, that constant and this section change
together.

## Filesystem access, and what changed in contract 2

`plugins/dota2` writes a configuration file into the user's Dota 2 installation
directory, because that is how Valve's Game State Integration is configured.
Contract 1 had no typed way to say so — the disclosure lived in the manifest's
`description`, which is honest but cannot be validated, cannot be summarised
the way `NetworkAccess::summary` is, and does not lapse consent the way a
change to `network` does
([issue #343](https://github.com/wildware-uk/clipped/issues/343)). Contract 2
adds `filesystem`, the same shape as `network` and consented to alongside it:

```text
Writes to the game's own installation directory — writes the Game State
Integration configuration Counter-Strike 2 reads at start-up
```

**The scope is a closed enumeration, not a path.** A manifest naming a path it
wants to write to would be a string the host has to trust and cannot check: it
has no way to confirm that the string a plugin supplied really is the game's
installation directory, or a directory at all, without asking the plugin —
which is exactly the thing being declared. `FilesystemScope` is
`game-installation` or `plugin-data` instead, so a declaration is checkable and says
what a user needs to know rather than a string that means nothing until the
plugin runs. `FilesystemAccessLevel` is `read`, `write` or `read-write`. Both
are validated and rendered the same way `network`'s grants are
(`clipped_plugins::FilesystemAccess`).

**It is a contract version bump**, for the reason
[Versioning the contract](#versioning-the-contract) states of every new
manifest field: a build that has never heard of `filesystem` would refuse a
manifest that used it as malformed JSON rather than as "needs a newer
Clipped". `ContractVersion::is_supported` reads a declared contract as "at
most this build's own" rather than an exact match, specifically so that this
bump costs nothing to a manifest that has not touched the new field — a
plugin still declaring `"contract": 1` and no `filesystem` is read exactly as
it always was, and its `ConsentToken` does not move
(`clipped_plugins::ConsentToken::of`). Only a manifest that starts using
`filesystem` has any reason to declare `"contract": 2`.

**It is not enforcement.** Everything [above](#network-access-consent-and-what-enforcement-can-promise)
says about a network declaration applies here without a word changed: a
plugin is a separate process, and a separate process can open any file its
user account can reach whatever its manifest says. Declaring, validating and
consenting to a filesystem grant is the vocabulary and the consent surface —
what a plugin says it needs, shown to the user before they agree to it — and
it is not a sandbox. [Issue #280](https://github.com/wildware-uk/clipped/issues/280)
is where a plugin is held to *either* declaration by an AppContainer or a job
object, and it applies to both at once, because the mechanism does not care
which syscall it is confining.

**What this is not.** It does not hand a plugin a directory to use.
[Issue #381](https://github.com/wildware-uk/clipped/issues/381) is the other
end of the same subject: giving a plugin the game's installation directory and
a per-plugin state directory in `attach`, instead of letting it go and find
them, which is a change to the wire and not to the manifest. The two do not
conflict — a plugin can declare that it writes to `game-installation` under
this field while still having to locate that directory itself until #381
lands — but #381 is what would make the declaration and the capability line
up exactly, since a plugin that is *handed* the one directory it declared it
would write to is a plugin whose declaration a sandbox could hold it to
precisely. Building #381 is out of scope here.

**Status.** The type, its validation, its rendering and its effect on the
consent token are implemented in `crates/plugins` and covered by tests. The
bundled plugins have not adopted it yet: `plugins/dota2/plugin.json` and
`plugins/cs2/plugin.json` still carry the disclosure in `description` alone,
because moving them to `"contract": 2` and a typed `filesystem` grant is a
change to `plugins/`, outside `crates/plugins`, and is left as a follow-up
tracked on #343 rather than bundled into the contract change itself.

## Discovery

`clipped_plugins::discover` reads a plugins directory and returns two lists:
what was installed, and what was refused **and why**. Nothing is skipped
silently (AGENTS.md section 15) — a user who dropped a plugin into that folder
and cannot see it needs to be told that its manifest names an executable that is
not there.

The directory is `plugins` inside Clipped's own per-user directory —
`%LOCALAPPDATA%\Clipped\plugins` on Windows — which is the same per-user
directory the log, the encoder's capability cache and the user's own game
catalogue live in. `clipped-recorder watch` reads it once, when it starts, and
says what it found: reading it again every second would be the filesystem
polling AGENTS.md section 18 rules out, and a plugin that appeared while a game
was being recorded is not one that run has consent for anyway.

Directories are read in sorted order, so two runs of the same machine produce
the same list. Two plugins declaring the same identifier are not both loaded:
the first in that order keeps it and the second is refused, because every event
either of them reported would be attributed to the same name and a user
disabling one would have no way to tell which.

## The lifecycle

```text
 recording session                     supervisor                    plugin process
 ─────────────────                     ──────────                    ──────────────
 attach(enabled, session) ──────────▶  spawn ─────────────────────▶  {"report":"hello"}
                                       reader thread  ◀───────────   {"report":"event",…}
 drain the inbox  ◀───────────────────  bounded queue
 poll(now)        ──────────────────▶  exited? silent? flooding?
                                       kill / replace / disable ──▶
 detach()         ──────────────────▶  detach, then close its input
```

The session's arrow points one way. It hands over a plugin and then drains a
queue; it never calls into a plugin and is never given the chance to.

Everything time-based — a plugin that has not introduced itself, one that has
gone quiet, one whose replacement is due — happens in `PluginSupervisor::poll`,
which the owner calls with a clock reading, **about once a second, from a thread
that is not the capture thread**. There is no supervision thread: a state
machine over a clock reading the caller supplies is testable without waiting for
anything, which is the same shape and the same reasoning as
`clipped_session::automatic` ([sessions.md](sessions.md)). One thread per
*running plugin* is unavoidable and is the one reading its output, because a
pipe has no timed read.

**The owner is `clipped_session::plugins::SessionPlugins`**
([#338](https://github.com/wildware-uk/clipped/issues/338)), which is the thread
the left-hand column of that diagram runs on: a recording's own thread neither
attaches, drains nor polls. It starts when the recording's first frame fixes the
epoch, because that is where the timeline an event is placed on begins — see
[Timing](#timing) — so a recording that never captured a frame starts no plugin,
and one plugin session belongs to one *recording* rather than to one session.
`SessionPlugins::finish` is `detach`, followed by polling until every plugin has
gone or the stop grace has passed; the longest a recording can spend ending its
plugins is that grace plus one poll, whatever the plugins do.

A supervisor that is not polled costs a recording nothing. Events keep arriving,
the queue keeps bounding them; what does not happen is a hung plugin being
reclaimed.

## The wire

One JSON object per line, in both directions. The host writes commands to the
plugin's standard input; the plugin writes reports to its standard output.

```text
host  → {"command":"attach","contract":1,"session":{"session":"2026-08-11-cs2",
         "process":{"executable":"cs2.exe","process_id":4242}}}
plugin→ {"report":"hello","contract":1}
plugin→ {"report":"event","kind":"kill","ago_ns":480000000,"precision_ns":100000000,
         "confidence":1.0,"data":{"weapon":"ak47"}}
plugin→ {"report":"alive"}
plugin→ {"report":"problem","message":"Counter-Strike 2 has no gamestate_integration file"}
host  → {"command":"detach"}
```

| Report | Meaning |
| --- | --- |
| `hello` | The first thing a plugin says, carrying the contract version it speaks. Checked against the manifest as well, because an update can replace an executable without its manifest. |
| `event` | Something happened. The fields are the event model's, minus the two the plugin does not own. |
| `alive` | Nothing has happened and the plugin is still there. Required more often than the silence timeout: a game can go a minute without an event, and a host that read silence as health could not tell a quiet plugin from a deadlocked one. |
| `problem` | Something is wrong that the user can act on. Surfaced rather than logged and forgotten (AGENTS.md section 45). |

**A plugin is told the session's identifier and the process, and nothing else.**
Not the window title, not the command line, not where recordings are being
written. A plugin needs enough to find the game's own interface; the rest is
somebody's private machine.

**There is no `source` field on the wire.** The host stamps it from the
manifest, so a plugin cannot attribute a mark on a timeline to `clipped`, to
another plugin, or to a game it is not integrating. That is not a check that can
be forgotten; it is a field that does not exist.

**A plugin may not claim a word in the project's vocabulary.** An event `kind`
this build does not define and that carries no namespace is refused —
`kill_streak` is refused, `acme-cs2.kill_streak` is accepted. This is
deliberately the opposite of the read path, where an unrecognised kind out of a
database is kept: a stored event cannot be told, and a running plugin can.
Refusing costs one event; the plugin keeps running.

`detach` is followed by the plugin's standard input being closed, which is the
same message twice: a plugin that never reads a command still reads end of file,
and so learns that the host has gone even when the host went without saying
anything.

## How long ago, not when

**A plugin never reports a position on the session's timeline.** It reports
`ago_ns`: how long before writing the line the thing happened, measured on its
own clock.

The session's timeline is the capture clock's, which a separate process does
not have. The two ways to bridge that are a shared wall clock or a duration, and
the duration wins on every count: two processes reading the same wall clock
disagree by whatever NTP did in between, a clock step during a session moves
every subsequent event, and a plugin on a machine whose time zone changed
reports events an hour into the future. A duration measured inside one process
against one clock has none of those failure modes.

So the host reads its own clock when the report arrives, subtracts `ago_ns`, and
that is the event's `at`. The same number is its `latency` — how much later than
the moment it describes the report arrived — because that is exactly what it
measures. One number from the plugin fills in both, and neither can be a claim
the plugin did not make: a plugin reporting what it hears as it hears it sends
`ago_ns: 0` and gets an event at the moment it was heard, which is honest,
rather than an event at a moment it guessed.

`precision_ns` is required and has no default, for the reason
[Timing](#timing) gives: zero is the claim "I timed this exactly", and a plugin
that never made that claim must not start making it by leaving a field out.

`SessionTimeline` is the one place the conversion happens. It holds a reading of
the recorder's monotonic clock taken beside the capture epoch, which is a third
copy of the session's timeline and is bounded exactly as `crates/events`
bounds its own — one conversion, in one named function, until
[issue #253](https://github.com/wildware-uk/clipped/issues/253) extracts the
shared time crate.

## What a misbehaving plugin costs a recording

One sentence: **a recording never calls a plugin.** It drains a bounded queue,
and everything else about a plugin happens on threads it does not wait for.

| It | Costs a recording | Because |
| --- | --- | --- |
| crashes | nothing | it is another process; it is replaced, with a widening delay, a bounded number of times |
| hangs | nothing | nothing waits on it; after the silence timeout — counted from its `hello` — it is killed, and the thread reading it ends with it |
| floods | a bounded queue and a counter | delivery never blocks, a drain never returns more than one queue's worth, and the plugin is stopped |
| prints rubbish | a counter | unreadable lines are counted against a budget, and an over-long line is discarded without being allocated |
| is late | nothing | an event that arrives after the moment it describes has been written to disk is still placed where it belongs (`RecordedSpan`) |
| lies about its timing | nothing it cannot be checked on | it reports how long ago, and `precision` and `latency` are separate, explicit fields |
| claims to be something else | nothing | the host stamps the source |

Two of those bounds are worth spelling out, because they are the ones that were
wrong in the first draft and were found by breaking the tests:

- **A drain returns at most one queue's worth**, rather than looping until the
  queue is empty. A plugin delivering faster than that loop runs would otherwise
  keep the recording inside it for as long as it kept producing — the stall a
  bounded queue exists to prevent, reintroduced by the code that reads it.
- **Killing a plugin does not wait for it.** It terminates it and reaps it if
  that takes no time at all, which it does; a process that somehow outlives its
  own termination is picked up by a later poll instead of holding the thread
  that asked.

**What is dropped is counted and reported.** `InboxStats::dropped` and
`PluginHealth::dropped` are how a session knows its timeline is incomplete, and
a timeline that is missing marks has to say so rather than look complete
(AGENTS.md section 27).

## Supervision and restart

| Trouble | Restarted? | Why |
| --- | --- | --- |
| It exited on its own | yes | It may have hit something transient: a game that had not finished starting, a port briefly taken |
| It went silent | yes | Same, and the plugin is killed first |
| It never introduced itself | yes | Reported separately from a hang, because "it cannot start" and "it stopped answering" have different answers |
| It flooded | **no** | A replacement floods the same queue a second later, and what is being lost is the events this subsystem exists to record |
| Its output was unreadable | **no** | Same reasoning |
| It speaks another contract version | **no** | It will speak the same one next time |

**The silence timeout starts at `hello`, not at start-up.** Until a plugin has
introduced itself there is one question — has it started? — and the start-up
timeout is the budget for it. A plugin that has never spoken has not *gone*
quiet, and a host that judged the same interval by both numbers would charge a
slow start to whichever of the two was smaller: on a busy machine, a plugin the
operating system was still loading would be reported as one that hung
([issue #405](https://github.com/wildware-uk/clipped/issues/405)).

Restarts are bounded, widen, and reset: three attempts by default at one, two
and four seconds, and the counter resets once a plugin has run for a minute. A
plugin that fails permanently is left stopped **and says why**, which is visible
and leaves the user an action (AGENTS.md sections 16 and 45); a plugin that
fails once an hour is not permanently disabled by teatime, which matters for a
recorder that runs for days.

Every reason a plugin was stopped is a `PluginTrouble`, which renders as a
sentence: "the plugin said nothing for 10s and was stopped", "the plugin
reported events faster than they could be recorded, and 137 were lost before it
was stopped".

## Writing a plugin

`crates/plugins/examples/example_plugin.rs` is a complete plugin in one file,
and the crate's tests install it as a real plugin and run it under a real
supervisor. A real integration differs only in where the events come from.

1. **Read the `attach` command** from standard input. It carries the session's
   identifier and the process that started.
2. **Say `hello`**, with the contract version you were written against.
3. **Print an event** whenever something happens, saying how long ago it
   happened.
4. **Say `alive`** while nothing is happening, more often than the host's
   silence timeout — from a thread of its own if the plugin's own work can block
   for longer than that.
5. **Exit when standard input closes.** That happens when the session ends, and
   also when the host does.

A plugin written in Rust can use `clipped_plugins`'s own types
(`read_command`, `write_report`, `hello`) so that it is not hand-building JSON
this crate is about to parse. A plugin written in anything else prints the same
objects itself; nothing in the contract requires Rust, and the reason the wire
is line-delimited JSON rather than something more efficient is precisely that.

Install it by putting the executable and a `plugin.json` in a directory under
the plugins folder.

**Testing one.** `crates/plugins/examples/misbehaving_plugin.rs` is the other
half of the reference: a plugin that panics, hangs, floods, prints rubbish or
claims a contract from the future, depending on the name it is installed under.
The supervisor's tests run it and time every turn of a simulated recording loop
while it misbehaves. A plugin under development can be run by hand:

```text
cargo run -p clipped-plugins --example example_plugin
{"command":"attach","contract":1,"session":{"session":"by-hand","process":{"executable":"cs2.exe","process_id":1}}}
```

## What a plugin may not do

AGENTS.md section 34 is absolute, and it is a rule about a user's game account
rather than about code quality:

> **No DLL injection. No reading or writing another process's memory. No code
> injection. Nothing that resembles an anti-cheat bypass.**

Permitted: official APIs, local telemetry, game logs, Game State Integration,
documented IPC, supported replay files, and the game's own local endpoints. A
plugin in this repository that reaches for anything else is not merged. A plugin
outside it that does is a plugin whose users risk a ban for a highlight, which
is never a trade worth making — and no amount of richer detection changes that
arithmetic.

`SPEC.md` section 24 allows OCR as a last resort for games with no interface at
all. It is not forbidden here, and it is not what any of the three planned
integrations use.

## Versioning the contract

`ContractVersion` versions the contract — the manifest's shape, the wire, and
the lifecycle — and is deliberately **not** `clipped_events::SchemaVersion`,
which versions events. A stored event outlives every build that reads it; a
running plugin is negotiated with once, at start-up. Tying them together would
mean a plugin that added a wire message forcing a migration of every event in a
user's library.

There have been two contract versions: 2 added `filesystem`
([Filesystem access, and what changed in contract 2](#filesystem-access-and-what-changed-in-contract-2)).
Within a version:

- Adding a field to a *report* costs nothing: unknown fields on the wire are
  ignored, as [ipc.md](ipc.md) sets out for the control protocol.
- Adding a field to a *manifest* is a version bump, because unknown fields there
  are refused. That asymmetry is the price of a manifest being a permission
  document, and it is the intended cost: a build that cannot read the whole
  declaration should refuse the plugin rather than run it on less.

A plugin declaring a contract this build does not speak is not started, and the
message says which is behind.

**A bump costs nothing to a manifest that does not use the new field.**
`ContractVersion::is_supported` accepts any declared version up to and
including the one this build speaks, not only an exact match, because a
version bump only ever *adds* to a manifest's vocabulary — it never removes or
repurposes a field an earlier manifest relied on. A plugin whose manifest has
not changed since contract 1 keeps declaring `"contract": 1` forever, and this
build reads it exactly as it always did; only a manifest that starts using a
field contract 1 did not have — `filesystem` today — has a reason to declare
`"contract": 2`. This is why the bump that added `filesystem` did not have to
touch every bundled manifest to keep them working, though bundled manifests
that go on to declare filesystem access do need to say so
(see [Status](#filesystem-access-and-what-changed-in-contract-2)).

## The League of Legends plugin

`plugins/league`, [issue #72](https://github.com/wildware-uk/clipped/issues/72).
The first integration written against the contract above, and the answer to
what a plugin actually looks like once the contract stops being theoretical.

**The interface is Riot's own.** League serves a **Live Client Data API** over
HTTPS on `127.0.0.1:2999` while a match is running: a documented, supported,
read-only local endpoint. Nothing else is touched. AGENTS.md section 34 is
absolute about that, and it is a rule about a user's account rather than about
code quality — a plugin that read the game's memory to find a better kill feed
would be trading somebody's ranked account for a highlight.

**What it reports:**

| League says | The recording gets |
| --- | --- |
| `GameStart` | `match_started` |
| `GameEnd` | `match_ended`, and `win` or `loss` from its `Result` |
| `ChampionKill` | `kill`, `death` or `assist`, depending on which name in it is the player's |
| `TurretKilled`, `DragonKill`, `BaronKill`, `Multikill`, `Ace`, `FirstBlood`, and the rest | nothing, deliberately — see below |

Each event carries the game's own fields as its payload — `KillerName`,
`VictimName`, `Assisters`, `EventTime` — including fields from a patch this
build has never seen, because `data` is opaque above the plugin and filtering it
to the fields this build happens to know would be deciding what a future build
may read. `EventID` and `EventName` are dropped: the first is an index into a
list that only exists inside League, and the second is what the `kind` was
derived from, and keeping it would invite a consumer to switch on it.

The objectives are left out because each is a decision about the *shared*
vocabulary rather than a line of code. A dragon is not a `goal` in the sense
another game's plugin would mean by it, and inventing
`league-of-legends.dragon_killed` commits the project to a custom name before
anybody has asked for one ([Custom events](#custom-events)). They are already
read, indexed and timed; adding one is a match arm and a test.

### What is different about a polled API

Counter-Strike 2 pushes; League is asked. Three things follow, and they are the
whole of what is interesting here.

**A poll that misses a window loses nothing.** League's event list is
*cumulative and indexed*: every poll returns the whole match, each entry with an
`EventID` that never moves. So the state the plugin keeps is the identifier
after the last one it reported. A poll that took ten seconds returns ten seconds
of events, and none of them is lost. This is the property that makes polling
acceptable at all, and `plugins/league`'s tests drive three payloads of one
match through one watch to hold it.

**The same property is a hazard for a restarted plugin.** A plugin that exited
or went silent is restarted ([Supervision and restart](#supervision-and-restart)),
and the replacement is a new process with a cursor at zero attached to a
recording that already has marks on it. Its first poll carries the whole match,
and reporting it would put a second copy of every kill, death, assist and
`match_started` on the timeline. So the cursor is not the only thing that
decides what is reported: an event is reported only if it happened **after the
watch started observing**, which the match clock in the same payload answers
without any memory of a previous process. That also stops a session which began
recording part way through a match drawing the first ten minutes of it onto ten
seconds of video.

The cost is stated rather than implied: the events that happen during the second
or two a restart takes are **lost**, because nothing was watching for them.
That is the better direction to fail in — a timeline with two of every kill is
wrong in a way nobody can repair afterwards. In the ordinary case it costs
nothing at all, because the plugin is attached to `League of Legends.exe` and
that process starts before the match clock does.

The cursor needs one more thing beside it, and it is the part that is easy to
get wrong: **a second match through the same attachment starts the identifiers
again**, so the cursor has to be rewound. The list going backwards is the
obvious signal and it is not sufficient — it only fires while the new match has
fewer events than the old one had, and a first poll that lands a few minutes
into the second match would see identifiers past the cursor and quietly skip
everything below them. The match clock cannot go backwards inside a match, so a
clock that has is a different match whatever the identifiers say. Both are
checked, and there is a test for the case only the second one catches.

**The poll interval is a cost, and it is a small one.** A second, on a machine
that is also running League (AGENTS.md section 18). What it buys and what it
costs are not what one might assume: the interval does **not** affect where an
event is drawn, because an event's position comes from the match clock in the
same payload rather than from when this process noticed. Polling twice as slowly
does not make a mark twice as wrong. What it affects is how quickly anything
could react, and it bounds `latency` — and for a replay buffer measured in
minutes that is nowhere near mattering. The interval is set for the reporting to
feel prompt, and could be several times longer without losing an event.

**The two times come from one request.** `/liveclientdata/eventdata` would be
the smaller request, but an event's time is match-relative and the recording's
timeline is not, so turning one into the other needs the match clock *as it was
when that list was produced*. Two requests would give a list from one instant
and a clock from another, and the gap would be an error in every event's
position that nothing downstream could see. `allgamedata` gives both from the
same instant, for a few tens of kilobytes of JSON a second.

### The certificate, and what "for that endpoint only" means

League's certificate is signed by Riot's own authority, which is not in Windows'
trust store, and is not issued to `127.0.0.1`. A client that validated it in the
usual way would fail every request, so the certificate errors are ignored —
**on the request handle, for that request, and nowhere else**. It is not a
change to any trust store, it is not set on the session, and it cannot affect
another request in this process, let alone another process.

The plugin uses **WinHTTP**, the operating system's own HTTP stack, which is
what makes that scoping possible without a TLS crate and its dependency graph
for one loopback request a second. It also opens the session with
`WINHTTP_ACCESS_TYPE_NO_PROXY`, which is what makes the manifest's `loopback`
declaration true rather than nearly true: a machine with a system proxy
configured must not be able to send this request off the machine
([privacy.md](privacy.md)).

**The request also refuses to be redirected**, and that is not a detail beside
the certificate exception — it is what keeps the exception meaningful. WinHTTP
follows redirects by default, and its default policy permits `https` to `https`,
so a listener on port 2999 answering `302 Location: https://somewhere.else`
would take this request off the machine *with certificate validation disabled*,
under a manifest that declares loopback and nothing else. Every request
therefore sets `WINHTTP_OPTION_REDIRECT_POLICY` to
`WINHTTP_OPTION_REDIRECT_POLICY_NEVER` on the same handle, which makes a `3xx` a
status code the plugin reads rather than an address it goes to. A test stands
two loopback listeners up, has the first answer with a redirect to the second,
and asserts the second is never connected to.

What is left over is worth saying rather than glossing: because the certificate
is not checked, the plugin cannot prove that the thing answering on port 2999 is
League. So the body is treated as hostile input — bounded in size, bounded in
how long it may take to arrive, parsed leniently, and never used for anything
but producing marks on a timeline. The request carries no body, no cookie and no
authorisation header, so there is nothing there to give away.

### Which name in an event is the player

League has spent years moving from summoner names to Riot IDs, and which of them
the event list uses has changed with it. The plugin holds every name the payload
offered for the active player — `riotId`, `riotIdGameName`, `summonerName` — and
matches an event's name against all of them. The one piece of leniency is for a
client that reports the player as `Rosalind` while its events say
`Rosalind#EU1`: when no alias carries a tag, a tagged name is compared without
one.

It is deliberately not the other way round. Two players in one match can share a
game name and differ only by tag, so stripping the tag off an event's name when
the full Riot ID is known would trade a certain answer for an ambiguous one —
and the event it got wrong would be a kill attributed to the person who died.

When the payload names nobody at all — spectating, or a client that stopped
reporting those fields — kills cannot be told from deaths, and the plugin
**says so once** and keeps reporting the match events that do not need a name.
An integration that silently reported nothing would look exactly like one that
was working (AGENTS.md section 45).

### When it goes wrong

| What happens | What the plugin does |
| --- | --- |
| Nothing is listening on 2999 | Nothing. This is what a loading screen looks like, and what every machine that is not in a match looks like. After a minute of it, one line saying so |
| The endpoint answers "no game" | Nothing at all — the API is there, the match is not |
| The answer is not a payload this build can read | Counts it. After five in a row, one line saying a League patch may have changed it |
| One entry in the event list cannot be read | Skips that entry, counts it, and reports the ones either side |
| An event name this build has never seen | Ignored. The rule is a match arm, not a table of known names |
| A `Result` that is neither `Win` nor `Lose` | The match ends without a verdict. Guessing between the two from a word nobody has seen would be inventing the outcome of somebody's match |

### What has not been verified

Stated plainly, because this is the gap between the tests and the claim:

- **No match has been recorded through it.** League is not installed on the
  machine it was written on, so the payloads in `plugins/league/tests/fixtures`
  were constructed from the published shape of the API rather than captured —
  which `plugins/league/tests/fixtures/README.md` says in as many words. They
  prove the derivation; they cannot prove the shape matches the client on any
  particular patch.
- **The successful request has never run.** `tests/plugin_contract.rs` starts
  the real executable and holds it to the contract — it says `hello`, keeps its
  heartbeat while the API is unreachable, and leaves when its standard input
  closes — and on a machine with no match in progress that exercises WinHTTP up
  to `ERROR_WINHTTP_CANNOT_CONNECT` and no further. A local HTTPS server with a
  self-signed certificate is not something to spin up in a unit test on a
  machine that is also running a game.

Whoever has League installed can close both gaps in a few minutes:

```text
cargo run -p clipped-league-plugin
{"command":"attach","contract":1,"session":{"session":"by-hand","process":{"executable":"League of Legends.exe","process_id":1}}}
```

Start a match — a practice tool game is enough — and the events appear on
standard output as they happen. Saving the body of one
`GET https://127.0.0.1:2999/liveclientdata/allgamedata`, with the other nine
players' Riot IDs replaced ([privacy.md](privacy.md)), and pointing a test at it
is what turns the first gap into a fixture.

## The reference plugin

`plugins/cs2` is Counter-Strike 2, and it is the first plugin written against
everything above. It is the reference because two more follow it and copy its
shape, so its shape is stated here rather than left to be inferred from a diff.
`plugins/cs2/README.md` is the user-facing half — what is written where, and how
to take it away; this section is the part a plugin author needs.

It is Counter-Strike rather than something more interesting for one reason:
**the game has an official answer.** Game State Integration is Valve's own
documented mechanism, a `.cfg` in the game's configuration directory asks the
game to POST a JSON snapshot of its state to a local port, and it posts. That is
exactly what [What a plugin may not do](#what-a-plugin-may-not-do) asks for, and
it means the reference plugin never has to demonstrate a compromise.

**The layout is the contract's, with nothing added:**

```text
plugins/cs2/
    plugin.json            what it is, what it supports, what it will do with
                           the network
    src/main.rs            the plugin protocol loop, and three subcommands
    src/derive.rs          state snapshots into events
    tests/payloads/        payloads to test against, and no game
```

Four things in it are worth copying, and one is worth arguing with.

**The manifest is asserted against the code.** `plugin.json` declares
`127.0.0.1:3212`; `integration::DEFAULT_PORT` is what gets bound; a test
compares them. A declaration is what the user consents to before the plugin may
run, so a declaration that has drifted from the socket is consent for something
that is not happening. The same test checks that the manifest names the binary
Cargo actually builds, because a manifest naming an executable that is not there
is a plugin the host refuses at discovery and a mistake a rename makes silently.
What that test bounds is the **default**: `install --port` can still point the
game somewhere else, and nothing compares the two at run time, so
`plugins/cs2/README.md` says to edit the manifest to match. Enforcing it needs
somewhere to enforce it from
([issue #281](https://github.com/wildware-uk/clipped/issues/281)).

**It says `hello` before it does anything that can fail.** Introducing itself
and then reporting a `problem` is a plugin the host can tell the user about;
failing before the handshake is a plugin that "never introduced itself", which
is a different message and a less useful one. When the integration is not set
up, that is exactly what happens: `hello`, then a `problem` naming the command
to run, then exit — and the supervisor's bounded restart leaves it stopped with
the reason showing.

**It is tested as a process, not only as a function.** `tests/plugin_process.rs`
copies the built binary into a directory laid out like a plugin, runs its
`install` subcommand against a directory laid out like Counter-Strike 2, starts
it the way a supervisor does, POSTs payloads at the port it opened and reads the
events it prints. Everything else in the crate tests a function, and every unit
test passes when the token is written in one format and read in another.

**Its fixtures say where they came from.** The payloads in `tests/payloads/` are
constructed against the documented shape, not captured from a running game,
because the game was not installed on the machine it was written on, and the
directory's README says so in the first paragraph. A fixture that quietly claims
to be a recording is a test that claims more than it proves (AGENTS.md section
27).

The thing to argue with is that **a plugin has to remember where it installed
something**, and there is nowhere obvious to put that. A plugin is told the
game's executable *file name* and its process identifier, deliberately and not
its path ([The wire](#the-wire)), so a running plugin cannot find the game's
installation directory and therefore cannot find the file it wrote there —
which is where its port and its token live. `plugins/cs2` writes one line beside
its own executable naming that path, and nothing else: the port and the token
are read back out of the configuration file itself, so there is exactly one copy
of each and no way for two records to disagree about what the game was told. It
is the plugin's own state in the plugin's own directory, which is not the
application's configuration store (AGENTS.md section 30) — but the next two
integrations will meet the same problem, and if all three solve it separately
that is a shared facility waiting to be extracted.

## Writing into somebody else's game

Installing Game State Integration means **writing a file into the user's game
directory**, and that is the part of this plugin with the least code and the
most judgement in it. `docs/privacy.md` governs it, and three rules fall out.

**It is never a side effect.** Nothing is installed when the plugin is attached
to a session, or when the game launches, or when the plugin is enabled. It is a
command the user runs — `clipped-cs2-plugin install <game folder>`, or
`clipped-dota2-plugin install` — and until they do, an attached plugin reports a
problem naming that command and stops. A plugin that wrote into a game directory
because a game started would be doing something nobody asked for.

This was a rule stated of one plugin and contradicted by the other for a while:
`plugins/dota2` wrote its configuration on attach and then reported that the game
had to be restarted. Both behaviours were defensible — one asks first, and the
other cannot report anything at all until the file exists — and
[#382](https://github.com/wildware-uk/clipped/issues/382) settled it on the
first, for two reasons that outlast the convenience.

It is the shape that survives the sandbox
([#280](https://github.com/wildware-uk/clipped/issues/280)): a sandboxed plugin
cannot write into a game directory at all, so an explicit, user-initiated,
one-off install is the only arrangement that still works once that lands.
Writing on attach would have to be undone at that point.

And it keeps the privacy register honest. "The user ran this command" is a fact
[privacy.md](privacy.md) can point at; "the plugin was attached" is not one the
user participated in, and the register exists to make the two comparable.

**Exactly what is written is documented, and it is one file.**
`gamestate_integration_clipped.cfg`, in `game\csgo\cfg`, holding a loopback URI,
four timing values, a token and six `data` subscriptions.
`plugins/cs2/README.md` reproduces it in full. Six subscriptions rather than the
dozen available is the same instinct as the rest of this document: data the
plugin does not need is data it does not need to be handling.

**Removal is a command, and deletion is equivalent.** `uninstall` removes that
one file; deleting it by hand does the same job and breaks nothing. A user who
cannot get rid of something without the tool that put it there does not really
have a choice about it.

Then there is the part that is specific to Counter-Strike and general in
principle: **the directory is shared.** Counter-Strike loads *every*
`gamestate_integration_*.cfg` it finds, which is how several tools coexist in
one game. So the plugin writes under a name of its own, refuses to replace a
file of that name it did not write — recognised structurally, by the service
name at the top of the document, not by a comment anybody could copy — and
refuses to install onto a port a neighbouring file already posts to, naming that
file. Two integrations on one port means one of them silently receives nothing,
and being the tool that does that to somebody is worse than failing to install.

The listening socket is loopback, and [privacy.md](privacy.md) does not treat
that as a free pass: a loopback port is reachable by every process on the
machine, including a page open in a browser, so every payload has to carry the
token from the configuration file and one that does not is refused before
anything about it is believed. The token is generated from the operating
system's random number generator at install time, and there is no fallback — a
token from a worse source would be a check that looks like authentication and is
not.

## Deriving events from state

This is the substance of a Game State Integration plugin, and it is the part the
[integration table](#how-the-three-planned-integrations-map) compresses into the
word "difference".

Game State Integration reports **state**. There is no kill message: there is a
match statistics block whose `kills` was 8 a moment ago and is 9 now. Every
event `plugins/cs2` reports is a difference between two payloads, and every way
it could go wrong is a way of getting a difference wrong. One rule holds it
together:

> **An event is reported only for a transition the plugin observed directly,
> between two payloads it accepted.**

Five consequences, each of which is a test in `plugins/cs2/src/derive.rs`, and
each of which a plugin for any state-reporting game will need its own version
of:

- **The first payload reports nothing.** It is a baseline. A plugin attached to
  a game already three rounds into a match knows the score and knows nothing
  about how it got there; a `match_started` at the moment it happened to look
  would be a mark on a timeline where nothing happened.
- **A payload older than the last one accepted is discarded whole.** Each post
  is a separate connection to a loopback port, so two can arrive out of order,
  and a difference measured against a *newer* baseline is a negative number of
  kills — or, once the next payload lands, the same kills counted twice. The
  game's own timestamp is the only ordering information a payload carries; the
  plugin uses it, and accepts payloads stamped in the same second in arrival
  order, because within a second there is nothing to order by and pretending
  otherwise would be a guess.
- **A counter that goes backwards is not a negative event.** A new match, a
  warm-up ending, rejoining: all reset the game's counters. A decrease means the
  plugin's baseline is wrong, so the baseline is replaced and nothing is
  reported for that step.
- **A payload about somebody else is not about the player.** Dying in a
  competitive match moves the camera to a teammate and the payload follows the
  camera. The plugin compares the payload's Steam identifier against the one the
  game is running as, and a mismatch means the block is neither reported *nor
  taken as a baseline* — adopting it would produce a spurious decrease the
  moment the camera came back.
- **A field that cannot be attributed is left off.** A kill carries
  `"headshot"` only when one kill happened in the step and the round's headshot
  counter moved by one alongside it. Two kills with one headshot between them
  says nothing about which, so neither event claims it. The same reasoning
  removes `weapon` entirely: the payload carries the weapon held when it
  arrived, which after a kill is very often the next one.

**And the timing.** A payload says a counter changed; it does not say when. What
the plugin knows is that the change happened after the previous payload it
accepted and no later than this one, so `at` is the **middle of that window**
and `precision` is **half of it** — which is exactly the claim
[Timing](#timing) asks a source to make. Every event derived from one payload
carries the same moment, because two kills in one step really are two kills the
plugin cannot separate and giving them different times would be inventing an
order.

The one place this costs something is honesty about a quiet game: with nothing
changing, the window widens to the game's heartbeat interval and the precision
widens with it. That is the correct number rather than a flattering one, and a
highlight rule padding a clip by `precision` is the reason it matters.

## The Dota 2 plugin, and what it shares with Counter-Strike 2

`plugins/dota2` ([#73](https://github.com/wildware-uk/clipped/issues/73)) is the
third plugin written against this contract and the **second** over Game State
Integration. Its own README says what it reports and what it does to the
machine; this section is the part that is about **the next** plugin rather than
about Dota.

Dota 2 and Counter-Strike 2 use the same mechanism. A KeyValues file in the
game's own directory names a local address, a set of components and a token; the
game POSTs a JSON state to that address whenever it changes. Everything in that
sentence except *what is in the JSON* is identical for the two games — so
`plugins/cs2` and `plugins/dota2` now hold **two independent implementations of
the same plumbing**: a KeyValues writer, a hand-rolled loopback listener with its
request bounds, a shared secret generated and checked on every payload, and the
install of one `gamestate_integration_*.cfg`. That is the duplication AGENTS.md
section 55 exists to prevent, and it is written down here rather than left to be
discovered.

**It is deliberately not extracted by this change.** Lifting a module out from
under two branches that were both open would have conflicted with whichever
merged second, and the extraction is worth doing once, against both callers, by
somebody who can see what the two actually have in common rather than what one
of them guessed. Where it belongs is argued on
[#69](https://github.com/wildware-uk/clipped/issues/69) with a recommendation —
`crates/gsi`, linked by plugin binaries and not by the recorder — and that
issue is where the move happens. It could **not** live in `crates/plugins`: that
crate is the host side and is linked into the recorder, and a listening socket
and a writer of files inside a game's installation are two things ADR 0002 keeps
out of the process that is recording.

Each plugin is split so that the extraction stays a move rather than a rewrite.
In `plugins/dota2` the boundary is drawn where the games actually differ:

| Half | Knows about | Would a second Valve integration reuse it? |
| --- | --- | --- |
| `plugins/dota2/src/gsi` | The socket, the configuration file, the auth token, and how often payloads arrive | **All of it.** It names no Dota type and reads no Dota field |
| `plugins/dota2/src/dota` | What `DOTA_GAMERULES_STATE_POST_GAME` means, and which counter is a kill | **None of it**, and it should not try |

The second row is the honest half. A “configurable state differ” that both games
could point at would be a small language for describing state machines, written
so that two files of rules could avoid being two files of Rust — which is the
over-engineering AGENTS.md section 1 warns about, arriving in the disguise of
reuse. Dota's game rules states and Counter-Strike's rounds and phases are
different *concepts*, not different *values*.

**Two properties of Game State Integration that any plugin over it inherits**,
both of which cost a user something and neither of which is a bug to be fixed:

- **The configuration file is read when the game starts.** A plugin that writes
  it during a session has configured the *next* session. The Dota plugin says so
  in a `problem` — “Restart Dota 2 for it to take effect — this recording will
  not have any” — rather than leaving the user watching a timeline that never
  gains a mark (AGENTS.md section 45).
- **The token has to outlive the plugin process.** The game holds whatever token
  the file had when it started, so a plugin that generated a new one on every
  attach would refuse every payload of the match in progress. It is generated
  once and kept in Clipped's own directory.

**Where the two plugins answer the same question differently, and it is not
settled.** [Writing into somebody else's game](#writing-into-somebody-elses-game)
states installation as a rule — never a side effect — and `plugins/cs2` follows
it: nothing is written until the user runs `clipped-cs2-plugin install`.
`plugins/dota2` writes its file when the host attaches it, and reports that the
game has to be restarted for it to matter. Both are defensible, and one of them
is wrong for a project that ships both:
[#382](https://github.com/wildware-uk/clipped/issues/382) is where they are made
to agree, with a recommendation on it. It is recorded here rather than settled
by the change that found it, because settling it changes a plugin's behaviour.

## What is not built

Stated plainly, because the gap between this document and the running
application is the thing most likely to be misread (AGENTS.md section 7):

- **A recording starts the plugins the settings file enables**
  ([issue #282](https://github.com/wildware-uk/clipped/issues/282)), and no
  others. Nothing *writes* that file for you yet
  ([issue #281](https://github.com/wildware-uk/clipped/issues/281)), so a build
  whose settings nobody has hand-edited starts none. The
  *wiring* exists as of
  [issue #338](https://github.com/wildware-uk/clipped/issues/338):
  `clipped_session::plugins` creates the supervisor, attaches the plugins it is
  given, polls it once a second on a thread of its own and stops them when the
  recording ends, and `clipped-recorder watch` drives it. What it is given is an
  empty list, because starting a plugin needs an `EnabledPlugin` and the only
  thing that produces one is the consent the user recorded against its
  declaration — which nothing stores yet. A recording therefore names the
  installed plugins that claim the game it is of and says they are not enabled,
  rather than starting them uninvited; `docs/privacy.md` is why that is not a
  detail. So `plugins/league`, `plugins/cs2` and `plugins/dota2` still run only
  when they are started by hand.
- **What a plugin reports is kept, and it is now drawn**
  ([issue #71](https://github.com/wildware-uk/clipped/issues/71),
  [issue #65](https://github.com/wildware-uk/clipped/issues/65)). A recording
  drains its plugins' events at `SessionPlugins::take_events`, and
  `apps/recorder/src/watch.rs:1143` hands them to
  `SessionManager::record_game_events`, which writes them to the session's
  sidecar; the library indexer turns those into `game_events` rows, and
  `library_events` answers with them placed in the recording's own file. The
  playback screen draws them on the recording's timeline and seeks to one when it
  is pressed (`apps/desktop/src/RecordingTimeline.tsx`,
  [desktop-ui.md](desktop-ui.md)) — which is what gave `readEvents` its first
  caller outside a test.

  What is still missing is the *Editor's* end: it cannot open a clip
  (`apps/desktop/src/Shell.tsx`, issue #306), so its own event lane still has
  nothing to draw on.

  This entry previously said the events were "counted in the log and go no
  further", and then that "the marks reach no screen". Both stopped being true.
- **Nothing installs a plugin.** `plugins/league`, `plugins/cs2` and
  `plugins/dota2` each build an executable and have a `plugin.json` beside it;
  putting the two in a directory under the plugins folder — `plugins` inside
  Clipped's per-user directory, `%LOCALAPPDATA%\Clipped\plugins` on Windows — is
  a manual step, and there is no packaging step that does it.
- **The reference plugin's fixtures are constructed, not captured.**
  `plugins/cs2/tests/payloads/` was written against the documented Game State
  Integration payload, because Counter-Strike 2 was not installed on the machine
  that wrote it. The derivation is therefore tested against the shape it was
  told about and not against the shape the game produces, which is the
  outstanding half of
  [#70](https://github.com/wildware-uk/clipped/issues/70)'s first acceptance
  criterion. The directory's README says how to take a real capture.
- **Neither Game State Integration plugin has been verified against the game it
  integrates.** `plugins/dota2`'s tests run against constructed sample payloads
  too, because nobody who has worked on it has Dota 2 installed
  ([#73](https://github.com/wildware-uk/clipped/issues/73) records what that
  does and does not prove, and `plugins/dota2/fixtures/README.md` says how to
  take a real capture). A plugin can be shown to parse, diff, bound and report
  correctly without the game; it cannot be shown to be reading the right fields.
- **Which plugins are enabled, and the consent each was enabled with, are
  stored** ([issue #282](https://github.com/wildware-uk/clipped/issues/282)) --
  in the configuration API rather than here, because a plugin crate with its own
  settings file would be the second configuration store AGENTS.md section 30
  warns about. The token is kept as legible text, so a person reading their own
  settings can see what they agreed to; a plugin whose declaration no longer
  matches is refused and reported rather than started.
- **Nothing shows any of it**
  ([issue #281](https://github.com/wildware-uk/clipped/issues/281)). The
  sentences a user reads before enabling a plugin exist and are tested; the
  screen that shows them does not, so a user cannot yet see a plugin's network
  declaration in the application.
- **No sandbox** ([issue #280](https://github.com/wildware-uk/clipped/issues/280)).
  See above; the wording the user is shown says so.
- **The bundled plugins do not yet declare filesystem access as a typed
  field.** [Filesystem access, and what changed in contract 2](#filesystem-access-and-what-changed-in-contract-2)
  adds the vocabulary; `plugins/dota2/plugin.json` and `plugins/cs2/plugin.json`
  still carry the one thing each writes into a game's own directory in
  `description` alone, because moving them to it is a change in `plugins/`
  rather than `crates/plugins` and is left as the remaining step of
  [issue #343](https://github.com/wildware-uk/clipped/issues/343).
- **No ADR** ([issue #279](https://github.com/wildware-uk/clipped/issues/279)).
  The decision is argued here in the meantime.
