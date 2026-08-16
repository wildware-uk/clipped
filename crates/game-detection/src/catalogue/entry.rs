//! What a catalogue entry is, once it has been read and found valid.
//!
//! Everything here is the *validated* form. The shapes the TOML is
//! deserialised into live in [`super::schema`], and nothing in this module can
//! be constructed without having passed through that module's checks — a
//! [`GameId`] holds a string that is known to be a legal identifier, an
//! [`Entry`] holds at least one executable rule, and so on. That is the point
//! of the split: code that reads the catalogue never has to ask whether the
//! data made sense, because a catalogue that did not make sense never became
//! one (issue #42).
//!
//! # Fields that carry data for subsystems that do not exist
//!
//! [`Entry::default_settings`] and [`Entry::highlight_providers`] are held and
//! validated here and interpreted nowhere. Per-game settings are M7 and the
//! highlight provider API is M9, and inventing behaviour for either in a
//! catalogue ticket would be building a later milestone's work
//! (CONTRIBUTING.md, "Issues and milestones"). They are carried rather than
//! omitted because the alternative is that adding them later invalidates every
//! entry a contributor has written in the meantime.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// Which of the two catalogue files an entry came from.
///
/// This is the second half of the precedence rule — see
/// [`super::matching`] — and it is also what an error message needs in order
/// to tell a user *which* file to go and fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntrySource {
    /// The catalogue compiled into the application from
    /// `crates/game-detection/data/games.toml`.
    Seed,
    /// The user's own file, which they own and Clipped must not lose
    /// (AGENTS.md section 56).
    Overlay {
        /// Where that file is.
        path: PathBuf,
    },
}

impl EntrySource {
    /// The file this source refers to, when it is a file on disk.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Seed => None,
            Self::Overlay { path } => Some(path),
        }
    }

    /// Whether this source overrides the other one at equal specificity.
    #[must_use]
    pub const fn is_overlay(&self) -> bool {
        matches!(self, Self::Overlay { .. })
    }
}

impl fmt::Display for EntrySource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Seed => formatter.write_str("crates/game-detection/data/games.toml"),
            Self::Overlay { path } => write!(formatter, "{}", path.display()),
        }
    }
}

/// A game's stable identity.
///
/// Lower-case ASCII letters, digits and hyphens. The restriction is not
/// tidiness: an identifier ends up in a user's overlay file, in per-game
/// settings (M7) and eventually in a path, and an identifier that differs from
/// another only by case or by a space is one that two files will disagree
/// about. It is also what an overlay entry names when it replaces a shipped
/// one, so it has to be something a person can type exactly.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GameId(String);

impl GameId {
    /// The identifier as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Builds an identifier, having checked its characters.
    ///
    /// `None` when it is empty or contains anything outside `[a-z0-9-]`.
    /// [`super::schema`] turns that into an error naming the file and the
    /// entry; this returns an option so that the character rule lives in one
    /// place rather than being repeated by every caller.
    #[must_use]
    pub(crate) fn parse(value: &str) -> Option<Self> {
        if value.is_empty() {
            return None;
        }
        value
            .chars()
            .all(|character| {
                character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
            })
            .then(|| Self(value.to_owned()))
    }
}

impl fmt::Display for GameId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One way a game's process can be recognised.
///
/// A bare `name` claims every process with that file name anywhere on the
/// machine. `path_contains` narrows it to processes whose image path contains
/// the given fragment, which is the only thing that separates two games
/// shipping the same binary name — `hl2.exe` is Half-Life 2 and Team Fortress 2
/// and several more.
///
/// The fragment names whole directories rather than characters:
/// `steamapps/common/Half-Life 2` matches that directory and not
/// `steamapps/common/Half-Life 2 Deathmatch`, which is a different game running
/// the same executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutableRule {
    name: String,
    path_contains: Option<String>,
}

