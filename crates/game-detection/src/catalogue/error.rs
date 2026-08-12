//! Why a catalogue would not load.
//!
//! Every variant here names the file it is about, and every variant that is
//! about one entry names the entry — by its position in the file and, where it
//! got far enough to have one, by its `game_id`. That is the requirement issue
//! #42 sets and the reason none of this is a `bool`: a contributor whose pull
//! request fails, or a user whose own file has a typo in it, has to be able to
//! go straight to the line without reading any Rust (AGENTS.md section 15).
//!
//! Nothing here is recoverable by skipping. A malformed entry is never dropped
//! and the rest of the file loaded, because a game silently missing from the
//! catalogue is a recording that silently does not happen.

use std::error::Error;
use std::fmt;
use std::io;
use std::path::PathBuf;

use super::entry::EntrySource;

/// Where in a file an entry was.
///
/// The position is one-based and counts `[[game]]` blocks, because that is
/// what a person counts. The `game_id` is present whenever the entry had a
/// usable one, which is every problem except a broken identifier itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryLocation {
    position: usize,
    game_id: Option<String>,
}

impl EntryLocation {
    /// An entry identified only by where it is.
    #[must_use]
    pub(crate) const fn at(position: usize) -> Self {
        Self {
            position,
            game_id: None,
        }
    }

    /// An entry that also knows what it calls itself.
    #[must_use]
    pub(crate) fn named(position: usize, game_id: impl Into<String>) -> Self {
        Self {
            position,
            game_id: Some(game_id.into()),
        }
    }

    /// Which `[[game]]` block this is, counting from one.
    #[must_use]
    pub const fn position(&self) -> usize {
        self.position
    }

    /// The entry's `game_id`, where it had a readable one.
    #[must_use]
    pub fn game_id(&self) -> Option<&str> {
        self.game_id.as_deref()
    }
}

impl fmt::Display for EntryLocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.game_id {
            Some(game_id) => write!(formatter, "entry {} (`{game_id}`)", self.position),
            None => write!(formatter, "entry {}", self.position),
        }
    }
}

/// What is wrong with one entry.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum EntryProblem {
    /// `game_id` is empty.
    GameIdEmpty,
    /// `game_id` has characters outside `[a-z0-9-]`.
    GameIdCharacters {
        /// What was written.
        game_id: String,
    },
    /// Two entries in the same file claim the same `game_id`.
    GameIdDuplicated {
        /// The position of the entry that claimed it first.
        first_at: usize,
    },
    /// `name` is empty or only whitespace.
    NameEmpty,
    /// The entry has no `[[game.executables]]` block, so nothing could ever
    /// match it.
    NoExecutables,
    /// An executable's `name` is empty.
    ExecutableNameEmpty {
        /// Which `[[game.executables]]` block, counting from one.
        position: usize,
    },
    /// An executable's `name` contains a directory separator. The file name
    /// goes in `name` and the directory goes in `path_contains`; a name with a
    /// separator in it would match nothing, because a process's image name has
    /// no directory part.
    ExecutableNameIsPath {
        /// Which `[[game.executables]]` block, counting from one.
        position: usize,
        /// What was written.
        name: String,
    },
    /// An executable's `path_contains` is empty, which would qualify nothing
    /// and is almost certainly a half-finished edit.
    PathQualifierEmpty {
        /// Which `[[game.executables]]` block, counting from one.
        position: usize,
    },
    /// The same executable rule appears twice in one entry.
    ExecutableDuplicated {
        /// The rule, as it would be written.
        rule: String,
    },
    /// A `child_processes` name is empty or contains a directory separator.
    ChildProcessInvalid {
        /// What was written.
        name: String,
    },
    /// The launcher's `app_id` is present but empty. Omit the key instead.
    LauncherAppIdEmpty,
    /// `icon` is present but empty. Omit the key instead.
    IconEmpty,
    /// A capture `note` is present but empty. Omit the key instead.
    CaptureNoteEmpty,
    /// A highlight provider name is empty.
    HighlightProviderEmpty,
    /// The same highlight provider is named twice.
    HighlightProviderDuplicated {
        /// The name.
        provider: String,
    },
    /// A default setting is not a string, integer, float or boolean.
    DefaultSettingNotScalar {
        /// The key.
        key: String,
        /// What it was instead, in TOML's vocabulary.
        found: &'static str,
    },
}

