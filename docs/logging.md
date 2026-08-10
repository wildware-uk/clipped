# Logging

Clipped is usually debugged without access to the machine it failed on, so logs
are the primary diagnostic (SPEC.md section 36). `clipped-logging` owns where
logs go and how much is recorded. It does not own logging itself: there is no
Clipped-specific wrapper to route diagnostics through, and a crate that wants to
emit events calls `tracing::info!` and friends directly.

To do that, add the facade to the crate's manifest:

```toml
[dependencies]
tracing.workspace = true
```

`tracing` is pinned once in the root manifest's `[workspace.dependencies]`, so
every crate that adopts it gets the same version and the same callsite registry.
At the time of writing `clipped-logging`, `clipped-encoder` and
`clipped-recorder` are the packages that have taken the dependency; most of the
rest of `crates/` is still documentation-only stubs. A crate takes it in the
change that gives it something to log.

The crate sits at layer 0 and depends on no other `clipped-*` crate, so
platform primitives and application logic alike can use it.

## Starting up

Call `init` once, as early in `main` as possible, and keep the guard alive for
the life of the process:

```rust
let _logging = clipped_logging::init(&clipped_logging::LogSettings::default())?;
```

Events emitted before `init` runs are discarded, and so are events emitted
after the guard is dropped — dropping it flushes what the background writer
still holds and stops the writer thread.

## Where logs go

| What | Where |
| --- | --- |
| Log files | `%LOCALAPPDATA%\Clipped\logs\clipped.<yyyy-mm-dd-hh>.log` |
| Level configuration file | `%LOCALAPPDATA%\Clipped\log-level.txt` |

Both paths come from `%LOCALAPPDATA%` at run time; nothing is hardcoded. On a
non-Windows machine — Clipped targets Windows, but the crate still builds and
tests elsewhere — the directory follows `$XDG_STATE_HOME/clipped`, falling back
to `$HOME/.local/state/clipped`.

Keeping that true takes a little care in the tests: `Path::file_name` splits on
the separators of the platform it was compiled for, so a test that asserts on a
`C:\...` literal passes on Windows and fails on Linux, where the whole literal is
one file name. Those assertions are gated to Windows and have an equivalent that
runs everywhere, and `cargo test -p clipped-logging` is expected to pass on both.

The same log records are also written to standard error whenever console output
is enabled, which it is by default; `LogSettings::with_console(false)` turns that
layer off. Whether standard error is a terminal decides one thing only: colour.
Standard output is left alone so that anything the recorder prints there stays
machine-readable.

`LoggingGuard::directory()` returns the directory in use, which is what an
"open log folder" action in the desktop application should call. It is not
logged, because it contains the account name.

### Rotation and disk use

Files rotate **hourly** and the newest **48** are kept, which is two days of
history. The recorder is expected to stay running for days (AGENTS.md section
59), so rotating daily would mean a single file growing for the whole run.

Old files are deleted both when a file rotates and when the process starts, so
a machine that has been recording for a fortnight does not accumulate a
fortnight of logs, and neither does one that is restarted repeatedly.

`LogSettings::with_retained_files` changes the bound; `with_directory` changes
the location. Both exist mainly for tests and portable installations.

## Changing the level without rebuilding

The level is an [`EnvFilter`] directive, so it can be a bare level (`debug`) or
a per-target list (`info,clipped_capture=trace,clipped_encoder=debug`). Targets
are crate names with underscores.

Four sources are consulted, in this order, and the first one that is set and
parses wins:

1. `CLIPPED_LOG`
2. `RUST_LOG`
3. `%LOCALAPPDATA%\Clipped\log-level.txt`
4. the default the application passes to `LogSettings::with_default_level`

falling back to `info` if none of them says anything.

Environment variables come first because they are how someone reproduces a
problem in a single run without editing anything. `CLIPPED_LOG` exists so that
turning Clipped up does not also turn up an unrelated Rust program that reads
`RUST_LOG` in the same shell. A value that is empty or only whitespace counts
as unset, because an exported but empty `RUST_LOG` is a common shell accident.

The level file holds one directive on a line of its own; blank lines and lines
starting with `#` are ignored:

```text
# Clipped log level. Delete this file to return to the default.
info,clipped_capture=debug
```

A directive that does not parse is skipped with a warning and the next source
is tried, so a typo in `CLIPPED_LOG` cannot leave a run with no logging at all.
The first line of every run records which source was used:

```text
INFO clipped_logging: diagnostics initialised filter=debug filter_source=CLIPPED_LOG retained_files=48 rotation=hourly
```

