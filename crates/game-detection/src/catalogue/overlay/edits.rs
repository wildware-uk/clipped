//! Changing the user's overlay: register a game, rename one, exclude one.
//!
//! This is the API a settings screen drives ([#63], [#107]). It is deliberately
//! not a second place where user entries live: everything here reads and writes
//! the same `%LOCALAPPDATA%\Clipped\games.toml` that [`super`] loads and that a
//! user may edit by hand, through the same validation (AGENTS.md section 55).
//!
//! [#63]: https://github.com/wildware-uk/clipped/issues/63
//! [#107]: https://github.com/wildware-uk/clipped/issues/107
//!
//! # Every edit is a statement about what the file should say
//!
//! [`Overlay::exclude`] does not toggle anything and
//! [`Overlay::clear_rename`] does not fail if there is nothing to clear: each
//! operation leaves the file saying what was asked, and asking for what it
//! already says writes nothing at all. A settings screen driving a checkbox has
//! no state of its own to keep in step, and two windows open at once cannot
//! reach a state neither asked for.
//!
//! # What an edit does, in order
//!
//! 1. **Read the file, through the loader that will read it at start-up.** If
//!    this build could not load it — a newer schema, a syntax error, a
//!    half-finished hand edit — the change is refused as
//!    [`CatalogueError::WouldOverwrite`] and nothing is written. Refusing to
//!    *read* a newer build's file preserves nothing on its own: the user opens
//!    the settings screen, sees a list without their entries in it, changes one
//!    thing, and the save is what destroys the file. `clipped-session`'s
//!    settings store learned this the same way (issue #108, AGENTS.md section
//!    56).
//! 2. **Edit the document, not a rendering of it.** The file is parsed with
//!    `toml_edit` and the one table that changes is changed, so comments,
//!    ordering and formatting the user chose survive an edit made from a
//!    screen. This is the whole reason that dependency is here.
//! 3. **Read the result back before writing it.** The edited document goes
//!    through the same parser and the same validation the loader uses; a change
//!    that would not load is [`CatalogueError::WouldWriteInvalid`] and, again,
//!    nothing is written.
//! 4. **Write it through a temporary file and a rename**, so an interrupted
//!    write leaves the previous file rather than half of a new one.
//!
//! The window between the read in step 1 and the rename in step 4 is the same
//! one the settings store documents: cross-process locking is
//! [issue #194](https://github.com/wildware-uk/clipped/issues/194) and nothing
//! smaller closes it.
//!
//! # Which shape an edit takes
//!
//! | The game is | Renaming it | Excluding it |
//! | --- | --- | --- |
//! | one the user registered | changes that entry's `name` | adds a `[[decision]]` |
//! | one Clipped ships | adds a `[[decision]]` | adds a `[[decision]]` |
//!
//! A decision exists to sit over something the user cannot edit; where they own
//! the entry, editing it is what a person would do by hand and what keeps the
//! file obvious. See [`crate::catalogue::decision`] for why a rename is not
//! stored as a replacement entry.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use toml_edit::{value, ArrayOfTables, DocumentMut, Item, Table};

use crate::catalogue::entry::{EntrySource, GameId};
use crate::catalogue::error::CatalogueError;
use crate::catalogue::{schema, Catalogue, LoadedCatalogue, SCHEMA_VERSION};

use super::write_atomically;

/// The header written into an overlay Clipped creates.
///
/// Written once, when there is no file. Every later edit preserves whatever the
/// user has made of it, comments and all.
const NEW_FILE_HEADER: &str = "\
# Your games, and what you decided about the ones Clipped already knows.
#
# Clipped writes this file when you register, rename or exclude a game, and
# reads it whenever it starts. It is yours: edit it by hand, comment it, keep a
# copy. Updating Clipped never touches it.
#
# A `[[game]]` block describes a game. A `[[decision]]` block says what you
# decided about a game described elsewhere - a rename that survives an update of
# Clipped's own catalogue, or an exclusion that an update cannot undo.
#
# docs/game-detection.md is the field reference.
";

/// A game the user is registering.
///
/// The executable is a **file name**, not a path: `game.exe`, not
/// `C:\Games\Thing\game.exe`. A registration by file name still matches after
/// the user moves the game to another drive, which is the ordinary thing to
/// happen to a games library. [`Self::qualified_by`] narrows it to one
/// installation for the case that needs it — two games shipping one executable
/// name — at the cost of that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    name: String,
    executable: String,
    path_contains: Option<String>,
}

