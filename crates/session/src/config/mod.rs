//! Settings, and the one place they are resolved.
//!
//! AGENTS.md section 30 asks for four things and this module is all four of
//! them: settings with sensible defaults, explicit types, validation and
//! backwards-compatible migration; per-game settings that inherit from global
//! unless explicitly overridden; configuration resolved through a defined API;
//! and no configuration reads scattered through the codebase.
//!
//! # The three layers
//!
//! ```text
//! default          60 fps          the value Clipped ships with
//!    ↓
//! global           60 fps          what the user set for everything
//!    ↓
//! counter-strike-2 120 fps         what the user set for one game
//! ```
//!
//! A layer that says nothing about a setting passes the one below it through.
//! Minecraft, which says nothing, records at 60; change the global to 90 and
//! Minecraft follows while Counter-Strike 2 stays at 120. That only works
//! because a layer stores `Option<T>` and not `T`: a per-game layer holding the
//! *effective* value could not tell "inherited 60" from "set to 60", and the
//! first global change would silently stop propagating.
//!
//! The same distinction is what a settings screen needs
//! ([issue #51](https://github.com/wildware-uk/clipped/issues/51)), which is
//! why [`Resolved`] carries three things and not one: the value,
//! [`Resolved::source`] — which layer supplied it — and
//! [`Resolved::is_overridden`], which is what a Reset control is enabled by.
//!
//! # The API
//!
//! ```
//! use clipped_session::config::{Configuration, GameKey, Preferences, SettingSource};
//!
//! let mut configuration = Configuration::defaults();
//!
//! let mut global = Preferences::none();
//! global.set_framerate(Some(60))?;
//! configuration.set_global(global);
//!
//! let mut counter_strike = Preferences::none();
//! counter_strike.set_framerate(Some(120))?;
//! configuration.set_game(GameKey::parse("counter-strike-2")?, counter_strike);
//!
//! let minecraft = GameKey::parse("minecraft")?;
//! let resolved = configuration.resolve_for(&minecraft);
//! assert_eq!(resolved.framerate().get(), 60);
//! assert_eq!(resolved.framerate().source(), SettingSource::Global);
//! assert!(!resolved.framerate().is_overridden());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! [`ConfigurationStore`] is the same configuration with a file behind it: it
//! reads [`settings.json`](document::FILE_NAME), migrates an older one, and
//! keeps both the configuration in force *and* the file itself when one cannot
//! be read — a save over a file this build could not understand is refused
//! rather than performed, which is the half that makes the refusal worth
//! anything (AGENTS.md section 56).
//!
//! # What reads this, and what does not yet
//!
//! [`crate::automatic`] does, at one moment: when its `SessionManager` asks for
//! a recording it resolves that game's settings through [`Configuration::resolve_for`]
//! and hands the answer over on `RecordingRequest::settings`
//! ([issue #61](https://github.com/wildware-uk/clipped/issues/61)). The answer
//! is a value and is never re-read, so a settings change during a recording
//! reaches the next one rather than the encoder that is running.
//!
//! [`ResolvedSettings::apply_to`] is the conversion from what a user configured
//! into what a recording is told
//! ([`crate::RecordingSettings`]). Two settings are not part of it and
//! deliberately: `capture_target` decides which *handle* the caller resolves,
//! before a recording's target exists, and `replay_window` becomes a
//! `clipped_replay::ReplayConfig` where a recording opens a replay buffer.
//! `docs/configuration.md` has the whole of it.
//!
//! `apps/recorder` reads it in both of the ways a recording can start: `watch`
//! resolves each game's settings through the session manager and lays them over
//! its command line, and `serve` does the same for a recording the window asked
//! for — and re-reads the file whenever the window changes it, so a setting
//! saved from the Settings screen reaches the *next* recording without a
//! restart ([issue #51](https://github.com/wildware-uk/clipped/issues/51)).
//!
//! One setting is still carried and read by nothing when a recording starts,
//! and the recorder says so rather than leaving it to be discovered
//! (`apps/recorder/src/settings.rs`, AGENTS.md section 54):
//! [`SettingKey::CaptureTarget`], which decides which handle the caller
//! resolves before a recording exists.
//! [`SettingKey::ReplayWindow`] joined the others when
//! [issue #427](https://github.com/wildware-uk/clipped/issues/427) gave a
//! recording the window started a buffer: the request carries `replay` with no
//! length, and the recorder resolves the length from here.
//!
//! Per-game defaults from the game catalogue are a fourth layer that does not
//! exist yet either: `clipped_game_detection::catalogue::Entry::default_settings`
//! is parsed and nothing interprets it. Folding it in between the default and
//! the global layer is [issue
//! #247](https://github.com/wildware-uk/clipped/issues/247).

