//! What a resolved setting says about itself.
//!
//! A settings screen needs three things about every setting, and only the third
//! is the value: *what it is*, *where it came from*, and *whether this scope set
//! it*. The third is what a Reset control is enabled by — a screen that cannot
//! separate "inherited 60" from "set to 60" cannot offer one, and a Reset that
//! clears a value which was never set is a control that silently does nothing
//! (AGENTS.md section 27).

use core::fmt;

use crate::config::game::GameKey;

/// Which layer supplied the value a setting resolved to.
///
/// Three layers, folded in this order: the built-in default, then the global
/// settings, then the settings of one game. A layer that says nothing about a
/// setting passes the one below it through unchanged, which is the whole of
/// what "per-game settings inherit from global" means (AGENTS.md section 30).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SettingSource {
    /// Nothing set it; this is the value Clipped ships with.
    Default,
    /// The global settings set it.
    Global,
    /// This game's settings set it.
    Game,
}

impl SettingSource {
    /// The token this source is written as in a log line or a diagnostic.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Global => "global",
            Self::Game => "game",
        }
    }
}

impl fmt::Display for SettingSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.token())
    }
}

/// Which layer a resolution was asked for.
///
/// The settings screen has two of these — one page for the global settings and
/// one per game — and they differ in exactly one way: which layer counts as
/// "this one". [`Resolved::is_overridden`] answers against the scope rather
/// than against a fixed layer, so the global page offers Reset for a value the
/// global settings hold and the per-game page offers it for a value that game
/// holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// The global settings, which every game inherits from.
    Global,
    /// One game's settings.
    Game(GameKey),
}

impl Scope {
    /// The layer that counts as "set here" for this scope.
    #[must_use]
    pub const fn layer(&self) -> SettingSource {
        match self {
            Self::Global => SettingSource::Global,
            Self::Game(_) => SettingSource::Game,
        }
    }

    /// Which game this scope is for, if it is a game at all.
    #[must_use]
    pub const fn game(&self) -> Option<&GameKey> {
        match self {
            Self::Global => None,
            Self::Game(game) => Some(game),
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Global => formatter.write_str("global settings"),
            Self::Game(game) => write!(formatter, "settings for {game}"),
        }
    }
}

/// One setting's answer: the value, where it came from, and whether the scope
/// that was asked about set it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolved<T> {
    value: T,
    source: SettingSource,
    overridden: bool,
}

impl<T> Resolved<T> {
    /// The answer for a setting `source` supplied, resolved for a scope whose
    /// own layer is `scope_layer`.
    ///
    /// `overridden` is derived here rather than stored independently, because
    /// two fields that can disagree about the same fact is how a screen comes
    /// to offer Reset for a value nobody set.
    pub(crate) fn new(value: T, source: SettingSource, scope_layer: SettingSource) -> Self {
        Self {
            value,
            source,
            overridden: source == scope_layer,
        }
    }

    /// The value that applies.
    #[must_use]
    pub const fn value(&self) -> &T {
        &self.value
    }

    /// Which layer supplied it.
    #[must_use]
    pub const fn source(&self) -> SettingSource {
        self.source
    }

    /// Whether the scope this was resolved for set it itself, rather than
    /// inheriting it. This is what enables Reset.
    #[must_use]
    pub const fn is_overridden(&self) -> bool {
        self.overridden
    }

    /// Whether the value came from a layer below this scope.
    #[must_use]
    pub const fn is_inherited(&self) -> bool {
        !self.overridden
    }

    /// The value, consuming the answer.
    #[must_use]
    pub fn into_value(self) -> T {
        self.value
    }
}

impl<T: Copy> Resolved<T> {
    /// The value, for the settings whose type is a scalar.
    #[must_use]
    pub const fn get(&self) -> T {
        self.value
    }
}