impl Registration {
    /// A game called `name`, recognised by the process `executable`.
    #[must_use]
    pub fn new(name: impl Into<String>, executable: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            executable: executable.into(),
            path_contains: None,
        }
    }

    /// A registration for the executable at `path`, named after its file.
    ///
    /// What a settings screen builds when somebody picks a file: the name is
    /// the file's stem, which is a starting point to be corrected with
    /// [`Self::named`] rather than an answer — `eldenring.exe` is not what
    /// anybody calls that game.
    ///
    /// `None` when the path names no file at all.
    #[must_use]
    pub fn for_executable(path: &Path) -> Option<Self> {
        let executable = path.file_name()?.to_string_lossy().into_owned();
        let name = path.file_stem().map_or_else(
            || executable.clone(),
            |stem| stem.to_string_lossy().into_owned(),
        );
        Some(Self::new(name, executable))
    }

    /// The same registration under a different name.
    #[must_use]
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// The same registration, restricted to executables under `fragment`.
    ///
    /// The fragment names whole directories — `steamapps/common/Some Game` —
    /// and is compared as [`crate::catalogue::matching`] describes.
    #[must_use]
    pub fn qualified_by(mut self, fragment: impl Into<String>) -> Self {
        self.path_contains = Some(fragment.into());
        self
    }

    /// What the game will be called.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The executable's file name.
    #[must_use]
    pub fn executable(&self) -> &str {
        &self.executable
    }

    /// The directory fragment the registration is restricted to, if any.
    #[must_use]
    pub fn path_contains(&self) -> Option<&str> {
        self.path_contains.as_deref()
    }
}

/// The user's overlay file, and the changes a settings screen makes to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overlay {
    path: PathBuf,
}

impl Overlay {
    /// The overlay at `path`.
    ///
    /// Nothing is read here. Construction that touches the filesystem is
    /// construction that can fail, and a caller should choose when that happens.
    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The overlay in Clipped's per-user directory —
    /// `%LOCALAPPDATA%\Clipped\games.toml` on Windows.
    ///
    /// `None` when the environment describes no per-user directory, which is
    /// the supported state [`super::default_path`] documents: there is then
    /// nowhere to keep a user's games, and the catalogue is the shipped data.
    #[must_use]
    pub fn default_location() -> Option<Self> {
        super::default_path().map(Self::at)
    }

    /// Where the file is.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The shipped catalogue with this overlay applied.
    ///
    /// The list a settings screen shows: every entry carries where it came from
    /// ([`crate::catalogue::Entry::source`]) and what the user decided about it
    /// ([`crate::catalogue::Entry::decision`]).
    ///
    /// # Errors
    ///
    /// As [`Catalogue::load_with_overlay_at`].
    pub fn load(&self) -> Result<LoadedCatalogue, CatalogueError> {
        Catalogue::load_with_overlay_at(&self.path)
    }

    /// Registers a game the catalogue does not know.
    ///
    /// The identifier is derived from the name — "My Game" becomes `my-game` —
    /// and made unique against everything already in the catalogue, shipped
    /// entries included, so registering a game whose name collides with one
    /// Clipped ships adds a game rather than quietly replacing it.
    ///
    /// # Errors
    ///
    /// [`CatalogueError::NoIdentifierFromName`] if no identifier can be made
    /// from the name; [`CatalogueError::WouldWriteInvalid`] if the registration
    /// is not a valid entry — an executable with a directory in it, an empty
    /// name — with the message the loader would have given; and the errors
    /// every edit can raise, described on the module.
    pub fn register(&self, registration: &Registration) -> Result<GameId, CatalogueError> {
        let (mut document, existing) = self.read()?;
        let catalogue = Catalogue::seed()?.overlaid_with(existing);
        let game_id = unique_identifier(&registration.name, &catalogue)?;

        let mut executable = Table::new();
        executable["name"] = value(registration.executable.clone());
        if let Some(fragment) = &registration.path_contains {
            executable["path_contains"] = value(fragment.clone());
        }
        let mut executables = ArrayOfTables::new();
        executables.push(executable);

        let mut game = Table::new();
        game["game_id"] = value(game_id.as_str());
        game["name"] = value(registration.name.clone());
        game.insert("executables", Item::ArrayOfTables(executables));

        self.games_mut(&mut document)?.push(game);
        self.commit(document)?;
        Ok(game_id)
    }

    /// Calls a game something else.
    ///
    /// A rename of a game Clipped ships is stored as a decision, so a later
    /// release may correct that game's executables, its launcher identifier or
    /// its icon and the user still sees the name they chose.
    ///
    /// # Errors
    ///
    /// The errors every edit can raise, described on the module.
    pub fn rename(&self, game_id: &str, name: &str) -> Result<(), CatalogueError> {
        let (mut document, _) = self.read()?;
        match self.own_entry_mut(&mut document, game_id)? {
            Some(entry) => entry["name"] = value(name),
            None => self.decide(&mut document, game_id, |decision| {
                decision["name"] = value(name);
            })?,
        }
        self.commit(document)
    }

