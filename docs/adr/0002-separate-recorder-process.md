# 0002. The recorder runs as an independent process from the desktop UI

- Status: Accepted
- Date: 2026-08-10
- Issue: [#6](https://github.com/wildware-uk/clipped/issues/6)

## Context

SPEC.md section 5 states that the recording engine should preferably run as an
independent process, and that if the UI crashes, recording must continue.
AGENTS.md section 17 says the same thing from the other direction: a UI failure
must not destroy an active recording.

The two components have very different lifetimes and very different risk
profiles.

The recorder is a background process that starts at login and is expected to
stay up for days (AGENTS.md section 59). While a game is running it is doing
real-time work with hard deadlines, and it holds resources — encoder sessions,
GPU textures, audio clients, open file handles — that cannot be recreated
mid-session without a visible gap in the recording.

The desktop application is a Tauri window: a Rust host plus a WebView2 browser
engine, showing settings, a library, video playback and a clip editor. The user
is expected to close it (SPEC.md section 33 has closing the window minimise to
tray, which is to say the window's natural state is "not there"). It renders
video, decodes thumbnails and runs an editor, so it is where memory
exhaustion, GPU-related crashes and third-party webview faults will happen. It
is also the component that gets updated, restarted and iterated on most.

The question is whether a fault in the second can reach the first.

## Decision

The recorder is a **separate executable with its own process lifetime**. All
capture, encoding, muxing, session management and game detection happen inside
it. The desktop application is a client that connects to it over a local IPC
channel, sends commands and receives status.

Concretely:

- `apps/recorder` is the process that owns recordings. It is what runs at
  login, and it can be run entirely without a UI from its own command line.
- `apps/desktop` may start at any time, may be closed at any time, and may
  crash at any time. None of those events touch an active recording.
- No crate under `crates/` depends on anything in `apps/desktop` or
  `packages/`. `apps/desktop` and `packages/` are deliberately not Cargo
  packages so that this cannot happen by accident, and
  `tests/integration/tests/workspace_layering.rs` asserts it.
- The recorder never blocks on the UI. If no client is connected, it records
  anyway.

## Alternatives

### One process, capture on a background thread

The obvious design, and the one most desktop applications use: a single binary,
a UI thread, and capture threads beside it.

Its case is real and it is mostly about cost. There is no protocol to design,
version or test; no serialisation on the hot path; state is shared directly
instead of being copied across a boundary; there is one binary to build, ship,
update and sign; one log file and one stack trace when something goes wrong;
and a contributor can follow a control flow from a button press to an encoder
call by reading code rather than by reading a message schema. Everything about
day-to-day development is easier.

It was rejected because in-process isolation is not isolation. A panic that
aborts, a webview host fault, an out-of-memory abort while scrubbing a 4K
timeline, or an unhandled fault in any dependency the UI drags in, takes down
the whole process and the recording with it. There is no way to write the
recording side defensively enough to survive that, because the failure is
address-space wide. Updates have the same shape: replacing the binary means
stopping recording. Given that the requirement is not "recording usually
survives" but "recording continues", this design cannot satisfy it.

### The UI as a thin shell over an in-process engine

Keep Tauri's own architecture and put the engine in Tauri's Rust core. Tauri
already runs the webview in a separate process, so the front end can crash
without taking the Rust side with it, and command handlers can call the engine
directly.

This is the closest alternative, because a large part of the isolation comes
for free and the developer experience is nearly as good as the single-process
design. It was rejected on lifetime rather than on crash isolation: in that
model the engine lives inside the process whose reason to exist is the window.
It starts when the user opens the application, exits when the user quits it,
and is the process the updater replaces. Recording would then be scoped to "the
application is open", which is the exact coupling the requirement removes — and
"quit" is a normal thing for a user to do, not a fault. It also means the
process that must not fail links Tauri and its dependency tree, against
SPEC.md section 41's instruction to avoid unnecessary dependencies in the
recording process.

### A Windows service

Run the recorder as a service so that Windows itself keeps it alive, starts it
before login and restarts it on failure.

It answers the lifetime question more thoroughly than a user process does.
It was rejected because services run in a different session from the user's
desktop, which is a poor position from which to capture that desktop's windows
and that user's audio endpoints, and because installing a service requires
elevation and makes the installer a heavier and more invasive thing than an
open-source utility should need. The supervision that a service would have
provided has to be built instead
([issue #106](https://github.com/wildware-uk/clipped/issues/106)).

## Consequences

- **The IPC protocol becomes a first-class part of the system**, to be
  designed, versioned, documented and tested like any other compatibility
  surface (AGENTS.md section 43). It is
  [issue #49](https://github.com/wildware-uk/clipped/issues/49) in M5.
- **Version skew is a real state, not an edge case.** After an update the two
  processes restart at different times, and a long-lived recorder may be older
  than the UI that just connected to it. The protocol needs a version handshake
  and a defined behaviour when the versions do not match, rather than
  discovering the mismatch through a deserialisation failure.
- **Something has to supervise the recorder**: start it if it is not running,
  notice if it dies, and decide whether to restart it mid-session
  ([issue #106](https://github.com/wildware-uk/clipped/issues/106)).
- **Diagnosis spans two processes.** A bug report needs both sides, correlated.
  This raises the value of structured logging with shared identifiers
  ([issue #5](https://github.com/wildware-uk/clipped/issues/5)) and of a support
  bundle that collects from both
  ([issue #101](https://github.com/wildware-uk/clipped/issues/101)).
- **Everything the UI displays is a copy and can be stale.** The UI shows what
  the recorder last told it. When the connection is down it must show that
  honestly rather than falling back to plausible-looking defaults
  (AGENTS.md section 27).
- **High-bandwidth data does not cross the boundary casually.** Live preview
  frames, waveforms and thumbnails need a deliberate transport decision rather
  than being pushed through the command channel.
- **The local endpoint is an attack surface.** It must be reachable only by the
  user who owns it, and must never become a network listener; adding remote
  access would be a privacy decision, not an implementation detail
  (AGENTS.md section 14).
- **A fixed cost is accepted**: a second process's memory and startup, and
  serialisation on every interaction. Against SPEC.md section 38's idle budget
  this is small, but it is not nothing and it belongs in the idle-footprint
  measurement.
- **The recorder stays usable on its own.** A headless, scriptable recorder
  driven from a command line is a by-product of this decision, and one worth
  keeping: it is how capture is tested without a UI, and it is the whole of the
  M1 milestone.