Nothing here requires a rebuild, and nothing requires a restart of anything but
the recorder process.

## Standard fields

AGENTS.md section 35 asks every diagnostic to carry `session_id`, `game_id`,
`capture_backend`, `encoder` and `audio_source`. Attaching them by hand at each
call site fails in two ways that only surface when someone is debugging a
user's recording months later: a misspelled field name makes a log line
unsearchable, and a field filled with free text puts user content in a log.

`SessionContext` prevents both. The field names are written once, inside it,
and the values are either closed enumerations or validated identifiers:

```rust
use clipped_logging::{CaptureBackend, GameId, SessionContext, SessionId, VideoEncoder};

let context = SessionContext::new(SessionId::new(&session.id)?)
    .with_game_id(GameId::new("counter-strike-2")?)
    .with_capture_backend(CaptureBackend::WindowsGraphicsCapture)
    .with_encoder(VideoEncoder::Nvenc);

let span = context.span();
let _entered = span.enter();
tracing::info!(width = 2560, height = 1440, "capture started");
```

Everything logged inside the span carries the fields:

```text
INFO clipped_session{session_id=01H8XGJT4A game_id=counter-strike-2 capture_backend=windows_graphics_capture encoder=nvenc}: clipped_capture: capture started width=2560 height=1440
```

| Field | Type | Values |
| --- | --- | --- |
| `session_id` | `SessionId` | `[A-Za-z0-9._-]`, 1–64 characters |
| `game_id` | `GameId` | `[A-Za-z0-9._-]`, 1–64 characters |
| `capture_backend` | `CaptureBackend` | `windows_graphics_capture`, `desktop_duplication` |
| `encoder` | `VideoEncoder` | `nvenc`, `amd_amf`, `intel_quicksync`, `software_h264`, `software_h265` |
| `audio_source` | `AudioSource` | `compatibility_mix`, `game`, `other_system`, `system_audio`, `microphone`, `application` |

A field that is not yet known is absent from the log line rather than recorded
as a placeholder, so `game_id` appearing at all means game detection had
resolved one. Adding a value to one of the enumerations is a deliberate change
to this table as well as to the code.

