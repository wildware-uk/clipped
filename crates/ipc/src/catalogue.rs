//! What the recorder's game catalogue holds, as the window is told it.
//!
//! The catalogue is the recorder's: half of it compiled into the binary from
//! `crates/game-detection/data/games.toml`, half of it the user's own overlay at
//! `%LOCALAPPDATA%\Clipped\games.toml` (`docs/game-detection.md`). The window
//! has no file-system permission and may not link `clipped-game-detection`, so
//! without a command it can list nothing — which is why the Games screen drew no
//! table at all ([issue #245](https://github.com/wildware-uk/clipped/issues/245)).
//!
//! # This is what the catalogue *knows*, not what has been *recorded*
//!
//! [`LibraryGame`](crate::library::LibraryGame) is the other one and is easy to
//! confuse with this: it is what the library index counts — sittings,
//! recordings, clips, bytes — for games that have actually been played. A game
//! can be in the catalogue and never played, which is most of them, and a
//! sitting can be recorded under no catalogue entry at all, which is what
//! `game_id: None` means there. Neither is a subset of the other.
//!
//! # Which half an entry came from is part of the answer
//!
//! A user looking at this list needs to know which entries are theirs, because
//! those are the ones they can change and the ones an update will not replace.
//! `docs/game-detection.md` is explicit that the seed data is "replaced
//! wholesale" on update and the overlay is "never touched", so a list that did
//! not say which was which would be describing two different things in one
//! column.

use serde::{Deserialize, Serialize};

/// Where a catalogue entry came from.
///
/// Deliberately not a boolean. "Shipped" and "the user's own" are not the only
/// two answers this can ever have — an entry the user *changed* is a shipped one
/// with a decision applied over it (`docs/game-detection.md`) — so this is a
/// vocabulary a later variant can join without every reader having to be found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum EntrySource {
    /// Compiled into this build, from `crates/game-detection/data/games.toml`.
    ///
    /// Replaced wholesale by every update, so a change to one of these does not
    /// survive.
    Shipped,
    /// From the user's own overlay, which no update touches.
    User,
}

/// How a game is recognised.
///
/// One of these per executable rule the entry carries. The path fragment is what
/// tells two games apart that ship the same executable name — `hl2.exe` is both
/// Half-Life 2 and Team Fortress 2 — and is absent for an entry that asked for
/// nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogueExecutable {
    /// The file name, never a path.
    pub name: String,
    /// The directories the image path must contain, where the entry demands
    /// any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_contains: Option<String>,
}

/// One game the recorder knows about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogueGame {
    /// The identifier, which is also the key a per-game setting is written
    /// under.
    pub game_id: String,
    /// What a person calls it.
    pub name: String,
    /// Which half of the catalogue it came from.
    pub source: EntrySource,
    /// How it is recognised, in the order the entry lists.
    pub executables: Vec<CatalogueExecutable>,
    /// Which shop installed it, where the entry says: `steam`, `epic`, `xbox`,
    /// `battle-net`, `ea`, `ubisoft`, `riot`, `gog` or `other`.
    ///
    /// Absent for an entry that names no launcher, which is not the same as one
    /// that names a launcher and no identifier for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launcher: Option<String>,
    /// That launcher's own identifier for the game, where the entry records
    /// one.
    ///
    /// This is the rung matched before the executable is consulted at all, so an
    /// entry that has one is recognised even when the game renames its binary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launcher_app_id: Option<String>,
    /// Whether the user excluded it, so nothing of it is recorded.
    ///
    /// An exclusion is a decision *about* an entry rather than the deletion of
    /// one: the entry stays and is still listed, which is what stops an update
    /// resurrecting a game somebody excluded (`docs/game-detection.md`).
    pub excluded: bool,
}

/// Register a game the catalogue does not know.
///
/// Written to the user's overlay and never to the shipped seed data, which
/// every update replaces wholesale (`docs/game-detection.md`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterGame {
    /// What the person calls it.
    pub name: String,
    /// The executable file name, never a path.
    ///
    /// A full path here would be a path the recorder cannot use: the catalogue
    /// matches on the image's file name, and on a fragment of its directory
    /// where one is needed to tell two games apart.
    pub executable: String,
    /// A directory fragment the image path must contain, where one is needed.
    ///
    /// Absent unless the executable name is ambiguous — `hl2.exe` is both
    /// Half-Life 2 and Team Fortress 2 — which is the case this exists for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_contains: Option<String>,
}

/// Change what a game is called, or put its shipped name back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameGame {
    /// Which entry, shipped or the user's own.
    pub game_id: String,
    /// The new name, or [`None`] to drop the user's name and use the one the
    /// entry shipped with.
    ///
    /// Deliberately one command rather than two: "rename" and "put the name
    /// back" are the same decision seen from either end, and a client that had
    /// to choose between two commands would be the place the two could get out
    /// of step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Exclude a game from recording, or stop excluding it.
///
/// An exclusion is a decision *about* an entry and not the deletion of one: the
/// entry stays and is still listed, which is what stops an update resurrecting a
/// game somebody excluded (`docs/game-detection.md`). [`ForgetGame`] is the
/// other operation and is not this one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetGameExcluded {
    /// Which entry.
    pub game_id: String,
    /// Whether nothing of it should be recorded.
    pub excluded: bool,
}

/// Drop an entry the user added.
///
/// Only ever their own: a shipped entry cannot be forgotten, because the next
/// update would bring it back and the person would have to forget it again.
/// Excluding it is the operation that lasts ([`SetGameExcluded`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForgetGame {
    /// Which entry.
    pub game_id: String,
}
