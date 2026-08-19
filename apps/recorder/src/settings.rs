//! The settings file, as the window reads and changes it.
//!
//! # What this module is, and is not
//!
//! It is **not** a second configuration system. `clipped_session::config` owns
//! `settings.json` — the defaults, the validation, the layering, the migrations
//! and the refusal to overwrite a file it could not read — and every value here
//! goes in and comes out through that API (`docs/configuration.md`). What this
//! module adds is the two things the settings screen needs and the file cannot
//! answer:
//!
//! - **which settings this build actually reads when a recording starts.** A
//!   settings file can carry a key nothing acts on, and a screen that drew that
//!   key as a working control would be the lie AGENTS.md section 27 is about.
//!   [`APPLIES`] is that answer, per setting, and it is the recorder's to give
//!   because the recorder is the process that starts recordings;
//! - **the machine.** Which microphones exist is not configuration and cannot be
//!   in the file at all.
//!
//! # A setting this recorder keeps and never reads
//!
//! The four notification switches. They are configuration — defaults, a file, a
//! versioning policy — and the process that acts on them is the desktop
//! application, which may link one crate of this workspace and so cannot open
//! the settings file itself. Until
//! [issue #252](https://github.com/wildware-uk/clipped/issues/252) they were a
//! second store in a second file with a second versioning policy; now they cross
//! the same two commands as everything else, and this module is where they join
//! the list ([`notification_entries`]).
//!
//! # Why the file is read again on every change
//!
//! [`ConfigurationStore::store`] already refuses to replace a file this build
//! could not read. Reading it again before applying an edit closes the other
//! half: a settings file that changed since this process started — the user
//! edited it by hand, or a second window saved — must not be overwritten with
//! what this process happened to have in memory (AGENTS.md section 56).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::SystemTime;

use clipped_hotkeys::{Bindings, Hotkey, HotkeyAction, ACTIONS};
use clipped_ipc::settings::{
    ApplySettings, AudioDevice, AudioDevices, MicrophoneLevel, MicrophoneLevelRequest,
    SettingEntry, SettingsView,
};
use clipped_ipc::{ErrorCode, ProtocolError};
use clipped_session::config::{
    Configuration, ConfigurationStore, GameKey, HotkeyOverride, HotkeyOverrides,
    NotificationCategory, NotificationSettings, Preferences, SettingKey, StorageSettings,
};
use clipped_session::{CaptureMethod, CaptureMethodSetting};

/// The key the recording directory has in the settings file.
///
/// It is not a [`SettingKey`], because it is not a per-game setting: the
/// library is one thing however many games are in it
/// (`clipped_session::config::storage`). The screen still draws it beside the
/// others, so it is listed with them here.
pub const RECORDING_DIRECTORY: &str = "recording_directory";

/// The key holding how much the library may occupy, in bytes.
///
/// Not a [`SettingKey`] either, and for the same reason the recording directory
/// is not: a quota is over the library, which is one thing however many games
/// are in it. The three limits below are read by the storage sweep after every
/// reconciliation (`crate::library::LibraryIndexer`), which is what makes them
/// controls rather than an account (AGENTS.md section 27, issue #95).
pub const MAXIMUM_USAGE: &str = "maximum_usage_bytes";

/// The key holding how much of the drive to leave free, in bytes.
pub const MINIMUM_FREE_SPACE: &str = "minimum_free_space_bytes";

/// The key holding how old a recording may get, in whole days.
pub const MAXIMUM_AGE_DAYS: &str = "maximum_age_days";

/// What a hotkey's key is called in the settings the window sets.
///
/// Not a [`SettingKey`] either, and for a sharper version of the reason the
/// recording directory is not one: `SettingKey` is the set of settings a *game*
/// may override, and a hotkey never can. Windows registers a combination once
/// for the process, so a per-game binding is one that could not be honoured —
/// which is why `hotkeys` is a global-only section of the settings file
/// (`clipped_session::config::hotkeys`, `docs/configuration.md`) and stays one.
///
/// Prefixed rather than sent under the action's own name, because the file
/// spells them `save_replay` inside a `hotkeys` object and these keys are one
/// flat namespace shared with the per-game settings and the notification
/// switches. `save_replay` on its own would also be a protocol command name
/// (`docs/ipc.md`), and two things with one name in one namespace is how a
/// window comes to save a setting nothing reads.
pub const HOTKEY_PREFIX: &str = "hotkey_";

/// The key `action`'s binding has in the settings the window sets.
#[must_use]
pub fn hotkey_key(action: HotkeyAction) -> String {
    format!("{HOTKEY_PREFIX}{}", action.name())
}

/// The action a settings key names, if it names one.
fn hotkey_action(key: &str) -> Option<HotkeyAction> {
    HotkeyAction::from_name(key.strip_prefix(HOTKEY_PREFIX)?)
}

/// What a hotkey is spelled as when it is deliberately bound to nothing.
///
/// The same word the storage limits and `system_audio` use for the same idea,
/// and for the same reason: a [`SettingEntry::value`] may never be blank,
/// because a window has to be able to draw the answer rather than an empty
/// field. Clearing it with `null` is Reset, which is a different thing — it
/// stops the file saying anything, so the action follows whatever Clipped ships
/// with, today and after the day the default changes.
pub const UNBOUND: &str = "none";

/// What a limit is spelled as when there is not one.
///
/// The settings file leaves the key out; a [`SettingEntry::value`] may never be
/// blank, because a window has to be able to draw the answer rather than an
/// empty field. So "no limit" crosses as a word, and the same word is accepted
/// back - which is the spelling `system_audio` already uses for the same idea.
///
/// Clearing it with `null` does the same thing. That is not two ways to say one
/// thing: `null` is Reset, which follows a later change to what Clipped ships
/// with, and today what Clipped ships with is no limit at all.
pub const NO_LIMIT: &str = "none";

/// Whether a per-game setting is one this build reads when a recording starts,
/// and what to say when it is not.
///
/// The sentence is shown in place of a control (`crate::serve`), so it names
/// what would have to land rather than saying "not supported". A setting that
/// moves from `false` to `true` here is a line somebody deletes when they wire
/// it up, which is the point: the list is next to the answer rather than in a
/// document that goes stale.
fn applies(key: SettingKey) -> Result<(), &'static str> {
    match key {
        // Laid over what a recording was asked for by
        // `ResolvedSettings::apply_configured_to`, on the path both `watch` and
        // this recorder's `start_recording` take.
        SettingKey::Resolution
        | SettingKey::Framerate
        | SettingKey::Codec
        | SettingKey::Encoder
        | SettingKey::Microphone
        | SettingKey::SystemAudio => Ok(()),
        // Read when a recording that asked for a buffer without naming a length
        // starts: the window sends `replay` with no seconds and this recorder
        // resolves the length from here (`crate::serve`, `ReplayAsked::
        // Configured`, issue #427). `clipped-recorder replay --duration` is the
        // other reader. It was not in force when the settings screen was
        // written; it is now, so the screen draws it as a control rather than
        // as a sentence.
        SettingKey::ReplayWindow => Ok(()),
        // Not applied: a recording still captures the game's own window,
        // because the capture target decides which handle the caller resolves
        // before a recording exists to be configured
        // (`clipped_session::config`).
        //
        // This named issue #61 until #61 landed without it, deliberately:
        // reading the setting is not a line of plumbing on the settings path,
        // it is a change to how `watch` and `serve` resolve a window. It was
        // split out as issue #650, and this sentence names that, because a row
        // pointing at a closed issue reads as work already done.
        SettingKey::CaptureTarget => Err(
            "every recording captures the game's own window. Reading this setting when a \
             recording starts is issue #650",
        ),
    }
}

/// The settings file this recorder owns, and the configuration in force.
///
/// One store behind one lock: two windows saving at once are serialised here,
/// and a recording that is starting reads the same configuration a save just
/// wrote. Before this existed the configuration was a snapshot taken when the
/// process started, so a setting saved from the window reached the next
/// recording only after a restart — which is not what "close the window and
/// recording works from then on" means (SPEC.md section 45).
#[derive(Debug)]
pub struct SettingsFile {
    /// [`None`] when the environment describes no per-user directory at all, in
    /// which case there is nowhere to read or write and every settings command
    /// says so rather than pretending to have saved
    /// ([`ConfigurationStore::default_path`]).
    store: Option<Mutex<ConfigurationStore>>,
    /// How many times [`Self::apply`] has saved, so that a holder of a copy can
    /// tell whether its copy is still the current one without taking the lock.
    ///
    /// The automatic recorder keeps a `Configuration` of its own — the session
    /// manager owns what it resolves per-game settings from — and its loop runs
    /// a pass a second for as long as the recorder is up. Asking
    /// [`Self::configuration`] every pass would take this store's lock and
    /// clone the whole configuration a second, forever, to learn nothing on all
    /// but the handful of passes where a save actually happened. One relaxed
    /// load answers that question instead, and the lock and the clone happen
    /// only when the answer is yes — the same trade
    /// [`clipped_session::RecordingProgress`] makes for a bookmark reading a
    /// running recording (AGENTS.md section 20, `crate::watch`, issue #51).
    ///
    /// Relaxed is enough. It is not guarding data: the configuration itself
    /// travels through the mutex below, which orders the write against the read
    /// that follows it. All this has to do is differ from what the reader saw
    /// last time, and a counter that is observed late costs one more pass —
    /// about a second — before a saved setting reaches the next recording.
    generation: AtomicU64,
}

impl SettingsFile {
    /// The settings of whoever is running this recorder, read once.
    ///
    /// A file that cannot be read is reported and the defaults stand, exactly
    /// as `watch` treats one (`crate::watch::load_configuration`): a recorder
    /// that refused to start because of a settings file would be a recorder
    /// that stops recording over a typo.
    #[must_use]
    pub fn for_this_user() -> Self {
        match ConfigurationStore::default_path() {
            Some(path) => Self::at(path),
            None => Self::nowhere(),
        }
    }

