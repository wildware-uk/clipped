//! The catalogue as its callers will meet it: public API only, real files.
//!
//! The unit tests inside the crate cover the rules one at a time. These cover
//! the two things only an outside caller can: that the surface is usable
//! without reaching into the crate — the process watcher (#41) and the
//! registration UI (#45) both have to — and that the seed data actually
//! shipped in this build answers correctly about real install paths.

use std::fs;
use std::path::PathBuf;

use clipped_game_detection::catalogue::{
    Catalogue, CatalogueError, EntrySource, LauncherKind, Match, MatchStrength, OverlayStatus,
    ProcessCandidate,
};

/// A directory of one test's own, removed when it is dropped.
#[derive(Debug)]
struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "clipped-catalogue-it-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("the temporary directory can be created");
        Self(path)
    }

    fn overlay(&self, text: &str) -> PathBuf {
        let path = self.0.join("games.toml");
        fs::write(&path, text).expect("the overlay can be written");
        path
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn matched<'a>(outcome: &'a Match<'a>) -> &'a str {
    outcome
        .entry()
        .unwrap_or_else(|| panic!("expected exactly one match, got {outcome:?}"))
        .game_id()
        .as_str()
}

#[test]
fn the_shipped_catalogue_tells_two_source_games_apart_by_their_install_path() {
    // `hl2.exe` is Half-Life 2 and Team Fortress 2 and several more. This is
    // the shipped data answering, not a fixture.
    let catalogue = Catalogue::seed().expect("the shipped seed data is valid");

    let outcome = catalogue.match_process(
        &ProcessCandidate::new("hl2.exe")
            .with_path(r"D:\SteamLibrary\steamapps\common\Team Fortress 2\hl2.exe"),
    );
    assert_eq!(matched(&outcome), "team-fortress-2");

    let outcome = catalogue.match_process(
        &ProcessCandidate::new("hl2.exe")
            .with_path(r"C:\Program Files (x86)\Steam\steamapps\common\Half-Life 2\hl2.exe"),
    );
    assert_eq!(matched(&outcome), "half-life-2");

    // And the property that makes those two answers mean anything: another
    // Source game running the same binary is claimed by neither. If either
    // entry were left unqualified it would answer here, wrongly and silently.
    let outcome = catalogue.match_process(
        &ProcessCandidate::new("hl2.exe")
            .with_path(r"D:\SteamLibrary\steamapps\common\Day of Defeat Source\hl2.exe"),
    );
    assert_eq!(outcome, Match::None, "got {outcome:?}");

    // Half-Life 2 Deathmatch is the case a directory name that merely does not
    // collide would not have caught. It is Steam application 320, a different
    // game from Half-Life 2, it runs the same `hl2.exe`, and its install
    // directory has the shipped `half-life-2` qualifier as a *prefix*. A
    // substring test claims it as Half-Life 2 — confidently, at the second
    // strongest rung, with nothing reported as ambiguous.
    let outcome = catalogue.match_process(
        &ProcessCandidate::new("hl2.exe")
            .with_path(r"D:\SteamLibrary\steamapps\common\Half-Life 2 Deathmatch\hl2.exe"),
    );
    assert_eq!(outcome, Match::None, "got {outcome:?}");

    // Episode One, Episode Two and Lost Coast are the same shape, and the same
    // rule answers for all of them: the fragment names a directory, so it
    // matches that directory and not one it is a prefix of.
    let outcome = catalogue.match_process(
        &ProcessCandidate::new("hl2.exe")
            .with_path(r"D:\SteamLibrary\steamapps\common\Half-Life 2 Episode One\hl2.exe"),
    );
    assert_eq!(outcome, Match::None, "got {outcome:?}");
}

#[test]
fn the_shipped_catalogue_recognises_a_game_from_its_steam_application_id_alone() {
    let catalogue = Catalogue::seed().expect("the shipped seed data is valid");

    // Steam knows the identifier of the process it started (#43); the file
    // name here is deliberately one the catalogue has never heard of.
    let outcome = catalogue.match_process(
        &ProcessCandidate::new("launcher.exe").from_launcher(LauncherKind::Steam, "730"),
    );
    assert_eq!(matched(&outcome), "counter-strike-2");
    assert!(matches!(
        outcome,
        Match::One {
            strength: MatchStrength::LauncherIdentity,
            ..
        }
    ));
}

#[test]
fn a_user_adds_a_game_of_their_own_by_writing_one_file() {
    let directory = TestDirectory::new("own-game");
    let path = directory.overlay(
        r#"schema_version = 1

# A game nobody has contributed upstream yet.
[[game]]
game_id = "my-obscure-game"
name = "My Obscure Game"

[[game.executables]]
name = "obscure.exe"
"#,
    );

    let loaded = Catalogue::load_with_overlay_at(&path).expect("the overlay loads");
    assert_eq!(
        loaded.overlay(),
        &OverlayStatus::Loaded {
            path: path.clone(),
            entries: 1
        }
    );

    let catalogue = loaded.into_catalogue();
    let outcome = catalogue.match_process(&ProcessCandidate::new("obscure.exe"));
    assert_eq!(matched(&outcome), "my-obscure-game");
    assert_eq!(
        catalogue
            .find_by_id("my-obscure-game")
            .expect("the entry is in the catalogue")
            .source(),
        &EntrySource::Overlay { path }
    );
    // And the shipped entries are all still there.
    assert!(catalogue.find_by_id("counter-strike-2").is_some());
}

#[test]
fn a_users_entry_replaces_the_shipped_one_for_the_same_game() {
    let directory = TestDirectory::new("shadow");
    let path = directory.overlay(
        r#"schema_version = 1

# I keep this one on a second drive with a directory name of my own.
[[game]]
game_id = "counter-strike-2"
name = "CS2"

[[game.executables]]
name = "cs2.exe"
"#,
    );

    let catalogue = Catalogue::load_with_overlay_at(&path)
        .expect("the overlay loads")
        .into_catalogue();

    let entry = catalogue
        .find_by_id("counter-strike-2")
        .expect("the game is still in the catalogue");
    assert_eq!(entry.name(), "CS2");
    assert!(entry.source().is_overlay());
    assert_eq!(
        entry.launcher().map(|launcher| launcher.kind()),
        None,
        "an overlay entry replaces the shipped one rather than merging with it"
    );

    // The shipped entry's path qualifier is gone with it, so the user's
    // installation matches wherever they keep it.
    let outcome =
        catalogue.match_process(&ProcessCandidate::new("cs2.exe").with_path(r"E:\Games\cs2.exe"));
    assert_eq!(matched(&outcome), "counter-strike-2");
}

#[test]
fn a_broken_overlay_names_the_file_and_the_entry_and_does_not_take_the_seed_down_with_it() {
    let directory = TestDirectory::new("broken");
    let path = directory.overlay(
        r#"schema_version = 1

[[game]]
game_id = "my-game"
name = "My Game"

[[game.executables]]
name = "C:/Games/MyGame/my-game.exe"
"#,
    );

    let error = Catalogue::load_with_overlay_at(&path).expect_err("the entry is not valid");
    let message = error.to_string();
    assert!(
        matches!(error, CatalogueError::InvalidEntry { .. }),
        "expected an invalid entry, got {error:?}"
    );
    assert!(
        message.contains(&path.display().to_string()),
        "the message should name the file: {message}"
    );
    assert!(
        message.contains("entry 1 (`my-game`)"),
        "the message should name the entry: {message}"
    );

    // A caller that wants to carry on has somewhere to carry on from.
    assert!(!Catalogue::seed()
        .expect("the seed is valid")
        .entries()
        .is_empty());
}

#[test]
fn an_installation_with_no_overlay_gets_the_shipped_catalogue() {
    let directory = TestDirectory::new("no-overlay");
    let path = directory.0.join("games.toml");

    let loaded = Catalogue::load_with_overlay_at(&path).expect("an absent overlay is fine");

    assert_eq!(loaded.overlay(), &OverlayStatus::Absent { path });
    assert_eq!(
        loaded.catalogue().entries().len(),
        Catalogue::seed()
            .expect("the seed is valid")
            .entries()
            .len()
    );
}
