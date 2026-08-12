//! The local game database: what Clipped knows about games, as data.
//!
//! # Adding a game is not a code change
//!
//! That single requirement (issue #42) decides the design. The catalogue is a
//! TOML file — `crates/game-detection/data/games.toml` — that a contributor
//! appends a `[[game]]` block to and sends as a pull request. No Rust changes,
//! nothing is registered anywhere, and the entry is picked up because the file
//! is the catalogue rather than a source of one. `docs/game-detection.md` is
//! the reference, and the file's own header is the short version.
//!
//! The seed data is compiled in with `include_str!` rather than installed
//! beside the executable. It is Clipped's content, replaced wholesale by every
//! update, and a data file next to a binary is a data file that can go missing
//! or fall out of step with the build that reads it.
//!
//! # Two files
//!
//! | | Seed data | User overlay |
//! | --- | --- | --- |
//! | Where | compiled in from `data/games.toml` | `%LOCALAPPDATA%\Clipped\games.toml` |
//! | Whose | the project's | the user's |
//! | On update | replaced entirely | never touched |
//! | If the schema changes | rewritten by us | migrated, with a backup ([`overlay`]) |
//!
//! An overlay entry whose `game_id` matches a shipped one **replaces** it
//! outright rather than being merged into it: merging two lists of executables
//! would mean a user could add to a shipped entry but never correct it, and
//! "which of the two `name` fields won" is not a question anybody should have
//! to ask of their own file.
//!
//! Overlay entries with identifiers of their own are simply additional
//! entries, and beat shipped ones only where they matched equally well — see
//! [`matching`] for the whole precedence order, which is the part of this
//! module with the most tests.
//!
//! # Nothing is skipped quietly
//!
//! Every failure here is loud and names the file and the entry. A catalogue
//! that dropped the entry it could not read would leave a user with a game
//! that silently never records, and a contributor with a green build and an
//! entry that does nothing.
//!
//! # What this module is not
//!
//! It is not a process watcher (#41), not launcher detection (#43, #44), and
//! not the registration UI (#45). It answers one question — "what game, if
//! any, is this process?" — and holds the data needed to answer it.
//!
//! It is also not in SQLite, deliberately. M6's #55 introduces the database;
//! this is reference data that ships with the application and is read once at
//! start-up, and a second persistence mechanism arrived at by accident is what
//! AGENTS.md section 55 exists to prevent.

mod entry;
mod error;
mod matching;
mod overlay;
mod schema;

use std::path::Path;

pub use entry::{
    CaptureCompatibility, CaptureSupport, Entry, EntrySource, ExecutableRule, GameId, Launcher,
    LauncherKind, SettingValue,
};
pub use error::{CatalogueError, EntryLocation, EntryProblem};
pub use matching::{Match, MatchStrength, ProcessCandidate};

/// How a path is compared, shared with the launcher providers.
///
/// Not public API: it is here so that [`crate::launcher::steam`] decides
/// whether an executable is inside a Steam application's directory by exactly
/// the rule this module uses for `path_contains` (AGENTS.md section 55). Two
/// implementations of "is this path inside that one?" that disagreed about a
/// trailing separator would be two different answers about the same game.
pub(crate) use matching::{normalise_path, segments as path_segments};
pub use overlay::{default_path as overlay_path, OverlayStatus, OVERLAY_FILE_NAME};

/// The version of the catalogue file format.
///
/// The format's, not Clipped's: it changes when the shape of an entry changes
/// and at no other time, so adding a game never touches it. Both files carry
/// it, and a file that carries a **newer** one is refused and left exactly as
/// it is — see [`overlay`] for what happens to an older one, which is the case
/// that matters, because that file belongs to the user.
pub const SCHEMA_VERSION: u32 = 1;

/// The catalogue that ships with Clipped.
///
/// Compiled in, so `cargo build` is what publishes a contributor's entry and
/// there is no install step to get wrong.
const SEED_DATA: &str = include_str!("../../data/games.toml");

/// Everything Clipped knows about games, in precedence order's raw material.
///
/// Entries keep the order they were read in, seed data first. That order is
/// not the precedence rule — [`matching`] is — but it is what makes an
/// ambiguous match report its candidates in an order a person can predict.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Catalogue {
    entries: Vec<Entry>,
}