    /// Goes back to the name Clipped ships for a game.
    ///
    /// Does nothing to a game the user registered themselves: its name is not a
    /// rename of anything, and there is nothing underneath to go back to.
    ///
    /// # Errors
    ///
    /// The errors every edit can raise, described on the module.
    pub fn clear_rename(&self, game_id: &str) -> Result<(), CatalogueError> {
        let (mut document, _) = self.read()?;
        self.forget_decision(&mut document, game_id, "name")?;
        self.commit(document)
    }

    /// Asks that a game is never recorded.
    ///
    /// The entry stays exactly where it was: an exclusion is a decision about a
    /// game, not the deletion of one, so an update that changes the entry — or
    /// re-adds one this build does not have — cannot resurrect a game the user
    /// excluded.
    ///
    /// # Errors
    ///
    /// The errors every edit can raise, described on the module.
    pub fn exclude(&self, game_id: &str) -> Result<(), CatalogueError> {
        let (mut document, _) = self.read()?;
        self.decide(&mut document, game_id, |decision| {
            decision["excluded"] = value(true);
        })?;
        self.commit(document)
    }

    /// Asks that a game is recorded again.
    ///
    /// # Errors
    ///
    /// The errors every edit can raise, described on the module.
    pub fn include(&self, game_id: &str) -> Result<(), CatalogueError> {
        let (mut document, _) = self.read()?;
        self.forget_decision(&mut document, game_id, "excluded")?;
        self.commit(document)
    }

    /// Forgets everything the user's file says about a game.
    ///
    /// Their own entry for it, if they registered one, and any decision about
    /// it. What remains is whatever Clipped ships, which for a game they
    /// registered is nothing.
    ///
    /// # Errors
    ///
    /// The errors every edit can raise, described on the module.
    pub fn forget(&self, game_id: &str) -> Result<(), CatalogueError> {
        let (mut document, _) = self.read()?;
        self.remove_from(&mut document, "game", game_id)?;
        self.remove_from(&mut document, "decision", game_id)?;
        self.commit(document)
    }

