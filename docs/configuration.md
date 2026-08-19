# Configuration

**Status: a settings file changes what `clipped-recorder watch` and
`clipped-recorder serve` record, and nothing else reads one yet.**
`clipped_session::config` models the settings, resolves global and per-game
layers, validates them and reads and writes `settings.json`.
`clipped_session::automatic` uses it — every recording it asks for carries the
settings resolved for that game — and both long-running subcommands load the file
at start-up and apply what it holds to each recording they start: `watch` since
[issue #61](https://github.com/wildware-uk/clipped/issues/61) and `serve` since
[issue #402](https://github.com/wildware-uk/clipped/issues/402), through the same
call (["Applying a setting to a recording"](#applying-a-setting-to-a-recording)
below). `clipped-recorder record` takes its settings from its command line and
always will; that is what a command line is for. The Settings screen
([issue #51](https://github.com/wildware-uk/clipped/issues/51)) writes the file,
through `apply_settings` — the recorder is what saves it, because the window may
not open it — so it is no longer a file only a text editor changes.

What #61 still does not have is its evidence: two games recorded at two
resolutions and checked with `ffprobe`, an encoder substitution seen happening,
and the effective settings in diagnostics and session metadata. The issue lists
those as unmet.

This is stated first, and plainly, because a configuration API that looked as
though it were in force — or one that looked as though it were not — would be
worse than one that says exactly how far it reaches (AGENTS.md sections 27
and 54).

## The three layers

```text
default            60 fps       the value Clipped ships with
   |
global             60 fps       what the user set for everything
   |
counter-strike-2  120 fps       what the user set for one game
```

A layer that says nothing about a setting passes the one below it through
unchanged. That is the whole of AGENTS.md section 30's worked example: the
global layer says 60, Counter-Strike 2 says 120, and Minecraft — which says
nothing — records at 60.

### Unset is not the same as set to the same value

This is the distinction the whole design turns on, and the one thing to
understand before changing anything here.

|                      | Minecraft, frame rate unset | Counter-Strike 2, frame rate set to 60 |
| -------------------- | --------------------------- | -------------------------------------- |
| Resolves to today    | 60                          | 60                                     |
| Source               | `global`                    | `game`                                 |
| Reset offered        | no                          | yes                                    |
| Global changes to 90 | now records at 90           | still records at 60                    |

A per-game layer that stored the _effective_ value could not tell those two
apart, and the first change to the global settings would silently stop reaching
the games that were meant to follow it. So every field in a layer is an
`Option<T>`, `None` means "this layer says nothing", and the fold reports which
layer supplied the answer.

The same three-state model applies to hotkeys, where the states are _unset_
(follow the default), _bound to a combination_, and _deliberately unbound_ — see
[Hotkeys](#hotkeys).

## The API

Everything is reached through `clipped_session::config`. Configuration is not
read anywhere else: there is one fold, in `ResolvedSettings::fold`, and every
consumer goes through it.

```rust
use clipped_session::config::{Configuration, GameKey, Preferences, SettingSource};

let mut configuration = Configuration::defaults();

let mut global = Preferences::none();
global.set_framerate(Some(60))?;
configuration.set_global(global);

let mut counter_strike = Preferences::none();
counter_strike.set_framerate(Some(120))?;
configuration.set_game(GameKey::parse("counter-strike-2")?, counter_strike);

let minecraft = GameKey::parse("minecraft")?;
let resolved = configuration.resolve_for(&minecraft);

assert_eq!(resolved.framerate().get(), 60);
assert_eq!(resolved.framerate().source(), SettingSource::Global);
assert!(!resolved.framerate().is_overridden());
```

| Type                 | What it is                                                                                    |
| -------------------- | --------------------------------------------------------------------------------------------- |
| `Preferences`        | One layer: the global settings, or one game's. Every field optional, every setter validating. |
| `Configuration`      | The global layer, the per-game layers, and the hotkey layer. Valid by construction.           |
| `ConfigurationStore` | A `Configuration` with `settings.json` behind it. Loads, migrates, saves atomically.          |
| `ResolvedSettings`   | The answer for one scope: every setting, with its source.                                     |
| `Resolved<T>`        | One answer: `value()`, `source()`, `is_overridden()`.                                         |
| `Scope`              | Which layer a resolution was for — `Global`, or `Game(GameKey)`.                              |

### What a settings screen needs, and where it comes from

`Resolved<T>` carries three things because a screen needs three things.

- `value()` is what to show in the control.
- `source()` is `Default`, `Global` or `Game`, which is the "inherited from
  global" badge.
- `is_overridden()` is whether _this scope_ set it, which is what enables Reset.

`is_overridden` is asked against the scope, not against a fixed layer. On the
per-game page it means "this game set it"; on the global page it means "the
global settings set it, rather than the built-in default" — so Reset works on
both pages and means the same thing on both.

`ResolvedSettings::source_of(key)` and `is_overridden(key)` answer the same
questions without naming a setting's type, so the badge and the Reset control
can be drawn by a loop over `SettingKey::ALL`.

A screen also needs to change a setting without knowing its type, which is
`Preferences::set_written(key, value)`: the value is the text the settings file
spells that setting in — `120`, `hevc`, `name:Shure MV7` — and it is parsed by
the file reader's own parsers, so a value a screen can save is exactly a value
the file would accept, refused with the same message when it is not.
`SettingKey::choices()` is the closed set of values where there is one and empty
where the set is open, and `SettingKey::accepted()` is the sentence a refusal
would carry — which is how a screen draws a list of options for one setting and
a field for another without keeping a copy of either.

The window itself reaches none of this directly: it may link `clipped-ipc` and
nothing else of this workspace, so it asks the recorder — `get_settings`,
`apply_settings` and `get_audio_devices`, in `docs/ipc.md`. The recorder answers
with one entry per setting carrying exactly the fields above, plus whether
anything in that build acts on the setting, so that a screen never draws a
control for a key nothing acts on (AGENTS.md section 27).

That last field usually means "reads it when a recording starts", because the
recorder is what acts on a setting. The exception is
[Notifications](#notifications), whose reader is the window itself — the recorder
keeps those four and never looks at them.

## The settings

Exactly the settings this build can be told about, and no others. SPEC.md
section 31 lists more — capture mode, bitrate, event types, auto-clipping,
storage behaviour, HDR — and each of them arrives with the subsystem that reads
it. A setting for a subsystem that does not exist is a control that silently
does nothing (AGENTS.md section 27).

| Key                     | Type   | Default       | Accepts                                                            |
| ----------------------- | ------ | ------------- | ------------------------------------------------------------------ |
| `capture_target`        | text   | `game-window` | `game-window`, `display`                                           |
| `resolution`            | text   | `source`      | `source`, or a size such as `1920x1080`; both sides even, 128–7680 |
| `framerate`             | number | `60`          | 1–480                                                              |
| `codec`                 | text   | `auto`        | `auto`, `h264`, `hevc`, `av1`                                      |
| `encoder`               | text   | `auto`        | `auto`, `nvenc`, `amf`, `quicksync`, `software`                    |
| `microphone`            | text   | `default`     | `default`, `none`, or a device name of 1–256 characters            |
| `system_audio`          | text   | `default`     | `default`, `none`, or a device name of 1–256 characters            |
| `replay_window_seconds` | number | `300`         | `0` to keep no buffer, or 30–1800, whole seconds                   |

Where the library lives and what it may occupy are **not** in that table, and
not per game: they live in a `storage` section of their own, because a library
is one thing however many games are in it, and because SPEC.md section 31 —
which lists what a game may override — has neither on it. Nothing is limited unless one is set, which is what Clipped
ships with and why automatic cleanup deletes nothing on a machine nobody has
configured ([#111]).

| Key in `storage`           | Type   | Default                                             | Accepted                                                        |
| -------------------------- | ------ | --------------------------------------------------- | --------------------------------------------------------------- |
| `maximum_usage_bytes`      | number | none                                                | a gigabyte or more                                              |
| `minimum_free_space_bytes` | number | none                                                | any number of bytes                                             |
| `maximum_age_days`         | number | none                                                | one day or more, whole days                                     |
| `trash_directory`          | text   | beside the recordings                               | an absolute path, on the recordings' volume and not inside them |
| `recording_directory`      | text   | the `Clipped` folder of the user's videos directory | an absolute path                                                |

A limit outside those bounds is **refused**, and the file does not load: a quota
under a gigabyte can only be satisfied by a library with nothing in it, and a
maximum age under a day deletes footage recorded this afternoon. Both floors are
`clipped-library`'s own constants and the refusal is its own message. A key
inside `storage` that this build does not understand is kept and written back,
like every other unknown key.

`recording_directory` is where recordings are written, and it is **step 3 of the
MVP definition** (SPEC.md section 45): a fresh user picks a microphone and a
directory once, closes the window, and never configures capture again. Three
layers decide it, top down — the `--output`/`--output-directory` flag, then this
setting, then the videos folder Clipped would pick on its own — so a run somebody
typed a path into still goes where they said, and everything else goes where the
settings screen said ([#307]).

It must be absolute, and it must not be blank. The recorder is started by the
shell's `Run` key, with a working directory nobody chose, so a relative path
names somewhere different every time it starts; blank is refused separately,
because `""` is not a path anybody can be told to make absolute and because
clearing the setting is removing the key rather than writing an empty string.
Whether the directory exists, is writable, or has room is
**not** checked when the file is read: a settings file is read at start-up and a
drive can be unplugged after it, so the answer that matters is the one at the
moment a recording starts, and that is where it is reported.

`trash_directory` defaults to the recordings folder's path with `.trash`
appended — `D:\Clips` becomes `D:\Clips.trash`. Beside rather than inside,
because deletion is a rename and so must stay on the volume, and because a trash
_inside_ the recordings root would be counted as recordings by storage
accounting — which `StorageRoots` refuses outright.

### Seeing what a limit would do before you set one

`clipped-recorder storage` measures the library and prints what automatic
cleanup would take. It never deletes — there is no `--apply` — and it takes the
**same measurement the sweep does**, so it cannot disagree with what actually
happens: the sweep is that measurement plus the last step.

```text
Recordings  C:\Users\you\Videos\Clipped
Trash       C:\Users\you\Videos\Clipped.trash
Using       20.0 MB of the drive, with 162.4 GB free
  recordings 20.0 MB

Limits      none set, so nothing is ever deleted automatically.

Nothing would be deleted.
7 recording(s) are protected and would never be taken.

Largest recordings
      1.7 MB  2026-08-14  …\clipped-20260814-095157.mkv
```

It runs with no limit configured on purpose. "You have 400 GB of recordings and
no limit" is the most useful thing it can say to somebody deciding whether to
set one, and the list of the largest recordings is the review path [#111] asks
for: a chance to act before automatic deletion does.

[#111]: https://github.com/wildware-uk/clipped/issues/111
[#307]: https://github.com/wildware-uk/clipped/issues/307

The vocabulary is the command line's, deliberately: `--codec hevc` and
`"codec": "hevc"` mean the same thing, because a settings file and a command
line that disagreed about what an encoder is called would be two answers to one
question. `docs/recorder-cli.md` is the other half of that table.

Two of these are worth a note.

- **`resolution`** may name a size, and nothing in this build can produce a size
  other than the source's: there is no scaler in the capture-to-encoder path. A
  fixed size that does not match what capture produces is never quietly ignored
  — a recording asked for by hand is refused
  (`SessionError::ScalingNotSupported`) and a recording made from these settings
  is recorded at the source size with the substitution logged. ["What a stale
  setting does"](#what-a-stale-setting-does-and-why-it-is-not-what-a-flag-does)
  says why the two differ.
- **A device name** is matched against the endpoints present when a recording
  starts, so a headset that is unplugged is a failure the session reports rather
  than a settings file that has become invalid. To name a device that is
  genuinely called `default` or `none`, prefix it: `name:none`. Written files
  always use the prefix, so a device called `none` survives a round trip.

### Validation

Every setter validates, so a `Preferences` that exists is one whose values are
in range, and a `Configuration` that exists is valid. That is what makes "the
previous valid configuration is retained" a property of the type rather than a
discipline every caller has to keep.

"In range" means _the file can carry it and give it back unchanged_, not merely
that the Rust type can hold it. Two of the settings are wider than the file, and
the setters are where that is caught:

- `AudioDeviceSetting::Named` is a public variant, so a caller can build one
  without `AudioDeviceSetting::named`. `set_microphone` and `set_system_audio`
  apply the same rule — not blank, no control characters, at most 256 characters
  — because a name the writer renders as `"name:"` is one the reader refuses,
  and the user would be left with a settings file their own build cannot open.
- `replay_window_seconds` is whole seconds, so a `Duration` carrying part of one
  is refused rather than silently truncated by the writer.

`every_device_name_a_setter_accepts_survives_the_writer_and_the_reader` is that
property as a test: whatever a setter accepts is saved and read back equal.

Messages name the setting, the value and what would have been accepted:

```text
C:\Users\alice\AppData\Local\Clipped\settings.json cannot be used: in the
global settings, framerate is 0; it accepts 1-480 frames per second
```

The replay window is checked against `clipped_replay`'s own bounds, so the
refusal reaches the user when they set it rather than when a game launches.

### Declining the replay buffer, and what it saves

`replay_window_seconds` is the one setting with a value outside its own range:
**`0` means keep no replay buffer at all**, and it is there because every
recording the desktop window starts asks for one
([#539](https://github.com/wildware-uk/clipped/issues/539)). Until it existed the
nearest thing to "no" was a thirty-second buffer, which still spills.

What a buffer costs is disk rather than memory, and the figures are measured
([replay-buffer.md](replay-buffer.md)): about **0.94 MB of memory** whatever the
window is, and **208 MB on disk** for the half-hour maximum at 1080p60 — written
continuously into `%LOCALAPPDATA%\Clipped\replay\` at the recording's own
bitrate, for as long as the recording runs. That is a rolling figure rather than
a growing one, but the writes do not stop, and somebody on a small SSD or one
they are careful with may reasonably not want them. Anybody who never presses
Save Replay is paying for a feature they do not use.

`0` rather than `"none"`, which is what `microphone` and `system_audio` use for
the same idea: those are text keys whose whole value space is text, and a device
may genuinely be called "none". This key holds a number, and a value that made
its *type* depend on which answer it was would be worse to hand-edit, not
better — `0` is the reading of the number rather than a word to learn. It cannot
be confused with "unset" either, because unset is the key not being there: a
layer that says nothing writes no `replay_window_seconds` at all, which is how
inheritance tells "off" from "inherit" for this key exactly as it does for every
other one.

A number in between gets a refusal that offers both:

```text
C:\Users\alice\AppData\Local\Clipped\settings.json cannot be used: in the
global settings, replay_window_seconds is 5 seconds; it accepts 0 to keep no
replay buffer, or 30-1800 seconds
```

**What a recording with `0` actually keeps: nothing.** No `ReplayRecording` is
built, so no buffer is sized, no spill directory is made and no packet is copied
anywhere. `active_recording.replay_seconds` is `null` over the protocol, which is
a different answer from `Some(n)` with nothing in it yet, and the tray's Save
Replay is a refused item reading "this recording is not keeping a replay buffer"
rather than a live one that would do nothing (AGENTS.md section 27). It inherits
per game like everything else: a game may keep a buffer on a machine that
globally declines them, and may decline one on a machine that keeps them.

`clipped-recorder replay` is the one caller that refuses instead. Its whole
subject is a rolling window and a hotkey that saves from it, so it says so and
names the two ways on:

```text
replay_window_seconds is 0, so no replay buffer would be kept and the hotkey
would have nothing to save: pass --duration to keep one for this run, or use
`record` for a recording without one
```

## Notifications

Which failures interrupt the user, and they are **global only** for a reason of
their own: the thing being interrupted is a person rather than a recording, so
"should Counter-Strike's failures interrupt me" is the same question as "should
failures interrupt me". SPEC.md section 31's list of per-game overrides does not
mention notifications either.

Four switches, in a `notifications` section beside `hotkeys` and `storage`:

| Key                     | Default | What it is about                                                              |
| ----------------------- | ------- | ----------------------------------------------------------------------------- |
| `recording_failed`      | on      | A recording ended because something went wrong; the recorder is still running |
| `recording_interrupted` | on      | A recorder stopped mid-recording without being asked                          |
| `recorder_unavailable`  | on      | Nothing is being recorded and nothing further will be tried on its own        |
| `hotkey_unavailable`    | on      | Windows refused a combination, so pressing it does nothing                    |

```json
{
  "notifications": {
    "recording_failed": false
  }
}
```

Everything is on until somebody says otherwise, because all four are failures. A
key holding something that is not `true` or `false` is **kept and ignored**,
leaving that category on — which is the opposite of what the `storage` section
does with a value it cannot read, and deliberately. A limit that is quietly
ignored leaves somebody believing their library is capped when it is not, so it
is refused; a switch that is quietly ignored is a nuisance rather than a loss,
and refusing would mean a typo in a notification switch stopped the recording
settings in the same file from loading.

### The recorder keeps these and never reads them

The process that acts on them is the **desktop application**, at the moment it
decides whether to show a toast (`docs/desktop-ui.md`). It may link
`clipped-ipc` and nothing else of this workspace, so it cannot open this file —
it asks, with `get_settings` when its link attaches and `apply_settings` when
somebody moves a switch, exactly as it does for every other setting.

They are here rather than in a store of the window's own because that store
existed until [issue #252](https://github.com/wildware-uk/clipped/issues/252): a
`notifications.json` in `%APPDATA%\uk.wildware.clipped`, with a version field, a
missing-key policy and a reader of its own. Two files of user preferences in two
directories is the duplication AGENTS.md section 55 forbids. One of those files
is migrated into this one and deleted the first time a link attaches; the window
is what does that, because the window is the only process that knows where its
own configuration directory is.

## Hotkeys

Hotkeys are configuration, and they are **global only**. A per-game hotkey could
not be honoured: `clipped_hotkeys` registers a combination with Windows once,
for the process, and there is no per-foreground-application variant of that
registration — so a per-game hotkey override would be a control that silently
does nothing. SPEC.md section 31's list of per-game overrides does not include
hotkeys either.

Three states, for the same reason the recording settings have them:

| In the file                 | Means                                                                       |
| --------------------------- | --------------------------------------------------------------------------- |
| key absent                  | follow the default (`Ctrl+F10` for Save Replay, `Ctrl+F9` for Add Bookmark) |
| `"save_replay": "Ctrl+F10"` | pinned to that combination, whatever the default becomes                    |
| `"save_replay": null`       | deliberately unbound                                                        |

Two actions on one combination is refused when it is set, naming both actions —
including when the combination is one another action holds _by default_, because
that is the binding the user would find had stopped working. The other half of
conflict detection, a combination another application already owns, can only be
discovered by asking Windows and belongs to `clipped_hotkeys::HotkeyService`
(`docs/hotkeys.md`).

## The file

`%LOCALAPPDATA%\Clipped\settings.json` — the same per-user directory the logs,
the encoder capability cache and the game catalogue overlay live in
(`clipped_logging::application_directory`). There is no file until something is
saved: a user who has never changed a setting has no settings file, and reading
one that is not there is `Loaded::Absent` and the defaults, not an error.

```json
{
  "version": 1,
  "global": {
    "framerate": 60
  },
  "games": {
    "counter-strike-2": {
      "framerate": 120
    }
  },
  "hotkeys": {
    "save_replay": "Ctrl+F10"
  },
  "notifications": {
    "recording_failed": false
  },
  "plugins": {
    "acme.counter-strike-2": {
      "enabled": true,
      "consented_to": "loopback listen 127.0.0.1:3212"
    }
  },
  "capture": {
    "counter-strike-2": {
      "method": "desktop_duplication",
      "since": "2026-08-16T10:14:02+01:00"
    }
  }
}
```

Minecraft is not in that file, and that is what makes it inherit. A key present
with a `null` value means the same as an absent key — this layer says nothing —
which is what a settings screen writes when the user presses Reset.

JSON rather than TOML because `clipped-session` already writes JSON for a
session's sidecar (`docs/sessions.md`) and already depends on `serde_json`; a
second serialisation format in one crate would be a dependency and a set of
quoting rules bought for nothing.

### `capture`

The one section of this file the user did not write, and the only value in
Clipped's configuration that Clipped writes on their behalf. It records, per
game, the capture method a recording of that game was observed to end on, and
when that answer was established — so the next recording of the same game
_starts_ there instead of spending a second or two falling back to it again
(issue [#286](https://github.com/wildware-uk/clipped/issues/286)).

It is deliberately **not** a per-game setting in `games`, for three reasons:

- a settings screen would offer a Reset control for a choice the user never
  made, which is the control that silently means something else that AGENTS.md
  section 27 is about;
- `Configuration::set_game` replaces a game's layer with what the settings
  screen built, so a memory living inside it would be erased every time the user
  changed that game's frame rate;
- it does not inherit. "Windows Graphics Capture could not capture
  Counter-Strike here" says nothing about Minecraft, because whether a backend
  can capture a target depends on that target.

**It is a preference and never a pin.** It changes which capture candidate is
asked first and nothing else: a remembered method this machine can no longer
offer, or that fails to start, falls back exactly as if nothing had been
remembered, and it is ignored outright for a game whose method the user pinned
(`clipped_capture::CaptureFallback::start_preferring`). Being wrong about it
costs one fall back on one recording — which is what remembering nothing costs
on _every_ recording.

**It is forgotten after a fortnight.** Drivers update and hardware changes, and
a memory nothing ever revisits is a permanent downgrade bought with one bad
afternoon; after `clipped_session::config::MEMORY_LIFETIME` the published
preference order is tried afresh and whatever that recording ends on is
remembered anew. `since` is when the answer was last _established_ and not when
it was last confirmed — a game recorded every evening on the same method keeps
its original stamp, or the fortnight would never elapse. It is also cleared
outright by "forget this game" (`Configuration::clear_game`).

An entry this build cannot read is **dropped**, where an unreadable `plugins`
entry is kept and an unreadable `storage` limit refuses the whole file. The
difference is whose value it is: this section is Clipped's own note about a
machine and can be made again by recording the game once, so neither losing the
user's real settings over it nor writing back a note nothing can interpret is
worth doing.

### `plugins`

Which plugins the user enabled, and **what they agreed to when they did**
([#282](https://github.com/wildware-uk/clipped/issues/282)). It does not inherit
and has no per-game layer: a plugin already decides which games it supports from
its own manifest.

`consented_to` is the canonical rendering of the plugin's network declaration,
sorted so that reordering a manifest does not lapse consent and any real change
does. It is legible on purpose — a person reading their own settings can see
what they agreed to without running anything, which a hash could not give them.

It is compared with what the plugin declares _now_, every time one is started.
A plugin that updates and asks for a new endpoint no longer matches, so it is
not started and the reason is reported; the user is asked again rather than the
new access being granted on the strength of the old answer.

Three rules follow from that, and they are the whole of the section's
behaviour:

- **A plugin the file does not mention is off.** Absence is the safe answer:
  "we have no record of you enabling this" must never resolve to "so run it".
  It is also what makes a settings file written before this section existed read
  correctly rather than fail.
- **An entry marked enabled with no `consented_to` is not obeyed**, and is kept
  rather than deleted. Running it would grant access nobody examined; deleting
  it would throw away a record a newer build wrote.
- **Turning a plugin off keeps its token**, so turning it back on does not ask
  again for access that has not changed.

`docs/privacy.md` is why none of this can be skipped: every bundled plugin opens
a loopback socket, and the register on that page is only true if a deliberate
action is what starts one.

### Saving

A temporary file and a rename, so that a crash or a full disk leaves either the
previous settings or the new ones and never half of each (AGENTS.md section 17).
The directory is created if it is not there. Nothing else writes the file.

**Saving looks at the file before it replaces it, and refuses if this build
could not read it.** Refusing to _read_ a newer build's file preserves nothing on
its own: the user whose other machine is a version ahead opens the settings here,
sees the defaults, changes one thing, and the save is what destroys their file —
the same loss, arrived at one step later. So `ConfigurationStore::store` parses
what is on disk first and fails with `ConfigurationError::WouldOverwrite` rather
than replacing something it does not understand.

It checks the file rather than remembering what the last `load` found, because a
store that was never asked to load has no such memory, and because a file that
changed since the load — the other machine's sync client landed it — is exactly
the case worth catching. What is left is the window between that read and the
rename, which cross-process locking
([issue #194](https://github.com/wildware-uk/clipped/issues/194)) is what would
close.

The refusal has to leave the user somewhere to go (AGENTS.md section 45), so the
message says what is in the way and what to do about it:

```text
the settings were not saved: settings.json is settings version 2 and this
build understands up to 1; update Clipped, or move the file aside to start
again from the defaults. The file was left exactly as it is, because saving
over settings this build cannot read would destroy them; move it aside to
start again from the defaults
```

Moving it aside is a real recovery and not just advice —
`moving_the_unreadable_file_aside_is_the_recovery_the_message_promises` follows
it through to a successful save.

### Versions and migration

`version` is the format, and this build writes version 1.

| The file says    | What happens                                                                                  |
| ---------------- | --------------------------------------------------------------------------------------------- |
| no `version` key | version 0: migrated in memory, reported as `Loaded::Migrated { from: 0 }`                     |
| `1`              | read as written                                                                               |
| `2` or higher    | **refused**, and the file is left exactly as it was — by the next save as well as by the read |

A migration runs **in memory**. The file on disk is not touched until the user
next saves something, because rewriting a file for somebody who only wanted to
look at it is not a migration anybody asked for.

A file from a newer Clipped is never rewritten at this version. The likely
reason for one is a user with two machines, one ahead of the other, and
flattening the newer build's settings to what this one understands would destroy
exactly what AGENTS.md section 56 protects. The refusal says what to do:

```text
settings.json is settings version 2 and this build understands up to 1;
update Clipped, or move the file aside to start again from the defaults
```

#### Version 0

Version 0 is a document with no `version` key, in which the frame rate is
spelled `fps`. Clipped has never shipped a settings file, so no user has one;
what version 0 describes is the vocabulary that did exist before this module —
the game catalogue's `default_settings` table, whose worked example spells the
frame rate `fps`. Version 1 spells it `framerate`, the name `--framerate` and
`RecordingSettings::framerate` use.

It is a small migration on purpose. What it is really for is that the mechanism
— detect a version, migrate one step at a time, refuse to go backwards, keep
what is not understood — is written and tested before there is a user's file
depending on it being right. A document that sets _both_ spellings is refused
rather than guessed: choosing wrongly would record at the wrong rate for every
session that followed.

### Settings this build has never heard of

They are kept. Every section carries the keys it could not interpret and writes
them back out unchanged — at the top level, in `global`, in each game's section,
in `hotkeys`, in `storage` and in `notifications`.

The failure this prevents: a user configures something on the machine running
the newer Clipped, opens the settings on the older one, changes anything at all,
and silently loses it. Note that the caller is not asked to carry those keys —
it cannot, because it has never heard of them — so `set_global`, `set_game` and
`set_hotkeys` carry them over from the layer they replace. Only
`clear_game` drops them, and that is a thing the user has to have asked for.

## Failure, and what survives it

`ConfigurationStore` holds the last configuration it knows to be good. Every
failure leaves it standing and leaves the file alone — reading _and_ saving:

| Went wrong                                   | Configuration in force | The file                 | The next save                  |
| -------------------------------------------- | ---------------------- | ------------------------ | ------------------------------ |
| file absent                                  | the defaults           | not created              | writes it                      |
| not JSON                                     | unchanged              | untouched                | **refused**, `WouldOverwrite`  |
| a value out of range                         | unchanged              | untouched                | **refused**, `WouldOverwrite`  |
| two actions on one hotkey                    | unchanged              | untouched                | **refused**, `WouldOverwrite`  |
| version newer than this build                | unchanged              | untouched                | **refused**, `WouldOverwrite`  |
| both `fps` and `framerate` at version 0      | unchanged              | untouched                | **refused**, `WouldOverwrite`  |
| the file cannot be read at all (permissions) | unchanged              | untouched                | **refused**, `Read`            |
| the save itself failed                       | unchanged              | previous contents intact | writes it, if the disk lets it |

A user who hand-edits their settings into nonsense while Clipped is running keeps
recording with the settings they had, and their file stays as they left it until
they move it aside themselves. The last column is the half that matters: a
refusal to read that was followed by a save which overwrote anyway would preserve
nothing at all.

Note the deliberate asymmetry in the last two rows. A _content_ the reader cannot
understand means the file belongs to somebody — a newer build, or the user's own
hand — and this build does not get to replace it. A _write_ that failed says
nothing about what is in the file, so the next save simply tries again.

## Applying a setting to a recording

### Resolved once, when the recording starts

`SessionManager` reads the configuration at exactly one moment — the moment it
asks for a recording — and the answer travels with that recording as a value:

```text
a game launches
   |
SessionManager::begin_recording      resolve_for(game)  ← the only read
   |
SessionAction::StartRecording { …, settings }
   |
the driver:  settings.apply_to(RecordingSettings::new(target, output))
   |
clipped_session::record
```

Nothing re-reads it afterwards. A user who changes the frame rate while a game
is being recorded gets the new frame rate on the **next** recording:
`SessionManager::set_configuration` replaces what future recordings resolve
from and does not reach into one that is running. A setting changing under a
running encoder is a different feature, and it is not this one.

A session that spans a change therefore holds recordings made at different
settings, which is why each recording's own record carries its own answer
(`docs/sessions.md`).

### What calls `set_configuration`, and how often

The automatic recorder's loop, once a pass — `Driver::take_the_settings_the_user_saved`
in `apps/recorder/src/watch.rs`, before anything in that pass can ask the
manager for a recording.

It has to, and this is the part that was missing until issue #51. A recording
the window asks for reads `SettingsFile` at the moment it starts, so it always
gets the settings as last saved. The automatic recorder cannot: its
`SessionManager` **owns** the configuration it resolves per-game settings from,
and is handed a copy when the watcher thread starts. Nothing replaced that
copy, so a microphone saved from the Settings screen reached automatic
recordings only after the recorder was restarted — which SPEC.md section 45
rules out in as many words.

The loop does not take the settings lock every pass to find out. `SettingsFile`
keeps a generation counter, bumped under the lock by `apply` after a save
lands, and the driver compares one relaxed atomic load against the generation
it took its copy at. The lock is taken and the configuration cloned only on a
pass that follows a save. Nothing about this runs on a capture thread: it is
the watcher's loop, which spends its time waiting on process events, and a
recording's own thread never reads settings after it starts (AGENTS.md
section 20).

Read the generation **before** the configuration, never after. A save landing
between the two reads must leave the driver believing it is behind — costing
one redundant clone on the next pass — rather than believing it is current
with a configuration that predates the save, which would strand that setting
until somebody saved again.

What the whole `Configuration` is replaced with, rather than a global half laid
over per-game entries the driver had been carrying, so per-game overrides
survive the refresh. Plugin consents do not travel this path: they are cloned
out of the configuration *before* it moves into the manager, so that
`attach_plugins` never goes looking for a settings file, and they remain a
start-up snapshot (`docs/plugin-api.md`).

### The recording directory: between sittings, never during one

**Where** automatic recordings are written is not in the configuration
`set_configuration` replaces. It is resolved by `watch::recordings_directory`
before the watcher thread starts and held in the session manager's
`AutomaticSettings`. It travels the same pass and the same generation check —
one mechanism, not two — through
`Driver::take_the_directory_the_user_saved`, and it lands differently:

| What was saved | Reaches |
| --- | --- |
| any recording setting | the next **recording** |
| the recording directory | the next **sitting** |

A sitting is a sequence of recordings held together by one session record, and
`SessionManager::begin_recording` writes that record *next to the files it
names*. Moving the directory half way through would leave the record in one
folder and some of its own recordings in another — and the failure is silent,
because every file is still on disk and nothing is left able to say which
sitting they belonged to (AGENTS.md section 56). So
`SessionManager::set_recording_directory` holds the change, and
`close_active` takes it up after the ended sitting has been written where its
files are and handed over, and before any deferred game opens a sitting of its
own.

The two rejected answers, for the record. **Applying it immediately and moving
the sidecar with it** keeps the record and the files together, but rewrites
user data as a side effect of a settings change — a much larger promise than a
folder picker makes. **Refusing while a sitting is open** is honest and makes
the Settings screen a control that sometimes says no, for a wait that ends by
itself within seconds of the game closing.

#### What the user sees between saving and it taking effect

The wait is bounded by the sitting, not by how often somebody plays. A
directory saved with nothing being recorded is in force on the watcher's next
pass — about a second — so the user who changes it and then does not launch a
game for a week has nothing pending at all. A change only waits while a game is
running or inside its restart grace, and it is taken up the moment that sitting
ends.

For the sitting that is waiting, two things say so.

- **The recorder logs it**, at `info`, both when a change is held and when it
  is taken up. A change nobody can see happening is indistinguishable from one
  that never did (AGENTS.md sections 27 and 35).
- **`get_settings` and `apply_settings` say it**, on the `recording_directory`
  row: `not_yet_in_force` carries "Automatic recordings still go to …. They go
  here from the next session." It is answered from what the launch watcher is
  *using*, which only the recorder knows — the settings file holds what was
  *saved*, and for every other setting the two are the same thing. It is absent
  for a recorder that watches for no games, which has no automatic recordings to
  be behind.

#### A folder that cannot be used

Nothing new checks one. The path is validated when it is saved — blank and
non-absolute are refused — and the folder is created when the change arrives,
for the reason it is created at start-up: this recorder runs for days before it
writes anything, and "the drive you named is not there" is not a thing to find
out at the moment a game launches (AGENTS.md section 17).

A folder that could not be created is still taken, and the failure lands where
it already did: a recording starting into a directory that is missing, is not a
directory or cannot be written to fails naming it
(`ConfigError::OutputDirectoryMissing` and its neighbours). The alternative —
keeping the old folder — would be a setting the user saved, saw accepted, and
which silently does not apply.

### Which layer a game is resolved against

`RecordingRequest::game` is a `GameIdentity`, and `GameIdentity::slug()` is the
catalogue's `game_id` for a `Known` game.

| What the catalogue said                            | Resolved against                                 |
| -------------------------------------------------- | ------------------------------------------------ |
| one entry, whose `game_id` is a valid game key     | that game, inheriting from global                |
| one entry, whose `game_id` is not a valid game key | the global settings, and the log says which game |
| several entries tied                               | the global settings                              |

A tie is filed under `unattributed` precisely because the catalogue would not
choose between the candidates, so resolving one candidate's settings would be
the same guess wearing a different hat. An unusable identifier cannot happen
with the shipped catalogue —
`every_catalogue_identifier_is_a_valid_settings_key` holds the two spelling
rules together — but a user's own overlay is text on disk, and losing a
session over a spelling would be the worse failure.

**Read through `resolve_for`.** A second place that decides what a game records
at is the scattering AGENTS.md section 30 forbids, and
`ResolvedSettings::apply_to` is the one conversion from what a user configured
into what a recording is told.

### What a stale setting does, and why it is not what a flag does

A per-game setting is not a sentence somebody typed a second ago. It was chosen
once — possibly on another machine, possibly before the graphics card was
replaced — and the recording it governs starts because a game launched with
nobody watching. So a recording built from settings is given
`UnavailableChoice::Substitute`, and one built from a command line keeps
`UnavailableChoice::Refuse`:

| The setting names                     | `--encoder nvenc`, typed now        | `"encoder": "nvenc"`, configured                                                 |
| ------------------------------------- | ----------------------------------- | -------------------------------------------------------------------------------- |
| an encoder this machine will not open | the recording fails, naming it      | the ranked encoders are tried after it, and the substitution is logged at `warn` |
| a size the capture is not producing   | `SessionError::ScalingNotSupported` | recorded at the source size, and the substitution is logged at `warn`            |

In both configured cases the footage exists and the log says what was done
instead (AGENTS.md sections 16, 17 and 45). The configured encoder is still
tried _first_, so a machine that does have it uses it.

An invalid _value_ cannot reach a recording at all: every setter validates, so a
`Configuration` that exists is valid, and a file this build cannot read leaves
the previous configuration standing (["Failure, and what
survives it"](#failure-and-what-survives-it)).

### What `apply_to` does not carry

- **`capture_target`** decides which handle the _caller_ resolves — the game's
  window, or the display it is on via `clipped_windows::monitor_for_window` — so
  it is read before a recording's target exists.
- **The remembered capture method** is not a setting at all and does not live in
  a `ResolvedSettings`. It rides on `RecordingRequest::remembered_capture_method`
  and is applied with
  `RecordingSettings::with_remembered_capture_method`, because it is something
  Clipped observed about this machine rather than something the user configured
  — and because a value folded into `ResolvedSettings` would show up on the
  settings screen as a choice with a Reset control (see
  [`capture`](#capture)).
- **`replay_window_seconds`** becomes a `clipped_replay::ReplayConfig` when a
  recording opens a replay buffer, which needs the bitrate the encoder session
  was opened with; `record_with_replay` is where that meets. Who reads it:
  `clipped-recorder replay` with no `--duration`, and any `start_recording` that
  sent `replay` without a length — which is every recording the desktop starts,
  because that window cannot read a setting and the answer inherits per game
  anyway ([`docs/ipc.md`](ipc.md), issue
  [#427](https://github.com/wildware-uk/clipped/issues/427)). Both read it
  through `ResolvedSettings::replay_buffer_window`, which is `None` for `0` —
  the one place the off value becomes the absence the rest of the workspace
  spells `Option`, so that no caller decides for itself what off means.

### The two ways a caller applies them, and which one to use

A driver reaches a recording with a `RecordingSettings` that either carries
answers of its own or does not, and that decides which method it wants:

| The caller                             | The base recording                                                  | Method                | A setting nobody configured          |
| -------------------------------------- | ------------------------------------------------------------------- | --------------------- | ------------------------------------ |
| had nothing but a target and an output | `RecordingSettings::new(target, output)`                            | `apply_to`            | becomes the value Clipped ships with |
| was already told what to record with   | built from a command line, as `watch` builds it from `settings_for` | `apply_configured_to` | stays as the caller asked for it     |

`apps/recorder/src/watch.rs` is the second row: its command line names a
resolution, a frame rate, a codec, an encoder and two audio selections before
any game has launched. So is `apps/recorder/src/serve.rs`, for the same reason
wearing different clothes: a `start_recording` may carry any of those parameters,
and `apply_to` would replace every one of them with the shipped default on a
machine with no settings file. The recording a window asks for resolves through
the game's layer where the catalogue claims the window's process, and through the
global layer where it does not — the same rule an automatic recording follows,
because it is the same lookup ([sessions.md](sessions.md),
[issue #403](https://github.com/wildware-uk/clipped/issues/403)).

```rust
// at start-up, from `%LOCALAPPDATA%\Clipped\settings.json`
let manager = SessionManager::new(catalogue, settings).with_configuration(configuration);
// … and in the recording thread, where `settings_for` builds the recording:
let settings = request.settings.apply_configured_to(settings_for(&config, &window));
```

`apply_to` there would have put the shipped default over every option that
command line offers, on every machine with no settings file for it —
`watch --framerate 144` recording at 60, and `--microphone none` recording a
microphone. A flag that parses and then does nothing is AGENTS.md section 27's
defect, and the microphone case is one that records a device somebody asked not
to record. `apply_configured_to` applies only settings whose `SettingSource` is
not `Default`, which is exactly "what a user configured".

The unavailable-encoder question follows the same rule: `apply_configured_to`
gives the recording `UnavailableChoice::Substitute` when the _configuration_
supplied the encoder or the resolution, and leaves a command line's `Refuse`
standing when it did not — the two rows of the table above.

## Where the code is

| File                                       | What is in it                                                |
| ------------------------------------------ | ------------------------------------------------------------ |
| `crates/session/src/config/mod.rs`         | `Configuration`: the layers, and resolution                  |
| `crates/session/src/config/preferences.rs` | One layer, the settings themselves, validation, and the fold |
| `crates/session/src/config/value.rs`       | `Resolved`, `SettingSource`, `Scope`, `SettingKey`           |
| `crates/session/src/config/hotkeys.rs`     | The hotkey layer and its three states                        |
| `crates/session/src/config/notifications.rs` | Which failures interrupt the user                          |
| `crates/session/src/config/game.rs`        | How a settings file names a game                             |
| `crates/session/src/config/capture_memory.rs` | What Clipped observed about capturing each game           |
| `crates/session/src/config/document.rs`    | The file format, versions and migration                      |
| `crates/session/src/config/store.rs`       | Reading, saving, and what survives a bad file                |
| `crates/session/src/config/tests.rs`       | Inheritance, validation, migration and the file              |
