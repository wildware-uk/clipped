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

**A recording has sound in it.** `--system-audio` and `--microphone` each open
an endpoint through `clipped-audio` and give it a named audio track of its own,
placed on the recording's timeline by `docs/av-sync.md`'s model
([#180](https://github.com/wildware-uk/clipped/issues/180)). Both default to
`default`, so an invocation that says nothing about audio records the output
device and the microphone; `none` on either turns that source off completely —
no device is opened and no track is declared. What a recording does not have yet
is *routing*: the game's own process tree
([#26](https://github.com/wildware-uk/clipped/issues/26)), per-application
tracks ([#27](https://github.com/wildware-uk/clipped/issues/27)) and the
compatibility mix ([#29](https://github.com/wildware-uk/clipped/issues/29)) are
M2, so today the system track is the whole output endpoint.

One thing a recording still does **not** have, stated here rather than left to
be discovered in a file:

- **No scaling.** `--resolution` may only name the size the capture is already
  producing. Frames go from the capture to the encoder without being copied,
  which is what keeps the cost off the game, and there is no scaler in that
  path; a size that would need one is refused with exit code 3 rather than
  silently recorded at the source size.

A recording also ends if the window changes size, because Matroska fixes a
track's dimensions in its header and the encoder is configured for one size. The
file is finished at that point and says so; what a session should do instead is
[#184](https://github.com/wildware-uk/clipped/issues/184).

**`replay` keeps the last few minutes and saves them on a key.** It is `record`
with a rolling buffer beside it: the recording runs as usual, every encoded
packet is copied into `clipped-replay`'s buffer as well, and `Ctrl`+`F10` turns
the last N seconds of that buffer into a clip of the thing that just happened
([#38](https://github.com/wildware-uk/clipped/issues/38), SPEC.md sections 15
and 16). The clip is written beside the recording, named after the session, and
entered in the session's own record — so it reaches the library exactly as the
recording does ([sessions.md](sessions.md), [replay-buffer.md](replay-buffer.md)).

**`watch` records games automatically.** It is the mode the product exists for
(SPEC.md section 2): a game launching starts a session recording and quitting it
finalises one, with nothing to press
([#46](https://github.com/wildware-uk/clipped/issues/46)). It records the same
audio tracks and has the same gap `record` has — no scaling — because it makes
the same recording through the same call. [sessions.md](sessions.md) is the
subsystem
document: what a session is, what happens on a crash, a fast restart, a second
game or a suspend, and where a session is written down.

Everything else is here: the argument surface, `list-windows`, `capabilities`,
`serve`, and a shutdown path that is now exercised against a real recording
rather than only a fixture.

## Commands

```text
clipped-recorder record --window <TITLE>
clipped-recorder replay --window <TITLE> [--duration <SECONDS>]
clipped-recorder watch [--output-directory <PATH>]
clipped-recorder list-windows [--all] [<selector>]
clipped-recorder capabilities [--refresh]
clipped-recorder serve [--endpoint <NAME>] [--watch-for-games]
clipped-recorder start-at-login <enable|disable|status>
clipped-recorder plugins <list|enable <ID>|disable <ID>>
clipped-recorder recover [--directory <PATH>] [--session <ID>] [--adopt | --discard]
```

Nothing is currently specified without being declared: `record`,
`replay` ([#38](https://github.com/wildware-uk/clipped/issues/38)),
`watch` ([#46](https://github.com/wildware-uk/clipped/issues/46)),
`list-windows` ([#10](https://github.com/wildware-uk/clipped/issues/10)),
`capabilities` ([#14](https://github.com/wildware-uk/clipped/issues/14)),
`serve` ([#49](https://github.com/wildware-uk/clipped/issues/49)),
`recover` ([#103](https://github.com/wildware-uk/clipped/issues/103),
[#451](https://github.com/wildware-uk/clipped/issues/451)),
`start-at-login` ([#106](https://github.com/wildware-uk/clipped/issues/106)) and
`plugins` ([#492](https://github.com/wildware-uk/clipped/issues/492)) are
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
| `--system-audio <DEVICE>` | `default` | `default` or `none`; naming a device is refused ([#316](https://github.com/wildware-uk/clipped/issues/316)) |

Exactly one of `--window`, `--process` and `--pid` may be given. `--help` is the
authority on all of this; the table above is here so the shape can be read
without a build.

The target is resolved through the same `clipped-windows` rules `list-windows`
runs on, so `list-windows --window <TITLE>` shows exactly what
`record --window <TITLE>` will point at — including the candidates of an
ambiguous title, which is a usage error rather than a guess.

**A minimised window is refused, before anything is created.** Windows draws a
minimised window for nobody, so a recording of one is a container header and no
picture; the refusal names the window and what to do about it, and no file is
made ([#383](https://github.com/wildware-uk/clipped/issues/383)):

```text
Counter-Strike 2 (cs2.exe) is minimised, so there would be nothing to record:
Windows hands over no frames for a window it is not drawing. Restore it and
start again
```

Minimising it *during* a recording is different, and does not end one — see
"What a finished recording prints" below.

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

### The audio tracks

`--system-audio default` records the endpoint Windows is playing through, in
loopback, and `--microphone default` records the default input device;
`--microphone <NAME>` records the one microphone whose name contains that text,
and refuses rather than guessing when nothing matches or several do. Each source
gets a track of its own, named as an editor will show it, and the two are never
mixed together (AGENTS.md section 21).

The tracks are ordered by `clipped-muxer`'s track model rather than by the order
the devices happened to open, so `track 2 is the microphone` keeps being true of
every recording made the same way. The first of them carries Matroska's default
flag, because some players take one audio track from a multi-track file and the
one they take should be the one that sounds right (SPEC.md section 13).

Audio is stored as uncompressed 16-bit PCM, which is what `clipped-muxer`
records in and why (`docs/muxing.md`): nothing in Clipped encodes audio yet, the
bitrate is small beside the video beside it, and an archival recording that has
never been through a lossy encoder is the one an editor should be given.

`--system-audio` will not take a device name. WASAPI loopback is opened against
the endpoint Windows is *playing through*, and recording a different one is
[#316](https://github.com/wildware-uk/clipped/issues/316); a name is refused
with exit code 3 rather than the default endpoint being recorded quietly in its
place.

`--microphone` and `--system-audio` treat `default` and `none` as reserved
words. Prefix with `name:` to select a device that is really called one of them:
`--microphone name:none`.

`--microphone none --system-audio none` makes a video-only recording: no device
is opened, no track is declared, and nothing is said about audio.

### What a finished recording prints

```text
Recorded 233 frames of 1280x720 AV1 in 7.76s to D:\clips\session.mkv (NVIDIA NVENC, Windows Graphics Capture, 29.9 fps sustained; 0 frames dropped). Stopped by request.
  Other System Audio: 7.77s at 48000 Hz, 2 channels from Speakers (Realtek(R) Audio)
  Microphone: 7.77s at 48000 Hz, 1 channel from Microphone (Yeti Stereo Microphone)
```

On standard error, with the same figures in the log. "Frames dropped" is the one
number that means something went wrong: it counts frames that were not encoded
because the thread writing the file had not caught up. Frames skipped to hold
`--framerate` are counted separately and are not in it, because they are the
recorder doing what it was asked.

A window minimised part way through adds a sentence, because nothing else in
the line would show it: the frame counts are simply lower and the duration still
covers the silence, so a session that spent most of itself on the taskbar reads
as a recording of a very still game.

```text
Recorded 421 frames of 2560x1440 AV1 in 96.30s to D:\clips\session.mkv (NVIDIA NVENC, Windows Graphics Capture, 4.4 fps sustained; 0 frames dropped). Stopped by request. The window was minimised once during the recording, and nothing was recorded while it was.
```

The recording is deliberately **kept open** while the window is minimised.
Alt-tabbing out of an exclusive fullscreen game minimises it, and stopping there
would cost the rest of the session for two keystrokes; everything before the
minimise and everything after the restore is in the one file, on one timeline.

**A recording that captured nothing at all leaves no file.** If capture never
produced a frame the file would be a header with no picture in it — which the
media library would index and draw as a tile that cannot be played — so it is
removed and the run reports why:

```text
Nothing was recorded
Nothing had been recorded, so no file was left.
  - Restore the window if it is minimised
  - Make sure it is on a desktop that is showing
```

One line per audio track follows, and each says what it came to. A track whose
device produced nothing says so — a microphone muted in Windows still delivers
packets, of silence, so a recording of one looks perfectly healthy and contains
nothing, and that is the commonest reason a track is silent. The log carries
more: how much of the track was silence synthesised to cover periods the device
said nothing for, and how far the track moved against the recording's reference
clock over the session (`sync_drift_ppb`, `sync_state`), measured rather than
assumed (`docs/av-sync.md`).

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

## `replay`

Records, and keeps the last few minutes to save from.

```text
clipped-recorder replay --window "Counter-Strike 2"
```

That is a complete invocation. It records exactly as `record` does — the same
target rules, the same options, the same file in the same place — and adds two
things:

- **A rolling buffer** of the last `--duration` seconds of encoded video, held
  in memory. There is one encoder and two consumers of its packets, so the
  buffer costs one `memcpy` per packet and the memory its window needs, not a
  second encode ([replay-buffer.md](replay-buffer.md)).
- **A hotkey.** `Ctrl`+`F10` writes the last `--save-duration` seconds out as a
  clip, beside the recording and named after the session
  (`clipped-<session>-replay-1.mkv`, then `-2`, and so on). A combination
  another application already owns is reported when the command starts rather
  than discovered when a press does nothing ([hotkeys.md](hotkeys.md)).

| Option | Default | Meaning |
| --- | --- | --- |
| `--duration <SECONDS>` | the configured replay window, 300 unless changed | How much video the buffer keeps, from 30 to 1800 |
| `--save-duration <SECONDS>` | the whole of `--duration` | How much one save keeps, from 1 second up |

Every `record` option — `--window`, `--process`, `--pid`, `--output`,
`--resolution`, `--framerate`, `--codec`, `--encoder`, `--microphone`,
`--system-audio` — means exactly what it means there, because they are the same
arguments validated by the same code.

**It writes the ordinary recording as well as the clips.** SPEC.md section 4's
Manual/Replay capture mode keeps the buffer and writes no continuous file; this
build has no recording without one, so `replay` costs the disk what `record`
costs it. Buffer-only capture is
[#423](https://github.com/wildware-uk/clipped/issues/423).

What comes out is not exactly what was asked for, and the command says so on
every save:

```text
Keeping the last 5 minutes. Press Ctrl+F10 to save 5 minutes; Ctrl+C to stop.
Replay saved: D:\clips\clipped-unattributed-20260813-201400-replay-1.mkv (5 minutes 2 seconds)
```

`Ctrl`+`F10` there is the shipped default. The combination named is the one
`replay` actually registered, which is whatever the `hotkeys` section of
`settings.json` says (`docs/configuration.md`) — so on a machine where another
application already owns the default, rebinding it in that file works and the
line says so. It did not until
[#444](https://github.com/wildware-uk/clipped/issues/444): the subcommand
registered the defaults and ignored the file, which made the conflict report's
advice to "choose a different combination" impossible to act on.

A clip can only begin on a keyframe, so it is up to one keyframe interval longer
at the front than the request; and a buffer that has not filled yet gives less
than was asked for, which is said in as many words rather than left to be
noticed:

```text
Replay saved: …-replay-1.mkv (12 seconds)
  18 seconds of what was asked for was not in the buffer yet.
```

A duration no buffer can hold, or a save longer than the buffer it comes from,
is a usage error with the acceptable range in it — before a capture session
exists, not after a game has launched:

```text
error: invalid value '4000' for '--duration <SECONDS>': a replay buffer of 4000
seconds is outside the supported range 30-1800 seconds
```

The session is indexed by the next `serve` that starts, exactly as a `watch`
session is: what makes a sitting findable is its sidecar, and the library run at
start-up catches everything no run has seen ([library.md](library.md)).

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
| `--output-directory <PATH>` | `recording_directory` from the settings file, or the Clipped folder of your videos directory | Where recordings and session records go |
| `--window-timeout <SECONDS>` | 120 | How long a game may take to put a window on screen |
| `--resolution`, `--framerate`, `--codec`, `--encoder` | as `record` | Applied to every automatic recording the settings file says nothing about |
| `--microphone`, `--system-audio` | `default` | As `record`: one named audio track per source, `none` to record without it |

### The settings file, and what these options mean beside it

`watch` reads `%LOCALAPPDATA%\Clipped\settings.json` once, at start-up, and each
recording is made with the settings resolved for the game that launched
([configuration.md](configuration.md)). A setting that file gives a game — or
gives everything, in its global layer — is what that game records at. **Every
setting it does not mention is what this command line asked for**, so
`watch --framerate 144` records at 144 on a machine with no settings file, and
on one whose file says nothing about the frame rate.

The recording directory follows the same rule the other way round: it is the one
setting the flag wins, because `--output-directory` is what somebody typed for
*this* run, and where a run writes is decided before any game has launched. With
no flag, `recording_directory` from the settings file is where recordings go —
which is what a directory picked on the Settings screen sets.

A setting configured for a game therefore wins over the same option typed here.
Which of the two should win is
[issue #61](https://github.com/wildware-uk/clipped/issues/61)'s open question;
what is not open is that an option must not be silently discarded.

There need not be a settings file. Somebody who has never changed a setting has
none, and `watch` does not write one: a missing file is the ordinary case, and
one that exists but cannot be read is reported once and then ignored, leaving
these options and the shipped defaults standing. Neither case is written back
over, so a file written by a newer Clipped survives being read by an older one.

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

### Plugins

`watch` reads the plugins directory — `plugins` inside Clipped's own per-user
directory, `%LOCALAPPDATA%\Clipped\plugins`
([plugin-api.md](plugin-api.md)) — once, when it starts, and says what is there.
A directory under it that is not a usable plugin is reported with the reason
rather than passed over:

```text
A plugin could not be read: plugin.json names an executable that is not there: …
```

**Only what you enabled is started.** The settings file's `plugins` section
records which plugins you turned on and what you agreed to when you did
([#282](https://github.com/wildware-uk/clipped/issues/282)); a plugin it does
not mention is off. There is no screen that writes that section yet
([#281](https://github.com/wildware-uk/clipped/issues/281)), so unless one has
been hand-edited in, nothing is started — and when a game launches, any
installed plugin that claims it is named as one that is not running, with which
of the three reasons it was, instead of being ignored:

```text
Counter-Strike 2 highlight plugin supports Counter-Strike 2 and is installed, but
nothing in this build can record that you enabled it, so it is not running.
```

The wiring behind that line is real and is the whole of
[#338](https://github.com/wildware-uk/clipped/issues/338): a recording that is
given an enabled plugin starts it on a thread of its own, polls it once a second
and stops it when the recording ends, and a plugin that crashes, hangs or floods
costs the recording nothing. What a running plugin says goes to the log and, for
the two kinds worth interrupting somebody for, to standard error:

```text
counter-strike-2: Counter-Strike 2's integration file is not installed, so nothing will be reported.
counter-strike-2 was stopped for the rest of this recording: the plugin said nothing for 10s and was stopped
  The recording itself is unaffected.
```

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

## `recover`

Lists the recordings an interrupted recorder left behind, and lets you keep or
discard them. [sessions.md](sessions.md#recovering-what-a-killed-recorder-left)
is where the vocabulary lives — what an interrupted recording is, what adopting
and discarding write into the session record, and the words `interrupted` and
`discarded` mean once they are there. This is the reference for the command
line over it.

```text
clipped-recorder recover
clipped-recorder recover --adopt
clipped-recorder recover --discard --session <ID>
```

| Option | Default | Meaning |
| --- | --- | --- |
| `--directory <PATH>` | the Clipped folder of your videos directory, same as `watch` | Where to look for session records and the recordings they name |
| `--session <ID>` | every interrupted recording | Only the one named — the identifier `recover` prints for each |
| `--adopt` | off | Keep every recording found, or the one `--session` names |
| `--discard` | off | Move one recording's file to the trash. Requires `--session` |

**Listing is the default, and it changes nothing.** Running `recover` with no
arguments prints what there is — session, game, when it started, how large the
file is — and touches neither the recordings directory nor the session records.
That is deliberate: this is the command somebody runs to answer "where did my
recording go", and answering it must not itself change the answer.

```text
2 interrupted recordings in D:\clips:
  cs2-20260811-143205 of Counter-Strike 2, started 2026-08-11T14:32:05+01:00, 1.2 GB at D:\clips\clipped-cs2-20260811-143205.mkv
  cs2-20260811-150102 of Counter-Strike 2, started 2026-08-11T15:01:02+01:00, no file was written

These recordings play from the start. They have no index, so seeking scans the file
until it is rewritten (issue #283).
  --adopt                    keep them, and stop listing them here
  --discard --session <ID>   move one recording to the trash and record that you did
```

**`--adopt` never touches the file.** What changes is the session record: the
entry gains an end time and the `interrupted` outcome, so the recording is
indexed like any other and is not offered again. It does not rewrite the file
to give it back the index a normal recording ends with — that is
[#283](https://github.com/wildware-uk/clipped/issues/283) — so the footage
still plays from the start and seeks by scanning.

**`--discard` moves the file into `clipped-library`'s real trash rather than
deleting it** ([#451](https://github.com/wildware-uk/clipped/issues/451)). It
indexes the recording first — the recording has no library row until this
point, because the library only indexes one once its session record exists —
and only then sends that row to the trash, the same call a deletion made from
the library uses. The session record gets an end time and the `discarded`
outcome, the same shape as `--adopt`'s, because the record that a recording
existed and was thrown away is worth more than a gap. What `--discard` prints
names where the file went:

```text
Discarded D:\clips\clipped-cs2-20260811-143205.mkv: moved to the trash at D:\clips.trash\20260811-090000\clipped-cs2-20260811-143205.mkv, and listed there -- restorable until the trash is emptied or its retention expires it.
```

That is not a courtesy — it is the same trash [storage-management.md](storage-management.md#the-trash)
describes for everything else deleted from the library: listed on the trash
screen, counted towards what emptying it would reclaim, restorable, and swept
by whatever retention is configured. `storage-management.md`'s
["Getting a row before there is one"](storage-management.md#getting-a-row-before-there-is-one-clipped-recorder-recover---discard)
has the ordering that makes this safe — which of indexing, moving and closing
the sidecar record runs first, and what a failure between two of them leaves
behind.

**`--discard` always requires `--session`.** Even though the choice is
recoverable now, a bulk action nobody chose item by item is still refused
(AGENTS.md section 56) — `recover --discard` on its own is rejected rather than
moving everything it found.

## `plugins`

Shows what a plugin declares, and allows or stops one.

```text
clipped-recorder plugins list
clipped-recorder plugins enable <ID>
clipped-recorder plugins disable <ID>
```

A plugin is a program somebody else wrote, and every bundled one opens a
loopback socket. **Enabling one *is* the consent to the network access it
declares**, so the declaration is printed before consent is taken — every time,
including when you have enabled that plugin before, because consent to something
you were not shown is not consent ([docs/privacy.md](privacy.md)).

```text
acme.counter-strike-2  Counter-Strike 2 highlight plugin  0.1.0
  Reports kills, deaths and rounds from Game State Integration.
  Listens on 127.0.0.1:3212 (this machine only) — receives Counter-Strike 2 game state
  Clipped shows what a plugin declares and refuses to start one whose declaration
  has changed since you allowed it. It cannot yet stop a plugin from using the
  network in ways it did not declare.
  status: not enabled — run `plugins enable` to allow what it declares above
```

That last sentence is not decoration. A declaration shown without it reads as a
guarantee, and the guarantee Clipped can actually make is narrower than the one
a reader would assume.

`list` also prints anything under the plugins directory that could not be read
and why, because something you put there expecting it to work, which does not,
is exactly what you need told.

### The four states

| Status | What it means |
| --- | --- |
| `enabled` | It will start with the next game it supports. |
| `not enabled` | Nothing has ever allowed it. This is what a plugin you have just installed says. |
| `turned off` | You allowed it and then stopped it. What you agreed to is kept, so turning it back on will not ask again unless the plugin has changed. |
| `needs consent again` | It asks for something other than what you agreed to. Both texts are printed. It does not run until `enable` agrees to the new one. |

The state comes from the same resolution a recording uses, rather than being
worked out separately here: two answers to "will this start?" that could
disagree is the defect this command exists to prevent.

### What it does not do

It does not talk to a running recorder, so it reports no health — whether a
plugin is running, restarting, or was stopped for flooding is a live session's
business and belongs with the screen
([#281](https://github.com/wildware-uk/clipped/issues/281)), along with showing
all of this in a window rather than a terminal.

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

A **minimised** window is not excluded *from the listing*. It is a window like
any other and it is about to be restored, so it is shown, with `minimised` in
place of a size and a note in the resolution output. What it cannot be is
recorded while it stays that way: its size is not final and neither capture
backend can produce a frame for it, so `record`, `watch` and `start_recording`
all refuse it and say why
([#383](https://github.com/wildware-uk/clipped/issues/383)). Listing it and
refusing to record it are not in tension — the listing is how somebody finds the
window they need to restore.

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
best encoder is Quick Sync encodes on the CPU today. Whichever is chosen, the
audio tracks beside the picture are the same: they do not go through a video
encoder at all.

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
clipped-recorder serve [--endpoint <NAME>] [--watch-for-games]
```

| Option | Default | Notes |
| --- | --- | --- |
| `--endpoint <NAME>` | `clipped-recorder.<session>` | A name, never a path |
| `--watch-for-games` | off | Also record games as they launch, in this process |

**`--watch-for-games` is what a shipped build passes.** It runs the same launch
watcher `watch` runs, on a thread of its own, in the process that serves the
protocol and owns the global hotkeys — so a bookmark, a screenshot and a stop
reach a recording nobody had to start, through the same commands they reach one
somebody did ([#421](https://github.com/wildware-uk/clipped/issues/421),
[sessions.md](sessions.md)). `start-at-login` writes it into the `Run` key and
the desktop supervisor passes it when it starts a recorder.

It is a flag rather than the default because a `serve` started by hand, or by a
test, must not begin recording whatever game happens to be running on the
machine, or create that person's recordings folder (AGENTS.md section 25).
Recordings go where the settings file says, and to the Clipped folder of the
videos directory when it says nothing — the same three layers `watch` resolves,
minus the flag it has no command line to read.

Nothing about it can stop the recorder serving. A recordings folder that cannot
be made, or a machine that cannot be watched for launches, is reported and then
left: a recorder that refused to answer the window because it could not create a
folder would be a far worse thing to ship (AGENTS.md sections 16 and 17).

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

**A recording started over the protocol produces a session record, and reaches
the library.** `serve` opens a session for it and writes the same JSON sidecar
`watch` writes, beside the recording, from the moment the recording starts
([sessions.md](sessions.md)); when the recording ends, the session is closed and
the library index is brought up to date on a thread of its own, so the Library
screen shows it without anything being restarted ([library.md](library.md),
[#402](https://github.com/wildware-uk/clipped/issues/402)). `serve` also
reconciles once at start-up, after the ready line, which is what picks up
sittings `watch` recorded in a process of its own.

That also means `serve` reads the user's settings file, exactly as `watch` does:
what a person configured is laid over what a `start_recording` asked for, and the
session's record says which layer each answer came from
([configuration.md](configuration.md)).

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

`recover` exits 2 for `--discard` with no `--session`, and for a `--session`
that names nothing waiting to be recovered — both are the command line to
blame, not the recordings directory. It exits 1 for everything else that can
go wrong: the directory could not be read, the library index could not be
opened or indexed, a recording could not be moved into the trash, or a
session record could not be rewritten. None of those is the footage being
lost — for every one but the last, nothing has moved and the sidecar is
still open, so the next `recover` offers exactly what this one did; for the
last, the recording is already safely in the trash and only its sidecar
still says otherwise.

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
