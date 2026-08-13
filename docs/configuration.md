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
always will; that is what a command line is for. Nothing yet *writes* a
settings file: the screen that edits all of this is
[issue #51](https://github.com/wildware-uk/clipped/issues/51), and until it
exists the file is one somebody writes by hand.

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
| `microphone` | text | `default` | `default`, `none`, or a device name of 1–256 characters |
| `system_audio` | text | `default` | `default`, `none`, or a device name of 1–256 characters |
| `replay_window_seconds` | number | `300` | 30–1800, whole seconds |

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

"In range" means *the file can carry it and give it back unchanged*, not merely
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

**Saving looks at the file before it replaces it, and refuses if this build
could not read it.** Refusing to *read* a newer build's file preserves nothing on
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

| The file says | What happens |
| --- | --- |
| no `version` key | version 0: migrated in memory, reported as `Loaded::Migrated { from: 0 }` |
| `1` | read as written |
| `2` or higher | **refused**, and the file is left exactly as it was — by the next save as well as by the read |

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
failure leaves it standing and leaves the file alone — reading *and* saving:

| Went wrong | Configuration in force | The file | The next save |
| --- | --- | --- | --- |
| file absent | the defaults | not created | writes it |
| not JSON | unchanged | untouched | **refused**, `WouldOverwrite` |
| a value out of range | unchanged | untouched | **refused**, `WouldOverwrite` |
| two actions on one hotkey | unchanged | untouched | **refused**, `WouldOverwrite` |
| version newer than this build | unchanged | untouched | **refused**, `WouldOverwrite` |
| both `fps` and `framerate` at version 0 | unchanged | untouched | **refused**, `WouldOverwrite` |
| the file cannot be read at all (permissions) | unchanged | untouched | **refused**, `Read` |
| the save itself failed | unchanged | previous contents intact | writes it, if the disk lets it |

A user who hand-edits their settings into nonsense while Clipped is running keeps
recording with the settings they had, and their file stays as they left it until
they move it aside themselves. The last column is the half that matters: a
refusal to read that was followed by a save which overwrote anyway would preserve
nothing at all.

Note the deliberate asymmetry in the last two rows. A *content* the reader cannot
understand means the file belongs to somebody — a newer build, or the user's own
hand — and this build does not get to replace it. A *write* that failed says
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

### Which layer a game is resolved against

`RecordingRequest::game` is a `GameIdentity`, and `GameIdentity::slug()` is the
catalogue's `game_id` for a `Known` game.

| What the catalogue said | Resolved against |
| --- | --- |
| one entry, whose `game_id` is a valid game key | that game, inheriting from global |
| one entry, whose `game_id` is not a valid game key | the global settings, and the log says which game |
| several entries tied | the global settings |

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

| The setting names | `--encoder nvenc`, typed now | `"encoder": "nvenc"`, configured |
| --- | --- | --- |
| an encoder this machine will not open | the recording fails, naming it | the ranked encoders are tried after it, and the substitution is logged at `warn` |
| a size the capture is not producing | `SessionError::ScalingNotSupported` | recorded at the source size, and the substitution is logged at `warn` |

In both configured cases the footage exists and the log says what was done
instead (AGENTS.md sections 16, 17 and 45). The configured encoder is still
tried *first*, so a machine that does have it uses it.

An invalid *value* cannot reach a recording at all: every setter validates, so a
`Configuration` that exists is valid, and a file this build cannot read leaves
the previous configuration standing (["Failure, and what
survives it"](#failure-and-what-survives-it)).

### What `apply_to` does not carry

- **`capture_target`** decides which handle the *caller* resolves — the game's
  window, or the display it is on via `clipped_windows::monitor_for_window` — so
  it is read before a recording's target exists.
- **`replay_window_seconds`** becomes a `clipped_replay::ReplayConfig` when a
  recording opens a replay buffer, which needs the bitrate the encoder session
  was opened with; `record_with_replay` is where that meets.

### The two ways a caller applies them, and which one to use

A driver reaches a recording with a `RecordingSettings` that either carries
answers of its own or does not, and that decides which method it wants:

| The caller | The base recording | Method | A setting nobody configured |
| --- | --- | --- | --- |
| had nothing but a target and an output | `RecordingSettings::new(target, output)` | `apply_to` | becomes the value Clipped ships with |
| was already told what to record with | built from a command line, as `watch` builds it from `settings_for` | `apply_configured_to` | stays as the caller asked for it |

`apps/recorder/src/watch.rs` is the second row: its command line names a
resolution, a frame rate, a codec, an encoder and two audio selections before
any game has launched. So is `apps/recorder/src/serve.rs`, for the same reason
wearing different clothes: a `start_recording` may carry any of those parameters,
and `apply_to` would replace every one of them with the shipped default on a
machine with no settings file. The recording a window asks for resolves through
the *global* layer, because nothing identified a game for it to have a layer of
its own ([issue #403](https://github.com/wildware-uk/clipped/issues/403)).

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
gives the recording `UnavailableChoice::Substitute` when the *configuration*
supplied the encoder or the resolution, and leaves a command line's `Refuse`
standing when it did not — the two rows of the table above.

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