    /// The settings kept at `path`, read once.
    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        let mut store = ConfigurationStore::at(path);
        if let Err(error) = store.load() {
            // The same sentence `watch` says, through the same function, so
            // that an unreadable settings file reads the same way whichever
            // subcommand is running (AGENTS.md section 55). The store keeps the
            // defaults, and a later save is refused rather than performed
            // (`ConfigurationStore::store`).
            crate::watch::report_unreadable_settings(&error);
        }
        Self {
            store: Some(Mutex::new(store)),
            generation: AtomicU64::new(0),
        }
    }

    /// A settings file that is nowhere, for the environment that has no
    /// per-user directory.
    #[must_use]
    pub const fn nowhere() -> Self {
        Self {
            store: None,
            generation: AtomicU64::new(0),
        }
    }

    /// The configuration a recording starting now is made with.
    ///
    /// A clone, taken under the lock and let go of immediately: what a
    /// recording is made with belongs to the moment it started, and nothing
    /// re-reads this while an encoder is running (issue #61).
    #[must_use]
    pub fn configuration(&self) -> Configuration {
        match &self.store {
            Some(store) => store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .current()
                .clone(),
            None => Configuration::defaults(),
        }
    }

    /// How many times a save has landed here, for a holder of a copy to compare
    /// against.
    ///
    /// The number itself means nothing and is not part of any protocol; only
    /// *changing* means anything. A reader keeps the value it last saw beside
    /// the configuration it took, and takes a fresh one when the two differ —
    /// see the `generation` field for why the automatic recorder asks this
    /// rather than asking for the configuration every pass.
    ///
    /// Read this **before** [`Self::configuration`], never after. A save that
    /// lands between the two calls must leave the reader believing it is
    /// behind, which costs it one redundant refresh; reading it afterwards
    /// would let the reader file a generation its configuration does not
    /// contain, and that setting would never arrive.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    /// Where the settings are kept, for the window to show.
    #[must_use]
    pub fn path(&self) -> Option<PathBuf> {
        self.store.as_ref().map(|store| {
            store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .path()
                .to_path_buf()
        })
    }

    /// Every setting, as it now stands, for the global settings or for one
    /// game.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::Internal`] when there is nowhere to keep settings at all,
    /// and [`ErrorCode::InvalidParameters`] when the text offered is not a game
    /// identifier — named at the moment it is asked for rather than answered
    /// with the global settings, because a page drawn under a game's name from
    /// the global answer would show every value as inherited when the global
    /// settings had set half of them.
    pub fn view(&self, game: Option<&str>) -> Result<SettingsView, ProtocolError> {
        let game = game.map(parse_game).transpose()?;
        let store = self.locked()?;
        Ok(view_of(store.current(), store.path(), game.as_ref()))
    }

    /// Applies changes and saves them, answering with the settings as they now
    /// stand.
    ///
    /// Nothing is written when anything is refused: the configuration is built
    /// in full and only then stored, so a request that names one good value and
    /// one bad one leaves the file exactly as it was.
    ///
    /// # What naming a game changes
    ///
    /// The layer the values are written into, and nothing else. A change with
    /// no game edits the global settings every game inherits from; a change with
    /// one edits that game's own section, which is what makes the value an
    /// override (AGENTS.md section 30). `null` is Reset in both, and on a game's
    /// page it means "inherit this again" rather than "return to the shipped
    /// default" — the two differ whenever the global settings hold the setting.
    ///
    /// A game whose last override is cleared keeps its section, empty.
    /// [`Configuration::set_game`] stores one rather than dropping it, because a
    /// game somebody has opened the settings for and reset every value on is a
    /// game they may well come back to — so the game stays on
    /// [`SettingsView::games`] and its page is still reachable.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidParameters`] naming the setting, the value and what
    /// would have been accepted, and [`ErrorCode::Internal`] when the file
    /// itself cannot be read or written — carrying `clipped_session`'s own
    /// sentence, which names the file and what to do about it.
    pub fn apply(&self, request: &ApplySettings) -> Result<SettingsView, ProtocolError> {
        let game = request.game.as_deref().map(parse_game).transpose()?;
        let mut store = self.locked()?;

        // Read first. What is on disk may be newer than what this process last
        // saw, and an edit is applied on top of it rather than instead of it.
        store.load().map_err(|error| {
            ProtocolError::new(
                ErrorCode::Internal,
                format!("the settings could not be read: {error}"),
            )
        })?;

        let mut configuration = store.current().clone();
        let mut global = configuration.global().clone();
        let mut storage = configuration.storage().clone();
        let mut notifications = configuration.notifications().clone();
        let mut hotkeys = configuration.hotkeys().clone();
        // What this game already overrides, edited in place. Empty for a game
        // the file says nothing about, which is how a game comes to have a
        // section: by somebody saving something for it, and never by looking at
        // its page.
        let mut overrides = game.as_ref().map(|game| {
            configuration
                .game(game)
                .cloned()
                .unwrap_or_else(Preferences::none)
        });

        for (key, value) in &request.values {
            // A game may override the settings `SettingKey` enumerates and
            // nothing else. Refused by name rather than written to the global
            // layer: a window saving a storage limit against a game and being
            // answered "saved" would have changed the quota for every game
            // (AGENTS.md sections 27 and 45).
            if let Some(overrides) = overrides.as_mut() {
                let setting = SettingKey::from_name(key).ok_or_else(|| {
                    ProtocolError::new(
                        ErrorCode::InvalidParameters,
                        format!(
                            "`{key}` cannot be set for one game: a game may override {}, and \
                             everything else is global",
                            SettingKey::ALL
                                .map(|key| format!("`{}`", key.name()))
                                .join(", ")
                        ),
                    )
                })?;
                set_setting(overrides, setting, value.as_deref())?;
                continue;
            }

            match key.as_str() {
                RECORDING_DIRECTORY => set_recording_directory(&mut storage, value.as_deref())?,
                MAXIMUM_USAGE | MINIMUM_FREE_SPACE | MAXIMUM_AGE_DAYS => {
                    set_limit(&mut storage, key, value.as_deref())?;
                }
                name => {
                    // Before the per-game settings, because a hotkey is not
                    // one: the combinations are a global-only section, and this
                    // is where the whole request would be refused if two of
                    // them named one combination (issue #233).
                    if let Some(action) = hotkey_action(name) {
                        set_hotkey(&mut hotkeys, action, value.as_deref())?;
                        continue;
                    }
                    if let Some(category) = NotificationCategory::from_key(name) {
                        set_notification(&mut notifications, category, value.as_deref())?;
                        continue;
                    }
                    let setting = SettingKey::from_name(name).ok_or_else(|| {
                        ProtocolError::new(
                            ErrorCode::InvalidParameters,
                            format!("this recorder has no `{name}` setting"),
                        )
                    })?;
                    set_setting(&mut global, setting, value.as_deref())?;
                }
            }
        }

        configuration.set_global(global);
        configuration.set_storage(storage);
        configuration.set_notifications(notifications);
        configuration.set_hotkeys(hotkeys);
        // Only when something was actually changed. `set_game` stores a section
        // even when it is empty, so writing one on every save would give a
        // section to every game whose page was merely opened — and the games
        // with a section are what the per-game screen lists (`view_of`).
        if let (Some(game), Some(overrides)) = (game.as_ref(), overrides) {
            if !request.values.is_empty() {
                configuration.set_game(game.clone(), overrides);
            }
        }

        store.store(configuration).map_err(|error| {
            ProtocolError::new(
                ErrorCode::Internal,
                format!("the settings could not be saved: {error}"),
            )
        })?;

        // After the save and before the lock is let go: a reader that sees this
        // number change and then takes the lock is guaranteed the configuration
        // it clones is the one that was just stored, or a later one. Bumping it
        // before the store would announce a save that could still be refused.
        //
        // This is the whole of what makes a saved setting reach an automatic
        // recording. Everything else in this process reads the store directly
        // when it needs it; the automatic recorder is the one holder of a copy,
        // and this is how it is told the copy is stale (`crate::watch`, issue
        // #51). A per-game save bumps it for exactly the same reason: a frame
        // rate saved for Counter-Strike is resolved by the session manager the
        // driver owns, out of the copy it was handed (issue #63).
        self.generation.fetch_add(1, Ordering::Relaxed);

        Ok(view_of(store.current(), store.path(), game.as_ref()))
    }

    /// Records that a recording of `game` ended on `method`, so that the next
    /// recording of it starts there.
    ///
    /// This is the one thing in this module the *user* did not ask for, and it
    /// goes through the same store and the same lock as everything they did —
    /// the settings screen and this must not race over one file, and a second
    /// writer with a store of its own is how they would.
    ///
    /// It never fails a recording, and it never fails loudly. The recording is
    /// already made; what is at stake is whether the next one of the same game
    /// loses a second or two falling back, which is worth a warning in the log
    /// and nothing more (AGENTS.md section 17). In particular a settings file
    /// this build could not read is *not* replaced, exactly as a save from the
    /// settings screen is not — [`ConfigurationStore::store`] refuses, and
    /// refusing to write a note is a far smaller loss than overwriting a file a
    /// newer Clipped wrote (AGENTS.md section 56).
    pub fn remember_capture_method(&self, game: &GameKey, method: CaptureMethod) {
        let remembered = || -> Result<bool, String> {
            // The same lock and the same refusal every other caller here gets,
            // rather than a second copy of "Clipped has nowhere to keep
            // settings on this machine" that could come to read differently.
            let mut store = self.locked().map_err(|error| error.to_string())?;

            // Read first, for the reason `apply` reads first: what is on disk
            // may be newer than what this process last saw, and a note is added
            // on top of it rather than instead of it.
            store.load().map_err(|error| error.to_string())?;

            let mut configuration = store.current().clone();
            // The same decision the session manager already made, made again
            // against what the file actually holds — which may have changed, and
            // may already say this. `false` here means there is nothing to
            // write, which is the answer for nearly every recording.
            if !configuration.remember_capture_method(
                game,
                CaptureMethodSetting::Automatic,
                method,
                SystemTime::now(),
            ) {
                return Ok(false);
            }
            store
                .store(configuration)
                .map_err(|error| error.to_string())?;
            Ok(true)
        };

        match remembered() {
            Ok(true) => {
                // The generation, for the same reason `apply` bumps it: the
                // automatic recorder holds a copy of the configuration, and this
                // is how it is told the copy is stale. It has already applied
                // this to its own copy, so the bump is what keeps the two from
                // drifting rather than what delivers the change.
                self.generation.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(
                    game = game.as_str(),
                    capture_backend = method.log_value(),
                    "the capture method this game records with was saved"
                );
            }
            Ok(false) => {}
            Err(detail) => tracing::warn!(
                game = game.as_str(),
                capture_backend = method.log_value(),
                detail,
                "the capture method this game records with could not be saved, so the next \
                 recording of it may fall back again"
            ),
        }
    }

    fn locked(&self) -> Result<std::sync::MutexGuard<'_, ConfigurationStore>, ProtocolError> {
        let store = self.store.as_ref().ok_or_else(|| {
            ProtocolError::new(
                ErrorCode::Internal,
                "Clipped has nowhere to keep settings on this machine: no per-user application \
                 directory could be worked out, so nothing can be read or saved",
            )
        })?;
        Ok(store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner))
    }
}

