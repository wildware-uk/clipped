# Sessions and automatic recording

Clipped records games without being told to. This document covers the part that
decides *when*: what a session is, how a game launching becomes a recording,
what happens when one crashes, restarts, is joined by a second game or is
interrupted by the machine going to sleep, and where a session is written down.

**Status: this is built and running.** `clipped-recorder watch` records games as
they start and finalises the recording when they stop
([#46]). What is deliberately not built is named as such throughout, with the
issue that builds it.

## Which process watches

**`clipped-recorder serve --watch-for-games`**, in a shipped build, and
`clipped-recorder watch` at a terminal. They run the same loop over the same
session manager; the difference is what can reach the recordings it makes.

```text
serve --watch-for-games                    watch
  the launch watcher                         the launch watcher
  the control protocol   ◀── add_bookmark
  the global hotkeys     ◀── Ctrl+F9
```

`serve` is what the desktop supervisor starts, what `start-at-login` writes into
the `Run` key, and what registers the global hotkeys — so it is the only process
in which a bookmark, a screenshot or a stop can reach a recording nobody asked
for ([#421]). Before it gained the watcher, the two halves were in different
processes and the recordings a user is most likely to want to bookmark were the
ones nothing could bookmark.

Giving `watch` a control endpoint of its own was the alternative and was
rejected: [ADR 0009](adr/0009-the-recorder-registers-global-hotkeys.md) rests on
exactly one process registering the combinations, and it is the endpoint that
decides which one — a second recorder has already exited saying the pipe name
was taken before it could ask Windows for a key. Two recorders both watching for
games would be two processes both wanting them.

So `watch` stays as it is: it records, and nothing can reach what it records.
That is a terminal-facing command's honest shape, and it is why the flag exists
rather than `serve` always watching — a `serve` started by hand, or by a test,
must not begin recording whatever game happens to be running (AGENTS.md
section 25).

**What reaches an automatic recording.** The recording is handed to the same
state `start_recording` puts one in, from the moment the game's window is found
until the file is finished, so every command below is the one implementation
there has ever been of it (`apps/recorder/src/serve.rs`):

| Command | What it does to an automatic recording |
| --- | --- |
| `add_bookmark`, and `Ctrl`+`F9` | Marks the moment, in the recording's own bookmark file (`docs/bookmarks.md`) |
| `take_screenshot` | Writes a still from a frame the recording already captured |
| `stop_recording` | Finishes the file, and the sitting starts no further recording of a game that is still running — pressing stop is not undone five seconds later |
| `get_status` | Reports it, so the window shows what is being recorded and offers no control that would be refused |
| `save_replay` | Refused: an automatic recording keeps no replay buffer. Whether it should is [#427] |

[#37]: https://github.com/wildware-uk/clipped/issues/37
[#38]: https://github.com/wildware-uk/clipped/issues/38
[#41]: https://github.com/wildware-uk/clipped/issues/41
[#42]: https://github.com/wildware-uk/clipped/issues/42
[#46]: https://github.com/wildware-uk/clipped/issues/46
[#55]: https://github.com/wildware-uk/clipped/issues/55
[#240]: https://github.com/wildware-uk/clipped/issues/240
[#184]: https://github.com/wildware-uk/clipped/issues/184
[#241]: https://github.com/wildware-uk/clipped/issues/241
[#421]: https://github.com/wildware-uk/clipped/issues/421
[#427]: https://github.com/wildware-uk/clipped/issues/427
[#561]: https://github.com/wildware-uk/clipped/issues/561

## What a session is

**One sitting with one game.** It is not the same thing as a recording, and the
difference is the whole reason the model exists.

```text
Session  counter-strike-2-20260811-143205
  ├── recording 1   clipped-counter-strike-2-20260811-143205.mkv     18m 04s
  ├── recording 2   clipped-counter-strike-2-20260811-143205-2.mkv   41m 12s
  └── events        started, recording started, game exited, relaunched, …
```

Two files, one sitting. A player who alt-F4s and comes straight back has not
started a second evening; a game whose window is destroyed and recreated on a
resolution change has not either. A container cannot span a gap and there is one
encoder, so each of those produces a new file — and the session is what says the
files belong together.

A session groups **recordings, clips and events**. Recordings and events exist
and are modelled. Clips wait on [#38] — one can be *written* ([#37]) but no
build runs a recording with a buffer to write from — so there is no Rust type
for one and no invented data (AGENTS.md section 27).

**Bookmarks are not in this file.** They exist
([#64](https://github.com/wildware-uk/clipped/issues/64), `docs/bookmarks.md`)
and they belong to a *recording* rather than to a session: a bookmark is an
offset into one file, so it lives in that file's own sidecar beside it, which is
also the shape `clipped-storage`'s `bookmarks` table has. The `bookmarks` key
below is reserved and stays empty; a reader looking for a session's marks reads
the bookmark file of each of its recordings.

## Where the pieces are

```text
 clipped-game-detection            clipped-session::automatic         apps/recorder
 ──────────────────────            ──────────────────────────         ─────────────
 ProcessWatcher   what started ──▶ SessionManager                     watch
 Catalogue        is it a game? ─▶   the policy, and the session ──▶  resolve a window
                                     model around it              ◀── run the recording
                                                                      report the outcome
```

Three things already existed and nothing joined them: the watcher reports
launches and exits without polling ([#41]), the catalogue answers "which game is
this process?" ([#42]), and `clipped_session::record` records a window to a
playable file. `clipped_session::automatic` is the policy between them.

It is a **state machine over `(watcher event, wall-clock reading)`**. It opens no
window, starts no thread and touches nothing but the small file it writes for
each session. Every rule below is therefore a rule about timing and identity, and
is tested against constructed process trees on a clock the test supplies — a
suspend of eight hours is two `SystemTime` values, not eight hours of waiting.
`apps/recorder/src/watch.rs` is the driver that carries out what it decides,
because turning a process identifier into a window needs a desktop.

## The capture mode

**Full Session**, and only that: start when the game starts, stop when it exits,
keep everything (SPEC.md section 7).

The other three modes are not offered rather than offered and doing nothing.
Match Recording needs an integration that can say when a match begins, which is
the highlight provider API in M9. Highlights Only and Manual/Replay Buffer need a
replay buffer a clip can be *saved* from: the buffer exists, fills from the same
encoder and can be written out as a clip (`docs/replay-buffer.md`, [#37]). The
command exists ([#38]) and the hotkey now reaches an automatic recording ([#421])
— what is still missing is a *buffer* on one. An automatic recording keeps none,
because a rolling window costs about 140 MiB a minute and nothing has decided to
spend it on every game somebody launches; that is [#427]. So `save_replay` is
refused during an automatic session, in the recorder's own words, rather than
being a key that quietly does nothing.

## How a launch becomes a recording

1. The watcher reports a launch — the whole chain, with a launcher, the game and
   any wrapper between them collapsed into one thing that started ([#41]).
2. The session manager tests each member against the catalogue, **newest first**.
   A process cannot start before its parent, so the last member to start is the
   best single guess at what was launched, and searching backwards from it finds
   the game rather than the launcher that started it.
3. If nothing matches, nothing happens. That is almost every launch on a Windows
   machine, and it is logged at `debug` because the volume would otherwise
   dominate the diagnostics.
4. If something matches, a session opens and a recording is asked for — with the
   settings resolved for *that game* at that moment, over the global ones
   ("What a recording is made with", below).
5. The driver waits for the game to put a capturable window on screen —
   `--window-timeout`, two minutes by default. A launch is reported a few seconds
   after the process starts and a game can take much longer than that to reach a
   window while it compiles shaders, so giving up at the first look would mean
   recording almost nothing.
6. `clipped_session::record` runs on a thread of its own until the game's window
   goes or the manager raises the stop signal, and finalises the file on every
   path out.
7. Alongside it, `clipped_session::plugins` runs the highlight plugins for that
   game on a third thread — below.

## The plugins a recording runs

Each recording gets a `clipped_session::plugins::SessionPlugins`
([#338](https://github.com/wildware-uk/clipped/issues/338)), which owns the
`clipped_plugins::PluginSupervisor`, starts the plugins whose manifest claims
the process being recorded, polls it once a second, drains the events it
produces and stops everything when the recording ends
([docs/plugin-api.md](plugin-api.md)).

Three facts about it are worth having in one place:

- **It is a third thread, and neither of the other two.** The recording thread
  may not wait on a plugin (AGENTS.md section 20), and neither may the loop
  waiting on the process watcher — it has a game exiting to notice. The only
  thing a recording gives a plugin is the instant its first frame fixed the
  capture epoch, which is one `OnceLock` store (`RecordingProgress`).
- **Plugins start with the recording's first frame, not with the game.** A
  plugin says how long *ago* something happened and the host owns the timeline
  it is placed on, so there has to be a timeline first. A recording that never
  captured a frame — no window appeared — starts no plugin, and a session with
  two recordings in it runs its plugins twice, once per file.
- **Only what the settings file enables starts.** The `plugins` section records
  which plugins the user turned on and the consent token each was enabled with
  ([#282](https://github.com/wildware-uk/clipped/issues/282)); a plugin it does
  not mention is off, and one whose declaration no longer matches its token is
  refused and reported rather than started. `watch` reads the plugins
  directory, says what is installed and what was refused, and names any
  installed plugin that claims the game it is about to record as one it is not
  starting, with the reason. [docs/privacy.md](privacy.md) is why enabling one uninvited is not
  an option. What a plugin reports is likewise handed over rather than kept:
  storing events against the recording is
  [#71](https://github.com/wildware-uk/clipped/issues/71).

## The decisions, and why each one is what it is

### A tie in the catalogue is recorded, and left unattributed

`Catalogue::match_process` can answer `Ambiguous`: several entries claim the
executable equally well, and the catalogue deliberately does not guess between
them (`docs/game-detection.md`). Both obvious responses are wrong. Not recording
loses footage that cannot be made again; guessing files somebody's session under
the wrong game, silently, and the file is simply named after a game they were
not playing.

So the session is recorded and filed under `unattributed`, and **every candidate
is written into its record**. The footage is safe, no claim is made that was not
earned, and a person — or M6's library — has exactly what they need to say which
game it was.

### Killing the game finalises the recording

A crash is not a special case. The process vanishes, the window goes with it,
capture ends, and `clipped_session` flushes the encoder and writes the
container's trailer on its way out (AGENTS.md section 17,
[ADR 0001](adr/0001-mkv-archival-container.md)). The watcher reports the process
exiting a moment later, and the manager raises the stop signal if the recording
somehow has not ended already.

In practice the window is gone before the watcher notices the process is, which
is why both paths exist and why the file is finalised by the first of them.

### A fast restart is one session, two recordings

The session stays open for **60 seconds** after the game's last process exits.
The same game launching inside that window rejoins the session and its recording
becomes recording 2. A relaunch after it, or of a different game, is a new
session.

Getting this wrong is costly in both directions. Too short and a library is
fragmented by somebody restarting a game that crashed on load; too long and a
session spans a gap the user thinks ended. A minute is comfortably longer than
alt-F4-and-back and comfortably shorter than a break.

### Known child processes hold a session open, and are never recorded

A catalogue entry can list a game's known helpers. The catalogue is explicit that
they are **not match keys**: a crash handler is not the game, and treating the
list as a way in would make every anti-cheat service a reason to start recording.

Here they mean one thing. The 60-second grace does not begin while a process
named in the entry, from the same launch, is still running. That is the case
where a launcher exits and hands over, or where a game quits to a helper that
lingers, and it stops the sitting being split in two. They are never a capture
target, so the crash reporter is not what gets recorded.

Only processes that were part of the launch that started the session count. A
helper started much later is a launch of its own, matches nothing, and is
ignored.

### A second game during a session is noted, not recorded

There is one encoder and one capture target. The session in progress keeps them:
pre-empting would truncate a recording the user is in the middle of, and a game
starting while you are playing another is very often a launcher or a companion
application rather than a change of mind.

The launch is written into the active session's events and remembered. When that
session ends, **the most recently deferred game that is still running** becomes a
session of its own and is recorded from that moment — not from its launch, which
may have been an hour earlier. One that exited while it waited is forgotten.

A game that is waiting is still watched, and so are its helpers. The watcher
reports a process as gone exactly once, so an exit that arrives while a game is
deferred is its only chance to be counted: a helper not accounted for then would
be promoted along with the game as a live child that is already dead and can
never be seen to die. The session it belonged to would never run out of live
processes, never begin its grace period and never end — and because a session
that never ends defers every game after it, the recorder would keep running,
report nothing wrong, and record nothing ever again. That is the worst outcome
in this document, and it is why exits are folded into everything the manager is
tracking rather than only into the session that happens to be open.

### A suspend ends the recording rather than putting a hole in it

The manager is driven by a loop that calls it at least once a second, so a
wall-clock jump of **90 seconds or more** means the machine slept. A file whose
timestamps span eight hours of nothing is not a recording anybody can use: it
reports an eight-hour duration, lays out on an editor's timeline at eight hours,
and holds five minutes of pictures.

So on a resume the recording is finished, and if the game is still running
another begins in the same session. A session that was inside its restart grace
is closed outright — hours went by, not seconds, and a game launched after a
resume must not be joined to yesterday's session.

This is inferred from the clock rather than from `WM_POWERBROADCAST`, which would
give warning *before* the suspend and let the file be finished first. That needs
a message loop this crate does not have, and it is [#240].

### A game already running when the recorder starts is not recorded

Full Session means "start when the game starts". Joining halfway would produce a
session that began at an arbitrary moment and a file that starts in the middle of
whatever the player was doing.

`watch` says so on start-up, by name, rather than leaving somebody to conclude
the recorder is broken:

```text
Clipped Video Pattern is already running, so it is not being recorded.
Automatic recording starts when a game launches.
```

### Recordings within a session are bounded

A recording that ends while the game is still running is followed by another,
after a delay, which is what carries a session across a window being destroyed
and recreated. A game that somehow ended every recording immediately would spin,
so a session starts at most **100** recordings and says so when it stops.

The delay is **5 seconds**, and it is a race rather than politeness: a recording
ends the instant the window goes, and the process exiting reaches the manager
through the watcher up to `notification_interval + exit_settle_period` — three
seconds with the shipped configuration — later. Restarting sooner would have the
recorder go looking for the window of a game it does not yet know has quit, on
every ordinary exit.

**A recording that ended because its target changed size does not wait.** There
is no exit to race: the window is on screen, drawing, at a new size, and the only
reason the file ended is that a Matroska track's dimensions and an encoder
session's resolution are both fixed for the length of one file
([ADR 0012](adr/0012-a-session-follows-a-resize-with-a-new-file.md), [#184]).
Waiting the delay out there would spend five seconds of a game somebody is still
playing on every dragged window edge and every resolution change, which is the
opposite of what following a resize with a new file is for. Every other ending
keeps the delay, including the ones that look similar: a window that was *lost*
is the exit race itself, a suspend's exits arrive as a batch afterwards, and a
recording that failed or found no window says nothing about whether the process
is still there.

Two consequences of that are worth knowing. Every resize spends one of the
hundred recordings below, so a window dragged repeatedly can reach the cap; and a
window resized to a client area with an **odd** dimension cannot be encoded at
all, so the recording that follows such a resize fails to open and the sitting
records nothing more ([#561]).

A recording that found **no window at all** is not retried for the same process.
The window timeout has already given the game its chance, and retrying would have
the driver searching the desktop for a process that has none for as long as it
ran.

## Settings

| Setting | Default | What it decides |
| --- | --- | --- |
| `restart_grace` | 60s | How long a session waits for the same game to come back |
| `suspend_gap` | 90s | How large a wall-clock jump is read as the machine having slept |
| `recording_restart_delay` | 5s | How long before recording the same game again |
| `max_recordings_per_session` | 100 | The loop guard on recordings in one session |
| `--window-timeout` | 120s | How long a game may take to put a window on screen |

Only the last is a command-line option. The other four are
`clipped_session::automatic::AutomaticSettings`, with the defaults above, and are
not exposed to a user yet because the place a user would set them is the settings
screen (M5) and the per-game overrides (M7).

### What a recording is made with

Those four are how the *session* behaves. What the recording itself is made with
— the size, the frame rate, the codec, the encoder, the two audio selections —
comes from the user's settings, resolved for the game that launched
(`docs/configuration.md`):

```text
default          →  global  →  this game
```

The manager resolves them **once, at the moment it asks for a recording**, and
hands the answer over on `RecordingRequest::settings`. Nothing re-reads it: a
settings change while a game is being recorded applies to the next recording,
not to the encoder that is running. A game the user has set nothing for
inherits the global settings, which is not a special case but what a layer
saying nothing means; a session filed under `unattributed` — the catalogue
tied — is recorded at the global settings rather than at one candidate's.

A setting that this machine cannot honour does not cost the session. A
configured encoder that will not open is followed by the ranked ones, and a
configured size the capture is not producing is recorded at the size it is
captured at; both are logged at `warn` and both appear in the session's record.
A recording asked for by hand — `clipped-recorder record --encoder nvenc` — is
still refused, for the reason `docs/configuration.md` gives.

**One link is missing.** `apps/recorder/src/watch.rs` does not yet hand the
session manager a `ConfigurationStore` or apply what it is given, so on a
shipped build today every automatic recording is still made with the settings
`watch`'s command line was given. It is the rest of
[#61](https://github.com/wildware-uk/clipped/issues/61). A catalogue entry's own
`default_settings` remain uninterpreted, which is a fourth layer and
[#247](https://github.com/wildware-uk/clipped/issues/247).

## Where a session is written down

**M6's [#55] owns the real store**: the SQLite library index that makes sessions
searchable, joins them to clips and survives being asked about a thousand of
them. Nothing here is a second attempt at it (AGENTS.md section 55).

What exists today is the minimum the recorder needs so that "which game was this,
and which of these files belong together" is not held only in the memory of a
process that is expected to be killed (AGENTS.md section 17): **one JSON sidecar
per session**, written beside the recordings and named after the session. It is
rewritten whenever the session changes — to a temporary file and then renamed
over the previous one, so a recorder killed mid-write does not leave a truncated
record where the session's own history used to be.

```text
D:\clips\
    clipped-counter-strike-2-20260811-143205.session.json
    clipped-counter-strike-2-20260811-143205.mkv
    clipped-counter-strike-2-20260811-143205-2.mkv
```

A session's identifier is `<game_id>-<yyyymmdd>-<hhmmss>` in local time, matching
the form `clipped-recorder record` already names its own files in. The first
recording of a session takes the plain name, so the overwhelmingly common case —
one sitting, one file — produces a file named after the session and nothing else.

### The file

```json
{
  "schema_version": 2,
  "session_id": "counter-strike-2-20260811-143205",
  "game": {
    "kind": "known",
    "game_id": "counter-strike-2",
    "name": "Counter-Strike 2"
  },
  "started_at": "2026-08-11T14:32:05+01:00",
  "ended_at": "2026-08-11T15:31:21+01:00",
  "recordings": [
    {
      "index": 1,
      "output": "D:\\clips\\clipped-counter-strike-2-20260811-143205.mkv",
      "started_at": "2026-08-11T14:32:09+01:00",
      "ended_at": "2026-08-11T14:50:13+01:00",
      "outcome": "recorded",
      "frames_encoded": 65040,
      "duration_seconds": 1084.0,
      "starts_at_nanos": 0,
      "width": 2560,
      "height": 1440,
      "end_reason": "target-lost",
      "settings": {
        "capture_target": { "value": "game-window", "source": "default" },
        "resolution": { "value": "2560x1440", "source": "game" },
        "framerate": { "value": "60", "source": "global" },
        "codec": { "value": "auto", "source": "default" },
        "encoder": { "value": "auto", "source": "default" },
        "microphone": { "value": "default", "source": "default" },
        "system_audio": { "value": "default", "source": "default" },
        "replay_window_seconds": { "value": "300", "source": "default" }
      }
    }
  ],
  "clips": [
    {
      "path": "D:\\clips\\clipped-counter-strike-2-20260811-143205-replay-1.mkv",
      "created_at": "2026-08-11T14:41:52+01:00",
      "source_recording": 1,
      "source_start_seconds": 553.017,
      "source_end_seconds": 583.0,
      "duration_seconds": 29.983,
      "requested_seconds": 30.0,
      "complete": true
    }
  ],
  "bookmarks": [],
  "events": [
    { "at": "2026-08-11T14:32:05+01:00", "event": "session-started", "pid": 4242, "image_name": "cs2.exe" },
    { "at": "2026-08-11T14:32:09+01:00", "event": "recording-started", "index": 1, "output": "…" },
    { "at": "2026-08-11T14:50:13+01:00", "event": "recording-ended", "index": 1, "outcome": "recorded" },
    { "at": "2026-08-11T15:31:21+01:00", "event": "session-ended", "reason": "game-exited" }
  ],
  "game_events": [
    {
      "schema": 1,
      "kind": "kill",
      "at": 137000000000,
      "precision": 0,
      "latency": 250000000,
      "source": "cs2",
      "confidence": 1.0,
      "data": { "weapon": "ak47", "headshot": true }
    }
  ]
}
```

`settings` is what that recording was made with, and where each answer came
from: the value in the words `settings.json` uses, so the two can be read
against each other, and the layer that supplied it, because "this game
overrode it" and "it followed the global settings" are different things to go
and change. It is per recording rather than per session because that is where
the answer can differ — a session that spans a settings change holds one
recording made at the old settings and one at the new.

`starts_at_nanos` is where that recording begins on the **session's** timeline,
whose zero is the first video frame the session kept. The first recording starts
at zero by definition; a second one — a window destroyed and recreated, a game
relaunched inside its restart grace — starts wherever it began relative to that
same origin, never back at zero. With `duration_seconds` it is the span the file
covers, and a span is what turns a moment on the session's timeline into a
position in *this* file, which is how a kill ends up drawn on the right second
of the right recording ([#71](https://github.com/wildware-uk/clipped/issues/71),
[docs/av-sync.md](av-sync.md)).

It is absent for a recording that produced no frame: there is no timeline for it
to start on, and no moment it can cover. An event that falls in such a gap
belongs to the session and to no file, which the library stores as a null
`recording_id` rather than as the nearest guess.

Both keys were added after the schema shipped and the `schema_version` is
deliberately unchanged: every other field means exactly what it did, and a
reader that does not know the key ignores it. A sidecar written by an older
build has no `settings` on its recordings, which is not the same as a recording
made at the defaults.

`clips` is **one entry per replay saved out of this session's recordings**
([#38]). A save takes the last N seconds out of the recording's replay buffer
and writes a shorter file beside it, and this is where the session says what
that file is and which part of which recording it came from:

- `source_recording` is a recording's `index` in the list above, and
  `source_start_seconds`/`source_end_seconds` are offsets into *that recording's
  own timeline* rather than wall-clock times, so they still mean something after
  the folder has been moved to another drive. They are the two columns
  `clipped-storage`'s `clips` table already has for exactly this.
- `requested_seconds` and `complete` are what the library does not store and a
  person reading the file wants. A clip is bought at keyframe granularity, so it
  is usually slightly longer at the front than was asked for; and a buffer that
  had not filled yet gives less, which is what `complete: false` records. "I
  pressed the key for thirty seconds and got twelve" has an answer here
  (`docs/replay-buffer.md`).

### A clip with no file

`path` is **absent** for a clip nothing has exported yet — a generated
highlight is a range of a recording and costs no disk until somebody asks for a
file (SPEC.md sections 19, 20 and 44). A saved replay has one from the moment it
is written, because the packets it is made of were about to be evicted from
memory. Absent and empty are not the same thing: a `path` that is present and
blank is a malformed record and is still refused.

Such a clip carries three keys instead:

- `edit` is what the clip *is*, as `clipped_edit::EditDocument::write` wrote it.
  The library stores the text without interpreting it.
- `origin` is `manual`, `replay-buffer` or `highlight`. **Absent means
  `replay-buffer`**, because until a clip with no file could be stored that was
  the only kind there was.
- `origin_detail` is the rest of the serialised origin: for a highlight,
  `{"kind":…,"at":…,"source":…}` — what happened, when, and which plugin said
  so.

`origin_detail` is not only description. **It is what identifies the clip.** A
clip with a file is identified by that file, which is why `clips.path` is
unique; a clip with none has nothing else unique about it, so the library
matches one on its session and its cause. Two highlights of one session cannot
share both, because the generator refuses to cut a second clip over a window it
has already taken. Getting that wrong duplicates every generated clip on every
reconciliation — which is why `crates/library/tests/reconciliation.rs` indexes
the same session twice and counts.

The key was reserved from the first version of this schema and is filled now, so
the `schema_version` is unchanged: a reader that ignores it loses the clips and
still indexes the sitting. A session that saved none writes an empty list, as
every session did before.

`bookmarks` is **still always empty**. It is written so that a reader can tell
"no bookmarks" from "a file that predates them", and its presence is not a claim
that a session has none: a session's bookmarks are in its recordings' own files
(`docs/bookmarks.md`). Bookmarks *are* taken during an automatic session now —
`serve --watch-for-games` is one process with the watcher, the protocol and the
hotkeys in it ([#421], and "Which process watches" above) — and each one is
written into the bookmark file of the recording it is in, which is where a reader
looks for it. A sitting recorded by `clipped-recorder watch` still has none,
because nothing can reach a recording that command makes.

An ambiguous session writes its candidates instead of a name it did not earn:

```json
"game": { "kind": "ambiguous", "candidates": ["half-life-2", "team-fortress-2"] }
```

A session nobody asked the catalogue about writes neither:

```json
"game": { "kind": "unidentified" }
```

`kind` is an **open vocabulary**: `known`, `ambiguous` and `unidentified` today,
and a kind added later does not change `schema_version`. That is a promise the
reader keeps rather than a hope — `clipped-library` files a `kind` it has never
met as unattributed and reports it, instead of refusing the whole sitting over
one word it could not interpret.

`event` values are `session-started`, `recording-started`, `recording-ended`,
`replay-saved`, `game-exited`, `game-relaunched`, `another-game-started`,
`system-resumed`, `recording-limit-reached` and `session-ended`. A
`replay-saved` carries the `index` of the recording it came out of and the
`output` it was written to — the same clip `clips` describes, in the session's
history, so that "what happened during this sitting" reads in order. These are events about the
*session*; game events — a kill, a round starting — are a different vocabulary
entirely, they come from plugins, and they are M9's `clipped-events`.

A sidecar that cannot be written is a warning and nothing else. The video is what
cannot be made again, and a metadata failure must not cost a recording
(AGENTS.md section 17).

## A session somebody asked for

Everything above is about a session a *game launch* produced. There is a second
way one starts, and it produces the same record: somebody opens Clipped, picks a
window and presses record. The window sends `start_recording` and `serve` opens a
session for it ([#402]).

```text
 clipped-session::automatic
 ──────────────────────────
 SessionManager   a game launched      ──┐
                                         ├─▶ identify_process ──▶ Session ──▶ sidecar::write
 ManualSession    somebody pressed record ┘
```

**One session, one recording, and it ends when that recording does.** There is no
restart grace, no suspend rule and no deferral, because none of them can apply:
the sitting is the recording. Its `session-ended` reason is `recording-ended`,
which is a fourth value of that vocabulary and therefore a migration in
`clipped-storage` (`docs/storage.md`).

**Its game comes from the catalogue, exactly as an automatic session's does**
([#403]). The person chose the window and that settles whether to record; what
they recorded is a question only the catalogue can answer, so `serve` asks it
about the window's process — the executable's file name, and its full path where
Windows will say — through the same `identify_process` the session manager uses.
The three answers mean what they mean everywhere else:

| The catalogue says | The session's game | Filed under |
| --- | --- | --- |
| one entry claims the process | `known` | that `game_id` |
| several entries tie | `ambiguous`, with the candidates | `unattributed` |
| nothing claims it | `unidentified` | `unattributed` |

A window that is not a game — a browser, an editor, a game nobody has
catalogued — is still recorded and its sitting is still `unidentified`, because
that is the honest answer and a guess would be worse than none (AGENTS.md
section 27). **A game the user excluded stays excluded here too**: an exclusion
is a decision about a game rather than about a subcommand, so the recording
happens — they asked for it — and it is not filed under the game they told
Clipped to leave alone. A rename they made is the name that is recorded.

**A catalogue that cannot be read does not stop a recording.** `watch` refuses
to start without one, because it has nothing to do without one; `serve` reports
the failure, names the file and carries on with an empty catalogue, so every
sitting made until it is fixed is `unattributed`. The shipped data is
deliberately *not* used as the fallback: the failure is almost always in the
user's own overlay, which is where their exclusions and renames live, and
falling back to what Clipped ships would file recordings under games they
excluded.

**Its settings are resolved through the same fold.** `serve` reads the user's
settings file exactly as `watch` does and lays what the user configured over what
the request asked for, so a frame rate somebody set applies to a recording they
started from the window, and the session's record says where each answer came
from. Since the session has a game, it resolves *that game's* layer — a
recording of Counter-Strike started from the window is made with the settings
for Counter-Strike, however it was started — and a session with no `known` game
resolves the global layer, which is not a special case but what "this game has no
overrides" means.

The two records are compared field for field by a test rather than described as
similar: `clipped_session::automatic`'s
`a_session_somebody_asked_for_is_written_exactly_as_a_session_a_game_produced`
records one process both ways and asserts the two files are identical once the
identifier and the end reason — the only two things that *can* differ — are
replaced. The game is no longer one of them.

[#402]: https://github.com/wildware-uk/clipped/issues/402
[#403]: https://github.com/wildware-uk/clipped/issues/403

## When part of the pipeline fails

SPEC.md section 35 and AGENTS.md sections 16 and 17. The rule behind every row
below is one sentence: **the recording is the thing that cannot be made again**,
so the pipeline gives it up last and always says what became of it.

Three mechanisms carry most of it, and none of them is new to this section:

- **The container.** MKV with `AVFMT_FLAG_FLUSH_PACKETS` and a one-second
  cluster limit, so a recording killed mid-write is a playable recording of
  everything up to the kill ([ADR 0001](adr/0001-mkv-archival-container.md),
  [muxing.md](muxing.md), which measures exactly what is lost).
- **Finalisation on every path out.** `clipped_session::record` flushes the
  encoder and writes the trailer on a stop request, a closed window, an encoder
  failure, a full disk and a panic — the muxing thread finalises from `Drop` as
  well as from `finish`.
- **The disk guard.** A recording watches how much room is left where it is
  being written and stops itself *before* the drive fills, so that there is
  still room to finish the file properly. See below.

| What fails | What is kept | What the user is told | What they can do |
| --- | --- | --- | --- |
| The game crashes | Everything. The window goes first, so the recording has usually finished already | `end_reason=target-lost`, and the session ends after its restart grace | Nothing; a fast restart rejoins the same session |
| The disk fills | Everything. The recording is stopped at the reserve, with room to write the trailer | `end_reason=disk-space-low`, and a warning four times the reserve earlier | Free up space, or record to another drive |
| The output drive is unplugged | Whatever reached the drive before it went | `end_reason=output-unavailable` | Reconnect the drive; record to an internal one if it recurs |
| The GPU driver resets | Everything up to the reset, finalised | `graphics-device-lost`, naming the driver reset rather than "encoding stopped" | Record again — a new encoder opens on the recovered device |
| The encoder session table is full | Nothing was recorded; it failed to open | `encoder-unavailable`, naming the encoder | Close other recording or streaming applications; pick another encoder |
| An audio device is unplugged mid-recording | Everything, including the track: it becomes silence of the right length | A warning, and how much of the track was silence when the recording ends | Plug it back in; the capture picks it up again |
| An audio device cannot be opened at the start | Nothing was recorded; it failed before the file existed | `audio-unavailable`, naming the track | Connect the device, choose another, or record with that source turned off |
| The recorder process is killed | Everything up to the last closed cluster | Nothing at the time. `clipped-recorder recover` finds it on the next launch | Keep it or discard it — see below |
| The window changes size | Everything up to the change | `end_reason=target-resized`; a second recording follows in the same session, at once ([ADR 0012](adr/0012-a-session-follows-a-resize-with-a-new-file.md)) | Nothing |
| The machine sleeps | Everything up to the suspend | `system-resumed` in the session's events; a second recording follows | Nothing |
| A metadata write fails | The video, always | A warning; the session is in memory until the next change | Nothing |

Two of the audio failures SPEC.md section 35 lists — a device removed and a
microphone changed — are handled a layer down rather than by anything in this
table, and the answer is the same for both: the track becomes silence of exactly
the right length and the capture keeps looking for the device, because a
recording in progress is worth more than the audio it is missing
(`docs/audio-routing.md`, AGENTS.md sections 16 and 17). Nothing about the
session ends, and the recording's report says afterwards how much of each track
was synthesised silence. A device that cannot be opened when the recording
*starts* is a different matter and does fail it: `audio-unavailable`, before any
file exists, because a recording made silently without the microphone somebody
asked for is one they find out about when it cannot be made again.

The remaining one, an HDR change, is **not** covered here and is not claimed to
be: a high dynamic range capture is refused before it starts with a message
naming [#99](https://github.com/wildware-uk/clipped/issues/99). Capture
failures — a display disconnected, a backend that stops producing frames —
surface as `capture-lost` with whatever `clipped-capture` said, and fallback
between backends is [#97](https://github.com/wildware-uk/clipped/issues/97).

### The disk guard

The most likely real-world failure, and the one where doing nothing is actively
destructive. A recorder that simply writes until the disk is full does not end
with a slightly shorter recording: the writes start failing, and then the
*trailer* write fails too, so the file loses its segment length, its duration and
its cue index.

So a recording holds itself to a floor and stops before it gets there.

```text
free space          verdict     what happens
──────────          ───────     ────────────
> 4 × reserve       ample       nothing
> reserve           low         one warning, and the recording carries on
≤ reserve           exhausted   the recording is finished, cleanly, now
unreadable          —           the drive has gone; the recording is closed
```

The reserve defaults to **1 GiB**, which is about four minutes of 1080p60 at the
bit rate a recording is given — long enough for somebody who is told mid-game to
finish what they are doing, small enough not to make a half-full drive unusable.
`RecordingSettings::with_minimum_free_space` moves it, and **zero turns the guard
off** for a caller that would rather fill the disk than lose the tail of a
recording.

The same floor refuses a recording *before* it starts, because a recording that
opens and is stopped four seconds later by the same floor looks like a bug rather
than a full disk. A volume whose free space cannot be read is not refused: the
recording is about to try to create a file on it, and that failure says something
far more specific.

Where the measurement happens matters. Reading a volume's free space is a
filesystem call and the capture thread may not make one (AGENTS.md section 20),
so the **writer thread** — which already owns the file — asks at most once every
two seconds and publishes the answer as one atomic byte that the capture loop
reads between frames. A recording with the guard turned off makes no extra
filesystem call at all.

### What a failure says

Every failure a recording can have is turned into the same shape by
`clipped_session::failure` (AGENTS.md section 45): a headline with no error codes
in it, a sentence saying what became of the footage, at least one action, and the
technical words kept but demoted.

```text
D: filled up while recording
Everything recorded before this was finished and plays: D:\clips\clipped-…​.mkv
  - Free up space on D:
  - Record to a drive with more room
  FFmpeg failed while writing a packet: No space left on device (-28)
```

Two conditions that arrive as the *same* error are deliberately told apart there,
by the error number FFmpeg returned: `ENOSPC` is a full disk and `ENODEV` is an
unplugged drive, and they want opposite advice. Offering both for either is how
recovery advice becomes noise nobody reads.

### Recovering what a killed recorder left

A recorder that is killed — the process ended, the machine lost power — leaves
two things: a file that plays as far as the last cluster it closed, and a session
record whose entry for that recording says it began and never says it ended. The
footage is not lost. Nothing knows about it.

`clipped-recorder recover` is where somebody decides:

```text
clipped-recorder recover                                  list, and change nothing
clipped-recorder recover --adopt                          keep them
clipped-recorder recover --discard --session <ID>         move one to the trash, and say so
```

`watch` says the same thing at start-up, once, and does nothing about it —
start-up is also the only moment at which the question is unambiguous, because a
recording that is *running* looks exactly like one that was interrupted and there
is no lock file to tell them apart.

Adopting writes an `ended_at` and the `interrupted` outcome onto the entry and
appends a `recording-ended` event. **It does not touch the file**, and it does
not rewrite it: a recording without a trailer plays from the start and is
seekable only by scanning, and putting the index back means rewriting the
container, which `clipped-muxer` cannot do yet
([#283](https://github.com/wildware-uk/clipped/issues/283)). What adopting buys
is that the footage is *known* — named, sized, attributed, and indexed like any
other recording rather than looking like one still being written.

Discarding indexes the recording, moves it into `clipped-library`'s trash and
writes the `discarded` outcome — not `remove_file`, since [issue #451] found
that a delete a user has not necessarily watched yet was the wrong default
once a trash existed to catch it. It names one session, always: even a
recoverable choice is refused in bulk, so there is deliberately no way to
move everything at once (AGENTS.md section 56). The entry stays either way,
because the record that a recording existed and was discarded is worth more
than a gap. [recorder-cli.md](recorder-cli.md#recover) has what moving it to
the trash means in practice, and the order that keeps a failure partway
through from stranding it.

[issue #451]: https://github.com/wildware-uk/clipped/issues/451

Both are read-modify-write over the sidecar as JSON rather than through a typed
mirror of the schema, so a file written by a newer Clipped keeps its newer fields
when an older one recovers it (AGENTS.md sections 43 and 56).

### The words

The tokens the failure paths add to the vocabulary above, all of them written
into the sidecar and into logs:

| Field | Word | Means |
| --- | --- | --- |
| `recordings[].end_reason` | `disk-space-low` | Stopped deliberately at the reserve; the file is complete |
| `recordings[].end_reason` | `output-unavailable` | The output drive stopped answering |
| `recordings[].outcome` | `interrupted` | The recorder was killed; the footage was adopted afterwards |
| `recordings[].outcome` | `discarded` | The same, and the file was sent to the trash |

`clipped-library`'s indexer knows the two `outcome` words: `recover --discard`
indexes a recording before it moves it (issue #451), so the row it writes has
to survive a later reconciliation of the same, now-closed, sidecar, and
`crates/storage/migrations/0006_recovered_recording_outcomes.sql` widened
`recordings.outcome`'s vocabulary to match. The two `end_reason` words are the
gap that is left: it degrades gracefully — an unknown word becomes `NULL` and a
reported `IndexProblem` — and teaching it the rest is
[#278](https://github.com/wildware-uk/clipped/issues/278). The IPC protocol
carries the two end reasons as `EndReason::Other` for the same reason, and
[#284](https://github.com/wildware-uk/clipped/issues/284) promotes them.

## How to run it

```text
clipped-recorder watch
```

Recordings and session records go to `%USERPROFILE%\Videos\Clipped` unless
`--output-directory` says otherwise. Ctrl+C stops watching, finishing any
recording first. See [recorder-cli.md](recorder-cli.md) for the options.

The desktop application cannot drive this yet, and cannot see a session even when
the recorder is running one: the IPC protocol describes a recording by its
capture target and has no vocabulary for a game, a session or a recorder that is
watching. That is [#241].

## How to test it

The policy has no machine in it, so most of it is unit tests that run anywhere:

```text
cargo test -p clipped-session automatic
```

Those cover a crash, a fast restart, a relaunch after the grace, a known child
process holding a session open, a second game arriving and being recorded
afterwards, a deferred game's helper exiting before that game is promoted, a tie
in the catalogue, a suspend during a recording and during a grace period, the
recording cap, and shutting down — including shutting down while a recording is
already being stopped for another reason.

The failure paths are the same shape — thresholds and classification are pure
functions, and the one filesystem call is asked of a real volume:

```text
cargo test -p clipped-session disk failure recovery muxing
cargo test -p clipped-recorder --test recover_command
```

Those cover every band of the disk guard including a floor of zero and a floor
near `u64::MAX`; the pre-flight refusal against a real drive in both directions;
the guard's probe against a real volume and against one that is not there;
telling a full disk from an unplugged drive by FFmpeg's error number; every
`SessionError` having an action attached and keeping its technical detail; and
recovery — finding an interrupted recording and not a finished one, adopting it
without touching the file, discarding one and recording that it happened, a
damaged sidecar not hiding the recoverable footage beside it, and a field a newer
Clipped wrote surviving the rewrite.

What is **not** covered automatically is a drive genuinely filling underneath a
live recording. Doing that needs a small volume to fill and several seconds of
real capture, so it is a manual reproduction: create a VHD of a few gigabytes,
mount it, `clipped-recorder record --window <TITLE> --output <VHD>:\test.mkv`,
and copy files onto it until the reserve is crossed. The recording should stop by
itself with `Stopped because the drive was nearly full`, and `ffprobe` should
report a duration and a seekable file rather than a truncation.

The end-to-end tests need a GPU, an encoder and a desktop session, so they are
`#[ignore]`d:

```text
cargo test -p clipped-recorder --test automatic_sessions -- --ignored --nocapture --test-threads=1
```

They start the real recorder as a child process, launch `test-apps/video-pattern`
— once through a `cmd.exe` parent, so that the recorder records a process it did
not start and has no handle on — and validate the resulting file with
`clipped-media-validation`, asserting that it **decodes** rather than merely
opens. One of them sends a real Ctrl+C while a recording is still running, which
is the path on which the session has to survive being stopped from underneath. A
fourth uses a real process that never draws anything — the recorder's own Ctrl+C
fixture — to check what the console says when the search for a window gives up,
and that it never claimed a recording that did not happen.

The `cmd.exe` parent does **not** reliably prove that the debounce joins a
launcher and its game into one launch, and the test module says so at length:
whether the watcher reports `cmd.exe → video-pattern.exe` as one launch or as
two depends on the order WMI happens to deliver the two creation events in,
which is not ordered, and both orderings were observed on this machine. That
case is pinned deterministically where it can be — against a constructed launch
in `clipped_session::automatic`'s own tests, which assert that a
`[launcher.exe, game.exe]` group is recorded by its game and not by its
launcher.

The recorder is made to recognise the pattern through a **user overlay**, with
`LOCALAPPDATA` pointed at the test's own directory. `video-pattern.exe` is not in
the shipped catalogue and must not be: that file is compiled into every build,
and a test application in it would have Clipped recording a test application on
somebody's machine.

## Assumptions

- **The manager is polled at least once a second.** The suspend rule is stated
  against that promise, and `apps/recorder/src/watch.rs` keeps it.
- **One recording at a time.** There is one encoder and one capture target, and
  the manager never asks for a second.
- **A `game_id` is a legal file name.** The catalogue restricts it to `[a-z0-9-]`,
  which is what makes a session identifier safe to name a file after.
- **The watcher reports the exit of every process it reported.** Session lifetime
  depends on it; `clipped_game_detection`'s `ProcessExit` promises it.