/// A catalogue, and what happened to the user's overlay while loading it.
///
/// The status is returned rather than logged so that the caller decides how to
/// report it; a migration in particular is something the user should be told
/// about, since their file was rewritten and a copy kept.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedCatalogue {
    catalogue: Catalogue,
    overlay: OverlayStatus,
}

impl LoadedCatalogue {
    /// The catalogue.
    #[must_use]
    pub const fn catalogue(&self) -> &Catalogue {
        &self.catalogue
    }

    /// What happened to the overlay.
    #[must_use]
    pub const fn overlay(&self) -> &OverlayStatus {
        &self.overlay
    }

    /// The catalogue, when the status has been dealt with.
    #[must_use]
    pub fn into_catalogue(self) -> Catalogue {
        self.catalogue
    }
}

impl Catalogue {
    /// The catalogue shipped with this build, with no overlay.
    ///
    /// # Errors
    ///
    /// [`CatalogueError`] if the shipped data does not parse or does not
    /// validate, which would be a fault in this repository rather than
    /// anything about the machine. `the_shipped_seed_data_is_valid` asserts it
    /// does, so this cannot reach a release without a red build.
    pub fn seed() -> Result<Self, CatalogueError> {
        Self::parse(SEED_DATA, EntrySource::Seed)
    }

    /// The shipped catalogue with the user's overlay applied, from the usual
    /// place.
    ///
    /// An installation with no overlay — every installation, until somebody
    /// adds a game of their own — gets the seed data and
    /// [`OverlayStatus::Absent`].
    ///
    /// # Errors
    ///
    /// [`CatalogueError`] if either file cannot be read, parsed, migrated or
    /// validated. An overlay that exists and cannot be understood is an error
    /// rather than an empty overlay: entries the user added going quietly
    /// missing is the thing this must not do.
    pub fn load() -> Result<LoadedCatalogue, CatalogueError> {
        match overlay::default_path() {
            Some(path) => Self::load_with_overlay_at(&path),
            None => Ok(LoadedCatalogue {
                catalogue: Self::seed()?,
                overlay: OverlayStatus::NoUserDirectory,
            }),
        }
    }

    /// The same, with the overlay taken from a named file.
    ///
    /// What tests use, and what a caller with a data directory of its own
    /// would use.
    ///
    /// # Errors
    ///
    /// As [`load`](Self::load).
    pub fn load_with_overlay_at(path: &Path) -> Result<LoadedCatalogue, CatalogueError> {
        let seed = Self::seed()?;
        let (entries, overlay) = overlay::load(path, SCHEMA_VERSION, schema::MIGRATIONS)?;
        Ok(LoadedCatalogue {
            catalogue: seed.overlaid_with(Self { entries }),
            overlay,
        })
    }

    /// Reads catalogue text that came from `source`.
    ///
    /// Public because writing an entry and finding out whether it is valid is
    /// the same operation, and the registration UI (#45) will need to do it to
    /// a string before it writes anybody's file.
    ///
    /// # Errors
    ///
    /// [`CatalogueError`], naming `source` and, for anything wrong with an
    /// entry, naming that entry.
    pub fn parse(text: &str, source: EntrySource) -> Result<Self, CatalogueError> {
        let parsed = schema::parse(text, &source, SCHEMA_VERSION, schema::MIGRATIONS)?;
        Ok(Self {
            entries: parsed.entries,
        })
    }

    /// This catalogue with `overlay`'s entries applied on top.
    ///
    /// An overlay entry replaces the entry with the same `game_id` in place,
    /// keeping the shipped entry's position; one with a new identifier is
    /// appended. Position matters only for reporting: it is what makes an
    /// ambiguous match list its candidates predictably.
    #[must_use]
    pub fn overlaid_with(mut self, overlay: Self) -> Self {
        for replacement in overlay.entries {
            match self
                .entries
                .iter()
                .position(|existing| existing.game_id() == replacement.game_id())
            {
                Some(index) => self.entries[index] = replacement,
                None => self.entries.push(replacement),
            }
        }
        self
    }