/// Reads the game a settings command named.
///
/// Refused rather than resolved to the global settings, because the two answers
/// are not near-misses: a page drawn under a game's name out of the global
/// settings shows every value as inherited when the global settings had set
/// half of them, and offers a Reset that would clear a value for every game
/// (AGENTS.md section 27).
///
/// `clipped_session`'s own sentence, which already says what a game identifier
/// looks like and shows one (AGENTS.md section 45).
fn parse_game(game: &str) -> Result<GameKey, ProtocolError> {
    GameKey::parse(game)
        .map_err(|error| ProtocolError::new(ErrorCode::InvalidParameters, error.to_string()))
}

/// Sets one setting in the layer it belongs to, or clears it.
///
/// `layer` is the global settings on the global page and one game's own
/// overrides on its page. The validation is the same either way — it is the
/// file's, reached through `Preferences::set_written` — which is what makes a
/// value the per-game page can save exactly a value the global page can
/// (AGENTS.md section 55).
fn set_setting(
    layer: &mut Preferences,
    key: SettingKey,
    value: Option<&str>,
) -> Result<(), ProtocolError> {
    layer.set_written(key, value).map_err(|error| {
        // `clipped_session`'s own sentence, which already names the setting,
        // the value and what would have been accepted (AGENTS.md section 45).
        ProtocolError::new(ErrorCode::InvalidParameters, error.to_string())
    })?;

    // Refused here as well as when a recording starts, because "at the moment
    // you set it" is where a refusal is useful: a named playback endpoint is
    // something the file can carry and this build cannot open, and the session's
    // own sentence is the one shown so the two cannot disagree
    // (`clipped_session::audio`, issue #316).
    if key == SettingKey::SystemAudio {
        if let Some(text) = value {
            if !text.eq_ignore_ascii_case("default") && !text.eq_ignore_ascii_case("none") {
                return Err(ProtocolError::new(
                    ErrorCode::InvalidParameters,
                    clipped_session::SessionError::AudioDeviceNotSelectable.to_string(),
                ));
            }
        }
    }
    Ok(())
}

/// Points one action at a combination, unbinds it, or clears it.
///
/// The three states are the file's own, and they are three rather than two for
/// the reason the per-game settings have three: *unset* has to be
/// distinguishable from *deliberately unbound*, so that somebody who has never
/// touched Save Replay follows the default when the default changes, and
/// somebody who cleared it stays cleared (`clipped_session::config::hotkeys`).
///
/// Saving is only half of it. What makes this a control rather than a note in a
/// file is `serve`, which rebinds the running service after the save
/// (`crate::hotkeys::RegisteredHotkeys::apply`, issue #233); before that a
/// combination saved here reached Windows only at the next start, with nothing
/// on screen saying so (AGENTS.md section 27).
fn set_hotkey(
    overrides: &mut HotkeyOverrides,
    action: HotkeyAction,
    value: Option<&str>,
) -> Result<(), ProtocolError> {
    let binding = match value.map(str::trim) {
        // Reset: stop saying anything about this action at all.
        None => None,
        Some(text) if text.is_empty() || text.eq_ignore_ascii_case(UNBOUND) => {
            Some(HotkeyOverride::Unbound)
        }
        Some(text) => Some(HotkeyOverride::Bound(text.parse::<Hotkey>().map_err(
            |error| {
                // `clipped_hotkeys`' own sentence, which already says what a
                // combination looks like and which keys can be bound (AGENTS.md
                // section 45).
                ProtocolError::new(
                    ErrorCode::InvalidParameters,
                    format!("`{}` cannot be `{text}`: {error}", hotkey_key(action)),
                )
            },
        )?)),
    };

    // Refused here rather than when the service is asked to rebind, because
    // "at the moment you set it" is where a refusal is useful — and because a
    // saved file that points one combination at two actions is one this
    // recorder registers *nothing* from at its next start
    // (`crate::hotkeys::start`). The conflict `clipped_session` reports names
    // both actions, including when the combination is one another action holds
    // only by default.
    overrides
        .set(action, binding)
        .map_err(|error| ProtocolError::new(ErrorCode::InvalidParameters, error.to_string()))
}

/// Switches one notification category on or off, or clears it.
fn set_notification(
    notifications: &mut NotificationSettings,
    category: NotificationCategory,
    value: Option<&str>,
) -> Result<(), ProtocolError> {
    notifications
        .set_written(category, value)
        // `clipped_session`'s own sentence, which already names the switch, the
        // value and what would have been accepted (AGENTS.md section 45).
        .map_err(|error| ProtocolError::new(ErrorCode::InvalidParameters, error.to_string()))
}

/// Reads a limit's value as the settings file spells it.
///
/// [`NO_LIMIT`] and an absent value both mean no limit. Anything else has to be
/// a whole number, and a refusal names what was typed and what would have been
/// taken (AGENTS.md section 45).
fn limit_value(key: &str, value: Option<&str>, unit: &str) -> Result<Option<u64>, ProtocolError> {
    let Some(text) = value.map(str::trim) else {
        return Ok(None);
    };
    if text.is_empty() || text.eq_ignore_ascii_case(NO_LIMIT) {
        return Ok(None);
    }
    text.parse::<u64>().map(Some).map_err(|_| {
        ProtocolError::new(
            ErrorCode::InvalidParameters,
            format!("`{key}` cannot be `{text}`: it takes {unit}, or `{NO_LIMIT}` for no limit"),
        )
    })
}

/// Sets one storage limit, or clears it.
///
/// The bounds are `clipped_session`'s, and its refusal is the one shown: a quota
/// below a gigabyte and a maximum age below a day both delete footage somebody
/// recorded this afternoon, and the sentence that says so lives with the code
/// that enforces it rather than being written a second time here (AGENTS.md
/// sections 45 and 55).
fn set_limit(
    storage: &mut StorageSettings,
    key: &str,
    value: Option<&str>,
) -> Result<(), ProtocolError> {
    let refuse = |error: clipped_library::accounting::LimitError| {
        ProtocolError::new(ErrorCode::InvalidParameters, error.to_string())
    };

    match key {
        MAXIMUM_USAGE => storage
            .set_maximum_usage(limit_value(key, value, "a number of bytes")?)
            .map_err(refuse),
        MINIMUM_FREE_SPACE => {
            storage.set_minimum_free_space(limit_value(key, value, "a number of bytes")?);
            Ok(())
        }
        MAXIMUM_AGE_DAYS => storage
            .set_maximum_age_days(limit_value(key, value, "a whole number of days")?)
            .map_err(refuse),
        other => Err(ProtocolError::new(
            ErrorCode::InvalidParameters,
            format!("this recorder has no `{other}` setting"),
        )),
    }
}

/// Sets the recording directory, or clears it.
fn set_recording_directory(
    storage: &mut StorageSettings,
    value: Option<&str>,
) -> Result<(), ProtocolError> {
    storage
        .set_recording_directory(value.map(PathBuf::from))
        .map_err(|error| ProtocolError::new(ErrorCode::InvalidParameters, error.to_string()))
}

/// Every setting, resolved for one scope, in the order a screen lists them.
///
/// # Why the two pages are not the same list
///
/// A game may override the eight settings [`SettingKey`] enumerates and nothing
/// else. The recording directory, the three storage limits, the seven hotkey
/// bindings and the four notification switches are global by construction, each
/// for a reason written where it lives: a library is one thing however many
/// games are in it, Windows registers a combination once per process, and the
/// thing a notification interrupts is a person rather than a recording
/// (`clipped_session::config::storage`, `::hotkeys`, `::notifications`).
///
/// So a per-game page is not the global page with some rows disabled — it is a
/// shorter list, and sending the global-only rows on it would be sending
/// controls that either do nothing for this game or quietly change every game
/// (AGENTS.md section 27).
fn view_of(configuration: &Configuration, path: &Path, game: Option<&GameKey>) -> SettingsView {
    let resolved = game.map_or_else(
        || configuration.resolve_global(),
        |game| configuration.resolve_for(game),
    );
    let mut settings: Vec<SettingEntry> = SettingKey::ALL
        .into_iter()
        .map(|key| {
            let unavailable = applies(key).err();
            SettingEntry {
                key: key.name().to_owned(),
                label: key.label().to_owned(),
                value: resolved.written_value(key),
                // The scope's own answer, not a comparison this module makes:
                // "this game set it" on a game's page and "the global settings
                // set it, rather than the built-in default" on the global one.
                // A game that set a value to the same text the global layer
                // holds is overridden and offers Reset; a game that set nothing
                // is inherited and does not
                // (`clipped_session::config::Resolved::is_overridden`).
                overridden: resolved.is_overridden(key),
                choices: choices_for(key),
                accepted: accepted_for(key),
                applies: unavailable.is_none(),
                unavailable: unavailable.map(str::to_owned),
                // Every per-game setting reaches the next recording, so none of
                // them is ever waiting on anything. Only the recording
                // directory can be, and only for the length of a sitting
                // ([`note_where_recordings_still_go`]).
                not_yet_in_force: None,
            }
        })
        .collect();

    if game.is_none() {
        settings.push(recording_directory_entry(configuration.storage()));
        settings.extend(storage_limit_entries(configuration.storage()));
        settings.extend(hotkey_entries(configuration));
        settings.extend(notification_entries(configuration.notifications()));
    }

    SettingsView {
        file: path.to_string_lossy().into_owned(),
        game: game.map(|game| game.as_str().to_owned()),
        // Sent on both pages. On the global one it is the list a per-game page
        // opens from; on a game's own it is what says whether the game being
        // looked at is one the file already has a section for, which is the
        // difference between "everything is inherited because nobody has
        // configured this game" and "everything is inherited because it was all
        // reset".
        games: configuration
            .games()
            .map(|(key, _)| key.as_str().to_owned())
            .collect(),
        settings,
    }
}

