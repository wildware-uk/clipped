# Configuration

**Status: the API and the file exist; nothing applies them to a recording yet.**
`clipped_session::config` models the settings, resolves global and per-game
layers, validates them and reads and writes `settings.json`. What it does *not*
do is choose a recording's settings — `clipped-recorder record` still takes them
from its command line and `clipped-recorder watch` still uses the defaults
`crates/session/src/automatic` was built with. Reading the resolved settings at
the moment a recording starts is
[issue #61](https://github.com/wildware-uk/clipped/issues/61), and
["What #61 consumes"](#what-61-consumes) below is the shape it will read. The
settings screen that edits all of this is
[issue #51](https://github.com/wildware-uk/clipped/issues/51).

This is stated first, and plainly, because a configuration API that looked as
though it were in force would be worse than one that admits it is not
(AGENTS.md sections 27 and 54).

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

| | Minecraft, frame rate unset | Counter-Strike 2, frame rate set to 60 |
| --- | --- | --- |
| Resolves to today | 60 | 60 |
| Source | `global` | `game` |
| Reset offered | no | yes |
| Global changes to 90 | now records at 90 | still records at 60 |

A per-game layer that stored the *effective* value could not tell those two
apart, and the first change to the global settings would silently stop reaching
the games that were meant to follow it. So every field in a layer is an
`Option<T>`, `None` means "this layer says nothing", and the fold reports which
layer supplied the answer.

The same three-state model applies to hotkeys, where the states are *unset*
(follow the default), *bound to a combination*, and *deliberately unbound* — see
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

| Type | What it is |
| --- | --- |
| `Preferences` | One layer: the global settings, or one game's. Every field optional, every setter validating. |
| `Configuration` | The global layer, the per-game layers, and the hotkey layer. Valid by construction. |
| `ConfigurationStore` | A `Configuration` with `settings.json` behind it. Loads, migrates, saves atomically. |
| `ResolvedSettings` | The answer for one scope: every setting, with its source. |
| `Resolved<T>` | One answer: `value()`, `source()`, `is_overridden()`. |
| `Scope` | Which layer a resolution was for — `Global`, or `Game(GameKey)`. |

### What a settings screen needs, and where it comes from

`Resolved<T>` carries three things because a screen needs three things.

- `value()` is what to show in the control.
- `source()` is `Default`, `Global` or `Game`, which is the "inherited from
  global" badge.
- `is_overridden()` is whether *this scope* set it, which is what enables Reset.

`is_overridden` is asked against the scope, not against a fixed layer. On the
per-game page it means "this game set it"; on the global page it means "the
global settings set it, rather than the built-in default" — so Reset works on
both pages and means the same thing on both.

`ResolvedSettings::source_of(key)` and `is_overridden(key)` answer the same
questions without naming a setting's type, so the badge and the Reset control
can be drawn by a loop over `SettingKey::ALL`.

## The settings

Exactly the settings this build can be told about, and no others. SPEC.md
section 31 lists more — capture mode, bitrate, event types, auto-clipping,
storage behaviour, HDR — and each of them arrives with the subsystem that reads
it. A setting for a subsystem that does not exist is a control that silently
does nothing (AGENTS.md section 27).

| Key | Type | Default | Accepts |
| --- | --- | --- | --- |
| `capture_target` | text | `game-window` | `game-window`, `display` |
| `resolution` | text | `source` | `source`, or a size such as `1920x1080`; both sides even, 128–7680 |
| `framerate` | number | `60` | 1–480 |
| `codec` | text | `auto` | `auto`, `h264`, `hevc`, `av1` |
| `encoder` | text | `auto` | `auto`, `nvenc`, `amf`, `quicksync`, `software` |
| `microphone` | text | `default` | `default`, `none`, or a device name |
| `system_audio` | text | `default` | `default`, `none`, or a device name |
| `replay_window_seconds` | number | `300` | 30–1800 |

The vocabulary is the command line's, deliberately: `--codec hevc` and
`"codec": "hevc"` mean the same thing, because a settings file and a command
line that disagreed about what an encoder is called would be two answers to one
question. `docs/recorder-cli.md` is the other half of that table.

Two of these are worth a note.

- **`resolution`** may name a size, and nothing in this build can produce a size
  other than the source's: there is no scaler in the capture-to-encoder path. A
  fixed size that does not match what capture produces is refused when a
  recording starts (`SessionError::ScalingNotSupported`), not quietly ignored.
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

Messages name the setting, the value and what would have been accepted:

```text
C:\Users\alice\AppData\Local\Clipped\settings.json cannot be used: in the
global settings, framerate is 0; it accepts 1-480 frames per second
```

The replay window is checked against `clipped_replay`'s own bounds, so the
refusal reaches the user when they set it rather than when a game launches.

## Hotkeys

Hotkeys are configuration, and they are **global only**. A per-game hotkey could
not be honoured: `clipped_hotkeys` registers a combination with Windows once,
for the process, and there is no per-foreground-application variant of that
registration — so a per-game hotkey override would be a control that silently
does nothing. SPEC.md section 31's list of per-game overrides does not include
hotkeys either.

Three states, for the same reason the recording settings have them:

| In the file | Means |
| --- | --- |
| key absent | follow the default (`Ctrl+F10` for Save Replay, `Ctrl+F9` for Add Bookmark) |
| `"save_replay": "Ctrl+F10"` | pinned to that combination, whatever the default becomes |
| `"save_replay": null` | deliberately unbound |

Two actions on one combination is refused when it is set, naming both actions —
including when the combination is one another action holds *by default*, because
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

### Saving

A temporary file and a rename, so that a crash or a full disk leaves either the
previous settings or the new ones and never half of each (AGENTS.md section 17).
The directory is created if it is not there. Nothing else writes the file.

### Versions and migration

`version` is the format, and this build writes version 1.

| The file says | What happens |
| --- | --- |
| no `version` key | version 0: migrated in memory, reported as `Loaded::Migrated { from: 0 }` |
| `1` | read as written |
| `2` or higher | **refused**, and the file is left exactly as it was |

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
depending on it being right. A document that sets *both* spellings is refused
rather than guessed: choosing wrongly would record at the wrong rate for every
session that followed.

### Settings this build has never heard of

They are kept. Every section carries the keys it could not interpret and writes
them back out unchanged — at the top level, in `global`, in each game's section
and in `hotkeys`.

The failure this prevents: a user configures something on the machine running
the newer Clipped, opens the settings on the older one, changes anything at all,
and silently loses it. Note that the caller is not asked to carry those keys —
it cannot, because it has never heard of them — so `set_global`, `set_game` and
`set_hotkeys` carry them over from the layer they replace. Only
`clear_game` drops them, and that is a thing the user has to have asked for.

## Failure, and what survives it

`ConfigurationStore` holds the last configuration it knows to be good. Every
failure leaves it standing and leaves the file alone:

| Went wrong | Configuration in force | The file |
| --- | --- | --- |
| file absent | the defaults | not created |
| not JSON | unchanged | untouched |
| a value out of range | unchanged | untouched |
| two actions on one hotkey | unchanged | untouched |
| version newer than this build | unchanged | untouched |
| both `fps` and `framerate` at version 0 | unchanged | untouched |
| the save failed | unchanged | previous contents intact |

A user who hand-edits their settings into nonsense while Clipped is running
keeps recording with the settings they had.

## What #61 consumes

Automatic recording currently applies one set of choices to every game.
`crates/session/src/automatic` holds no recording settings at all — it decides
*when* to record — and its driver, `apps/recorder/src/watch.rs`, builds a single
`RecordingPlan` from the `watch` command line and hands the same one to every
`RecordingRequest`. That is the hard-coding
[issue #61](https://github.com/wildware-uk/clipped/issues/61) replaces, and this
is the shape it reads.

`RecordingRequest::game` is a `GameIdentity`, and `GameIdentity::slug()` is the
catalogue's `game_id` for a `Known` game. So the driver holds a
`ConfigurationStore` and asks:

```rust
let resolved = match &request.game {
    GameIdentity::Known { game_id, .. } => match GameKey::parse(game_id) {
        Ok(game) => store.current().resolve_for(&game),
        Err(_) => store.current().resolve_global(),
    },
    // `Ambiguous` has no single game to resolve for: several catalogue entries
    // tied, and the session is filed under "unattributed". The global settings
    // are the honest answer, not one of the candidates'.
    GameIdentity::Ambiguous { .. } => store.current().resolve_global(),
};

let settings = RecordingSettings::new(target, output)
    .with_resolution(*resolved.resolution().value())
    .with_framerate(resolved.framerate().get())
    .with_codec(*resolved.codec().value())
    .with_encoder(*resolved.encoder().value());
```

Two settings have no `RecordingSettings` field to go into yet, and #61 should
not invent one:

- **`microphone` and `system_audio`** wait on audio being wired into a session
  ([#180](https://github.com/wildware-uk/clipped/issues/180)). Until then
  `RecordingSettings::with_audio_requested` is the honest signal — the session
  says once that it cannot record what was asked for.
- **`replay_window_seconds`** becomes a `clipped_replay::ReplayConfig` when a
  recording opens a replay buffer, which needs the bitrate the encoder session
  was opened with; `record_with_replay` is where that meets.
- **`capture_target`** decides which handle the driver resolves: the game's
  window, as today, or the display it is on via
  `clipped_windows::monitor_for_window`.

The one rule for #61: read through `resolve_for`. A second place that decides
what a game records at is the scattering AGENTS.md section 30 forbids.

## Where the code is

| File | What is in it |
| --- | --- |
| `crates/session/src/config/mod.rs` | `Configuration`: the layers, and resolution |
| `crates/session/src/config/preferences.rs` | One layer, the settings themselves, validation, and the fold |
| `crates/session/src/config/value.rs` | `Resolved`, `SettingSource`, `Scope`, `SettingKey` |
| `crates/session/src/config/hotkeys.rs` | The hotkey layer and its three states |
| `crates/session/src/config/game.rs` | How a settings file names a game |
| `crates/session/src/config/document.rs` | The file format, versions and migration |
| `crates/session/src/config/store.rs` | Reading, saving, and what survives a bad file |
| `crates/session/src/config/tests.rs` | Inheritance, validation, migration and the file |