impl fmt::Display for EntryProblem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GameIdEmpty => formatter.write_str("`game_id` is empty"),
            Self::GameIdCharacters { game_id } => write!(
                formatter,
                "`game_id` is `{game_id}`, which is not made only of lower-case letters, \
                 digits and hyphens"
            ),
            Self::GameIdDuplicated { first_at } => write!(
                formatter,
                "this `game_id` is already used by entry {first_at} of the same file"
            ),
            Self::NameEmpty => formatter.write_str("`name` is empty"),
            Self::NoExecutables => formatter.write_str(
                "it has no `[[game.executables]]` block, so no process could ever match it",
            ),
            Self::ExecutableNameEmpty { position } => {
                write!(formatter, "executable {position} has an empty `name`")
            }
            Self::ExecutableNameIsPath { position, name } => write!(
                formatter,
                "executable {position} has `name = \"{name}\"`, which contains a directory \
                 separator; `name` is the file name alone and the directory belongs in \
                 `path_contains`"
            ),
            Self::PathQualifierEmpty { position } => write!(
                formatter,
                "executable {position} has an empty `path_contains`; omit the key to match the \
                 name anywhere"
            ),
            Self::ExecutableDuplicated { rule } => {
                write!(formatter, "the executable rule {rule} appears twice")
            }
            Self::ChildProcessInvalid { name } => write!(
                formatter,
                "the child process `{name}` is not a bare executable file name"
            ),
            Self::LauncherAppIdEmpty => {
                formatter.write_str("`[game.launcher] app_id` is empty; omit the key instead")
            }
            Self::IconEmpty => formatter.write_str("`icon` is empty; omit the key instead"),
            Self::CaptureNoteEmpty => {
                formatter.write_str("`[game.capture] note` is empty; omit the key instead")
            }
            Self::HighlightProviderEmpty => {
                formatter.write_str("a `highlight_providers` name is empty")
            }
            Self::HighlightProviderDuplicated { provider } => write!(
                formatter,
                "the highlight provider `{provider}` is named twice"
            ),
            Self::DefaultSettingNotScalar { key, found } => write!(
                formatter,
                "the default setting `{key}` is {found}; a default setting must be a string, \
                 integer, float or boolean"
            ),
        }
    }
}

/// What is wrong with one `[[decision]]` block.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DecisionProblem {
    /// `game_id` is empty or has characters outside `[a-z0-9-]`, so it names no
    /// game the catalogue could ever have.
    GameIdInvalid,
    /// Two decisions in the same file are about the same game.
    Duplicated {
        /// The position of the decision that claimed it first.
        first_at: usize,
    },
    /// `name` is present but empty. A game with no name cannot be shown.
    NameEmpty,
    /// The block has neither `name` nor `excluded`, so it decides nothing.
    Empty,
}

impl fmt::Display for DecisionProblem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GameIdInvalid => formatter.write_str(
                "`game_id` is not made only of lower-case letters, digits and hyphens, so it \
                 names no game",
            ),
            Self::Duplicated { first_at } => write!(
                formatter,
                "decision {first_at} of the same file is already about this game; put both \
                 changes in one block"
            ),
            Self::NameEmpty => formatter.write_str(
                "`name` is empty; omit the key to keep the \
                 name Clipped ships",
            ),
            Self::Empty => formatter.write_str(
                "it has neither `name` nor `excluded`, so it decides nothing; delete the block \
                 or say what it changes",
            ),
        }
    }
}

