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
see [The TypeScript types](#the-typescript-types) — but nothing in the desktop
application opens the pipe yet, which is
[issue #217](https://github.com/wildware-uk/clipped/issues/217).

Related: SPEC.md section 5, AGENTS.md sections 5, 27, 43 and 44,
[privacy.md](privacy.md), [ADR 0002](adr/0002-separate-recorder-process.md),
[ADR 0005](adr/0005-named-pipe-control-protocol.md).

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
{"type":"hello","protocol_version":1,
 "client":{"name":"clipped-desktop","version":"0.1.0"},
 "role":"control"}
```

```json
{"type":"welcome","protocol_version":1,
 "recorder":{"name":"clipped-recorder","version":"0.1.0"},
 "role":"control",
 "features":["recording","status_events"]}
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
{"type":"hello","protocol_version":1,
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
that. Today: `recording`, `status_events`.

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
  `2`, knows the recorder is behind, and can say so — and can offer to restart
  the recorder, which is the action that fixes it ([issue #106](https://github.com/wildware-uk/clipped/issues/106)
  owns that behaviour).
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

| Command | Parameters | Reply | This build |
| --- | --- | --- | --- |
| `ping` | none | `pong` | yes |
| `get_status` | none | `status` | yes |
| `start_recording` | the `record` options, below | `recording_started` | yes |
| `stop_recording` | `recording_id` (optional) | `recording_stopped` | yes |
| `save_replay` | not yet defined | — | no — M3, [#37](https://github.com/wildware-uk/clipped/issues/37) |
| `add_bookmark` | not yet defined | — | no — M8, [#64](https://github.com/wildware-uk/clipped/issues/64) |
| `take_screenshot` | not yet defined | — | no — M8, [#67](https://github.com/wildware-uk/clipped/issues/67) |
| `apply_settings` | not yet defined | — | no — M7, [#108](https://github.com/wildware-uk/clipped/issues/108) |

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

A `recording_id` is unique for the life of the recorder process. It exists so
that a stop meant for a recording that has already ended by itself cannot stop
its successor — a real race when a window closes at the moment the user presses
the button.

This recorder records **one thing at a time**: a second `start_recording` while
one is running is refused with `already_recording` rather than queued. A second
recording means a second encoder session and a second capture loop competing
with the game the first one is recording.

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

## Commands this build cannot perform

Four commands are defined by the protocol and refused by this build, with
`not_implemented` and a detail naming the subsystem, the milestone and the
issue:

```json
{"type":"response","id":3,"outcome":{"error":{
  "code":"not_implemented",
  "message":"the replay buffer is not in this build",
  "detail":{"detail":"not_implemented","subsystem":"the replay buffer",
            "milestone":"M3","tracking_issue":37}}}}
```

They are refused **before dispatch**, so there is no handler for one to be wired
to. That is the point: a command that could be handled is a command that could
be answered "saved" by something that saved nothing (AGENTS.md sections 27 and
54). The UI is expected to render the refusal as what it is — "Save replay is
not in this build" — rather than showing a dead control.

Their *parameters* are deliberately left as an open object rather than given a
schema. Nobody knows yet what `save_replay` takes, because the thing it would
ask for does not exist; inventing a shape now would be a public API designed
against a guess, and one the milestone that builds it would have to break
(AGENTS.md section 43).

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
{"type":"event","event":"recording_failed","recording_id":"r-1",
 "error":{"code":"recording_failed","message":"the encoder stopped accepting frames"}}
```

| Stream | Events | This build |
| --- | --- | --- |
| `status` | `status_changed` | yes |
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
| `already_recording` | A recording is running, and this recorder runs one at a time. |
| `not_recording` | There is nothing to stop, or the named recording is not the one running. |
| `target_not_found` | No window matched what was asked for. |
| `recording_failed` | Capture, encoding or muxing refused. Whatever was written before the failure is still a finished file. |
| `too_many_connections` | The recorder is serving as many connections as it will. |
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
([ADR 0002](adr/0002-separate-recorder-process.md)); starting one is
[issue #106](https://github.com/wildware-uk/clipped/issues/106).

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
message saying another recorder is already listening. The full single-instance
story is [issue #106](https://github.com/wildware-uk/clipped/issues/106); what
the transport guarantees is that they cannot both serve.

**The recorder is shutting down.** Ctrl+C stops the listener first, then stops
any recording and waits for its file to be finished, then exits. Connection
threads own nothing that needs finalising and go with the process.

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

Send-Frame $pipe @{ type = 'hello'; protocol_version = 1; role = 'control'
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
  to the Rust and not to the schema stops the crate compiling;
- and every sample frame — including ones carrying an error code, an end reason,
  an event or a field invented after this build — records what the **real**
  deserialiser made of it.

Two tests then hold the ends together, one in each CI job:

| Test | Fails when |
| --- | --- |
| `the_committed_schema_is_the_one_this_build_produces` (`cargo test`) | the committed schema is no longer what the Rust types produce |
| `conformance.test.ts` (`npm test`) | the TypeScript no longer matches the committed schema |

The TypeScript side is not free to lie to the check either: its enumerations are
the arrays its union types are built from, and its field descriptors are mapped
types over the interfaces themselves, so a descriptor that disagrees with its
interface does not compile.

The asymmetry this document is built on survives the crossing. An unknown error
code, end reason, error detail or event parses on the TypeScript side and keeps
what it could not understand; an unknown **recorder state** fails the message,
in both languages, and there is a sample of each proving it.

Regenerate the schema after changing anything on the wire:

```powershell
cargo run -p clipped-ipc --bin protocol-schema
```

## How it is tested

| Where | What it covers |
| --- | --- |
| `crates/ipc/src/*.rs` unit tests | Message round trips, the frozen handshake shape, unknown versions, unknown fields, unknown codes, unknown error details, unknown events, framing including a hostile length prefix, dispatch, event routing |
| `crates/ipc/src/transport/windows.rs` tests | A real pipe: a round trip, endpoint exclusivity, connecting to nothing, stopping a blocked listener |
| `apps/recorder/tests/ipc_protocol.rs` | The whole thing against a real `clipped-recorder serve` child process: handshake, commands, every rejection path, the connection cap, a client that vanishes, a second recorder, and Ctrl+C |
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
- **Starting or supervising the recorder.** Deciding that no recorder is running
  and doing something about it belongs to
  [issue #106](https://github.com/wildware-uk/clipped/issues/106). This protocol
  only reports the fact.
- **Authentication.** There is none, and there should be none: the operating
  system's access control is the authentication, and a token would be a second,
  weaker copy of it. That reasoning holds only while the transport is a named
  pipe — it is the first thing to revisit if the transport ever changes.
