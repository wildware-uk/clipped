# The desktop application

Clipped's window: a [Tauri](https://tauri.app) shell hosting a React interface,
in `apps/desktop`, with the components and design tokens it is drawn from in
`packages/ui` and the types both sides share in `packages/shared`.

This document covers the shape of the application and the decisions behind it.
[apps/desktop/README.md](../apps/desktop/README.md) has the commands.

## Where it sits

The desktop application is a **client** of the recorder, not a host for it. The
recorder is a separate process that owns capture, encoding and session state
([ADR 0002](adr/0002-separate-recorder-process.md)), so closing or crashing this
window cannot interrupt a recording.

That is a rule about linking, and
`tests/integration/tests/workspace_layering.rs` asserts it in both directions
rather than hoping. It reads every dependency each manifest declares — including
the ones that are not members of the workspace being read, which is the only way
either question can be answered at all:

- no crate in the Cargo workspace names `clipped-desktop`, whatever the
  dependency's source;
- `apps/desktop/src-tauri` names no crate of the Cargo workspace but
  `clipped-ipc`, so the window reaches the recorder over IPC rather than by
  linking capture or encoding into its own process. That one exception is the
  protocol itself — a webview cannot open a named pipe, so the Tauri host is the
  client — and it is only sound while `clipped-ipc` depends on nothing else in
  the workspace, which the same test asserts
  ([ADR 0006](adr/0006-recorder-lifetime-and-supervision.md));
- `apps/desktop`, `packages/ui` and `packages/shared` are not Cargo packages at
  all, so turning one into a crate has to be a deliberate decision.

The layering table itself covers only workspace members, and `clipped-desktop`
is not one — which is why these are separate assertions and not something the
layer table could have caught.

The two processes speak over the IPC protocol in [ipc.md](ipc.md), and this
application now drives it: at startup it claims a single-instance name, attaches
to a recorder or starts one detached, and follows its status
([ADR 0006](adr/0006-recorder-lifetime-and-supervision.md)). The recorder status
block in the sidebar shows what the link reports and nothing else — one wording
for each of the link's four states, and one for "this is not the Clipped window",
which is what `npm run dev:web` and the tests see.