/// Why a catalogue would not load.
#[derive(Debug)]
#[non_exhaustive]
pub enum CatalogueError {
    /// The overlay file exists but could not be read.
    Read {
        /// The file.
        path: PathBuf,
        /// What the filesystem said.
        source: io::Error,
    },
    /// The file is not TOML. The message carries the parser's line and column.
    Syntax {
        /// Which file.
        file: EntrySource,
        /// The parser's account of it.
        message: String,
    },
    /// The file has no readable `schema_version`.
    SchemaVersionMissing {
        /// Which file.
        file: EntrySource,
    },
    /// The file was written by a newer Clipped than this one.
    ///
    /// Nothing is written back, ever: a build that does not understand a file
    /// is the last build that should be rewriting it (AGENTS.md section 56).
    SchemaTooNew {
        /// Which file.
        file: EntrySource,
        /// The version in the file.
        found: u32,
        /// The newest version this build understands.
        supported: u32,
    },
    /// An older file has no route to the current schema, which is a gap in
    /// this crate's migration list rather than a fault in the user's file.
    MigrationMissing {
        /// Which file.
        file: EntrySource,
        /// The version reached before the chain ran out.
        from: u32,
        /// The version it needed to reach.
        to: u32,
    },
    /// A migration ran and refused what it found.
    MigrationFailed {
        /// Which file.
        file: EntrySource,
        /// The version being migrated from.
        from: u32,
        /// The version being migrated to.
        to: u32,
        /// Why the step refused.
        reason: String,
    },
    /// A migrated file could not be copied aside before being rewritten, so it
    /// was not rewritten.
    BackupFailed {
        /// The file that was to be backed up.
        path: PathBuf,
        /// Where the copy was to go.
        backup: PathBuf,
        /// What the filesystem said.
        source: io::Error,
    },
    /// A migrated file could not be written back. The backup taken immediately
    /// before still holds the original.
    WriteFailed {
        /// The file.
        path: PathBuf,
        /// What the filesystem said.
        source: io::Error,
    },
    /// An entry parsed but is not usable.
    InvalidEntry {
        /// Which file.
        file: EntrySource,
        /// Which entry.
        entry: EntryLocation,
        /// What is wrong with it.
        problem: EntryProblem,
    },
    /// A `[[decision]]` block parsed but is not usable.
    InvalidDecision {
        /// Which file.
        file: EntrySource,
        /// Which `[[decision]]` block, counting from one.
        position: usize,
        /// The `game_id` as written, which is the only thing identifying the
        /// block to whoever has to go and fix it — and may itself be the fault.
        game_id: String,
        /// What is wrong with it.
        problem: DecisionProblem,
    },
    /// The shipped catalogue carries a `[[decision]]` block.
    ///
    /// A decision records what a *user* decided about an entry somebody else
    /// wrote. Clipped shipping one would be Clipped disagreeing with its own
    /// catalogue, where the fix is to change the entry.
    DecisionInSeedData {
        /// Which `[[decision]]` block, counting from one.
        position: usize,
    },
    /// The user's overlay was not replaced, because this build could not read
    /// what is already in it.
    ///
    /// Refusing to *read* a file a newer build wrote preserves nothing on its
    /// own: the user opens the settings screen, sees a catalogue without their
    /// entries in it, changes one thing, and the save is what destroys the
    /// file. So every edit reads the file first and refuses on anything it
    /// could not have loaded (AGENTS.md section 56).
    WouldOverwrite {
        /// The file that was left exactly as it is.
        path: PathBuf,
        /// Why it could not be read.
        source: Box<CatalogueError>,
    },
    /// An edit would have produced an overlay this build could not read back.
    ///
    /// The document is validated through the same reader that will load it
    /// before anything is written, so a rename to a name that is not one, or a
    /// registration colliding with an entry already in the file, fails with the
    /// message the loader would have given and leaves the file alone.
    WouldWriteInvalid {
        /// The file that was left exactly as it is.
        path: PathBuf,
        /// What the reader made of the document that was about to be written.
        source: Box<CatalogueError>,
    },
    /// A registration named a game whose identifier cannot be derived from its
    /// name, and no identifier was given.
    NoIdentifierFromName {
        /// The name that was offered.
        name: String,
    },
    /// The overlay loads, and is written in a shape an edit cannot change
    /// without rewriting the whole file.
    CannotEdit {
        /// The file that was left exactly as it is.
        path: PathBuf,
        /// What about it cannot be edited.
        detail: String,
    },
}