impl ExecutableRule {
    /// A rule matching a file name anywhere on the machine.
    pub(crate) fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            path_contains: None,
        }
    }

    /// The same rule, restricted to image paths containing `fragment`.
    #[must_use]
    pub(crate) fn qualified_by(mut self, fragment: impl Into<String>) -> Self {
        self.path_contains = Some(fragment.into());
        self
    }

    /// The executable's file name, with no directory part.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The path fragment an image path must contain, if the rule is qualified.
    ///
    /// Contained as whole path segments, not as a substring.
    #[must_use]
    pub fn path_contains(&self) -> Option<&str> {
        self.path_contains.as_deref()
    }
}

/// The launchers Clipped knows how to name.
///
/// The list is SPEC.md section 6's, plus GOG, plus [`Self::Other`] so that a
/// game from a shop nobody has thought of is still expressible in data — the
/// whole point of the catalogue being a file is that adding a game does not
/// need a Rust change, and a closed list would have made "which shop sells it"
/// the one field that did.
///
/// Detecting *installs* from each launcher is issues #43 and #44. This is only
/// the vocabulary those detectors and this catalogue share.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum LauncherKind {
    /// Valve's Steam.
    Steam,
    /// The Epic Games launcher.
    Epic,
    /// Xbox on Windows and the Microsoft Store.
    Xbox,
    /// Blizzard's Battle.net.
    BattleNet,
    /// The EA application.
    Ea,
    /// Ubisoft Connect.
    Ubisoft,
    /// The Riot client.
    Riot,
    /// GOG Galaxy.
    Gog,
    /// Anything else, including a game installed without a launcher at all.
    Other,
}

impl LauncherKind {
    /// The identifier as it is written in the catalogue files.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Steam => "steam",
            Self::Epic => "epic",
            Self::Xbox => "xbox",
            Self::BattleNet => "battle-net",
            Self::Ea => "ea",
            Self::Ubisoft => "ubisoft",
            Self::Riot => "riot",
            Self::Gog => "gog",
            Self::Other => "other",
        }
    }
}

impl fmt::Display for LauncherKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// What a launcher calls a game.
///
/// `app_id` is optional because it is often not known: a catalogue entry can
/// record that a game comes from Epic without anybody having found its
/// identifier. An entry with no identifier still describes the game; it simply
/// cannot be matched by launcher metadata, only by executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launcher {
    kind: LauncherKind,
    app_id: Option<String>,
    not_the_game: Vec<String>,
}

impl Launcher {
    /// A launcher with no identifier.
    pub(crate) const fn new(kind: LauncherKind) -> Self {
        Self {
            kind,
            app_id: None,
            not_the_game: Vec::new(),
        }
    }

    /// The same launcher, with the identifier it uses for this game.
    #[must_use]
    pub(crate) fn with_app_id(mut self, app_id: impl Into<String>) -> Self {
        self.app_id = Some(app_id.into());
        self
    }

    /// Which launcher.
    #[must_use]
    pub const fn kind(&self) -> LauncherKind {
        self.kind
    }

    /// The same launcher, with the processes in its directory that are not the
    /// game.
    #[must_use]
    pub(crate) fn with_not_the_game<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.not_the_game = names.into_iter().map(Into::into).collect();
        self
    }

    /// The launcher's own identifier for the game, where one is recorded.
    #[must_use]
    pub fn app_id(&self) -> Option<&str> {
        self.app_id.as_deref()
    }

    /// Processes that live in this game's install directory and are not it.
    ///
    /// A launcher claims a *directory*, so every process under it carries the
    /// game's identity — including the ones that are not the game. League of
    /// Legends is the case this exists for: `LeagueClient.exe` sits beside the
    /// game and runs for as long as somebody has the shop open, including while
    /// they are playing something else
    /// ([issue #514](https://github.com/wildware-uk/clipped/issues/514)).
    ///
    /// Deliberately **not** the executable list. An entry's executables are what
    /// the name and path rungs match; this is a statement about the launcher
    /// rung only, and it has to be, because the whole value of that rung is
    /// identifying a game whose executable name the catalogue does not know.
    #[must_use]
    pub fn not_the_game(&self) -> &[String] {
        &self.not_the_game
    }

    /// Whether `executable_name` is one of them.
    ///
    /// Compared without case, as every executable name in this module is:
    /// Windows does not distinguish them and a catalogue written by hand should
    /// not have to.
    #[must_use]
    pub fn is_not_the_game(&self, executable_name: &str) -> bool {
        self.not_the_game
            .iter()
            .any(|name| name.eq_ignore_ascii_case(executable_name))
    }
}

