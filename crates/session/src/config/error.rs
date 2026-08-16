//! What goes wrong reading or setting configuration, and what the user can do
//! about it.
//!
//! Every message here names the setting, the value that was offered and what
//! would have been accepted, because "Invalid configuration" tells a user
//! nothing they can act on (AGENTS.md sections 15 and 45). The two levels are
//! deliberately separate: a [`SettingError`] is about one value and is what a
//! settings screen shows next to the control the user just typed in, and a
//! [`ConfigurationError`] adds where it came from — which file, which section —
//! which is what somebody reading a log line needs.

use core::fmt;
use std::path::PathBuf;

use clipped_hotkeys::{Hotkey, HotkeyAction};

use crate::config::value::SettingKey;

/// One value a setting cannot take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingError {
    /// A value of the right kind, outside the range the setting allows.
    OutOfRange {
        /// Which setting.
        key: SettingKey,
        /// What was offered, already phrased for a sentence.
        value: String,
        /// What would have been accepted.
        accepted: String,
    },
    /// A value the setting does not recognise at all.
    Unrecognised {
        /// Which setting.
        key: SettingKey,
        /// What was offered.
        value: String,
        /// The values it does recognise.
        accepted: &'static str,
    },
    /// A value of the wrong JSON type — a string where a number belongs.
    WrongType {
        /// Which setting.
        key: SettingKey,
        /// What the setting is written as.
        expected: &'static str,
        /// What was found instead.
        found: &'static str,
    },
    /// A combination pointed at two actions at once.
    ///
    /// Caught here rather than by Windows, which reports only that a
    /// registration failed (`clipped_hotkeys::Bindings`).
    HotkeyConflict {
        /// The combination.
        hotkey: Hotkey,
        /// The action being bound.
        action: HotkeyAction,
        /// The action that already holds it, which may be a default.
        held_by: HotkeyAction,
    },
    /// An older document spelled one setting two ways and this build cannot
    /// tell which the user meant. Nothing is discarded; the file is left as it
    /// is (AGENTS.md section 56).
    AmbiguousMigration {
        /// The older spelling.
        from: &'static str,
        /// The setting it migrates to, which is also present.
        to: SettingKey,
    },
}

impl fmt::Display for SettingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfRange {
                key,
                value,
                accepted,
            } => write!(formatter, "{key} is {value}; it accepts {accepted}"),
            Self::Unrecognised {
                key,
                value,
                accepted,
            } => write!(formatter, "{key} is {value}; it accepts {accepted}"),
            Self::WrongType {
                key,
                expected,
                found,
            } => write!(
                formatter,
                "{key} is written as {expected} and this one is {found}"
            ),
            Self::HotkeyConflict {
                hotkey,
                action,
                held_by,
            } => write!(
                formatter,
                "{hotkey} cannot be bound to {} because {} already uses it; unbind that first",
                action.name(),
                held_by.name()
            ),
            Self::AmbiguousMigration { from, to } => write!(
                formatter,
                "both \"{from}\" and \"{to}\" are set, and \"{from}\" is the older name for \
                 \"{to}\"; remove one of them"
            ),
        }
    }
}

impl std::error::Error for SettingError {}

/// Which part of a settings file a problem is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Section {
    /// The `global` object.
    Global,
    /// One game's object under `games`.
    Game(String),
    /// The `hotkeys` object.
    Hotkeys,
    /// The document itself.
    Document,
}

impl fmt::Display for Section {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Global => formatter.write_str("the global settings"),
            Self::Game(game) => write!(formatter, "the settings for {game}"),
            Self::Hotkeys => formatter.write_str("the hotkeys"),
            Self::Document => formatter.write_str("the file"),
        }
    }
}