mod capture_memory;
mod document;
mod effective;
mod error;
mod game;
mod hotkeys;
mod notifications;
mod plugins;
mod preferences;
mod storage;
mod store;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::time::SystemTime;

use clipped_capture::{CaptureMethod, CaptureMethodSetting};

pub use capture_memory::{CaptureMemory, MEMORY_LIFETIME};
pub use document::{Loaded, FILE_NAME, SCHEMA_VERSION};
pub use effective::{effective_settings, EffectiveSetting, EffectiveSource};
pub use error::{ConfigurationError, Section, SettingError};
pub use game::{GameKey, InvalidGameKey};
pub use hotkeys::{HotkeyOverride, HotkeyOverrides, ResolvedHotkeys};
pub use notifications::{NotTrueOrFalse, NotificationCategory, NotificationSettings};
pub use plugins::{NotStarted, PluginConsent, PluginConsents};
pub use preferences::{
    AudioDeviceSetting, CaptureTargetSetting, Preferences, ResolvedSettings, DEFAULT_REPLAY_WINDOW,
    MAXIMUM_DEVICE_NAME, MAXIMUM_DIMENSION, MAXIMUM_FRAMERATE, MINIMUM_DIMENSION,
    MINIMUM_FRAMERATE, REPLAY_WINDOW_OFF,
};
pub use storage::{trash_beside, DirectoryPathError, StorageProblem, StorageSettings};
pub use store::ConfigurationStore;
pub use value::{Resolved, Scope, SettingKey, SettingSource};

// Re-exported rather than redefined. A configured codec, encoder or resolution
// is the same value a recording is asked for, and a second set of types
// meaning the same thing is the duplication AGENTS.md section 55 exists to
// prevent; what this module adds is the layering around them, not new
// vocabulary.
pub use crate::settings::{CodecPreference, EncoderPreference, ResolutionSetting};

mod value;

/// Everything the user has configured.
///
/// Valid by construction: every way of putting a value in validates it, so a
/// `Configuration` that exists is one whose settings are all in range. That is
/// what makes "the previous valid configuration is retained" a property of the
/// type rather than a discipline the callers have to keep
/// ([`ConfigurationStore`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Configuration {
    global: Preferences,
    games: BTreeMap<GameKey, Preferences>,
    hotkeys: HotkeyOverrides,
    plugins: PluginConsents,
    /// What the library is allowed to occupy. Global only, and a section of its
    /// own for the reason `storage` gives.
    storage: StorageSettings,
    /// Which failures interrupt the user. Global only, and a section of its own
    /// for the reason `notifications` gives; the recorder keeps them and the
    /// desktop application is what reads them, over the protocol
    /// ([issue #252](https://github.com/wildware-uk/clipped/issues/252)).
    notifications: NotificationSettings,
    /// What Clipped observed about capturing each game, as opposed to what the
    /// user chose. A section of its own for the reasons `capture_memory` gives,
    /// the first of which is that a settings screen must not offer to reset a
    /// value nobody set.
    capture: BTreeMap<GameKey, CaptureMemory>,
    /// Top-level keys from a newer build, kept and written back (AGENTS.md
    /// section 56).
    unknown: BTreeMap<String, serde_json::Value>,
}

/// Everything a settings file was read into, on its way to a
/// [`Configuration`].
///
/// A struct rather than a list of arguments, and not only because there are
/// eight of them: two of them are `BTreeMap`s keyed by [`GameKey`] holding
/// different things, and a pair like that passed positionally is a pair a
/// caller can swap without the compiler noticing.
pub(crate) struct Parts {
    pub(crate) global: Preferences,
    pub(crate) games: BTreeMap<GameKey, Preferences>,
    pub(crate) hotkeys: HotkeyOverrides,
    pub(crate) plugins: PluginConsents,
    pub(crate) storage: StorageSettings,
    pub(crate) notifications: NotificationSettings,
    pub(crate) capture: BTreeMap<GameKey, CaptureMemory>,
    pub(crate) unknown: BTreeMap<String, serde_json::Value>,
}

impl Configuration {
    /// Nothing configured: every setting resolves to its default.
    #[must_use]
    pub fn defaults() -> Self {
        Self::default()
    }

    /// The settings that apply to every game.
    #[must_use]
    pub const fn global(&self) -> &Preferences {
        &self.global
    }