/// What is known about capturing a game.
///
/// This is a record of what somebody verified, not a prediction. The only
/// honest value for a game nobody has tried is [`Self::Unknown`], and every
/// entry in the shipped seed data says exactly that (AGENTS.md section 18).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum CaptureSupport {
    /// Nobody has recorded a result for this game.
    #[default]
    Unknown,
    /// Windows Graphics Capture was verified to work.
    GraphicsCapture,
    /// Windows Graphics Capture does not work and desktop duplication does.
    DesktopDuplication,
    /// Neither works, for a reason the note should give.
    Unsupported,
}

impl CaptureSupport {
    /// The value as it is written in the catalogue files.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::GraphicsCapture => "graphics-capture",
            Self::DesktopDuplication => "desktop-duplication",
            Self::Unsupported => "unsupported",
        }
    }
}

impl fmt::Display for CaptureSupport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A capture finding and the evidence behind it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaptureCompatibility {
    support: CaptureSupport,
    note: Option<String>,
}

impl CaptureCompatibility {
    /// A finding with no note.
    pub(crate) const fn new(support: CaptureSupport) -> Self {
        Self {
            support,
            note: None,
        }
    }

    /// The same finding, with the note that explains it.
    #[must_use]
    pub(crate) fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// What was found.
    #[must_use]
    pub const fn support(&self) -> CaptureSupport {
        self.support
    }

    /// Why, where somebody wrote it down.
    #[must_use]
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }
}

/// One value from an entry's `default_settings` table.
///
/// Deliberately only the four scalar types. The catalogue validates that a
/// default setting is a value and not a nested structure, and stops there:
/// which keys exist and what they mean is per-game configuration, which is M7
/// (SPEC.md section 31). A nested table would be a shape M7 then had to accept
/// for compatibility, so it is refused now while refusing it is free.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingValue {
    /// A string.
    Text(String),
    /// An integer.
    Integer(i64),
    /// A floating-point number.
    Float(f64),
    /// A boolean.
    Boolean(bool),
}

impl fmt::Display for SettingValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text(value) => formatter.write_str(value),
            Self::Integer(value) => write!(formatter, "{value}"),
            Self::Float(value) => write!(formatter, "{value}"),
            Self::Boolean(value) => write!(formatter, "{value}"),
        }
    }
}

/// The user's decision about an entry, as it was applied to that entry.
///
/// Held on the entry it changed rather than kept in a list beside it, so that
/// anything holding an [`Entry`] — a settings screen listing games, a
/// diagnostics screen explaining a match — can say *why* it reads the way it
/// does without having to consult a second structure and get the join right.
///
/// The entry keeps its shipped shape underneath: only [`Entry::name`] is
/// replaced, and [`Self::renamed_from`] is what it was, so "reset to the name
/// Clipped ships" is an offer a screen can make without knowing anything else.
/// See [`super::decision`] for why a rename and an exclusion are stored this way
/// rather than as a replacement entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedDecision {
    pub(crate) path: PathBuf,
    pub(crate) renamed_from: Option<String>,
    pub(crate) excluded: bool,
}