/// The three storage limits' rows.
///
/// `applies` is `true` for all three, and what reads them is neither a recording
/// nor the window: it is the storage sweep, which runs on the indexer thread
/// after every reconciliation and moves recordings to the trash to stay inside
/// them (`crate::library::LibraryIndexer::enforce_storage_limits`,
/// `clipped_session::cleanup::sweep`).
///
/// They are the only settings in this build whose effect is to **delete
/// somebody's footage**, which is why the window does not save one on the
/// strength of the field alone: `get_storage` answers what a limit would take
/// before it is saved, and the screen confirms against that (AGENTS.md section
/// 56, issue #95).
///
/// A limit that is not set crosses as [`NO_LIMIT`] rather than as blank or as
/// zero. Zero is a real value for the free-space limit - fill the disk - and a
/// quota of zero is refused outright, so the three readings must not share a
/// spelling.
fn storage_limit_entries(storage: &StorageSettings) -> Vec<SettingEntry> {
    let written =
        |set: Option<u64>| set.map_or_else(|| NO_LIMIT.to_owned(), |value| value.to_string());

    vec![
        SettingEntry {
            key: MAXIMUM_USAGE.to_owned(),
            label: "Maximum usage".to_owned(),
            value: written(storage.maximum_usage()),
            overridden: storage.maximum_usage().is_some(),
            choices: Vec::new(),
            accepted: format!(
                "a number of bytes, at least {} (1 GB) - 250000000000 is 250 GB - or `{NO_LIMIT}` \
                 to keep everything",
                clipped_library::accounting::MINIMUM_QUOTA
            ),
            applies: true,
            unavailable: None,
            not_yet_in_force: None,
        },
        SettingEntry {
            key: MINIMUM_FREE_SPACE.to_owned(),
            label: "Minimum free space".to_owned(),
            value: written(storage.minimum_free_space()),
            overridden: storage.minimum_free_space().is_some(),
            choices: Vec::new(),
            accepted: format!(
                "a number of bytes to leave free on the drive - 21474836480 is 20 GB - `0` to \
                 fill it, or `{NO_LIMIT}` for no such limit"
            ),
            applies: true,
            unavailable: None,
            not_yet_in_force: None,
        },
        SettingEntry {
            key: MAXIMUM_AGE_DAYS.to_owned(),
            label: "Maximum recording age".to_owned(),
            value: written(storage.maximum_age_days()),
            overridden: storage.maximum_age_days().is_some(),
            choices: Vec::new(),
            accepted: format!(
                "a whole number of days, at least 1, or `{NO_LIMIT}` to keep recordings however \
                 old they get"
            ),
            applies: true,
            unavailable: None,
            not_yet_in_force: None,
        },
    ]
}

/// One row per hotkey action: what it is bound to, and what would change it.
///
/// `applies` is `true` for all seven, and what reads them is this process's own
/// hotkey service: `serve` registers them at start-up and rebinds them when one
/// is saved, so a combination typed here reaches Windows before the reply to
/// the save does (`crate::hotkeys`, issue #233).
///
/// **This is not the hotkey list the screen already draws.** That one is
/// `get_hotkeys`, and it answers a different question — what Windows *gave*
/// this process, which is where a combination another application owns shows
/// up. These rows are what the settings file asks for. The two disagreeing is
/// the normal, meaningful case: an action can be configured to `Ctrl`+`F10` and
/// reported as a conflict, and collapsing them into one row would leave nowhere
/// to say so (AGENTS.md section 27).
///
/// Every action gets a row, including the ones no build performs yet. A key
/// that is registered and does nothing when pressed reports itself, and the
/// list above is where that is said; leaving the binding unconfigurable as well
/// would only mean somebody could not move a dead key off a combination they
/// wanted for something else.
fn hotkey_entries(configuration: &Configuration) -> Vec<SettingEntry> {
    // Only reachable from a settings file somebody edited by hand *and* got
    // past the reader, which validates the whole `hotkeys` section as it reads
    // it (`clipped_session::config::document::read_hotkeys`). Drawing the
    // shipped defaults and saying nothing would be the worst of the three
    // options here, so the rows are drawn from what the file says and every one
    // of them carries the reason it is not in force.
    let resolved = configuration.resolve_hotkeys();
    let unavailable = resolved.as_ref().err().map(|error| {
        format!(
            "no hotkey is registered, because the `hotkeys` section of the settings file points \
             one combination at two actions: {error} Fix the file and restart Clipped"
        )
    });
    let defaults = Bindings::defaults();

    ACTIONS
        .into_iter()
        .map(|action| {
            // The same fold `resolve` performs — the override if there is one,
            // the default otherwise — reached through `resolve` wherever it
            // could be, and written out only for the row that has to be drawn
            // when the whole set failed to resolve (AGENTS.md section 55).
            let (binding, overridden) = resolved.as_ref().map_or_else(
                |_| match configuration.hotkeys().get(action) {
                    Some(written) => (written.hotkey(), true),
                    None => (defaults.binding(action), false),
                },
                |resolved| {
                    let answer = resolved.binding(action);
                    (answer.get(), answer.is_overridden())
                },
            );

            SettingEntry {
                key: hotkey_key(action),
                label: action.label().to_owned(),
                value: binding.map_or_else(|| UNBOUND.to_owned(), |hotkey| hotkey.to_string()),
                overridden,
                choices: Vec::new(),
                accepted: format!(
                    "a combination such as Ctrl+F10 — one key, with any of Ctrl, Alt, Shift and \
                     Win — or `{UNBOUND}` to leave it bound to nothing"
                ),
                applies: unavailable.is_none(),
                unavailable: unavailable.clone(),
                not_yet_in_force: None,
            }
        })
        .collect()
}

/// The notification switches' rows.
///
/// `applies` is `true` for all four, and it is not a recording that reads them:
/// the desktop application does, at the moment it decides whether to show a
/// toast (`apps/desktop/src-tauri/src/notification_policy.rs`). That is what
/// the field means — whether anything in this build acts on the setting — and
/// these are the first settings where the reader is the window rather than a
/// recording, which is why `crates/ipc/src/settings.rs` says so in as many
/// words ([issue #252](https://github.com/wildware-uk/clipped/issues/252)).
///
/// This recorder keeps them and never reads them, deliberately: the process
/// that reads them may link one crate of the workspace and so cannot open the
/// settings file, and a second file for it to open was the duplication #252
/// removed.
fn notification_entries(notifications: &NotificationSettings) -> Vec<SettingEntry> {
    NotificationCategory::ALL
        .into_iter()
        .map(|category| SettingEntry {
            key: category.key().to_owned(),
            label: category.label().to_owned(),
            value: notifications.written_value(category),
            overridden: notifications.configured(category).is_some(),
            choices: NotificationCategory::choices(),
            accepted: NotificationCategory::accepted(),
            applies: true,
            unavailable: None,
            not_yet_in_force: None,
        })
        .collect()
}

/// The recording directory's row.
///
/// Its value is a path or nothing, and "nothing" is not blank: it is the
/// recorder's own default, which a window has to be able to show as the answer
/// rather than as an empty field (`crate::config::default_output_directory`).
fn recording_directory_entry(storage: &StorageSettings) -> SettingEntry {
    let configured = storage.recording_directory();
    SettingEntry {
        key: RECORDING_DIRECTORY.to_owned(),
        label: "Recording directory".to_owned(),
        value: configured.map_or_else(
            || {
                crate::config::default_output_directory()
                    .map_or_else(String::new, |path| path.to_string_lossy().into_owned())
            },
            |path| path.to_string_lossy().into_owned(),
        ),
        overridden: configured.is_some(),
        choices: Vec::new(),
        accepted: "a folder on this machine, such as D:\\Clips".to_owned(),
        applies: true,
        unavailable: None,
        // Filled in by [`note_where_recordings_still_go`] where there is a
        // launch watcher to be behind. This module reads the settings file and
        // the file holds what was *saved*; whether the launch watcher has taken
        // it up yet is a fact about the recorder, and only the recorder has it.
        not_yet_in_force: None,
    }
}

/// Says where automatic recordings are still going, when the directory that was
/// saved is not the one they are going to yet.
///
/// # Why this is not something the file can answer
///
/// Where automatic recordings are written moves **between sittings and never
/// during one**. A sitting's session record is written next to the recordings it
/// names, so a directory that moved half way through would leave the record in
/// one folder and some of its own files in another — and the failure is silent,
/// because every file is still on disk and nothing is left able to say which
/// sitting they belonged to (AGENTS.md section 56,
/// `clipped_session::automatic::SessionManager::set_recording_directory`).
///
/// So for the length of one sitting the settings file holds a folder the
/// recorder is not using. That is a saved setting whose effect is not yet
/// visible, which is a control that looks as though it did nothing (AGENTS.md
/// section 27) unless the screen is given the sentence — and the sentence is
/// the recorder's, because only the recorder knows what is in force.
///
/// # How long that is
///
/// Bounded by the sitting, not by how often somebody plays. `in_force` differs
/// from what was saved only while a game is running or inside its restart grace;
/// a directory saved with nothing being recorded is taken up on the launch
/// watcher's next pass, about a second later, and this says nothing.
///
/// `in_force` is [`None`] for a recorder that watches for no games, which has no
/// automatic recordings to be behind: nothing is said then either.
pub(crate) fn note_where_recordings_still_go(view: &mut SettingsView, in_force: Option<&Path>) {
    let Some(in_force) = in_force else {
        return;
    };
    let Some(entry) = view
        .settings
        .iter_mut()
        .find(|entry| entry.key == RECORDING_DIRECTORY)
    else {
        return;
    };

    // Compared as the same text the entry carries, which is what the window
    // draws and what `apply_settings` took: the recorder resolved `in_force`
    // from that same value through `watch::chosen_recordings_directory`, so
    // equal text is the same folder and unequal text is a change that has not
    // landed.
    if entry.value == in_force.to_string_lossy() {
        return;
    }

    entry.not_yet_in_force = Some(format!(
        "Automatic recordings still go to {}. They go here from the next session.",
        in_force.display()
    ));
}

