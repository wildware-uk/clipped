# Diagnostics and the support report

SPEC.md section 36 asks for excellent diagnostics from day one: twelve things
logged, and `Diagnostics → Export Support Bundle` to send them with. The logs are
built — `docs/logging.md` covers where they go, what may appear in them and what
may not — and this document covers the other half: the screen a user opens when
something is wrong, and the report they send.

It is also the honest account of what that screen can and cannot show, and why
the rest is a list of what is missing rather than a dashboard of zeros. Four of
the twelve reach the window; nine of the remaining rows name the work that would
supply them.

Related standards: SPEC.md sections 36 and 37, AGENTS.md sections 13, 27 and 45,
[privacy.md](privacy.md), [logging.md](logging.md).

## Contents

- [Where it is](#where-it-is)
- [The capture health summary](#the-capture-health-summary)
- [What this build reports](#what-this-build-reports)
- [The support report](#the-support-report)
- [What is in it, field by field](#what-is-in-it-field-by-field)
- [What is never in it](#what-is-never-in-it)
- [How a path is reduced](#how-a-path-is-reduced)
- [What is not built](#what-is-not-built)
- [What is tested, and what is not](#what-is-tested-and-what-is-not)

## Where it is

The Diagnostics screen, in `apps/desktop/src/DiagnosticsScreen.tsx`, reached from
the sidebar's Maintenance group at `#/diagnostics`. It is the second of the
seven screens to be written (issue #101; Games was the first, issue #107).

The wording and the report are in `diagnostics.ts` beside it, and the redaction
they depend on is in `redactPath.ts`. Splitting them that way is not ceremony:
the component is untestable without a DOM and the other two are pure, and the
parts worth guarding are the pure ones.

Three parts, in the order somebody with a problem reads them:

```text
Diagnostics
───────────────────────────────────────────────
▌ Recording                         ← capture health, a live region
▌ Recording process `cs2.exe` to D:\clips\match.mkv.
▌ No recording has failed or been interrupted since this window opened.
▌ This describes the recorder this window is attached to, since …

What this build reports             ← the twelve of SPEC.md section 36
  Game detection      Counter-Strike 2, in the sitting cs2-20260811-201400 …
  Capture backend     Desktop Duplication, chosen automatically. …
  Resolution changes  Not reported. Issue #98.
  Encoder             NVIDIA NVENC on NVIDIA GeForce RTX 4090. …
  …
  Recording paths     D:\clips\match.mkv
  …

What this machine can encode        ← the report `capabilities` prints
  NVIDIA GeForce RTX 4090   nvidia, own video memory, driver 32.0.15.6094 …
  NVIDIA NVENC              Available, and this build can record with it.
  AMD AMF                   no adapter from this vendor is present

Support report                      ← what you send, shown in full
  Clipped diagnostics report
  Taken                  2026-08-12T09:14:02.311Z
  …
  [ Copy report ]
```

## The capture health summary

`describeCaptureHealth` has one rendering for each of the recorder link's four
states, plus one for a recorder that is attached and recording, and one for
"this is not the Clipped window" — which is what `npm run dev:web` and the tests
see. The screen is a pure function of it, so it follows the recorder rather than
restating a sentence somebody typed.

| Link | State | What it says |
| --- | --- | --- |
| Not the Clipped window | `Not known` | There is no recorder to ask. |
| `connecting` | `Not known yet` | Looking for the recorder. |
| `reconnecting` | `Not known` | This window has lost sight of the recorder, with the reason and which attempt. |
| `unavailable` | `No recorder` | The link's own reason, and what to do about it. |
| `attached`, idle | `Ready` | The recorder is running and **it says** nothing is being recorded. |
| `attached`, recording | `Recording` | What is being recorded, and the file. |

Beneath that, whichever of these applies:

- **A recording failed.** The recorder's own sentence, its stable error code, and
  the file it wrote up to the failure.
- **A recording was interrupted.** The recorder died mid-recording; the file is
  named and it was not resumed ([ADR 0006](adr/0006-recorder-lifetime-and-supervision.md)).
- **Neither** — *"No recording has failed or been interrupted since this window
  opened."* Stated rather than left blank, because a screen that says nothing
  where a failure would go is indistinguishable from one that is not watching.
  That sentence is the healthy case, and it is the third of issue #101's
  acceptance criteria.

### The claim it must never make

**No rendering says that nothing is being recorded unless the recorder said so.**

A link that dropped tells you *this window* lost sight of a recorder. It does not
tell you the recorder stopped: the recorder is a separate process precisely so
that it goes on recording when no window is open
([ADR 0002](adr/0002-separate-recorder-process.md)), and a window that announced
"nothing is being recorded" on a dropped pipe would be stating something it has
not looked at and cannot look at. The one state that says it is `attached` and
`idle`, where the recorder itself is the source.

`DiagnosticsScreen.test.tsx` asserts that property over every state rather than
over the one that would be wrong today, because it is the same mistake whichever
branch it gets written into. It is the same defect the Games screen had and
fixed — a heading that made a claim about the machine out of a reading about one
recorder.

### The action

A failure that arrives with nothing to do about it is the message AGENTS.md
section 45 exists to prevent. `No recorder` therefore carries one: restarting
Clipped makes the attempt again — a Try again control on this screen is
[issue #221](https://github.com/wildware-uk/clipped/issues/221) — and the
recorder's own account of what went wrong is in its log files, at the directory
the sentence names. `reconnecting` says that there is nothing to do *yet* and
that the link will say so if it gives up. The two working states carry no action,
because instructions under a working recorder are noise.

## What this build reports

SPEC.md section 36 lists twelve diagnostics. **This window can report four of
them.** The rest are inside the recorder process with nothing measuring them.

The screen draws all twelve as a table, one row each, with what this build
reports against each. Drawing gauges reading zero was the tempting alternative
and is the one AGENTS.md section 27 rules out: a dropped-frame count of zero and
a dropped-frame count nobody took are different facts, and this build has not
counted.

| Diagnostic | What this build reports |
| --- | --- |
| **Game detection** | **The game the open sitting is of**, and when that sitting started — or that no sitting is open. Read off the `session` a `recording` or `watching` status carries, so the name survives the seconds a sitting spends waiting out a restart grace with nothing being recorded ([#241](https://github.com/wildware-uk/clipped/issues/241), protocol 2). |
| **Capture backend** | **The method capturing the recording in progress**, what it was asked for, the method that recording started with, and every replacement and restart with the recorder's own reason. From `clipped_capture::CaptureStatus` ([#97](https://github.com/wildware-uk/clipped/issues/97)) through [`get_diagnostics`](ipc.md#get_diagnostics). Absent when nothing is being recorded, because there is no backend running then. |
| Resolution changes | Not reported. A recording follows its target being resized ([#98](https://github.com/wildware-uk/clipped/issues/98)); nothing counts the times it happened. |
| **Encoder** | **The adapters, the encoder families, the codecs and the limits** — everything `clipped-recorder capabilities` prints ([#14](https://github.com/wildware-uk/clipped/issues/14), [encoder-capabilities.md](encoder-capabilities.md)), through the same command, drawn as its own section below the table. |
| Dropped frames | Not reported. The `metrics` event stream is defined and this recorder refuses it with `not_implemented`. [#100](https://github.com/wildware-uk/clipped/issues/100) |
| Encoder latency | Not reported. [#100](https://github.com/wildware-uk/clipped/issues/100) |
| Audio drift | Not reported. [#100](https://github.com/wildware-uk/clipped/issues/100) |
| Audio devices | Not reported. A recording does capture audio (#180) and the recorder can list this machine's microphones for the Settings screen; which devices a *recording* used is not carried into diagnostics. [#100](https://github.com/wildware-uk/clipped/issues/100) |
| **Recording paths** | **The path of the recording in progress**, which arrives inside a `recording` status — or, when nothing is being recorded, that there is none. |
| Muxer status | Not reported. [#100](https://github.com/wildware-uk/clipped/issues/100) |
| Disk latency | Not reported. [#100](https://github.com/wildware-uk/clipped/issues/100) |
| Plugin events | Not reported. There is no plugin system. [#69](https://github.com/wildware-uk/clipped/issues/69) |
| Log files | `%LOCALAPPDATA%\Clipped\logs`, rotated hourly with the newest 48 kept. This window can neither read them nor open the folder. [#303](https://github.com/wildware-uk/clipped/issues/303) |

The last row is not on the specification's list and is on the screen because it
is the one thing a user can act on today: the logs exist, they are the primary
diagnostic, and attaching them by hand is a thing a person can do.

**How the four get here, and why the rest do not.** The window reaches the
recorder over [the control protocol](ipc.md). The recording path and the game
arrive inside a `recording` or `watching` status, which it was already
subscribed to; the capture backend and the encoder are what
[`get_diagnostics`](ipc.md#get_diagnostics) answers, asked once when this screen
opens through the `recorder_diagnostics` command on the Tauri host
(`apps/desktop/src/recorderDiagnostics.ts`).

The rest are not waiting on a transport any more. **Nine of them are waiting on a
measurement**: the `metrics` stream is defined and this recorder refuses it with
`not_implemented` because nothing counts a dropped frame, times an encode or
watches the muxer during a recording ([#100](https://github.com/wildware-uk/clipped/issues/100)),
and there is no plugin system for a plugin event to come from. The log files are
a file-system permission this window does not have
([#303](https://github.com/wildware-uk/clipped/issues/303)). Each row names its
own, which is the difference between a screen that is missing a feature and one
that is broken.

**One of them was waiting on nothing.** The Game detection row named
[#241](https://github.com/wildware-uk/clipped/issues/241) and said the protocol
had "no vocabulary for a game or a session" long after protocol 2 had given it
one — the sitting is on the status, the recording-now panel was already reading
the game's name off it, and this screen had not been changed to. That is the
shape of defect this repository keeps finding: not a producer that was never
built, but one nothing was rewired to consume.

## The support report

**What Clipped has today is a diagnostics report, not the bundle SPEC.md section
36 asks for.** The difference is the log files, and it is stated plainly here
rather than glossed: see [What is not built](#what-is-not-built).

The report is composed in the window from what the window was told, shown **in
full** on the screen — every line of it, no scroll box, no fold — and copied to
the clipboard, which is where a bug report is pasted from. Showing it before it
goes anywhere is [privacy.md](privacy.md)'s rule applied literally: nothing
surprising, nothing hidden. What is copied is byte for byte what was shown, and
a test asserts that rather than trusting it.

An example, from a machine where a recording failed and the recorder was later
found to be missing:

```text
Clipped diagnostics report

Taken                  2026-08-12T09:14:02.311Z
Interface              @clipped/desktop 0.1.0
Protocol version       1
Webview                Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Edg/141.0.0.0
Status observed        2026-08-12T09:13:58.000Z
Recorder link          attached
Recorder process       4242
Recorder state         recording
Recording id           r-7
Recording target       process cs2.exe
Recording file         match.mkv#eb9715073a66288e
Elapsed when observed  42 s
Capture health         Recording — Recording process cs2.exe to match.mkv#eb9715073a66288e.
Capture backend        Desktop Duplication
  asked for            Automatic
  started with         Windows Graphics Capture
  changed              Desktop Duplication took over from Windows Graphics Capture
                       (capture_failed): the compositor stopped delivering frames
Encoders               nvenc on NVIDIA GeForce RTX 4090 (h264, hevc, av1)
  reading              stored, taken 2026-08-11T20:14:00+01:00
Recording failed       r-7
  seen                 2026-08-12T09:13:12.000Z
  code                 recording_failed
  message              the muxer could not write to match.mkv#eb9715073a66288e
  file                 match.mkv#eb9715073a66288e
Recording interrupted  r-6
  target               process cs2.exe
  file                 earlier.mkv#c02c1076eaa32bc3
  elapsed              1 min 30 s
Notice                 the recorder was not found at clipped-recorder.exe#c01c72cf03258db7

Not reported by this build: Resolution changes, Dropped frames, Encoder latency,
Audio drift, Audio devices, Muxer status, Disk latency, Plugin events, Log files.
Log files are not in this report. They are in %LOCALAPPDATA%\Clipped\logs; attach
clipped.*.log yourself if the problem is a recording that failed (issue #303).
Paths are reduced to a file name and a digest of the whole path, the way Clipped
reduces one in a log line, so no directory component leaves this machine.
```

The last four lines are not decoration. A reader of a bug report has to be able
to tell *"dropped no frames"* from *"counted no frames"*, and the list of what is
not reported is the only thing that says which.

## What is in it, field by field

This is the whole list. There is no other field, and
`DiagnosticsScreen.test.tsx` asserts the exact set — so a field added later has
to be added to that list, and to this table, before the suite goes green.

| Field | Where it comes from | Why it is worth sending |
| --- | --- | --- |
| `Taken` | The window's clock, when the report was composed | Nothing else in the report is timestamped by the recorder |
| `Interface` | `apps/desktop/package.json` | Which build of the window wrote it |
| `Protocol version` | `PROTOCOL_VERSION` in `@clipped/shared` | Version skew between the two processes is a whole class of bug ([ipc.md](ipc.md)) |
| `Webview` | `navigator.userAgent` | The Windows build and the WebView2 version. No account name, no machine name |
| `Status observed` | When the window last had a state from the link | Makes `Elapsed when observed` readable instead of misleading |
| `Recorder link` | `RecorderLinkState`'s tag | `connecting`, `attached`, `reconnecting` or `unavailable` |
| `Recorder process` | The pipe's own answer | Tells a recorder that was replaced apart from one that never went |
| `Recorder state` | The recorder's `status` | `idle` or `recording` |
| `Attempt`, `Delay`, `Reason` | The `reconnecting` and `unavailable` states | The supervisor's own sentence, which is where "the recorder exited with status 1" appears |
| `Recording id` | The `recording` status | Lines the report up with the log records for the same recording |
| `Recording target` | The `recording` status | ``process `cs2.exe` ``. **Never a window title** — the protocol does not carry one, deliberately |
| `Recording file` | The `recording` status, redacted | Which file, without saying where it lives |
| `Elapsed when observed` | The `recording` status | How long it had been going when the recorder last answered |
| `Capture health` | The summary above, redacted | One line saying what the screen said |
| `Capture backend` + `asked for`, `started with`, `changed` | [`get_diagnostics`](ipc.md#get_diagnostics) | Which backend recorded the file, and every one it fell past. `none — nothing is being recorded` when there is no recording, `not read: …` when the recorder could not be asked — the two are different facts and neither is left blank |
| `Encoders` + `reading` | The same reply | What this machine can encode, and whether the answer was taken now or stored. A recording that failed on a machine whose driver was updated last week reads differently from one that failed on a machine with no hardware encoder |
| `Recording failed` + `seen`, `code`, `message`, `file` | The `recording_failed` event | The reason anybody sends a report at all |
| `Recording interrupted` + `target`, `file`, `elapsed` | The `recording_interrupted` event | A recorder that died mid-recording, and the file it left |
| `Notice` | The startup notice and the tray | The only record of a failure that happened before React was running — a notification-area icon that could not be added changes what closing the window does |

Both failure blocks read `none since this window opened` when there has been
none, rather than being left out. Absence and *"nothing has happened"* are
different things, and only one of them is a measurement.

## What is never in it

Stated so that a future contributor can see what would be a regression.

- **No recorded media**, at any size, in any form. No video, no audio, no
  screenshot, no thumbnail, no waveform. Nothing of the sort reaches this window
  in the first place: it holds no frames and no samples, and there is no field
  above that could carry one.
- **No window contents, microphone content, private message contents or file
  contents** (AGENTS.md section 13). Same reason.
- **No window title.** The protocol describes what is being recorded as
  ``process `cs2.exe` `` and never as a title, because a title is user content
  and is the most reliable way to get somebody's document name into a screenshot
  of a bug report.
- **No absolute path.** Every path is reduced (below), including the paths inside
  the recorder's own sentences.
- **No library database, settings file or game catalogue.** None of them is
  reachable from this window, and the first two would name every file the user
  has recorded and the directory they are in.
- **Nothing is transmitted.** The report is put on the clipboard when the user
  asks. Clipped opens no socket, here or anywhere
  ([privacy.md](privacy.md)'s register is empty by design).

## How a path is reduced

The same reduction `crates/logging/src/redact.rs` applies before a path reaches a
log line, restated in TypeScript in `apps/desktop/src/redactPath.ts`: the final
component, plus an FNV-1a digest of the whole path.

```text
C:\Users\alice\Videos\Clipped\match.mkv   →   match.mkv#eb9715073a66288e
```

No directory component survives — not the account name, not the drive layout,
not the folder names somebody chose. Equal digests mean the same path, so a
report can still be lined up with the log lines about the same file, and two
recordings that share a name stay apart.

Three details are worth stating rather than discovering:

- **It applies to free text as well as to path fields.** The leak that matters is
  not the recording's own `output`, which is obviously a path; it is
  `SupervisorError::ExecutableMissing`, whose sentence reads *"the recorder was
  not found at `C:\Users\alice\…\clipped-recorder.exe`"* and is exactly the
  sentence a user is asked to send. **Every** free-text value in the report goes
  through `redactPathsIn`, and the first version of this screen leaked through
  the one that did not — the health summary, which names the recording's file in
  full on purpose, because on screen that path is the one thing anybody can act
  on. A test caught it, which is the only reason it is not in this document as a
  guarantee that was not true.
- **The named pipe is deliberately left alone.** `\\.\pipe\clipped-recorder.1` is
  the endpoint the two processes talk over. It is in most of the sentences the
  supervisor writes about a recorder it could not reach, it is what somebody
  diagnosing that needs, and it names nothing of the user's: it is a
  device-namespace name, not a filesystem location.
- **A relative name is left alone too.** `clipped-recorder.exe` has no directory
  component, so there is nothing to redact and a digest would only make the
  sentence harder to read.

**Two implementations, one function.** The window may not link
`clipped-logging` — `tests/integration/tests/workspace_layering.rs` permits
`apps/desktop/src-tauri` exactly one crate of the workspace, `clipped-ipc` — and
a webview could not call it in any case. What stops the two drifting is that
`redactPath.test.ts` pins the same string `redact.rs` pins for the same input,
`match.mkv#eb9715073a66288e`, so a difference in either side's arithmetic fails a
test rather than producing two digests for one file.

Redaction is a backstop, not a licence. It does not anonymise the file *name*,
and Clipped names its own recordings, but a path the user chose can carry meaning
in its final component.

## What is not built

**The bundle.** SPEC.md section 36 asks for `Diagnostics → Export Support
Bundle`, and a bundle worth sending is this report *plus the log files*. The log
files are at `%LOCALAPPDATA%\Clipped\logs` and this window cannot reach them:
`capabilities/default.json` grants three `core:` permissions and `dialog:allow-save`, none of which
touches the file system, and there is no command that would read a log or open a
folder. Writing one archive with both in it is
[issue #303](https://github.com/wildware-uk/clipped/issues/303).

**So there is no Export Support Bundle button**, and that is a decision rather
than an omission. A button that opened a save dialog and wrote a report with no
logs in it would be an export in name only; a disabled one would say less than
the paragraph that names the issue and the directory (AGENTS.md section 27, and
the same reasoning the Games screen applied to its Add Game control). What the
screen does instead is name the directory, so that attaching `clipped.*.log` is
something a person can do today.

**The nine that nothing measures.**
[Issue #302](https://github.com/wildware-uk/clipped/issues/302) built the
command that carries the capture status and the capability report, so those two
rows are measurements now. The nine that remain are not waiting on a way to
travel; they are waiting on something to count them, which is
[#100](https://github.com/wildware-uk/clipped/issues/100) for the eight figures a
running recording would produce and
[#69](https://github.com/wildware-uk/clipped/issues/69) for plugin events. Until
those land, the table of what is missing is as much a part of this screen as the
four rows that are not.

**Falling back mid-recording.** `CaptureFallback::recover` and
`recover_from_black_frames` are built and tested in `clipped-capture` and are
called by nothing: `clipped_session::record` uses the fallback to choose a
backend and drops it before the frame loop starts. So the change list this screen
draws only ever carries the start-up fall-through — a preferred backend that
could not be created — and a backend that dies mid-recording still ends the
recording rather than being replaced. The screen is honest about it either way,
because an empty change list says the backend has not been replaced and that is
true; what is missing is the *behaviour*, not the reporting of it.

**A Try again control** for a link that has given up is
[issue #221](https://github.com/wildware-uk/clipped/issues/221). The health
summary says what restarting Clipped does in the meantime.

## What is tested, and what is not

`apps/desktop/src/DiagnosticsScreen.test.tsx` and `redactPath.test.ts`, in
`npm test`. The properties they guard are the ones that would rot in silence:

- **The health summary follows the recorder.** The case drives the whole
  application, opens Diagnostics and then moves the link underneath it with a
  `recorder-link` event, rather than rendering `describeCaptureHealth`'s output
  beside itself — a screen whose wording is a constant looks identical to one
  that is following the link.
- **No rendering claims nothing is being recorded** unless the recorder said so,
  asserted over every state.
- **A failed recording survives the "idle" that follows it**, through the hook,
  the shell and the screen, which is where it would be dropped.
- **The report leaks nothing**, from the worst state the window can be in: six
  separate places a Windows path arrives at once — the sixth being the refusal
  `get_diagnostics` produces when the recorder has gone, whose sentence names the
  executable it was looking for. Each leaked string is asserted separately, so a
  failure names which one got through.
- **The capture backend and the encoder come from the recorder**, asserted by
  driving the whole application and answering `recorder_diagnostics` from a stub,
  rather than by handing the component a value. A case that passed them in as a
  prop would go on passing after the command had been disconnected from the
  screen.
- **A recorder that could not be asked is not drawn as a machine with nothing to
  report.** "Clipped found no encoder here" and "Clipped never asked" are the two
  readings this whole command exists to keep apart (AGENTS.md section 27).
- **The report carries the fields it says it carries and no others**, as an exact
  list. A check that only looked for known leaks could not see a new kind of one.
- **What is copied is what was shown.**
- **Both clipboard failures say so** — no clipboard, and a clipboard that
  refuses — and both name the way out.

The recorder's half is in `cargo test`, against a real recorder over a real pipe:
`apps/recorder/tests/ipc_protocol.rs::a_recorder_reports_what_this_machine_can_encode_without_a_terminal`
for the encoder report, which needs no GPU because a machine with no hardware
encoder is exactly the report it must produce;
`apps/recorder/tests/ipc_protocol.rs::a_recorder_carries_no_path_into_its_diagnostics`
for the fourth acceptance criterion, asserted over the bytes of the frame rather
than a parsed reply so that a path in a field this build does not define is
caught too; and
`apps/recorder/tests/ipc_protocol.rs::a_recording_driven_entirely_over_the_protocol_produces_a_playable_file`
for the capture account, which is `#[ignore]`d because it needs a GPU, an encoder
and a desktop session — a capture backend exists only while something is being
captured, so there is nowhere else the claim can be made.

**Not verified:** that the clipboard works in the real WebView2 window. It needs
a secure context, and establishing that means opening a window on a machine
nobody else is using. The tests drive both answers against a stub; they do not
establish which one WebView2 gives. The report is on screen and selectable
either way, which is why a refusal is a nuisance rather than a dead end.
