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
and no code. It writes no file and produces no output beyond the message.

What works today is the argument surface and the shutdown path. The pipeline
between them is milestone M1 — capture backend
[#11](https://github.com/wildware-uk/clipped/issues/11), Windows Graphics
Capture [#12](https://github.com/wildware-uk/clipped/issues/12), encoders
[#15](https://github.com/wildware-uk/clipped/issues/15)–[#18](https://github.com/wildware-uk/clipped/issues/18),
muxer [#21](https://github.com/wildware-uk/clipped/issues/21).

## Commands

```text
clipped-recorder record --window <TITLE>
```

`record` is the only subcommand. Two more are specified and deliberately absent
until they do something (AGENTS.md section 27):

| Command | What it will do | Issue |
| --- | --- | --- |
| `list-windows` | List capturable windows with title, process and PID | [#10](https://github.com/wildware-uk/clipped/issues/10) |
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

The default output directory — `Clipped` inside the user's videos folder — is
created if it does not exist, because it is Clipped's own. A directory named
with `--output` must already exist: a path that is not there is far more often a
typo than an instruction to build a tree, and creating one silently is how
recordings end up somewhere nobody looks.

An existing output file is never replaced without `--overwrite`.

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

## Diagnostics

The recorder uses `clipped-logging`, so log files are under
`%LOCALAPPDATA%\Clipped\logs` and the same records go to standard error. See
[logging.md](logging.md) for the level file and for the standard context fields.

```text
CLIPPED_LOG=debug clipped-recorder record --window "Counter-Strike 2"
```

Standard output is left for machine-readable output — `list-windows` and
`capabilities` will use it — so errors and progress go to standard error.

The resolved configuration is logged once, at `info`, before anything uses it.
The output path is redacted to its file name and a digest of the whole path
(`RedactedPath`), because a recording path normally contains the account name.

## Stopping a recording

Ctrl+C asks the recorder to stop; it does not kill it. The handler raises a
shutdown signal, the recording loop notices at its next frame boundary, and a
finalisation hook runs before the process exits — that is where the encoder is
flushed and the container closed, so that the file left behind is complete.

The seam is `clipped_recorder::shutdown::run_until_shutdown`, and the hook is
guaranteed to run exactly once whether the recording ended by itself, was
interrupted, returned an error or panicked. The panic case is deliberate: a bug
in the pipeline should still leave a file that plays.

The signal path is tested against a real process receiving a real
`CTRL_C_EVENT`, in `apps/recorder/tests/ctrl_c.rs`, using the fixture in
`apps/recorder/examples/shutdown_fixture.rs`. What is *not* tested, because it
cannot be until the pipeline exists, is that the resulting file plays — that is
the second half of acceptance criterion 3 on
[issue #9](https://github.com/wildware-uk/clipped/issues/9) and it is not
claimed anywhere.

## Testing the command line

```text
cargo test -p clipped-recorder
```

Unit tests cover parsing and validation, including the wording of the error
messages: an error message is behaviour someone depends on, and changing one
should be a decision rather than a side effect. `tests/command_line.rs` runs the
built binary and asserts what it prints and what it exits with, including that a
`record` invocation creates no file.

`cargo test --test ctrl_c` on its own is not enough: cargo builds examples for
`cargo test` but not for a single named test target, so the Ctrl+C test would
run against a stale fixture. Run `cargo test -p clipped-recorder`.
