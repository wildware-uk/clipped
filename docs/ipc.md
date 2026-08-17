# The recorder control protocol

Clipped is two processes. The recorder owns every recording and is expected to
run for days; the desktop application is a window the user opens, closes and
occasionally crashes, and none of those three may touch a recording
([ADR 0002](adr/0002-separate-recorder-process.md)). This document is the
protocol between them: what they talk over, what they say, and what happens when
the two are not the same age.

It is a specification rather than a description of code. Where the two disagree
the code is wrong, and the crate-level documentation in `crates/ipc/src/lib.rs`
is the map from one to the other.

**Status.** The transport, the framing, the handshake, the compatibility policy
and the command and event vocabulary are implemented and tested.
`clipped-recorder serve` speaks it. Four of the commands belong to subsystems
that do not exist yet and are refused with a typed "not in this build" error;
they are listed, with the issue that builds each, in
[Commands this build cannot perform](#commands-this-build-cannot-perform). The
messages exist in TypeScript as well, checked against the Rust on every build —
see [The TypeScript types](#the-typescript-types).

The desktop application attaches to a recorder or starts one, subscribes to its
status, and shows what it is told
([ADR 0006](adr/0006-recorder-lifetime-and-supervision.md)). It also **drives**
it now: the notification-area menu starts and stops recordings and asks the
recorder to exit ([issue #50](https://github.com/wildware-uk/clipped/issues/50),
`docs/desktop-ui.md`). Those commands are sent from the Tauri host in Rust —
nothing crosses the webview boundary as a request yet, which is
[issue #217](https://github.com/wildware-uk/clipped/issues/217) and is a
different thing.

Related: SPEC.md section 5, AGENTS.md sections 5, 27, 43 and 44,
[privacy.md](privacy.md), [ADR 0002](adr/0002-separate-recorder-process.md),
[ADR 0005](adr/0005-named-pipe-control-protocol.md),
[ADR 0006](adr/0006-recorder-lifetime-and-supervision.md).

## Contents

- [Shape of a conversation](#shape-of-a-conversation)
- [Transport](#transport)
- [Endpoints](#endpoints)
- [Framing](#framing)
- [Serialisation](#serialisation)
- [Connections and roles](#connections-and-roles)
- [The handshake](#the-handshake)
- [Compatibility policy](#compatibility-policy)
- [Commands](#commands)
- [Commands this build cannot perform](#commands-this-build-cannot-perform)
- [Events](#events)
- [Errors](#errors)
- [When something goes wrong](#when-something-goes-wrong)
- [Trying it by hand](#trying-it-by-hand)
- [The TypeScript types](#the-typescript-types)
- [How it is tested](#how-it-is-tested)
- [What is deliberately not here](#what-is-deliberately-not-here)

## Shape of a conversation

```text
 desktop application                          recorder
 ───────────────────                          ────────
 open \\.\pipe\clipped-recorder.<session> ──▶ accept
 hello  { protocol_version, role }        ──▶ check the version
        ◀── welcome { features }  |  refused { supported: [...] }
 request { id, command, params }          ──▶ dispatch
        ◀── response { id, outcome }

 (a second connection, role "events")
 hello  { protocol_version, role: events, streams } ──▶ subscribe
        ◀── welcome
        ◀── event { status_changed }        the state as it is now
        ◀── event { … }                     whenever it changes
```

Every arrow is one frame. The recorder never waits for the desktop application:
if nothing is connected it records anyway, and a command that arrives during a
recording is answered on a connection thread rather than on the thread doing the
capturing.

## Transport

**A Windows named pipe**, created with an explicit access-control list and with
`PIPE_REJECT_REMOTE_CLIENTS`. The alternative seriously considered was a TCP
socket on loopback. [ADR 0005](adr/0005-named-pipe-control-protocol.md) records
the decision in full; this is the comparison it turns on.

| | Named pipe | Loopback socket |
| --- | --- | --- |
| Who can reach it | Only accounts the pipe's DACL names — here, the user who created it | Every process on the machine, including other signed-in users and a web page in a browser |
| Authentication needed | None: the operating system enforces the ACL before a byte is read | A shared secret, which has to be stored somewhere both ends can read and nobody else can |
| Name | A string the two sides agree on; a per-session name cannot collide | A port, which can be taken by something else, blocked by a firewall, or handed to the wrong process after a restart |
| Recorder not running | `CreateFile` fails immediately with "the endpoint does not exist" | `connect` fails, or worse, succeeds against whatever else took the port |
| Recorder dies mid-request | The client's next read fails with a broken pipe, at once | The socket resets — equivalent — but a stale listener on the port is indistinguishable from the real one |
| Two recorders | `FILE_FLAG_FIRST_PIPE_INSTANCE` makes the second one fail at once | Both may bind with `SO_REUSEADDR`, or the second silently fails |
| [privacy.md](privacy.md) | Explicitly **not** network communication | **Is** network communication: it must be declared, documented in the register, and authenticated |

The privacy row is the one that settles it rather than merely favours it.
`docs/privacy.md` classifies a loopback listener as network communication, and
requires anything Clipped listens on to authenticate its callers and to appear
in the register. A control channel that can start and stop recordings and read
what is being recorded is exactly the sort of listener that rule exists for. A
named pipe reaches the same end — a channel only this user can use — through the
operating system's own access control, with nothing to declare and no secret to
store. **This change adds no network communication of either class.**

### What the security descriptor does and does not promise

The pipe is created with the SDDL `D:P(A;;GA;;;<the user's SID>)`: a protected
DACL granting everything to the account the recorder is running as, and nothing
to anybody else. The SID is read from the process token rather than assumed, so
it is correct for a local account, a Microsoft account and a domain account
alike.

Without that descriptor Windows applies a default that grants read access to
`Everyone`, which would let a process running as a different signed-in user read
the recorder's replies. With it, they cannot open the pipe at all.

What it does not promise: `SYSTEM` and members of the Administrators group can
take ownership of any object and rewrite its ACL. This keeps other *users* out.
Nothing on Windows keeps an administrator out, and this document does not
pretend otherwise.

`PIPE_REJECT_REMOTE_CLIENTS` is the second half. Named pipes are reachable over
SMB by default — `\\machine\pipe\name` from another computer is the same object
— and that flag refuses any client that did not come from this machine. Without
it the "not network communication" claim above would be false.

### Who the client is talking to

The descriptor above answers "who may open this pipe". It does not answer the
other direction — "whose pipe is this?" — and this document should say so rather
than leave it implied.

The endpoint name is predictable: `clipped-recorder.<session>`, where the session
identifier is not a secret. Any process running as the user can create that name
before the real recorder does, and be what the desktop application connects to.
`FILE_FLAG_FIRST_PIPE_INSTANCE` does not prevent that; it protects the *server*,
so the genuine recorder finds the name taken and exits saying another recorder is
already listening rather than half-serving it. A client has nothing equivalent:
there is no authentication in either direction, by design, because the operating
system's access control is the authentication.

`Client::recorder_process_id` — `GetNamedPipeServerProcessId` — says *which*
process is serving the connection, and is not an exception to any of that. It is
what a supervisor uses to tell a recorder it started from one that was already
there, and it reports a squatter's identifier as readily as the genuine
recorder's. It answers "who am I talking to", not "is this the right one".

**Under the threat model this transport is built for, that costs nothing.** The
threat is another *user* on the machine, and the DACL keeps them out. A process
running as this user could already send the real recorder any command, read its
replies, or terminate it; squatting the name buys it nothing it could not do
more simply. It would matter if the recorder ever ran as a different account —
a service, say — and that is the change that would require the client to verify
the server rather than assume it.

What the client does do is ask for the smaller of the two grants Windows offers
it. `open_client` opens the pipe with
`SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION`, so the server can find out
who connected and no more. Without those flags Windows applies
`SECURITY_IMPERSONATION`, which would let whatever holds the server end act as
the connecting process elsewhere on the machine — a capability nothing in this
protocol needs, and one worth not handing to a pipe whose name anything running
as the user can take.

### Not Windows

`clipped-ipc`'s messages, framing and dispatch are platform-independent and are
unit-tested anywhere. The transport is not: on a non-Windows build every
constructor returns "this build has no local transport", the same way
`clipped-session` refuses to record. A Linux port would add a Unix domain
socket with `SO_PEERCRED` beside it, which has the same properties as a named
pipe; nothing above `crates/ipc/src/transport/` would change.

## Endpoints

An endpoint is a **name**, never a path. The `\\.\pipe\` prefix is added by
`Endpoint::path`, so a name from a configuration file or a command line can
never be pointed at `\\some-server\pipe\…` — the one shape of this transport
that genuinely would be network communication.

```text
default   clipped-recorder.<session>
path      \\.\pipe\clipped-recorder.<session>
override  clipped-recorder serve --endpoint <NAME>
```

`<session>` is the Windows sign-in session the recorder is running in. The pipe
namespace is machine-wide, so without it two people signed in at once — one at
the keyboard and one over Remote Desktop, or one machine after a fast user
switch — would be racing for a single name. The access-control list would stop
them reading each other's messages; it would not stop the second recorder
finding the name taken.

A name may contain ASCII letters, digits, `-`, `_` and `.`, and may be 1 to 200
characters. Anything else is refused with the character that caused it.

`--endpoint` exists so that a second recorder can run beside the one that starts
at login: a development build, or a test that must not connect to the recorder
the person at the keyboard is using. Every test in
`apps/recorder/tests/ipc_protocol.rs` names an endpoint of its own for exactly
that reason.

## Framing

```text
┌──────────────┬────────────────────────────────┐
│ length: u32  │ payload: `length` bytes of JSON │
│ little-endian│                                 │
└──────────────┴────────────────────────────────┘
```

A pipe is a byte stream: two messages can arrive in one read and one message can
arrive in three. The length prefix is what makes a message boundary explicit.
Nothing about the payload delimits it — JSON has no terminator, and a newline is
legal inside a JSON string, so a reader that looked for one could be confused by
a window title.

**The maximum frame is 1 MiB, and the limit is checked before anything is
allocated and before a single payload byte is read.** A length prefix is an
instruction from the other end of a pipe to allocate memory, and the recorder is
the process that must not fall over (AGENTS.md section 17). A peer that declares
four gigabytes gets a refusal and a closed connection; it does not get four
gigabytes. The largest message this protocol has is a few hundred bytes, so the
limit is a bound on damage rather than a budget: a frame anywhere near it means
something is wrong.

A malformed frame — bad JSON, or an oversized prefix — ends the connection after
the refusal is written. There is no resynchronisation, because a peer that
cannot frame correctly has no defined position in the stream to recover to.

## Serialisation

**JSON.** The reasons, in the order they mattered:

- **Both ends have to read it.** The other end of this protocol is a Tauri
  application whose front end is TypeScript, where JSON is the native
  representation and anything else needs a decoder written and kept in step.
- **Unknown fields are ignorable by construction**, which is the whole of the
  additive half of the [compatibility policy](#compatibility-policy). A format
  with a positional or schema-bound layout makes "add a field" a coordinated
  change to two programs that ship separately.
- **It can be read.** A protocol trace in a bug report is a sequence of legible
  lines rather than a hex dump, and the [`Trying it by hand`](#trying-it-by-hand)
  section below is possible at all.
- **The rate is human-scale.** A few messages a second, a few hundred bytes
  each. Compactness buys nothing measurable here, and AGENTS.md section 18's
  concern is the capture path — which this is deliberately not on.

Rejected: `bincode` and `postcard`, which are compact and fast and produce
opaque bytes whose meaning depends on field order, and which would need a
hand-written TypeScript decoder maintained in step with the Rust; MessagePack,
which is JSON's shape without JSON's legibility and adds a dependency to both
ends for a saving nobody here can measure; Protocol Buffers, which brings a
schema compiler and a build step into a project that currently needs neither, to
solve a versioning problem the handshake already solves.

**High-bandwidth data is not in scope for this protocol at all.** Live preview
frames, waveforms and thumbnails do not belong in JSON on a control channel, and
they get their own transport decision when something needs them
([ADR 0002](adr/0002-separate-recorder-process.md) says the same).

## Connections and roles

A connection declares in its handshake what it is for, and never does both.

| Role | Direction after the handshake | Used for |
| --- | --- | --- |
| `control` (default) | Client sends a request, recorder sends its response, in strict alternation | Commands |
| `events` | Recorder writes, client only reads | Status and error events |

The split is a transport decision showing through. A synchronous Windows file
handle serialises the operations issued against it, so a recorder that wanted to
push an event down a connection while a read was outstanding on the same handle
would need overlapped I/O and a completion loop to do it safely. Two
connections, each used in one direction at a time, buy the same thing for a
fraction of the machinery — and they cost the client one extra `CreateFile`.

A recorder serves **8 connections at once**. Beyond that it refuses with
`too_many_connections` and closes. The desktop application needs two; the rest
is room for a second window, a command-line client and a diagnostic tool. The
cap exists because the endpoint is reachable by anything running as the user,
and an unbounded accept loop is an unbounded thread count inside the process
that must not fall over.

## The handshake

The first frame on every connection is a `hello`. It is answered by exactly one
`welcome` or one `refused`, and a `refused` is followed by the connection
closing.

```json
{"type":"hello","protocol_version":2,
 "client":{"name":"clipped-desktop","version":"0.1.0"},
 "role":"control"}
```

```json
{"type":"welcome","protocol_version":2,
 "recorder":{"name":"clipped-recorder","version":"0.1.0"},
 "role":"control",
 "features":["recording","status_events","bookmarks","screenshots"]}
```

```json
{"type":"refused",
 "code":"unsupported_protocol_version",
 "message":"this recorder speaks protocol version 1, and 2 was asked for",
 "detail":{"detail":"unsupported_protocol_version",
           "requested":2,"supported":[1],"recorder_version":"0.1.0"}}
```

An `events` connection adds the streams it wants, and its `welcome` says which
it will get:

```json
{"type":"hello","protocol_version":2,
 "client":{"name":"clipped-desktop","version":"0.1.0"},
 "role":"events","streams":["status","errors"]}
```

**`hello` and `refused` are frozen.** Whatever else changes, those two shapes
stay readable by every version in both directions. They are the mechanism by
which two builds that agree on nothing else still manage to say so, and a
mechanism that can itself become incompatible is not one.

`features` is what a client checks before *offering* a control. A version number
says what a build can express; a feature says what it can do, and the two are
not the same — two recorders speaking protocol 1 can differ in what was compiled
into them. A UI that offers a button whose command will be refused has told the
user something untrue (AGENTS.md section 27), and `features` is how it avoids
that. Today: `recording`, `status_events`, `bookmarks`, `screenshots`,
`shutdown`, `library`, `export`, `playback`, `hotkeys`, `replay`, `settings`,
`microphone_level`, `startup`, `automatic`.

`automatic` is the clearest case of why a feature is not a version. Protocol 2
says a recorder can *describe* an automatic sitting; `automatic` says it
*makes* them. A recorder that serves a window and records only what it is asked
to speaks protocol 2 and reports `idle` for ever, and a window that drew
"Watching for games" from the version number alone would be saying something
untrue about a recorder that will never record on its own.

`shutdown` is announced by the *server* rather than by the recording engine
behind it, because it is the accept loop a shutdown ends and the accept loop
belongs to `clipped-ipc` (`crates/ipc/src/server.rs`). The others are the
application's own.

`library` is the one a window checks before it draws a library screen at all. A
recorder built before [`library_sessions`](#library_sessions) existed refuses it
with `unknown_command`, and without the check the window would have no way to
tell that from a library with nothing in it — which is exactly the confusion
those commands are shaped to avoid.

`export` is the same check in front of an Export control. A recorder built
before [`export_recording`](#export_recording) refuses it with
`unknown_command`, and the cost of finding that out late is worse than for a
library: the person has already chosen a file name for a file that was never
going to be written.

`playback` is the check in front of a player. A recorder built before
[`open_playback`](#open_playback) has no way to open a recording for playback and
refuses the command by name, and a window that had already drawn a transport
would be showing a control that cannot work.

`hotkeys` is the one where the two answers are *opposites*. A recorder built
before [`get_hotkeys`](#get_hotkeys) registers no global hotkey at all, so every
one of the user's keys does nothing; a recorder that answers with no conflicts
has registered them all cleanly. Both would be drawn as an untroubled list by a
window that did not check, which is the worst available reading of the same
empty screen.

`replay` is the check in front of a "Save Replay" control, and it is the one
with a second half. The feature says the *build* has
[`save_replay`](#save_replay) — a recorder built before it parses the command
and always refuses it with `not_implemented`, which reads plausibly enough that
nobody would question it. Whether the recording that is running has a buffer to
save from is `active_recording.replay_seconds`, because that is a property of
the recording rather than of the build: a window offering the control needs both
to be true.

`settings` says the build has all three of [`get_settings`](#the-settings),
`apply_settings` and [`get_audio_devices`](#get_audio_devices) — one build has
either all of them or none. It is the sharpest of these to get wrong: a recorder
built before issue #51 *has* an `apply_settings` command and refuses every call
to it with `not_implemented`, so a window that drew a form and checked nothing
would find out that none of it saves only when somebody pressed Save. Clipped's
own Settings screen reaches the same place from the other end — it asks
`get_settings` when it opens and draws no control until that is answered, so an
older recorder produces the refusal in place of the form rather than a form that
does not work.

`microphone_level` is deliberately *not* part of `settings`, and the reason is
what a window does without it. A window that cannot read the settings can say so
and offer nothing; a window that can read them but cannot measure a microphone
still has a working list of devices, so it must draw the chooser and leave out
the meter rather than refuse the whole screen. It is also the one capability a
build can lack for a reason other than its age: a recorder compiled without an
audio backend does not claim it.
`startup` says the build has both [`get_start_at_login`](#get_start_at_login)
and `set_start_at_login`. It is separate from `settings` because starting at
login is not a setting — it is a `Run` value Windows reads at sign-in rather
than a key in `settings.json` — and because a recorder built before issue #308
has every settings command and neither of these, so it refuses both with
`unknown_command`. That refusal and "Clipped does not start at sign-in" are
opposite answers, and a switch drawn without checking would show the second for
the first.

## Compatibility policy

This is the part that gets exercised in the field, because the recorder can be
running at login from a build the user has since updated. Four cases, four
different answers.

### An unknown protocol version: refused

The recorder accepts a version only if it is in the set it speaks. A version it
does not know — newer *or* older — is refused with
`unsupported_protocol_version`, and the refusal carries every version the
recorder does speak plus its own build version. Nothing is downgraded and
nothing is attempted.

That gives both directions a usable answer:

- **Newer UI, older recorder.** The UI sees `supported: [1]` against its own
  `2`, knows the recorder is behind, and can say so. It deliberately does *not*
  restart the recorder to fix it: the recorder that is too old may be recording,
  and the only way to replace it is to kill it. `clipped_ipc::supervisor` treats
  a refused version as a failure no retry can fix and reports it at once, with
  the recorder's own message ([ADR 0006](adr/0006-recorder-lifetime-and-supervision.md)).
- **Older UI, newer recorder.** The recorder was updated and restarted while an
  old UI was running. `supported: [2]` against its own `1` tells the UI it is
  the one that is behind, and the message names the recorder's version so the
  user can be told which side to update.

A recorder may speak more than one version at once, which is how a breaking
change is deployed without requiring both processes to be updated in the same
instant: ship a recorder that accepts `[1, 2]`, then a UI that asks for `2`,
then drop `1`. `SUPPORTED_PROTOCOL_VERSIONS` is the list, and it is the only
place that decides.

### An unknown field inside a known version: ignored

Receivers ignore fields they do not recognise, on every message. This is what
makes a version bump rare: adding a field to an event, a reply or a status
payload does not break a client compiled before it existed.

**The corollary is a rule, not a footnote.** Because unknown fields are ignored,
*a change to what an existing command does may never arrive as a new optional
field*. An older recorder would drop the field and report success for something
it did not do, which is the exact failure AGENTS.md sections 27 and 54 forbid —
and it would be invisible to both sides. Such a change is either:

- a **new command name**, which an older recorder refuses with
  `unknown_command`; or
- a **new feature name** in `welcome.features`, which the client checks before
  sending anything.

Adding a command, a field, an event, an error code, an end reason or a feature
does **not** increment the protocol version. Removing one, renaming one, or
changing what one means does.

A new **recorder state** is deliberately not on that list. `status` is a tagged
union with no catch-all, so a client that met a state it had never heard of
would fail to read the message rather than guess at it — and that is the wanted
behaviour, because the alternative is a window showing "idle" for a recorder
that is doing something else. An event a client cannot read is information it
does without; a state it cannot read is a lie it would otherwise tell. Adding
one is a version bump, and the handshake is then what stops the two ever
meeting.

### Version 2, which is that rule being applied

Protocol 2 is the first bump, and it is the case the paragraph above was
written for: `status` gained a third state, `watching` — a recorder that is
going to record the next game to start, which used to answer `idle` and so was
indistinguishable from one that will never record anything.

Three things came with it, and none of them would have needed a bump on their
own:

- `active_recording` gained `session`, the sitting a recording belongs to. That
  is where the **game** is; `target` is a capture selector, and a window cannot
  turn `process 4242` into "Counter-Strike 2" without the catalogue that lives
  in the recorder.
- `watching` carries the same `session`, because a game that exits keeps its
  sitting open for a restart grace period. A window reading the game off the
  *recording* would blank the name for those seconds and then bring it back.
- `session_ended` is a new event, carrying the sitting's files rather than an
  identifier to look up. It fires when the sitting ends, which may be before
  the library has indexed anything.

**`SUPPORTED_PROTOCOL_VERSIONS` is `[2]`, not `[1, 2]`,** which is a deliberate
departure from the transitional advice above — ship a recorder accepting both,
then a UI asking for the newer, then drop the older. That path is available
only when a recorder can honestly serve both versions at once, and this
recorder cannot. Serving version 1 would be a promise never to say `watching`,
and a watching recorder has no way to keep it: the state is not an optional
field it can omit, it is what the recorder *is* at that moment. A recorder that
answered `idle` to a version 1 client while watching would be telling that
client the one lie this whole change exists to stop.

So the two builds refuse each other at the handshake, in both directions, with
a message naming the versions and which side is behind. That is the wanted
outcome and the reason `hello` and `refused` are frozen.

### An unknown command: refused by name

`unknown_command`, naming the command. Deliberately not a parse failure: a
command name a recorder has never heard of is version skew the UI can report,
and a corrupt frame is a bug. They must not look the same, which is why a
request carries its command as a string and its parameters as an open object,
and why the typed dispatch happens after the envelope has been read.

### An unknown error code, error detail, end reason or event: kept

A client compiled against protocol 1 that meets an error code added later keeps
the code verbatim and shows the message. The same holds for an end reason, for
an error's machine-readable `detail`, and for an event: each is kept as it
arrived rather than failing the frame that carried it. The alternative — failing
to parse the frame — would turn "a refusal you have not seen before" into "the
connection is broken".

That is a stronger requirement than it first looks, and it is the one place this
policy has to be implemented rather than inherited. An unknown *field* costs
nothing to ignore, because ignoring it is what JSON deserialisation does by
default. An unknown *variant* is not free: a tagged union whose tag a build does not
recognise fails the whole message it is part of, so a `detail` invented later
would take its refusal's code and message down with it, and an event invented
later would end a subscription. Both types therefore keep a catch-all holding
the raw JSON — `ErrorDetail::Other` and `Event::Other` — which is what makes the
paragraph above true rather than aspirational. `crates/ipc`'s tests assert it in
both directions, including that a known message carrying a field added later is
still read as itself rather than falling into the catch-all.

An unknown *event stream* name is the exception, and it is refused: a
subscription that was accepted and then never delivered anything would be a UI
showing an empty panel with no explanation. The difference is that a stream is
something a client *asks for* and can be told about, while an event and a detail
arrive unannounced.

## Commands

Sent on a `control` connection:

```json
{"type":"request","id":7,"command":"get_status"}
```

```json
{"type":"response","id":7,"outcome":{"ok":{"reply":"status","status":{"state":"idle"}}}}
```

`id` is chosen by the client and quoted in the response. `params` may be omitted
when a command's parameters are all optional.

`status` is a tagged union on `state`, with three tags and no catch-all:

| `state` | Means | Carries |
| --- | --- | --- |
| `idle` | Nothing is being recorded, and nothing will be until something asks | nothing |
| `watching` | Nothing is being recorded, and the next game to start will be | `session`, when a sitting is waiting out its restart grace |
| `recording` | A recording is in progress | `recording_id`, `output`, `target`, `elapsed_ms`, and optionally `replay_seconds` and `session` |

`session` appears on both `watching` and `recording` and is the same shape in
each, so a window that wants to keep showing one game across the moment a
recording stops reads it from the status rather than matching on the state
twice. It is absent for a recording that is not part of a sitting.

| Command | Parameters | Reply | This build |
| --- | --- | --- | --- |
| `ping` | none | `pong` | yes |
| `get_status` | none | `status` | yes |
| `start_recording` | the `record` options, below | `recording_started` | yes |
| `stop_recording` | `recording_id` (optional) | `recording_stopped` | yes |
| `add_bookmark` | all optional, below | `bookmark_added` | yes |
| `take_screenshot` | all optional, below | `screenshot_taken` | yes |
| `save_replay` | all optional, below | `replay_saved` | yes |
| `library_sessions` | all optional, below | `library_sessions` | yes |
| `library_games` | none | `library_games` | yes |
| `library_events` | `recording` | `library_events` | yes |
| `library_trash` | none | `library_trash` | yes |
| `restore_from_trash` | `kind`, `id` | `restored` | yes |
| `empty_trash` | `items`, `bytes` | `trash_emptied` | yes |
| `plugins` | none | `plugins` | yes |
| `export_recording` | `source`, `destination` | `recording_exported` | yes |
| `open_playback` | `source`, `audio_track` (optional) | `playback_opened` | yes |
| `get_hotkeys` | none | `hotkeys` | yes |
| `get_settings` | none | `settings` | yes |
| `apply_settings` | `values`, below | `settings` | yes |
| `get_audio_devices` | none | `audio_devices` | yes |
| `get_microphone_level` | `microphone` | `microphone_level` | yes |
| `get_start_at_login` | none | `start_at_login` | yes |
| `set_start_at_login` | `enabled` | `start_at_login` | yes |
| `shutdown` | `finalise_recording` (optional) | `shutting_down` | yes |

### `save_replay`

Keeps the last few seconds of what is being recorded, as a clip
([#38](https://github.com/wildware-uk/clipped/issues/38)).

```json
{"type":"request","id":1,"command":"save_replay","params":{}}
```

**Every parameter is optional, and that is the design.** The shape a hotkey
sends is no parameters at all: keep the duration the recording was started with,
out of whatever is being recorded, and put the clip where that recording's clips
go. Somebody pressing `Ctrl`+`F10` mid-fight has said everything they are going
to say.

| Parameter | Default | Meaning |
| --- | --- | --- |
| `recording_id` | whatever is being recorded | Which recording to save out of, as `active_recording.recording_id` reported it |
| `duration_seconds` | the window the recording's buffer was started with | How much to keep |
| `output` | beside the recording, named after the session | Where to write the clip |

`duration_seconds` is a number rather than one of a fixed set, because SPEC.md
section 15's periods (15 s, 30 s, 1 min, 2 min, 5 min) come with "and custom",
and an enumeration with an escape hatch is an enumeration with a hole in it. A
duration longer than the buffer's window is **not** refused: the buffer cannot
hold more than its window, so the clip is what there was and `complete` says it
was short.

There is deliberately no range in the request — no start, no end, no "from this
moment". A replay buffer holds the last N seconds of *now*; an arbitrary window
of a recording is a different feature over a different source, and it is the
clip editor's (M11).

The reply says what the clip turned out to be, because what comes out is not
exactly what was asked for:

```json
{"type":"response","id":1,"outcome":{"ok":{"reply":"replay_saved","clip":{
  "path":"D:\\clips\\clipped-cs2-20260813-201400-replay-1.mkv",
  "recording_id":"r-1","requested_seconds":30.0,"duration_seconds":31.983,
  "source_start_seconds":553.017,"source_end_seconds":585.0,
  "leading_slack_seconds":1.983,"complete":true,"shortfall_seconds":0.0,
  "bytes":51204112}}}}
```

- A clip can only begin on a keyframe, so it is up to one keyframe interval
  longer at the front than the request. `leading_slack_seconds` is how much.
- A buffer that has not filled yet gives less than was asked for —
  `complete: false` and `shortfall_seconds` — which is a clip worth having and
  worth labelling, not a failure.

**Which recordings can be saved from.** A buffer costs a recording a spill
directory and the newest few seconds in memory, so one is kept only when
`start_recording` asked for it — either by naming a length with `replay_seconds`
or by asking for the configured one with `replay`.
`active_recording.replay_seconds` says whether the recording that is running has
one and how much it keeps, which is what a window reads before offering the
control; the `replay` feature says only that the build has the command.

Refusals: `not_recording` when nothing is being recorded, when a named recording
is not the one running, or when the recording that is running keeps no buffer —
which is a different sentence, because the answer to it is to start one that
does. `invalid_parameters` for a `duration_seconds` that is not a number of
seconds or a blank `output`. `internal` when the clip could not be written,
naming the file: a destination that already exists is refused rather than
replaced.

### `start_recording`

The parameters are the `clipped-recorder record` options under the names they
have on the command line, and the recorder validates them through the same code
— so a value the command line rejects is rejected here, with the same message.
One set of rules in one place (AGENTS.md section 55).

```json
{"type":"request","id":1,"command":"start_recording",
 "params":{"pid":4242,"output":"D:\\clips\\session.mkv","overwrite":false,
           "resolution":"source","framerate":60,"codec":"auto","encoder":"auto",
           "microphone":"none","system_audio":"none"}}
```

Exactly one of `window`, `process` and `pid` is required; everything else has
the same default the command line has. The reply names the recording:

```json
{"type":"response","id":1,"outcome":{"ok":{
  "reply":"recording_started","recording_id":"r-1",
  "output":"D:\\clips\\session.mkv"}}}
```

**Asking for a replay buffer.** Two parameters, and they are the two halves of
`clipped-recorder replay`: `replay` asks for a buffer, and `replay_seconds`
names how long it keeps — exactly as the subcommand asks for one and
`--duration` overrides the configured window.

| Sent | What the recording keeps |
| --- | --- |
| neither | no buffer |
| `"replay": true` | the configured window: `replay_window_seconds`, resolved for the game this recording turns out to be of |
| `"replay_seconds": 120` | 120 seconds |
| both | the length that was named |

Neither, meaning no buffer, is what every client that predates `replay` sends
and what an ordinary recording is. `replay` exists because the length is a
*setting*, and a caller cannot resolve it: `replay_window_seconds` inherits per
game (`docs/configuration.md`), and which game a `pid` is, is what the recorder
asks its catalogue once the window is resolved. The desktop window sends
`replay` for exactly that reason — it may link `clipped-ipc` and nothing else
of the workspace, so a length it named would be one nobody chose.

A `recording_id` is unique for the life of the recorder process. It exists so
that a stop meant for a recording that has already ended by itself cannot stop
its successor — a real race when a window closes at the moment the user presses
the button.

This recorder records **one thing at a time**: a second `start_recording` while
one is running is refused with `already_recording` rather than queued. A second
recording means a second encoder session and a second capture loop competing
with the game the first one is recording.

### `add_bookmark`

Marks a moment in the recording that is running, without saving a clip. Every
parameter is optional, because the request a hotkey or a tray item sends carries
none of them.

| Parameter | Meaning |
| --- | --- |
| `recording_id` | Which recording to mark. Absent means "whatever is being recorded". |
| `label` | What to call it. At most 200 characters. |
| `colour` | Any notation the interface likes, at most 64 characters; the recorder does not interpret it. |
| `duration_seconds` | How long the marked moment lasts, up to an hour. Absent means it is a moment rather than a span. |
| `lead_seconds` | How far *before* this request to stamp it. Absent means the recorder's default, which is **not zero**; at most 120 seconds. |

```json
{"type":"response","id":7,"outcome":{"ok":{
  "reply":"bookmark_added",
  "bookmark":{"recording_id":"r-1","at_seconds":115.0,"pressed_at_seconds":120.0,
              "lead_seconds":5.0,
              "bookmarks_file":"D:\clips\session.bookmarks.json",
              "bookmarks_in_recording":3}}}}
```

The reply says where the bookmark **landed**, and that is not where the request
was made: a person presses the key after the thing they wanted to mark, so the
recorder stamps it `lead_seconds` earlier. An interface that showed
`pressed_at_seconds` would be showing a moment that is not the one in the file.
`docs/bookmarks.md` has the reasoning, the accuracy figures and the file the
bookmarks are written to.

Refused with `not_recording` when nothing is being recorded, when the named
recording is not the one running, or when the recording has not captured its
first frame yet — there is no moment to mark, and marking zero would put the
bookmark somewhere the user was not looking.

### `take_screenshot`

Saves a still image of what is being captured (SPEC.md section 26,
[docs/screenshots.md](screenshots.md)). Every parameter is optional, because the
request a hotkey or a tray item sends carries none of them.

| Parameter | Meaning |
| --- | --- |
| `recording_id` | Which recording to photograph. Absent means "whatever is being recorded". |
| `window`, `process`, `pid` | Which window to photograph **when nothing is being recorded**. Exactly one, under the same names `start_recording` uses. |
| `format` | `png` (the default), `jpeg` or `webp`. |

```json
{"type":"response","id":9,"outcome":{"ok":{
  "reply":"screenshot_taken",
  "screenshot":{"path":"C:\Users\player\Pictures\Clipped\clipped-cs2-20260811-143205.png",
                "format":"png","width":2560,"height":1440,"bytes":4812009,
                "recording_id":"r-1","at_seconds":115.0}}}}
```

Which of the two paths the recorder takes is not the caller's choice. **If a
recording is running**, the picture comes from a frame that recording already
captured: it costs the capture thread one texture copy, `recording_id` and
`at_seconds` are present, and the recording is not interrupted. **If nothing is
running**, a capture is opened for the target the request names, one frame is
taken and it is shut down - a few hundred milliseconds rather than a few, which
is why the target parameters exist at all.

Refused with `invalid_parameters` for a format this build cannot write -
lossless WebP needs a `libwebp` the FFmpeg build may not carry - or when nothing
is being recorded and no target was named; with `target_not_found` when the
window named does not exist or has stopped drawing; with `target_not_capturable`
when the window named is minimised, which is a picture Windows would not produce
either; and with `internal`, naming the file, when the picture was taken and the
disk refused it.

### `library_sessions`

Reads one page of the recording library: the sittings, newest first, with the
recordings and clips each produced ([library.md](library.md), issue
[#301](https://github.com/wildware-uk/clipped/issues/301)).

It exists because the desktop window cannot read the index any other way. It has
no file-system permission for `library.db` — `capabilities/default.json` grants
three `core:` permissions and `dialog:allow-save`, none of which opens a file — and it may not link
`clipped-library`, because `tests/integration/tests/workspace_layering.rs`
permits `apps/desktop/src-tauri` exactly one member of the workspace, this one.
That is [ADR 0002](adr/0002-separate-recorder-process.md): the process that owns
the database answers for it.

| Parameter | Meaning |
| --- | --- |
| `limit` | How many sittings. Absent means the recorder's own page size (50); anything larger than 200 is clamped rather than refused. **A page may still come back shorter than asked for** — see below. |
| `after` | Continue after the sitting this cursor names. It comes from `next_cursor` and is **opaque** — the only thing to do with one is send it back. |
| `query` | A search query in the language of [search.md](search.md). Absent or blank means the whole library. |

```json
{"type":"response","id":9,"outcome":{"ok":{
  "reply":"library_sessions",
  "page":{"sessions":[
    {"session_id":"cs2-20260811-201400","game_id":"cs2","game_name":"Counter-Strike 2",
     "started_at":"2026-08-11T20:14:00+01:00","ended_at":"2026-08-11T22:03:00+01:00",
     "end_reason":"game-exited","favourite":false,
     "recordings":[{"recording_id":12,"session_index":1,
                    "path":"D:\\clips\\cs2-20260811-201400-1.mkv",
                    "started_at":"2026-08-11T20:14:00+01:00","outcome":"recorded",
                    "duration_seconds":6540.0,"width":2560,"height":1440,
                    "size_bytes":9812009112,"favourite":false,"tags":[]}],
     "clips":[]}],
   "next_cursor":"2026-08-11T20:14:00+01:00|cs2-20260811-201400"}}}}
```

A session's `end_reason` is `game-exited`, `system-resumed`, `recorder-stopping`
or `recording-ended` — the last for a sitting that was one recording somebody
asked for over this protocol ([sessions.md](sessions.md)). `game_id` and
`game_name` are absent for a sitting nothing attributed to a game, which is what
a `start_recording` produces today: the person chose a window and nothing asked
the catalogue about it.

**An empty library is this reply, not a refusal.** `{"sessions":[]}` means the
library was read and holds nothing matching the request. A library that could
not be read is `library_unavailable` and says why. The two must never be drawn
the same way: "you have not recorded anything" over a database that is locked,
corrupt, from a newer build or on a drive that is not plugged in is the
fabricated state AGENTS.md section 27 forbids.

**A recording whose file has gone is listed, carrying `missing_since`**, and is
never omitted. That is the field the whole command exists for: a screen has to
*say* the file has gone rather than draw a broken tile, and it can only do that
if the fact crosses the boundary. `size_bytes` is kept beside it — the size the
file had when it was last seen, so a drive coming back needs no re-measurement —
but a screen must not add it into a total meanwhile, because that space is not
being used.

**`next_cursor` is present only when a further sitting was actually found**, so
a caller stops on its absence rather than on an empty page. The cursor is a
keyset rather than an offset, which is what makes page four hundred cost what
page one costs; [library.md](library.md) measures a page of 25 at **4.8 ms on a
10,000-session library, and the twenty-first page at 4.0 ms**. A search is a
walk rather than an index lookup and costs more: 188 ms for one that fills a
page, 316 ms for one that matches nothing at all and therefore reads every
sitting.

A cursor the recorder cannot read starts at the newest sitting rather than being
refused, because it is a string a window may have kept across a restart, and
refusing to draw a library over one would be the least useful possible answer.

**A page is bounded in bytes, not in sittings, so it may be shorter than `limit`
asked for.** A count cannot be the bound: a sitting holds any number of
recordings and clips, so two hundred of them is 135 KB with one recording each
and over 3 MB with thirty — and [`MAX_FRAME_BYTES`](#framing) is a ceiling the
reader *closes the connection* over rather than a request it fails. The recorder
therefore fills a page up to half a frame and then stops, and `next_cursor` names
the last sitting it actually carried, so the next page begins with the first one
left out. A caller must page until `next_cursor` is absent rather than until a
page is shorter than it asked for. One sitting is always carried even if it
alone exceeds the budget, because a page that came back empty could never be
paged past.

Refused with `invalid_parameters`, naming the command and the position, for a
`query` the search language will not parse — a search box has to be able to say
what is wrong with what was typed rather than show an empty result set — and
with `library_unavailable` when the index could not be read.

### `library_games`

What the library holds per game: SPEC.md section 17's list. No parameters, and
not a page — it is every game at once, which is what the screen draws.

```json
{"type":"response","id":10,"outcome":{"ok":{
  "reply":"library_games",
  "games":[{"game_id":"cs2","name":"Counter-Strike 2",
            "first_seen_at":"2026-01-04T19:30:00+00:00",
            "last_played_at":"2026-08-11T20:14:00+01:00",
            "sessions":214,"recordings":265,"clips":31,"favourites":12,
            "bytes":411204889112,"missing":3},
           {"sessions":2,"recordings":2,"clips":0,"favourites":0,
            "bytes":1204889,"missing":0}]}}}
```

The second row is the one with no `game_id` and no `name`: the sittings the
catalogue would not attribute, because it reported a tie and the recording was
filed under no game rather than under a guess ([sessions.md](sessions.md)).
There is at most one such row and it is last. What to call that group on screen
is the screen's decision, which is why the protocol does not make one.

`bytes` counts only files that are still there. A missing file contributes
nothing — the space it is not occupying is not being used — and is counted in
`missing` instead. Anything in the trash contributes to neither.

Refused with `library_unavailable` on the same terms as `library_sessions`.

### `library_trash`

What is waiting in the trash: everything deleted and not yet emptied.

```json
{"type":"request","id":14,"command":"library_trash","params":{}}
```

```json
{"type":"response","id":14,"outcome":{"ok":{
  "reply":"library_trash",
  "trash":{"items":[
    {"kind":"recording","id":1,
     "path":"D:\Clips.trash\clipped-cs2-20260814-201500.mkv",
     "original_path":"D:\Clips\clipped-cs2-20260814-201500.mkv",
     "deleted_at":"2026-08-15T09:00:00+01:00",
     "expires_at":"2026-09-14T09:00:00+01:00",
     "size_bytes":2147483648,"dependent_clips":2}],
   "total_items":1,"total_bytes":2147483648,"directory":"D:\Clips.trash"}}}}
```

**No paging.** The trash is what somebody deleted and has not emptied, bounded
by the retention period rather than by the size of the library, so a cursor
would be machinery for a case that does not arise. If that turns out to be
wrong it gains one the way `library_sessions` has one.

**`original_path` is the one a person recognises.** A file inside the trash is
named for the trash; asking somebody to identify their own recording by a name
they have never seen is not showing it to them.

**`total_items` and `total_bytes` travel together** because emptying takes them
back: a window that showed "3 recordings, 12 GB" and then emptied a trash that
had gained a fourth is refused rather than deleting something nobody saw
(`clipped_library::trash::EmptyTrash`). The commands that restore and empty are
the other half of [issue #450](https://github.com/wildware-uk/clipped/issues/450).

**`expires_at` is absent from this build's replies.** Nothing configures the
retention period yet, and a date computed from a policy nobody set would be a
screen promising a deletion it cannot keep. The field is on the wire so that it
does not have to be added later.

### `restore_from_trash`

Puts one thing back where it was.

```json
{"type":"request","id":15,"command":"restore_from_trash",
 "params":{"kind":"recording","id":1}}
```

```json
{"type":"response","id":15,"outcome":{"ok":{
  "reply":"restored",
  "restored":{"kind":"recording","id":1,
    "path":"D:\\Clips\\clipped-cs2-20260814-201500.mkv",
    "file_restored":true,"renamed":false}}}}
```

Named by kind and identifier, which is what a listing gave. A path would be the
wrong key: the file inside the trash is not the thing the index knows about.

`file_restored` is `false` for something whose media had already gone before it
was deleted — the row comes back and reports itself missing, which is the truth
rather than a row with no explanation. `renamed` is `true` when something was
occupying the original location, so the file went somewhere else rather than
over the top of it.

### `empty_trash`

Destroys everything in the trash, **confirmed against the listing that was
shown**.

```json
{"type":"request","id":16,"command":"empty_trash",
 "params":{"items":1,"bytes":2147483648}}
```

```json
{"type":"response","id":16,"outcome":{"ok":{
  "reply":"trash_emptied",
  "emptied":{"removed":1,"reclaimed_bytes":2147483648,"refused":[]}}}}
```

**Both numbers are checked, and that is the point of them.** They are the
listing the user was looking at when they pressed the button. If the trash has
gained an item since, the recorder refuses with `invalid_parameters` naming both
counts, and the window shows the new listing — because the alternative is
destroying something nobody saw. It is why this takes two numbers rather than a
boolean.

`refused` is always present and names each thing that would not go, with the
reason: a file another program had open is a real outcome, the next sweep tries
it again, and a reply carrying only a count would say the trash is empty when it
is not.

### `library_events`

The marks on one recording's timeline: what a plugin reported while it was being
recorded, **placed in that recording's file**.

```json
{"type":"request","id":11,"command":"library_events","params":{"recording":"1"}}
```

```json
{"type":"response","id":11,"outcome":{"ok":{
  "reply":"library_events",
  "lane":{"marks":[
    {"recording":"1","at":4000000000,"kind":"kill","source":"counter-strike-2"},
    {"recording":"1","at":9500000000,
     "kind":"acme-cs2.flashbang_blinded_five","source":"acme-cs2"}]}}}}
```

`at` is nanoseconds **into that recording's file**, which is what a timeline
draws at and a player seeks to — not a moment on the session's timeline, which
is how the events are stored. A session can write several files and a recording
can begin after the game did, so the two are different numbers; the recorder
does the subtraction because it needs the recording's span, which the window has
no way to know ([av-sync.md](av-sync.md), "One epoch per recording, one timeline
per session").

Three properties of this reply are deliberate:

- **`kind` is not a closed vocabulary.** The second mark above is a plugin's own
  namespaced name, and a kind added after the window shipped arrives the same
  way. A client that validated `kind` against a list would delete exactly the
  marks that have to survive, which is the compatibility rule at the top of this
  document applied to an open set.
- **The payload does not travel.** A plugin's own detail can be kilobytes and
  nothing above the plugin interprets it, so it is not sent for a mark two
  pixels wide. It stays in the library.
- **`marks` is always present.** An empty array means the recording has no
  events; it does not mean the question was not asked. Those are different
  things to draw, and a client that could not tell them apart would have to
  guess whether to show an empty lane or say nothing is known.

A recording whose span the library does not know — one that produced no frame,
or a row indexed before the span was recorded — has no marks rather than marks
in the wrong place.

Refused with `invalid_parameters` if `recording` is not an identifier this
library uses, and with `library_unavailable` on the same terms as
`library_sessions`.

### `plugins`

What is installed, what each plugin declares, and what will start.

```json
{"type":"response","id":12,"outcome":{"ok":{
  "reply":"plugins",
  "installed":[
    {"id":"acme-cs2","name":"Counter-Strike 2 highlights","version":"0.1.0",
     "description":"Reports kills, deaths and rounds from Game State Integration.",
     "network":["Listens on 127.0.0.1:3212 (this machine only) — receives Counter-Strike 2 game state"],
     "enforcement":"Clipped shows what a plugin declares and refuses to start one whose declaration has changed since you allowed it. It cannot yet stop a plugin from using the network in ways it did not declare.",
     "state":{"state":"not-enabled"}}],
  "refused":[]}}}
```

**A declaration is shown before consent is taken, never after.** Enabling a
plugin *is* the consent to the network access it declares, and every bundled
plugin opens a loopback socket, so [privacy.md](privacy.md)'s register is only
true if a deliberate, informed action is what starts one.

That is why the sentences and `enforcement` travel rather than being composed by
the reader: the words somebody agrees to are the recorder's, and a second
rendering of one declaration is a second thing to keep in step with what is
actually enforced.

`network` is empty when a plugin declares none. A screen must **say** that
rather than draw a blank row — "it asks for nothing" and "we did not ask" are
different things.

### The four states

| `state` | What it means |
| --- | --- |
| `enabled` | It will start with the next game it supports. |
| `not-enabled` | Nothing has ever allowed it. What a newly installed plugin says. |
| `turned-off` | Allowed, then turned off. What was agreed to is kept. |
| `needs-consent-again` | It asks for something other than what was agreed to. Carries `agreed_to` and `now_declares`, because "here is what changed" cannot be asked with one of them. |

`refused` is everything under the plugins directory that is not a usable plugin,
with the reason. Reported rather than omitted: somebody put it there expecting
it to work.

Both arrays are always present, so "nothing installed" and "the question was not
asked" are told apart by whether the reply arrived.

**Enabling is not here.** Writing the consent record is a settings write the
protocol does not have; `clipped-recorder plugins enable` is what writes one
today, and the screen is
[#281](https://github.com/wildware-uk/clipped/issues/281). Neither is a plugin's
*health* — whether it is running, restarting or was stopped for flooding belongs
to a live session rather than to the list of what is installed.

### `export_recording`

Copies a finished recording into MP4 without decoding it. Clipped records
Matroska because it survives an interrupted recording
([ADR 0001](adr/0001-use-mkv-for-recording.md)); MP4 is what everything else
accepts, and a **stream copy** is how a file becomes one without a re-encode
([muxing.md](muxing.md)).

```json
{"type":"request","id":11,"command":"export_recording",
 "params":{"source":"D:\clips\cs2-20260811-201400-1.mkv",
           "destination":"E:\share\ace on mirage.mp4"}}
```

Both parameters are required, and neither has a default — unlike every other
command's, which is deliberate. There is no sensible recording to export and
nowhere sensible to put one, so "you did not say" is a refusal
(`invalid_parameters` naming the field) rather than a value something further
down has to recognise as absent.

The reply does not arrive until the MP4's index has been written, so a window
told an export finished is pointing at a playable file:

```json
{"type":"response","id":11,"outcome":{"ok":{
  "reply":"recording_exported",
  "export":{"source":"D:\clips\cs2-20260811-201400-1.mkv",
            "destination":"E:\share\ace on mirage.mp4",
            "duration_ms":6540000,"packets":588120,"bytes":9811204112,
            "elapsed_ms":4182,"lossless":true}}}}
```

`elapsed_ms` is measured rather than estimated, and is worth reporting: the
whole argument for remuxing instead of re-encoding is that it is small
(AGENTS.md section 18). `lossless` is false when something *beside* the
recording was left out — chapter marks, an attached font — and `losses` then
says what, in words. It is never a picture or a sound track: a container that
cannot carry one of those is a refusal, because a file missing one of its audio
tracks looks exactly like a file that never had it.

### `open_playback`

Opens a finished recording so that the desktop window can play it, on one of its
sound tracks ([#304](https://github.com/wildware-uk/clipped/issues/304)).

```json
{"type":"request","id":13,"command":"open_playback",
 "params":{"source":"D:\clips\cs2-20260811-201400-1.mkv","audio_track":3}}
```

`source` is required and has no default, for the reason
[`export_recording`](#export_recording)'s has none. `audio_track` is optional and
is a **stream index of the file** rather than an ordinal among the sound tracks
— the two differ by however many picture tracks come first. Absent means the
track a player should choose on its own, which the recorder decides: the one the
container flags as the default, falling back to the first.

```json
{"type":"response","id":13,"outcome":{"ok":{
  "reply":"playback_opened",
  "playback":{"path":"D:\clips\cs2-20260811-201400-1.mkv",
              "audio_track":1,
              "audio_tracks":[
                {"index":1,"name":"Compatibility Mix","default":true},
                {"index":2,"name":"Game","default":false},
                {"index":3,"name":"Microphone","default":false}],
              "prepared":false}}}}
```

**`path` is usually the recording itself, and `prepared` says so.** A WebView2
plays a Clipped recording as it stands — Matroska, AV1 picture, uncompressed PCM
sound — which [ADR 0011](adr/0011-what-the-webview-plays.md) measures rather than
assumes. So opening a recording on its default track writes nothing at all: the
file is opened, its streams are described, and it is closed.

`prepared` is `true` when `path` is a **copy carrying one sound track**, which is
what a request for any track other than the first produces. That exists because
a media element cannot choose one: `HTMLMediaElement.audioTracks` is not
implemented in Chromium, and its demuxer takes the first sound track the
container declares — ignoring Matroska's default-track flag. So hearing the
microphone on its own means being handed a file that holds the microphone. The
copy is still a stream copy (`clipped_muxer::remux_to_mp4_carrying`); nothing is
decoded and nothing is encoded, and it goes in
`%LOCALAPPDATA%\Clipped\playback`, never beside the recording. A prepared copy is
a cache entry: it is in nobody's library, and it is swept after a day.

`audio_tracks` is every sound track of the **recording**, not of the file being
played, because it is what a window offers next. It is absent for a recording
with no sound at all — a capture that found no audio device — which a window has
to be able to tell from a track that would not play.

**There is deliberately no duration and no picture size.** The media element
measures both from the file it is given; a figure sent from here would be a
second answer to the same question, and the two would disagree for exactly the
files where it matters — a recording a killed recorder left, whose container may
carry no duration at all
([#283](https://github.com/wildware-uk/clipped/issues/283)).

Two refusals, both `playback_failed` and both saying which:

- **the recording's file has gone** — checked before the muxer, so the answer
  names the file and says what probably happened to it rather than being
  FFmpeg's account of an I/O error;
- **the sound track asked for is not one the recording has** — refused rather
  than quietly answered with the default, because a window that asked for the
  microphone and was handed the compatibility mix would play something, with
  sound, and look exactly as though it had worked.

The recording is opened for reading and is never modified, on either path.

### `get_hotkeys`

Where every global hotkey stands. The recorder registers them —
[ADR 0009](adr/0009-the-recorder-registers-global-hotkeys.md) says why it and
not the window — so this is the only way a window can see what happened when it
did.

```json
{"type":"request","id":12,"command":"get_hotkeys"}
```

```json
{"type":"response","id":12,"outcome":{"ok":{
  "reply":"hotkeys",
  "hotkeys":[
    {"action":"save_replay","label":"Save replay","hotkey":"Ctrl+F10",
     "state":{"state":"conflict",
              "reason":"Ctrl+F10 could not be Clipped's shortcut for Save replay: another application already uses it. Choose a different combination, or close the application that has this one and try again"},
     "handled":true},
    {"action":"add_bookmark","label":"Add bookmark","hotkey":"Ctrl+F9",
     "state":{"state":"registered"},"handled":true},
    {"action":"open_overlay","label":"Open overlay",
     "state":{"state":"unbound"},"handled":false,
     "unavailable":"Open overlay is not in this build: the overlay arrives in M5 (issue #53)"}]}}}
```

Always every action, including the ones bound to nothing: a screen sent a subset
could not offer the rest, and an action missing from the list is
indistinguishable from one the recorder has never heard of.

**`state` and `handled` are two questions, and a client that reads only one of
them will be wrong half the time.** `state` is what Windows said —
`unbound`, `registered`, or `conflict` carrying the sentence to show. `handled`
is whether anything in the recorder performs the action, and `unavailable` is
the recorder's own words for why not. The two come apart in both directions:
`Ctrl`+`F10` is the combination another application is most likely to have
taken, and the recorder performs `save_replay` whether or not Windows granted
it; `open_overlay` would register cleanly on any machine and nothing behind it
would happen. A row drawn from `state` alone reports a working hotkey for a key
that reports itself as unbuilt when pressed (AGENTS.md section 27).

`state` is a **closed** enumeration and tolerates no stranger, unlike an event
or an error code and for the same reason [`recorder_status`](#get_status) does
not: a state a client cannot read would be drawn as one it can, and every one it
can read says the key either works or plainly does not.

This is asked for rather than pushed. Registration happens once, when the
recorder starts, which is usually long before any window exists — so an event
would be published to nobody, and the window that opened an hour later would
show a clean list.

Two refusals, and they are deliberately different codes because the useful
action differs:

- `destination_exists` — there is already a file there. **Nothing is written and
  the file that is there is not touched** (AGENTS.md section 56). The one thing
  to do is choose another name, and the message says so.
- `export_failed` — the recording could not be read, or MP4 has no way to store
  one of its tracks. The message is the muxer's own sentence, naming the track
  and the codec, because a generic failure gives nobody anything to act on
  (AGENTS.md section 15).

The recording itself is opened for reading and is never modified, on either
path.

### `stop_recording`

`recording_id` is optional. Absent means "whatever is running", which is what a
tray menu wants; naming one is what a window with a particular recording on
screen does. The reply is the recording's own account of itself, and it does not
arrive until the file has been finalised:

```json
{"type":"response","id":2,"outcome":{"ok":{
  "reply":"recording_stopped",
  "summary":{"output":"D:\\clips\\session.mkv","duration_ms":3800,
             "end_reason":"stopped","frames_encoded":115,
             "frames_skipped_for_rate":0,"frames_dropped_writer_behind":0,
             "sustained_framerate":30.0,
             "encoder":"nvenc","codec":"av1","width":1280,"height":720}}}}
```

The frame counts are deliberately separate rather than summed. A frame skipped
to hold the requested rate is the recorder doing what it was asked; a frame
dropped because the writer fell behind is the recorder failing. One "dropped"
number would mean nothing, and a UI cannot re-separate what the protocol has
already mixed (AGENTS.md section 19).

`end_reason` is `stopped`, `target_lost` or `target_resized`.

`encoder` and `codec` use the same tokens `--encoder` and `--codec` accept, so a
support request saying `encoder=nvenc` means the same thing whichever produced
it.

### `shutdown`

Asks the recorder to stop listening, finish anything it is recording, and exit.
It is the answer to
[issue #220](https://github.com/wildware-uk/clipped/issues/220): a recorder
started by the desktop application is detached, with its own process group and no
console, so it cannot be sent `CTRL_C_EVENT`, and before this command the only
way to end one was Task Manager.

```json
{"type":"request","id":8,"command":"shutdown","params":{"finalise_recording":false}}
```

```json
{"type":"response","id":8,"outcome":{"ok":{"reply":"shutting_down"}}}
```

**It runs the shutdown the recorder already had rather than a second one.** The
command stops the listener, which is exactly what Ctrl+C does; everything after
that — stopping the recording, waiting for its file to be finalised, closing the
event subscriptions, exiting — is the recorder's existing path
(`apps/recorder/src/serve.rs`). There is no second ordering to get wrong.

#### What happens if a recording is running

The endpoint is reachable by anything running as this user, and "exit" must not
become a way for any of it to end somebody's recording (AGENTS.md section 17).
So the recorder refuses by default and performs it only when asked in as many
words:

```json
{"type":"response","id":8,"outcome":{"error":{
  "code":"already_recording",
  "message":"`process `cs2.exe`` is being recorded to D:\\clips\\session.mkv; ask again with `finalise_recording` to finish that file and exit"}}}
```

`finalise_recording: true` — which a request that omits the field does **not**
mean — stops the recording, finishes its file and exits, and the reply names the
file so the client can tell the user where it is:

```json
{"type":"response","id":8,"outcome":{"ok":{"reply":"shutting_down",
  "finalising":{"recording_id":"r-1","output":"D:\\clips\\session.mkv",
                "target":"process `cs2.exe`","elapsed_ms":4200}}}}
```

Either way no footage is lost: the file is closed properly, not abandoned. The
tray's Exit is the caller this shape was designed for — its menu item reads
"Stop recording and exit" while something is being recorded, so the permission
in the request is the same sentence the user read (`docs/desktop-ui.md`).

#### Nothing new is started once one is accepted

A recorder serves eight connections at once, so the status the decision above is
made from is read while seven others may be sending commands. The recorder
therefore closes itself to new recordings **before** it reads that status, and a
`start_recording` arriving from then on is refused:

```json
{"type":"response","id":9,"outcome":{"error":{
  "code":"shutting_down",
  "message":"this recorder has been asked to exit and will not start a recording"}}}
```

Without that, a `start_recording` landing between the read and the reply would
begin a recording the permission never covered, and the shutdown would end it
having asked nobody. A shutdown that is **refused** opens it again at once, so a
`start_recording` after one is served exactly as it was before.

#### When the reply arrives, and what it promises

**Before** the recorder winds up, because a reply written after the process
ended would never be written at all. It says the shutdown was accepted and the
endpoint is closing. The proof that it *finished* is the endpoint going away,
which is the last thing the recorder does;
`clipped_ipc::supervisor::wait_for_recorder_to_exit` is the wait for it, and it
has to allow for finalising a file.

#### An older recorder

`shutdown` is a new command name, so a recorder built before it refuses it with
`unknown_command` naming the command — which is version skew a client can report,
rather than a request that was ignored. A client that would rather not ask at all
checks `shutdown` in [`welcome.features`](#the-handshake) first.

### The settings

`get_settings` and `apply_settings` are how a window reads and changes
`settings.json`, which the **recorder** owns: its defaults, its validation, its
layering and its migrations are `clipped_session::config`
(`docs/configuration.md`), and the desktop application may link
`clipped-ipc` and nothing else of the workspace. A window that read that file
itself would be a second implementation of all of it, against the file
somebody's settings live in (AGENTS.md section 55, [#252]).

```json
{"type":"request","id":13,"command":"get_settings"}
```

```json
{"type":"response","id":13,"outcome":{"ok":{"reply":"settings","settings":{
  "file":"C:\\Users\\alex\\AppData\\Local\\Clipped\\settings.json",
  "settings":[
    {"key":"microphone","label":"Microphone","value":"name:Shure MV7",
     "overridden":true,"accepted":"\"default\", \"none\" or a device name",
     "applies":true},
    {"key":"capture_target","label":"Capture target","value":"game-window",
     "overridden":false,"choices":["game-window","display"],
     "accepted":"\"game-window\" or \"display\"","applies":false,
     "unavailable":"every recording captures the game's own window. Reading this setting when a recording starts is issue #61"}]}}}}
```

Every value crosses as **the words the settings file spells it in** — `120`,
`hevc`, `name:Shure MV7` — and goes back the same way. A variant per setting on
the wire would have been a second vocabulary beside the file's own and a
protocol change for every setting added; instead the recorder parses what comes
back with the file reader's own parsers, so a value a window can save is exactly
a value the file would accept.

Two fields decide what a screen may draw, and both are the recorder's answer
rather than the window's guess:

- `choices` is the closed set of values, and is **absent** when the set is open
  — a frame rate, a size, a device name. That is how a list of options is told
  from a field without the window keeping a copy of either.
- `applies` is whether anything in *this build* reads the setting when a
  recording starts. `false` carries `unavailable`, the recorder's sentence
  naming what would have to land, and a window draws the value and that sentence
  rather than a control — a control that changed nothing being the defect
  AGENTS.md section 27 is about. It is the same pair a `hotkeys` row carries.

`apply_settings` sends only what changed. `null` clears a setting, which is
Reset: it returns the setting to the value Clipped ships with *and* keeps
following it, which writing today's default in as a value would not.

```json
{"type":"request","id":14,"command":"apply_settings",
 "params":{"values":{"microphone":"name:Shure MV7","framerate":null}}}
```

The reply is `settings` again — the settings **as they now stand** — so a window
draws what was saved rather than what it hoped had been: a value the recorder
refused, or one another window changed a moment earlier, cannot be drawn as
applied. Nothing is written unless every value is accepted, so a request naming
one good value and one bad one leaves the file exactly as it was, and the
refusal is `invalid_parameters` carrying `clipped_session`'s own sentence, which
names the setting, the value and what would have been accepted.

A recorder that cannot work out where settings live at all refuses with
`internal` and says so, rather than accepting a change it has nowhere to keep.

### `get_audio_devices`

Which microphones this machine has, so that picking one is a choice from a list
rather than a name somebody has to type and cannot see ([#308]). The window
cannot enumerate them: `clipped-audio` is in the recorder's process, and a
configured name is matched against the endpoints present when a recording
starts — so the list has to be the recorder's own.

```json
{"type":"response","id":15,"outcome":{"ok":{"reply":"audio_devices","devices":{
  "microphones":[{"name":"Shure MV7","is_default":true},
                 {"name":"Line In (Realtek)","is_default":false}]}}}}
```

The default one is **marked** rather than being first: Windows lists endpoints
in its own order, and `default` follows whichever endpoint it is currently
using. Playback endpoints are not listed at all, because a recording cannot be
told to use one that is not the default ([#316]) — an empty list of them would
say something untrue about the machine, so there is no field for them.

A recorder that could not enumerate the endpoints refuses with `internal` and
the reason, so a window says why there is no list rather than drawing an empty
one as though it had looked (AGENTS.md section 27).

### `get_microphone_level`

What one microphone is hearing at this moment, so that choosing one is a thing
somebody can *check* rather than guess at ([#109]). A list of endpoint names says
which microphones exist and nothing about which of them can hear the person
choosing, and on a machine with a webcam, a headset and a monitor's array
microphone that is the whole of the question.

```json
{"type":"request","id":19,"command":"get_microphone_level",
 "params":{"microphone":"name:Shure MV7"}}
```

```json
{"type":"response","id":19,"outcome":{"ok":{"reply":"microphone_level","level":{
  "device":"Shure MV7","peak":0.5,"muted":false}}}}
```

`microphone` is a **setting's value**, spelled as the settings file spells it —
`default`, `none`, `name:Shure MV7` — and not a device name. The question is
what the choice somebody is looking at would record, asked before it is saved,
so the recorder parses it with the settings file's own parser and resolves it to
an endpoint with the code a recording resolves it with: a value that can be asked
about is exactly a value that could be saved, and the meter cannot end up pointed
at a different endpoint from the recording.

`peak` is the loudest sample in the moment that was listened to — **not** since
the last question — as a linear amplitude from 0 to 1. The endpoint is opened,
listened to and closed inside the call, so a window that is killed mid-choice
leaves no capture behind and no microphone-in-use indicator; the cost is that
between two questions there is a gap nothing measured, which is why a client asks
again rather than accumulating.

The other two fields are the ones a meter cannot say for itself, and both are
absent rather than guessed at:

- `device` is missing while the endpoint is unplugged or disabled, during which a
  capture produces silence rather than failing. It is what tells "nobody is
  speaking" from "there is nothing there";
- `muted` is missing when Windows will not report the switch for that device,
  which some virtual devices do not have. A muted microphone reads as exactly the
  silence of a quiet room, and telling somebody to speak up when the answer is a
  switch is the vague message AGENTS.md section 28 is about.

`none` is refused with `invalid_parameters` rather than answered with a peak of
zero: it is a setting somebody chose, and a reading of silence would be drawn as
a dead meter over a deliberate choice (AGENTS.md section 27). A device that
cannot be opened is `internal` carrying the reason, for the same reason
`get_audio_devices` refuses rather than sending an empty list.

[#109]: https://github.com/wildware-uk/clipped/issues/109
[#252]: https://github.com/wildware-uk/clipped/issues/252
[#308]: https://github.com/wildware-uk/clipped/issues/308
[#316]: https://github.com/wildware-uk/clipped/issues/316

### `get_start_at_login`

Whether the recorder starts when this user signs in, and what is arranged. Not
part of the settings: it is one value under
`HKEY_CURRENT_USER\…\CurrentVersion\Run` that Windows reads once, at sign-in,
and that Windows also lists in **Settings > Apps > Startup** with a switch of
its own. `settings.json` does not carry it, and a copy there could disagree with
the registry — which is the thing that actually decides ([#308]).

```json
{"type":"response","id":16,"outcome":{"ok":{"reply":"start_at_login",
  "start_at_login":{"enabled":true,
    "location":"HKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Run\\Clipped Recorder",
    "command":"\"C:\\Program Files\\Clipped\\clipped-recorder.exe\" serve --watch-for-games"}}}}
```

`enabled` is the switch's position and nothing more. `command` is what Windows
would run, present exactly when `enabled` is true, and it is worth sending
because there is nowhere else to see it short of a registry editor. `location`
is sent rather than known by the caller, so that a window can say where the
value lives without keeping its own copy of a path only the recorder changes.

`missing_executable` is the third state, and it is neither on nor off:

```json
{"type":"response","id":18,"outcome":{"ok":{"reply":"start_at_login",
  "start_at_login":{"enabled":true,"location":"…",
    "command":"\"C:\\Old\\clipped-recorder.exe\" serve --watch-for-games",
    "missing_executable":"C:\\Old\\clipped-recorder.exe"}}}}
```

A Clipped that was moved or reinstalled leaves a value naming a path that is no
longer there. `enabled` stays **true**, because Windows will still try it — a
caller that drew that as "off" would offer to turn on something already on — and
the path that is missing is named so that the caller can say what was looked
for. It is **reported rather than repaired**: rewriting somebody's startup entry
because a status was read, or because a settings screen was opened, is the
surprising behaviour this whole arrangement avoids
([privacy.md](privacy.md)). The repair is `set_start_at_login` with `true`,
which writes the running recorder's own path.

### `set_start_at_login`

Turns it on or off. One boolean, because it is one switch:

```json
{"type":"request","id":17,"command":"set_start_at_login","params":{"enabled":true}}
```

Answered with `start_at_login` as it now stands, read back out of the registry —
for the reason `apply_settings` answers with the settings as they now stand: a
window that drew the switch where the user put it would show "on" for a write
the registry refused.

Both are idempotent. `true` over an entry that is already there rewrites it with
this recorder's path, which is the repair above; `false` over nothing is the
state being asked for rather than a failure.

**The recorder writes it, and no other process can.** The value is a command
line naming the executable to run, and that executable is the recorder — a
window writing a path it worked out from its own location would leave a startup
entry pointing at nothing whenever the two were not where it assumed, and a
startup entry that points at nothing fails silently, once, at a sign-in nobody
is watching. `clipped-recorder start-at-login` and these two commands are the
same code (`apps/recorder/src/start_at_login.rs`,
[recorder-cli.md](recorder-cli.md)).

A build with no registry — anything that is not Windows — refuses both with
`internal` and the reason, so a caller says why it cannot offer the switch
rather than drawing one in the off position as though it had looked (AGENTS.md
section 27).

## Commands this build cannot perform

**None.** The protocol used to define commands it could not perform: they parsed
— so that the refusal could name the command rather than rejecting the name —
and were then refused with `not_implemented` and a detail naming the subsystem,
the milestone and the issue. `save_replay` was one until issue #38 built it, and
`apply_settings` was the last, until the settings reached the protocol (issue
#51). Every command this build defines, it performs.

The shape is still in the protocol, for the thing that still needs it: an
`events` connection asking for the `metrics` stream is refused the same way,
because nothing measures those figures during a recording yet ([#100]).

```json
{"type":"refused","code":"not_implemented",
 "message":"live recording metrics is not in this build",
 "detail":{"detail":"not_implemented","subsystem":"live recording metrics",
           "milestone":"M14","tracking_issue":100}}
```

A client should render that as what it is — "live metrics are not in this build"
— rather than as a stream that is simply quiet, which is the same rule that
applied to the commands.

[#100]: https://github.com/wildware-uk/clipped/issues/100

## Events

Sent on an `events` connection, unprompted:

```json
{"type":"event","event":"status_changed","status":{"state":"idle"}}
```

```json
{"type":"event","event":"status_changed",
 "status":{"state":"recording","recording_id":"r-1",
           "output":"D:\\clips\\session.mkv","target":"process `cs2.exe`",
           "elapsed_ms":4200}}
```

```json
{"type":"event","event":"status_changed",
 "status":{"state":"watching",
           "session":{"session_id":"cs2-20260811-201400","game_id":"cs2",
                      "game_name":"Counter-Strike 2",
                      "started_at":"2026-08-11T20:14:00+01:00"}}}
```

```json
{"type":"event","event":"session_ended",
 "session":{"session_id":"cs2-20260811-201400","game_id":"cs2",
            "game_name":"Counter-Strike 2",
            "started_at":"2026-08-11T20:14:00+01:00",
            "ended_at":"2026-08-11T22:03:00+01:00",
            "end_reason":"disk_full",
            "recordings":[{"session_index":1,
                           "output":"D:\\clips\\cs2-20260811-201400-01.mkv",
                           "outcome":"recorded","duration_ms":1800000}]}}
```

```json
{"type":"event","event":"recording_failed","recording_id":"r-1",
 "error":{"code":"recording_failed","message":"the encoder stopped accepting frames"}}
```

| Stream | Events | This build |
| --- | --- | --- |
| `status` | `status_changed`, `session_ended` | yes |
| `errors` | `recording_failed` | yes |
| `metrics` | live throughput, dropped frames, encoder load | no — M14, [#100](https://github.com/wildware-uk/clipped/issues/100) |

**A `status` subscription opens with the current state**, before anything
changes. Without it a client that attaches to a recorder which then does nothing
for an hour has nothing to display for an hour, and a client that asked
separately would race its own subscription.

`metrics` is refused at subscription time with `not_implemented`, not accepted
and left silent. Nothing measures those figures during a recording yet, and a
stream that delivers nothing is a control that does nothing.

A subscriber that stops reading loses events rather than making the recorder
wait: the queue per subscriber is bounded, and the thread that publishes is
quite possibly the thread that is recording, which may not wait on a window
(AGENTS.md section 20). Because every `status_changed` carries the whole state
rather than a delta, a client that missed one recovers on the next.

`target` is the selector the user gave — `process `cs2.exe`` — and never the
window title. A title is user content and the most reliable way to put somebody's
document name into a screenshot of a bug report (AGENTS.md section 13).

**`session_ended` carries the sitting itself, not an identifier to look up.** A
sitting ends the moment the game does, and the library may not have indexed a
thing by then — a client sent only an identifier would have nothing to show and
no way to know when it would. It is the same `session` shape the status carries,
with the two fields only an ended sitting has: `ended_at`, and `end_reason` when
there is one to give. Whether a sitting is over is therefore the presence of
`ended_at` rather than a separate type, which is the answer `library_sessions`
had already settled on for the same question.

## Errors

Every refusal is a code, a message and an optional machine-readable detail. The
code is stable and is what a client branches on; the message is the sentence a
person reads, written to AGENTS.md section 28.

| Code | Means |
| --- | --- |
| `unsupported_protocol_version` | The version asked for is not one this recorder speaks. Carries the versions it does. |
| `handshake_required` | A request arrived before the handshake. |
| `malformed_frame` | The bytes were not a message, or the length prefix was over the limit. The connection closes. |
| `unknown_command` | No command by that name in this build. |
| `invalid_parameters` | The command exists; the parameters do not describe something that could be done. |
| `not_implemented` | The command exists and its subsystem is not in this build. Carries the milestone and issue. |
| `already_recording` | A recording is running. For `start_recording`, because this recorder runs one at a time; for `shutdown`, because the request did not say it could finish one. |
| `not_recording` | There is nothing to stop, or the named recording is not the one running. |
| `target_not_found` | No window matched what was asked for. |
| `target_not_capturable` | A window matched and cannot be recorded as it is — it is minimised, so Windows draws it for nobody and the recording would be empty ([#383](https://github.com/wildware-uk/clipped/issues/383)). The message names the window; the thing to change is the window, not the request. |
| `recording_failed` | Capture, encoding or muxing refused. Whatever was written before the failure is still a finished file. |
| `too_many_connections` | The recorder is serving as many connections as it will. |
| `shutting_down` | The recorder has accepted a [`shutdown`](#shutdown) and will not start a recording. |
| `destination_exists` | Something is already where a file was going to be written, and Clipped does not overwrite (AGENTS.md section 56). Choose another name; nothing was changed. |
| `export_failed` | A finished recording could not be copied into the container asked for. The message is the muxer's own, naming what stopped it. |
| `playback_failed` | A recording could not be opened for playback: its file has gone, it could not be read, or the sound track asked for is not one it has. The message says which. |
| `library_unavailable` | The recording library could not be read, and the message says why. **Never an empty library**, which is a successful reply carrying no sessions. |
| `internal` | The recorder is at fault and cannot say more usefully. |

A code a client has never seen is kept verbatim and its message shown, and so is
a `detail` whose shape it does not know — see [the compatibility
policy](#an-unknown-error-code-error-detail-end-reason-or-event-kept). A refusal
is the one message that must stay readable by a build that understands nothing
else about it.

## When something goes wrong

**The recorder is not running.** `CreateFile` on the endpoint fails at once with
"no recorder is listening on `\\.\pipe\…`". The desktop application must render
that state honestly rather than falling back to plausible-looking defaults
([ADR 0002](adr/0002-separate-recorder-process.md)), and starts one — detached,
so it outlives the window — through `clipped_ipc::supervisor`
([ADR 0006](adr/0006-recorder-lifetime-and-supervision.md)).

**The recorder dies mid-request.** The client's read fails with a broken pipe,
which `clipped-ipc` reports as a disconnection rather than an error, because it
is not one — it is a fact about the recorder. There is no ambiguity between
"still thinking" and "gone", which is the property a request timeout would have
had to guess at.

**The client dies mid-request.** The recorder's write of the reply fails, it
logs at debug and closes that connection, and it carries on serving everybody
else. A user closing the window mid-command is an ordinary event, not an error.
`a_client_that_disappears_mid_request_leaves_the_recorder_serving` asserts it
against a real process.

**A recording fails on its own.** The recording thread publishes
`recording_failed` on the `errors` stream and `status_changed` on `status`, and
the file is finalised and playable — `clipped-session` closes the container on
every path out. The failure is still collectable by a later `stop_recording`,
which returns it rather than "nothing is being recorded", so a UI that was not
subscribed still finds out.

**Two recorders on one endpoint.** The second fails to bind and exits with a
message saying another recorder is already listening. That is the whole of the
recorder's single-instance story, and `clipped_ipc::supervisor` builds on it
rather than adding a second mechanism: two supervisors that decide at the same
instant that nothing is running produce one serving recorder and one that exits,
and the one that lost reports having lost. Keeping the *desktop application* to
one is a separate problem with a separate answer, a session-local named mutex
([ADR 0006](adr/0006-recorder-lifetime-and-supervision.md)).

**The recorder is shutting down.** Ctrl+C stops the listener first, then stops
any recording and waits for its file to be finished, then exits. Connection
threads own nothing that needs finalising and go with the process. The
[`shutdown`](#shutdown) command takes exactly that path — it is what stops the
listener — so there is one shutdown rather than two, and a client that sent it
sees the reply first and then the endpoint go.

**A client that connects and then stalls.** Reads on a connection block with no
timeout, so a peer that announces a frame and never sends it holds that
connection's thread until the recorder exits. The blast radius is bounded by the
8-connection cap and nothing else in the process waits on those threads, so a
stalled client costs a thread and cannot affect a recording. It is a real
limitation rather than a designed behaviour: giving reads a deadline needs
overlapped I/O, which is the same machinery [ADR
0005](adr/0005-named-pipe-control-protocol.md) declined for events, and it is
only worth taking on if something other than a local process running as the same
user — which could simply kill the recorder instead — can cause it.

## Trying it by hand

```powershell
cargo run -p clipped-recorder -- serve --endpoint scratch
```

It prints one line and then serves:

```text
ready endpoint=\\.\pipe\scratch
```

That line is the hook for whatever started it — a supervisor, or a test. From
another shell, a named pipe is an ordinary file to PowerShell, so a handshake
and a command are a few lines:

```powershell
$pipe = New-Object System.IO.Pipes.NamedPipeClientStream('.', 'scratch', 'InOut')
$pipe.Connect(2000)

function Send-Frame($pipe, $object) {
    $json = [Text.Encoding]::UTF8.GetBytes(($object | ConvertTo-Json -Compress -Depth 6))
    $pipe.Write([BitConverter]::GetBytes([uint32]$json.Length), 0, 4)
    $pipe.Write($json, 0, $json.Length)
    $pipe.Flush()
}

function Read-Frame($pipe) {
    $length = New-Object byte[] 4
    $pipe.Read($length, 0, 4) | Out-Null
    $payload = New-Object byte[] ([BitConverter]::ToUInt32($length, 0))
    $pipe.Read($payload, 0, $payload.Length) | Out-Null
    [Text.Encoding]::UTF8.GetString($payload)
}

Send-Frame $pipe @{ type = 'hello'; protocol_version = 2; role = 'control'
                    client = @{ name = 'powershell'; version = '0' } }
Read-Frame $pipe

Send-Frame $pipe @{ type = 'request'; id = 1; command = 'get_status' }
Read-Frame $pipe
```

Being able to do that at all is one of the reasons the format is JSON.

## The TypeScript types

The desktop application's front end is TypeScript and needs a view of these
messages. It is at `packages/shared/src/ipc`, and it is **mirrored by hand from
`crates/ipc` rather than generated from it**, with a check that fails when the
two disagree. Both halves of that sentence matter, and the second is what makes
the first defensible.

```text
packages/shared/src/ipc/
  protocol.ts             every message, and the open and closed sets of wire strings
  parse.ts                a frame into one of those types, never throwing
  frame.ts                the little-endian u32 prefix and the 1 MiB limit
  protocol-schema.json    generated from crates/ipc; do not edit
  conformance.test.ts     holds the first three against the fourth
```

### Why mirrored rather than generated

Generation — `ts-rs`, `typeshare` or `schemars` and an emitter — cannot drift,
and it was the expected answer. It was not taken, for three reasons in this
order:

- **It gets this protocol's hard parts wrong.** The compatibility policy above
  lives in `#[serde(from = "String", into = "String")]` on `ErrorCode` and
  `EndReason`, in `#[serde(untagged)]` catch-alls on `ErrorDetail` and `Event`,
  and in the deliberate *absence* of one on `RecorderStatus`. Those attributes
  are exactly the ones a Rust-to-TypeScript emitter does not follow: the string
  conversions are invisible to it, and a `serde_json::Value` catch-all becomes
  `any` — which erases the distinction this document spends a section on and
  hands the interface a type that cannot be wrong because it says nothing.
- **It would put a TypeScript concern inside the protocol crate.** `crates/ipc`
  is a leaf crate that both ends depend on, and the derive would have to sit on
  the types themselves rather than beside them.
- **The interesting drift is not in the field names.** It is in what happens to
  a value neither side was compiled against, and no generator checks that. A
  check that runs real frames past both implementations does.

### What the check actually checks

`crates/ipc/src/schema.rs` **derives** a description of the protocol from the
Rust types and writes it to `protocol-schema.json`. Nothing in it is typed out
by hand:

- field names and which fields may be left out come from `serde` — each field is
  removed in turn and offered back to the deserialiser, so the answer is the
  deserialiser's rather than a reading of the attributes;
- wire strings come from serialising real values;
- every enumeration is walked through an exhaustive `match`, so a variant added
  to the Rust stops the crate compiling until it is named in the schema, beside
  the list it belongs in;
- naming a variant and still leaving it out of that list is a hole the compiler
  cannot see, so the closed enumerations — `reply`, `outcome`, `recorder_state`
  and the two envelope types — are checked a second way, against the list
  `serde` itself publishes in the error it raises for a tag it does not know.
  `event` and `error_detail` have untagged catch-alls and so never raise that
  error; their `match` is what covers them. A capability is a constant rather
  than a variant and has no `match` at all: `features::ALL` is generated from
  the same lines that define the constants, which is the same guarantee by
  another route;
- and every sample frame — including ones carrying an error code, an end reason,
  an event or a field invented after this build — records what the **real**
  deserialiser made of it.

Three tests then hold the ends together, across the two CI jobs:

| Test | Fails when |
| --- | --- |
| `a_closed_enumeration_lists_every_variant_the_deserialiser_has` (`cargo test`) | a variant exists that the schema does not list |
| `the_committed_schema_is_the_one_this_build_produces` (`cargo test`) | the committed schema is no longer what the Rust types produce |
| `conformance.test.ts` (`npm test`) | the TypeScript no longer matches the committed schema |

The TypeScript side is not free to lie to the check either: its enumerations are
the arrays its union types are built from, and its field descriptors are mapped
types over the interfaces themselves, so a descriptor that disagrees with its
interface does not compile.

The asymmetry this document is built on survives the crossing. An unknown error
code, end reason, error detail or event parses on the TypeScript side and keeps
what it could not understand; an unknown **recorder state** fails whatever
carried it, in both languages — the reply, and with it the frame, or the
`status_changed` event, which then becomes an unrecognised event rather than
costing the connection its subscription. Either way nothing renders a state the
build cannot name, and there is a sample of each case proving both sides agree.

Regenerate the schema after changing anything on the wire:

```powershell
cargo run -p clipped-ipc --bin protocol-schema
```

## How it is tested

| Where | What it covers |
| --- | --- |
| `crates/ipc/src/*.rs` unit tests | Message round trips, the frozen handshake shape, unknown versions, unknown fields, unknown codes, unknown error details, unknown events, framing including a hostile length prefix, dispatch, event routing |
| `crates/ipc/src/transport/windows.rs` tests | A real pipe: a round trip, endpoint exclusivity, connecting to nothing, stopping a blocked listener |
| `apps/recorder/tests/ipc_protocol.rs` | The whole thing against a real `clipped-recorder serve` child process: handshake, commands, every rejection path, the connection cap, a client that vanishes, a second recorder, Ctrl+C — and an `export_recording` whose MP4 is decoded frame by frame and compared packet payload by packet payload against the recording it was copied from |
| `apps/recorder/tests/supervision.rs` | Supervision against real processes that are really killed: a recorder outliving the process that started it, a second launch attaching rather than competing, a killed recorder reported and replaced, and a bounded restart policy |
| `crates/ipc/src/schema.rs` tests | That the description of the protocol the TypeScript is checked against is derived rather than asserted — a tag is never reported as optional because a catch-all absorbed it, every sample records what the real deserialiser did with it — and that the committed schema is still what this build produces |
| `packages/shared/src/ipc/conformance.test.ts` | The TypeScript mirror against that schema: every enumeration both ways, every field of every object, and every sample frame parsed to the same verdict the recorder reached |

The rejection tests each end by asserting that the *next* client is still
served. The interesting half of "a bad client is refused" is that a bad client
cannot stop the recorder serving a good one, and a test that only checked the
refusal would pass against a recorder that had closed its listener.

One test in `ipc_protocol.rs` is `#[ignore]`d because it needs a GPU, an encoder
and a desktop session: it starts, observes and stops a real recording entirely
over the protocol and validates the file it produces.

```text
cargo test -p clipped-ipc
cargo test -p clipped-recorder --test ipc_protocol
cargo test -p clipped-recorder --test ipc_protocol -- --ignored --nocapture --test-threads=1
npm test --workspace @clipped/desktop -- ipc/conformance
```

## What is deliberately not here

- **A TypeScript client.** The messages are in TypeScript
  ([The TypeScript types](#the-typescript-types)); the thing that opens the
  pipe, performs the handshake and matches replies to requests is
  [issue #217](https://github.com/wildware-uk/clipped/issues/217). Nothing in
  `packages/shared/src/ipc` does any I/O.
- **Preview frames, waveforms and thumbnails.** High-bandwidth data does not go
  through a JSON control channel; it gets its own transport decision when
  something needs it.
- **Killing the recorder.** Nothing here terminates a process. Asking one to
  exit is [`shutdown`](#shutdown), which goes over the protocol precisely so
  that the recording is finished rather than abandoned; there is deliberately
  no command that ends a recorder without finishing what it is writing.
- **Authentication.** There is none, and there should be none: the operating
  system's access control is the authentication, and a token would be a second,
  weaker copy of it. That reasoning holds only while the transport is a named
  pipe — it is the first thing to revisit if the transport ever changes.
