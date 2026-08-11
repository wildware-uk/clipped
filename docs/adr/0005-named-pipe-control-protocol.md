# 0005. A named pipe carries the control protocol between the UI and the recorder

- Status: Accepted
- Date: 2026-08-11
- Issue: [#49](https://github.com/wildware-uk/clipped/issues/49)

## Context

[ADR 0002](0002-separate-recorder-process.md) put the recorder in a process of
its own and left the protocol between it and the desktop application as a
consequence to be designed: "the IPC protocol becomes a first-class part of the
system, to be designed, versioned, documented and tested like any other
compatibility surface". This record is the transport half of that. The messages
and the compatibility policy are in [ipc.md](../ipc.md).

What the channel has to carry is small and slow: start, stop, status, errors,
settings. A few messages a second, a few hundred bytes each. What it has to be
is trustworthy, because it can start and stop recordings and it reports what is
being recorded — a channel another user's process could open would be a way to
watch somebody's screen recording state, and a channel any local process could
open would be a way for a web page in a browser to stop a recording.

Three constraints bound the choice:

- **AGENTS.md section 14** and [privacy.md](../privacy.md): new network
  communication is never introduced silently. `privacy.md` classifies a loopback
  listener as network communication, requires it to be documented in a register
  and to authenticate its callers, and explicitly says that Windows named pipes
  used for on-machine IPC are *not* network communication.
- **[ADR 0002](0002-separate-recorder-process.md)**: "the local endpoint is an
  attack surface. It must be reachable only by the user who owns it, and must
  never become a network listener."
- **The recorder must not fall over** (AGENTS.md section 17). Whatever the
  transport is, a malicious or buggy peer must not be able to exhaust it.

High-bandwidth data — preview frames, waveforms, thumbnails — is explicitly out
of scope. ADR 0002 already says that needs its own decision, and making it here
would pick a transport for two problems that have different requirements.

## Decision

**A Windows named pipe**, at `\\.\pipe\clipped-recorder.<session>`, created with
three properties that are the whole of the decision:

- an explicit DACL — `D:P(A;;GA;;;<the process token's user SID>)` — so only the
  account that created it can open it;
- `PIPE_REJECT_REMOTE_CLIENTS`, so it cannot be reached over SMB from another
  machine;
- `FILE_FLAG_FIRST_PIPE_INSTANCE` on the first instance, so a second recorder on
  the same endpoint fails at once instead of half-serving.

Messages are **JSON**, each in a frame prefixed by a little-endian `u32` length,
with a 1 MiB limit checked before anything is allocated.

The endpoint is named per sign-in session because the pipe namespace is
machine-wide, and callers name an *endpoint name* rather than a path so that
nothing can point it at `\\some-server\pipe\…`.

The boundary of this decision: it covers the control channel. It does not commit
to a transport for bulk media, and it does not commit to JSON for anything but
this protocol.

## Alternatives

### A TCP socket on loopback

The obvious portable answer, and the one most desktop applications with a web
front end reach for. Its case is real: it is the same code on every platform, it
needs no Windows-specific API and no security descriptor built by hand, every
language has a client, and a WebSocket over it would let the Tauri front end
talk to the recorder without going through the Rust host at all.

It was rejected on access control before anything else. **A loopback listener is
reachable by every process on the machine** — every other signed-in user's
session, every background service, and any web page a browser will let issue a
request to `127.0.0.1`. There is no operating-system check to lean on, so the
channel would have to authenticate its callers itself: a token generated at
startup, written to a file only the user can read, read by the UI, and compared
on every connection. That is a home-made copy of an access check Windows already
performs correctly, with a secret at rest and a file to get the permissions right
on.

Two further costs. A port can be taken, firewalled, or — worse — answered by
whatever bound it after a restart, so "connect succeeded" is not "I am talking to
the recorder"; a pipe name resolves to the object or to nothing. And
`privacy.md` classifies it as network communication, so it would need a register
row, a declaration in every pull request that touched it, and the authentication
above, for a channel that never leaves the machine.

It would win if Clipped were being ported to a platform with no equivalent
local-only channel. There is no such platform in scope: Linux has Unix domain
sockets with `SO_PEERCRED`, which have the properties chosen here, and the
transport is behind an interface small enough to add one beside this without
touching anything above it.

### Shared memory with an event pair

The fastest option, and the one to want if this channel ever carries frames.

Rejected as premature and as the wrong shape for the traffic. A control channel
sending a few hundred bytes a second gains nothing measurable from avoiding a
copy, and shared memory gives none of what this channel actually needs: no
framing, no ordering, no notion of a connection closing, and no way to tell that
the peer died rather than stopped writing. All of that would have to be built on
top, correctly, in a process that must not fall over. It stays the right answer
for preview frames, which is a separate decision.

### Component Object Model, or a Windows RPC interface

The Windows-native answers, with real access control and generated stubs.

Rejected on weight. Both need interface definitions, registration and a
marshalling layer, and both make the recorder harder to drive from anything that
is not a Windows program — including the test suite and a contributor with a
shell. The protocol here is eight commands and two events; a length prefix and
some JSON is proportionate to that, and it can be read in a bug report.

### One duplex connection instead of two

Not a transport choice so much as a shape, and worth recording because the
result looks unusual: commands and events travel on *separate* connections, each
used in one direction at a time.

A single duplex connection was the first design. It was rejected because a
synchronous Windows file handle serialises the operations issued against it, so
pushing an event while a read is outstanding on the same handle needs overlapped
I/O and a completion loop. That is a meaningful amount of `unsafe` and a class of
bug — a cancelled or leaked `OVERLAPPED` — inside the process that must not fall
over, bought for the saving of one `CreateFile`. Two connections cost the client
one extra open and cost the recorder one extra thread, and they keep every handle
in this crate on ordinary blocking reads and writes.

If the protocol later needs genuinely concurrent request pipelining, overlapped
I/O is where to go, and this is the paragraph to reread.

## Consequences

- **No network communication is introduced.** [privacy.md](../privacy.md)'s
  register stays empty, and no pull request touching this protocol has to tick
  its network item. If the transport is ever changed to a socket, that is a
  privacy decision requiring a register row and authentication, not an
  implementation detail.
- **Other users cannot reach the endpoint, and administrators can.** The DACL is
  enforced by the operating system before a byte is read, which is stronger than
  any token scheme this project would have written. `SYSTEM` and Administrators
  can take ownership of any object and rewrite its ACL; the documentation says so
  rather than implying a guarantee Windows does not offer.
- **The protocol is Windows-only, and the rest of the crate is not.** Messages,
  framing and dispatch are platform-independent and unit-tested anywhere; only
  `crates/ipc/src/transport/windows.rs` is not. A Linux port adds a file beside
  it.
- **A peer cannot make the recorder allocate.** The frame limit is checked before
  the allocation and before the payload is read, and concurrent connections are
  capped, because the endpoint is reachable by anything running as the user —
  including a buggy script, not only the desktop application.
- **The protocol can be read, and driven, by hand.** JSON over a length prefix
  means a bug report can carry a legible trace and a contributor can drive the
  recorder from PowerShell in a dozen lines. That is worth more here than
  compactness, which nothing in this traffic profile would notice.
- **The desktop application needs a TypeScript view of these messages.** JSON
  makes that a matter of declaring types rather than writing a decoder, but the
  types still have to be generated or mirrored and kept honest, and that is
  outstanding work rather than a solved problem.
- **Two connections is a shape clients have to know about.** A client that opens
  one and expects events on it will wait for ever. It is in the handshake — a
  connection states its role — and it is the first thing
  [ipc.md](../ipc.md) explains about connections.
