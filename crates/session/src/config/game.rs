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
    /// The rule itself lives in `GameId::parse` and is not repeated here
    /// (issue #246). What this adds is the typed rejection a settings file
    /// needs: the catalogue's parser answers `None`, and a person editing
    /// `settings.json` has to be told which key was refused.
    ///
    /// A key is deliberately **not** required to name a game the catalogue
    /// knows. Settings for a game that is not listed — one added by a later
    /// build, or one somebody registered and then removed — must still load,
    /// resolve and save (AGENTS.md section 56), so this checks the shape of the
    /// name and nothing else.
    pub fn parse(value: &str) -> Result<Self, InvalidGameKey> {
        clipped_game_detection::catalogue::GameId::parse(value)
            .map(|identifier| Self(identifier.as_str().to_owned()))
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

    /// A game the catalogue does not list still has usable settings.
    ///
    /// The second acceptance criterion of
    /// [issue #246](https://github.com/wildware-uk/clipped/issues/246), and the
    /// property that decides how `GameKey::parse` is allowed to be implemented:
    /// it checks the *shape* of a name and never asks the catalogue whether the
    /// game exists.
    ///
    /// Delegating the rule to `GameId::parse` would be the obvious place to
    /// acquire a catalogue lookup by accident, and doing so would silently
    /// discard the settings of a game added by a later build, or one somebody
    /// registered and then removed (AGENTS.md section 56).
    #[test]
    fn settings_for_a_game_the_catalogue_does_not_list_still_load_and_resolve() {
        let catalogue = clipped_game_detection::catalogue::Catalogue::seed()
            .expect("the shipped catalogue parses");
        let stranger = GameKey::parse("a-game-no-catalogue-lists")
            .expect("a well-formed name is a key whether or not a game has it");
        assert!(
            catalogue.find_by_id(stranger.as_str()).is_none(),
            "this test is worthless if the catalogue happens to list it"
        );

        let mut configuration = crate::config::Configuration::defaults();
        let mut preferences = crate::config::Preferences::default();
        preferences
            .set_framerate(Some(120))
            .expect("120 is a framerate the settings accept");
        configuration.set_game(stranger.clone(), preferences);

        // Held.
        assert_eq!(
            configuration
                .game(&stranger)
                .and_then(crate::config::Preferences::framerate),
            Some(120),
            "settings for an unlisted game have to be readable back"
        );

        // Resolved, rather than falling through to the global answer.
        assert_eq!(
            configuration.resolve_for(&stranger).framerate().get(),
            120,
            "and they have to win over the global setting for that game"
        );

        // And survive being written and read again, which is where a lookup
        // against the catalogue would quietly drop them.
        let written = crate::config::document::render(&configuration);
        let (reloaded, _) =
            crate::config::document::parse(std::path::Path::new("settings.json"), &written)
                .expect("what render wrote, parse reads");
        assert_eq!(
            reloaded
                .game(&stranger)
                .and_then(crate::config::Preferences::framerate),
            Some(120),
            "a game the catalogue does not know must not lose its settings on a save"
        );
    }

    #[test]
    fn every_catalogue_identifier_is_a_valid_settings_key() {
        // There is no longer a duplicated rule for this to hold together —
        // `GameKey::parse` delegates to `GameId::parse` (issue #246) — so what
        // it now guards is the shipped data rather than the agreement: an entry
        // whose `game_id` the rule rejects would be a game whose settings
        // nobody could name, and that is a defect in the catalogue file rather
        // than in either parser.
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
