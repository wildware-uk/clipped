# Game detection

Clipped records games without being told to, which means it has to know what a
game is. `clipped-game-detection` is where that knowledge lives.

**Status: the game catalogue exists ([#42]) and the process watcher exists
([#41]). Nothing joins them yet.** The watcher reports what started and what
stopped; the catalogue can say which game a process is; the layer that asks one
about the other, and starts a recording, is [#46]. Launcher-specific detection
is [#43] and [#44], and the user-facing registration screen is [#45]. This
document grows a section per part as they land, and marks the rest as intent
(AGENTS.md section 7).

[#41]: https://github.com/wildware-uk/clipped/issues/41
[#42]: https://github.com/wildware-uk/clipped/issues/42
[#43]: https://github.com/wildware-uk/clipped/issues/43
[#44]: https://github.com/wildware-uk/clipped/issues/44
[#45]: https://github.com/wildware-uk/clipped/issues/45
[#46]: https://github.com/wildware-uk/clipped/issues/46

## The game catalogue

The catalogue answers one question: *given a running process, which game is
this?* It holds the record SPEC.md section 6 asks for — identity, name,
executables, launcher, icon, capture compatibility, known child processes,
default settings and highlight providers — and the rules for matching a process
against it.

It does not start recordings, watch processes, or talk to a launcher.

### Adding a game needs no code

Append a `[[game]]` block to
[`crates/game-detection/data/games.toml`](../crates/game-detection/data/games.toml)
and open a pull request. That is the whole procedure. Nothing has to be
registered, no Rust file changes, and the file's own header carries the field
reference so you need not read this document to add an entry.

```toml
[[game]]
game_id = "some-game"
name = "Some Game"

[[game.executables]]
name = "somegame.exe"
path_contains = "steamapps/common/Some Game"

[game.launcher]
kind = "steam"
app_id = "12345"

[game.capture]
compatibility = "unknown"
```

The file is compiled into the binary with `include_str!`, so `cargo build`
publishes the entry and there is no install step that can go wrong or data file
that can fall out of step with the build reading it.

### Why TOML

The seed file's audience is a contributor reading a pull request diff, so the
format was chosen for that and not for the parser:

- **Entries are stanzas.** Adding one appends contiguous lines; it does not
  modify the entry above it. In JSON, a trailing comma makes every insertion a
  two-line diff and makes the last entry a merge conflict for the next
  contributor.
- **It has comments.** Half of what an entry needs to say is *why* — why an
  executable is path-qualified, what a launcher's identifier refers to, what has
  and has not been verified about a game's capture. A `"_comment"` key is a
  worse version of a comment.
- **Its errors carry a line and a column**, which is most of the requirement
  that a bad entry fails loudly and says where.

The cost is a second parser in the dependency graph: `serde_json` stays where it
is for the encoder capability cache and the IPC protocol, which are
machine-written files where none of the three reasons applies.

### Two files, and which wins

| | Seed data | User overlay |
| --- | --- | --- |
| Where | compiled in from `crates/game-detection/data/games.toml` | `%LOCALAPPDATA%\Clipped\games.toml` |
| Whose | the project's | the user's |
| Edited by | a pull request | the user, or the registration UI ([#45]) |
| On update | replaced wholesale | never touched |
| If the schema changes | rewritten by us | migrated, with a backup |

An overlay entry whose `game_id` matches a shipped one **replaces it entirely**
— it is not merged field by field. Merging would mean a user could add to a
shipped entry but never correct one, and would leave "which of the two `name`
fields won?" as a question about somebody's own file. An overlay entry with an
identifier of its own is simply another entry.

### Matching, and the precedence order

A process is looked up by its executable's file name, and — where the caller has
them — its full image path and whatever its launcher says it is. Every entry
that matches at all matches at one of three strengths:

| Rung | Strength | What matched |
| --- | --- | --- |
| 1 (strongest) | `LauncherIdentity` | The launcher's own identifier for the game. The executable is not consulted. |
| 2 | `QualifiedPath` | The file name matched **and** the image path contains the entry's `path_contains` fragment, as whole directory names. |
| 3 | `ExecutableName` | The file name matched an entry that asked for nothing else. |

The order in full:

1. **The strongest rung wins outright.**
2. **At equal strength, the user's overlay entry beats the shipped one.**
3. **If entries still tie, the answer is ambiguous** and names every candidate.
   Nothing picks one.

Rule 2 sits *below* rule 1 deliberately, and it is the one part worth
explaining. If a user's bare `game.exe` entry outranked a shipped entry that had
path-qualified the same `game.exe`, one broad personal entry would quietly take
over every game sharing a common executable name. Specificity is evidence about
*this* process; a bare file name is a guess by whoever wrote it. A user who
wants to override one specific installation writes a path qualifier of their own
and wins on rung 2 by having said something equally specific.

Ambiguity is reported rather than resolved, because a recorder that guessed
would file somebody's session under the wrong game. Three publishers shipping
`launcher.exe` is a real situation, and the caller — the process watcher, or
eventually the user — is in a far better position to settle it.

Two details that follow from the rungs:

- **A qualified rule never falls back to matching on the name alone.** The
  qualifier exists precisely because the name is not enough, so a process whose
  path is unknown does not match a qualified entry at all.
- **`child_processes` is not a way in.** A game's anti-cheat service or crash
  handler is not the game, and matching on that list would make every helper
  process a reason to start recording. The list is carried for whoever watches
  processes ([#41]) to use.

Comparison follows Windows: executable names and path fragments are matched
case-insensitively, and a `path_contains` fragment may be written with either
kind of slash.

#### A path qualifier names directories, not characters

The fragment is compared segment by segment, and it has to line up with
directory boundaries at both ends. This is a correctness rule rather than
tidiness. `steamapps/common/Half-Life 2` is a *substring* of
`steamapps/common/Half-Life 2 Deathmatch`, which is a different Steam
application — 320 rather than 220 — that installs beside Half-Life 2 and runs
the same `hl2.exe`. Half-Life 2: Lost Coast and both episodes are the same
shape. Matched as characters, the shipped Half-Life 2 entry claims all of them:
a confident wrong answer at rung 2, with nothing reported as ambiguous, which is
precisely the outcome the ambiguity rule above exists to avoid.

The same applies at the front of a fragment, so `common/Portal` is not found
inside `.../uncommon/Portal/...`.

Leading, trailing and doubled separators in a fragment change nothing, because
what is compared is the list of directory names either side of them. A fragment
that names no directory at all — `"/"` — matches nothing rather than everything;
validation cannot tell it from a real qualifier, since it is not empty, and an
entry that claimed every process on the machine is the failure this rung exists
to prevent.

### Fields for subsystems that do not exist yet

`default_settings` and `highlight_providers` are held and validated by the
catalogue and interpreted by nothing. Per-game settings are M7 (SPEC.md section
31) and the highlight provider API is M9, and inventing behaviour for either
here would be building a later milestone's work. They are carried rather than
omitted so that adding them later does not invalidate every entry contributed in
the meantime.

`icon` is the same: a name the desktop application will resolve, carried and not
resolved.

`compatibility` is a record of what somebody actually verified, not a
prediction. Every shipped entry says `unknown`, because nobody has yet run a
capture against these games and written down what happened; a test asserts that,
so a future entry cannot quietly claim otherwise without the evidence arriving in
the same pull request.

### Nothing is skipped quietly

A malformed entry fails the load with a message naming the file, the entry — by
position and by `game_id` — and what to write instead:

```text
C:\Users\somebody\AppData\Local\Clipped\games.toml: entry 2 (`my-game`): executable 1 has
`name = "C:/Games/MyGame/my-game.exe"`, which contains a directory separator; `name` is the
file name alone and the directory belongs in `path_contains`
```

Unknown keys are refused rather than ignored, so a misspelled `path_contian`
fails instead of silently doing nothing. Nothing is ever dropped and the rest of
the file loaded: a game silently missing from the catalogue is a recording that
silently does not happen.

### Schema versioning and the migration path

Both files carry `schema_version`, which is the version of the *format* and not
of Clipped. It changes when the shape of an entry changes and at no other time,
so adding a game never touches it. It is version 1 today.

| What Clipped finds | What happens |
| --- | --- |
| The current version | Read directly. The file is not rewritten, so a user's comments and layout survive. |
| An **older** version | Converted in memory, validated, then — for the overlay only — backed up and rewritten. |
| A **newer** version | Refused, with a message saying so. **Nothing is written back.** |

Refusing a newer file matters: an older Clipped rewriting a file it does not
understand is exactly how a user loses the entries they added on the machine
that was up to date.

Migrating the overlay happens in a careful order, because that file is the
user's and cannot be replaced from anywhere (AGENTS.md sections 43 and 56):

1. Read it. Nothing is written yet.
2. Convert it in memory and validate the result. If either fails, **the file has
   not been touched at all**, and the error says so.
3. Copy the original to `games.toml.v<old>.bak`. If that fails, stop — still
   without having touched the original.
4. Write the converted document to a neighbouring temporary file and rename it
   over the original, so an interrupted write leaves the previous file rather
   than half of a new one.

A migration therefore always leaves two readable files: the converted one, and
the exact bytes the user had before. That backup is not a nicety — TOML comments
do not survive the conversion, because the document is rewritten from its parsed
form.

The list of conversions is **empty today, and correctly so**: version 1 is the
first version there has ever been, so no file older than the current schema
exists anywhere, and writing a migration for a version that never shipped would
be inventing history. The machinery that runs them is built and tested now —
against conversions the tests supply themselves, including a two-step chain, a
step that refuses, a conversion whose result does not validate, and a backup that
cannot be taken. When version 2 arrives, the change is one entry in a list and
one function.

### Why this is not in the database

The catalogue is reference data that ships with the application and is read at
start-up. M6's [#55] introduces the SQLite schema and migration framework and
does not exist yet; adding a second persistence mechanism ahead of it would be
the accident AGENTS.md section 55 exists to prevent. The user overlay is a file
for the same reason it is separate from the seed data: it is the user's, it
should be editable by hand, and it should be obvious where it is.

[#55]: https://github.com/wildware-uk/clipped/issues/55

## The process watcher

### What it does

```text
Windows ──▶ process started / exited ──▶ debounce ──▶ one launch, or one exit
```

The watcher reports two things, and deliberately not a third:

- **A launch**: every process the debounce collected into it, oldest first, with
  the executable path, the process identifier and the parent identifier of each.
- **An exit**: a process it had previously reported is gone, carrying what it
  was reported with.

It does not report *games*. It has no idea what a game is, and it does not
consult the catalogue above: the layer that puts the two together, and decides
that a launch is worth recording, is [#46]. Keeping them apart is what lets the
debounce be tested against process trees written down in a test, and the
matching rules against paths written down in a test, neither needing the other.

### Why it exists in this shape

#### The watcher does not poll

SPEC.md section 6 asks for automatic detection, which means something has to be
watching all the time, on a machine that is also running a game. A loop that
enumerates every process every few hundred milliseconds is precisely what
AGENTS.md section 18 forbids, so the primary source is a subscription: this
process blocks inside `IEnumWbemClassObject::Next` and Windows wakes it.

Windows offers three ways to be told, and only one of them is available to an
application running as an ordinary user:

| Mechanism | Latency | Standing cost | Requires |
| --- | --- | --- | --- |
| WMI `__InstanceCreationEvent` / `__InstanceDeletionEvent` `WITHIN n` over `Win32_Process` | up to `n` seconds | the WMI service compares the process table with itself once per `n` seconds, per subscription | nothing |
| `Win32_ProcessStartTrace` / `Win32_ProcessStopTrace` (WMI over ETW) | milliseconds | none until something happens | administrator |
| An ETW kernel session | milliseconds | none until something happens | administrator, and a session other tools compete for |

Clipped runs unelevated, because a recorder that demands administrator rights is
a recorder people run once. That rules the second and third rows out as the
primary source — a mechanism that works only for some users is not a detection
strategy — and leaves the first, which works for everybody at the cost of a
bounded delay and of work done in a service rather than here. What that cost
actually is, measured, is [below](#what-it-costs).

#### The fallback, and what triggers it

WMI is a service, and services stop. Its repository can be corrupted, group
policy can refuse the connection, and `winmgmt` can be restarted underneath a
working subscription. Every one of those ends with no events arriving, which for
a recorder means silently never recording again, so the failure is handled
rather than hoped against (AGENTS.md section 16):

```text
                 ┌─ established ──▶ WMI notifications ─── lost ──┐
start ──▶ try WMI┤                                               ├──▶ snapshot polling
                 └─ refused, or no answer within 10 seconds ─────┘         │
                                                                       lost │
                                                                            ▼
                                                             WatchEvent::Stopped
```

The fallback is `CreateToolhelp32Snapshot` on a timer, diffed against the
previous pass. It polls, which is the thing the design is trying to avoid, and
it is still worth having: the alternative when WMI is unavailable is no
detection at all. It is the only mechanism left that needs neither WMI nor
elevation.

Two things make the change visible rather than silent. `WatchEvent::SourceChanged`
is delivered to the caller with the reason, and `ProcessWatcher::declined_source`
reports why the preferred source was not used, so a diagnostics screen can say
"this machine is on the fallback, because …" instead of leaving somebody to
wonder why detection feels slow. If the fallback is lost too, the watcher
reports `WatchEvent::Stopped` and goes quiet — it never pretends to be watching.

#### A game launch is not one process

Steam starts a launcher, the launcher starts the game, an anti-cheat wrapper may
sit between them, and some titles re-execute themselves once. Reporting each of
those separately would have the session layer start and abandon three recordings
in two seconds, so starts are collected into a *launch* by following the parent
chain — which is why every event carries a parent — and reported once the chain
goes quiet.

The rules, in full:

1. A process that starts joins the launch its parent belongs to, if that launch
   is still being collected; otherwise it opens a new one.
2. A launch stays open until nothing has joined it for `launch_quiet_period`, up
   to `max_launch_window` from its first member. The cap is what stops a
   launcher that starts a helper every second from deferring its own launch
   indefinitely.
3. A process that exits before its launch is reported is removed from it, and
   neither its start nor its exit is ever reported. **This is what makes a
   launcher that hands over to the game and disappears invisible.** A launch that
   loses every member is dropped entirely.
4. A process that exits after its launch was reported is reported gone, with the
   executable and parent it was reported with. Nothing looks it up at that
   point, because there is nothing left to look up.
5. Exits are gathered for `exit_settle_period` and reported children first, so a
   parent and a child dying together arrive in an order that means something.
6. A process identifier that starts again while the watcher still believes it is
   alive means Windows reused it; the old one is reported gone first.

`LaunchGroup::newest` is the best single guess at "the thing that was actually
launched", and it is a guess with a proof behind it rather than a heuristic: a
process cannot start before its parent, so the last member to start has no
descendants inside the group.

### What it costs

All figures measured on:

| | |
| --- | --- |
| Machine | AMD Ryzen 9 9950X3D, 16 cores / 32 logical processors, Windows 11 Pro 26200 |
| Processes running | 382–390 |
| Build | `--release` |
| Window | 180 seconds per measurement |
| Method | cumulative `\Process V2(…)\% Processor Time` raw counters for the `Winmgmt` service host and every `WmiPrvSE.exe`, sampled before and after; `GetProcessTimes` for the watcher's own process (`examples/process_watch_probe.rs`) |

The machine was in ordinary use during these runs rather than quiescent — the
watcher saw 79 real process events in the 180-second window at the default
settings — so the control row is what the numbers are read against.

#### Idle cost

"Idle" here means the watcher is running and no game is being launched. It is
not a quiet machine: Windows starts and stops background processes constantly,
and that is the load the WMI comparison is doing work about.

| Configuration | WMI side, % of one core | Attributable to Clipped | Watcher process, % of one core |
| --- | --- | --- | --- |
| No watcher (control, two runs) | 2.47, 2.37 | — | — |
| `WITHIN 1` | 25.73 | +23.3 | 0.069 |
| **`WITHIN 2` (the default)** | **13.78** | **+11.4** | **0.017** |
| `WITHIN 4` | 7.51 | +5.1 | below the 15.6 ms clock granularity |

Read the two halves separately, because they are not the same kind of cost.

**The watcher's own process is close to free**: 31 ms of processor time in 180
seconds at the default settings, 0.017% of one core, or 0.0005% of a
32-processor machine. That is the point of not polling — the thread is asleep
except when something happens.

**The WMI service is not.** The cost of the subscriptions is roughly inverse in
the interval, which is what the sweep above shows, because the service compares
the whole process table with itself once per interval per subscription and there
are two of them. At the default two-second interval it is **11.4% of one core,
or 0.36% of this 32-processor machine**. On a four-core machine the same absolute
cost would be about 2.9% of the machine, which is most of SPEC.md section 38's
entire 3% budget for the recorder, and that is not acceptable — it is the reason
the default is not one second, and it is the reason for
[issue #230](https://github.com/wildware-uk/clipped/issues/230), which proposes
detecting exits by waiting on process handles instead of by a second
subscription.

Reporting this rather than a flattering summary is deliberate (AGENTS.md
section 19). "Negligible" would have been wrong by an order of magnitude.

#### Detection latency

Twenty runs at the default settings, with a `cmd.exe` started and ended by the
probe so that both moments are known exactly:

| | min | median | max |
| --- | --- | --- | --- |
| Process started → `WatchEvent::Launched` | 4.112 s | 4.142 s | 4.612 s |
| Process exited → `WatchEvent::Exited` | 2.134 s | 2.158 s | 2.282 s |

The launch figure includes the 2.5-second debounce; the source itself delivered
the creation event a median of 1.64 seconds after the process started. At a
one-second interval with a 1.5-second quiet period, the same twenty runs gave a
median launch latency of 2.330 s and exit latency of 1.004 s — one and a bit
seconds faster, for twice the standing cost.

Four seconds to notice a launch is comfortable for what this is for: a game
takes tens of seconds to reach anything worth recording, and the session layer
has only to be recording by then.

#### Taking the measurements again

```powershell
cargo run --release -p clipped-game-detection --example process_watch_probe -- latency 20
cargo run --release -p clipped-game-detection --example process_watch_probe -- idle 180
# and, for the WMI side, the counters named in the table above
```

A third argument overrides the notification interval in seconds, which is how
the sweep was taken.

### Testing it

| What | Where | Needs |
| --- | --- | --- |
| The debounce rules — launcher and game, re-exec, two games at once, a process that comes and goes inside the window, exit ordering, identifier reuse | `src/process_watcher/debounce.rs` | nothing; constructed process trees and an explicit clock |
| The process table, the executable name, the stop latch | `src/process_watcher/windows/` | Windows |
| That WMI answers at all, and that the fallback poller really reports a process starting and exiting | `src/process_watcher/windows/{wmi,mod}.rs` | Windows, and a working WMI service for the first |
| The whole watcher against a real process | `tests/process_watcher.rs` | Windows |

The split matters. "What did Windows say?" can only be answered by Windows and
has almost no logic in it; "is that one launch or three?" is all logic and needs
no machine at all, so it is a state machine over `(event, now)` and is tested
against process trees written down in the test rather than against whatever
happened to be running (AGENTS.md section 25).

The subject for the tests that do need a real process is `cmd.exe` with its
standard input piped and nothing else attached: it is on every Windows machine,
it starts immediately, and it exits when its input closes — so the test knows
the exact moment the process began and the exact moment it ended.

### Assumptions and limits

- **A process that starts in the gap between the baseline snapshot and the
  subscription is invisible** for the life of that watcher. The gap is tens of
  milliseconds. The other ordering was considered and is worse: it produces a
  spurious exit for a process that is still running.
- **Processes already running when the watcher starts are not launches.** They
  are remembered, so their exits are reported, and they are available through
  `ProcessWatcher::already_running`.
- **Executable paths are not always available.** Protected and higher-integrity
  processes — the anti-cheat services that sit alongside games, most system ones
  — refuse to be opened, and WMI reports a null `ExecutablePath` for them. The
  file name is always present.
- **An executable path is a user path.** It carries the account name and the
  shape of somebody's games library, so it reaches a log line only through
  `RedactedPath` (docs/logging.md).
- **Shutdown waits for the notification interval.** A thread blocked inside a
  COM call cannot be interrupted, so dropping the watcher takes up to
  `notification_interval` while that call returns.