/// The values a setting offers, where the set is closed.
fn choices_for(key: SettingKey) -> Vec<String> {
    match key {
        // Narrower than what the file accepts, deliberately: the file can carry
        // the name of a playback endpoint and this build cannot open one, so
        // offering it would be offering a recording that fails when a game
        // launches (issue #316).
        SettingKey::SystemAudio => vec!["default".to_owned(), "none".to_owned()],
        other => other.choices(),
    }
}

/// What a setting would accept, in the words its refusal uses.
fn accepted_for(key: SettingKey) -> String {
    match key {
        SettingKey::SystemAudio => {
            "\"default\" to record what you hear, or \"none\" to record no system audio".to_owned()
        }
        other => other.accepted(),
    }
}

/// The microphones this machine has.
///
/// # Errors
///
/// [`ErrorCode::Internal`] carrying the reason the endpoints could not be
/// listed, so that a window can say why there is no list rather than drawing an
/// empty one as though it had looked (AGENTS.md section 27).
#[cfg(windows)]
pub fn audio_devices() -> Result<AudioDevices, ProtocolError> {
    let microphones = clipped_session::available_microphones().map_err(|error| {
        ProtocolError::new(
            ErrorCode::Internal,
            format!("this machine's microphones could not be listed: {error}"),
        )
    })?;

    Ok(AudioDevices {
        microphones: microphones
            .into_iter()
            .map(|device| AudioDevice {
                name: device.name,
                is_default: device.is_default,
            })
            .collect(),
    })
}

/// The same, on a build with no audio backend at all.
#[cfg(not(windows))]
pub fn audio_devices() -> Result<AudioDevices, ProtocolError> {
    Err(ProtocolError::new(
        ErrorCode::Internal,
        "this build has no audio capture and cannot list the machine's microphones",
    ))
}

/// How long one level check listens.
///
/// Long enough to be past the first packet of a capture that has just started —
/// WASAPI delivers at around 10 ms and the first one costs the activation — and
/// short enough that a window polling for a meter is not waiting on the recorder
/// (`clipped_session::microphone_level` explains why the device is opened per
/// question rather than held).
#[cfg(windows)]
const LISTEN_FOR: std::time::Duration = std::time::Duration::from_millis(120);

/// What the microphone a settings value names is hearing.
///
/// # Why the value is parsed rather than taken as a device name
///
/// Because the window is asking about a *setting* it has not saved yet, and the
/// settings file's spelling is the only vocabulary either side has for one
/// (`clipped_ipc::settings`). Parsing it through [`Preferences::set_written`] —
/// the same call [`SettingsFile::apply`] makes — means a value this can be asked
/// about is exactly a value that could be saved, refused with the same sentence
/// when it is not, and resolved to a device by the code a recording resolves it
/// with. Three separate ways for the meter to end up pointed at a different
/// endpoint from the recording, closed by using one path (AGENTS.md section 55).
///
/// # Errors
///
/// [`ErrorCode::InvalidParameters`] for a value the settings file would refuse,
/// carrying `clipped_session`'s own sentence, and for `none` — which is a
/// perfectly good setting with no level to report, and must not come back as a
/// reading of silence (AGENTS.md section 27).
///
/// [`ErrorCode::Internal`] when the device cannot be opened: unplugged, in use,
/// or a name that matches nothing on this machine. A window shows the reason
/// rather than a meter at zero, because those are opposite answers.
#[cfg(windows)]
pub fn microphone_level(
    request: &MicrophoneLevelRequest,
) -> Result<MicrophoneLevel, ProtocolError> {
    let mut scratch = Preferences::default();
    scratch
        .set_written(SettingKey::Microphone, Some(&request.microphone))
        .map_err(|error| ProtocolError::new(ErrorCode::InvalidParameters, error.to_string()))?;

    let setting = scratch
        .microphone()
        .cloned()
        .unwrap_or_default()
        .as_source();

    let level = clipped_session::microphone_level(&setting, LISTEN_FOR)
        .map_err(|error| {
            ProtocolError::new(
                ErrorCode::Internal,
                format!("this microphone could not be listened to: {error}"),
            )
        })?
        .ok_or_else(|| {
            ProtocolError::new(
                ErrorCode::InvalidParameters,
                "\"none\" records no microphone, so there is no level to report",
            )
        })?;

    Ok(MicrophoneLevel {
        device: level.device,
        peak: level.peak,
        muted: level.muted,
    })
}

