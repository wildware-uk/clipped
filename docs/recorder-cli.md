# Recorder command line

`clipped-recorder` is the process that owns recordings. It runs independently of
the desktop application ([ADR 0002](adr/0002-separate-recorder-process.md)), and
until the IPC protocol arrives in M5 the command line is the only way to drive
it. It is also how capture is tested without a UI, which is a reason to keep it
good rather than a reason to treat it as scaffolding.

## Status

**The recorder cannot record yet.** `record` parses and validates its arguments,
resolves them into a configuration, installs its Ctrl+C handler and then reports
that there is no capture engine, because there is not one: `crates/capture`,
`crates/encoder`, `crates/audio` and `crates/muxer` contain module documentation
and no code.

It produces no recording, and leaves nothing behind on the way to not producing
one: no output file, and no recordings directory. It is not silent, though. The
resolved configuration and the error both go to standard error, and to the log
files under `%LOCALAPPDATA%\Clipped\logs` that every Clipped process writes
([logging.md](logging.md)). The one thing validation puts on disk is a write
probe, described under [defaults that touch the disk](#defaults-that-touch-the-disk),
and it is removed again immediately.

What works today is the argument surface, `list-windows`, and the shutdown path.
The pipeline between them is milestone M1 — capture backend
[#11](https://github.com/wildware-uk/clipped/issues/11), Windows Graphics
Capture [#12](https://github.com/wildware-uk/clipped/issues/12), encoders
[#15](https://github.com/wildware-uk/clipped/issues/15)–[#18](https://github.com/wildware-uk/clipped/issues/18),
muxer [#21](https://github.com/wildware-uk/clipped/issues/21).

`record` does not yet resolve its target through `list-windows`' machinery. The
enumeration and the selection rules are in `clipped-windows` and are what
`list-windows` runs on; wiring them into `record` belongs with the capture
backend that will consume the resulting window handle
([#11](https://github.com/wildware-uk/clipped/issues/11) and
[#12](https://github.com/wildware-uk/clipped/issues/12)), because until then
there is nothing to hand a resolved window to.

## Commands

```text
clipped-recorder record --window <TITLE>
clipped-recorder list-windows [--all] [<selector>]
```

One more is specified and deliberately absent until it does something
(AGENTS.md section 27):

| Command | What it will do | Issue |
| --- | --- | --- |
| `capabilities` | Report detected encoders, codecs and limits | [#14](https://github.com/wildware-uk/clipped/issues/14) |

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

## `list-windows`

Lists what the recorder can be pointed at. This is the answer to "why can't it
find my game?", and it is how the target selection rules are inspected without
starting a recording.

```text
clipped-recorder list-windows
```

The listings in this section come from a real run on a two-monitor desktop, with
rows removed and window titles shortened so that they fit the page and do not
publish anybody's open tabs. The shape, the columns and the wording are exactly
what the command prints.

```text
8 of 424 top-level windows can be captured.

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

At most one selector may be given. With none, the command lists; with one, it
resolves and prints everything known about the single window that answers to it:

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

error: 7 windows match the window title containing `e`, and choosing between them would be a guess:
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

Long lists are unremarkable — the desktop above had 424 top-level windows and 8
that could be captured — which is why the exclusion reasons exist rather than a
filter that quietly drops them.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | The command succeeded |
| 1 | The command failed while running |
| 2 | The arguments were rejected — the same code clap uses for a usage error |
| 3 | The command is not implemented in this build |

3 is separate from 1 so that a script, and the test suite, can tell "this does
not exist yet" from "this went wrong". Today `record` always exits 3 once its
arguments are accepted.

`list-windows` exits 2 when a selector matched no window, or more than one: the
command line is what has to change, and the message already lists the windows
that could have been meant. It exits 1 only when Windows refused to describe the
desktop at all.

## Diagnostics

The recorder uses `clipped-logging`, so log files are under
`%LOCALAPPDATA%\Clipped\logs` and the same records go to standard error. See
[logging.md](logging.md) for the level file and for the standard context fields.

```text
CLIPPED_LOG=debug clipped-recorder record --window "Counter-Strike 2"
```

Standard output is left for a command's result — `list-windows` prints its
listing there, and `capabilities` will — so errors and progress go to standard
error. A `list-windows` run piped into another program therefore carries the
table and nothing else.

The resolved configuration is logged once, at `info`, before anything uses it.
The output path is redacted to its file name and a digest of the whole path
(`RedactedPath`), because a recording path normally contains the account name.

## Stopping a recording

Ctrl+C asks the recorder to stop; it does not kill it. The handler raises a
shutdown signal and a finalisation hook runs before the process exits. There is
no recording loop between them yet: when the pipeline arrives, the loop will
notice the signal at its next frame boundary, and the hook will flush the
encoder and close the container so that the file left behind is complete.

What exists today is both ends of that. The seam is
`clipped_recorder::shutdown::run_until_shutdown`, and the hook it runs is
guaranteed to run exactly once whether the body ended by itself, was
interrupted, returned an error or panicked. The panic case is deliberate: a bug
in the pipeline should still leave a file that plays.

The signal path is tested against a real process receiving a real
`CTRL_C_EVENT`, in `apps/recorder/tests/ctrl_c.rs`, using the fixture in
`apps/recorder/examples/shutdown_fixture.rs`. What is *not* tested, because it
cannot be until the pipeline exists, is that the resulting file plays. That is
the second half of acceptance criterion 3 on
[issue #9](https://github.com/wildware-uk/clipped/issues/9); it is claimed
nowhere, and verifying it with `ffprobe` against a real interrupted recording is
[issue #126](https://github.com/wildware-uk/clipped/issues/126).

## Testing the command line

```text
cargo test -p clipped-recorder
```

Unit tests cover parsing and validation, including the wording of the error
messages: an error message is behaviour someone depends on, and changing one
should be a decision rather than a side effect. `tests/command_line.rs` runs the
built binary and asserts what it prints and what it exits with, including that a
`record` invocation creates neither an output file nor, when it is left to work
out the default path, a recordings directory.

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