    /// The file as a document to edit, and as a catalogue that was read.
    ///
    /// Both, because the two answer different questions: the catalogue is
    /// whether this build could have loaded the file — the check that stops an
    /// edit destroying a file it does not understand — and the document is what
    /// an edit changes without disturbing the rest.
    fn read(&self) -> Result<(DocumentMut, Catalogue), CatalogueError> {
        // Through the loader, so that an older file is migrated and backed up
        // exactly as it would be at start-up, and a file this build cannot read
        // stops the edit before anything is written.
        let (catalogue, _) =
            super::load(&self.path, SCHEMA_VERSION, schema::MIGRATIONS).map_err(|source| {
                CatalogueError::WouldOverwrite {
                    path: self.path.clone(),
                    source: Box::new(source),
                }
            })?;

        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            // No file yet, which is every installation until the first edit.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                format!("{NEW_FILE_HEADER}\nschema_version = {SCHEMA_VERSION}\n")
            }
            Err(source) => {
                return Err(CatalogueError::WouldOverwrite {
                    path: self.path.clone(),
                    source: Box::new(CatalogueError::Read {
                        path: self.path.clone(),
                        source,
                    }),
                })
            }
        };

        let document =
            text.parse::<DocumentMut>()
                .map_err(|error| CatalogueError::WouldOverwrite {
                    path: self.path.clone(),
                    source: Box::new(CatalogueError::Syntax {
                        file: self.source(),
                        message: error.to_string(),
                    }),
                })?;
        Ok((document, catalogue))
    }

    /// Validates the edited document and writes it, if it changed anything.
    fn commit(&self, document: DocumentMut) -> Result<(), CatalogueError> {
        // Undoing something on a machine that never did it leaves an empty
        // document, and writing that would put a file on somebody's disk that
        // says nothing and that they did not ask for.
        if !self.path.exists() && !holds_anything(&document) {
            return Ok(());
        }

        let text = document.to_string();
        // Through the same reader that will load it, so a change that would not
        // load is refused with the message the loader would have given rather
        // than written and discovered at the next start-up.
        schema::parse(&text, &self.source(), SCHEMA_VERSION, schema::MIGRATIONS).map_err(
            |source| CatalogueError::WouldWriteInvalid {
                path: self.path.clone(),
                source: Box::new(source),
            },
        )?;

        // An edit that asked for what the file already said is not a reason to
        // touch it: the user's file keeps its modification time, and a screen
        // that re-asserts a setting on every render costs nothing.
        if fs::read_to_string(&self.path).is_ok_and(|existing| existing == text) {
            return Ok(());
        }

        write_atomically(&self.path, &text).map_err(|source| CatalogueError::WriteFailed {
            path: self.path.clone(),
            source,
        })
    }

    /// How errors about this file name it.
    fn source(&self) -> EntrySource {
        EntrySource::Overlay {
            path: self.path.clone(),
        }
    }

    /// The `[[game]]` blocks, creating the array if the file has none.
    fn games_mut<'a>(
        &self,
        document: &'a mut DocumentMut,
    ) -> Result<&'a mut ArrayOfTables, CatalogueError> {
        let item = document
            .entry("game")
            .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()));
        item.as_array_of_tables_mut()
            .ok_or_else(|| self.cannot_edit("game"))
    }

    /// The user's own `[[game]]` block for a game, if their file has one.
    fn own_entry_mut<'a>(
        &self,
        document: &'a mut DocumentMut,
        game_id: &str,
    ) -> Result<Option<&'a mut Table>, CatalogueError> {
        if document.get("game").is_none() {
            return Ok(None);
        }
        Ok(self
            .games_mut(document)?
            .iter_mut()
            .find(|game| names_game(game, game_id)))
    }

    /// Changes the `[[decision]]` block for a game, adding one if there is none.
    fn decide(
        &self,
        document: &mut DocumentMut,
        game_id: &str,
        change: impl FnOnce(&mut Table),
    ) -> Result<(), CatalogueError> {
        let decisions = document
            .entry("decision")
            .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()))
            .as_array_of_tables_mut()
            .ok_or_else(|| self.cannot_edit("decision"))?;

        if let Some(existing) = decisions
            .iter_mut()
            .find(|decision| names_game(decision, game_id))
        {
            change(existing);
            return Ok(());
        }

        let mut decision = Table::new();
        decision["game_id"] = value(game_id);
        change(&mut decision);
        decisions.push(decision);
        Ok(())
    }

    /// Removes one key from a game's decision, and the decision if that empties
    /// it.
    ///
    /// Removing the block rather than leaving `excluded = false` behind is what
    /// keeps the file readable: what is in it is what the user decided, and
    /// nothing else.
    fn forget_decision(
        &self,
        document: &mut DocumentMut,
        game_id: &str,
        key: &str,
    ) -> Result<(), CatalogueError> {
        if document.get("decision").is_none() {
            return Ok(());
        }
        let decisions = document
            .get_mut("decision")
            .and_then(Item::as_array_of_tables_mut)
            .ok_or_else(|| self.cannot_edit("decision"))?;

        let Some(index) = decisions
            .iter()
            .position(|decision| names_game(decision, game_id))
        else {
            return Ok(());
        };
        let decision = decisions
            .get_mut(index)
            .expect("the index came from this array");
        decision.remove(key);
        if decision.iter().all(|(key, _)| key == "game_id") {
            decisions.remove(index);
        }
        if decisions.is_empty() {
            document.remove("decision");
        }
        Ok(())
    }

    /// Removes every block of one kind that names a game.
    fn remove_from(
        &self,
        document: &mut DocumentMut,
        kind: &str,
        game_id: &str,
    ) -> Result<(), CatalogueError> {
        if document.get(kind).is_none() {
            return Ok(());
        }
        let blocks = document
            .get_mut(kind)
            .and_then(Item::as_array_of_tables_mut)
            .ok_or_else(|| self.cannot_edit(kind))?;
        blocks.retain(|block| !names_game(block, game_id));
        if blocks.is_empty() {
            document.remove(kind);
        }
        Ok(())
    }

    /// The file is valid and still not something these edits can change.
    ///
    /// Reachable, because TOML has two spellings of an array of tables and only
    /// one of them is a set of blocks: a file written `game = [{ … }]` loads
    /// perfectly well and has no `[[game]]` block to add one beside. Saying so
    /// is better than rewriting the file into the other spelling behind
    /// somebody's back.
    fn cannot_edit(&self, key: &str) -> CatalogueError {
        CatalogueError::CannotEdit {
            path: self.path.clone(),
            detail: format!(
                "its `{key}` entries are written as an inline array rather than as `[[{key}]]` \
                 blocks"
            ),
        }
    }
}

/// Whether a document says anything beyond its schema version.
fn holds_anything(document: &DocumentMut) -> bool {
    ["game", "decision"].iter().any(|kind| {
        document
            .get(kind)
            .and_then(Item::as_array_of_tables)
            .is_some_and(|blocks| !blocks.is_empty())
    })
}

/// Whether a block is about a game.
fn names_game(block: &Table, game_id: &str) -> bool {
    block
        .get("game_id")
        .and_then(Item::as_str)
        .is_some_and(|written| written == game_id)
}