    /// Replaces the global settings.
    ///
    /// Settings a newer Clipped wrote and this build does not understand are
    /// carried over from what is being replaced, because a caller cannot be
    /// asked to preserve a key it has never heard of
    /// ([`Preferences::adopt_unrecognised_from`]).
    pub fn set_global(&mut self, mut preferences: Preferences) {
        preferences.adopt_unrecognised_from(&self.global);
        self.global = preferences;
    }

    /// What one game overrides, if it overrides anything.
    #[must_use]
    pub fn game(&self, game: &GameKey) -> Option<&Preferences> {
        self.games.get(game)
    }

    /// Replaces one game's overrides.
    ///
    /// An empty `Preferences` is stored rather than dropped: a game the user
    /// has opened the settings for, and reset every value on, is a game they
    /// may well come back to, and the file recording that is a few bytes.
    /// Unrecognised keys are carried over as [`Self::set_global`] carries them.
    pub fn set_game(&mut self, game: GameKey, mut preferences: Preferences) {
        if let Some(previous) = self.games.get(&game) {
            preferences.adopt_unrecognised_from(previous);
        }
        self.games.insert(game, preferences);
    }

    /// Forgets a game's overrides entirely, returning what they were.
    ///
    /// This is the one place a per-game section is dropped, unrecognised keys
    /// included, and it is a thing the user has to have asked for — "forget
    /// this game" — rather than something an edit does on their behalf.
    ///
    /// What Clipped remembered about capturing that game goes with them, for
    /// the same reason: "forget this game" that left an observation behind
    /// would go on steering the next recording from a section the user cannot
    /// see they still have.
    pub fn clear_game(&mut self, game: &GameKey) -> Option<Preferences> {
        self.capture.remove(game);
        self.games.remove(game)
    }

    /// The capture method to try first for `game`, if one was remembered and is
    /// still worth trying.
    ///
    /// [`None`] means "decide by the published preference order", which is the
    /// answer for a game nothing has been recorded of and for a memory older
    /// than [`MEMORY_LIFETIME`]. It is a preference and not a pin: the capture
    /// layer still applies the same tests to it as to any other candidate,
    /// still falls back when it cannot start, and ignores it outright under a
    /// method the user pinned
    /// (`clipped_capture::CaptureFallback::start_preferring`). That last rule
    /// is stated there rather than repeated here, because there is where the
    /// pin is known.
    ///
    /// `now` is passed rather than read because whether a memory has expired is
    /// the sort of thing a test has to be able to state rather than wait for
    /// (AGENTS.md section 25); it is the same discipline
    /// [`crate::automatic::SessionManager`] holds itself to.
    #[must_use]
    pub fn remembered_capture_method(
        &self,
        game: &GameKey,
        now: SystemTime,
    ) -> Option<CaptureMethod> {
        self.capture
            .get(game)
            .filter(|memory| !memory.is_expired(now))
            .map(CaptureMemory::method)
    }

    /// Records that a recording of `game` ended on `method`, and says whether
    /// that changed anything.
    ///
    /// `true` means the memory is new or different and the configuration is
    /// worth saving. `false` means there was nothing to learn, and it is the
    /// answer for the overwhelmingly common recording: the same method as last
    /// time, remembered already, still fresh. That matters — a caller that
    /// wrote the settings file after every recording would rewrite a user's
    /// file dozens of times a session to store the same three words.
    ///
    /// Nothing is remembered when the user pinned a method. What a pinned
    /// recording ended on is not an observation about the machine, it is the
    /// setting being obeyed, and writing it back would turn a choice the user
    /// made into one Clipped made (issue #286's third acceptance criterion).
    ///
    /// The stamp only moves when the answer does, or when the previous answer
    /// had already expired — see `capture_memory` for why a memory that is
    /// re-stamped on every confirmation is a memory that never expires.
    pub fn remember_capture_method(
        &mut self,
        game: &GameKey,
        setting: CaptureMethodSetting,
        method: CaptureMethod,
        now: SystemTime,
    ) -> bool {
        if setting != CaptureMethodSetting::Automatic {
            return false;
        }
        if self
            .capture
            .get(game)
            .is_some_and(|memory| memory.method() == method && !memory.is_expired(now))
        {
            return false;
        }
        self.capture
            .insert(game.clone(), CaptureMemory::new(method, now));
        true
    }