    /// Every entry, in catalogue order.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The entry with this identifier, if the catalogue has one.
    #[must_use]
    pub fn find_by_id(&self, game_id: &str) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|entry| entry.game_id().as_str() == game_id)
    }

    /// Which game, if any, a running process is.
    ///
    /// [`matching`] documents the precedence order and what makes an answer
    /// ambiguous.
    #[must_use]
    pub fn match_process(&self, candidate: &ProcessCandidate<'_>) -> Match<'_> {
        matching::best_match(&self.entries, candidate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wraps a body in the smallest valid file around it.
    fn file(body: &str) -> String {
        format!("schema_version = {SCHEMA_VERSION}\n{body}")
    }

    /// Parses `body` as seed data and returns the error it must produce.
    fn rejected(body: &str) -> CatalogueError {
        Catalogue::parse(&file(body), EntrySource::Seed)
            .expect_err("this fixture is supposed to be rejected")
    }

    const ONE_GAME: &str = r#"
[[game]]
game_id = "a-game"
name = "A Game"
[[game.executables]]
name = "a-game.exe"
"#;

    #[test]
    fn the_shipped_seed_data_is_valid() {
        let catalogue = Catalogue::seed().expect("the shipped seed data parses and validates");
        assert!(
            catalogue.entries().len() >= 10,
            "the shipped catalogue should not have shrunk to {} entries",
            catalogue.entries().len()
        );
        assert!(catalogue
            .entries()
            .iter()
            .all(|entry| entry.source() == &EntrySource::Seed));
    }

    #[test]
    fn every_shipped_entry_reports_its_capture_compatibility_as_unverified() {
        // Nobody has run a capture against these games and written down what
        // happened, so the only honest value is `unknown` (AGENTS.md section
        // 18). This test is what stops a future entry quietly claiming
        // otherwise without the evidence arriving in the same pull request.
        for entry in Catalogue::seed().expect("the seed data is valid").entries() {
            assert_eq!(
                entry.capture().support(),
                CaptureSupport::Unknown,
                "{} claims capture compatibility `{}`; if that was measured, say so in its \
                 `note` and update this test",
                entry.game_id(),
                entry.capture().support()
            );
        }
    }

    #[test]
    fn a_contributor_adds_a_game_by_appending_to_the_data_file_alone() {
        // The acceptance criterion, exercised the way a contributor meets it:
        // the shipped file's own text plus one appended block, through the
        // same loader, with no Rust involved.
        let contributed = format!(
            "{SEED_DATA}\n{}",
            r#"
[[game]]
game_id = "a-newly-contributed-game"
name = "A Newly Contributed Game"
[[game.executables]]
name = "newly-contributed.exe"
path_contains = "steamapps/common/Newly Contributed"

[game.launcher]
kind = "steam"
app_id = "1234567"
"#
        );

        let catalogue = Catalogue::parse(&contributed, EntrySource::Seed)
            .expect("an appended entry is a valid catalogue");
        let entry = catalogue
            .find_by_id("a-newly-contributed-game")
            .expect("the appended entry is in the catalogue");
        assert_eq!(entry.name(), "A Newly Contributed Game");

        let outcome = catalogue.match_process(
            &ProcessCandidate::new("newly-contributed.exe")
                .with_path(r"D:\Steam\steamapps\common\Newly Contributed\newly-contributed.exe"),
        );
        assert_eq!(
            outcome.entry().map(|entry| entry.game_id().as_str()),
            Some("a-newly-contributed-game")
        );
    }

    #[test]
    fn an_entry_carries_the_fields_whose_subsystems_do_not_exist_yet() {
        // Default settings are M7 and highlight providers are M9. The
        // catalogue holds and validates them so that adding them later does
        // not invalidate entries written in the meantime.
        let catalogue = Catalogue::parse(
            &file(
                r#"
[[game]]
game_id = "a-game"
name = "A Game"
icon = "a-game"
child_processes = ["a-game-helper.exe"]
highlight_providers = ["a-game-events"]
[[game.executables]]
name = "a-game.exe"
[game.capture]
compatibility = "graphics-capture"
note = "verified by hand on 2026-08-11"
[game.default_settings]
fps = 120
mode = "match"
hdr = false
scale = 0.5
"#,
            ),
            EntrySource::Seed,
        )
        .expect("the fixture is valid");

        let entry = &catalogue.entries()[0];
        assert_eq!(entry.icon(), Some("a-game"));
        assert_eq!(entry.child_processes(), ["a-game-helper.exe"]);
        assert_eq!(entry.highlight_providers(), ["a-game-events"]);
        assert_eq!(entry.capture().support(), CaptureSupport::GraphicsCapture);
        assert_eq!(
            entry.capture().note(),
            Some("verified by hand on 2026-08-11")
        );
        assert_eq!(
            entry.default_settings().get("fps"),
            Some(&SettingValue::Integer(120))
        );
        assert_eq!(
            entry.default_settings().get("mode"),
            Some(&SettingValue::Text("match".to_owned()))
        );
        assert_eq!(
            entry.default_settings().get("hdr"),
            Some(&SettingValue::Boolean(false))
        );
        assert_eq!(
            entry.default_settings().get("scale"),
            Some(&SettingValue::Float(0.5))
        );
    }

    #[test]
    fn a_misspelled_key_is_refused_rather_than_ignored() {
        let error = rejected(
            r#"
[[game]]
game_id = "a-game"
name = "A Game"
[[game.executables]]
name = "a-game.exe"
path_contian = "steamapps/common/A Game"
"#,
        );
        let message = error.to_string();
        assert!(
            matches!(error, CatalogueError::Syntax { .. }),
            "expected a syntax error, got {error:?}"
        );
        assert!(
            message.contains("path_contian"),
            "the message should name the key that was not understood: {message}"
        );
        assert!(
            message.contains("games.toml"),
            "the message should name the file: {message}"
        );
    }

    #[test]
    fn a_duplicate_game_id_names_both_entries() {
        let error = rejected(&format!("{ONE_GAME}{ONE_GAME}"));
        let message = error.to_string();
        assert!(
            matches!(
                error,
                CatalogueError::InvalidEntry {
                    problem: EntryProblem::GameIdDuplicated { first_at: 1 },
                    ..
                }
            ),
            "expected a duplicate identifier, got {error:?}"
        );
        assert!(
            message.contains("entry 2 (`a-game`)") && message.contains("entry 1"),
            "the message should name both entries: {message}"
        );
    }

    #[test]
    fn an_entry_with_no_executable_is_refused() {
        let error = rejected(
            r#"
[[game]]
game_id = "a-game"
name = "A Game"
"#,
        );
        assert!(
            matches!(
                error,
                CatalogueError::InvalidEntry {
                    problem: EntryProblem::NoExecutables,
                    ..
                }
            ),
            "expected an entry with no executables to be refused, got {error:?}"
        );
    }

    #[test]
    fn a_game_id_that_is_not_a_stable_identifier_is_refused() {
        let error = rejected(
            r#"
[[game]]
game_id = "A Game"
name = "A Game"
[[game.executables]]
name = "a-game.exe"
"#,
        );
        let message = error.to_string();
        assert!(
            matches!(
                error,
                CatalogueError::InvalidEntry {
                    problem: EntryProblem::GameIdCharacters { .. },
                    ..
                }
            ),
            "expected a rejected identifier, got {error:?}"
        );
        // It has no usable identifier, so the message locates it by position.
        assert!(
            message.contains("entry 1:"),
            "the message should locate the entry: {message}"
        );
    }

    #[test]
    fn an_executable_name_with_a_directory_in_it_is_refused_with_the_fix() {
        let error = rejected(
            r#"
[[game]]
game_id = "a-game"
name = "A Game"
[[game.executables]]
name = "steamapps/common/A Game/a-game.exe"
"#,
        );
        let message = error.to_string();
        assert!(
            matches!(
                error,
                CatalogueError::InvalidEntry {
                    problem: EntryProblem::ExecutableNameIsPath { position: 1, .. },
                    ..
                }
            ),
            "expected a path in a name to be refused, got {error:?}"
        );
        assert!(
            message.contains("path_contains"),
            "the message should say where the directory belongs: {message}"
        );
    }

    #[test]
    fn a_default_setting_that_is_not_a_scalar_is_refused() {
        let error = rejected(
            r#"
[[game]]
game_id = "a-game"
name = "A Game"
[[game.executables]]
name = "a-game.exe"
[game.default_settings]
tracks = ["game", "microphone"]
"#,
        );
        assert!(
            matches!(
                error,
                CatalogueError::InvalidEntry {
                    problem: EntryProblem::DefaultSettingNotScalar {
                        found: "an array",
                        ..
                    },
                    ..
                }
            ),
            "expected a non-scalar default setting to be refused, got {error:?}"
        );
    }

    #[test]
    fn an_unknown_launcher_is_refused_with_its_line() {
        let body = r#"
[[game]]
game_id = "a-game"
name = "A Game"
[[game.executables]]
name = "a-game.exe"
[game.launcher]
kind = "not-a-launcher"
"#;
        let error = rejected(body);
        let message = error.to_string();
        assert!(
            message.contains("not-a-launcher"),
            "the message should quote what was written: {message}"
        );

        // The parser pointing at the exact line is half the reason the format
        // is TOML, so the number is worth asserting — but the words around it
        // are `toml`'s prose and not ours, and pinning `line 9` would turn a
        // patch release that rewords it red for no behavioural reason. What is
        // asserted is that the number appears, and it is computed from the
        // fixture so that editing the fixture cannot silently satisfy it.
        let kind_line = file(body)
            .lines()
            .position(|line| line.starts_with("kind ="))
            .expect("the fixture has a `kind` line")
            + 1;
        assert!(
            message
                .split(|character: char| !character.is_ascii_digit())
                .any(|number| number == kind_line.to_string()),
            "the message should carry line {kind_line}: {message}"
        );
    }

    #[test]
    fn a_file_with_no_schema_version_is_refused() {
        let error = Catalogue::parse(ONE_GAME, EntrySource::Seed)
            .expect_err("a file with no schema version is not a catalogue");
        assert!(
            matches!(error, CatalogueError::SchemaVersionMissing { .. }),
            "expected a missing schema version, got {error:?}"
        );
        assert!(
            error.to_string().contains("schema_version = 1"),
            "the message should say what is missing: {error}"
        );
    }

    #[test]
    fn a_file_from_a_newer_clipped_is_refused_rather_than_half_understood() {
        let error = Catalogue::parse(
            &format!("schema_version = {}\n{ONE_GAME}", SCHEMA_VERSION + 1),
            EntrySource::Seed,
        )
        .expect_err("a newer schema is not readable");
        assert!(
            matches!(
                error,
                CatalogueError::SchemaTooNew {
                    found: 2,
                    supported: 1,
                    ..
                }
            ),
            "expected a too-new schema, got {error:?}"
        );
        assert!(
            error.to_string().contains("left exactly as it is"),
            "the message should promise the file was not touched: {error}"
        );
    }

    #[test]
    fn an_overlay_entry_with_a_new_identifier_is_added_rather_than_replacing() {
        let seed = Catalogue::parse(&file(ONE_GAME), EntrySource::Seed).expect("valid");
        let overlay = Catalogue::parse(
            &file(
                r#"
[[game]]
game_id = "another-game"
name = "Another Game"
[[game.executables]]
name = "another-game.exe"
"#,
            ),
            EntrySource::Overlay {
                path: std::path::PathBuf::from("games.toml"),
            },
        )
        .expect("valid");

        let catalogue = seed.overlaid_with(overlay);
        assert_eq!(catalogue.entries().len(), 2);
        assert!(catalogue.find_by_id("a-game").is_some());
        assert!(catalogue.find_by_id("another-game").is_some());
    }

    #[test]
    fn an_error_in_a_users_own_file_names_that_file_rather_than_the_shipped_one() {
        let path = std::path::PathBuf::from(r"C:\Users\somebody\AppData\Local\Clipped\games.toml");
        let error = Catalogue::parse(
            &file(
                r#"
[[game]]
game_id = "a-game"
name = ""
[[game.executables]]
name = "a-game.exe"
"#,
            ),
            EntrySource::Overlay { path: path.clone() },
        )
        .expect_err("an empty name is not valid");

        let message = error.to_string();
        assert!(
            message.contains(&path.display().to_string()),
            "the message should name the user's file: {message}"
        );
        assert!(
            !message.contains("crates/game-detection"),
            "the message should not point at the repository: {message}"
        );
    }
}
