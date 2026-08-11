# Recorder command line

`clipped-recorder` is the process that owns recordings. It runs independently of
the desktop application ([ADR 0002](adr/0002-separate-recorder-process.md)), and
it can be driven two ways: from this command line, or over the control protocol
in [ipc.md](ipc.md), which `serve` speaks. The command line is how capture is
tested without a UI and how a recording is made on a machine with no window
open, which is a reason to keep it good rather than a reason to treat it as
scaffolding.

## Status

**`record` records.** It resolves the target through the same rules
`list-windows` prints, captures the window, encodes its frames on the graphics
device they are already on, writes them into a Matroska file as they arrive, and
finishes the file when it is asked to stop
([#126](https://github.com/wildware-uk/clipped/issues/126)). The coordination
lives in `clipped-session`; this command line is a front end over it, and so is
`serve`, so a recording started over IPC is the same recording made by the same
call rather than a second implementation
([ADR 0002](adr/0002-separate-recorder-process.md)).

Two things a recording does **not** have yet, stated here rather than left to be
discovered in a file:

- **No audio track.** `clipped-audio` captures system audio and a microphone and
  is tested doing it ([audio-routing.md](audio-routing.md)), but nothing routes
  it into a session yet ([#180](https://github.com/wildware-uk/clipped/issues/180)),
  so a recording has one video stream and nothing else. `--microphone` and
  `--system-audio` are accepted, and a run that asks for either logs a warning
  saying it cannot be recorded.
- **No scaling.** `--resolution` may only name the size the capture is already
  producing. Frames go from the capture to the encoder without being copied,
  which is what keeps the cost off the game, and there is no scaler in that
  path; a size that would need one is refused with exit code 3 rather than
  silently recorded at the source size.

A recording also ends if the window changes size, because Matroska fixes a
track's dimensions in its header and the encoder is configured for one size. The
file is finished at that point and says so; what a session should do instead is
[#184](https://github.com/wildware-uk/clipped/issues/184).

**`watch` records games automatically.** It is the mode the product exists for
(SPEC.md section 2): a game launching starts a session recording and quitting it
finalises one, with nothing to press
([#46](https://github.com/wildware-uk/clipped/issues/46)). It has the same two
gaps `record` has — no audio track and no scaling — because it makes the same
recording through the same call. [sessions.md](sessions.md) is the subsystem
document: what a session is, what happens on a crash, a fast restart, a second
game or a suspend, and where a session is written down.

Everything else is here: the argument surface, `list-windows`, `capabilities`,
`serve`, and a shutdown path that is now exercised against a real recording
rather than only a fixture.

## Commands

```text
clipped-recorder record --window <TITLE>
clipped-recorder watch [--output-directory <PATH>]
clipped-recorder list-windows [--all] [<selector>]
clipped-recorder capabilities [--refresh]
clipped-recorder serve [--endpoint <NAME>]
clipped-recorder start-at-login <enable|disable|status>
```

Nothing is currently specified without being declared: `record`,
`watch` ([#46](https://github.com/wildware-uk/clipped/issues/46)),
`list-windows` ([#10](https://github.com/wildware-uk/clipped/issues/10)),
`capabilities` ([#14](https://github.com/wildware-uk/clipped/issues/14)),
`serve` ([#49](https://github.com/wildware-uk/clipped/issues/49)) and
`start-at-login` ([#106](https://github.com/wildware-uk/clipped/issues/106)) are
all implemented below (AGENTS.md section 27).

Adding one is a variant on `Command` in `apps/recorder/src/cli.rs` and an arm in
`clipped_recorder::run`. Nothing else has to move.

## `record`

The capture target is the only required argument; everything else has a default,
so this is a complete invocation:

```text
clipped-recorder record --window "Counter-Strike 2"
```

| Option | Default | Notes |
| --- | --- | --- |
| `--window <TITLE>` | — | Substring match on the window title |
| `--process <NAME>` | — | Executable name, such as `cs2.exe` |
| `--pid <PID>` | — | Process identifier |
| `-o, --output <PATH>` | `%USERPROFILE%\Videos\Clipped\clipped-<date>-<time>.mkv` | Must end in `.mkv` |
| `--overwrite` | off | Required to replace an existing file |
| `-r, --resolution <WIDTHxHEIGHT>` | `source` | Or `1920x1080`; both sides even, 128–7680 |
| `-f, --framerate <FPS>` | `60` | 1–480 |
| `--codec <CODEC>` | `auto` | `auto`, `h264`, `hevc`, `av1` |
| `--encoder <ENCODER>` | `auto` | `auto`, `nvenc`, `amf`, `quicksync`, `software` |
| `--microphone <DEVICE>` | `default` | `default`, `none`, or part of a device name |
| `--system-audio <DEVICE>` | `default` | Same values; per-application tracks are M2 |

Exactly one of `--window`, `--process` and `--pid` may be given. `--help` is the
authority on all of this; the table above is here so the shape can be read
without a build.

The target is resolved through the same `clipped-windows` rules `list-windows`
runs on, so `list-windows --window <TITLE>` shows exactly what
`record --window <TITLE>` will point at — including the candidates of an
ambiguous title, which is a usage error rather than a guess.

`--framerate` is a **ceiling**, not a pace. The compositor produces a frame when
the window's content changes, so a source slower than the requested rate records
at the source's rate, with the real intervals between frames in the timestamps;
a source faster than it has frames skipped before they are encoded, and the
count is reported when the recording ends. Nothing is ever duplicated to pad a
recording out to a nominal rate.

Because it is a ceiling, **the file does not declare it**. The track carries no
nominal frame rate at all, so a player or an editor reads the rate off the
timestamps — which is where the recording's own account of when its frames
happened lives. Declaring the ceiling instead would label a real 30 fps
recording as 60 fps for having been made with the default `--framerate`.

What the ceiling *is* still used for is configuring the encoder, and that has
two consequences worth knowing when a source is much slower than the ceiling:
the bitrate is budgeted for the ceiling's worth of frames, and the keyframe
interval is two seconds converted into a number of frames at the ceiling — so a
30 fps source recorded at `--framerate 60` gets keyframes four seconds apart.
Deriving those from the rate the source is actually producing is
[#191](https://github.com/wildware-uk/clipped/issues/191); until then, passing
`--framerate` close to the source's rate is what a recording of a slow source
wants.

`--codec auto` and `--encoder auto` pick from what
[`capabilities`](#capabilities) measured, most efficient first, and fall back to
the next candidate when one refuses to open a session on the device the frames
are captured on. An encoder named explicitly is never fallen back from: somebody
who typed `--encoder nvenc` wants to know it was not used.

### What a finished recording prints

```text
Recorded 233 frames of 1280x720 AV1 in 7.76s to D:\clips\session.mkv (NVIDIA NVENC, Windows Graphics Capture, 29.9 fps sustained; 0 frames dropped). Stopped by request.
```

On standard error, with the same figures in the log. "Frames dropped" is the one
number that means something went wrong: it counts frames that were not encoded
because the thread writing the file had not caught up. Frames skipped to hold
`--framerate` are counted separately and are not in it, because they are the
recorder doing what it was asked.

`--microphone` and `--system-audio` treat `default` and `none` as reserved
words. Prefix with `name:` to select a device that is really called one of them:
`--microphone name:none`.

### Defaults that touch the disk

The default output directory — `Clipped` inside the user's videos folder — will
be created if it does not exist, because it is Clipped's own. It is created by
the recording that goes in it, not by validating the command: until there is
something to write, a run leaves the videos folder exactly as it found it.

A directory named with `--output` must already exist, and is never created: a
path that is not there is far more often a typo than an instruction to build a
tree, and creating one silently is how recordings end up somewhere nobody looks.

An existing output file is never replaced without `--overwrite`.

Validation does write one thing, briefly: a uniquely named zero-byte probe file
in the output directory, created and immediately deleted, because Windows has no
permission bit that can be read and believed. Failing here beats failing twenty
minutes into a session.

## `watch`

Records games as they start and stops when they exit, without being asked.

```text
clipped-recorder watch
```

That is a complete invocation. Recordings and session records go to
`%USERPROFILE%\Videos\Clipped`; the directory is created at start-up rather
than when a game launches, so a drive that is not connected is reported before
it costs somebody a session.

| Option | Default | Meaning |
| --- | --- | --- |
| `--output-directory <PATH>` | the Clipped folder of your videos directory | Where recordings and session records go |
| `--window-timeout <SECONDS>` | 120 | How long a game may take to put a window on screen |
| `--resolution`, `--framerate`, `--codec`, `--encoder` | as `record` | Applied to every automatic recording |
| `--microphone`, `--system-audio` | `default` | As `record`, and with the same warning: there is no audio track yet |

What it prints, on standard error:

```text
Watching for games. Recordings go to D:\clips. Press Ctrl+C to stop.
Counter-Strike 2 started. Looking for its window.
Recording Counter-Strike 2 to D:\clips\clipped-counter-strike-2-20260811-143205.mkv.
Session counter-strike-2-20260811-143205 of Counter-Strike 2: 1 recording totalling 1084s.
  D:\clips\clipped-counter-strike-2-20260811-143205.mkv
Automatic recording stopped.
```

Those first two are two different facts and are printed at two different
moments. A launch is noticed before there is anything to capture — a game can
spend a minute compiling shaders before it draws — and the search for a window
can also fail. Announcing a recording at the moment the game was noticed would
be claiming a recording that may never happen. When the search does give up, or
a recording fails, that is printed too, rather than being left as an absence in
the summary at the end:

```text
Nothing was recorded of Counter-Strike 2: no window to record appeared within 120s: …
```

Standard output is left empty, as it is for `record`: what this command produces
is files.

`--window-timeout` is not a timeout on something going wrong. A launch is
reported a few seconds after the process starts and a game can take much longer
than that to reach a window while it compiles shaders, so this is how long a
game is allowed to take to appear. A game that never shows one is reported in
the session's record and not tried again.

There is deliberately **no capture-mode option**. Full Session is the only mode
this build can run, and an option offering four values three of which would do
nothing is a control that silently does nothing (AGENTS.md section 27).
[sessions.md](sessions.md) names the issues that build the other three, and the
rest of what `watch` decides: a tie in the game catalogue, a crash, a fast
restart, a second game launching mid-session, and a suspend.

Ctrl+C stops watching. Any recording is finished first, exactly as it is for
`record`, and the session is written out and reported before the process exits.

The desktop application cannot drive this yet, and cannot see a session even
when the recorder is running one: the control protocol describes a recording by
its capture target and has no vocabulary for a game or a session. That is
[#241](https://github.com/wildware-uk/clipped/issues/241).

## `list-windows`

Lists what the recorder can be pointed at. This is the answer to "why can't it
find my game?", and it is how the target selection rules are inspected without
starting a recording.

```text
clipped-recorder list-windows
```

The listings in this section come from a real run on a two-monitor desktop, with
rows removed and window titles shortened so that they fit the page and do not
publish anybody's open tabs. Where a count describes the rows that follow it, it
has been reduced with them, so that no example contradicts itself. The shape,
the columns and the wording are exactly what the command prints.

```text
5 of 421 top-level windows can be captured.

HANDLE      PID    PROCESS              CLIENT     DPI  MONITOR       TITLE
0x00010698  24860  steamwebhelper.exe   2560x1392  96   \\.\DISPLAY2  Steam
0x000201f2  11428  WindowsTerminal.exe  2560x1392  96   \\.\DISPLAY1  clipped
0x000403ae  10228  chrome.exe           2560x1392  96   \\.\DISPLAY2  Issues - Google Chrome
0x000a01ce  8560   explorer.exe         2560x1400  96   \\.\DISPLAY1  Videos - File Explorer
0x00010a0a  36220  Spotify.exe          minimised  96   \\.\DISPLAY2  Spotify Free

416 more windows cannot be captured. Pass --all to list them with the reason.
```

| Option | Default | Notes |
| --- | --- | --- |
| `--all` | off | Also list the windows that cannot be captured, and why |
| `--window <TITLE>` | — | Resolve, rather than list: substring match on the title |
| `--process <NAME>` | — | Resolve by executable name; the `.exe` is optional |
| `--pid <PID>` | — | Resolve by process identifier |
| `--handle <HANDLE>` | — | Resolve one exact window, as printed in `HANDLE` |

At most one selector may be given, and an empty `--window` or `--process` is
rejected as a usage error rather than matched: an empty substring is inside every
title, so it names the whole desktop, and nobody means that. With no selector the
command lists; with one, it resolves and prints everything known about the single
window that answers to it:

```text
> clipped-recorder list-windows --window "File Explorer"

the window title containing `File Explorer` resolves to one window:

  Handle    0x000a01ce
  Title     Videos - File Explorer
  Process   explorer.exe (pid 8560)
  Client    2560x1400 pixels at 96 DPI (100% scale)
  Monitor   \\.\DISPLAY1 2560x1440 at (2560, 0)
```

### What "capturable" means

Every top-level window Windows enumerates is examined, and the ones that cannot
be recorded are listed by `--all` with the first reason that applies:

| Reason | Why it is not a capture target |
| --- | --- |
| `shell` | The desktop or `Progman`; capturing it yields the wallpaper. Recording the screen is a monitor capture |
| `invisible` | `IsWindowVisible` is false: hidden helper and message windows, which are most of a desktop |
| `cloaked` | Composited away by the DWM: suspended Store apps, and everything on another virtual desktop |
| `tool-window` | `WS_EX_TOOLWINDOW`: palettes, tray helpers, tooltips — the windows Alt-Tab also hides |
| `untitled` | No title, so there is no way to name it and no way to tell it from the others |
| `zero-sized` | The client area has no pixels |
| `content-protected` | `SetWindowDisplayAffinity`: the owner excluded it from capture, and it would record black |

A **minimised** window is not excluded. It is a legitimate target — it is about
to be restored — but its size is not final and Windows Graphics Capture produces
no frames until it is, so the listing shows `minimised` in place of a size and
the resolution output says so.

### Ambiguity is reported, never guessed

A selector matching more than one capturable window is an error, not an
invitation to pick one: recording the wrong window for twenty minutes is worse
than being asked again.

```text
> clipped-recorder list-windows --window "e"

error: 5 windows match the window title containing `e`, and choosing between them would be a guess:
  0x000201f2  WindowsTerminal.exe (pid 11428)  clipped
  0x00010698  steamwebhelper.exe (pid 24860)  Steam
  0x000403ae  chrome.exe (pid 10228)  Issues - Google Chrome
  0x000a01ce  explorer.exe (pid 8560)  Videos - File Explorer
  0x00010a0a  Spotify.exe (pid 36220)  Spotify Free
```

The exception, and the only narrowing rule there is: a title that matches one
window *exactly* wins over the windows it merely appears inside, so `--window
Discord` finds "Discord" on a machine that also has "Discord Updater" open. Two
windows with the same exact title stay ambiguous. A process owning several
windows is ambiguous too — Windows has no concept of a process's "main" window,
and every rule for inventing one is wrong for some application. `--handle` is
the way out of any of these, which is why the handle is the first column.

At most ten candidates are listed, with a count of the rest: `explorer.exe`
alone owns sixty top-level windows, and an error nobody reads is not an error
message.

Long lists are unremarkable — the desktop these examples came from had 424
top-level windows and 8 that could be captured — which is why the exclusion
reasons exist rather than a filter that quietly drops them.

## `capabilities`

```text
clipped-recorder capabilities
```

Prints the display adapters in the machine, the encoders on them, the codecs
each encoder supports, and the order "Automatic" would try them in. It writes
the report to standard output and the same findings to the log, through the
standard `encoder` field ([logging.md](logging.md)).

An encoder the machine has that this build cannot open is still reported, but
under a heading of its own — "Detected on this machine, and not available to
choose" — rather than as a numbered entry in the ranking, so that nothing listed
as something "Automatic would choose" is a thing it would not
([#175](https://github.com/wildware-uk/clipped/issues/175)).

| Option | Default | Notes |
| --- | --- | --- |
| `--refresh` | off | Ignore the cached report, ask the machine again — including the encoders themselves — and store the new answer |

The report distinguishes what was **measured on this machine** from what was
**inferred** from published limits, and marks every inferred value `(i)`. That
distinction is the point of the command rather than a detail of it. A limit
shown as `—` is one the report declines to state, because the encoder will not
produce that codec and a limit beside that word reads as a promise.

An encoder counts as **available** when its vendor runtime loads, because that
is the library Clipped will encode through. A driver that registers media
transforms without installing its encode runtime is reported as unavailable,
with the transforms listed underneath as the evidence.
[encoder-capabilities.md](encoder-capabilities.md) explains how each answer is
arrived at, what the cache does and when it is thrown away.

**Without `--refresh`, detection opens no encoder session**, so running this
while a game is recording cannot take a session slot from it. The numeric limits
are then the published ones, marked `(i)`.

`--refresh` is the exception, and it is deliberate: it opens one session per
*available* hardware encoder — a few hundred milliseconds in total, and nothing
at all on an adapter whose encoder runtime will not load — and asks each for its
own maximum resolution, throughput, B-frame and 10-bit support, which is the
only way those stop being inferred. The answers are cached, so the next plain
run shows them for nothing, and no later plain run takes them away again: a run
that opens no session never overwrites a stored measurement of the same machine
([encoder-capabilities.md](encoder-capabilities.md)). Do not run it mid-match;
that session slot may belong to the game. The report itself names the command while an encoder has
never been asked, and stops mentioning it once every encoder has — some limits
stay `(i)` for ever, and being told to measure them again would waste a session
slot.

**A supported codec is not a recording.** The footer separates the two. NVENC
([#15](https://github.com/wildware-uk/clipped/issues/15)), AMF
([#16](https://github.com/wildware-uk/clipped/issues/16)) and the software
fallback on the CPU ([#18](https://github.com/wildware-uk/clipped/issues/18))
have a backend proven to encode with in this build, each measured doing so on
real hardware. Quick Sync ([#17](https://github.com/wildware-uk/clipped/issues/17))
is different from the two that used to sit beside it in this sentence: it has a
real backend too, `QuickSyncEncoder`, written to the same interface — but there
is no Intel GPU on the machine it was written on, so nothing has ever seen it
encode a frame, and the footer does not count it until it has
([#160](https://github.com/wildware-uk/clipped/issues/160)). A machine whose
best encoder is Quick Sync encodes on the CPU today. Whichever is chosen, a
recording made with it has a video track and no audio track.

Which encoders count comes from `EncoderKind::is_implemented` rather than from
a sentence in the report, because the sentence that used to be there went on
claiming no encoder was implemented through two of them landing
([#167](https://github.com/wildware-uk/clipped/issues/167)). That function's
own doc comment is where "counts" is defined precisely — proven on real
hardware, not merely compiling — and why Quick Sync fails it today.

## `serve`

`record` makes one recording and exits. `serve` is the shape the recorder
actually runs in beside a user interface: it listens on a named pipe and takes
its instructions over the control protocol, for as long as it is left running.

```text
clipped-recorder serve [--endpoint <NAME>]
```

| Option | Default | Notes |
| --- | --- | --- |
| `--endpoint <NAME>` | `clipped-recorder.<session>` | A name, never a path |

[ipc.md](ipc.md) is the protocol itself — the framing, the handshake, the
compatibility policy, every command and event, and what the transport does and
does not promise about who can reach it. What belongs here is only the command
line around it.

**It prints one line to standard output and then serves:**

```text
ready endpoint=\\.\pipe\clipped-recorder.1
```

That line is the hook for whatever started the recorder, and it is the only
thing this subcommand writes to standard output; everything else is a diagnostic
and goes to standard error, as it does for every other subcommand.

The supervisor in the desktop application does **not** read it. It starts the
recorder detached, with standard output pointed at nothing, and decides the
recorder is up by connecting to the endpoint — because a recorder holding a pipe
of the window's would fail its next write when the window closed, which is the
one thing the arrangement exists to prevent
([ADR 0006](adr/0006-recorder-lifetime-and-supervision.md)). The tests in
`apps/recorder/tests/ipc_protocol.rs` do read it, which is what it is for.

`<session>` in the default name is the Windows sign-in session the process is
running in. The pipe namespace is machine-wide, so without it two people signed
in at once — one at the keyboard and one over Remote Desktop — would be racing
for a single name. `--endpoint` is for running a second recorder beside the one
somebody is using: a development build, or a test. A name may contain ASCII
letters, digits, `-`, `_` and `.`, and the `\\.\pipe\` prefix is added for you,
so an endpoint can never be pointed at another machine.

**One recorder owns an endpoint.** A second `serve` on a name already taken
fails immediately, saying another recorder is already listening, rather than
half-serving it. That *is* the recorder's single-instance story: the supervisor
builds on it rather than adding a second mechanism, and two applications
starting at the same instant produce one serving recorder and one that exits
([ADR 0006](adr/0006-recorder-lifetime-and-supervision.md)).

**Ctrl+C stops it the way it stops `record`.** The listener stops first, so
nothing new arrives, and then any recording is stopped and its file finished
before the process exits — the recording is the only thing here that has to end
correctly. Connection threads own nothing and go with the process.

**A client can ask it to exit**, which is how a recorder started detached by the
desktop application is stopped: it has no console, so Ctrl+C cannot reach it, and
before the `shutdown` command it could only be killed
([#220](https://github.com/wildware-uk/clipped/issues/220)). The command runs the
path above rather than a second one — it stops the listener, and everything after
that is the same code Ctrl+C reaches. It **refuses** while a recording is running
unless the request says in as many words that it may finish one, so nothing that
can open the pipe can end somebody's recording by accident;
[ipc.md](ipc.md#shutdown) has the shape of it.

```powershell
# see docs/ipc.md, "Trying it by hand", for Send-Frame and Read-Frame
Send-Frame $pipe @{ type = 'request'; id = 1; command = 'shutdown' }
Read-Frame $pipe
# {"type":"response","id":1,"outcome":{"ok":{"reply":"shutting_down"}}}
```

Exit codes are the ordinary ones: 0 when it was stopped, 1 if the endpoint could
not be taken or serving failed. A recording that fails while it is being served
does not stop the recorder; it is reported to whoever is connected, on the
`errors` stream.

## `start-at-login`

Asks Windows to run `clipped-recorder serve` when this user signs in. It is the
mechanism behind "the recorder starts at login" in SPEC.md section 5, and it is
**opt-in and reversible**: nothing in Clipped writes this value except `enable`,
and `disable` removes it.

```text
clipped-recorder start-at-login enable
clipped-recorder start-at-login disable
clipped-recorder start-at-login status
```

It writes exactly one value, under this account only:

```text
HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run
    "Clipped Recorder" = "C:\…\clipped-recorder.exe" serve
```

`HKEY_CURRENT_USER` rather than `HKEY_LOCAL_MACHINE`, so it needs no elevation
and applies to one person on a shared machine — and so the recorder runs in the
sign-in session whose windows it has to capture. A `Run` value rather than a
Startup-folder shortcut because Windows lists a `Run` value in **Settings > Apps
> Startup** and in Task Manager's Startup tab, each with a switch, so it can be
turned off without finding this subcommand.
[ADR 0006](adr/0006-recorder-lifetime-and-supervision.md) has the alternatives.

`status` prints the command that is configured, and says so plainly when the
executable it names no longer exists — which is what a moved or reinstalled
Clipped leaves behind. It reports that rather than repairing it: silently
rewriting somebody's startup entry because a status command was run is exactly
the surprising behaviour this subcommand avoids. Run `enable` from the
installation you want.

**A recorder started this way cannot currently be stopped except by killing it**
([#220](https://github.com/wildware-uk/clipped/issues/220)). That is worth
knowing before turning it on.

There is no setting for this in the desktop application yet; the settings screen
is [#108](https://github.com/wildware-uk/clipped/issues/108).

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | The command succeeded |
| 1 | The command failed while running |
| 2 | The arguments were rejected — the same code clap uses for a usage error |
| 3 | The recording asked for something this build cannot produce |

3 is separate from 1 so that a script, and the test suite, can tell "this does
not exist yet" from "this went wrong". `record` exits 3 for a `--resolution`
that would need a scaler, and for a capture in a high dynamic range pixel format
no encoder here accepts ([#99](https://github.com/wildware-uk/clipped/issues/99)).
A driver that failed mid-recording is a 1, and the file it leaves behind is
still finished and playable.

Both `record` and `list-windows` exit 2 when a selector matched no window, or
more than one: the command line is what has to change, and the message already
lists the windows that could have been meant. They exit 1 when Windows refused
to describe the desktop at all.

`capabilities` exits 0 even on a machine with no hardware encoder — that is a
report, not a failure — and 1 only when the adapters could not be enumerated at
all.

`watch` exits 0 when Ctrl+C stopped it, and 1 when the output directory cannot
be written to, the game catalogue cannot be read, or process detection stopped
while it was watching — the last of which means no further game would have been
noticed, so continuing would be a recorder that quietly records nothing.

`start-at-login` exits 0 for all three actions, including `disable` when nothing
was configured: that is the state being asked for, and treating it as a failure
would make "turn it off" fail for everybody who never turned it on. It exits 1
when the registry refused, and 2 for an action that is not one of the three.

## Diagnostics

The recorder uses `clipped-logging`, so log files are under
`%LOCALAPPDATA%\Clipped\logs` and the same records go to standard error. See
[logging.md](logging.md) for the level file and for the standard context fields.

```text
CLIPPED_LOG=debug clipped-recorder record --window "Counter-Strike 2"
```

Standard output is left for a command's result — `list-windows` prints its
listing there and `capabilities` writes its report there — so errors and
progress go to standard error. A `list-windows` run piped into another program
therefore carries the table and nothing else.

The resolved configuration is logged once, at `info`, before anything uses it.
The output path is redacted to its file name and a digest of the whole path
(`RedactedPath`), because a recording path normally contains the account name.

## Stopping a recording

Ctrl+C asks the recorder to stop; it does not kill it. The recording loop polls
the signal between frames, so it stops at a frame boundary and never mid-write.
Then, in order:

1. the encoder is told the stream has ended and drained of the pictures it was
   holding back — that last fraction of a second is the part somebody who
   pressed Ctrl+C was watching;
2. the queue into the muxing thread is closed, everything in it is written, and
   the container's trailer is written, which is where Matroska's segment length,
   duration and cue index go;
3. the recorder's finalisation hook runs, and says why the run ended.

The seam is `clipped_recorder::shutdown::run_until_shutdown`, and the hook it
runs is guaranteed to run exactly once whether the body ended by itself, was
interrupted, returned an error or panicked. The panic case is deliberate, and it
is real rather than nominal: the muxing thread finalises from its own destructor
as well, so a bug in the pipeline still leaves a file that plays.

A recorder started in a process group of its own — which is how a launcher
isolates a long-running child — inherits Ctrl+C *disabled*, so the recorder asks
for it back at startup (`shutdown::allow_ctrl_c`). Without that it could only be
killed, which is the failure this whole path exists to prevent.

### How that is verified

Two tests, both against real processes and neither simulating the signal.

`apps/recorder/tests/ctrl_c.rs` sends a genuine `CTRL_C_EVENT` to the fixture in
`apps/recorder/examples/shutdown_fixture.rs` and asserts the hook ran with the
right reason. It needs no GPU, so it runs in CI on every change.

`apps/recorder/tests/record_end_to_end.rs` does it to a real recording:
`test-apps/video-pattern` on screen, `clipped-recorder record` capturing it,
a real `CTRL_C_EVENT` four seconds in, and the file it leaves read back with the
pinned FFmpeg build through `clipped-media-validation`. It asserts the container
opens, holds one video stream of the codec and size the recorder said it wrote,
has a plausible duration and monotonic timestamps, declares the frame rate it
sustained rather than the `--framerate` ceiling it was recorded under, and that
**every frame the recorder reported encoding decodes out of the file**. It also
asserts the three finalisation lines above appear in order — each one searched
for in what is left after the one before it, so a trailer written before the
flush fails exactly as a missing line does — which is how the hook is shown to
have run rather than the file merely happening to be readable. That test needs a
GPU, an encoder and a desktop session, so it is `#[ignore]`d:

```text
cargo test -p clipped-recorder --test record_end_to_end -- --ignored --nocapture --test-threads=1
```

What is **not** asserted is that the decoded pictures are the frames the source
drew, in order. The video pattern carries a decodable counter for exactly that,
but its decoder lives in `clipped-video-pattern`, which the workspace layering
places above `clipped-recorder` so that nothing in the product can depend on a
test application — dev-dependencies included. A test that reads those counters
back out of a recording belongs beside `tests/capture/wgc_video_pattern.rs`,
which already reads them out of captured frames, and is
[#183](https://github.com/wildware-uk/clipped/issues/183).

## Testing the command line

```text
cargo test -p clipped-recorder
```

Unit tests cover parsing and validation, including the wording of the error
messages: an error message is behaviour someone depends on, and changing one
should be a decision rather than a side effect. `tests/command_line.rs` runs the
built binary and asserts what it prints and what it exits with, including that a
`record` invocation which never gets as far as a frame creates neither an output
file nor, when it is left to work out the default path, a recordings directory.

The recording itself is `tests/record_end_to_end.rs`, described under
[stopping a recording](#how-that-is-verified). Nothing in `tests/command_line.rs`
starts a capture, which is why that file still runs on a machine with no GPU.

`serve` has a test file of its own, `tests/ipc_protocol.rs`, which starts the
built binary, talks to it over the real named pipe and stops it with a real
Ctrl+C. It needs no GPU either — a pipe and a child process are all a recorder
needs to answer `ping` — so it runs in CI. [ipc.md](ipc.md) lists what it
covers.

`tests/supervision.rs` is the other side of the same subcommand: a real
supervisor process (`examples/supervised_ui_fixture.rs`) starting a real
detached recorder, and both of them ended with a real `TerminateProcess`. Six of
its tests need no GPU and run in CI — a recorder outliving the process that
started it, a second launch attaching rather than competing, a second launch
holding the instance name starting nothing at all, a killed recorder reported
and replaced, and a restart policy that gives up — and two are `#[ignore]`d
because they record a window and read the file back.

`start-at-login`'s registry calls are exercised for real, against a scratch key
of this account's rather than the `Run` key: a test that wrote there would
arrange for the machine running it to start a recorder at every sign-in
afterwards. The scratch key is removed when the test finishes.

Two of those tests read the command definition rather than a copy of it: they
walk `record`'s arguments and require every one of them but the capture target
to document a default, in `-h` as well as `--help`. Adding an option without a
default fails them, which is the acceptance criterion rather than today's
rendering of it.

`cargo test --test ctrl_c` on its own is not enough: cargo builds examples for
`cargo test` but not for a single named test target, so the Ctrl+C test would
run against a stale fixture. Run `cargo test -p clipped-recorder`.

What `list-windows` prints depends on the machine, so the tests here assert only
what does not: that it exits 0, that the columns are there, and that a selector
matching nothing is a usage error rather than a panic. The behaviour that can be
pinned down is tested where it lives — the selection rules against constructed
desktops in `crates/windows/src/selection.rs`, and the enumeration against
windows the test creates and destroys itself in
`crates/windows/tests/desktop.rs`:

```text
cargo test -p clipped-windows
```
