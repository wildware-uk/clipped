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

**Almost every control is in the notification area, not in the window** — see
[The tray](#the-tray). No screen drives the recorder: one of the seven is not
built, and none of the six that are draws anything that would, because nothing
they would drive can be reached from here. The Editor draws none of the editing
controls for the same reason — the operations exist in `crates/edit` and this
window cannot reach them. A button with nothing behind it is exactly what
AGENTS.md section 27 forbids, which is also why Diagnostics has no Export Support
Bundle button ([diagnostics.md](diagnostics.md)). A "Try again" control for a link
that has given up is
[issue #221](https://github.com/wildware-uk/clipped/issues/221).

The controls a screen does draw change nothing outside the window: the Settings
screen's rail, which moves between that screen's own sections — see
[The Settings screen](#the-settings-screen); the Diagnostics screen's Copy
report, which does what it says with a browser API and no recorder involved — see
[The Diagnostics screen](#the-diagnostics-screen); the Editor's three zoom
controls, which change how the timeline is drawn and nothing else; and the
Editor's Export, which opens a dialog — all of them this window's own state, and
therefore things it can actually perform. What that dialog then says about
exporting, including that no export can be started from here, is
[The export dialog](#the-export-dialog).

There is one **link** in the chrome that is not navigation, and a link is a
destination rather than an action: when a recorder dies mid-recording, the notice
that names the file it left also leads to that recording's own playback screen —
see [The playback screen](#the-playback-screen). That screen does not play it,
and does not offer to; it says what state the recording is in and what stands
between this window and playing it, which is more than a sidebar has room for.

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
exist. **Six of the seven screens have been written** —
[Home and Library](#home-and-library), [Games](#the-games-screen),
[Editor](#the-editor-screen), [Settings](#the-settings-screen) and
[Diagnostics](#the-diagnostics-screen), which has a document of its own in
[diagnostics.md](diagnostics.md). The last, Trash, leads to a panel saying so and
naming the issue that builds it — #94. Building it replaces its placeholder route
with the real screen, in `elementFor` in `Shell.tsx`, which is the one place that
knows a screen from a placeholder.

There is an **eighth route** that is not in `SCREENS` and deliberately not in the
sidebar: `/clip/:recordingId`, the playback screen, which is opened *for* a
recording rather than navigated to — see
[The playback screen](#the-playback-screen). A sidebar item called Playback would
be an item with no recording behind it. `titleFor` in `Shell.tsx` names it in the
window title all the same, so the taskbar, Alt+Tab and a screen reader say where
you are on that screen as they do on the seven.

## Home and Library

SPEC.md section 17, and issue #60. The deck draws Home as tiles of recent
sessions, recently clipped and favourites, and Library as a Sessions / Clips /
Highlights tab strip over a grid of thumbnails, with a filter row and a search
field above it.

**The sessions and the per-game figures are drawn. The clips, the highlights,
the favourites and the thumbnails are not**, and the reason has changed: it is
no longer that nothing can be read.

### How the window sees the library

`clipped-library` builds the index — games, sessions, recordings, clips,
favourites, `game_summaries` and `missing_since` — by reconciling the session
sidecars against the disk ([library.md](library.md)), **inside the recorder's
process**. Two of the three ways into it are still shut, and always will be:

| The way in | Where it stands |
| --- | --- |
| Reading `library.db` from the window | Shut. `capabilities/default.json` grants three `core:` permissions and no file-system access, and Tauri denies what is not listed |
| Linking `clipped-library` into the Tauri host | Shut. `tests/integration/tests/workspace_layering.rs` permits it exactly one member of the workspace, `clipped-ipc` — which is what keeps the recording engine out of the window's process ([ADR 0002](adr/0002-separate-recorder-process.md)) |
| A protocol command | **Open.** [Issue #301](https://github.com/wildware-uk/clipped/issues/301) added `library_sessions` and `library_games`; the recorder reads the index and answers, and `library.ts` asks through a Tauri command in front of each ([ipc.md](ipc.md)) |

So the round trip is window → Tauri command → control protocol → recorder →
`library.db`, and the window links nothing but the protocol. Which is the same
shape of answer the Games screen gives, and deliberately not
[#245](https://github.com/wildware-uk/clipped/issues/245) — the game
*catalogue* — nor [#241](https://github.com/wildware-uk/clipped/issues/241), the
*live* session; this is the record of what has already been recorded.

### Three answers, never two

Every library read ends in one of three states, and both screens draw all three
differently: **read** — holding what the index says, which may legitimately be
nothing; **unread** — holding why; and **reading**, while the round trip is in
flight.

Collapsing the first two is the failure this is shaped to prevent. "You have not
recorded anything" over a database that is locked, corrupt, from a newer build
or on a drive that is not plugged in is the fabricated state AGENTS.md section
27 forbids, and it is indistinguishable from the truth unless the protocol keeps
them apart — which it does: an empty library is a successful `library_sessions`
carrying no sessions, and an unreadable one is a `library_unavailable` refusal
that says why. `LibraryRead<T>` in `library.ts` therefore has no "empty" case at
all; empty is a successful read of an empty page.

The window tells apart three more things the recorder cannot: a question that
never reached a recorder (`recorder_unreachable`), a build with none configured
(`no_recorder_configured`), and a recorder older than this window, which refuses
the command with `unknown_command` and is told to restart. Those codes are
outside the protocol's own vocabulary on purpose, so that "the recorder said no"
and "there was no recorder" cannot be confused.

**One thing the index knows is still not on a real machine's screen**: nothing
calls `reconcile`, so the database is empty until something does
([#385](https://github.com/wildware-uk/clipped/issues/385)). The window reports
that honestly — it says the library was read and holds nothing — which is true
of the database and, until #385, false of the disk.

What each screen still owes is a row in `WaitingOn.tsx`, the table both share
with Games, naming the work that lands it: clips and highlights wait on
something creating one (#91, #76), favourites on anything being favouritable
(#58), and thumbnails and waveforms on a transport for bytes rather than rows.

### What is being recorded right now, and the button that changes it

**What is being recorded, into which file, and a control that starts and stops
it.** That is the one part of Home which is not a read of the index, and since
[issue #389](https://github.com/wildware-uk/clipped/issues/389) it is the part
that makes this an application rather than a window over one: press the button, a
recording starts; press it again, a file exists.

`describeRecordingNow` in `recordingNow.ts` renders the state and
`describeRecordControl` in `recording.ts` renders the button. Both are pure
functions beside the screen rather than conditions inside it.

Three rules shape them, and all three are properties `HomeScreen.test.tsx`
asserts across every state rather than at the one place they could go wrong:

- **A heading carries its own scope.** It is either "Not known…", which claims
  nothing; "This recorder…", which names what the claim is about; or
  "Recording `<target>`", which asserts a recording that demonstrably exists.
  The wording this rules out is "Nothing is being recorded" — a statement about
  the machine, when `clipped-recorder watch` serves no protocol and could be
  recording a game this link cannot see. The same trap the Games screen's
  detection state describes.
- **The state is asked for, never assumed.** See below.
- **The duration is the recorder's measurement, rendered.** `formatElapsed`
  turns `ActiveRecording::elapsed_ms` into `M:SS` or `H:MM:SS` and does nothing
  else. There is no branch anywhere that computes a duration from a clock in
  this window.

The file is printed in full, in `.clipped-path`, because it is the thing on the
screen anybody can act on and a path with an ellipsis in it cannot be typed into
Explorer (AGENTS.md sections 28 and 45). The recording that was just *stopped*
has its file printed too, from the `stop_recording` reply, because the panel
would otherwise go from "Recording cs2.exe" straight to "not recording" and take
the path with it.

### The record control

Four Tauri commands in `src-tauri/src/main.rs`, and one rule about where the
state comes from.

| Command | Sends | Answers |
| --- | --- | --- |
| `record_target` | nothing; reads `foreground::last_seen()` | the process the button would record, or `null` |
| `recorder_status` | `get_status` | `RecorderStatus` — idle, or a recording and its elapsed time |
| `start_recording` | `start_recording` with `pid` | the recording's identifier and the file it is writing |
| `stop_recording` | `stop_recording` with `recording_id` | the finished `RecordingSummary`, after the file is closed |

**What is recorded** is the application the user was last in — the same answer
the tray's Start Recording gives, from the same `foreground` module, so there is
one idea of "what to record" in this process rather than two. The button names
it, because a screen has somewhere to put a name and a control that records
something unnamed is one nobody consented to. The identifier the button was
showing is what `start_recording` is given, so a foreground change between the
label being drawn and the button being pressed cannot record something else;
`stop_recording` names the recording the screen had for the same reason, which is
the race `StopRecording::recording_id` exists for.

**Where the state comes from.** `useRecording` asks `recorder_status` once a
second while Home is open, and *that answer* is what the screen draws. Three
things it deliberately does not do:

- it does not draw the status inside the link. That one is pushed, and the
  recorder publishes `status_changed` when a recording starts and when it ends
  and at no point between (`apps/recorder/src/serve.rs`) — so its `elapsed_ms` is
  the figure from the moment the recording began and never moves;
- it does not count up from that figure with a timer of its own, which would be
  a number nobody measured (AGENTS.md section 27);
- it does not set a recording state when `start_recording` resolves. The command
  being accepted is not the recording still running a second later.

All three collapse into the same failure: **a window that says "recording" after
the recorder has died.** Because the state is asked for, an ask that fails drops
it — the screen says it does not know, and gives the reason — and the window
follows a dead recorder down within one interval. `HomeScreen.test.tsx` drives
the link and the answer *apart* to check this: the link says idle while
`get_status` says recording, and the other way about, so a screen wired to the
wrong one fails rather than passing on a coincidence.

A refusal is reported in the recorder's own words — `target_not_found` for a
window that has closed, `target_not_capturable` for one that is minimised and
would record nothing, `already_recording` for a second start — because those
are different problems with different answers (AGENTS.md section 45). The
minimised one is the reason the message is shown verbatim rather than mapped to
a sentence of the window's own: only the recorder knows which window it is, and
"Counter-Strike 2 (cs2.exe) is minimised" is the whole of what the user has to
act on ([#383](https://github.com/wildware-uk/clipped/issues/383)). A request
that never reached a recorder carries one of `recorder_unreachable`,
`no_recorder_configured` or `unexpected_reply` instead, which are outside the
protocol's own vocabulary so that "the recorder said no" and "there was no
recorder" cannot be confused. `RecorderProblem` is the one shape all of that
arrives in, shared with the library commands.

The middle hop — the Tauri command itself — is tested in `src-tauri/src/main.rs`
against a real named pipe, because `invoke` is stubbed in the TypeScript suite
and the recorder's own tests start at its dispatch. Without those, a
`start_recording` that sent the wrong protocol command and dropped its parameter
would pass every other test in the repository.

### No tab strip and no chips

[Issue #215](https://github.com/wildware-uk/clipped/issues/215) asks for the
deck's tab strip and its selectable chips **at the point a screen needs them**,
so that a component with no consumer is not designed against a guess. **The
Library screen still does not need them**: since #301 it has one populated list
— sessions — and two empty ones, because nothing creates a clip or a highlight
yet, so a strip over them would still be the speculative component that issue
exists to prevent. It belongs with the first screen that has three lists worth
switching between, which is this one once #91 and #76 land.
`LibraryScreen.test.tsx` asserts that no `tablist` and no `tab` is drawn, so
adding one is a decision rather than a drift.

The search field is a different case and is drawn, because it now does
something: the query goes to the recorder, which parses it with the language in
[search.md](search.md) and answers with the sittings it selects. It runs on
submission rather than on every keystroke — `game:` on the way to `game:cs2` is
a parse error nobody asked about — and a query the recorder will not parse is
reported with what was wrong with it rather than as an empty result set.

## The Games screen

SPEC.md sections 6 and 17, and issue #107. The deck draws it as a table of
detected games — name, executable, launcher, capture mode, last played — with an
Add Game control above it and a New Game Detected panel below.

**None of that is drawn, because none of it can be got.** What the screen shows
instead is the one thing about game detection this window can establish, and a
table of what the rest is waiting for.

### What the window can and cannot see

The desktop reaches the recorder over [the control protocol](ipc.md) and reaches
its own Tauri host through the eight commands listed under
[The record control](#the-record-control) and
[How the window sees the library](#how-the-window-sees-the-library). Not one of
them is about a game. Against that:

| The screen would show | Where it would come from | Why it cannot, yet |
| --- | --- | --- |
| The list of games | `clipped-game-detection`'s catalogue: the compiled-in `games.toml` plus the user's overlay ([game-detection.md](game-detection.md)) | No protocol command lists it, and the window has no file-system permission to read it — `capabilities/default.json` grants three `core:` permissions and nothing else. [Issue #245](https://github.com/wildware-uk/clipped/issues/245) |
| Add an executable, rename, exclude, disable capture | The same overlay, written | Same. [#45](https://github.com/wildware-uk/clipped/issues/45) owns the behaviour, #245 the way to reach it |
| Sessions, clips, favourites, storage per game | The library index | Reachable since [#301](https://github.com/wildware-uk/clipped/issues/301): `library_games` carries exactly these figures and Home draws them. Bringing them onto *this* screen, beside the catalogue entry each belongs to, needs the catalogue itself, which is #245 |
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

## The Editor screen

SPEC.md section 19, `docs/editing.md`, and issue #83. The editor is
deliberately lightweight — "this is not Premiere" — and this is its shell: the
layout, a timeline with a ruler and a playhead, one lane per audio track, zoom,
and the wiring that shows a document.

```text
┌──────────────────────────────┬───────────────────────────┐
│                              │ Playhead   00:08.000 …    │
│   the frame at the playhead  │ Recording  rec-…-cs2      │
│   (there is none — #306)     │ Source     01:32.000      │
│                              │ Segment    2 of 3 …       │
├──────────────────────────────┴───────────────────────────┤
│ 00:08.000 of 00:24.000            Zoom 2×  − + Fit       │
├──────────┬───────────────────────────────────────────────┤
│          │ 00:00      00:05      00:10      00:15        │
│ Video    │ ├─ seg 0 ──┼───── seg 1 ─────┼── seg 2 ──┤    │
│ Game     │ No waveform         ▮                         │
│ −3.0 dB  │                     playhead                  │
│ Micro…   │ No waveform                                   │
│ Muted    │                                               │
└──────────┴───────────────────────────────────────────────┘
```

### What an edit is, and what the screen may do to one

A clip is not a copy of a recording with the boring parts cut out. It is a
document naming which recordings to play, which parts of them, in which order,
how loud each track is and what text to draw over the picture — and **making,
changing or exporting one never modifies, moves, truncates or re-encodes the
recording it refers to** (AGENTS.md sections 56 and 57).

Here that is an inability rather than care taken at each call site, the same way
`crates/edit` implements it: this window has no file-system permission at all,
and nothing on this screen writes anything. The playhead and the zoom are the
component's own state.

### Two kinds of time, which is the whole reason this screen is hard

A timeline draws **output** time — measured from the start of the clip — while
the media it points at is in **source** time, measured from the first frame of
one recording. `docs/editing.md` argues the model; what matters here is that the
screen shows both, side by side, at the playhead: eight seconds into the fixture
clip is one minute thirty-two into the recording, because a cut removed
everything between.

`EditDocument::locate` is "what both the preview and the exporter must use, so
that they cannot disagree", and `crates/edit`'s own documentation names this
screen as one of its two callers. **This window cannot call it** —
`workspace_layering.rs` permits `apps/desktop/src-tauri` exactly one crate of
the workspace, the protocol — so `src/editor/timeline.ts` writes the same
formulae out, in `BigInt` for the reason the crate uses 128 bits, and
`timeline.test.ts` holds them to the figures `crates/edit`'s own tests use. A
test written from the port's own output would prove only that it agrees with
itself.

The document is read from the same JSON `crates/edit` writes, because
`docs/editing.md` settles that a document crosses the boundary as that text
rather than as a second representation. `src/editor/document.ts` is the reader,
and it follows that document's compatibility table: a newer document is refused
by *version* rather than misread, one with no version is refused rather than
guessed at, and one carrying an unknown field is refused rather than opened with
the field silently dropped.

### Nothing is drawn that this window cannot get

**No clip can be opened at all.** An edit document is stored as text in the
library's database (#55), and this window can neither read that database nor ask
the recorder for a row of it: the control protocol has no command about a
library, and the window has no file-system permission. So the screen says so and
names the work — **issue #306** for a clip's document, a frame and its
waveforms; **#301** for the library index behind it — rather than drawing an
empty timeline with a dead playhead, which is indistinguishable from a broken
editor (AGENTS.md section 27).

The screen takes the document as a prop, so the day something can supply one,
one line of `Shell.tsx` changes.

Given a document, two things are still absent rather than drawn:

| | Why | Drawn as |
| --- | --- | --- |
| The picture at the playhead | A frame is inside a recording this window cannot open | The sentence saying so, on the ground a frame would be drawn on |
| A waveform under each lane | `crates/waveform` computes the peaks (#66) beside the recording; the window cannot read a file | "No waveform" in the lane. **Never a flat line** — `docs/waveforms.md` is explicit that a flat line is indistinguishable from silence |

What the screen *does* show is all real, computed from the document: the
recording under the playhead, its source time, which segment of how many and
where that segment starts, the speed if it is not the recording's own, the text
on screen at that moment, and each track's name and what it contributes once
mute and solo are resolved.

### No editing controls, and why

There are none — not a Split, not a volume slider. The four operations that cut
a clip up are **built and tested**, in `crates/edit` with undo and redo (#84),
and a button here could not reach them any more than it could open a clip. The
mix is #85, framing and speed #86, overlays #87 and combining recordings #88,
and each owns its own control.

The three zoom controls are the exception, because zoom is this component's own
state and they do exactly what they say. Each is disabled at the end of the
scale where it would do nothing. **Export** is the fourth, for the same reason:
opening a dialog is this component's own state.

### The export dialog

SPEC.md sections 19 and 20, `docs/exporting.md`, and issue #90. It is opened by
Export beside the clip's name, and it is the only dialog in the application.

The engine behind it (#89) implements **one** method. A **stream copy** writes
the recording's own coded packets: about as fast as reading the file, with the
pictures bit for bit. Everything else needs a re-encode, and **there is no
re-encode** — an edit that needs one is refused with every reason named, rather
than exported as something that is not the clip. That shapes the whole dialog:
its job is to say what is about to happen before somebody waits for it, and to
name what stands in the way in words they can act on (AGENTS.md sections 27
and 45).

#### What it says

| The clip | What the dialog says |
| --- | --- |
| Nothing in the edit rules out a copy | That it would be a fast lossless copy, that the recording is never modified, and the four or five things still to be checked against the recording itself |
| Something does | That Clipped cannot export it yet, and one line per reason: which edit is responsible, and what to change so that it can be copied |
| Something the engine would refuse to plan at all | That sentence, on its own — "nothing rules out a copy" about a document the engine would reject is the more confident of the two wrong answers |

The reasons are the engine's own `CopyBlocker`s, in the order `ExportPlan::of`
collects them, so a reason read here and a refusal read in a log are the same
list in the same order. The **words** are not the crate's: `CopyBlocker`'s
`Display` writes for a log — "the recording's picture is {codec}, which
Clipped's container writer cannot describe" — and a dialog has the document
open, so it says *which* transformation, *what* level and *how many* streams
where the crate says "transformed" and "a mix".

#### Half the answer, and the half that is missing

`ExportPlan::of` decides from the document plus one demuxing pass over the
recording. This window has the document and **cannot open the recording**, so
`apps/desktop/src/editor/exportPlan.ts` ports exactly the half the document
settles — several recordings, a transformed segment, an overlay, an audio mix —
and names the rest instead of guessing:

| Settled here, from the document | Needs the recording, and is named rather than answered |
| --- | --- |
| The clip joins more than one recording | A cut that does not fall on a keyframe |
| A segment is sped up, cropped or rotated | A codec the container writer cannot describe |
| Text is drawn over the picture | Pictures stored out of the order they are shown |
| A track is a mix: several inputs, a level, a mute or a solo elsewhere, a fade | A segment covering no pictures, and the recording's shape |

That split is the crate's own structure rather than an approximation of it, and
`exportPlan.test.ts` holds the port to the cases `crates/export`'s own tests
assert. [Issue #322](https://github.com/wildware-uk/clipped/issues/322) is the
gap: no command carries a plan, an export, its progress or its cancellation, and
`ExportPlan` does not serialise at all.

#### What it deliberately does not have

- **No resolution, framerate, codec or quality**, and none of the deck's preset
  chips. All four are properties of a re-encode that does not exist, and a copy
  has no settings by definition — it writes what the recording holds. A quality
  picker over a lossless copy is the worst kind of control that does nothing,
  because its label implies the file was affected by it.
- **No Export button.** Nothing here can start one: no command, no file-system
  permission, no way to choose a destination. The dialog says so in its marked
  panel rather than offering a button that would fail.
- **No estimated size.** `ExportPlan` reports the method, the blockers, the
  segments, the tracks and the duration, and no bytes;
  [#323](https://github.com/wildware-uk/clipped/issues/323) is what makes a
  figure possible. Drawing one now would be a figure nobody measured — and it is
  the figure somebody decides whether they have room for.
- **No progress bar and no Cancel.** Both are the engine's (`ExportProgress`,
  `Cancellation`) and arrive with #322. A progress bar that does not move and a
  cancel button that does nothing are two of the same bug.

Three of #90's acceptance criteria are therefore **not met**, and the issue says
so: the estimated size, every option being honoured, and cancellation from the
UI.

### The keyboard

This matters more here than anywhere else in Clipped. AGENTS.md section 46 asks
that core workflows do not require precise mouse interaction, and a timeline is
the control that most tempts a designer to break that: every operation #84 built
is aimed at a boundary, and nobody can drag to a nanosecond.

The playhead is a real `slider` — the platform's own role, so it reaches a
screen reader — with `aria-valuetext` reading a timecode rather than a number of
milliseconds.

| Key | What it does |
| --- | --- |
| Tab | Moves through the editor's controls, in [the order below](#the-tab-order) |
| ← / → | Steps a tenth of a second |
| Shift + ← / → | Steps a second |
| Home / End | The start of the clip, and its end |
| Page Up / Page Down | The previous or next **cut** — exactly, whatever the zoom |
| `+` / `-` | Zooms in and out |
| `0` | Back to fitting the whole clip |

Clicking the timeline seeks to where it was clicked. That is an alternative to
the keys above and never the only way to do anything.

End lands on the end of the clip, where the screen says nothing plays: every
range in the model is half-open, so the last position of a clip is the
nanosecond before its end and the end itself belongs to nothing. Saying that is
better than silently moving the playhead somewhere the user did not ask for.

### The tab order

The editor is the one screen where controls from several issues share a
component — the export dialog's button (#90) and the event filters and marks
(#71) arrived separately, at opposite ends of the same screen. The order between
them is decided here rather than left to whichever branch merged last:

1. **Export**, in the header beside the clip's title;
2. **Zoom in**, and Zoom out and Fit when they are enabled — all three are
   disabled at the first zoom step, where only Zoom in is a tab stop;
3. **the kind filters**, one per kind of event on the clip, when there are any;
4. **the event marks**, in the order they occur on the edited timeline;
5. **the playhead**.

Two rules produce that list, and both are worth stating because the plausible
alternative — content first, whole-document actions last — fails them:

- **Focus order follows visual order.** Export is drawn first, at the top, so it
  is reached first. Ordering by importance instead would send focus from the
  bottom of the screen back to the top, which is exactly the mismatch WCAG 2.4.3
  Focus Order is about; in this component it could only be built with a positive
  `tabIndex`, which the shell uses nowhere.
- **Outermost inwards, and the playhead last.** The controls that change how the
  timeline is *drawn* come before the things drawn on it, and the playhead is
  the innermost of those and the one a user stays on — the arrow keys, Home, End
  and Page Up/Down all work from it, so it is where Tab leaves somebody who
  wants to move about the clip.

`EditorEvents.test.tsx` asserts that list **whole**, as a single comparison
rather than a search for one element in it, so a control added to the editor
without a decision about where it belongs fails a test instead of quietly
reordering the screen. `EditorScreen.test.tsx` asserts the same order for a clip
with no events, where the filters and marks are absent.

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
## The Diagnostics screen

SPEC.md section 36, and issue #101. It has a document of its own,
[diagnostics.md](diagnostics.md), because most of what it is about is not the
interface: what the recorder records, what reaches this window, and exactly what
a support report may and may not contain.

The shape is the Games screen's, for the same reason — a live panel saying what
this window can establish, and a table of what the rest is waiting for — with a
third part the Games screen has no equivalent of: the support report, composed
here, shown in full, and copied to the clipboard.

Two things it adds to this document. Its `Copy report` is the **one control in a
screen that acts** rather than navigating: a browser clipboard call, needing no
Tauri permission and reaching no recorder, which reports both of its failure
modes rather than appearing to work. And it is the reason
`.clipped-screen__report` exists in `styles.css` — a monospaced block on the card
ground, wrapping rather than scrolling and with no height limit, because
[privacy.md](privacy.md) asks that nothing about what leaves the machine is
hidden and a scroll box showing eight of thirty lines hides twenty-two of them.

It draws **no Export Support Bundle button**, which SPEC.md section 36 asks for
and the deck draws. The log files that would make a bundle worth sending are
unreachable from this window, and diagnostics.md sets out why a button that wrote
a report with no logs in it would be an export in name only
([issue #303](https://github.com/wildware-uk/clipped/issues/303)).

## The playback screen

SPEC.md section 42 and [issue #52](https://github.com/wildware-uk/clipped/issues/52).
The route is `/clip/:recordingId`; the screen is `ClipPlaybackScreen.tsx` and
everything it decides is in `clipPlayback.ts`, which is pure and therefore the
part with tests.

The ticket asks for playback with transport controls, keyboard shortcuts,
frame-accurate seeking and an audio-track selector. **None of it is drawn,
because this window cannot play a Clipped recording at all.** That is not a
scheduling remark; it is four independent facts, and the design that follows from
them is below.

### Why a `<video>` cannot be pointed at a recording

Each of these is enough on its own, so fixing any single one changes nothing.
They are on the screen itself, with the evidence beside each, in the same
contract the unbuilt screens keep.

| What stops it | Where it can be checked |
| --- | --- |
| **This window cannot load a file from the disk.** | `src-tauri/tauri.conf.json` does not enable the asset protocol; `capabilities/default.json` grants three `core:` permissions and none reaches the file system; the content-security policy declares no `media-src`, so it falls back to `default-src 'self'` — the bundle Vite built, and nothing else. |
| **A recording is Matroska, and WebView2 does not demux it.** | [ADR 0001](adr/0001-mkv-archival-container.md) writes recordings into MKV so a killed recorder still leaves a playable file. WebView2 is Chromium, whose Matroska support is WebM: a strict subset restricted to Opus or Vorbis audio and VP8, VP9 or AV1 video. |
| **The audio is uncompressed PCM, and nothing in Clipped encodes audio.** | [muxing.md](muxing.md): every track is 16-bit PCM because no crate in the workspace encodes audio ([#28](https://github.com/wildware-uk/clipped/issues/28)). No browser decodes PCM in MP4. |
| **A media element cannot choose an audio track.** | `HTMLMediaElement.audioTracks` is not implemented in Chromium, so a multi-track file gives whichever track the demuxer lands on and no way off it. |

`apps/desktop/src/playbackReach.test.ts` reads the first of those out of the
three files rather than asserting it in prose: the day somebody enables the asset
protocol, grants a file-system permission or widens the policy, that test fails
and brings them here. A claim in a comment is true on the day it is written; a
claim a test resolves is true whenever it passes.

The fourth row is the one that decides the shape of the answer. **No arrangement
that hands a whole multi-track file to a media element can satisfy #52's first
acceptance criterion**, however the container question is settled, because the
element has no way to switch tracks. Track selection has to happen on the way
*out* of the recorder.

### The decision, and what it costs

**The recorder serves the media; the window plays a stream, one track at a time.**
[Issue #304](https://github.com/wildware-uk/clipped/issues/304) builds it.

Concretely: a protocol command opens a recording for playback and reports its
duration, its dimensions and its track list; the recorder remuxes the source into
fragmented MP4, copying the video without re-encoding it and encoding the chosen
audio track to AAC; and it answers byte ranges, so a seek is a range request
rather than a re-read. The Tauri host registers a URI scheme that relays those
ranges and the screen points a `<video>` at it. The compatibility mix is the
default, which is what the container already flags ([muxing.md](muxing.md)), and
choosing another track is a new URL at the current time.

The recorder rather than the window, because
`tests/integration/tests/workspace_layering.rs::the_desktop_application_links_nothing_of_this_workspace_but_the_protocol`
permits `src-tauri` exactly one crate of the workspace, `clipped-ipc`. The window
may not link `clipped-muxer`, so it cannot remux or encode anything in its own
process, and it should not: that is the boundary
[ADR 0002](adr/0002-separate-recorder-process.md) exists to keep.

What it costs, stated rather than glossed:

- **A live remux and an audio encode for every recording watched**, and again for
  every track switched to. Video is copied, so the cost is the audio encode and
  the container work, not a transcode — but it is not free, and it happens beside
  a game.
- **An audio encoder Clipped does not have.** Measured on this machine: the
  pinned LGPL FFmpeg build carries FFmpeg's native `aac` encoder
  (`third-party/ffmpeg/current/bin/ffmpeg -encoders`), so this is wiring rather
  than a new dependency or a fresh licence question — but it is still a subsystem
  that does not exist.
- **Seek accuracy is the video's keyframe interval** unless the served stream
  carries an index. "Frame-accurate seeking where practical" is the ticket's own
  wording, and this is where the practical limit sits.
- **Privilege.** The window gains a way to receive bytes it could not before.
  #304's last criterion is that whatever it gains is the smallest thing that
  works, and that `playbackReach.test.ts` is rewritten to describe the new
  boundary rather than deleted.

The alternatives, and why not:

| Instead | Why not |
| --- | --- |
| Point a `<video>` at the MKV through Tauri's asset protocol | Rows two, three and four above. It is the cheapest thing to write and it plays nothing. |
| Remux the whole file to MP4 first ([#92](https://github.com/wildware-uk/clipped/issues/92)) and play that | #92 copies streams without re-encoding, so the video arrives and **the sound does not** — PCM has nowhere to go in an MP4 a browser will decode. It also writes a second full-size copy of every recording somebody watches, and makes playback wait for a pass over the whole file. Even with the audio encoded it still cannot answer #52's track selector: one file, one track a media element can reach. |
| Convert on the fly to WebM instead | The video would have to be re-encoded, because WebM cannot carry H.264 or HEVC. That is the one thing worth avoiding: a transcode of gameplay footage beside a running game. |
| A native video surface behind the webview | No transport, no keyboard handling and no layout that the rest of the interface shares, and Tauri offers nothing for it. It is the answer if the stream above proves too expensive, and it is a much larger change. |

### What it does show

The one thing that is real: **what the recorder link says about this recording.**
The window follows a single recorder, so it learns of exactly two recordings —
the one being written now, and the one a recorder died in the middle of, whose
file [ADR 0006](adr/0006-recorder-lifetime-and-supervision.md) says naming is the
whole of recovery. `resolveClip` has one answer for each, and one for everything
else.

That third answer is the careful one. It reads **"Not known to this window"** and
explains that the library index is where a recording would be looked up and that
this screen has not looked in it
([#52](https://github.com/wildware-uk/clipped/issues/52)). Since #301 the index
*can* be read — the Library screen does — but the identifier in the address bar
is the recorder's `recording_id` for a live recording and the index keys
recordings by its own integer, so reconciling the two is work of its own.
It does **not** say the recording is missing. This window has not been to the
disk and cannot; `missing_since` in the library index is the only thing that has
looked ([#56](https://github.com/wildware-uk/clipped/issues/56)), and reporting a
file as gone because *this* window could not find it is exactly the invented
state AGENTS.md section 27 is about. `clipPlayback.test.ts` asserts the wording
carries none of "missing", "gone" or "deleted", so the distinction cannot be lost
to an edit that reads better.

Where a recording *is* known, the screen shows the four fields the protocol
carries and no more: the file in full, the capture target, and how long the
recorder had been recording when it last said so — labelled as a lower bound
rather than a duration, because nothing has opened the file, and a recording a
killed recorder left may have no Matroska trailer at all
([#283](https://github.com/wildware-uk/clipped/issues/283)). **There is no
duration, no thumbnail and no waveform**, because there is nothing to get them
from.

### Why it is reachable from the sidebar, and nowhere else

A screen nothing links to is a screen nobody finds. The one recording this window
can name is the one a recorder died writing, and the sidebar notice that names
the file it left now carries a link to that recording's screen.

That link is a destination and not a control: it does not claim the recording
will play, and the screen it leads to says so in its first paragraph. It is the
same bargain the tray's Open Library keeps — "a thing that happens, rather than a
control that does nothing". Everything else waits on this screen looking a
recording up in the library index (#52); Home and Library (#60), which now list
what the index holds, are what will open this screen properly.

### What is not built

Every row of the screen's second table, each naming the work that supplies it:
playing anything at all and choosing a track (#304); opening a recording somebody
picked, and saying a file has gone (#52 — the read that carries both landed with
#301, and this screen does not yet use it); a poster frame, which is the thing
[#57](https://github.com/wildware-uk/clipped/issues/57) has been waiting for —
thumbnails are generated, cached and tested, and *nothing has ever drawn one*, so
on a real machine none is produced; a waveform
([#66](https://github.com/wildware-uk/clipped/issues/66)); and bookmarks and
events on a timeline ([#64](https://github.com/wildware-uk/clipped/issues/64) and
[#65](https://github.com/wildware-uk/clipped/issues/65)).

The alternative to that table was a transport bar, a scrubber and a track
selector drawn over a black rectangle. That is AGENTS.md section 27 broken twice
in one screen — controls that do nothing, above a picture Clipped never made —
and the scrubber is the worst of the three, because a scrubber implies a duration
and nothing in this window has measured one.

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
with the screen that first needs them. **The export dialog has since been
written and deliberately has no chips**: the presets they would select are
resolution, framerate, codec and quality, none of which the export engine can
honour — see [The export dialog](#the-export-dialog). So #215's chips are now
waiting on the first screen that has something to select, which is not this one.

The dialog is the one part of the set that is also a **component**,
`Dialog` in `@clipped/ui`. Its classes are in `components.css` with the rest;
what the component adds is the behaviour no class can carry and that every
dialog has to have the same way (AGENTS.md section 46) — the `dialog` role and
an accessible name taken from the title on screen, Escape, focus moving in when
it opens and back to whatever opened it when it closes, and Tab cycling inside
it. It is not built on the platform's `<dialog>` and `showModal()`, which would
give all of that for nothing: jsdom 27, the environment every test here runs in,
does not implement `showModal`, so a dialog built on it would be non-modal and
always visible in tests and every assertion about modality would be an
assertion about nothing. Five screens are now written and none
needed either — the Settings rail draws `role="tab"` buttons, but that is the
rail pattern below rather than the deck's underlined strip: the tab strip belongs
to the per-game detail view behind the game list, which #245 has to land first,
and to the Library's Sessions / Clips / Highlights row, which has nothing to
switch between until #301 does — see
[No tab strip and no chips](#no-tab-strip-and-no-chips).

Beside the set above, the shell has classes of its own that a screen draws with.
They are not from the reference pages, which have no screen in them:

| Class | What it is |
| --- | --- |
| `.clipped-screen__title`, `.clipped-screen__heading` | A screen's own two levels of heading |
| `.clipped-screen__lead` | Running prose at the measure |
| `.clipped-panel` + `__heading`, `__body` | The marked panel: an accent rule down the left of the one paragraph that has to be read. Drawn by an unbuilt screen's "Not built yet", by the Games screen's detection state, by Home's recording state, by Library's reason for being empty, by the Editor's "No clip is open", by the Settings screen's one statement and by the Diagnostics screen's capture health — all of them the same thing to look at |
| `.clipped-screen__split` + `__pane` | A screen divided into a rail of sections and the pane one of them opens. `--rail-width` is its one metric |
| `.clipped-rail` | The rail itself, which draws its entries in `.clipped-nav__link` rather than in a class of its own — the same reasoning as the panel above |
| `.clipped-code` | Text somebody types or finds in a file: a settings key, a path, a command |
| `.clipped-path` | A file path, printed in full: monospaced, and broken anywhere, because a Windows path has no spaces to break at. It sets no size, so it takes whatever block it sits in, and no colour, so it is the window's own ink |
| `.clipped-screen__report` | A block of machine-written text a person is meant to read before sending it on: the Diagnostics screen's support report. Monospaced, on the card ground, wrapping rather than scrolling and with no height limit |
| `.clipped-editor__*`, `.clipped-timeline__*` | The Editor's timeline — see below. `.clipped-editor__header` carries the clip's name and Export, and `.clipped-editor__reasons` is the export dialog's list of what decides an export; both are the Editor's, and neither sets a colour |

**Every screen written so far consumes the component layer** — Home, Library,
Games, Editor, Settings, Playback and Diagnostics, between them `.clipped-table`,
`.clipped-panel`, `.clipped-muted`, the rail, the screen classes above, the
secondary button on the Editor, and `.clipped-btn--primary` on Diagnostics, the
first button in the application outside the skip link. The classes exist ahead of
that so that #94 does not invent its own styling, which is the reason issue #79
followed the shell.

`.clipped-nav__link` now serves two mechanisms: the sidebar's anchors and the
rail's `role="tab"` buttons. The declarations a button needs and an anchor does
not — a width, a border, a ground, a typeface, a text alignment — are in the one
rule rather than in a second class, because two classes drawing the same thing
drift, and a screen's rail that stopped matching the sidebar would look like a
mistake. The rule that marks the open one covers both `aria-current="page"` and
`aria-selected="true"` for the same reason, and `contrast.test.ts` measures it on
both grounds it is drawn on.

`.clipped-screen__report` is Diagnostics' own, for the block of machine-written
text a user is asked to read before sending it on. It is not a component-layer
class: nothing in the deck has one, and it belongs to the screen that needs it
rather than to the system.

One element default was added with the playback screen and belongs with the
classes above: **`code` takes `--font-mono`**. That token was declared with the
other two typefaces in issue #79 and had no consumer until a screen had to put a
recording's full path in front of somebody — the one thing on that screen anybody
can act on, and a string that has to be read character by character. It is a step
down the type scale, because a monospace face at the body size reads larger than
the body around it, and it wraps anywhere, because a Windows path has no space in
it to break at and would otherwise push a table wider than the window.

### The Editor's timeline is screen classes, not components

The component layer is the set the design system's reference pages draw and the
deck builds screens from; a timeline is not one of them. So the Editor's classes
sit in `styles.css` beside the shell's own — which is what keeps them inside the
same two gates, because `stylesheet.test.ts` and `contrast.test.ts` read that
file. A parallel stylesheet in `apps/desktop` would have been outside both.

Two things follow from a timeline that are worth knowing before adding to it:

- **Every distance _along_ the timeline is a percentage**, written by the
  component rather than by the stylesheet, because a position is a proportion of
  the clip. Percentages are deliberately outside `stylesheet.test.ts`'s gate for
  exactly this reason: there is nothing for a token to hold. What is fixed —
  how tall a lane is, how tall the ruler is, how wide the label column is — is
  three tokens in `tokens.css`, each with the reason it is not on a scale.
- **A segment is bounded rather than filled.** Where one segment ends and the
  next begins *is* the cut, so the edge identifies something rather than
  separating two paragraphs: it takes `--color-control-edge` and is measured
  against WCAG 1.4.11's 3:1, as is the playhead, on both grounds it crosses.

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
  WAI-ARIA tab list's contract — see [The rail](#the-rail). The Editor's timeline
  is the one place where that rule needed real design rather than markup, and
  [its own table](#the-keyboard) says what every key does: Page Up and Page Down
  land exactly on a cut, which is the thing a pointer cannot do at all.
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
  | A segment's recording name          | 15.21:1 |
  | Body text on the window ground      | 14.86:1 |
  | A section in a screen's rail        | 14.86:1 |
  | A button's label, unfilled          | 14.86:1 |
  | Body text on a card                 | 13.70:1 |
  | A field's own text                  | 13.70:1 |
  | A dialog's title                    | 13.70:1 |
  | The title strip                     | 11.45:1 |
  | The editor frame's "no picture"     | 11.45:1 |
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
  | A fact's term at the playhead       | 5.81:1  |
  | A ruler mark's label                | 5.81:1  |
  | A lane's "No waveform"              | 5.59:1  |
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
  | The playhead over a segment                               | 3.85:1 |
  | A secondary button's edge                                 | 3.85:1 |
  | A radio's ring                                            | 3.85:1 |
  | The segmented control's edge                              | 3.85:1 |
  | The focus ring on the window ground                       | 3.76:1 |
  | The radio's own ring, on its stand-in                     | 3.76:1 |
  | The segmented option's focus ring, against its halo       | 3.76:1 |
  | A field's edge against its own fill                       | 3.55:1 |
  | A field's edge on a card                                  | 3.55:1 |
  | A segment's edge, which is where the cut is               | 3.55:1 |
  | The playhead on a lane                                    | 3.47:1 |
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
`npm run lint`, and `apps/desktop/src/Shell.test.tsx`, `GamesScreen.test.tsx`,
`HomeScreen.test.tsx` and `LibraryScreen.test.tsx` drive the window with Tab and
Enter rather than asserting that the markup looks right. Home is the screen the
application opens on, so its case opens the window on Library — by putting the
route in the fragment before mounting, as reopening the application on the last
screen would — and then tabs back to Home, because the shell deliberately does
not move focus on the *first* screen and there would otherwise be nothing to
observe.

## Testing

`npm test` runs Vitest, from `apps/desktop` but over `packages/*/src` as well,
because those packages are consumed as source and one test command for the npm
workspace is worth more than a second configuration to remember. Most of it runs
against jsdom; `packages/ui/src/contrast.test.ts` and
`packages/ui/src/stylesheet.test.ts` ask for the node environment, because they
read the stylesheets as text and Vitest replaces a CSS import with an empty
module.

The tests assert the things about this shell that would rot quietly: that no
part of it shows data it does not have — including that the controls in the whole
window are the skip link, Home's record button, the Settings rail and
Diagnostics' Copy report and no others, that Games offers none at all and
Diagnostics offers no button that would not work — that the chrome is operable
from the keyboard alone, that every pairing of words and ground clears 4.5:1, and
that the component layer still consumes the design system rather than a value
somebody typed. The last of those is why `stylesheet.test.ts` exists: "no hard-coded
colours" is a promise a reviewer has to re-check on every diff, and a test that
reads the stylesheet is one that cannot be forgotten.

`playbackReach.test.ts` is the same idea aimed at a different file. The playback
screen's central claim is that this window has no way to load a file from the
disk, and that is a fact about `tauri.conf.json` and `capabilities/default.json`
rather than about any code — so it reads both, asserts the asset protocol is off,
that the three granted permissions are exactly the three, that the policy has no
`media-src`, and that no scheme which could carry a local file appears in it at
all. It runs in the node environment, like the two stylesheet files, for the same
reason: the subject is configuration as text. Enabling any of those makes it fail
rather than leaving a paragraph on screen that has quietly stopped being true.

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

`HomeScreen.test.tsx` and `LibraryScreen.test.tsx` are built the same way, and
add one check the other screens do not need: **no figure that nobody measured**.
`test/counts.ts` matches a number against the nouns SPEC.md sections 17, 29 and
30 count — sessions, recordings, clips, favourites, highlights, games, bytes.

Since #301 that check has a condition on it, and the condition is the whole
point: Home is held to drawing **no** such figure *while the library has not
been read* — the stub answers neither library command — and a second case reads
a library and holds the figures on the screen to being the index's own. Both
halves are needed. The first alone would pass on a screen that can never show
anything; the second alone would pass on a screen that invents while a read is
in flight or has failed.

Home adds two more of its own that are worth knowing about before changing them.
**The link and the recorder's answer are driven apart on purpose** — the case
stubs a link saying "idle" and a `get_status` saying "recording", and the
reverse — so that a screen reading the wrong one fails rather than passing
because the two happened to agree. And **one case runs in real time**: the
recorder answers the same `elapsed_ms` however often it is asked, the case waits
for four rounds of asking, and the duration on screen has to be unchanged. A fake
clock is exactly what a local timer would also be driven by, so advancing one
would prove less; the few seconds it costs buy the acceptance criterion.

That check is run over each **text node** rather than over the screen's
`textContent`, and the reason is worth knowing before writing another one like
it: `textContent` concatenates adjacent table cells with nothing between them.
A row ending "Issue #301" beside one beginning "Clips" reads as `301Clips`,
which fired the pattern on a screen that draws no figure at all; and a real
"0 sessions" followed by the next heading reads as `sessionsWhat`, whose missing
word boundary made the pattern *miss* the very thing it exists for. Both
happened while the check was being written, and the second only came to light
because the mutation that should have failed it did not.

The Editor's three files are tested at the level each is about, and the split is
deliberate:

- `document.test.ts` is about **refusing**. Every row of `docs/editing.md`'s
  compatibility table has a case, and the unknown-field sweep pushes a key this
  build does not know into each object of a fully populated document in turn —
  the same sweep `crates/edit`'s round-trip test does, so a structure added to
  the model later is covered without anybody adding it to a list.
- `timeline.test.ts` is about **agreeing with the crate**. Its assertions are
  the ones `crates/edit`'s `timeline.rs` makes, against the same clip: eight
  seconds in is ninety-two seconds into the recording, the material a cut
  removed is unreachable, a half-speed segment lasts twice as long and runs
  through its material half as fast, and the end of the clip is past its end.
  One case is about the port rather than the model: at the top of the range a
  document may hold, `BigInt` and a double give answers a nanosecond apart, and
  the test asserts both.
- `EditorScreen.test.tsx` drives **real keys at the real element** and asserts
  the timecode the screen shows, so a key that moved a variable nothing draws
  would fail. It also asserts the absences: no timeline at all when no clip is
  open, "No waveform" in every lane, no picture, and that the only controls on
  the screen are the three zoom buttons.

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

`ClipPlaybackScreen.test.tsx` is built the same way and asks the same two
questions. The first is that the screen follows the recorder: one address is
opened and the link is then moved underneath it three times — no recorder, then a
recorder writing that very recording, then idle — and the panel has to say
something different each time, which a screen with the wording baked in could not
do. The second is absence, and it is stated as elements rather than as sentences:
no `video`, `audio`, `source` or `track`, no `img` or `canvas`, and no button,
slider or combobox anywhere in `<main>`. The scrubber is the one that matters —
`queryAllByRole('slider')` is what stops one being added over a duration nobody
measured. `clipPlayback.test.ts` covers the decisions underneath: which of the
two recordings the window may name wins when both carry the same identifier, and
that no wording anywhere says a recording is missing.

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
