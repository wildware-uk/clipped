# 0009. The recorder registers the global hotkeys, and a press becomes a protocol command

- Status: Accepted
- Date: 2026-08-13
- Issue: [#232](https://github.com/wildware-uk/clipped/issues/232)

## Context

`RegisterHotKey` gives a combination to **exactly one process**. Windows keeps
one table per combination, the second caller is refused with
`ERROR_HOTKEY_ALREADY_REGISTERED`, and there is no per-application variant of
that registration (`docs/hotkeys.md`). So "which process registers Clipped's
hotkeys" is not a preference; it is a question with one answer that has to be
chosen deliberately.

Clipped is two processes, and the two pull in opposite directions.

The **recorder** is the process that owns recordings. It starts at login, is
expected to run for days, and outlives every window the user opens
([ADR 0002](0002-separate-recorder-process.md),
[ADR 0006](0006-recorder-lifetime-and-supervision.md)). It is also the only
process that can *perform* any of what a hotkey asks for: a bookmark is a
position in a recording it is making, a screenshot is a frame it already
captured, and stopping a recording is a container it has open.

The **desktop application** is the process the user configures. It has the
screen that will bind a combination
([issue #54](https://github.com/wildware-uk/clipped/issues/54)), it is where a
conflict has to be *seen*, and it already knows what the user is looking at —
its Record button and its tray item record the foreground application, from a
window hook it installs.

Three requirements bound the answer:

- **A hotkey has to work with no window open.** That is most of the point:
  SPEC.md section 33 has closing the window minimise to tray, and a recorder
  started at login on a machine where nobody has opened Clipped for a week is a
  supported state (ADR 0006).
- **Starting a second copy of Clipped must not silently cost the user their
  hotkeys.** Two copies of Clipped are the most likely thing to conflict with
  Clipped, and the failure is invisible: the second copy takes the combination,
  or fails to, and either way somebody's key stops working with no message.
- **A press must not make a capture thread wait** (AGENTS.md section 20), and
  must never be swallowed (AGENTS.md section 54).

Not in scope: which combination is bound to what, which is the configuration API
and issue #54's screen; and changing a binding without restarting, which is
[issue #233](https://github.com/wildware-uk/clipped/issues/233).

## Decision

**The recorder registers every global hotkey. The desktop application registers
none, and reads what happened over the protocol.**

Concretely, and in the order it happens:

- `clipped-recorder serve` **binds the endpoint first**, and registers hotkeys
  afterwards. That ordering is the whole of the single-registration guarantee:
  the named pipe is already exclusive (`FILE_FLAG_FIRST_PIPE_INSTANCE`,
  [ADR 0005](0005-named-pipe-control-protocol.md)), so a second recorder in the
  session has exited saying the name was taken before it could ask Windows for a
  combination. Nothing new is locked, and there is no second answer to "how many
  recorders are there".
- The bindings come from `clipped_session::config`, resolved once at start-up
  from the user's settings file, exactly as everything else a recording is made
  with is (`docs/configuration.md`).
- **A press becomes the same `Command` the window would have sent**, dispatched
  through the same `CommandHandler`. Pressing the bookmark key and clicking Add
  Bookmark reach one implementation, take one validation path and produce one
  kind of failure. The recorder-side module holds a `CommandHandler` and not the
  recording state, so it cannot reach past the protocol even by accident.
- **The only difference between a press and a request is where the answer
  goes.** A request is answered to the client that sent it; a press has no
  client, so its outcome is logged — at `info` for what happened and `warn` for
  what did not.
- **Handlers exist only for what this build performs**: `add_bookmark`,
  `take_screenshot`, and the stop half of `toggle_recording`. An action with no
  handler reports itself as unhandled when pressed, carrying the milestone and
  issue that would build it, and is shown the same way before it is pressed.
  Nothing is wired to a handler that quietly does nothing.
- **Where every hotkey stands is a protocol question**, `get_hotkeys`, and the
  window draws it on Settings → Hotkeys. It is a question rather than an event
  because registration happens when the recorder starts, which is usually before
  any window exists to be told.

The boundary: this says who registers and what a press turns into. It says
nothing about which combination is bound to what, and nothing about how the
window presents any of it beyond requiring that it present it truthfully.

## Alternatives

### The desktop application registers, and sends a command per press

The obvious design, and the one most overlay-style applications use. It puts the
registration in the process that has the settings screen, so binding a
combination and registering it are one operation with no round trip and no
restart — which is most of issue #233's difficulty gone. It is also the only
process that knows what the user is looking at, so `toggle_recording` could
start a recording of the foreground application, which is the thing a user most
wants a hotkey to do and which the chosen design cannot yet do
([issue #416](https://github.com/wildware-uk/clipped/issues/416)).

It was rejected on lifetime, which is the same axis ADR 0002 turned on. A hotkey
that works only while a particular window process is alive is a hotkey scoped to
"the application is open", and quitting is a normal thing for a user to do. The
recorder is what starts at login and what a user is told will keep recording;
tying the keys to the other process would mean that a machine where somebody has
enabled start-at-login and never opens the window has a recorder running and no
hotkeys at all, with nothing on screen to say so.

There is a second cost that only shows up later. Every press would become a
round trip, so a press during the seconds when the link is down — a recorder
being restarted, a supervisor backing off — is a press that does nothing, and
the user would have to be told why. Putting the registration in the process that
performs the action removes that failure mode rather than reporting it.

**What would make it win**: the foreground question. If the window turns out to
be the only place "what would this record" can honestly be answered, and issue
#416 cannot answer it in the recorder, then the balance shifts — a hotkey that
starts a recording is worth more than one that survives the window closing. The
answer to that is issue #416's, not this record's.

### Both processes register, each taking the actions it can perform

Split by capability: the recorder takes the bookmark and the stop, the window
takes anything that needs a foreground or a screen.

Its case is that each action is registered by the process that can do it, with
no round trip anywhere. It was rejected because it makes the *set* of working
hotkeys depend on which processes happen to be running, which is the least
explicable state a user could be in — some keys work, some do not, and which is
which changes when a window is closed. It also doubles the conflict surface: the
two processes would have to agree not to ask for the same combination, which is
a distributed agreement problem invented to avoid a function call.

### A lock of the hotkeys' own, so that whoever holds it registers

A named mutex — `Local\clipped-hotkeys` — claimed before registering, so any
process may register and only one does.

Close, and rejected as redundant. Every question it answers is already answered
by the endpoint: `serve` cannot start twice in a session, so ordering the
registration after the bind gives the same guarantee with nothing to acquire,
nothing to release and no path on which a crashed process leaves a claim behind.
ADR 0006 rejected a lock file for single-instance for a related reason and chose
to build on the endpoint's exclusivity; this follows that. **What would make it
win**: a second process that legitimately needs to register something — an
overlay process, say ([issue #53](https://github.com/wildware-uk/clipped/issues/53)) —
at which point who-registers-what stops being decidable by process identity.

### A low-level keyboard hook instead of `RegisterHotKey`

`WH_KEYBOARD_LL` sees every keystroke, so there is no registration to own and no
conflict to report, and it reaches games that suppress system hotkeys.

Rejected outright, and not by this record: `docs/hotkeys.md` and AGENTS.md
section 34 already rule it out. A process reading the whole keyboard is what a
keylogger is built from and what anti-cheat software treats as hostile, and
getting a user banned from a game is a worse failure than a hotkey that does not
fire. It is listed here only because "which process registers" stops being a
question if this is chosen, and a future reader should know the answer was
considered and refused.

## Consequences

- **Hotkeys work with no window open**, which is what makes them worth having.
  They are registered by the process the user was told keeps running.
- **Changing a binding needs the recorder restarted.** The bindings are resolved
  once at start-up, so editing the settings file takes effect at the next start.
  That is a real cost and it is [issue #233](https://github.com/wildware-uk/clipped/issues/233);
  it is worse under this decision than under the window-registers alternative,
  because the process that must be restarted is the one holding the recording.
- **A hotkey cannot start a recording yet.** The recorder has no notion of "the
  window the user is in", and inventing one is
  [issue #416](https://github.com/wildware-uk/clipped/issues/416). Until then the
  toggle key stops a recording and refuses, by name, to start one. That refusal
  is honest and it is still a gap.
- **The window's view of the hotkeys is a copy and can be stale.** It is asked
  for when the Settings screen is opened, and nothing pushes an update, because
  nothing changes a registration while the recorder runs. The moment issue #233
  makes one change, this needs an event as well.
- **A conflict is visible only to somebody who looks.** Settings → Hotkeys shows
  it; nothing interrupts anybody with it, which is
  [issue #417](https://github.com/wildware-uk/clipped/issues/417). For a hotkey
  the user cannot have, "visible where you would go to fix it" is a floor rather
  than an answer.
- **The protocol gains a command and a capability.** `get_hotkeys` and the
  `hotkeys` feature are a compatibility surface now, and a recorder older than
  the window it is talking to refuses the command by name — which is what the
  feature exists to make visible before the screen is drawn (`docs/ipc.md`).
- **The recorder links `clipped-hotkeys`**, which is a leaf crate that depends on
  nothing else in the workspace, so nothing of the window comes with it. A
  reviewer should push back on anything in `apps/recorder/src/hotkeys.rs` that
  knows what a recording is: it holds a `CommandHandler`, and that is the whole
  of its reach.
- **What to watch**: whether users report keys that do nothing. The recorder logs
  every press it could not act on with the action, the combination and the
  refusal, so a support bundle answers "the hotkey does nothing" without
  guesswork. If the common answer turns out to be "a conflict nobody saw",
  issue #417 is the fix and this record is unchanged; if it turns out to be
  "I pressed it and there was no recording", issue #416 is.
