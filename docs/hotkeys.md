# Global hotkeys

**Status: the service exists and is tested; nothing calls it yet.**
`crates/hotkeys` registers combinations, reports the ones another application
already owns, and delivers presses to handlers without making anything wait.
Wiring it into the recorder and the desktop application is
[issue #232](https://github.com/wildware-uk/clipped/issues/232); the screen that
configures it is [issue #54](https://github.com/wildware-uk/clipped/issues/54).

A hotkey is the only part of Clipped a user touches while they are playing. It
has to work through a fullscreen game, it has to say something useful when it
cannot be registered, and pressing it must never be something the game notices.
Those three requirements decide everything below.

## The mechanism, and why it is not a keyboard hook

Windows offers two ways to be told about a key combination while somebody else
has the foreground.

| | `RegisterHotKey` | `SetWindowsHookEx(WH_KEYBOARD_LL)` |
| --- | --- | --- |
| What it sees | One combination, told to the OS in advance | **Every keystroke on the machine**, including passwords |
| What anti-cheat software makes of it | Nothing; it is how Windows expects an application to ask | A process reading the whole keyboard, which is what a cheat's input layer looks like |
| Combination already taken | Fails, loudly and specifically | Sees it anyway |
| Reaches a game with exclusive input | Not always | More often |

Clipped uses `RegisterHotKey`, and it is not a close decision. AGENTS.md
section 34 rules out techniques likely to resemble cheats and says plainly that
user account safety comes before richer behaviour: a recorder that gets somebody
banned from a game has failed at something far more important than a hotkey. A
low-level keyboard hook is also indistinguishable from a keylogger to anybody
who looks — and they should look, because a recorder is exactly the kind of
program that ought to be looked at.

The price is the last two rows of that table, and the rest of this section is
about paying it honestly rather than pretending it is not there.

### What it does

Measured on this project's development machine — Windows 11 Pro build 26200,
RTX 4090, two 2560x1440 displays, `\\.\DISPLAY1` — with
`crates/hotkeys/tests/windows_hotkeys.rs` against `test-apps/fullscreen-dx11`,
which covers a whole display and asks DXGI for it exclusively the way a game
does:

| The subject | `exclusive` from its `ready` line | Frames it presented | Hotkey fired | Press to reported |
| --- | --- | ---: | --- | ---: |
| Borderless window covering the display | `no` | 121 | yes | 84 µs |
| Display held exclusively through `SetFullscreenState` | **`yes`** | 130 | yes | 53 µs |
| Borderless window covering the display | `no` | not recorded | yes | 55 µs |
| Display held exclusively through `SetFullscreenState` | **`yes`** | not recorded | yes | 50 µs |

Those are two consecutive runs of the same `#[ignore]`d test, each of which
covers both modes; the subject had the foreground in all four, which the test
asserts rather than assumes. The latency is from `SendInput` returning to the
press being reported to the caller, on an unloaded machine with a debug build.
It is a sanity check, not a benchmark: what the table is evidence for is the
`yes` column.

**Both halves of the acceptance criterion therefore hold on this machine.** A
hotkey fires while a borderless-fullscreen application has the display, and
while one holds it exclusively through DXGI.

Getting the exclusive case to happen at all takes care. Windows grants
`SetFullscreenState` only while a process that has synthesised an input event is
still running (`tests/capture/README.md` has the measurements). The hotkey test
is that process — it synthesises a zero-pixel mouse movement before starting the
subject and stays alive for the run — but a run that Windows refuses is reported
as `NOT EXERCISED` and, under `CLIPPED_REQUIRE_HOTKEYS`, fails. A green run that
never entered the case it is named for is worse than no run at all.

### What it does not do

Stated because a hotkey that silently does not work is the failure this whole
document exists to prevent.

- **A combination another application already owns will not register.** This is
  the common one and it is not hypothetical: Discord, Steam, GeForce Experience
  and NVIDIA's own overlay all claim function-key combinations, and Windows
  reserves several of its own (`Win`+`L`, `Ctrl`+`Alt`+`Delete`). Clipped is
  told, and says so — see [Conflicts](#conflicts) below.
- **Nothing reaches the secure desktop.** The UAC prompt, the lock screen and
  `Ctrl`+`Alt`+`Delete` run on a desktop no application is on. No mechanism
  available to a normal program changes this.
- **Elevation is not tested.** If an elevated application has the foreground and
  Clipped is not elevated, Windows' integrity rules apply. This has not been
  measured, and the honest answer is that we do not know; it is not asserted
  either way anywhere in the code.
- **Legacy exclusive DirectInput keyboard acquisition.** Historically a game
  that acquired the keyboard this way suppressed system hotkeys. Modern titles
  use Raw Input, which does not. Not measured — there is no controlled subject
  for it, and building one is not obviously worth it before somebody reports the
  problem.
- **Only one process can hold a combination.** Two copies of Clipped are the
  most likely thing to conflict with Clipped. `HotkeyService` reports it as any
  other conflict; deciding which process registers is
  [issue #232](https://github.com/wildware-uk/clipped/issues/232).

## The actions

SPEC.md section 34, in the order the configuration screen should list them.
`clipped_hotkeys::ACTIONS` is that list, and the name is what appears in a log
line and will appear in a configuration file.

| Action | Name | Default | What is behind it today |
| --- | --- | --- | --- |
| Save replay | `save_replay` | `Ctrl`+`F10` | Nothing: the replay buffer is M3, [#37](https://github.com/wildware-uk/clipped/issues/37) |
| Add bookmark | `add_bookmark` | `Ctrl`+`F9` | Nothing: bookmarks are M8, [#64](https://github.com/wildware-uk/clipped/issues/64) |
| Take screenshot | `take_screenshot` | — | Nothing: screenshots are M8, [#67](https://github.com/wildware-uk/clipped/issues/67) |
| Start or stop recording | `toggle_recording` | — | Recording exists; a handler has to be supplied |
| Mute microphone | `mute_microphone` | — | Nothing mutes a microphone mid-recording, [#234](https://github.com/wildware-uk/clipped/issues/234) |
| Toggle microphone | `toggle_microphone` | — | As above |
| Open overlay | `open_overlay` | — | Nothing: the overlay is M5, [#53](https://github.com/wildware-uk/clipped/issues/53) |

The two defaults are the two SPEC.md names: `Ctrl`+`F10` in section 7 ("Manual /
Replay Buffer") and `Ctrl`+`F9` in section 25 ("Manual Bookmarks"). The other five
start unbound on purpose: binding all seven would take five more combinations
away from every other application on the machine before the user has asked for
any of them.

**A press with nothing behind it says so.** It is reported as `Unhandled`,
carrying the milestone and issue where there is one — "Save replay is not in
this build: the replay buffer arrives in M3 (issue #37)" — and as "nothing in
this process handles Start or stop recording" where the subsystem exists and
this process simply was not given a handler. Nothing is ever swallowed, and no
handler is ever faked to make a key appear to work (AGENTS.md section 54).

## Conflicts

`RegisterHotKey` fails with `ERROR_HOTKEY_ALREADY_REGISTERED` when something
else has the combination. Clipped attempts every binding it was given and
reports the outcome of each, so one taken combination costs the user that
combination and not the rest:

```rust
for conflict in service.registration().conflicts() {
    eprintln!("{conflict}");
}
```

```text
Ctrl+F10 could not be Clipped's shortcut for Save replay: another application
already uses it. Discord, Steam, NVIDIA's overlay and GeForce Experience all
claim function-key combinations, and Windows reserves a few of its own. Choose a
different combination, or close the application that has this one and try again.
```

That is the shape AGENTS.md section 45 asks for: what failed, who is likely to
have it, and what to do next. A refusal that is *not* a conflict carries the
`HRESULT` instead, because a code belongs in the diagnostics rather than in the
sentence a user reads.

`Registration` also answers the two questions the configuration screen needs and
nothing more: what every action is bound to, and whether anything in this
process handles it. A bound, registered hotkey with no handler is still a key
that does nothing, and a screen that showed it as working would be lying.

Conflicts between two *Clipped* actions never reach Windows at all: `Bindings`
refuses the second one, naming the action that already holds the combination.

## Combinations

Written and parsed the same way — `Ctrl+Alt+Shift+F10` — in any order and any
case, with `Control`, `Windows` and `Super` accepted as aliases. Bindable keys
are `F1` to `F24`, the letters, the top-row digits, and `Space`, `Insert`,
`Delete`, `Home`, `End`, `PageUp`, `PageDown`, `PrintScreen` and `Pause`.

Two rules are enforced before Windows is asked:

- **A letter, digit or `Space` needs `Ctrl`, `Alt` or the Windows key.** `Shift`
  does not count: `Shift`+`A` is how a capital A is typed. Registering a bare
  typing key takes that keystroke away from every application on the machine,
  including the one the user is typing their password into.
- **A combination belongs to one action.** Binding `Ctrl`+`F10` to two actions
  is refused with a message naming the one that has it.

Bare function keys and the navigation cluster are allowed: `F9` on its own is a
normal thing to bind and takes nothing away from typing.

The numeric keypad is deliberately absent. Its virtual-key codes are different
from the top-row digits, and a binding made on a keyboard with a keypad that
silently does nothing on a keyboard without one is worse than a binding that
could not be made.

## Threading

**A hotkey press never makes another thread wait.** That is an architecture
requirement rather than a performance note: saving a replay writes a file, which
is a syscall a capture loop must not be behind (AGENTS.md section 20).

```text
  caller's thread          hotkey thread                one thread per
  ───────────────          ─────────────                handled action
  HotkeyService::start ──▶ RegisterHotKey               ─────────────────
                           GetMessageW   ──WM_HOTKEY──▶  press ──▶ handler
  HotkeyService::stop  ──▶ UnregisterHotKey
```

- **One hotkey thread.** `RegisterHotKey` with a null window posts `WM_HOTKEY`
  to the calling thread's queue and `UnregisterHotKey` must be called from the
  same thread, so one thread registers, pumps and unregisters. Every Win32 call
  in the crate is in `src/service/windows.rs`, and nothing else in the process
  may call either function.
- **It runs no handlers.** A press becomes a map lookup, an atomic increment and
  a non-blocking send, and the thread is back in `GetMessageW`. A handler that
  blocks for a second costs the next press nothing —
  `a_second_hotkey_arrives_while_the_first_handler_is_still_busy` presses two
  real keys and measures it.
- **One worker thread per handled action**, created when the service starts and
  never after. Across actions handlers are concurrent: a save that takes a
  second does not delay a bookmark. Within one action they are serial and in
  order, because two saves at once would be two writers for one buffer.
- **Nothing is shared but the queues**, and the press side only ever `try_send`s
  to one. There is no lock anywhere on the press path.
- **Four presses of one action may be waiting.** A handler that is busy has
  already taken its own press off the queue, so a stuck handler absorbs five —
  the one it is running and four behind it — and the sixth is the first reported
  as dropped rather than waited for: somebody hammering the save key wants one
  clip and a responsive machine, not eight clips. The drop is counted, logged
  and delivered to the caller.
- **Stopping waits** for the handler that is running, deliberately: a replay
  being written when the user quits should finish being written (AGENTS.md
  section 17). That wait is on the thread that calls `stop`, never on a capture
  thread. Dropping the service without stopping it does the same thing, so no
  path leaves a combination registered or a handler thread running.

Nothing in this crate is called from a capture or encode thread, and nothing in
it can be: the only entry points are `HotkeyService::start`, `stop`, and reading
the event channel.

## Testing it

The dispatch rules need no keyboard and no desktop, and are unit tests in
`src/dispatch.rs`: what a slow handler delays, what an unhandled action reports,
what a full queue does, what a panicking handler does. They run everywhere,
including on a machine that is not Windows.

Everything that needs Windows is in `crates/hotkeys/tests/windows_hotkeys.rs`,
which registers real combinations and presses real keys with `SendInput`. It
chooses `Ctrl`+`Alt`+`Shift`+`F13` upwards, picked from the process identifier
so two checkouts running the suite at once do not fight over one registration,
and serialises its injections so two tests cannot interleave half a chord each.
Four of its five tests run in `cargo test --workspace`:

```text
cargo test -p clipped-hotkeys
```

The fifth takes over a display and is `#[ignore]`d. It is the one that answers
the acceptance criterion, and it needs an interactive shell — see below.

```powershell
cargo build -p clipped-fullscreen-dx11
$env:CLIPPED_REQUIRE_HOTKEYS = "1"
cargo test -p clipped-hotkeys --test windows_hotkeys -- --ignored --nocapture --test-threads=1
```

`CLIPPED_REQUIRE_HOTKEYS` turns two skips into failures: a window station that
will not register a hotkey, and a session that will not accept injected input.
Without it, a machine that cannot run these tests prints `SKIPPED (hotkeys): …`
and passes, which is the right default for somebody who just ran the suite and
useless as evidence. **Set it whenever a result is being recorded.** A
combination that registered, a keystroke Windows accepted, and then no press is
never a skip: that is the defect these tests exist to catch.

CI does not set it yet, though a GitHub-hosted runner has now been observed
doing both. The four tests that are not `#[ignore]`d — including the one that
injects a keystroke with `SendInput` — ran and passed on `windows-latest` with
no `SKIPPED (hotkeys)` line anywhere in the job log. So the runner registers
combinations and accepts synthetic input, and setting `CLIPPED_REQUIRE_HOTKEYS`
in CI is a decision about how much a hosted runner's input session should be
trusted to stay that way, not an unknown —
[issue #235](https://github.com/wildware-uk/clipped/issues/235).

### Reading a fullscreen run

```text
[subject] ready hwnd=0x0000000000900936 client=2560x1440 fps=60 presentation=fullscreen-exclusive exclusive=yes monitor=\\.\DISPLAY1 tone=off
[info] mode=exclusive exclusive=yes subject-has-foreground=yes hotkey=Ctrl+Alt+Shift+F23
[result] mode=exclusive exclusive=yes delivered=yes latency=50.6µs dispatch=28.7µs
```

`exclusive=yes` is the field that decides whether the run means anything for the
exclusive case; `exclusive=no` means Windows refused the transition and the
subject was a borderless window covering the display, which is a real case in
its own right — it is what a game in "fullscreen (windowed)" mode is — but says
nothing about the other one. `subject-has-foreground=yes` is asserted: a press
that reached Clipped while nothing was in front of it would prove nothing.

`latency` and `dispatch` are two different measurements, and neither is a
benchmark — they are sanity checks on an unloaded machine, taken once per run.

- **`latency`** is the test calling `SendInput` to the message loop taking the
  press off its queue. Most of it is Windows: the input stack, the desktop, and
  the scheduler getting round to the hotkey thread. Clipped can make this worse
  and cannot make it much better.
- **`dispatch`** is that same message-loop timestamp to the handler's first
  instruction on the handler's own thread — a map lookup, an atomic increment, a
  `try_send` and a thread wake-up. This is the part the crate is responsible
  for, and it is the number to watch if the dispatch model ever changes.

They are taken from different clocks in different threads by construction, so
they cannot be the same number by accident. Neither is asserted; what the test
asserts is that the handler ran at all, within `DELIVERY`.