/// An identifier for a newly registered game, unused by anything in
/// `catalogue`.
fn unique_identifier(name: &str, catalogue: &Catalogue) -> Result<GameId, CatalogueError> {
    let stem = identifier_stem(name).ok_or_else(|| CatalogueError::NoIdentifierFromName {
        name: name.to_owned(),
    })?;

    let taken = |candidate: &str| {
        catalogue.find_by_id(candidate).is_some()
            || catalogue
                .pending_decisions()
                .iter()
                .any(|decision| decision.game_id().as_str() == candidate)
    };

    if !taken(&stem) {
        return GameId::parse(&stem).ok_or_else(|| CatalogueError::NoIdentifierFromName {
            name: name.to_owned(),
        });
    }
    // A second game called what an existing one is called is a real situation —
    // a remaster, a second installation, a name Clipped already ships. Numbering
    // is what keeps it from replacing that entry.
    for suffix in 2..=u32::MAX {
        let candidate = format!("{stem}-{suffix}");
        if !taken(&candidate) {
            return GameId::parse(&candidate).ok_or_else(|| CatalogueError::NoIdentifierFromName {
                name: name.to_owned(),
            });
        }
    }
    Err(CatalogueError::NoIdentifierFromName {
        name: name.to_owned(),
    })
}

/// A name as a catalogue identifier: lower-case, and hyphens for the rest.
///
/// `None` when nothing survives, which is a name made entirely of characters an
/// identifier cannot carry. The name itself is kept as written and is what a
/// user sees; this is only what files, settings and paths use (see
/// [`GameId`]).
fn identifier_stem(name: &str) -> Option<String> {
    let mut stem = String::with_capacity(name.len());
    for character in name.to_lowercase().chars() {
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            stem.push(character);
        } else if !stem.ends_with('-') {
            stem.push('-');
        }
    }
    let stem = stem.trim_matches('-').to_owned();
    (!stem.is_empty()).then_some(stem)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::overlay::tests::TestDirectory;
    use crate::catalogue::{Match, ProcessCandidate};

    /// A file as a user who has been editing it by hand would have left it.
    const HAND_WRITTEN: &str = r#"schema_version = 1

# A demo I was given, which is not on Steam and never will be.
[[game]]
game_id = "a-demo"
name = "A Demo"

