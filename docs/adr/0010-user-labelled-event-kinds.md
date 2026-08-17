# 0010. A user-labelled event kind carries its own text, and a host subsystem gets its own source

- Status: Accepted
- Date: 2026-08-16
- Issue: [#345](https://github.com/wildware-uk/clipped/issues/345)

## Context

`clipped_events::EventKind` is a closed set — `kill`, `match_started`, and the
rest of SPEC.md section 21's list — plus [`Custom`], which is how a plugin
says something the closed set does not cover. A custom name is namespaced,
`acme-cs2.flag_captured`, precisely so two plugins cannot collide and so a
plugin can never pre-empt a name the vocabulary has not yet defined. Neither
of those was built with the *user* in mind. Three examples motivate this
record, and they do not all want the same thing:

- An input binding the user has given a name — "my ultimate" — becomes an
  event when it fires.
- A fingerprint match the user typed a name for becomes an event when it is
  recognised again.
- A salience peak the host detects on its own — no user text involved — is
  still host-produced and still needs a source distinguishable from the input
  bindings and the fingerprint matcher.

None of the three is a plugin's word: they are produced by Clipped itself,
from something a person typed into Clipped's own interface, or from nothing a
person typed at all. Building on `Custom` would mean inventing a namespace
nobody asked the user for. Leaving the closed vocabulary to grow one variant
per feature would mean the vocabulary — which SPEC.md section 21 and
`crates/events/src/kind.rs` both describe as *what the application acts on
generically* — absorbing "my ultimate," which is a fact about one person's
keybinds, not a game concept.

Two questions had to be settled, both raised directly by issue #345:

1. **Where does the label itself live** — the actual text "my ultimate," not a
   description of it?
2. **How is the *source* of a host-produced event told apart from another
   host-produced event** — a mark from the input bindings from a mark from
   the fingerprint matcher — given `EventSource::application()` is one fixed
   string, `clipped`, today?

Two constraints bound the answer, both from the issue directly:

- **It has to survive being written and read back, intact.** Not normalised,
  not slugged: whatever a person typed is what a person reads back, because
  `crates/events` is the vocabulary shared by the library's database and a
  recording's sidecar, and a kind that changes meaning between sessions is
  the failure this whole crate exists to prevent.
- **Every user-supplied string in this workspace is bounded**, because it is
  displayed and stored (`crates/plugins/src/manifest.rs`,
  `crates/plugins/src/report.rs::MAX_PROBLEM_BYTES`).

Not in scope: how a user types a label into an interface (no interface exists
yet), how the session or the library actually produces one of these events,
and how a highlight rule decides what a user-labelled kind is worth — all of
that is `crates/session` and `crates/library`'s work, on top of what this
record settles. Also not in scope: extending the plugin wire protocol
(`crates/plugins/src/report.rs`) itself, though its producer boundary is a
direct consequence of this decision and is called out under Consequences.

## Decision

**A user-labelled kind is its own [`EventKind`] variant, `UserLabelled`,
carrying a `UserLabel` whose wire form is `user:<the text a person typed>` —
a reserved prefix, not a namespace.**

**A host subsystem gets its own [`EventSource`], `clipped.<component>` —
`clipped.input`, `clipped.fingerprint` — through
`EventSource::application_component`, and the whole `clipped` namespace,
not only the bare word, is now reserved to the host.**

In full:

- `UserLabel::new` takes the bare text a person typed and validates it: not
  empty, at most 200 bytes (the figure `docs/bookmarks.md`'s bookmark label
  already uses, so the two user-facing label fields agree rather than
  disagreeing for no reason), and no control characters, because it is
  displayed as written. It does **not** apply `CustomName`'s
  lowercase-ASCII-segment rule — the label is prose, not an identifier a
  program compares, so upper case, spaces, punctuation and non-ASCII
  characters all survive.
- The wire form is `user:<label>`, stored as `EventKind`'s ordinary bare-string
  form (`#[serde(from = "String", into = "String")]`, unchanged). `user:` can
  never be produced by `CustomName::new` (it contains no dot, so it fails the
  namespace requirement outright) and never collides with a standard tag
  (none of them contain a colon), so a reader can always tell the three apart
  by inspection, without a table.
- `EventKind::from(String)` checks the `user:` prefix before falling through
  to `CustomName`'s namespace check, and on a label that fails validation —
  empty, too long, a control character — keeps the whole tag as
  `Unrecognised` rather than repairing it. This is the same read-path rule
  `crates/events` already applies to a malformed custom name: a stored event
  is not deleted to enforce a rule it has already broken.
- `EventSource::application_component(component)` builds `clipped.<component>`,
  where `component` is one segment (no dot — this names one part of the
  host, not a hierarchy) validated by the same syntax `CustomName` and
  `EventSource::plugin` already use.
- `EventSource::plugin` now refuses any identifier whose *namespace* — not
  only whose whole string — is `clipped`. Before this record, `clipped.cs2`
  was an accepted plugin identifier and was explicitly tested as such; that
  was a gap this record closes, because a plugin claiming a name under the
  reserved namespace could put a mark on a timeline the user reads as
  Clipped's own, which is exactly what `EventSource::plugin` refusing
  `clipped` itself was for.
- `EventSource::is_application` now answers `true` for `clipped.<anything>` as
  well as for the bare `clipped`, because a named component *is* Clipped,
  not a third thing between "the application" and "a plugin."

## Alternatives

### A reserved dot-namespace, `user.<label>`

The shape the issue itself offers as an example. It reuses `CustomName`
outright — reserve the namespace `user` the way `clipped` already is, and a
user-labelled kind is a `Custom` name under it.

Rejected because `CustomName`'s syntax is an identifier's, not prose's: each
segment must start with a lowercase ASCII letter and continue with lowercase
letters, digits, `-` or `_`. "my ultimate," "My Ultimate!," and a fingerprint
name with an accent in it are not valid segments. The only way to make this
alternative work is to slug the text — lower-case it, replace the space, drop
the punctuation and the accent — and at that point the label a person reads
back is not the label they typed. The acceptance criterion is that the label
round-trips *intact*; a lossy transform fails it by construction, however
convenient the resulting syntax is for the rest of the vocabulary. Keeping the
raw text and *also* offering a slug would be two representations of one
label, and this crate does not need a second one to satisfy anything issue
#345 asks for.

### A single fixed kind, with the label in the event's `data` payload

Give `UserLabelled` no inner value at all — a closed variant like `Kill`,
always serialising to one constant string — and put the text a person typed
in [`EventPayload`] instead, the way a game's own vocabulary already travels
there.

This was close, and it is not obviously wrong: `data` is exactly where
free-form, source-specific detail is supposed to live, and it needs no new
syntax in `kind.rs` at all.

It was rejected for two reasons. First, `EventPayload`'s whole point,
documented at its definition, is that *nothing above the plugin interprets
it* — the payload is the game's own words, ignorable by everything else. A
user-labelled event's text is the opposite: it is the one thing every
consumer — a timeline, a search index, a title — exists to show. Splitting an
event's *identity* across two fields, one of which every other kind of event
promises to ignore, invites exactly the bug this crate's tests are built to
catch: a consumer that reads `kind` for grouping and `data` for display,
and forgets that this one kind is the exception. Second, it throws away the
one property a bare-string `kind` already has for free: two labels are two
different wire strings, comparable, sortable and searchable without
unpacking JSON, in the same way `acme-cs2.flag_captured` already is.

**What would make it win**: if a user-labelled kind turns out to need
several independent fields — a label *and* a category *and* a colour, say —
`data` is where the second and third belong regardless, and at that point a
single fixed kind carrying all of it in the payload stops being a workaround
and becomes the obviously right shape. Nothing in issue #345 asks for that
yet.

### Let the user's own text be the `CustomName` namespace

Treat a person's label as if it were a plugin's identifier: `EventSource`
already accepts a dot-namespaced string, so let the user's chosen word be the
namespace of a `Custom` name they invent.

Rejected outright, and for the reason constraint 1 states directly: a
plugin's namespace exists to stop two plugins colliding, which only works
because a namespace is an *identifier* — assigned once, compared, looked up.
A person typing a label is not choosing an identifier; two different users
who both bind an ability called "ultimate" would collide under this scheme,
and a person should not have to invent a namespace to name their own keybind
in the first place. This is the same objection as the first alternative,
sharper: it does not even offer the dot-namespace's syntactic tidiness in
return.

### A closed enumeration of host components, in `crates/events`

For the source-identity half: instead of an open, namespaced string
(`clipped.input`, `clipped.fingerprint`), give `EventSource` a closed Rust
enum of known host components, the way [`SchemaVersion`] is a closed
enumeration rather than a bare integer.

Rejected on layering. `crates/events` is a leaf crate specifically so that
neither the plugin surface nor the session or library needs to be understood
to read it (`lib.rs`'s "Position in the architecture" section). A closed list
of host components would need every future one — an input binding watcher, a
fingerprint matcher, whatever comes after — added to this crate before the
crate that implements it could use it, which is exactly the coupling the
open, syntactic `CustomName` rule was built to avoid for plugins. The
namespaced-string approach costs nothing to extend: a new component is a new
call to `application_component`, in the crate that owns it, with no change
here.

## Consequences

- **`crates/events` can name a user's label and a host component; nothing yet
  produces either.** No code in this crate, or reachable from it, calls
  `EventKind::UserLabelled` or `EventSource::application_component` outside
  the type's own tests. Wiring an actual input-binding or fingerprint
  subsystem to produce one is `crates/session`'s work and is not part of
  #345's scope.
- **The plugin producer boundary needs a matching change, and does not have
  it yet.** `crates/plugins/src/report.rs::ReportedEvent::into_event` refuses
  a report whose kind is `EventKind::Unrecognised`, on the grounds that an
  unnamespaced word a plugin invents could pre-empt a future standard tag.
  It does not yet refuse `EventKind::UserLabelled`, because `EventKind` has
  no producer boundary of its own — by design, the same as `Custom` — and the
  boundary for what a *plugin* may claim belongs to whichever crate parses a
  plugin's report. Until `into_event` is updated to refuse a `UserLabelled`
  kind exactly as it refuses `Unrecognised`, a plugin sending
  `"kind":"user:something"` would be accepted rather than refused. This is
  flagged rather than fixed here because `crates/plugins` is outside this
  crate's boundary; it is the first thing whoever wires a producer for these
  events should close, and a test written against `report.rs` that fails if
  the check is missing is the acceptance criterion issue #345 already names.
- **Closing the `clipped.*` plugin-identifier gap is a behaviour change, not
  only an addition.** A manifest declaring `"id": "clipped.something"` was
  previously accepted by `EventSource::plugin` and is refused after this
  record. No shipped plugin uses a `clipped`-namespaced identifier (checked
  against `plugins/dota2` and every test in this workspace), so nothing
  breaks in practice, but a reviewer should read this as tightening existing
  validation rather than as new validation with no prior behaviour.
- **What to watch**: whether 200 bytes is enough for a real label. It is the
  figure already shipped for a bookmark's label, so this is not a new number
  being guessed at; if it turns out wrong, the fix is `docs/bookmarks.md`'s
  problem too; and it should be moved and fixed once, not diverge into two
  numbers that happen to have started the same.

[`Custom`]: https://github.com/wildware-uk/clipped/blob/main/crates/events/src/kind.rs
[`EventPayload`]: https://github.com/wildware-uk/clipped/blob/main/crates/events/src/event.rs
[`SchemaVersion`]: https://github.com/wildware-uk/clipped/blob/main/crates/events/src/schema.rs
