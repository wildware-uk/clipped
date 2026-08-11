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
//! Applying a resolved setting to a recording is
//! [issue #61](https://github.com/wildware-uk/clipped/issues/61), and it is not
//! this module. `crate::automatic` decides *when* to record and holds no
//! recording settings at all; its driver — `apps/recorder/src/watch.rs` —
//! builds one `RecordingPlan` from the `watch` command line and applies it to
//! every game. What #61 changes is that
//! [`crate::automatic::RecordingRequest`] carries the game it is for, in
//! [`GameIdentity`](crate::automatic::GameIdentity), so its driver can ask:
//!
//! ```text
//! let resolved = match &request.game {
//!     GameIdentity::Known { game_id, .. } => match GameKey::parse(game_id) {
//!         Ok(game) => store.current().resolve_for(&game),
//!         Err(_) => store.current().resolve_global(),
//!     },
//!     GameIdentity::Ambiguous { .. } => store.current().resolve_global(),
//! };
//!
//! RecordingSettings::new(target, output)
//!     .with_resolution(*resolved.resolution().value())
//!     .with_framerate(resolved.framerate().get())
//!     .with_codec(*resolved.codec().value())
//!     .with_encoder(*resolved.encoder().value())
//! ```
//!
//! The two audio selections and the replay window have no field to go into yet
//! and #61 should not invent one: audio waits on being wired into a session
//! ([#180](https://github.com/wildware-uk/clipped/issues/180)) and the replay
//! window becomes a `clipped_replay::ReplayConfig`, which needs the bitrate the
//! encoder session was opened with. `docs/configuration.md` has the whole of
//! it.
//!
//! Until #61 lands, nothing in the workspace reads this module — which is
//! stated here rather than left to be discovered, because a configuration API
//! that looked as though it were in force would be worse than one that admits
//! it is not (AGENTS.md section 54).
//!
//! Per-game defaults from the game catalogue are a fourth layer that does not
//! exist yet either: `clipped_game_detection::catalogue::Entry::default_settings`
//! is parsed and nothing interprets it. Folding it in between the default and
//! the global layer is [issue
//! #247](https://github.com/wildware-uk/clipped/issues/247).

mod document;
mod error;
mod game;
mod hotkeys;
mod preferences;
mod store;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

pub use document::{Loaded, FILE_NAME, SCHEMA_VERSION};
pub use error::{ConfigurationError, Section, SettingError};
pub use game::{GameKey, InvalidGameKey};
pub use hotkeys::{HotkeyOverride, HotkeyOverrides, ResolvedHotkeys};
pub use preferences::{
    AudioDeviceSetting, CaptureTargetSetting, Preferences, ResolvedSettings, DEFAULT_REPLAY_WINDOW,
    MAXIMUM_DEVICE_NAME, MAXIMUM_DIMENSION, MAXIMUM_FRAMERATE, MINIMUM_DIMENSION,
    MINIMUM_FRAMERATE,
};
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
    /// Top-level keys from a newer build, kept and written back (AGENTS.md
    /// section 56).
    unknown: BTreeMap<String, serde_json::Value>,
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
    pub fn clear_game(&mut self, game: &GameKey) -> Option<Preferences> {
        self.games.remove(game)
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
    pub(crate) const fn from_parts(
        global: Preferences,
        games: BTreeMap<GameKey, Preferences>,
        hotkeys: HotkeyOverrides,
        unknown: BTreeMap<String, serde_json::Value>,
    ) -> Self {
        Self {
            global,
            games,
            hotkeys,
            unknown,
        }
    }
}