/// Every setting a recording is configured by.
///
/// It exists as an enumeration, rather than only as fields, for two callers:
/// an error names the setting it is about without a hand-written string, and
/// the settings screen ([issue
/// #51](https://github.com/wildware-uk/clipped/issues/51)) can list what there
/// is to render without this module having to publish a second list that goes
/// stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SettingKey {
    /// Whether the game's window or the display it is on is captured.
    CaptureTarget,
    /// The size video is encoded at.
    Resolution,
    /// The frame rate ceiling.
    Framerate,
    /// Which codec to produce.
    Codec,
    /// Which encoder family to encode with.
    Encoder,
    /// Which microphone to record.
    Microphone,
    /// Which system-audio endpoint to record.
    SystemAudio,
    /// How much video the replay buffer keeps.
    ReplayWindow,
}

impl SettingKey {
    /// Every setting, in the order a settings screen should list them.
    pub const ALL: [Self; 8] = [
        Self::CaptureTarget,
        Self::Resolution,
        Self::Framerate,
        Self::Codec,
        Self::Encoder,
        Self::Microphone,
        Self::SystemAudio,
        Self::ReplayWindow,
    ];

    /// The setting's key in the settings file, and its name in a diagnostic.
    ///
    /// These are the file format. Changing one is a change to a user's file
    /// and needs a migration (AGENTS.md section 43).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::CaptureTarget => "capture_target",
            Self::Resolution => "resolution",
            Self::Framerate => "framerate",
            Self::Codec => "codec",
            Self::Encoder => "encoder",
            Self::Microphone => "microphone",
            Self::SystemAudio => "system_audio",
            Self::ReplayWindow => "replay_window_seconds",
        }
    }

    /// The setting's name in the words a person reads (AGENTS.md section 28).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::CaptureTarget => "Capture target",
            Self::Resolution => "Resolution",
            Self::Framerate => "Frame rate",
            Self::Codec => "Codec",
            Self::Encoder => "Encoder",
            Self::Microphone => "Microphone",
            Self::SystemAudio => "System audio",
            Self::ReplayWindow => "Replay window",
        }
    }

    /// The setting a file key names, if it names one.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|key| key.name() == name)
    }

    /// Every value this setting can be spelled as, where the set is closed.
    ///
    /// Empty when the set is open — a frame rate, a size, a device name — so a
    /// screen offers a list of choices for one setting and a field for another
    /// without keeping a copy of either. The spellings are the file's, which is
    /// what [`ResolvedSettings::written_value`](crate::config::ResolvedSettings::written_value)
    /// returns and what [`Preferences::set_written`](crate::config::Preferences::set_written)
    /// takes back.
    #[must_use]
    pub fn choices(self) -> Vec<String> {
        crate::config::document::choices_for(self)
    }

    /// What this setting accepts, in the words its refusal uses.
    #[must_use]
    pub fn accepted(self) -> String {
        crate::config::document::accepted_for(self)
    }
}

impl fmt::Display for SettingKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_from_the_scopes_own_layer_is_an_override_and_nothing_else_is() {
        // The distinction the settings screen's Reset control depends on.
        let set_here = Resolved::new(120, SettingSource::Game, SettingSource::Game);
        assert!(set_here.is_overridden());
        assert!(!set_here.is_inherited());

        let inherited = Resolved::new(60, SettingSource::Global, SettingSource::Game);
        assert!(!inherited.is_overridden());
        assert!(inherited.is_inherited());

        let defaulted = Resolved::new(60, SettingSource::Default, SettingSource::Game);
        assert!(!defaulted.is_overridden());
    }

    #[test]
    fn the_global_scope_treats_a_global_value_as_its_own_override() {
        // The same fact read from the global page: a value the global settings
        // hold can be reset to the default there, and a defaulted one cannot.
        let set_here = Resolved::new(60, SettingSource::Global, SettingSource::Global);
        assert!(set_here.is_overridden());

        let defaulted = Resolved::new(60, SettingSource::Default, SettingSource::Global);
        assert!(!defaulted.is_overridden());
    }

    #[test]
    fn every_setting_key_round_trips_through_its_file_name() {
        for key in SettingKey::ALL {
            assert_eq!(SettingKey::from_name(key.name()), Some(key));
        }
        assert_eq!(SettingKey::from_name("fps"), None);
    }

    #[test]
    fn no_two_settings_share_a_file_key() {
        // A duplicate would mean one setting silently reading another's value.
        let mut names: Vec<&str> = SettingKey::ALL.iter().map(|key| key.name()).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "two settings share a key: {names:?}");
    }
}
