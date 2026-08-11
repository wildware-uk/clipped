# 0006. The desktop application starts a detached recorder and supervises it, and neither stops the other

- Status: Accepted
- Date: 2026-08-11
- Issue: [#106](https://github.com/wildware-uk/clipped/issues/106)

## Context

[ADR 0002](0002-separate-recorder-process.md) made the recorder a separate
executable with its own process lifetime, and listed the work that decision
creates: "something has to supervise the recorder: start it if it is not
running, notice if it dies, and decide whether to restart it mid-session". This
record is that decision.

The constraints it has to satisfy come from three places. SPEC.md section 5 and
AGENTS.md section 17: **a UI failure must not destroy an active recording**.
[ADR 0005](0005-named-pipe-control-protocol.md) and the transport it chose: the
endpoint is already exclusive, so whatever is decided here must build on that
rather than invent a second answer. And `docs/privacy.md`: nothing surprising,
nothing hidden — which governs anything that arranges for software to start
itself.

Three questions, and they are genuinely separate:

1. **Who owns the recorder's lifetime**, given that it must outlive the window.
2. **How many of each there may be** — one recorder, and one window — which are
   different problems.
3. **What happens when the recorder is not there**: on purpose at startup, and
   by surprise mid-session.

Not in scope: stopping the recorder deliberately. Nothing in the protocol asks
it to exit, so the only way to end one today is to kill it. That is a real gap
and it belongs with the tray icon that would offer it
([issue #50](https://github.com/wildware-uk/clipped/issues/50)); see
[Consequences](#consequences).

## Decision

**The desktop application starts the recorder detached, attaches to one that is
already running by preference, and never stops it.**

Concretely, and in the order it happens:

- The window **claims a session-local named mutex** (`Local\clipped-desktop`)
  before anything else. A second launch finds it taken, says so and exits
  without touching the recorder.
- It **probes the endpoint**. A recorder that answers is attached to, whatever
  started it. Only if nothing is listening does it start one.
- It **starts the recorder detached**: `DETACHED_PROCESS`,
  `CREATE_NEW_PROCESS_GROUP`, and standard streams pointed at nothing, so no
  console event, no closing terminal and no exiting parent reaches it, and no
  pipe of the window's can make its next write fail.
- **Two recorders are prevented by the endpoint, not by a lock here.**
  `FILE_FLAG_FIRST_PIPE_INSTANCE` already makes the second `serve` on a name
  fail. Two supervisors that decide at the same instant produce one serving
  recorder and one that exits saying the name was taken, and the supervisor that
  lost reports having lost rather than counting the winner as its own — which it
  can only do because `GetNamedPipeServerProcessId` says who is actually on the
  other end.
- **The link is watched, and a loss is bounded.** A subscription to the
  recorder's `status` and `errors` streams is the liveness check; when it ends,
  the supervisor tries again after a delay, up to four times at 1, 2, 4 and 8
  seconds, and then stops and says so. The counter resets once a recorder has
  stayed reachable for a minute. Failures no retry could fix — a missing
  executable, a protocol version this build does not speak — skip the backoff
  entirely and are reported at once.
- **Recovery names the file; it does not resume the recording.** When a recorder
  is killed mid-recording, the file it was writing is a playable recording of
  everything up to about a second before the kill
  ([ADR 0001](0001-mkv-archival-container.md), and `docs/muxing.md` measures the
  bound). The supervisor reports which file that was and that it was not
  resumed.
- **Starting at login is opt-in and reversible**, through
  `clipped-recorder start-at-login enable|disable|status`, which writes one value
  under `HKEY_CURRENT_USER\…\Run`. Nothing else in Clipped writes that key.

The boundary of this decision: it says who starts the recorder and what happens
when it is not there. It says nothing about what the recorder does once it is
running, and nothing about how the window presents any of it beyond requiring
that it present it truthfully.

## Alternatives

### The recorder as a child that dies with the window

Spawn it in the ordinary way, in a job object with
`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so the operating system tears it down when
the window goes.

Its case is that nothing leaks: no orphaned process, no "why is
clipped-recorder.exe still running?", no need for a way to stop it, and a clean
uninstall. That is a genuine benefit and it is exactly what most applications
with a helper process want.

It was rejected because it inverts the requirement. SPEC.md section 5 is "if the
UI crashes, recording must continue", and a job object makes a UI crash the one
thing guaranteed to end the recording. There is no configuration of this design
that satisfies the requirement, because the mechanism *is* the coupling.

### One supervisor, in the recorder, watching the recorder

Give the recorder a second small process — a watchdog — that restarts it, so
that supervision does not depend on a window being open at all.

This is the strongest alternative and the one to revisit. It supervises a
recorder that starts at login on a machine where nobody has opened the window
for a week, which the chosen design does not: today, a recorder that dies with
no window open stays dead until the next launch. It was rejected for now on
cost against benefit — it is a third executable to build, install, sign and
supervise in its turn, and "who watches the watchdog" is a real question rather
than a joke — and because the failure it protects against has never been
observed. **What would make it win**: evidence that recorders die on their own
in the field. The `recording_failed` events and the exit reasons in the recorder's
own log are what would show that, and that evidence is worth collecting before
building a third process.

### A Windows service, or Task Scheduler at logon

[ADR 0002](0002-separate-recorder-process.md) already rejected a service, on
session isolation and on requiring elevation to install. Task Scheduler avoids
the elevation, runs in the user's session, and would give restart-on-failure for
free.

It was rejected because a scheduled task is invisible where people look. A
`Run` value appears in Settings → Apps → Startup and in Task Manager's Startup
tab, each with a switch; a scheduled task appears in neither, and somebody
wondering why Clipped starts would have to find Task Scheduler and know to look
in it. For a local-first application whose privacy document promises nothing
hidden, being listed where the operating system lists startup software is worth
more than the free supervision — particularly as the supervision it offers is
"restart it", which is the part this design already does while a window is open.

### A Startup-folder shortcut instead of the `Run` value

The same visibility in Settings, and a file the user can delete with Explorer,
which is more obvious than a registry value.

Close, and rejected only on cost: writing a `.lnk` needs COM and `IShellLink`,
which is a great deal of machinery, and a shortcut file can be broken by a
half-finished copy in a way a registry value cannot. If Clipped ever ships an
installer that already writes shortcuts, doing this there instead would be
reasonable.

### A lock file for single-instance

The portable answer, and the one a future Linux port would need.

Rejected because a lock file outlives the process that made it. A window that
crashed — which is the failure mode the whole design assumes — would leave a
file behind, and the next launch would have to decide whether the process that
made it is still alive. That check is a race, and getting it wrong either locks
the user out of their own application or lets two run. A named mutex has no such
question: Windows closes the handle when the process ends, however it ends.

### Restarting for ever

Simply keep trying. The recorder is the thing that has to be running, so never
give up on it.

Rejected because a recorder that cannot start fails identically every time. A
missing runtime or a broken installation would produce an endless loop of
process creation beside a running game — which is the one place AGENTS.md
section 18 says cost must not go — and a log with one line in it a million
times. Staying down and saying why is visible, cheap, and leaves the user
something to do (AGENTS.md sections 16 and 45).

### Resuming an interrupted recording

When the recorder dies mid-recording, start a replacement and have it carry on
recording the same target.

Rejected as a promise that cannot be kept honestly. The replacement cannot
continue the file: the container was opened by a process that is gone, and
Matroska's trailer is written by whoever holds the handle. So "resuming" would
mean a *second* file, beginning at a moment the user cannot see, of a target the
recorder would have to guess is still the right one. That is a recording nobody
asked for, and joining the two afterwards is an editing operation with a hole in
it. Naming the file that exists is the whole of what can be said truthfully, so
it is the whole of what is said.

## Consequences

- **A recorder can outlive every window and cannot be asked to stop.** Nothing
  in the protocol says "exit", and a detached recorder has no console to receive
  Ctrl+C, so today the only way to end one is Task Manager. That is a real gap
  rather than a design property, it becomes user-visible the moment somebody
  enables starting at login, and it is
  [issue #220](https://github.com/wildware-uk/clipped/issues/220) — the tray's
  Quit ([issue #50](https://github.com/wildware-uk/clipped/issues/50)) is what
  would use it.
- **A recorder that dies with no window open stays dead** until the next launch.
  Supervision is a property of a window being open, which is the cost of not
  building the watchdog above.
- **The desktop crate now links `clipped-ipc`**, which
  `tests/integration/tests/workspace_layering.rs` previously forbade outright. A
  webview cannot open a named pipe, so the Tauri host is the protocol's client,
  and the alternative is a second implementation of the handshake and the
  framing inside the window. The test now allows that one crate and asserts the
  property that makes it sound: `clipped-ipc` depends on no other crate of the
  workspace, so nothing of the recording engine comes with it.
- **`clipped-ipc` is no longer only the wire format.** It holds the supervision
  as well, because supervision is expressed entirely in endpoints, clients and
  events, and both ends of the boundary need it — which is the same property
  that put the protocol there. A reviewer should push back on anything in
  `supervisor` that knows what a recording is.
- **The link's state is a public shape the window renders**, with four variants
  and no catch-all, for the reason `RecorderStatus` has none: a state that
  cannot be determined must be shown as unknown rather than guessed
  (AGENTS.md section 27).
- **The restart policy's numbers are a promise.** Four attempts over fifteen
  seconds is what a user will see; loosening it is a change to behaviour and not
  a way to quieten a test.
- **`start-at-login` writes to a real machine.** Its tests therefore run against
  a scratch key of their own and remove it afterwards, because a test that
  exercised the real `Run` value would arrange for the machine running it to
  start a recorder at every sign-in.
- **Nothing here is network communication.** The endpoint is a named pipe, which
  `docs/privacy.md` names explicitly as not network communication, and the
  registry value is local. The register in `docs/privacy.md` is unchanged.