    /// What was remembered about `game`, expired or not.
    ///
    /// The unfiltered form, for a diagnostic that has to be able to say "this
    /// was remembered on the third and has since expired" rather than only
    /// "nothing is remembered".
    #[must_use]
    pub fn capture_memory(&self, game: &GameKey) -> Option<&CaptureMemory> {
        self.capture.get(game)
    }

    /// Every game something has been remembered about, in identifier order.
    pub fn capture_memories(&self) -> impl Iterator<Item = (&GameKey, &CaptureMemory)> {
        self.capture.iter()
    }

    /// Every game the settings say anything about, in identifier order.
    pub fn games(&self) -> impl Iterator<Item = (&GameKey, &Preferences)> {
        self.games.iter()
    }

    /// The hotkey layer.
    #[must_use]
    pub const fn hotkeys(&self) -> &HotkeyOverrides {
        &self.hotkeys
    }

    /// Replaces the hotkey layer, carrying over bindings for actions this build
    /// does not have.
    pub fn set_hotkeys(&mut self, mut hotkeys: HotkeyOverrides) {
        hotkeys.adopt_unrecognised_from(&self.hotkeys);
        self.hotkeys = hotkeys;
    }

    /// What every setting resolves to with no game in the picture: the global
    /// settings page.
    #[must_use]
    pub fn resolve_global(&self) -> ResolvedSettings {
        ResolvedSettings::fold(Scope::Global, &self.global, None)
    }

    /// What every setting resolves to for one game.
    ///
    /// A game with no section of its own is not an error and not a special
    /// case: it inherits everything, which is what
    /// [`Resolved::is_overridden`] returning `false` for every setting says.
    #[must_use]
    pub fn resolve_for(&self, game: &GameKey) -> ResolvedSettings {
        let overrides = self.games.get(game);
        ResolvedSettings::fold(Scope::Game(game.clone()), &self.global, overrides)
    }

    /// What every hotkey resolves to.
    ///
    /// # Errors
    ///
    /// [`SettingError::HotkeyConflict`] cannot happen for a `Configuration`
    /// built through this API or read from a file — both validate — so this is
    /// `Result` only because [`HotkeyOverrides::resolve`] is. Callers that have
    /// not modified the hotkeys may treat it as infallible.
    pub fn resolve_hotkeys(&self) -> Result<ResolvedHotkeys, SettingError> {
        self.hotkeys.resolve()
    }

    /// Top-level keys from a newer build that are being kept.
    pub fn unrecognised_keys(&self) -> impl Iterator<Item = &str> {
        self.unknown.keys().map(String::as_str)
    }

    /// The kept keys, for writing back out.
    pub(crate) const fn unrecognised(&self) -> &BTreeMap<String, serde_json::Value> {
        &self.unknown
    }

    /// Assembles a configuration the file reader has already validated.
    pub(crate) fn from_parts(parts: Parts) -> Self {
        let Parts {
            global,
            games,
            hotkeys,
            plugins,
            storage,
            notifications,
            capture,
            unknown,
        } = parts;
        Self {
            global,
            games,
            hotkeys,
            plugins,
            storage,
            notifications,
            capture,
            unknown,
        }
    }

    /// What the library is allowed to occupy.
    ///
    /// Global, and deliberately not resolvable per game: a library is one thing
    /// however many games are in it.
    #[must_use]
    pub const fn storage(&self) -> &StorageSettings {
        &self.storage
    }

    /// Replaces what the library is allowed to occupy.
    pub fn set_storage(&mut self, storage: StorageSettings) {
        self.storage = storage;
    }

    /// Which failures interrupt the user.
    ///
    /// Global, and deliberately not resolvable per game: the thing being
    /// interrupted is a person rather than a recording, so "should
    /// Counter-Strike's failures interrupt me" is the same question as "should
    /// failures interrupt me" (`super::notifications`).
    #[must_use]
    pub const fn notifications(&self) -> &NotificationSettings {
        &self.notifications
    }

    /// Replaces which failures interrupt the user.
    pub fn set_notifications(&mut self, notifications: NotificationSettings) {
        self.notifications = notifications;
    }

    /// The limits automatic cleanup enforces, which is unlimited unless the
    /// user has set one.
    #[must_use]
    pub fn storage_limits(&self) -> clipped_library::accounting::StorageLimits {
        self.storage.limits()
    }

    /// Which plugins the user enabled, and what they agreed to.
    #[must_use]
    pub const fn plugins(&self) -> &PluginConsents {
        &self.plugins
    }

    /// Records what the user decided about a plugin.
    pub fn set_plugin(&mut self, plugin: String, consent: PluginConsent) {
        self.plugins.set(plugin, consent);
    }
}