/// The same, on a build with no audio backend at all.
#[cfg(not(windows))]
pub fn microphone_level(
    _request: &MicrophoneLevelRequest,
) -> Result<MicrophoneLevel, ProtocolError> {
    Err(ProtocolError::new(
        ErrorCode::Internal,
        "this build has no audio capture and cannot listen to a microphone",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Scratch;
    use std::collections::BTreeMap;
    use std::fs;

    /// A directory of this test's own, removed when the test that made it
    /// passes.
    ///
    /// The removal is [`Scratch`]'s rather than this type's own. What it had
    /// was `let _ = fs::remove_dir_all(…)` in [`Drop`]: that takes a failing
    /// test's evidence with it, and it cannot report a removal that did not
    /// happen — the defect PR #597 was written to expose (issue #598).
    struct TestDirectory(Scratch);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            Self(Scratch::new(&format!("recorder-settings-{label}")))
        }

        fn file(&self) -> PathBuf {
            self.0.join("settings.json")
        }
    }

    fn change(key: &str, value: Option<&str>) -> ApplySettings {
        let mut values = BTreeMap::new();
        values.insert(key.to_owned(), value.map(str::to_owned));
        ApplySettings { game: None, values }
    }

    fn entry(view: &SettingsView, key: &str) -> SettingEntry {
        view.settings
            .iter()
            .find(|entry| entry.key == key)
            .unwrap_or_else(|| panic!("the view carries {key}"))
            .clone()
    }

    #[test]
    fn the_three_storage_limits_are_controls_and_travel_as_the_file_spells_them() {
        // Issue #95's first criterion, from the settings side: a screen can only
        // draw a control for a setting the recorder sends, and before this the
        // recorder sent none of the three - the whole storage section of the
        // file was reachable only by editing it by hand.
        let directory = TestDirectory::new("storage-limits");
        let file = SettingsFile::at(directory.file());

        let unset = file.view(None).expect("the view reads");
        for key in [MAXIMUM_USAGE, MINIMUM_FREE_SPACE, MAXIMUM_AGE_DAYS] {
            let row = entry(&unset, key);
            assert_eq!(
                row.value, NO_LIMIT,
                "{key} with nothing configured must not be blank: a window has to draw the \
                 answer, and `` is a field with nothing in it"
            );
            assert!(!row.overridden, "{key} was not configured by anybody");
            assert!(
                row.applies,
                "{key} is read by the storage sweep, so it is a control rather than an account"
            );
        }

        file.apply(&change(MAXIMUM_USAGE, Some("250000000000")))
            .expect("250 GB is above the floor");

        let saved = file.view(None).expect("the view reads");
        assert_eq!(entry(&saved, MAXIMUM_USAGE).value, "250000000000");
        assert!(entry(&saved, MAXIMUM_USAGE).overridden);

        // And it is in the file, in the words the file spells it in, so that
        // the sweep and `clipped-recorder storage` read the same figure.
        let mut reopened = ConfigurationStore::at(directory.file());
        reopened.load().expect("the file this just wrote reads");
        assert_eq!(
            reopened.current().storage().maximum_usage(),
            Some(250_000_000_000),
            "the quota did not reach the settings file, so nothing would enforce it"
        );
    }

    #[test]
    fn a_quota_small_enough_to_empty_the_library_is_refused_in_the_words_that_explain_it() {
        // The bound is `clipped_library`'s and the sentence is its own, so a
        // window shows what the code that enforces it would say (AGENTS.md
        // sections 45 and 55). A quota under a gigabyte is breached from the
        // first sitting and can only be satisfied by a library with nothing in
        // it, which is a setting whose effect is indistinguishable from a bug.
        let directory = TestDirectory::new("storage-quota-floor");
        let file = SettingsFile::at(directory.file());

        let refusal = file
            .apply(&change(MAXIMUM_USAGE, Some("1000")))
            .expect_err("a thousand bytes is not a library");

        assert_eq!(refusal.code, ErrorCode::InvalidParameters);
        assert!(
            refusal.message.contains("1000"),
            "the refusal should name what was asked for: {}",
            refusal.message
        );

        assert_eq!(
            file.view(None)
                .expect("the view reads")
                .settings
                .iter()
                .find(|row| row.key == MAXIMUM_USAGE)
                .map(|row| row.value.clone()),
            Some(NO_LIMIT.to_owned()),
            "a refused quota was written anyway, so the sweep now has a limit nobody agreed to"
        );
    }

    #[test]
    fn a_limit_is_cleared_by_the_word_for_it_and_by_a_reset_alike() {
        // Two spellings of one thing, and they are not two ways to say it:
        // `none` is a value somebody types, and `null` is Reset, which follows a
        // later change to what Clipped ships with. Today those land in the same
        // place, and both have to, or one of them silently keeps a quota.
        let directory = TestDirectory::new("storage-clearing");
        let file = SettingsFile::at(directory.file());

        for cleared in [Some(NO_LIMIT), None] {
            file.apply(&change(MAXIMUM_USAGE, Some("250000000000")))
                .expect("250 GB is above the floor");
            file.apply(&change(MAXIMUM_USAGE, cleared))
                .expect("a limit can be taken off");

            assert_eq!(
                file.configuration().storage().maximum_usage(),
                None,
                "`{cleared:?}` left a quota in force, so recordings would still be deleted"
            );
        }
    }

    #[test]
    fn zero_free_space_is_a_setting_and_zero_days_is_not() {
        // The two zeros mean opposite things and must not share a fate. Keeping
        // no free space is a defensible thing to want on a drive kept for
        // nothing else; keeping recordings for no days deletes footage as it is
        // written (AGENTS.md section 56).
        let directory = TestDirectory::new("storage-zeros");
        let file = SettingsFile::at(directory.file());

        file.apply(&change(MINIMUM_FREE_SPACE, Some("0")))
            .expect("filling the disk is a choice");
        assert_eq!(file.configuration().storage().minimum_free_space(), Some(0));

        file.apply(&change(MAXIMUM_AGE_DAYS, Some("0")))
            .expect_err("no days is not an age");
    }

    #[test]
    fn the_capture_method_that_worked_is_written_to_the_file_and_survives_a_restart() {
        // The half of issue #286 that outlives the process. The session manager
        // applies what it learned to the configuration it holds, which is enough
        // for the rest of the run; this is what makes the next run start on the
        // method that worked rather than re-learning it.
        let directory = TestDirectory::new("remember-capture-method");
        let game = GameKey::parse("counter-strike-2").expect("a game key");
        let file = SettingsFile::at(directory.file());

        file.remember_capture_method(&game, CaptureMethod::DesktopDuplication);

        // Through a second store over the same path, which is a restart in
        // everything but name: nothing of the first is carried over.
        let mut restarted = ConfigurationStore::at(directory.file());
        restarted.load().expect("the file this just wrote reads");
        assert_eq!(
            restarted
                .current()
                .remembered_capture_method(&game, SystemTime::now()),
            Some(CaptureMethod::DesktopDuplication),
            "the capture method that worked was not on disk, so the next run of the recorder \
             will fall back all over again"
        );
    }

    #[test]
    fn remembering_a_capture_method_leaves_the_settings_the_user_chose_alone() {
        // It writes through the same store as the settings screen and reads the
        // file before it writes, so a note added on a recorder's own initiative
        // must not cost the user a setting they saved a moment earlier.
        let directory = TestDirectory::new("remember-keeps-settings");
        let game = GameKey::parse("counter-strike-2").expect("a game key");
        let file = SettingsFile::at(directory.file());
        file.apply(&change("framerate", Some("144")))
            .expect("144 is in range");

        file.remember_capture_method(&game, CaptureMethod::DesktopDuplication);

        let view = file.view(None).expect("the view reads");
        assert_eq!(
            entry(&view, "framerate").value,
            "144",
            "remembering a capture method threw away a setting the user had saved"
        );
        assert_eq!(
            file.configuration()
                .remembered_capture_method(&game, SystemTime::now()),
            Some(CaptureMethod::DesktopDuplication),
            "the note was not kept, so this proves nothing about the setting surviving beside it"
        );
    }

    #[test]
    fn a_settings_file_this_build_cannot_read_is_not_replaced_by_a_note() {
        // A file a newer Clipped wrote is worth far more than knowing which
        // capture backend worked, and this write is one nobody asked for
        // (AGENTS.md section 56).
        let directory = TestDirectory::new("remember-refuses-unreadable");
        let game = GameKey::parse("counter-strike-2").expect("a game key");
        let newer = r#"{"version": 9999, "global": {}}"#;
        fs::write(directory.file(), newer).expect("the file can be written");

        SettingsFile::at(directory.file())
            .remember_capture_method(&game, CaptureMethod::DesktopDuplication);

        assert_eq!(
            fs::read_to_string(directory.file()).expect("the file is still there"),
            newer,
            "a settings file this build could not read was overwritten with a note about capture"
        );
    }

    #[test]
    fn a_microphone_picked_in_the_window_is_what_the_next_recording_is_made_with() {
        // The whole of step 3 of SPEC.md section 45, in one test: the window
        // saves a microphone, and the configuration a recording starting after
        // that is made with carries it. Before the settings reached the
        // protocol this could not be asked at all.
        let directory = TestDirectory::new("microphone");
        let settings = SettingsFile::at(directory.file());

        settings
            .apply(&change("microphone", Some("name:Shure MV7")))
            .expect("a device name is a value the file can hold");

        let resolved = settings.configuration().resolve_global();
        assert_eq!(
            resolved.written_value(SettingKey::Microphone),
            "name:Shure MV7",
            "the recording the recorder starts next is not made with what was saved",
        );
        assert!(
            directory.file().exists(),
            "the change has to reach the file, not only this process",
        );
    }

    #[test]
    fn the_recording_directory_is_saved_and_read_back_as_the_directory_that_was_picked() {
        let directory = TestDirectory::new("recordings");
        let settings = SettingsFile::at(directory.file());

        let view = settings
            .apply(&change(RECORDING_DIRECTORY, Some(r"D:\Clips")))
            .expect("an absolute path is accepted");

        assert_eq!(entry(&view, RECORDING_DIRECTORY).value, r"D:\Clips");
        assert!(entry(&view, RECORDING_DIRECTORY).overridden);
        assert_eq!(
            settings
                .configuration()
                .storage()
                .recording_directory()
                .map(Path::to_path_buf),
            Some(PathBuf::from(r"D:\Clips")),
        );
    }

    #[test]
    fn a_directory_the_launch_watcher_has_not_taken_up_yet_says_where_recordings_still_go() {
        // Where automatic recordings are written moves between sittings and
        // never during one, so that a sitting's session record is never
        // separated from the files it names (AGENTS.md section 56, issue #609).
        // For the length of that sitting the file holds a folder the recorder is
        // not using, and a screen drawing the file's answer alone would say
        // something untrue about where the next few minutes of footage is going.
        let directory = TestDirectory::new("directory-pending");
        let settings = SettingsFile::at(directory.file());
        let mut view = settings
            .apply(&change(RECORDING_DIRECTORY, Some(r"D:\Clips")))
            .expect("an absolute path is accepted");

        note_where_recordings_still_go(&mut view, Some(Path::new(r"C:\Users\alex\Videos\Clipped")));

        let row = entry(&view, RECORDING_DIRECTORY);
        assert_eq!(
            row.value, r"D:\Clips",
            "the saved value is still what the control holds, because it is what was saved",
        );
        assert!(row.applies, "and it is a setting this build reads");
        assert_eq!(
            row.unavailable, None,
            "which is what `unavailable` would deny"
        );
        assert_eq!(
            row.not_yet_in_force.as_deref(),
            Some(
                "Automatic recordings still go to C:\\Users\\alex\\Videos\\Clipped. They go \
                 here from the next session."
            ),
            "a saved value that is not yet in use has to say so, and say where the footage is \
             going in the meantime",
        );
    }

    #[test]
    fn a_directory_the_launch_watcher_is_already_using_says_nothing_about_waiting() {
        // The half that keeps the sentence honest. A delay that is described
        // when there is none is the same defect as one that is not described
        // when there is: both leave somebody unable to tell what the control
        // did (AGENTS.md section 27).
        let directory = TestDirectory::new("directory-in-force");
        let settings = SettingsFile::at(directory.file());
        let mut view = settings
            .apply(&change(RECORDING_DIRECTORY, Some(r"D:\Clips")))
            .expect("an absolute path is accepted");

        note_where_recordings_still_go(&mut view, Some(Path::new(r"D:\Clips")));
        assert_eq!(entry(&view, RECORDING_DIRECTORY).not_yet_in_force, None);

        // And a recorder that watches for no games has no automatic recordings
        // to be behind, so it says nothing either.
        note_where_recordings_still_go(&mut view, None);
        assert_eq!(entry(&view, RECORDING_DIRECTORY).not_yet_in_force, None);
    }

    #[test]
    fn a_directory_that_is_not_absolute_is_refused_with_what_would_have_been_accepted() {
        let directory = TestDirectory::new("relative");
        let settings = SettingsFile::at(directory.file());

        let refusal = settings
            .apply(&change(RECORDING_DIRECTORY, Some("clips")))
            .expect_err("a relative path is refused");

        assert_eq!(refusal.code, ErrorCode::InvalidParameters);
        assert!(
            refusal.message.contains("clips") && refusal.message.contains("absolute"),
            "the refusal should name the value and what would have been accepted: {}",
            refusal.message,
        );
        assert!(
            !directory.file().exists(),
            "a refused change must not have written anything",
        );
    }

    #[test]
    fn a_refused_value_leaves_the_settings_that_were_already_saved_alone() {
        // The half that matters: a screen saving two edits at once, one of
        // which is wrong, must not save half of them.
        let directory = TestDirectory::new("atomic");
        let settings = SettingsFile::at(directory.file());
        settings
            .apply(&change("framerate", Some("120")))
            .expect("120 is in range");

        let mut values = BTreeMap::new();
        values.insert("framerate".to_owned(), Some("60".to_owned()));
        values.insert("resolution".to_owned(), Some("wide".to_owned()));
        let refusal = settings
            .apply(&ApplySettings { game: None, values })
            .expect_err("`wide` is not a size");

        assert_eq!(refusal.code, ErrorCode::InvalidParameters);
        assert_eq!(
            settings
                .configuration()
                .resolve_global()
                .written_value(SettingKey::Framerate),
            "120",
            "the frame rate that was already saved was changed by a request that was refused",
        );
    }

    #[test]
    fn clearing_a_setting_returns_it_to_the_default_and_says_so() {
        let directory = TestDirectory::new("reset");
        let settings = SettingsFile::at(directory.file());
        settings
            .apply(&change("framerate", Some("120")))
            .expect("120 is in range");

        let view = settings
            .apply(&change("framerate", None))
            .expect("clearing is always allowed");

        let framerate = entry(&view, "framerate");
        assert!(
            !framerate.overridden,
            "a setting that was reset is still reported as configured",
        );
        assert_eq!(framerate.value, "60", "and it reads as the shipped default");
    }

    #[test]
    fn a_setting_nothing_reads_is_reported_as_one_rather_than_drawn_as_working() {
        // AGENTS.md section 27, on the wire. The capture target is a key the
        // file carries and no recording acts on, and a window has to be able to
        // tell it from one that works.
        let directory = TestDirectory::new("unavailable");
        let view = SettingsFile::at(directory.file())
            .view(None)
            .expect("a view of the defaults");

        let capture_target = entry(&view, "capture_target");
        assert!(!capture_target.applies);
        // Issue #650, and deliberately not the #61 this said before: #61 landed
        // without reading this setting, so a row pointing at it now reads as
        // work that is already done.
        assert!(capture_target
            .unavailable
            .as_deref()
            .is_some_and(|reason| reason.contains("#650")));

        let microphone = entry(&view, "microphone");
        assert!(
            microphone.applies,
            "the microphone is read when a recording starts"
        );
        assert_eq!(microphone.unavailable, None);
    }

    #[test]
    fn a_named_playback_endpoint_is_refused_now_rather_than_when_a_game_launches() {
        // The file can carry it and this build cannot open it, so saving it
        // would be a control that produces a recording which fails later
        // (issue #316). The sentence is the session's own.
        let directory = TestDirectory::new("system-audio");
        let settings = SettingsFile::at(directory.file());

        let refusal = settings
            .apply(&change("system_audio", Some("name:Speakers")))
            .expect_err("a named playback endpoint is refused");

        assert_eq!(refusal.code, ErrorCode::InvalidParameters);
        assert_eq!(
            refusal.message,
            clipped_session::SessionError::AudioDeviceNotSelectable.to_string(),
        );

        settings
            .apply(&change("system_audio", Some("none")))
            .expect("recording no system audio is always allowed");
    }

    #[test]
    fn a_setting_this_recorder_does_not_have_is_refused_by_name() {
        let directory = TestDirectory::new("unknown");
        let refusal = SettingsFile::at(directory.file())
            .apply(&change("hdr", Some("on")))
            .expect_err("there is no `hdr` setting");

        assert_eq!(refusal.code, ErrorCode::InvalidParameters);
        assert!(
            refusal.message.contains("hdr"),
            "the refusal should name the setting: {}",
            refusal.message,
        );
    }

    #[test]
    fn a_settings_file_a_hand_edit_changed_is_read_before_it_is_written_over() {
        // AGENTS.md section 56: what is on disk may be newer than what this
        // process last saw, and one saved setting must not cost the other.
        let directory = TestDirectory::new("hand-edited");
        let settings = SettingsFile::at(directory.file());
        settings
            .apply(&change("framerate", Some("120")))
            .expect("120 is in range");

        fs::write(
            directory.file(),
            r#"{"version": 1, "global": {"framerate": 120, "codec": "hevc"}}"#,
        )
        .expect("the file can be edited by hand");

        let view = settings
            .apply(&change("microphone", Some("none")))
            .expect("the edit applies on top of what is on disk");

        assert_eq!(
            entry(&view, "codec").value,
            "hevc",
            "the setting somebody wrote by hand was overwritten by this process's copy",
        );
        assert_eq!(entry(&view, "microphone").value, "none");
    }

    #[test]
    fn there_being_nowhere_to_keep_settings_is_said_rather_than_pretended_about() {
        let refusal = SettingsFile::nowhere()
            .view(None)
            .expect_err("there is nowhere to read from");

        assert_eq!(refusal.code, ErrorCode::Internal);
        assert!(
            refusal.message.contains("nowhere to keep settings"),
            "the refusal should say what is wrong: {}",
            refusal.message,
        );
        assert_eq!(
            SettingsFile::nowhere().configuration(),
            Configuration::defaults(),
            "and the recorder still records, at the settings Clipped ships with",
        );
    }

    #[test]
    fn a_notification_switched_off_in_the_window_is_kept_in_the_settings_file() {
        // Issue #252's first acceptance criterion, at the seam it changed: the
        // switches were a file of their own in the Tauri configuration
        // directory, and they are now a section of the one settings file the
        // recorder owns. Nothing here reads them — the window does — so what
        // this asserts is that they survive the round trip and reach the file.
        let directory = TestDirectory::new("notifications");
        let settings = SettingsFile::at(directory.file());

        let view = settings
            .apply(&change("recording_failed", Some("false")))
            .expect("a switch takes true or false");

        assert_eq!(entry(&view, "recording_failed").value, "false");
        assert!(entry(&view, "recording_failed").overridden);
        assert!(
            !settings
                .configuration()
                .notifications()
                .is_enabled(NotificationCategory::RecordingFailed),
            "the switch did not reach the configuration this recorder holds",
        );
        assert!(
            fs::read_to_string(directory.file())
                .expect("the file was written")
                .contains("\"notifications\""),
            "the switch has to be in the one settings file, not only in memory",
        );

        // And the other three are untouched, which is what a per-category
        // switch means.
        for category in NotificationCategory::ALL {
            if category != NotificationCategory::RecordingFailed {
                assert_eq!(
                    entry(&view, category.key()).value,
                    "true",
                    "switching one category off also switched off {category}",
                );
            }
        }
    }

    #[test]
    fn a_notification_switch_the_recorder_would_refuse_names_the_value_and_the_two_it_takes() {
        let directory = TestDirectory::new("notification-refusal");
        let refusal = SettingsFile::at(directory.file())
            .apply(&change("recording_failed", Some("sometimes")))
            .expect_err("a switch has two positions");

        assert_eq!(refusal.code, ErrorCode::InvalidParameters);
        assert!(
            refusal.message.contains("sometimes") && refusal.message.contains("true or false"),
            "the refusal should name the value and what would have been accepted: {}",
            refusal.message,
        );
    }

    #[test]
    fn every_notification_category_is_a_row_the_window_can_draw_a_switch_from() {
        // A category the recorder never sends is one the window cannot switch
        // off, and it would fall back to notifying — which is the failure this
        // issue is about, arrived at from the other end. Walked over `ALL` so
        // that a category added without a row fails here.
        let directory = TestDirectory::new("notification-rows");
        let view = SettingsFile::at(directory.file())
            .view(None)
            .expect("a view of the defaults");

        for category in NotificationCategory::ALL {
            let row = entry(&view, category.key());
            assert_eq!(row.label, category.label());
            assert_eq!(row.value, "true", "{category} should default to on");
            assert!(!row.overridden);
            assert!(
                row.applies && row.unavailable.is_none(),
                "the window acts on {category}, so it must not be drawn as a dead control",
            );
            assert_eq!(row.choices, vec!["true".to_owned(), "false".to_owned()]);
        }
    }

    #[test]
    fn every_hotkey_action_is_a_row_the_window_can_type_a_combination_into() {
        // Issue #233's remaining half, from the settings side: before this the
        // recorder sent no hotkey at all, so the settings screen could not
        // offer one and the only way to change a binding was to edit the file
        // by hand and restart. Walked over `ACTIONS` so that an action added
        // without a row fails here.
        let directory = TestDirectory::new("hotkey-rows");
        let file = SettingsFile::at(directory.file());
        let view = file.view(None).expect("a view of the defaults");

        let defaults = Bindings::defaults();
        for action in ACTIONS {
            let row = entry(&view, &hotkey_key(action));
            assert_eq!(row.label, action.label());
            assert_eq!(
                row.value,
                defaults
                    .binding(action)
                    .map_or_else(|| UNBOUND.to_owned(), |hotkey| hotkey.to_string()),
                "{action} should start on whatever Clipped ships with",
            );
            assert!(!row.overridden, "{action} was not configured by anybody");
            assert!(
                row.applies && row.unavailable.is_none(),
                "the recorder registers {action}, so it must not be drawn as a dead control",
            );
        }
    }

    #[test]
    fn a_combination_saved_from_the_window_is_the_one_the_file_carries() {
        let directory = TestDirectory::new("hotkey-saved");
        let file = SettingsFile::at(directory.file());

        let saved = file
            .apply(&change(
                &hotkey_key(HotkeyAction::TakeScreenshot),
                Some("Ctrl+Shift+F8"),
            ))
            .expect("nothing else holds Ctrl+Shift+F8");
        let row = entry(&saved, &hotkey_key(HotkeyAction::TakeScreenshot));
        assert_eq!(row.value, "Ctrl+Shift+F8");
        assert!(row.overridden, "the user set this one");

        // Through a second reader, because what is saved is what the next start
        // registers: a value that only ever existed in this process's copy
        // would leave the file and the keyboard disagreeing (issue #233).
        let reread = SettingsFile::at(directory.file())
            .configuration()
            .resolve_hotkeys()
            .expect("the file resolves");
        assert_eq!(
            reread
                .binding(HotkeyAction::TakeScreenshot)
                .get()
                .map(|hotkey| hotkey.to_string()),
            Some("Ctrl+Shift+F8".to_owned()),
        );
    }

    #[test]
    fn a_hotkey_is_unbound_by_the_word_for_it_and_returned_to_the_default_by_a_reset() {
        let directory = TestDirectory::new("hotkey-unbound");
        let file = SettingsFile::at(directory.file());
        let key = hotkey_key(HotkeyAction::SaveReplay);

        let unbound = file
            .apply(&change(&key, Some(UNBOUND)))
            .expect("an action can be bound to nothing");
        let row = entry(&unbound, &key);
        assert_eq!(row.value, UNBOUND);
        assert!(
            row.overridden,
            "deliberately unbound is a thing the user said, not the absence of one — the two \
             differ the day the shipped default changes",
        );

        let reset = file
            .apply(&change(&key, None))
            .expect("a reset is accepted");
        assert_eq!(entry(&reset, &key).value, "Ctrl+F10");
        assert!(!entry(&reset, &key).overridden);
    }

    #[test]
    fn a_combination_another_action_already_holds_is_refused_and_saves_nothing() {
        // The refusal has to happen here, at the moment it is set. A file that
        // points one combination at two actions is one `crate::hotkeys::start`
        // registers *nothing* from, so accepting it would cost the user every
        // hotkey they have at their next start.
        let directory = TestDirectory::new("hotkey-conflict");
        let file = SettingsFile::at(directory.file());
        let key = hotkey_key(HotkeyAction::TakeScreenshot);

        let refusal = file
            .apply(&change(&key, Some("Ctrl+F10")))
            .expect_err("Ctrl+F10 is Save Replay's, by default");
        assert_eq!(refusal.code, ErrorCode::InvalidParameters);
        assert!(
            refusal.message.contains("Ctrl+F10"),
            "the refusal should name the combination: {}",
            refusal.message,
        );

        assert_eq!(
            entry(&file.view(None).expect("the view reads"), &key).value,
            UNBOUND,
            "a refused change must leave the file exactly as it was, and Take screenshot ships \
             bound to nothing",
        );
    }

    #[test]
    fn a_combination_this_build_cannot_bind_is_refused_in_the_words_that_say_why() {
        let directory = TestDirectory::new("hotkey-invalid");
        let file = SettingsFile::at(directory.file());

        let refusal = file
            .apply(&change(
                &hotkey_key(HotkeyAction::AddBookmark),
                Some("Ctrl"),
            ))
            .expect_err("modifiers alone are not a combination");
        assert_eq!(refusal.code, ErrorCode::InvalidParameters);
        assert!(
            refusal.message.contains("Ctrl+F10"),
            "the refusal should show what a combination looks like: {}",
            refusal.message,
        );
    }

    #[test]
    fn every_setting_the_view_offers_is_one_the_same_view_will_accept() {
        // The list a screen draws its options from. An option the setter then
        // refuses is a control that fails when it is used.
        let directory = TestDirectory::new("choices");
        let settings = SettingsFile::at(directory.file());
        let view = settings.view(None).expect("a view of the defaults");

        for entry in &view.settings {
            for choice in &entry.choices {
                settings
                    .apply(&change(&entry.key, Some(choice)))
                    .unwrap_or_else(|error| {
                        panic!(
                            "{} offers {choice} and refuses it: {}",
                            entry.key, error.message
                        )
                    });
            }
            assert!(
                !entry.accepted.is_empty(),
                "{} says nothing about what it would accept",
                entry.key,
            );
        }
    }
    /// A change to one game's settings, as the per-game page sends it.
    fn change_for(game: &str, key: &str, value: Option<&str>) -> ApplySettings {
        let mut values = BTreeMap::new();
        values.insert(key.to_owned(), value.map(str::to_owned));
        ApplySettings {
            game: Some(game.to_owned()),
            values,
        }
    }

    #[test]
    fn a_game_nobody_has_configured_inherits_every_setting_and_offers_no_reset() {
        // The state the per-game page opens in, and the one a screen most
        // easily gets wrong: every value is real and none of them is this
        // game's, so nothing on the page may offer Reset (AGENTS.md sections 27
        // and 30).
        let directory = TestDirectory::new("game-with-no-entry");
        let settings = SettingsFile::at(directory.file());
        settings
            .apply(&change("framerate", Some("144")))
            .expect("the global settings take a frame rate");

        let view = settings
            .view(Some("counter-strike-2"))
            .expect("a game with no section of its own still has a page");

        assert_eq!(view.game.as_deref(), Some("counter-strike-2"));
        assert!(
            view.games.is_empty(),
            "looking at a game must not give it a section: {:?}",
            view.games,
        );
        assert_eq!(entry(&view, "framerate").value, "144");
        for setting in &view.settings {
            assert!(
                !setting.overridden,
                "{} is not set by this game and must not draw a Reset",
                setting.key,
            );
        }
    }

    #[test]
    fn a_value_a_game_holds_is_an_override_even_when_it_matches_what_it_would_inherit() {
        // The case that separates "inherited 60" from "set to 60". The value is
        // identical, so a screen comparing the two pages would call this
        // inherited and draw no Reset — and the setting would then follow a
        // later change to the global settings, which is exactly what the user
        // pinned it against.
        let directory = TestDirectory::new("same-value-override");
        let settings = SettingsFile::at(directory.file());
        settings
            .apply(&change("framerate", Some("144")))
            .expect("the global settings take a frame rate");
        settings
            .apply(&change_for("counter-strike-2", "framerate", Some("144")))
            .expect("and the game pins the same one");

        let view = settings.view(Some("counter-strike-2")).expect("a page");
        let framerate = entry(&view, "framerate");
        assert_eq!(framerate.value, "144");
        assert!(
            framerate.overridden,
            "a value this game holds is this game's, whatever it would have inherited",
        );

        // And a different game inherits it, which is the other half of the same
        // fact: one page's override is not another's.
        let other = settings.view(Some("minecraft")).expect("a page");
        assert_eq!(entry(&other, "framerate").value, "144");
        assert!(!entry(&other, "framerate").overridden);
    }

    #[test]
    fn resetting_a_games_last_override_returns_it_to_inheriting_and_keeps_its_page() {
        // What Reset does on the per-game page: the value goes back to being
        // inherited, and it *follows* the global settings again rather than
        // being frozen at today's answer. The game keeps its section, so its
        // page stays on the list somebody opens it from
        // (`Configuration::set_game`).
        let directory = TestDirectory::new("reset-last-override");
        let settings = SettingsFile::at(directory.file());
        settings
            .apply(&change_for("counter-strike-2", "framerate", Some("120")))
            .expect("the game takes a frame rate");
        assert_eq!(
            settings
                .view(Some("counter-strike-2"))
                .expect("a page")
                .games,
            vec!["counter-strike-2".to_owned()],
        );

        let after = settings
            .apply(&change_for("counter-strike-2", "framerate", None))
            .expect("and gives it back");

        let framerate = entry(&after, "framerate");
        assert_eq!(framerate.value, "60", "back to what Clipped ships with");
        assert!(!framerate.overridden, "and inheriting rather than pinned");
        assert_eq!(
            after.games,
            vec!["counter-strike-2".to_owned()],
            "a game somebody has configured keeps its page after resetting it",
        );

        // Following again rather than frozen: the global settings move and this
        // game moves with them.
        settings
            .apply(&change("framerate", Some("144")))
            .expect("the global settings change");
        let view = settings.view(Some("counter-strike-2")).expect("a page");
        assert_eq!(entry(&view, "framerate").value, "144");
        assert!(!entry(&view, "framerate").overridden);
    }

    #[test]
    fn a_global_only_setting_is_refused_for_a_game_rather_than_written_to_every_game() {
        // The refusal that keeps the per-game page honest. A storage limit
        // saved against a game and answered "saved" would have changed the
        // quota over the whole library, which is one thing however many games
        // are in it (AGENTS.md section 27).
        let directory = TestDirectory::new("global-only-for-a-game");
        let settings = SettingsFile::at(directory.file());

        for key in [
            MAXIMUM_USAGE,
            RECORDING_DIRECTORY,
            "recording_failed",
            &hotkey_key(HotkeyAction::SaveReplay),
        ] {
            let refusal = settings
                .apply(&change_for("counter-strike-2", key, Some("1")))
                .expect_err("a global setting is not a game's to hold");
            assert_eq!(refusal.code, ErrorCode::InvalidParameters);
            assert!(
                refusal.message.contains(key) && refusal.message.contains("global"),
                "the refusal must name the setting and say why: {}",
                refusal.message,
            );
        }

        // And nothing was written: the game has no section, so its page still
        // shows everything inherited.
        let view = settings.view(Some("counter-strike-2")).expect("a page");
        assert!(view.games.is_empty(), "{:?}", view.games);
    }

    #[test]
    fn a_games_page_carries_only_the_settings_a_game_may_override() {
        // Not the global page with rows disabled: a shorter list. A hotkey or a
        // storage limit drawn on a game's page would be a control that either
        // does nothing for that game or changes every game.
        let directory = TestDirectory::new("per-game-list");
        let settings = SettingsFile::at(directory.file());

        let global = settings.view(None).expect("the global page");
        let game = settings.view(Some("counter-strike-2")).expect("a page");

        assert_eq!(
            game.settings
                .iter()
                .map(|entry| entry.key.as_str())
                .collect::<Vec<_>>(),
            SettingKey::ALL.map(SettingKey::name).to_vec(),
        );
        for key in [
            RECORDING_DIRECTORY,
            MAXIMUM_USAGE,
            MINIMUM_FREE_SPACE,
            MAXIMUM_AGE_DAYS,
            "recording_failed",
        ] {
            assert!(
                global.settings.iter().any(|entry| entry.key == key),
                "{key} belongs on the global page",
            );
            assert!(
                !game.settings.iter().any(|entry| entry.key == key),
                "{key} is global and must not be drawn on a game's page",
            );
        }
    }

    #[test]
    fn text_that_is_not_a_game_identifier_is_refused_rather_than_answered_with_the_global_page() {
        // Answering the global settings under a game's name would draw every
        // value as inherited when the global settings had set half of them, and
        // would offer a Reset that cleared a value for every game.
        let directory = TestDirectory::new("bad-game-key");
        let settings = SettingsFile::at(directory.file());

        let refusal = settings
            .view(Some("Counter Strike"))
            .expect_err("a capital and a space is not a game identifier");
        assert_eq!(refusal.code, ErrorCode::InvalidParameters);
        assert!(
            refusal.message.contains("Counter Strike"),
            "the refusal must name what was offered: {}",
            refusal.message,
        );

        assert_eq!(
            settings
                .apply(&change_for("Counter Strike", "framerate", Some("120")))
                .expect_err("and a save is refused the same way")
                .code,
            ErrorCode::InvalidParameters,
        );
    }

    #[test]
    fn a_setting_saved_for_a_game_moves_the_generation_the_automatic_recorder_watches() {
        // The whole of "changes apply to the next session without restarting"
        // for a per-game setting: the driver holds a copy of the configuration
        // and refreshes it when this number moves (`crate::watch`, issue #51).
        // A per-game save that did not bump it would reach a recording the
        // window asked for and never an automatic one — which is every
        // recording a per-game setting exists for.
        let directory = TestDirectory::new("per-game-generation");
        let settings = SettingsFile::at(directory.file());
        let before = settings.generation();

        settings
            .apply(&change_for("counter-strike-2", "framerate", Some("120")))
            .expect("the game takes a frame rate");

        assert!(
            settings.generation() > before,
            "a per-game save has to tell the automatic recorder its copy is stale",
        );
    }
}