`system_audio` was added that way, for
[issue #20](https://github.com/wildware-uk/clipped/issues/20). The other
`audio_source` values name tracks a session routes audio *into*; a capture of
the output device in loopback mode is none of them, and it is what
`clipped-audio` records today. That crate attaches `audio_source` to its own
lines rather than waiting for a session span, because a recording runs two
captures on two threads and the field is what tells their lines apart — but it
attaches the same `AudioSource` values, so the concept has one vocabulary and
not two.

## Privacy: what is and is not guaranteed

AGENTS.md section 13 forbids logging window contents, microphone content,
private message contents and file contents.

**Guaranteed by types, and tested in `crates/logging/tests/privacy.rs`:**

- The three enumerated fields cannot hold anything but their listed values —
  the compiler refuses free text.
- `session_id` and `game_id` reject anything that is not a plain identifier.
  Spaces, punctuation, path separators and newlines are all outside the
  permitted set, so a window title, a chat line or a file path cannot be
  recorded through them.
- Rejecting a value never repeats it. The rejection describes the shape of the
  problem — empty, too long, forbidden character at index *n* — because the
  value being rejected may be exactly the content that must not be printed.
- `RedactedPath` reduces a path to its final component plus a digest of the
  whole path, so no directory component survives: not the account name, not the
  drive layout, not the folder names someone chose.

  ```rust
  let recording_path = r"C:\Users\alice\Videos\Clipped\match.mkv";
  tracing::info!(path = %RedactedPath::new(recording_path), "recording finished");
  // path=match.mkv#eb9715073a66288e
  ```

  Equal digests mean the same path, so a sequence of operations on one file can
  still be followed, and two files that share a name stay distinguishable.

**Not guaranteed.** This crate cannot stop a contributor writing
`tracing::info!("{window_title}")` by hand, and it does not claim to. What it
does is remove the reason to: the fields with a legitimate need to identify a
session, a game or a file all have a type that carries the identity without the
content.

Two further limits, stated rather than glossed over:

- `RedactedPath` does not anonymise the file name itself. Clipped names its own
  recordings, so that is normally a generated name, but a path the *user* chose
  can carry meaning in its final component. Redaction is a backstop, not a
  licence to log arbitrary user paths.
- The digest is FNV-1a, chosen because it only has to be stable and cheap. It
  is a correlation key, not a way of hiding a value from someone determined to
  recover it.

## Logging from a capture thread

A capture thread runs once per frame — 240 times a second on a high refresh
rate monitor — and both per-frame log spam and diagnostics that cost something
when switched off are ruled out by AGENTS.md sections 18 and 35. Pick the
mechanism that matches what you are doing:

**Detail only useful while debugging the capture loop itself.** Use
`trace_frame!`. It takes the same arguments as `tracing::trace!` and is behind
the crate's `frame-tracing` feature, so in every shipped build it compiles to
nothing and its arguments are never evaluated:

```rust
clipped_logging::trace_frame!(
    frame_index,
    presentation_time_us = presentation_time.as_micros() as u64,
    "frame acquired"
);
```

Turn it on for a debugging session. A feature belongs to the package that
declares it, so the switch has to be spelled against a package that depends on
`clipped-logging`:

```text
cargo test -p clipped-logging --features frame-tracing
```

From a binary that depends on it, the same feature is reached through the
dependency:

```text
cargo run -p clipped-recorder --features clipped-logging/frame-tracing
```

That works today, because `clipped-recorder` lists `clipped-logging` in its
dependencies. Leaving off the `clipped-logging/` prefix does not: cargo reports
*"the package 'clipped-recorder' does not contain this feature:
frame-tracing"*, and names the package that does.

**Detail worth having occasionally in a normal build.** Use `FrameSampler`,
which reduces a per-frame event to one every *n* frames with a plain counter —
no allocation, no lock, no clock read. It is deliberately `&mut self` and not
shareable, so sampling never introduces synchronisation onto a capture thread:

```rust
let mut sampler = FrameSampler::every(NonZeroU32::new(600).unwrap());
loop {
    let frame = capture.next_frame()?;
    if sampler.should_log() {
        tracing::debug!(dropped = dropped_frames, "capture loop healthy");
    }
}
```

**Anything that happens once a second or less** — a resolution change, an
encoder reconfiguration, a device disappearing — is not a hot path. Log it
normally.

File writes never happen on the calling thread: the appender runs behind
`tracing-appender`'s background writer, so a capture thread cannot block on
disk.

## What disabled logging costs

AGENTS.md section 18 forbids claiming a performance property without measuring
it, so `crates/logging/tests/hot_loop_cost.rs` measures this one on every test
run. It times four `#[inline(never)]` loops with identical arithmetic — two of
them byte-for-byte identical, so their residual difference is the noise floor —
runs each for the same iteration count once per round, rotates the order each
round, and keeps each loop's fastest round.

Measured on an AMD Ryzen 9 9950X3D (16 cores, 4.3 GHz base), Windows 11 Pro,
rustc 1.97.1 x86_64-pc-windows-msvc, 5,000,000 iterations per round, 9 rounds,
median of three runs:

| | release | debug |
| --- | --- | --- |
| Baseline loop | 0.199 ns/iteration | 3.92 ns/iteration |
| Same loop again (noise floor) | +0.005 | −0.18 |
| `trace_frame!`, feature off | +0.016 | −0.21 |
| `tracing::debug!` below the level | +0.127 | +2.59 |

Read honestly:

- **`trace_frame!` has no measurable cost.** Its overhead is the same size as
  the difference between two identical loops, which is to say the measurement
  cannot see it. `crates/logging/tests/frame_tracing.rs` says why, without
  timing anything: the arguments are never evaluated, even with a subscriber
  installed that would accept the event.
- **A disabled `tracing::debug!` costs about 0.13 ns per call** in a release
  build — roughly half a cycle, the relaxed atomic load `tracing` uses to
  reject an event below the current level. That is *measurable*, so it is
  reported rather than rounded to zero. At 240 frames a second it is 30
  nanoseconds per second of recording, against a 4.2 millisecond frame budget.
- The debug-build figures are larger and are included for completeness. Nothing
  ships unoptimised, and the ±0.2 ns spread between two identical loops there
  is code placement, not logging.

The test asserts that `trace_frame!` stays within the measured noise floor plus
the larger of 0.5 ns and a quarter of the baseline loop's own cost, and that a
disabled `debug!` stays under the larger of 5 ns and that same baseline. Both
bounds scale with the machine on purpose: code placement noise is a proportion
of what a loop costs, not a fixed number of nanoseconds, and a bound that a slow
or contended runner trips is a failure nobody can tell apart from a real
regression (AGENTS.md sections 25 and 51). Both are set to catch a structural
regression — a lost feature gate, an eagerly evaluated argument, a lock — rather
than a single extra branch, which no threshold could distinguish from a busy
machine.

Reproduce it with the numbers visible:

```text
cargo test --release -p clipped-logging --test hot_loop_cost -- --nocapture
```

[`EnvFilter`]: https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html
