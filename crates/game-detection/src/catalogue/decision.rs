//! What the user decided about a game somebody else described.
//!
//! # Why this is not just another entry
//!
//! An overlay `[[game]]` block **replaces** the shipped entry with the same
//! identifier ([`super`]), which is the right answer when the user is
//! describing the game themselves — their executables, their paths, their name.
//! It is the wrong answer for the two things issue #45 is actually about:
//!
//! - **A rename must survive an update of the shipped catalogue.** Written as a
//!   replacement entry, calling Counter-Strike 2 "CS2" also freezes its
//!   executable list at whatever this build shipped. When a later release adds
//!   the executable Valve renamed, the user who typed a shorter name is the one
//!   person it never reaches.
//! - **An exclusion is not a deletion.** The shipped entry has to stay, because
//!   an update that re-adds a game the user excluded would otherwise resurrect
//!   it. What is stored is the user's *decision about* the entry, not the
//!   absence of one.
//!
//! So a decision is a `[[decision]]` block naming a `game_id` and saying only
//! what the user changed. Everything else about the game keeps coming from
//! whoever described it, update after update.
//!
//! ```toml
//! [[decision]]
//! game_id = "counter-strike-2"
//! name = "CS2"
//!
//! [[decision]]
//! game_id = "some-launcher"
//! excluded = true
//! ```
//!
//! # A decision outlives its game
//!
//! A decision naming a game this build's catalogue does not have is **kept**,
//! not dropped — [`super::Catalogue::pending_decisions`] reports it. Dropping
//! it is the resurrection above wearing a different hat: a user who excludes a
//! game, and then runs a build whose seed data does not list it, would find the
//! exclusion quietly gone the next time it did.

use std::path::{Path, PathBuf};

use super::entry::GameId;

/// One `[[decision]]` block: what the user decided about one game.
///
/// A decision that says nothing is refused when the file is read, so at least
/// one of [`Self::name`] and [`Self::is_excluded`] is always something other
/// than the default (see [`super::schema`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub(crate) game_id: GameId,
    pub(crate) name: Option<String>,
    pub(crate) excluded: bool,
    pub(crate) path: PathBuf,
}

impl Decision {
    /// Which game this is about.
    #[must_use]
    pub const fn game_id(&self) -> &GameId {
        &self.game_id
    }

    /// The user's file it was read from.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// What the user calls the game, where they renamed it.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Whether the user asked for this game never to be recorded.
    #[must_use]
    pub const fn is_excluded(&self) -> bool {
        self.excluded
    }
}