/// Why a settings file could not be read, understood or written.
///
/// Every variant leaves the file on disk exactly as it was. Nothing here is
/// recovered from by rewriting the user's settings, because a file that cannot
/// be understood is far more likely to be one this build is too old for than
/// one that is worthless (AGENTS.md section 56). That is why refusing to *read*
/// a file has a counterpart in [`Self::WouldOverwrite`]: a refusal to read only
/// preserves anything if the next save refuses as well.
#[derive(Debug)]
pub enum ConfigurationError {
    /// The file exists and could not be read.
    Read {
        /// The file.
        path: PathBuf,
        /// What the filesystem said.
        source: std::io::Error,
    },
    /// The file could not be written.
    Write {
        /// The file.
        path: PathBuf,
        /// What the filesystem said.
        source: std::io::Error,
    },
    /// The file is not JSON.
    Syntax {
        /// The file.
        path: PathBuf,
        /// The line the parser stopped on, counting from one.
        line: usize,
        /// The column it stopped at, counting from one.
        column: usize,
        /// What the parser said.
        message: String,
    },
    /// The file was written by a newer Clipped.
    ///
    /// It is not migrated backwards and it is not overwritten: the user's other
    /// installation still has to be able to read it.
    UnsupportedVersion {
        /// The file.
        path: PathBuf,
        /// The version it declares.
        found: u64,
        /// The newest version this build writes.
        supported: u32,
    },
    /// A key that has to be an object or a string is something else.
    Malformed {
        /// The file.
        path: PathBuf,
        /// Where in it.
        section: Section,
        /// What was wrong, phrased for a sentence.
        detail: String,
    },
    /// One setting in the file holds a value it cannot take.
    Invalid {
        /// The file.
        path: PathBuf,
        /// Where in it.
        section: Section,
        /// Which value, and what it should have been.
        source: SettingError,
    },
    /// A `storage` limit is outside the bounds `clipped-library` sets.
    ///
    /// Separate from [`Self::Invalid`] because the bounds are not this crate's:
    /// a quota below a gigabyte and a maximum age below a day are refused where
    /// they are defined, and this carries that refusal rather than restating it
    /// ([issue #111](https://github.com/wildware-uk/clipped/issues/111)).
    InvalidStorageLimit {
        /// The file.
        path: PathBuf,
        /// Which key carried it.
        key: &'static str,
        /// What the library said about the value.
        source: super::storage::StorageProblem,
    },
    /// Saving would have replaced a settings file this build cannot read.
    ///
    /// Refusing to read such a file is only half of preserving it. The other
    /// half is here: the settings in it belong to whoever wrote it — most
    /// likely a Clipped one version ahead, on the user's other machine — and
    /// rendering this build's configuration over them would destroy every key
    /// they hold, which is precisely what AGENTS.md section 56 forbids.
    ///
    /// The recovery is in the message and is the user's to make: move that
    /// file aside, and the next save starts again from the defaults.
    WouldOverwrite {
        /// The file that was left alone.
        path: PathBuf,
        /// Why it could not be read, which is what says how to recover.
        source: Box<Self>,
    },
}

impl fmt::Display for ConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => {
                write!(formatter, "failed to read {}: {source}", path.display())
            }
            Self::Write { path, source } => {
                write!(formatter, "failed to write {}: {source}", path.display())
            }
            Self::Syntax {
                path,
                line,
                column,
                message,
            } => write!(
                formatter,
                "{} is not valid JSON at line {line}, column {column}: {message}",
                path.display()
            ),
            Self::UnsupportedVersion {
                path,
                found,
                supported,
            } => write!(
                formatter,
                "{} is settings version {found} and this build understands up to {supported}; \
                 update Clipped, or move the file aside to start again from the defaults",
                path.display()
            ),
            Self::Malformed {
                path,
                section,
                detail,
            } => write!(
                formatter,
                "{} cannot be used: {detail} in {section}",
                path.display()
            ),
            Self::Invalid {
                path,
                section,
                source,
            } => write!(
                formatter,
                "{} cannot be used: in {section}, {source}",
                path.display()
            ),
            Self::InvalidStorageLimit { path, key, source } => write!(
                formatter,
                "{} cannot be used: `storage.{key}` is not a limit Clipped will act on: {source}",
                path.display()
            ),
            // The path is not repeated here: `source` names it, and this
            // message is already the longest one in the enum.
            Self::WouldOverwrite { source, .. } => write!(
                formatter,
                "the settings were not saved: {source}. The file was left exactly as it is, \
                 because saving over settings this build cannot read would destroy them; \
                 move it aside to start again from the defaults"
            ),
        }
    }
}

impl std::error::Error for ConfigurationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } | Self::Write { source, .. } => Some(source),
            Self::Invalid { source, .. } => Some(source),
            Self::InvalidStorageLimit { source, .. } => Some(source),
            Self::WouldOverwrite { source, .. } => Some(source),
            Self::Syntax { .. } | Self::UnsupportedVersion { .. } | Self::Malformed { .. } => None,
        }
    }
}