[[game.executables]]
name = "demo.exe"  # the launcher is `run.exe` and is not the game
"#;

    /// The overlay at `path`, and the catalogue it produces.
    fn catalogue(overlay: &Overlay) -> Catalogue {
        overlay.load().expect("the overlay loads").into_catalogue()
    }

    /// How many blocks of one kind the file has.
    ///
    /// Counted through the parser rather than by looking for `[[decision]]` in
    /// the text, because the header Clipped writes into a new file mentions
    /// both kinds of block by name.
    fn blocks(path: &Path, kind: &str) -> usize {
        fs::read_to_string(path)
            .expect("the file is there")
            .parse::<DocumentMut>()
            .expect("the file is TOML")
            .get(kind)
            .and_then(Item::as_array_of_tables)
            .map_or(0, ArrayOfTables::len)
    }

    #[test]
    fn a_registered_executable_is_matched_the_next_time_the_catalogue_is_read() {
        // The first acceptance criterion, as far as this crate reaches: what
        // turns a match into a recording is `clipped_session::automatic`, which
        // is tested against `Match::One` on its own side.
        let directory = TestDirectory::new("registered");
        let overlay = Overlay::at(directory.path());
        let candidate = ProcessCandidate::new("mystery.exe");

        assert_eq!(
            catalogue(&overlay).match_process(&candidate),
            Match::None,
            "the fixture has to be a game nothing already claims"
        );

        let game_id = overlay
            .register(&Registration::new("My Mystery Game", "mystery.exe"))
            .expect("the registration is written");
        assert_eq!(game_id.as_str(), "my-mystery-game");

        let catalogue = catalogue(&overlay);
        let matched = catalogue
            .match_process(&candidate)
            .entry()
            .expect("the registered game claims its own executable")
            .clone();
        assert_eq!(matched.game_id().as_str(), "my-mystery-game");
        assert_eq!(matched.name(), "My Mystery Game");
        assert!(
            matched.source().is_overlay(),
            "a registered game is the user's, and should say so"
        );
    }

    #[test]
    fn the_first_registration_creates_a_file_that_says_what_it_is() {
        let directory = TestDirectory::new("created");
        let overlay = Overlay::at(directory.path());
        overlay
            .register(&Registration::new("My Game", "my-game.exe"))
            .expect("the registration is written");

        let text = fs::read_to_string(directory.path()).expect("the file was created");
        assert!(
            text.contains(&format!("schema_version = {SCHEMA_VERSION}")),
            "a file with no schema version is not a catalogue: {text}"
        );
        assert!(
            text.starts_with('#') && text.contains("docs/game-detection.md"),
            "a file Clipped created should say what it is and where the reference is: {text}"
        );
    }

    #[test]
    fn a_registration_qualified_by_a_path_matches_only_that_installation() {
        let directory = TestDirectory::new("qualified");
        let overlay = Overlay::at(directory.path());
        overlay
            .register(&Registration::new("My Game", "game.exe").qualified_by("publisher/my game"))
            .expect("the registration is written");

        let catalogue = catalogue(&overlay);
        assert_eq!(
            catalogue
                .match_process(
                    &ProcessCandidate::new("game.exe")
                        .with_path(r"D:\Games\Publisher\My Game\game.exe")
                )
                .entry()
                .map(|entry| entry.game_id().as_str()),
            Some("my-game")
        );
        assert_eq!(
            catalogue.match_process(
                &ProcessCandidate::new("game.exe").with_path(r"D:\Games\Somebody Else\game.exe")
            ),
            Match::None
        );
    }

    #[test]
    fn registering_a_game_clipped_already_ships_adds_one_rather_than_replacing_it() {
        // Deriving the identifier from the name means a name that collides
        // derives an identifier that collides, and an overlay entry with a
        // shipped identifier *replaces* that entry (see `catalogue`). Silently
        // taking over a shipped game because the user typed its name is the
        // failure being prevented.
        let shipped = Catalogue::seed().expect("the seed data is valid");
        let first = shipped.entries().first().expect("the seed data has games");
        let (shipped_id, shipped_name) =
            (first.game_id().as_str().to_owned(), first.name().to_owned());

        let directory = TestDirectory::new("collides");
        let overlay = Overlay::at(directory.path());
        let registered = overlay
            .register(&Registration::new(shipped_name.clone(), "mine.exe"))
            .expect("the registration is written");

        assert_ne!(registered.as_str(), shipped_id);
        let catalogue = catalogue(&overlay);
        let survivor = catalogue.find_by_id(&shipped_id).expect("still shipped");
        assert_eq!(survivor.name(), shipped_name);
        assert!(
            !survivor.source().is_overlay(),
            "the shipped entry should not have been replaced"
        );
        assert!(catalogue.find_by_id(registered.as_str()).is_some());
    }

    #[test]
    fn registering_the_same_name_twice_numbers_the_second() {
        let directory = TestDirectory::new("twice");
        let overlay = Overlay::at(directory.path());
        let first = overlay
            .register(&Registration::new("My Game", "one.exe"))
            .expect("the first registration is written");
        let second = overlay
            .register(&Registration::new("My Game", "two.exe"))
            .expect("the second registration is written");

        assert_eq!(first.as_str(), "my-game");
        assert_eq!(second.as_str(), "my-game-2");
        assert_eq!(catalogue(&overlay).entries().len(), {
            let shipped = Catalogue::seed().expect("valid").entries().len();
            shipped + 2
        });
    }

    #[test]
    fn an_identifier_is_derived_from_the_name_a_person_would_type() {
        assert_eq!(
            identifier_stem("Half-Life 2: Episode One").as_deref(),
            Some("half-life-2-episode-one")
        );
        assert_eq!(identifier_stem("  Spaces  ").as_deref(), Some("spaces"));
        assert_eq!(identifier_stem("!!!").as_deref(), None);
    }

    #[test]
    fn a_name_no_identifier_can_be_made_from_is_refused_rather_than_guessed_at() {
        let directory = TestDirectory::new("nameless");
        let overlay = Overlay::at(directory.path());
        let error = overlay
            .register(&Registration::new("★★★", "stars.exe"))
            .expect_err("there is no identifier in that name");

        assert!(
            matches!(error, CatalogueError::NoIdentifierFromName { .. }),
            "expected a refused name, got {error:?}"
        );
        assert!(
            !directory.path().exists(),
            "a refused registration should not leave a file behind"
        );
    }

    #[test]
    fn a_registration_can_be_built_from_the_file_somebody_picked() {
        let registration = Registration::for_executable(Path::new(r"D:\Games\Thing\thing.exe"))
            .expect("the path names a file");
        assert_eq!(registration.executable(), "thing.exe");
        assert_eq!(registration.name(), "thing");
        assert_eq!(registration.named("Thing").name(), "Thing");
    }

    #[test]
    fn excluding_a_game_stops_it_matching_and_leaves_the_entry_in_place() {
        // The second acceptance criterion, through the API a settings screen
        // drives rather than through a fixture written by hand.
        let directory = TestDirectory::new("excluded");
        let overlay = Overlay::at(directory.path());
        overlay
            .register(&Registration::new("My Game", "my-game.exe"))
            .expect("the registration is written");

        overlay
            .exclude("my-game")
            .expect("the exclusion is written");

        let excluded = catalogue(&overlay);
        assert_eq!(
            excluded.match_process(&ProcessCandidate::new("my-game.exe")),
            Match::None
        );
        let entry = excluded.find_by_id("my-game").expect("still catalogued");
        assert!(entry.is_excluded());
        assert_eq!(
            entry.decision().expect("the decision is reported").path(),
            directory.path(),
            "an entry should say which file decided about it"
        );

        overlay
            .include("my-game")
            .expect("the game is included again");
        assert!(catalogue(&overlay)
            .match_process(&ProcessCandidate::new("my-game.exe"))
            .entry()
            .is_some());
        assert_eq!(
            blocks(&directory.path(), "decision"),
            0,
            "including a game again should remove the decision rather than leave `excluded = \
             false` behind"
        );
    }

    #[test]
    fn undoing_something_nobody_did_does_not_create_a_file() {
        // A screen that clears a rename or re-includes a game on a machine with
        // no overlay has nothing to undo, and a file holding only a header is a
        // file the user did not ask for.
        let directory = TestDirectory::new("no-file");
        let overlay = Overlay::at(directory.path());

        overlay.include("some-game").expect("nothing to include");
        overlay.clear_rename("some-game").expect("nothing to clear");
        overlay.forget("some-game").expect("nothing to forget");

        assert!(
            !directory.path().exists(),
            "no overlay should have been created"
        );
    }

    #[test]
    fn excluding_a_game_this_build_does_not_have_is_kept_for_when_it_arrives() {
        let directory = TestDirectory::new("pending");
        let overlay = Overlay::at(directory.path());
        overlay
            .exclude("a-game-from-a-newer-clipped")
            .expect("the exclusion is written");

        let catalogue = catalogue(&overlay);
        let pending = catalogue.pending_decisions();
        assert_eq!(pending.len(), 1, "{pending:?}");
        assert!(pending[0].is_excluded());
        assert_eq!(pending[0].game_id().as_str(), "a-game-from-a-newer-clipped");
    }

    #[test]
    fn renaming_a_shipped_game_writes_a_decision_and_keeps_the_name_underneath() {
        let shipped = Catalogue::seed().expect("the seed data is valid");
        let first = shipped.entries().first().expect("the seed data has games");
        let (shipped_id, shipped_name) =
            (first.game_id().as_str().to_owned(), first.name().to_owned());

        let directory = TestDirectory::new("renamed-shipped");
        let overlay = Overlay::at(directory.path());
        overlay
            .rename(&shipped_id, "The Short Name")
            .expect("the rename is written");

        assert_eq!(
            (
                blocks(&directory.path(), "decision"),
                blocks(&directory.path(), "game")
            ),
            (1, 0),
            "a rename of a shipped game is a decision, not a copy of its entry"
        );

        let entry = catalogue(&overlay)
            .find_by_id(&shipped_id)
            .expect("still catalogued")
            .clone();
        assert_eq!(entry.name(), "The Short Name");
        assert_eq!(
            entry.renamed_from().map(str::to_owned),
            Some(shipped_name.clone())
        );

        overlay
            .clear_rename(&shipped_id)
            .expect("the rename is cleared");
        assert_eq!(
            catalogue(&overlay)
                .find_by_id(&shipped_id)
                .expect("still catalogued")
                .name(),
            shipped_name,
            "clearing a rename should go back to the name Clipped ships"
        );
    }

    #[test]
    fn renaming_a_game_the_user_registered_changes_their_own_entry() {
        let directory = TestDirectory::new("renamed-own");
        let overlay = Overlay::at(directory.path());
        overlay
            .register(&Registration::new("My Game", "my-game.exe"))
            .expect("the registration is written");
        overlay
            .rename("my-game", "My Better Game")
            .expect("renamed");

        assert_eq!(
            blocks(&directory.path(), "decision"),
            0,
            "a decision exists to sit over an entry the user cannot edit; this one they can"
        );
        let entry = catalogue(&overlay)
            .find_by_id("my-game")
            .expect("still there")
            .clone();
        assert_eq!(entry.name(), "My Better Game");
        assert_eq!(
            entry.renamed_from(),
            None,
            "their own entry's name is not a rename of anything"
        );
    }

    #[test]
    fn forgetting_a_game_removes_the_registration_and_the_decision() {
        let directory = TestDirectory::new("forgotten");
        let overlay = Overlay::at(directory.path());
        overlay
            .register(&Registration::new("My Game", "my-game.exe"))
            .expect("the registration is written");
        overlay
            .exclude("my-game")
            .expect("the exclusion is written");

        overlay.forget("my-game").expect("the game is forgotten");

        let catalogue = catalogue(&overlay);
        assert!(catalogue.find_by_id("my-game").is_none());
        assert!(catalogue.pending_decisions().is_empty());
        let text = fs::read_to_string(directory.path()).expect("the file is there");
        assert!(!text.contains("my-game"), "{text}");
    }

    #[test]
    fn a_comment_the_user_wrote_survives_an_edit_made_from_a_screen() {
        // The reason the edits go through `toml_edit` rather than through a
        // rendering of the parsed document. This file is one people are told to
        // hand-edit; a settings screen that silently deleted what they wrote in
        // it would be destroying their data (AGENTS.md section 56).
        let directory = TestDirectory::new("comments");
        let path = directory.with_overlay(HAND_WRITTEN);
        let overlay = Overlay::at(path.clone());

        overlay.exclude("a-demo").expect("the exclusion is written");

        let text = fs::read_to_string(&path).expect("the file is there");
        assert!(
            text.contains("# A demo I was given, which is not on Steam and never will be.")
                && text.contains("# the launcher is `run.exe` and is not the game"),
            "both comments should still be there: {text}"
        );
        assert!(text.contains("[[decision]]"), "{text}");
        assert!(catalogue(&overlay)
            .find_by_id("a-demo")
            .expect("still catalogued")
            .is_excluded());
    }

    #[test]
    #[allow(
        clippy::permissions_set_readonly_false,
        reason = "the file is one this test made in a directory it owns, and clearing the \
                  read-only attribute again is what lets the directory be removed on Windows"
    )]
    fn asking_for_what_the_file_already_says_does_not_touch_it() {
        // A settings screen re-asserting a checkbox on every render must not
        // rewrite somebody's file every time. Proved by making the file
        // unwritable: a second exclusion that wrote anything would fail.
        let directory = TestDirectory::new("idempotent");
        let overlay = Overlay::at(directory.path());
        overlay
            .register(&Registration::new("My Game", "my-game.exe"))
            .expect("the registration is written");
        overlay
            .exclude("my-game")
            .expect("the exclusion is written");

        let path = directory.path();
        let mut permissions = fs::metadata(&path)
            .expect("the file is there")
            .permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions.clone()).expect("the file can be made read-only");

        let result = overlay.exclude("my-game");

        permissions.set_readonly(false);
        fs::set_permissions(&path, permissions).expect("the file can be made writable again");
        result.expect("excluding an already excluded game writes nothing and so cannot fail");
    }

    #[test]
    fn an_overlay_from_a_newer_clipped_is_not_written_over_by_an_edit() {
        // The failure AGENTS.md section 56 is about, arrived at one step later
        // than "refuse to read it": the user opens the settings screen on the
        // machine that is a version behind, sees a list without their entries,
        // changes one thing, and the *save* is what destroys the file.
        let directory = TestDirectory::new("newer");
        let newer = HAND_WRITTEN.replace("schema_version = 1", "schema_version = 99");
        let path = directory.with_overlay(&newer);
        let overlay = Overlay::at(path.clone());

        let error = overlay
            .exclude("a-demo")
            .expect_err("a file this build cannot read is not one to write over");

        assert!(
            matches!(error, CatalogueError::WouldOverwrite { .. }),
            "expected a refused overwrite, got {error:?}"
        );
        assert!(
            error.to_string().contains("was not changed"),
            "the message should say the file survived: {error}"
        );
        assert_eq!(
            fs::read_to_string(&path).expect("the file is there"),
            newer,
            "the file must be byte-for-byte what it was"
        );
    }

    #[test]
    fn an_edit_that_would_not_load_is_refused_and_writes_nothing() {
        let directory = TestDirectory::new("invalid");
        let path = directory.with_overlay(HAND_WRITTEN);
        let overlay = Overlay::at(path.clone());

        // A file name with a directory in it is the mistake the loader has a
        // message for; the edit borrows it rather than inventing another.
        let error = overlay
            .register(&Registration::new(
                "My Game",
                r"C:\Games\My Game\my-game.exe",
            ))
            .expect_err("an executable name with a directory in it is not valid");

        assert!(
            matches!(error, CatalogueError::WouldWriteInvalid { .. }),
            "expected a refused write, got {error:?}"
        );
        assert!(
            error.to_string().contains("path_contains"),
            "the loader's own message should reach the user: {error}"
        );
        assert_eq!(
            fs::read_to_string(&path).expect("the file is there"),
            HAND_WRITTEN,
            "nothing is written until the result has been read back"
        );
    }

    #[test]
    fn an_overlay_whose_games_are_an_inline_array_is_refused_rather_than_reshaped() {
        // TOML has two spellings of an array of tables and only one of them is
        // a set of blocks to add another beside. The file loads perfectly well,
        // so saying so is better than rewriting somebody's file into the other
        // spelling behind their back.
        let directory = TestDirectory::new("inline");
        let inline = "schema_version = 1\ngame = [{ game_id = \"a-demo\", name = \"A Demo\", \
                      executables = [{ name = \"demo.exe\" }] }]\n";
        let path = directory.with_overlay(inline);
        let overlay = Overlay::at(path.clone());

        assert!(
            catalogue(&overlay).find_by_id("a-demo").is_some(),
            "the fixture has to be a file that loads"
        );

        let error = overlay
            .register(&Registration::new("My Game", "my-game.exe"))
            .expect_err("there is no `[[game]]` block to add one beside");

        assert!(
            matches!(error, CatalogueError::CannotEdit { .. }),
            "expected an unchangeable file, got {error:?}"
        );
        assert!(
            error.to_string().contains("by hand"),
            "the message should say what the user can do instead: {error}"
        );
        assert_eq!(
            fs::read_to_string(&path).expect("the file is there"),
            inline
        );
    }
}
