//! How a settings file names a game.
//!
//! # Why this is not `clipped_game_detection::GameId`
//!
//! It is the same identifier, spelled by the same rule — lower-case ASCII
//! letters, digits and hyphens — and a [`GameKey`] is built from a `GameId`
//! infallibly for exactly that reason. What it is not is the same *type*, for
//! two reasons that both point the same way:
//!
//! - `GameId::parse` is crate-private to `clipped-game-detection`, so nothing
//!   outside it can turn text into one. A settings file is text.
//! - Settings must outlive the catalogue's opinion of a game. A user who
//!   removes a game from their overlay, or who is running a build whose seed
//!   catalogue no longer lists it, must not thereby lose the settings they
//!   chose for it (AGENTS.md section 56). So the settings file names games it
//!   can hold settings for, and the catalogue answers a different question.
//!
//! The duplicated character rule is the cost, and
//! `every_catalogue_identifier_is_a_valid_settings_key` is what keeps the two
//! from drifting apart: it parses every identifier the shipped catalogue
//! contains. Giving `GameId` a public parser so this can be deleted is
//! [issue #246](https://github.com/wildware-uk/clipped/issues/246).

use core::fmt;
use std::str::FromStr;

use clipped_game_detection::catalogue::GameId;

/// A game, as a settings file names it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GameKey(String);

impl GameKey {
    /// Builds a key, having checked its characters.
    ///
    /// # Errors
    ///
    /// [`InvalidGameKey`] when the text is empty or holds anything outside
    /// `[a-z0-9-]`. The rule is not tidiness: an identifier that differs from
    /// another only by case or by a space is one that two files will disagree
    /// about, and this one ends up as a key in the user's settings file.
    pub fn parse(value: &str) -> Result<Self, InvalidGameKey> {
        if value.is_empty() {
            return Err(InvalidGameKey {
                value: value.to_owned(),
            });
        }
        value
            .chars()
            .all(|character| {
                character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
            })
            .then(|| Self(value.to_owned()))
            .ok_or_else(|| InvalidGameKey {
                value: value.to_owned(),
            })
    }

    /// The identifier as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&GameId> for GameKey {
    /// The catalogue's identifier for the same game.
    ///
    /// Infallible because `GameId` has already applied the same rule; the
    /// characters are re-checked in a debug build so that a rule which drifts
    /// is found by the test suite rather than by a user's file.
    fn from(id: &GameId) -> Self {
        debug_assert!(
            Self::parse(id.as_str()).is_ok(),
            "the catalogue produced an identifier a settings file cannot name: {id}"
        );
        Self(id.as_str().to_owned())
    }
}

impl FromStr for GameKey {
    type Err = InvalidGameKey;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl fmt::Display for GameKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Text a settings file used as a game identifier that is not one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidGameKey {
    value: String,
}

impl InvalidGameKey {
    /// The text that was offered.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }
}

impl fmt::Display for InvalidGameKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "\"{}\" is not a game identifier; a game is named by lower-case letters, digits and \
             hyphens, as in \"counter-strike-2\"",
            self.value
        )
    }
}

impl std::error::Error for InvalidGameKey {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_identifier_rule_is_the_catalogues() {
        assert_eq!(
            GameKey::parse("counter-strike-2").map(|key| key.as_str().to_owned()),
            Ok("counter-strike-2".to_owned())
        );
        for rejected in ["", "Counter-Strike-2", "counter strike 2", "cs2!", "cs_2"] {
            assert!(
                GameKey::parse(rejected).is_err(),
                "{rejected:?} should not be a game key"
            );
        }
    }

    #[test]
    fn the_rejection_says_what_a_game_identifier_looks_like() {
        let error = GameKey::parse("Counter Strike").expect_err("a capital and a space");
        let message = error.to_string();
        assert!(
            message.contains("Counter Strike") && message.contains("counter-strike-2"),
            "the rejection must name the value and show a good one: {message}"
        );
    }

    #[test]
    fn every_catalogue_identifier_is_a_valid_settings_key() {
        // The one thing holding the duplicated character rule to the
        // catalogue's. If `GameId` ever admits a character this rejects, a
        // user's per-game settings would become unnameable, and this fails
        // first.
        let catalogue = clipped_game_detection::catalogue::Catalogue::seed()
            .expect("the shipped catalogue parses");
        assert!(
            !catalogue.entries().is_empty(),
            "the seed catalogue should not be empty"
        );
        for entry in catalogue.entries() {
            let id = entry.game_id();
            GameKey::parse(id.as_str())
                .unwrap_or_else(|error| panic!("{id} is not a settings key: {error}"));
            assert_eq!(GameKey::from(id).as_str(), id.as_str());
        }
    }
}