impl AppliedDecision {
    /// The user's file the decision was read from.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The name the entry had before the user renamed it.
    ///
    /// `None` when the decision did not rename anything.
    #[must_use]
    pub fn renamed_from(&self) -> Option<&str> {
        self.renamed_from.as_deref()
    }

    /// Whether the user asked for this game never to be recorded.
    #[must_use]
    pub const fn is_excluded(&self) -> bool {
        self.excluded
    }
}

/// One game, as the catalogue knows it.
///
/// The field list is SPEC.md section 6's, in the order it gives them.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub(crate) game_id: GameId,
    pub(crate) name: String,
    pub(crate) executables: Vec<ExecutableRule>,
    pub(crate) launcher: Option<Launcher>,
    pub(crate) icon: Option<String>,
    pub(crate) capture: CaptureCompatibility,
    pub(crate) child_processes: Vec<String>,
    pub(crate) default_settings: BTreeMap<String, SettingValue>,
    pub(crate) highlight_providers: Vec<String>,
    pub(crate) source: EntrySource,
    pub(crate) decision: Option<AppliedDecision>,
}

impl Entry {
    /// The game's stable identifier.
    #[must_use]
    pub const fn game_id(&self) -> &GameId {
        &self.game_id
    }

    /// What a person calls the game.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// How the game's process is recognised. Never empty.
    #[must_use]
    pub fn executables(&self) -> &[ExecutableRule] {
        &self.executables
    }

    /// Which launcher the game comes from, where that is recorded.
    #[must_use]
    pub const fn launcher(&self) -> Option<&Launcher> {
        self.launcher.as_ref()
    }

    /// The name of an icon asset for the game, where one is recorded.
    ///
    /// Carried, not resolved: what an icon name refers to is the desktop
    /// application's business (M5), and this ticket does not decide it.
    #[must_use]
    pub fn icon(&self) -> Option<&str> {
        self.icon.as_deref()
    }

    /// What is known about capturing this game.
    #[must_use]
    pub const fn capture(&self) -> &CaptureCompatibility {
        &self.capture
    }

    /// Processes the game is known to spawn.
    ///
    /// **Not match keys.** A process named here is not thereby the game — a
    /// game's crash handler or anti-cheat service is not something to start
    /// recording. Whoever watches processes (issue #41) decides what to do
    /// with the list; the catalogue only carries it.
    #[must_use]
    pub fn child_processes(&self) -> &[String] {
        &self.child_processes
    }

    /// Settings the catalogue suggests for this game.
    ///
    /// Carried and validated as scalars; interpreted by nothing today. See the
    /// module documentation.
    #[must_use]
    pub const fn default_settings(&self) -> &BTreeMap<String, SettingValue> {
        &self.default_settings
    }

    /// Highlight providers this game is known to have.
    ///
    /// Carried and validated as non-empty unique names; interpreted by nothing
    /// today. See the module documentation.
    #[must_use]
    pub fn highlight_providers(&self) -> &[String] {
        &self.highlight_providers
    }

    /// Which file this entry came from.
    #[must_use]
    pub const fn source(&self) -> &EntrySource {
        &self.source
    }

    /// What the user decided about this game, where they decided anything.
    #[must_use]
    pub const fn decision(&self) -> Option<&AppliedDecision> {
        self.decision.as_ref()
    }

    /// Whether the user asked for this game never to be recorded.
    ///
    /// An excluded entry stays in the catalogue — it is still listed, still
    /// found by [`super::Catalogue::find_by_id`], and still names sessions
    /// recorded before the user excluded it — and simply never matches a
    /// process ([`super::matching`]).
    #[must_use]
    pub fn is_excluded(&self) -> bool {
        self.decision
            .as_ref()
            .is_some_and(AppliedDecision::is_excluded)
    }

    /// The name the user replaced, where they renamed this game.
    #[must_use]
    pub fn renamed_from(&self) -> Option<&str> {
        self.decision
            .as_ref()
            .and_then(AppliedDecision::renamed_from)
    }
}