**The controls are in the notification area, not in the window** — see
[The tray](#the-tray). No screen drives the recorder: five of the seven are not
built, and the two that are draw nothing that would, because nothing they would
drive can be reached from here. A button with nothing behind it is exactly what
AGENTS.md section 27 forbids. A "Try again" control for a link that has given up
is [issue #221](https://github.com/wildware-uk/clipped/issues/221).

There is exactly one control in a screen, and it changes nothing outside the
window: the Settings screen's rail, which moves between that screen's own
sections — see [The Settings screen](#the-settings-screen).

## What the shell is

```text
┌─────────────────────────────────────────────────────────┐
│ ■ CLIPPED  Game Recorder                                │  <header>
├───────────────┬─────────────────────────────────────────┤
│ Home          │                                         │
│ Library       │                                         │
│ Games         │   <main>                                │
│ Editor        │   the screen                            │
│ Settings      │                                         │
│ ───────────── │                                         │
│ Trash         │                                         │
│ Diagnostics   │                                         │
│               │                                         │
├───────────────┤                                         │
│ RECORDER      │                                         │
│ ■ Not connect │                                         │
└───────────────┴─────────────────────────────────────────┘
```

A title strip, a fixed sidebar carrying two navigation lists and the recorder's
state, and the screen. It follows the application deck in the maintainer's
design project; the deck's custom window chrome does not, and
[issue #202](https://github.com/wildware-uk/clipped/issues/202) covers that
separately.

Both navigation lists and every route are derived from one array, `SCREENS` in
`@clipped/shared`, so a navigation item cannot point at a route that does not
exist. **Two of the seven screens have been written** — [Games](#the-games-screen)
and [Settings](#the-settings-screen). The other five each lead to a panel saying
so and naming the issue that builds it — #60 for Home and Library, #83 for
Editor, #94 for Trash, #101 for Diagnostics. Building one
replaces its placeholder route with the real screen, in `elementFor` in
`Shell.tsx`, which is the one place that knows a screen from a placeholder.

## The Games screen

SPEC.md sections 6 and 17, and issue #107. The deck draws it as a table of
detected games — name, executable, launcher, capture mode, last played — with an
Add Game control above it and a New Game Detected panel below.

**None of that is drawn, because none of it can be got.** What the screen shows
instead is the one thing about game detection this window can establish, and a
table of what the rest is waiting for.

### What the window can and cannot see

The desktop reaches the recorder over [the control protocol](ipc.md) and reaches
its own Tauri host through two commands, `recorder_link_state` and
`startup_notice`. Against that:

| The screen would show | Where it would come from | Why it cannot, yet |
| --- | --- | --- |
| The list of games | `clipped-game-detection`'s catalogue: the compiled-in `games.toml` plus the user's overlay ([game-detection.md](game-detection.md)) | No protocol command lists it, and the window has no file-system permission to read it — `capabilities/default.json` grants three `core:` permissions and nothing else. [Issue #245](https://github.com/wildware-uk/clipped/issues/245) |
| Add an executable, rename, exclude, disable capture | The same overlay, written | Same. [#45](https://github.com/wildware-uk/clipped/issues/45) owns the behaviour, #245 the way to reach it |
| Sessions, clips, favourites, storage per game | The library index | [#55](https://github.com/wildware-uk/clipped/issues/55). The session sidecars `watch` writes ([sessions.md](sessions.md)) are the only record today, and nothing can read one from here |
| Which game is being recorded now | A `status` that can name a game | [#241](https://github.com/wildware-uk/clipped/issues/241): the protocol describes a recording by its capture target, `process 4242` |

Drawing the deck's table with nothing in it was the tempting alternative and is
the one AGENTS.md section 27 rules out: an empty Game / Recording / Last played
table is indistinguishable from a machine that has played nothing, and this
build has not looked. The table above is on the screen itself, one row each,
naming the issue — the same contract the unbuilt screens keep.

### What it does show, and why that is real

**Whether anything is detecting games at all.** `describeGameDetection` in
`gameDetection.ts` has one rendering for each of the link's four states and one
for "this is not the Clipped window", and the screen is a pure function of it,
so it follows the recorder rather than restating a sentence.

One of those five says **"This recorder is not detecting games"** and the other
four say **"Not known"**, and the split is the point. **The link sees exactly one
thing: the recorder this window started or attached to.** `clipped-recorder
watch` serves no protocol, so a watcher somebody started in a terminal is
invisible to it — and the sentence directly beneath the state recommends
starting exactly that. So no rendering may say that games are going undetected
on this machine: the window has not looked, and cannot.

That includes the state for a recorder that could not be reached at all. It
reads "Not known — this window is not attached to a recorder", not "not
detecting games". It previously read the latter, which was a claim about the
machine that nothing here can make, and which contradicted its own next
paragraph.

The one rendering that says anything about detection names the recorder it is
about, and it is an inference rather than a reading. It holds:

- the supervisor starts, and can only attach to, `clipped-recorder serve`
  (`SERVE` in `crates/ipc/src/supervisor.rs`), because `serve` is the only
  subcommand that listens on the endpoint;
- `serve` does not watch for games. `clipped-recorder watch` is a separate
  subcommand that takes no `--endpoint`, which is exactly what #241's fourth
  acceptance criterion is about.

So a recorder this window can see is a recorder that is not detecting games —
which is a statement about that recorder, and about nothing else on the machine.

The screen then says the thing somebody can act on, because there is one
(AGENTS.md section 45): automatic recording is built and running, from a
terminal, as `clipped-recorder watch`. Making it something this window can start
and follow is #241.

### No controls

There are none — not a disabled Add Game, and not a row menu. The tray disables
an item and puts the reason in its own label because a notification-area menu
has nowhere else to put one; a screen does, and it uses it. A disabled control
here would say less than the row of the table that names the issue.

Which is also why this screen built nothing for
[issue #215](https://github.com/wildware-uk/clipped/issues/215). The tab strip
and the selectable chips are for the per-game detail view the deck draws behind
this list, and there is no list to open one from. #215 asks for them at the point
a screen needs them, and this one does not.

## The Settings screen

SPEC.md sections 10, 12, 15, 27 and 34, and
[issue #51](https://github.com/wildware-uk/clipped/issues/51). The deck draws a
rail of sections and panes of controls: device pickers, a recording directory,
quality presets, a container choice, hotkey bindings and a row of switches.

**The rail is here and the controls are not, because this window can neither
read nor write a setting.** That is a fact about what is built rather than about
what was finished, and it has four parts:

- the settings are `clipped_session::config` — three layers, validation, and
  each value reported with the layer it came from and whether this scope
  overrode it ([configuration.md](configuration.md)). It is exactly the shape a
  settings screen needs, and nothing here can ask it anything;
- the desktop application may link one crate of the repository's workspace,
  `clipped-ipc`, and `tests/integration/tests/workspace_layering.rs` enforces
  it. `clipped-session` sits above capture, audio, encoding and muxing, so
  naming it here would put the recording engine in the window's process — the
  separation [ADR 0002](adr/0002-separate-recorder-process.md) exists to make;
- the control protocol has no command that reads configuration, and the one
  that would write it, `apply_settings`, is refused as not implemented by every
  build ([ipc.md](ipc.md));
- reading `settings.json` from this process instead would be a second
  implementation of its versioning, migration and validation, against the file
  the user's own settings live in (AGENTS.md section 55).

[Issue #252](https://github.com/wildware-uk/clipped/issues/252) is the fix, by
either of its two routes, and it says it blocks this screen.

### What each pane carries instead

Three columns: the setting, **how it is set today**, and what has to land before
this window can hold the control. The middle column is the one that makes this a
screen rather than an apology — almost every setting *can* be changed today, and
the screen says how:

| Section | What can be changed today |
| --- | --- |
| Recording | `clipped-recorder watch --framerate 60 --codec auto …`, per run; #61 is what makes the settings file reach a recording |
| Audio | The same options, with the recorder's own warning that a recording has no audio track yet (#180) |
| Storage | `--output-directory`. The settings file has no key for it at all, which is [#307](https://github.com/wildware-uk/clipped/issues/307) |
| Hotkeys | Nothing: the hotkey service is written and no process installs it (#232) |
| Notifications | **The one thing this window's own behaviour follows**: the three switches in `notifications.json`, named with their keys and their file |
| Startup | `clipped-recorder start-at-login enable`, which no protocol command can reach — [#308](https://github.com/wildware-uk/clipped/issues/308) |

Two settings SPEC.md asks for have nowhere to be stored rather than nothing to
read them, and that is worth the distinction the screen draws: the recording
directory and the container are #307, and listing this machine's audio devices
at all is #308. Both were raised while this screen was built, because a row that
said "not yet" with no issue behind it is a promise nobody has made.

### What is checked, and against what

Everything above is a claim about code in another process, and a screen full of
those goes quietly wrong: a renamed settings key, a subcommand that moved, an
`apply_settings` that got implemented, and the screen still says what it said in
August. `apps/desktop/src/settingsConformance.test.ts` reads the definitions out
of the sources that hold them:

| The screen says | Read from |
| --- | --- |
| These are the settings, spelled this way | `SettingKey::name` in `crates/session/src/config/value.rs`, both directions — a setting the API gains and one the screen invented both fail |
| These are the notification switches | `NotificationCategory::key` in `apps/desktop/src-tauri/src/notification_policy.rs` |
| The settings file is at this path | `APPLICATION_DIRECTORY` in `clipped-logging` and `FILE_NAME` in `config::document` |
| The notification file is at this path | The bundle identifier in `tauri.conf.json` and `SETTINGS_FILE` in `notifications.rs` |
| `apply_settings` is refused | `UNBUILT_COMMANDS` in `crates/ipc/src/command.rs` |
| Nothing reads settings back | The command names `Command::from_request` parses |
| Run this command | The subcommands `apps/recorder/src/cli.rs` declares, and their options |

`SettingKey`'s own documentation asks for the first of those: it exists so that
"the settings screen can list what there is to render without this module having
to publish a second list that goes stale". This is what stops the copy of that
list on this side going stale instead.

### The rail

`SectionRail` in `packages/ui`, drawn in the shell's own `.clipped-nav__link`
because a rail entry and a sidebar item are the same thing to look at. What
differs is beneath: the sidebar's items are anchors with addresses, and these are
`role="tab"` buttons, because a section of a screen has no address of its own —
every route comes from `SCREENS`, one per screen, and both the window title and
the marked sidebar item are derived from an exact path match. Giving a section a
URL would mean nested routes and a title that understands a sub-path, and is
worth doing when something needs to link to one.

So the keyboard contract is the WAI-ARIA tab list's, and it is written out
rather than left to Tab: **one stop in the tab order**, the arrow keys moving
between sections, Home and End reaching the ends, and selection following focus.
Six sections each taking a tab stop would put five stops between the sidebar and
the pane, every time. The pane itself takes a tab stop because it holds text
rather than controls, which is what WAI-ARIA asks for and what
`jsx-a11y/no-noninteractive-tabindex` allows in its own default options —
`jsx-a11y`'s strict preset restates the rule with no options, which is why that
one line carries a suppression and a reason (AGENTS.md section 42).

## The tray

SPEC.md section 33: an icon in the notification area, a menu with the current
status and six items, and closing the window minimises to it. This is the first
part of Clipped's interface that *does* anything — until it, the window could
only watch ([issue #50](https://github.com/wildware-uk/clipped/issues/50)).

```text
  Recording process `cs2.exe`          the status, not a control
  ─────────────────────────────
  Save Replay — needs a recording with a replay buffer (#38)   disabled
  Add Bookmark
  Stop Recording
  ─────────────────────────────
  Open Library
  Settings
  ─────────────────────────────
  Stop recording and exit
```

Left-clicking the icon raises the window; right-clicking opens the menu.

### Where the parts are

| File | What it decides |
| --- | --- |
| `src-tauri/src/tray_model.rs` | What the tray should show and what each item does. Pure — no Tauri, no Windows, no I/O — and therefore the part with tests. |
| `src-tauri/src/tray.rs` | The menu, the redraws, and turning a click into a command on the recorder. |
| `src-tauri/src/tray_icon.rs` | The four marks, drawn in code. |
| `src-tauri/src/foreground.rs` | Which application the user was last in, which is what Start Recording records. |

### Four marks, and why they are shapes

AGENTS.md section 46 asks that state is never colour alone, and a tray icon is
the hardest place in the application to honour that: sixteen pixels, no label.
So each state is a different **shape**, legible in a greyscale screenshot:

| State | Mark | Tooltip |
| --- | --- | --- |
| Attached, nothing recording | an open square | `Clipped — not recording` |
| Recording | a filled disc, in the accent | ``Clipped — recording process `cs2.exe` `` |
| Connecting | four corner brackets — an outline that has not closed | `Clipped — looking for the recorder` |
| Reconnecting | the same four corner brackets | `Clipped — reconnecting to the recorder, attempt 2 of 4` |
| No recorder | a struck-through square | `Clipped — no recorder. <reason>` |

Five states and four marks: connecting and reconnecting are drawn the same,
because they are the same thing to look at — nothing is attached and something
is trying. The words are not the same, and that is the point of there being a
tooltip: it is where "which attempt, out of how many" fits.

Colour is carried as well and is the reinforcement rather than the signal. The
tooltip says the same thing in words, and the menu's first line says it again,
so the state reaches a screen reader too.

The marks are drawn as a light fill inside a dark outline, because Windows draws
the notification area dark by default and light on some machines and an icon is
not given a choice. `every_mark_reads_on_a_light_ground_and_on_a_dark_one`
measures every drawn colour against both grounds and holds the best of them to
WCAG 1.4.11's 3:1, rather than asserting that an outline exists:

| Drawn colour | On a dark taskbar (`#202020`) | On a light one (`#f3f3f3`) |
| --- | --- | --- |
| the fill, `#f3f2f2` | 14.58:1 | 1.01:1 |
| the outline, `#111111` | 1.16:1 | 17.02:1 |
| the accent, `#ec3013` | 3.88:1 | 3.79:1 |

Which is why the outline is not decoration: the fill alone would be invisible on
a light taskbar and nothing else in the application would notice. The recording
disc is the one mark whose fill carries both grounds by itself.

**There is no "buffering" mark**, which SPEC.md section 33's own list implies.
The recorder cannot report one: `RecorderStatus` is `idle` or `recording`, the
replay buffer exists as a crate and nothing in `serve` runs it, and a tray that
showed buffering would be showing something nobody measured (AGENTS.md section
27). It arrives with the recording that runs a buffer, in
[issue #38](https://github.com/wildware-uk/clipped/issues/38).

### Nothing offered that would do nothing

Every item is either something this build performs or is disabled **with the
reason in its own label**. A notification-area menu has no tooltip and no help
text, so the label is the only place a reason can go, and "greyed out with no
explanation" is the failure AGENTS.md section 27 names.

- **Save Replay** is a command the protocol defines and the recorder refuses.
  Its label is built from `UnbuiltCommand`'s own subsystem and tracking issue —
  the same two facts the recorder puts in the `not_implemented` refusal — so the
  day it is built, the menu stops claiming it has not.
- **Add Bookmark** is what that looked like the day it happened. Issue #64 built
  the bookmark store and the `add_bookmark` command, the refusal it quoted
  stopped existing, and the item became a control: live while something is being
  recorded, and disabled with `— nothing is being recorded` otherwise, because a
  bookmark is an offset into a recording and there is nothing to put one in.
  It sends no label and no colour — one click, and nowhere in a menu to type —
  so it takes the same bare mark a hotkey would (`docs/bookmarks.md`).
- **Open Library** and **Settings** raise the window and send it to that screen.
  Neither screen is written, and each says so and names the issue that builds it;
  that is a thing that happens, not a control that does nothing.
- **Start Recording** names what it would record — `Start Recording — cs2.exe` —
  and is disabled, saying which, when there is no recorder or nothing has been in
  front of the window to record.
- **Stop Recording** replaces it while something is being recorded.

`no_enabled_item_has_nothing_behind_it` asserts the property rather than the
five cases: across every link state and with and without a foreground window, an
item a user can click has something to do, and one that has not is disabled.

### What Start Recording records

The window the user was last in, by process identifier. A tray has no picker, so
the honest answer is "the application you were in when you opened this menu",
and finding that out is `foreground.rs`: an `EVENT_SYSTEM_FOREGROUND` window
event hook, which costs nothing until a foreground window changes — the same
non-polling choice `clipped-game-detection` made for process starts (issue #41),
for the same reason, because this process runs beside a game.

Two things are deliberately not remembered: this process's own windows, and the
shell's own surfaces by window class — the taskbar, the notification overflow,
Start, Search and the desktop. Opening the tray menu raises the taskbar, so
without that exclusion the answer would be `explorer.exe` every time. A File
Explorer *window* is `CabinetWClass` and is remembered like anything else,
because somebody may want to record one.

The recorder is then asked for a `pid`, and resolves the window itself. One set
of rules about what a recordable window is, in the recorder (AGENTS.md section
55).

### Closing, and exiting

They are different things.

**Closing the window hides it.** The recorder is a separate process and goes on
recording; the tray is where the application still is.

**Exit is the only path that stops the recorder**, and it goes over the protocol
rather than at the process, so that a recording is *finished* rather than
abandoned. The protocol refuses a bare `shutdown` while something is being
recorded ([ipc.md](ipc.md#shutdown)) and the menu item reads **"Stop recording
and exit"** in that state — so the permission the request carries is the same
sentence the user read. Then the window waits for the recorder to be gone, up to
twenty seconds, because a window that vanished the instant Exit was clicked
would leave a recorder finalising a file with nothing on screen to say so.

That whole path exists because it did not:
[issue #220](https://github.com/wildware-uk/clipped/issues/220) recorded that a
recorder started detached — no console, its own process group — could not be sent
Ctrl+C and had no command to ask it to exit, so the only way to end one was Task
Manager. `apps/recorder/tests/shutdown_command.rs` drives a recorder started
exactly that way, shows a `CTRL_C_EVENT` cannot reach it, and then ends it over
the protocol.

#### When Exit cannot reach the recorder

The dangerous case, because Exit is the only thing that stops a recorder: a
shutdown that could not be delivered leaves one running — quite possibly
recording — with the one thing that could have said so about to disappear. A
release build has no console, so saying it to standard error says it to nobody.

So the first Exit **does not exit**. The window comes up with the recording
named, the file named, and the sentence that choosing Exit again will close the
window regardless. The second one does. Both of the simpler answers are wrong:
closing silently is the recording-safety failure AGENTS.md section 17 puts above
almost everything, and refusing for ever is a user trapped in an application
that will not close (section 45). It also clears itself in the ordinary case — a
recorder that has genuinely gone is not *unreachable*, it is not listening,
which is "nothing was running" and exits first time.

#### When there is no tray at all

`tray::install` can fail, and the shell refusing an icon is not a reason to
refuse to start. But everything above depends on there being a tray to minimise
to and an Exit to quit with, so **without one the window closes normally**:
`on_window_event` asks `tray::installed` before it prevents a close. A build
that kept refusing would leave the application with no way back and nothing to
quit from, which is the exact opposite of the useful action section 45 asks for.

What that costs the user is told to them rather than discovered: the failure is
kept on the Rust side and the window asks for it on mount, through the
`startup_notice` command, because Tauri's `setup` runs before React does and an
event sent then would be sent to nobody. The sentence says the icon is missing,
that closing the window now quits, and that quitting leaves the recorder
running.

### Reporting a failure

A tray menu closes the instant it is clicked, so an action that fails has nowhere
of its own to say so. The window is raised carrying the sentence, on the
`tray-notice` event, and the sidebar's status block shows it — the only surface
Clipped has that can hold one (AGENTS.md section 45). `tray-navigate` is the
other direction of the same channel: Open Library and Settings name a path and
the shell is what knows what to do with one.

`startup_notice` is the third, and it is a command rather than an event because
of *when* it happens: the tray is built during Tauri's `setup`, before React has
run, so nothing is subscribed to be told. The Rust side keeps the sentence and
the window asks for it once on mount. Anything the tray says afterwards replaces
it, because that is the newer thing to have happened.

**Nothing a user has to act on is reported through `eprintln!`.** A release build
is `windows_subsystem = "windows"` and has no console, so a line written there is
a line written to nobody. What is still written that way is what has no user
surface to reach and nothing to be done about it either — a tray that could not
be redrawn, a window that could not be raised — and it is there for a developer
beside a debug build's console. Anything that changes what closing the window
does, or leaves a recorder running, goes to the window.

### Explorer restarting

When Explorer restarts it broadcasts `TaskbarCreated`, and an application that
does not re-add its icon loses it silently for the rest of the session. **Tauri
handles this**, through the `tray-icon` crate: it registers the message with
`RegisterWindowMessageA`, lets it through UIPI with `ChangeWindowMessageFilterEx`
so an elevated application can still receive it, and re-adds the icon in its
window procedure.

That was checked rather than believed. With the application running,
`taskkill /f /im explorer.exe` followed by `start explorer.exe`: Clipped's icon
was in the notification area before, with its tooltip reading
`Clipped — not recording`, and was still there afterwards reading the same thing.
Several other applications' icons did not come back in the same interval.

## Notifications

A notification in Clipped is a **Windows toast**, and it is the third of three
surfaces rather than a second copy of either of the others (issue #110).

| Surface | Carries | Reaches you when |
| --- | --- | --- |
| The tray | State: the icon's mark, the tooltip, the menu's first line. | You look at it. |
| The window | Sentences: the link's state, an interrupted recording's file, whatever the tray had to report. | It is open and in front of you. |
| A notification | One failure, with one thing to do about it. | You closed the window to the tray an hour ago and are in a game. |

That last row is the whole argument for having notifications at all. The recorder
is a separate process precisely so that it can go on recording with no window
open ([ADR 0002](adr/0002-separate-recorder-process.md)), and the hours it spends
that way are exactly the hours in which a user needs to be told that it has
stopped. Neither of the other two surfaces can reach them.

### What is notified

Everything, and there is no more:

| What the recorder link reports | Notified | Why |
| --- | --- | --- |
| `State(Connecting)` | no | Transient; the tray icon already says it. |
| `State(Attached { Idle })` | no | Recordings starting and stopping are the ordinary course of a day. |
| `State(Attached { Recording })` | no | As above. It is what the icon's mark is for. |
| `State(Reconnecting)` | no | A blip that usually fixes itself in a second. A toast per blip is the nuisance. |
| `State(Unavailable)` | **yes** | The link has given up. Nothing is being recorded and nothing further will be tried unless asked. |
| `RecordingInterrupted` | **yes** | A recorder died mid-recording. There is a playable file, and nothing else will ever say where. |
| `RecordingFailed` | **yes** | A recording ended because something went wrong, and the state that follows it is only "idle". |

Those seven rows are the whole of `RecorderLinkEvent` and `RecorderLinkState`.
The rule they encode is that **only failures interrupt anybody**: a recorder runs
for days, and a toast when a recording starts would train the user to dismiss
them without reading, taking the three that matter with it.

Issue #110's scope also lists "replay saved", "bookmark added" and "screenshot
taken". **None of them is here.** Two of the three do not exist at all:
`clipped_replay` can write a clip out of the retained segments (issue #37), but
no build runs a recording with a buffer to save from (issue #38) and
`save_replay` is a command this build refuses. Notifying about something no
subsystem reports would be the invented state AGENTS.md section 27 forbids, and
it would be the one thing worse than a missing notification: a user believing a
clip was saved.

Bookmarks are the third, and they *do* exist now (issue #64). A "bookmark added"
toast is still not here, and that is a decision rather than an omission: a
bookmark is a thing the user just did on purpose, several times a session, while
playing. A toast for each one is the nuisance the rule above is about. The tray
reports a bookmark only when it **failed**, which is the one case the user
cannot infer. Feedback that does not interrupt gameplay is what the overlay is
for (issue #53).

The same issue asks for non-critical notifications to be suppressed during
gameplay. There are none to suppress — every category above is a failure — and a
critical one is never withheld, because in a game is exactly when a user most
needs to know that nothing is being recorded. That is the rule, and the empty set
is what satisfies it.

### Nothing is announced twice

Two rules, both about not becoming a nuisance:

- **A state that has not changed raises nothing.** The link republishes whole
  states rather than deltas, and an identical one is not news. Giving up *again*
  after a "Try again" is not the same state twice — `Connecting` came between —
  and is announced, because otherwise a retry that failed would look like one
  that worked.
- **The state the application opened in raises nothing.** A notification is for
  something that happened while you were away. Without this an installation with
  no recorder beside it (issue #226) would toast on every launch, saying what the
  window in front of the user is already saying.

### Every notification has something to do

A failure that arrives with nothing to act on is the message AGENTS.md section 45
exists to prevent, so each of the three carries a button, and the policy only
ever offers one this build can actually perform:

| Notification | Says | Button |
| --- | --- | --- |
| Recording failed | The recorder's own sentence, and where the file it wrote up to the failure is. | **Show the file** — File Explorer, with the recording selected. |
| Recording interrupted | What was being recorded, that it was not resumed, and where the file is. | **Show the file** |
| Recorder unavailable | Why the link gave up. | **Try again** — `RecorderLink::retry`, and the window is raised to watch it. |

`recording_failed` carries a recording identifier and no path, so the file is
named from the last `Recording` status the window saw — and **only** when that
status's `recording_id` is the one the failure names. A failure for a recording
this window never saw claims no file at all and falls back to **Open Clipped**,
which raises the window carrying the sentence. Guessing that the last file seen
was the one that failed would put somebody else's recording in front of the user.

"Try again" is likewise conditional. `RecorderLink::retry` does nothing to a link
that never had a recorder to talk to — no endpoint could be named, or no
executable found — so for one of those the button is **Open Clipped** instead. A
button that would do nothing is worse than no button (AGENTS.md section 27).

Clicking the *body* of a toast raises the window rather than performing the
action, which is the platform convention and means neither click does nothing.

#### Keeping the button connected to its handler

There is no COM activator (see below), so the handler that performs a button's
action lives in **this** process, attached to the `ToastNotification` object
`Activated` is raised on. That object is reference-counted and this process holds
the only reference it will ever have. Release it and the object is destroyed
while its toast is still on screen or still in the Action Centre; whether Windows
keeps a reference of its own is neither documented nor promised, and a button
whose handler *might* have been freed is the control AGENTS.md section 27
forbids.

So `src/toast.rs` keeps every toast it shows — the last twenty, which is Windows'
own per-application Action Centre limit and therefore every toast the user can
still click, bounded rather than unbounded in a process that runs for days.

This is the whole reason `tauri-winrt-notification` is not used. Its
`Toast::show` builds the `ToastNotification`, attaches the handler, shows it and
returns `Result<()>`, dropping the object on the way out and giving the caller no
way to hold it.

**What has not been verified:** that a click actually reaches the handler on a
real desktop. Nothing short of clicking a real toast can establish it, and that
has to be done on a machine nobody else is using. The tests assert the button
reaches the toast's XML under the argument the handler matches on, and that a
shown notification is retained — not that the click arrives. The button's
presence in the XML is not evidence that it works, and this section will say so
until somebody has clicked one.

### Switching categories off

Per-category switches, in `notifications.json` in Clipped's configuration
directory — `%APPDATA%\uk.wildware.clipped\notifications.json`:

```json
{
  "version": 1,
  "recording_failed": true,
  "recording_interrupted": true,
  "recorder_unavailable": true
}
```

Every category defaults to on, because all three are failures. A missing field
takes its default and an unknown field is ignored, so a file written by an older
or a newer Clipped still works; a `version` from the future is refused rather
than guessed at (AGENTS.md sections 30 and 43). There is no file until somebody
writes one, and that is the ordinary case rather than a fault.

A leading byte-order mark is dropped before the file is parsed. JSON has no such
thing, but this file is edited by hand on Windows and both Notepad and
`Out-File -Encoding utf8` under Windows PowerShell write one — which is exactly
how the first end-to-end run of this feature was done, and the notification it
was supposed to switch off arrived. A settings file that looks right and does not
work is not a trap worth keeping.

**The Settings screen is issue #51**, and until it exists this file is where the
switches are. That is why a file which exists and cannot be read is reported
through the startup notice — naming the file, what is wrong with it, and the
categories it may contain — rather than ignored: somebody has switched something
off and it has not taken effect. Clipped notifies about everything in the
meantime, so a broken settings file can never be the reason a user is not told
that nothing is being recorded.

The other place these can be switched off is Windows' own Settings →
Notifications page, which is per-application and not per-category. It is Windows'
switch rather than Clipped's, and Clipped does not try to reflect or override it.

#### This file is a second configuration store, and why

Clipped has a configuration API — `crates/session/src/config`, issue #108 — with
defaults, types, validation, layered resolution and migrations, and it writes
`%LOCALAPPDATA%\Clipped\settings.json`. Notification switches are settings and
belong in it. Two preference files in two directories is the duplication AGENTS.md
section 55 forbids.

They are not in it because **the desktop application may not link the crate it
lives in**, and that is a rule with a test behind it:
`tests/integration/tests/workspace_layering.rs::the_desktop_application_links_nothing_of_this_workspace_but_the_protocol`
permits this crate exactly one member of the repository's workspace,
`clipped-ipc`. `clipped-session` sits above capture, audio, encoding, muxing and
replay, so naming it here would put the recording engine inside the window's
process — the separation [ADR 0002](adr/0002-separate-recorder-process.md) exists
to make, and the reason closing or crashing a window cannot interrupt a
recording. Reading `settings.json` from here directly would instead be a second
implementation of that file's versioning, migration and validation, against the
file the user's recording settings live in, which is worse than a second file.

**Issue #252** is the fix: move the configuration API to a crate at the
protocol's layer that both ends may link, or serve it over IPC. Either makes
these three booleans ordinary settings, migrates this file into `settings.json`
and deletes it. That migration is why this file carries a `version`, and why a
category's key is documented as stable above.

### Why neither notification crate

`tauri-plugin-notification`'s desktop `show()` is `let _ = notification.show()`,
so a failure to hand the toast over is discarded without a word (AGENTS.md
section 15), and on Windows it offers neither a button nor an activation handler
— which makes the action of section 45 impossible.

The crate beneath it, `tauri-winrt-notification`, offers both, and is not used
either: its `show()` drops the `ToastNotification` the handler is attached to,
for the reason set out above. What is left after that is about thirty lines of
XML document building, which `src/toast.rs` holds directly against the `windows`
crate this application already depends on — rather than a fork of a crate for the
sake of one `Result` type.

**What `Show` returning `Ok` does and does not mean.** It means the notification
platform accepted the notification. It does **not** mean a toast was displayed:
`tauri-winrt-notification`'s own `without_library.rs` example says so in a
comment — "this returns success in every case, including when the toast isn't
shown" — and the obvious case is a user who has switched Clipped off on Windows'
Settings → Notifications page. An earlier draft of this document claimed the
`Result` was a reason to prefer that crate over the plugin. It is not; the button
and the handler are.

A toast that could not be handed over at all is still not lost. Its title and
body go to the window instead, through the same `tray-notice` channel a failed
tray action uses. That raises the window, which is more intrusive than a toast —
and losing a failure notice silently is the one outcome that is not allowed.

### The AppUserModelID

A toast is filed by the AppUserModelID it was shown under: that identifier
decides the name on the notification, the entry on Windows' Settings →
Notifications page, and how the Action Centre groups it. Clipped's is its bundle
identifier, `uk.wildware.clipped`, and on startup it registers a `DisplayName`
for it under `HKCU\Software\Classes\AppUserModelId` so that all three say
"Clipped".

Whether that registration is *required* was measured rather than assumed, on
Windows 11 26200, with a probe that showed the same toast under three identifiers
and then read `ToastNotificationManager::History` back:

| App ID | `show()` | In the Action Centre |
| --- | --- | --- |
| An identifier registered nowhere | `Ok` | yes |
| The same identifier with a `DisplayName` in `HKCU` | `Ok` | yes |
| Windows PowerShell's own AppUserModelID | `Ok` | yes |

So toasts are delivered either way and the registration buys the name, not the
delivery — which is why a registration that fails is logged and carried on from
rather than treated as fatal. It is one `HKCU` value, needs no elevation, and
leaves at most an empty key behind.

There is deliberately **no COM activator**. Registering one would let Windows
start Clipped from a notification, which is not a reason to start a recorder
supervisor. The consequence is that the button works while Clipped is running —
whether the toast is on screen or has fallen into the Action Centre, because the
handler lives in this process — and a toast clicked after Clipped has exited does
nothing.

### What was actually seen

The application was run with `CLIPPED_RECORDER_EXE` naming an executable that
exits without ever listening, so the link exhausted its restart budget and
reached `Unavailable`. Three runs, with the notification history cleared before
each and read back afterwards:

| `notifications.json` | Toasts under `uk.wildware.clipped` |
| --- | --- |
| absent | 1 |
| `{"version": 1, "recorder_unavailable": false}` | 0 |
| `{"version": 1, "recording_failed": false}` | 1 |

The last row is the one worth having: switching a category off silences that
category and leaves the others alone. This is what the toast was, read out of the
history:

```xml
<toast duration="long"><visual><binding template="ToastGeneric"><text id="1">Recorder unavailable</text><text id="2">The recorder exited with status 1 within 10s without listening on \\.\pipe\clipped-recorder.1; its diagnostics are in the Clipped log directory. 4 attempts to reach or start a recorder failed, so nothing is being recorded and no more will be made without being asked.</text></binding></visual><actions><action content="Try again" arguments="action"/></actions></toast>
```

The second run was the one that found the byte-order mark: with the file written
by `Out-File -Encoding utf8`, the toast arrived anyway and the console carried
`Clipped could not read its notification settings … Every notification is
switched on until that file is corrected or deleted.` The failure was reported
rather than swallowed, which is what that path is for — and the mark is now
dropped, which is why the table above reads 0.

**Those three runs predate `src/toast.rs`**, and were made through
`tauri-winrt-notification`. What carries them across the rewrite is that the
document above is composed byte for byte by the current code:
`toast::tests::the_document_is_the_one_a_toast_was_seen_delivered_from` asserts
exactly this string. What changed is who holds the `ToastNotification` after
`Show` returns, not what Windows is handed — so the delivery and the
category-switch behaviour those runs established still stand, and the button's
*activation* remains the thing nobody has yet observed.

### Still to be verified on a real desktop

One thing, and it needs a machine nobody else is using, because it puts a toast
on screen:

- **Click the button on each of the three notifications and confirm the action
  runs**: File Explorer opens with the recording selected, "Try again" restarts
  the link and raises the window, "Open Clipped" raises the window carrying the
  sentence. Then dismiss a toast to the Action Centre and click it there, which
  is the case the retention in `src/toast.rs` exists for.

Until that is done, acceptance criterion 3 of issue #110 — "error notifications
lead to an action, not just a message" — is **not** met. A button in the XML is
not an action; it is a button in the XML.

## Decisions

### Tauri 2, React 19, Vite 7

SPEC.md section 4 recommends Tauri with React and TypeScript. Tauri 2 is the
current major; Tauri 1 is in security-fix-only maintenance, and starting on it
would mean paying for the migration later. Vite is Tauri's own recommendation
and is what `tauri dev` drives.

Routing is `react-router`'s `HashRouter`. The production window loads the
interface from Tauri's asset protocol as a set of files, with no server to
rewrite an unknown path back to `index.html`, so a browser-history router would
404 on a reload of `/settings`. The fragment never reaches the protocol handler,
which also makes the behaviour identical in a browser (`npm run dev:web`) and in
the window.

### The Tauri crate is not a Cargo workspace member

`apps/desktop/src-tauri` is its own single-crate Cargo workspace.

`tauri-build` fails when `frontendDist` — `apps/desktop/dist` — does not exist.
Making the crate a workspace member would therefore make `cargo build
--workspace` depend on a prior `npm install` and `npm run build`, breaking the
promise CONTRIBUTING.md makes about a clean clone, and would put several hundred
crates and a WebView2 dependency in front of every `cargo test --workspace`.

The cost is that `cargo fmt --all`, `cargo clippy --workspace`, `cargo build
--workspace`, `cargo test --workspace` and `cargo deny` do not reach it, and
every one of those is paid back explicitly rather than dropped:

- the Desktop UI job runs `cargo fmt --check`, `cargo clippy -- -D warnings` and
  `cargo test` against the crate, after the frontend build has produced `dist`.
  The test step arrived with the tray: `tray_model.rs` and `tray_icon.rs` hold
  the rules about what the menu offers and what the icon looks like, and a test
  that is compiled and never run is one that has stopped being a test;
- the Dependencies job runs `cargo deny` against it with `--manifest-path` and
  `--config deny.toml`, so both lockfiles are held to one policy. It needs
  neither Node nor `dist`, because `cargo metadata` runs no build scripts, which
  is why it sits in that job rather than beside the two above.

That last one matters more than it looks: the detached lockfile has 429 packages
against the root's 113, and almost none of them are shared. It reports five
unmaintained-crate advisories, all of them the `unic-*` family reached through
`urlpattern` → `tauri-utils` → `tauri`. They are accepted in `deny.toml`'s
`[advisories] ignore` against
[issue #200](https://github.com/wildware-uk/clipped/issues/200), which is a
decision on the record rather than a check that cannot see them.

### One npm workspace, one lockfile

`apps/desktop` and `packages/*` are npm workspaces of the repository root, with
a single `package-lock.json` there. Two lockfiles would mean two resolutions and,
sooner or later, two copies of React in one window. Every script is run from the
root:

```powershell
npm install     # once
npm run dev     # the application
npm run lint    # eslint, prettier and tsc
npm test        # the shell's behaviour, in jsdom
npm run build   # the production bundle
```

`packages/*` are consumed as TypeScript source rather than built to `dist`.
Vite compiles them along with the application, so there is no build order to get
wrong and no stale artefact to debug.

## Design system

The interface is drawn in the **Modernist** system: flat, Archivo, a single red
accent on a light ground, zero corner radius, strong 2px rules, flush-left
labels, [Lucide](https://lucide.dev) icons. The system is not in this
repository. It lives in the maintainer's design project, next to the application
deck that draws every Clipped screen:

- **the system** — its tokens, its written guide, and a reference page for each
  component: <https://claude.ai/design/p/a0eb3af1-6823-4eb0-8953-e637d60c5551>
- **the Clipped deck** — every screen of this application, drawn:
  <https://claude.ai/design/p/00676e7a-fd8a-44ce-9410-082644e1418e>

Read both before drawing anything new, and build a screen from the classes below
rather than from a parallel set of your own.

What is in the repository is the system's tokens and the component layer built
from them, in three files:

| File                             | What is in it                                                                     |
| -------------------------------- | --------------------------------------------------------------------------------- |
| `packages/ui/src/tokens.css`     | The tokens — and the only literals in the package                                 |
| `packages/ui/src/components.css` | The component classes, built entirely from those tokens                           |
| `packages/ui/src/styles.css`     | The typeface, element defaults and the shell's own classes; imports the other two |

### Consuming it

One rule, and it is enforced rather than asked for: **a colour, a typeface, a
type size, a distance or a leading is written as a value only in `tokens.css`.**
Everywhere else it is `var(--token)`. `packages/ui/src/stylesheet.test.ts` reads
the stylesheets and fails the suite on a hex value, a colour function, a number
in **any** CSS length unit — the absolute ones, the font-relative ones and every
viewport flavour, matched case-insensitively, because CSS is — a literal
typeface, a literal `line-height`, or a `var()` naming a token nobody declares.

Percentages and `fr` are deliberately outside the gate: both are proportions of
something else rather than distances, so there is nothing for a token to hold.
That exception is the whole of it. An earlier version of this check covered only
`px`, `rem` and `em` in lower case, which let `12PX`, `4pt`, `3VW` and `62ch`
through — and `styles.css` was shipping a `62ch` at the time. A gate narrower
than the claim built on it is worse than a narrower claim, because the claim is
what gets believed.

If a screen needs a value the tokens do not carry, add the token — do not write
the number. If a value is genuinely one-off geometry, it still goes in
`tokens.css`, next to a comment saying why it is not on a scale; that is where
`--underline-offset` and `--hairline` came from.

There is no CSS-in-JS and no utility framework. The design system is a
stylesheet, and this package stays one: a screen writes
`className="clipped-btn clipped-btn--primary"`.

### The classes

The reference pages name their classes `.btn`, `.card`, `.input`. Here they take
the shell's `clipped-block__element--modifier` convention, so markup copied out
of a reference page is renamed mechanically:

| Reference page                                          | Here                                                             |
| ------------------------------------------------------- | ---------------------------------------------------------------- |
| `.hr`                                                   | `.clipped-rule`                                                  |
| `.btn` + `-primary/-secondary/-ghost/-icon/-block`      | `.clipped-btn` + `--primary/--secondary/--ghost/--icon/--block`  |
| `.tag` + `-accent/-neutral/-outline`                    | `.clipped-tag` + `--accent/--neutral/--outline`                  |
| `.field` + `label`, `.input`                            | `.clipped-field` + `__label`, `.clipped-input`                   |
| `.radio` + `.dot`                                       | `.clipped-radio` + `.clipped-radio__dot`                         |
| `.seg` + `.seg-opt`                                     | `.clipped-segment` + `.clipped-segment__option`                  |
| `.card` + `-kicker/-title/-body/-meta`                  | `.clipped-card` + `__kicker/__title/__body/__meta`               |
| `.elev-sm/-md/-lg`                                      | `.clipped-elevation-sm/-md/-lg`                                  |
| `.table`                                                | `.clipped-table`                                                 |
| `.dialog-backdrop`, `.dialog` + `-title/-body/-actions` | `.clipped-scrim`, `.clipped-dialog` + `__title/__body/__actions` |
| `.nav` + `.nav-brand`                                   | the shell's own `.clipped-header` and `.clipped-nav` — see above |

That is the whole set, and it is the set the deck draws with. There is no
accordion, no toast and no tooltip, because no screen in the deck has one
(AGENTS.md section 1). Two patterns the deck does use are not here either — the
Library's underlined tab strip and the export dialog's selectable preset chips —
and [issue #215](https://github.com/wildware-uk/clipped/issues/215) covers them
with the screen that first needs them. The Games screen (#107) is the first
screen written and needed neither, so neither was built: they belong to the
per-game detail view behind the game list, and there is no list yet to open one
from.

Beside the set above, the shell has classes of its own that a screen draws with.
They are not from the reference pages, which have no screen in them:

| Class | What it is |
| --- | --- |
| `.clipped-screen__title`, `.clipped-screen__heading` | A screen's own two levels of heading |
| `.clipped-screen__lead` | Running prose at the measure |
| `.clipped-panel` + `__heading`, `__body` | The marked panel: an accent rule down the left of the one paragraph that has to be read. Drawn by an unbuilt screen's "Not built yet", by the Games screen's detection state and by the Settings screen's one statement, which are the same thing to look at |
| `.clipped-screen__split` + `__pane` | A screen divided into a rail of sections and the pane one of them opens. `--rail-width` is its one metric |
| `.clipped-rail` | The rail itself, which draws its entries in `.clipped-nav__link` rather than in a class of its own — the same reasoning as the panel above |
| `.clipped-code` | Text somebody types or finds in a file: a settings key, a path, a command |

**Games and Settings are the consumers of the component layer so far** — the
table, `.clipped-muted`, and the rail. The classes exist ahead of that so that
#60, #83, #94 and #101 do not each invent their own styling, which is the reason
issue #79 followed the shell.

`.clipped-nav__link` now serves two mechanisms: the sidebar's anchors and the
rail's `role="tab"` buttons. The declarations a button needs and an anchor does
not — a width, a border, a ground, a typeface, a text alignment — are in the one
rule rather than in a second class, because two classes drawing the same thing
drift, and a screen's rail that stopped matching the sidebar would look like a
mistake. The rule that marks the open one covers both `aria-current="page"` and
`aria-selected="true"` for the same reason, and `contrast.test.ts` measures it on
both grounds it is drawn on.

### Where it departs from the system, and why

Every departure is marked in `tokens.css` or at the rule that makes it, and
every ratio quoted below is computed by `packages/ui/src/contrast.test.ts`
rather than asserted.

- **Type sizes are `rem`**, not pixels, so that the Windows text-size setting
  and the application's zoom both work. The values are the system's own sizes at
  the default root size. The scale has seven steps; the reference pages' 10px,
  12px and 18px snap to the nearest of them.
- **Secondary text is 70% of the ink, not 55%.** At 55% it measures 3.65:1 on
  the window ground and 3.54:1 on the sidebar, short of the 4.5:1 AGENTS.md
  section 46 asks of body text; at 70% it measures 5.81:1 and 5.55:1. It also
  stands in for the system's softening opacities — the card body at 0.8, the
  dialog body at 0.85, the card meta row at 50% ink, the table header at 60% —
  because an opacity cannot be measured without knowing what is behind it, and a
  role can. The one at 50% would have measured 3.10:1 on a card.
- **The accent under words is `--color-accent-700`, not `--color-accent`.** The
  system fills the primary button, the selected segment and the skip link with
  `#ec3013`; `--color-bg` on it measures 3.76:1, and a 14px label at weight 800
  is not WCAG large text. One step down the ramp measures 6.41:1. The hover and
  pressed states shift down a step with it. `--color-accent` itself stays where
  it is not under words: marks, rules, the focus ring, the caret.
- **Accent-coloured words are `--color-accent-700` too** — the ghost button, a
  link, a card's kicker — for the same reason, at 6.41:1 on the window ground
  and 5.91:1 on a card.
- **A control's edge is `--color-neutral-600`, not `--color-divider`.** WCAG 2.1
  1.4.11 asks 3:1 of anything that identifies a control, and the divider
  measures 2.41:1 on the window ground — so an input's border, a secondary
  button's border and a radio's ring take an edge that measures 3.85:1 there and
  3.55:1 on a card. `--color-divider` keeps the rules _between_ things, where
  1.4.11 does not apply.
- **A radio's ring is 2px, not 1.5px.** There is no half-pixel step in the
  system, and the deck draws its own marks at 2px.
- **The segmented option's focus ring carries a halo.** The control clips its
  children, so the ring has to be drawn inside an option's border box — and
  inside means on the option's own fill, which on the _selected_ option is
  `--color-accent-solid`. The accent measures **1.71:1** there, far below
  1.4.11's 3:1, and the selected option is exactly the one a keyboard user lands
  on when tabbing into a control whose whole purpose is that one option is
  chosen. The indicator is therefore two-tone: the accent ring, and the window
  ground immediately inside it — the same halo the checked radio already draws.
  The accent measures 3.76:1 against that halo and the halo 6.41:1 against the
  fill, so the ring has an edge it clears 3:1 against on every option, selected
  or not.
- **Every control that can be disabled is dimmed, not only the button.** Issue
  #79 asks for "disabled at reduced opacity" of the component set as a whole, so
  `.clipped-input`, `.clipped-radio` and `.clipped-segment__option` each take
  `--disabled-opacity` and `cursor: not-allowed` alongside `.clipped-btn`, and
  none of the three lights up on hover any more.
  `stylesheet.test.ts` lists the four, so a fifth control that ships without one
  fails the suite rather than being drawn identically whether it is live or not.
- **The `.field` wrapper is a block here, not layout left to each screen.** The
  reference page's `.field` stacks a label above its control; `.clipped-field`
  does the same, because the gap between a label and the thing it names is a
  property of the component and seven screens each choosing their own would be
  seven different forms.

Archivo is bundled from `@fontsource/archivo` (SIL OFL 1.1) rather than fetched
from Google Fonts as the system's own stylesheet does. A locally installed
recorder must not make a network request to draw its own window
([docs/privacy.md](privacy.md)) and must work with no connection at all.

## Accessibility

AGENTS.md section 46 is the baseline, and the shell is built to it rather than
retrofitted:

- **Keyboard.** Navigation items are real anchors in a real list, so Tab reaches
  them and Enter activates them. The first stop in the tab order is a "Skip to
  content" button. Nothing in the chrome is reachable by mouse alone. A screen's
  own rail is one tab stop and the arrow keys move within it, which is the
  WAI-ARIA tab list's contract — see [The rail](#the-rail).
- **Focus.** `:focus-visible` draws a `--rule-weight` accent outline, and
  `:focus { outline: none }` is the only place a ring is ever suppressed —
  `stylesheet.test.ts` fails if a second one appears. Two components have to
  draw the ring themselves, because the global rule cannot reach them: the radio
  and the segmented option each keep a real `<input>` off-screen for the
  keyboard behaviour and paint a stand-in beside it, so `:focus-visible` never
  matches the element that is drawn. Two more take the global ring and only
  **move** it — the field pulls it flush against its border box (`outline-offset:
0`) so that in a dense form it does not collide with the field above, and
  turns its own border accent as a second, redundant mark on the same event; the
  navigation link pulls it inside, because a link spans the sidebar's full width
  and an outward ring would be clipped against the divider on its right. Neither
  of those two replaces the ring, and `stylesheet.test.ts` asserts that they do
  not: it reads the `outline` **declaration** of the two that draw their own and
  the absence of one in the two that move it, rather than checking that a
  selector appears somewhere in the package, which is what it used to do and
  which passed over both of them. After a
  navigation, focus moves into `<main>` — without that, a screen reader
  announces nothing, because as far as the platform is concerned the window
  never changed. On the _first_ screen it deliberately does not move, which is a
  guard that has to survive React's StrictMode double-invoking the effect: it
  holds the screen key it last acted on rather than a "have I run?" flag, and
  `Shell.test.tsx` mounts the same `<StrictMode>` tree `main.tsx` does so the
  guard is covered as it actually runs.
- **Contrast.** Every pairing of words and ground in the shell and in the
  component layer clears WCAG's 4.5:1 for body text, and
  `packages/ui/src/contrast.test.ts` measures it rather than asserting it — it
  implements the relative-luminance formula and resolves both colours of every
  pair **out of the rule that paints them**, in `styles.css` or `components.css`,
  through the tokens.

  That last part is the whole design of the file, and it was not always true of
  it. A case written as a pair of token names measures two constants: it goes on
  passing after the rule it claims to be about has been pointed somewhere else.
  Issue #48 shipped exactly that defect in the skip link, and a review of this
  ticket found it again in fourteen of the file's own cases — pointing
  `.clipped-card__kicker`, `.clipped-btn--ghost` and `.clipped-field__label` at
  colours measuring 3.47:1, 3.76:1 and 2.59:1 left the whole suite green. Every
  case now names a rule and a property, so changing a rule changes what is
  measured.

  |                                     | Ratio   |
  | ----------------------------------- | ------- |
  | Body text on the window ground      | 14.86:1 |
  | A section in a screen's rail        | 14.86:1 |
  | A button's label, unfilled          | 14.86:1 |
  | Body text on a card                 | 13.70:1 |
  | A field's own text                  | 13.70:1 |
  | A dialog's title                    | 13.70:1 |
  | The title strip                     | 11.45:1 |
  | The primary button, pressed         | 13.01:1 |
  | The primary button, hovered         | 9.59:1  |
  | The accent tag                      | 9.80:1  |
  | The neutral tag                     | 9.26:1  |
  | The skip link                       | 6.41:1  |
  | The open section of a rail          | 6.41:1  |
  | The primary button                  | 6.41:1  |
  | The selected segment                | 6.41:1  |
  | A link on the window ground         | 6.41:1  |
  | The ghost button                    | 6.41:1  |
  | The outlined tag                    | 6.41:1  |
  | A card's kicker                     | 5.91:1  |
  | The open navigation item            | 5.83:1  |
  | Secondary text on the window ground | 5.81:1  |
  | A settings key, path or command     | 5.81:1  |
  | A field's label                     | 5.81:1  |
  | A table's header                    | 5.81:1  |
  | A card's body                       | 5.59:1  |
  | A card's meta row                   | 5.59:1  |
  | A dialog's body                     | 5.59:1  |
  | Secondary text in the sidebar       | 5.55:1  |
  | The title strip's tagline           | 4.87:1  |

  The skip link is why the test reads the stylesheet rather than a table: it
  first shipped on `--color-accent` at 3.76:1, and at 14px weight 800 it is not
  WCAG large text, so 4.5:1 is the bar it has to clear.

  What is not text is held to 1.4.11's 3:1 instead — the edge that says a field
  is a field, and the ring that says where the keyboard is:

  |                                                           | Ratio  |
  | --------------------------------------------------------- | ------ |
  | The segmented option's focus halo, on the selected option | 6.41:1 |
  | A field's edge on the window ground                       | 3.85:1 |
  | A secondary button's edge                                 | 3.85:1 |
  | A radio's ring                                            | 3.85:1 |
  | The segmented control's edge                              | 3.85:1 |
  | The focus ring on the window ground                       | 3.76:1 |
  | The radio's own ring, on its stand-in                     | 3.76:1 |
  | The segmented option's focus ring, against its halo       | 3.76:1 |
  | A field's edge against its own fill                       | 3.55:1 |
  | A field's edge on a card                                  | 3.55:1 |
  | The focus ring on a card or a dialog                      | 3.47:1 |
  | A focused field's accent border, against its own fill     | 3.47:1 |
  | The focus ring in the sidebar                             | 3.42:1 |

  The last two rows of the first non-text group are the correction this round
  made. The segmented option's ring is drawn _inside_ its own border box,
  because the control clips its children — so on the selected option it landed
  on `--color-accent-solid` at **1.71:1**, and that case was missing from the
  list while the three grounds the ring happens to pass on were in it. It now
  carries a halo, and both of its edges are measured.

  The rules _between_ things — `.clipped-rule`, the table's row rules, the
  sidebar's dividers — are deliberately not measured. 1.4.11 applies to what
  identifies a control or conveys information, and a rule separating two
  paragraphs does neither; that is the only reason `--color-divider` is allowed
  to stay at 2.41:1.

  A disabled control is the one place text is dimmed by opacity, to the system's
  45%. WCAG 2.1 exempts an inactive component from both 1.4.3 and 1.4.11.

- **Labels.** Each of the two navigation lists is a named `<nav>`; the recorder
  status is a named region and a polite live region, so a change in state is
  announced rather than only drawn. The Games screen's detection block is the
  second of those, named "Game detection", for the same reason: a recorder
  appearing or going changes what the screen says while nobody is looking at it.
- **State is never colour alone.** The open screen is marked by an accent rule
  down its left edge, a heavier weight, _and_ `aria-current="page"`.
- **The window title** names the open screen, so the taskbar, Alt+Tab and the
  screen reader's window announcement all say where you are.

`eslint-plugin-jsx-a11y` runs in its `strict` configuration as part of
`npm run lint`, and `apps/desktop/src/Shell.test.tsx` and `GamesScreen.test.tsx`
drive the window with Tab and Enter rather than asserting that the markup looks
right.

## Testing

`npm test` runs Vitest, from `apps/desktop` but over `packages/*/src` as well,
because those packages are consumed as source and one test command for the npm
workspace is worth more than a second configuration to remember. Most of it runs
against jsdom; `packages/ui/src/contrast.test.ts` and
`packages/ui/src/stylesheet.test.ts` ask for the node environment, because they
read the stylesheets as text and Vitest replaces a CSS import with an empty
module.

The tests assert the things about this shell that would rot quietly: that no
part of it shows data it does not have — including that the only controls in the
whole window are the skip link and the Settings rail, neither of which touches a
recorder — that the chrome is operable from the keyboard alone, that every
pairing of words and ground clears 4.5:1, and that the component layer still
consumes the design system rather than a value somebody typed. The last of those is why `stylesheet.test.ts` exists: "no hard-coded
colours" is a promise a reviewer has to re-check on every diff, and a test that
reads the stylesheet is one that cannot be forgotten.

Both files hold to one rule that is easy to lose: **a check must resolve what it
claims to measure out of the stylesheet, not restate it.** A contrast case that
names two tokens measures two constants; a focus check that looks for a selector
somewhere in the package says nothing about what the matching rule draws. Both
mistakes were in this package and both were found by breaking the CSS and
watching the suite stay green, which is the only way either would have been.
Every case in both files now goes through a helper that throws when the rule or
the declaration it names is missing — `colourOf` in `contrast.test.ts`, `bodyOf`
in `stylesheet.test.ts` — so a check that has stopped measuring anything fails
rather than passing on nothing.

`Shell.test.tsx` renders the `<StrictMode>` tree `main.tsx` builds rather than
`<App />` on its own. That is not ceremony: StrictMode double-invokes effects on
mount while preserving refs, and a focus guard that passed under a bare `<App />`
failed under the real tree.

`GamesScreen.test.tsx` does the same, for the same reason and one more. The
property it is really about is that the screen shows the recorder's state rather
than a sentence somebody typed — and a screen whose wording is a constant looks
identical to one that is following the link. So the case drives the whole
application, opens Games, and then moves the link underneath it with a
`recorder-link` event, rather than rendering `describeGameDetection`'s output
beside itself. The other cases are about absence: no button, no link, no field,
no checkbox and no radio anywhere in `<main>`, and a table whose column headers
are what is missing rather than Game / Recording / Last played.

`SettingsScreen.test.tsx` is about three things, and the middle one is the
awkward one. That the screen offers nothing that would change a setting is
asserted as an absence — no button, link, field, combobox, checkbox, radio,
switch, slider, spinbutton or menu item anywhere on it, with the rail's own tabs
counted first so the case cannot pass on an empty screen. That the rail is
operable from the keyboard alone is driven with Tab, the arrow keys, Home and
End. And that every setting names both how it is set today and the work that
would bring it here is checked from a list written out in the test file, not
mapped from the screen's own tables: a case that walked the rendered rows and
checked their shape is satisfied by rows somebody invented, which is exactly the
defect a review found in the Games screen's first version of the same case.

`settingsConformance.test.ts` is the other half, and the table in
[What is checked, and against what](#what-is-checked-and-against-what) is what it
reads. It runs in the node environment because its subject is Rust sources as
text, and every case throws when the item it names is no longer in the file —
the same rule `contrast.test.ts` and `stylesheet.test.ts` hold to, for the same
reason: a check that has stopped finding its subject has to fail rather than
pass on nothing.

`useWindowTitle.test.ts` stands up a `__TAURI_INTERNALS__` so the branch that
only runs inside the window is reached — jsdom is a browser, so without it the
native call, which is the only reason the hook exists, has no coverage. The real
`@tauri-apps/api` runs against the stub, so the test sees the command it
actually sends, and asserts that `src-tauri/capabilities/default.json` grants
that command. Removing `core:window:allow-set-title` therefore fails a test
rather than a window nobody opened.

The Rust side of that call is out of reach here — Tauri decides whether to
answer it in the process that owns the window — and so is WebView2. Keyboard
behaviour in the real window is checked by hand, by driving it with Tab,
Shift+Tab and Enter and watching the window title follow the screen.

The tray's own rules are tested on the Rust side, with `cargo test
--manifest-path apps/desktop/src-tauri/Cargo.toml`, and the CI job runs it. What
is covered there is what can be: `tray_model.rs` is a pure function of the link's
state and the last foreground window, so every menu item's label, its enabled
state and what it would do are checked across every state the link has, and
`tray_icon.rs`'s marks are compared as silhouettes and measured for contrast.

What is **not** covered there is the tray itself — building a real notification-
area icon needs a desktop session and a message loop, and driving its menu needs
a pointer. That half is verified by hand and recorded on the issue: the icon
appearing, its tooltip following a real recording, surviving an Explorer restart,
and the four things the menu items do driven through the same `RecorderLink`
calls the handlers make.