impl fmt::Display for CatalogueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => {
                write!(formatter, "{} could not be read: {source}", path.display())
            }
            Self::Syntax { file, message } => {
                write!(formatter, "{file} is not valid TOML: {message}")
            }
            Self::SchemaVersionMissing { file } => write!(
                formatter,
                "{file} has no `schema_version`; a catalogue file must start with \
                 `schema_version = {}`",
                super::SCHEMA_VERSION
            ),
            Self::SchemaTooNew {
                file,
                found,
                supported,
            } => write!(
                formatter,
                "{file} is schema version {found} and this build of Clipped understands up to \
                 {supported}; the file has been left exactly as it is, and updating Clipped \
                 will read it"
            ),
            Self::MigrationMissing { file, from, to } => write!(
                formatter,
                "{file} is schema version {from} and this build has no migration from {from} to \
                 {to}; the file has been left exactly as it is"
            ),
            Self::MigrationFailed {
                file,
                from,
                to,
                reason,
            } => write!(
                formatter,
                "{file} could not be migrated from schema version {from} to {to}: {reason}; the \
                 file has been left exactly as it is"
            ),
            Self::BackupFailed {
                path,
                backup,
                source,
            } => write!(
                formatter,
                "{} needed migrating but could not first be copied to {}: {source}; the file \
                 has been left exactly as it is",
                path.display(),
                backup.display()
            ),
            Self::WriteFailed { path, source } => write!(
                formatter,
                "the migrated {} could not be written: {source}; the copy taken immediately \
                 before still holds the original",
                path.display()
            ),
            Self::InvalidEntry {
                file,
                entry,
                problem,
            } => write!(formatter, "{file}: {entry}: {problem}"),
            Self::InvalidDecision {
                file,
                position,
                game_id,
                problem,
            } => write!(
                formatter,
                "{file}: decision {position} (`{game_id}`): {problem}"
            ),
            Self::DecisionInSeedData { position } => write!(
                formatter,
                "the catalogue shipped with Clipped carries `[[decision]]` block {position}; a \
                 decision is what a user decided about an entry, so the fix is to change the \
                 entry itself"
            ),
            Self::WouldOverwrite { path, source } => write!(
                formatter,
                "{} was not changed, because this build of Clipped could not read what is \
                 already in it: {source}",
                path.display()
            ),
            Self::WouldWriteInvalid { path, source } => write!(
                formatter,
                "{} was not changed, because the change would have made it unreadable: \
                 {source}",
                path.display()
            ),
            Self::CannotEdit { path, detail } => write!(
                formatter,
                "{} was not changed, because {detail}; change it by hand instead",
                path.display()
            ),
            Self::NoIdentifierFromName { name } => write!(
                formatter,
                "\"{name}\" has no lower-case letters or digits in it, so it cannot be turned \
                 into a game identifier; give the game a name with some in it"
            ),
        }
    }
}

impl Error for CatalogueError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Read { source, .. }
            | Self::BackupFailed { source, .. }
            | Self::WriteFailed { source, .. } => Some(source),
            Self::WouldOverwrite { source, .. } | Self::WouldWriteInvalid { source, .. } => {
                Some(source)
            }
            Self::Syntax { .. }
            | Self::SchemaVersionMissing { .. }
            | Self::SchemaTooNew { .. }
            | Self::MigrationMissing { .. }
            | Self::MigrationFailed { .. }
            | Self::InvalidEntry { .. }
            | Self::InvalidDecision { .. }
            | Self::DecisionInSeedData { .. }
            | Self::NoIdentifierFromName { .. }
            | Self::CannotEdit { .. } => None,
        }
    }
}
