//! Steam detection against files Steam wrote.
//!
//! Every `.vdf` and `.acf` these tests read came off a real client and is
//! checked in under `tests/fixtures/steam/` with its own README explaining what
//! was copied and what was scrubbed. That is deliberate and it is most of the
//! value here: a KeyValues fixture written by hand agrees with the parser that
//! reads it by construction, and would prove nothing about Valve's tabs, Valve's
//! `\\`-escaped paths, or the four nested tables at the bottom of every
//! manifest.
//!
//! What is built here is the situation a single-library implementation gets
//! wrong: **two libraries**, the second one holding the game, exactly as the
//! machine this was developed on has it. The library paths inside
//! `libraryfolders.vdf` are the one thing substituted, because a fixture cannot
//! name `B:\SteamLibrary` on a machine that has no B drive.

use std::fs;
use std::path::{Path, PathBuf};

use clipped_game_detection::catalogue::{Catalogue, Match, MatchStrength, ProcessCandidate};
use clipped_game_detection::launcher::steam::{Steam, SteamError};

/// The library paths inside the fixture, as Steam wrote them.
const FIXTURE_DEFAULT_LIBRARY: &str = r"C:\Program Files (x86)\Steam";
/// See [`FIXTURE_DEFAULT_LIBRARY`].
const FIXTURE_SECOND_LIBRARY: &str = r"B:\SteamLibrary";

/// A directory of one test's own, removed when it is dropped.
#[derive(Debug)]
struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "clipped-steam-it-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("the temporary directory can be created");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A Steam installation with two libraries, built out of the fixtures.
///
/// `<temp>/Steam` is the installation and the default library;
/// `<temp>/SecondLibrary` is the other one, and holds both games.
#[derive(Debug)]
struct Installation {
    directory: TestDirectory,
}

impl Installation {
    fn new(label: &str) -> Self {
        let directory = TestDirectory::new(label);
        let installation = Self { directory };

        installation.write_library_index(&[
            (FIXTURE_DEFAULT_LIBRARY, installation.root()),
            (FIXTURE_SECOND_LIBRARY, installation.second_library()),
        ]);
        installation.install("appmanifest_228980.acf", installation.root());
        installation.install("appmanifest_730.acf", installation.second_library());
        installation.install("appmanifest_620.acf", installation.second_library());

        // Not needed to be found — nothing here stats an installation directory
        // — but the path the tests hand to `app_for_path` should be a path that
        // exists, as it would be when it came from a running process.
        fs::create_dir_all(
            installation
                .counter_strike_directory()
                .join("game/bin/win64"),
        )
        .expect("the game directory can be created");
        fs::write(installation.counter_strike_executable(), b"").expect("cs2.exe can be created");

        installation
    }

    /// The Steam installation, which is also the default library.
    fn root(&self) -> PathBuf {
        self.directory.path().join("Steam")
    }

    /// The other library.
    fn second_library(&self) -> PathBuf {
        self.directory.path().join("SecondLibrary")
    }

    fn counter_strike_directory(&self) -> PathBuf {
        self.second_library()
            .join("steamapps")
            .join("common")
            .join("Counter-Strike Global Offensive")
    }

    fn counter_strike_executable(&self) -> PathBuf {
        self.counter_strike_directory()
            .join("game")
            .join("bin")
            .join("win64")
            .join("cs2.exe")
    }

    /// Writes the fixture library index with its paths pointed at real
    /// directories.
    ///
    /// The substitution is asserted rather than assumed: a fixture edited so
    /// that these strings no longer appear would otherwise leave every test in
    /// this file passing against an installation with no libraries in it.
    fn write_library_index(&self, libraries: &[(&str, PathBuf)]) {
        let mut text = fixture("libraryfolders.vdf");
        for (fixture_path, real_path) in libraries {
            let escaped = escape(fixture_path);
            assert!(
                text.contains(&escaped),
                "the fixture should still name {fixture_path}"
            );
            text = text.replace(&escaped, &escape(&real_path.to_string_lossy()));
            fs::create_dir_all(real_path.join("steamapps"))
                .expect("a library directory can be created");
        }
        fs::write(
            self.root().join("steamapps").join("libraryfolders.vdf"),
            text,
        )
        .expect("the library index can be written");
    }

    /// Copies a manifest fixture into a library, unchanged.
    fn install(&self, manifest: &str, library: PathBuf) {
        fs::write(library.join("steamapps").join(manifest), fixture(manifest))
            .expect("a manifest can be written");
    }

    /// Puts a file in Steam's artwork cache.
    fn cache_artwork(&self, relative: &str) -> PathBuf {
        let path = self
            .root()
            .join("appcache")
            .join("librarycache")
            .join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
        fs::create_dir_all(path.parent().expect("the cache path has a parent"))
            .expect("the artwork cache can be created");
        fs::write(&path, b"").expect("the artwork can be written");
        path
    }

    fn read(&self) -> Steam {
        Steam::read_at(self.root()).expect("the fixture installation reads")
    }
}

/// The text of a fixture, as Steam wrote it.
fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("steam")
        .join(name);
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} is readable: {error}", path.display()))
}

/// A Windows path as KeyValues spells it.
fn escape(path: &str) -> String {
    path.replace('\\', r"\\")
}

#[test]
fn a_game_in_the_second_library_is_found_with_the_name_steam_gives_it() {
    // The acceptance criterion, and the case a single-library implementation
    // gets wrong. Counter-Strike 2 is in the library that is not the default
    // one, and its directory is named after the game it replaced, so neither
    // the library nor the name can be inferred from the other.
    let installation = Installation::new("second-library");
    let steam = installation.read();

    let app = steam
        .app_by_id("730")
        .expect("Counter-Strike 2 is installed in the second library");
    assert_eq!(app.name(), "Counter-Strike 2");
    assert_eq!(
        app.installation_directory(),
        installation.counter_strike_directory()
    );
    assert_eq!(app.library(), installation.second_library());
}

#[test]
fn both_libraries_are_read_and_the_default_one_is_not_read_twice() {
    // Steam lists its own directory in the index, so a reader that took the
    // file at face value would scan the default library twice and report every
    // application in it twice.
    let installation = Installation::new("libraries");
    let steam = installation.read();

    assert_eq!(
        steam.libraries(),
        [installation.root(), installation.second_library()]
    );

    let found: Vec<(&str, &str)> = steam
        .apps()
        .iter()
        .map(|app| (app.app_id(), app.name()))
        .collect();
    assert_eq!(
        found,
        [
            ("228980", "Steamworks Common Redistributables"),
            ("620", "Portal 2"),
            ("730", "Counter-Strike 2"),
        ],
        "every manifest in both libraries, once each, in identifier order"
    );
    assert!(
        steam.problems().is_empty(),
        "a healthy installation has nothing to report: {:?}",
        steam.problems()
    );
}

#[test]
fn steam_names_the_game_behind_an_executable_the_catalogue_cannot_place() {
    // This is the whole point of the launcher rung, and it is why the test does
    // not use `cs2.exe`: the catalogue can already recognise that by name and
    // path. A launcher process inside the game's own directory is something the
    // catalogue has nothing to say about, and Steam does.
    let installation = Installation::new("identity");
    let steam = installation.read();
    let catalogue = Catalogue::seed().expect("the shipped seed data is valid");

    let launcher = installation
        .counter_strike_directory()
        .join("some-launcher.exe");
    let launcher = launcher.to_string_lossy().into_owned();

    let unaided =
        catalogue.match_process(&ProcessCandidate::new("some-launcher.exe").with_path(&launcher));
    assert_eq!(
        unaided,
        Match::None,
        "the catalogue alone should have nothing to say about this process"
    );

    let outcome = catalogue.match_process(&steam.candidate_for("some-launcher.exe", &launcher));
    let Match::One { entry, strength } = outcome else {
        panic!("expected Steam's identity to place the process, got {outcome:?}");
    };
    assert_eq!(entry.game_id().as_str(), "counter-strike-2");
    assert_eq!(strength, MatchStrength::LauncherIdentity);
}

#[test]
fn a_real_game_executable_is_placed_by_the_launcher_rung_rather_than_by_its_path() {
    let installation = Installation::new("cs2");
    let steam = installation.read();
    let catalogue = Catalogue::seed().expect("the shipped seed data is valid");

    let executable = installation.counter_strike_executable();
    let executable = executable.to_string_lossy().into_owned();

    let candidate = steam.candidate_for("cs2.exe", &executable);
    assert_eq!(
        candidate.launcher().map(|(_, app_id)| app_id),
        Some("730"),
        "Steam should claim an executable inside its own installation directory"
    );

    let outcome = catalogue.match_process(&candidate);
    assert_eq!(
        outcome.entry().map(|entry| entry.game_id().as_str()),
        Some("counter-strike-2")
    );
    assert!(
        matches!(
            outcome,
            Match::One {
                strength: MatchStrength::LauncherIdentity,
                ..
            }
        ),
        "the launcher rung outranks the path qualifier that would also have matched"
    );
}

#[test]
fn an_executable_outside_every_library_gets_no_launcher_identity() {
    // A process Steam did not start must not be attributed to a Steam game, and
    // the candidate still carries what the catalogue's other rungs need.
    let installation = Installation::new("outside");
    let steam = installation.read();

    let candidate = steam.candidate_for("notepad.exe", r"C:\Windows\System32\notepad.exe");
    assert_eq!(candidate.launcher(), None);
    assert_eq!(
        candidate.executable_path(),
        Some(r"C:\Windows\System32\notepad.exe")
    );
    assert_eq!(steam.app_for_path(r"C:\Windows\System32\notepad.exe"), None);
}

#[test]
fn an_installation_directory_is_not_a_program_inside_it() {
    let installation = Installation::new("directory");
    let steam = installation.read();

    let directory = installation.counter_strike_directory();
    assert_eq!(
        steam.app_for_path(&directory.to_string_lossy()),
        None,
        "the directory itself is not an executable in it"
    );
}

#[test]
fn a_neighbouring_directory_whose_name_starts_the_same_is_a_different_game() {
    // `common\Portal 2` starts with `common\Portal`, and Steam has shipped both.
    // Comparing characters rather than directory names would answer Portal 2
    // about a process in a directory that is not Portal 2's.
    let installation = Installation::new("prefix");
    let steam = installation.read();

    let neighbour = installation
        .second_library()
        .join("steamapps")
        .join("common")
        .join("Portal 2 Community Edition")
        .join("portal2.exe");
    assert_eq!(steam.app_for_path(&neighbour.to_string_lossy()), None);
}

#[test]
fn the_innermost_installation_directory_answers_when_two_nest() {
    // A tool installed inside another game's directory would otherwise be
    // reported as that game.
    let installation = Installation::new("nested");
    // Derived from the real manifest by changing two values, so what is being
    // parsed is still Steam's own text.
    let inner = fixture("appmanifest_730.acf")
        .replace(r#""730""#, r#""1234567""#)
        .replace(
            "Counter-Strike Global Offensive",
            r"Counter-Strike Global Offensive\\game\\csgo",
        );
    assert!(
        inner.contains("1234567") && inner.contains("csgo"),
        "the derived manifest should differ from the fixture it came from"
    );
    fs::write(
        installation
            .second_library()
            .join("steamapps")
            .join("appmanifest_1234567.acf"),
        inner,
    )
    .expect("the derived manifest can be written");

    let steam = installation.read();
    let inside = installation
        .counter_strike_directory()
        .join("game")
        .join("csgo")
        .join("tool.exe");
    assert_eq!(
        steam
            .app_for_path(&inside.to_string_lossy())
            .map(|app| app.app_id()),
        Some("1234567")
    );
}

#[test]
fn an_icon_steam_has_already_downloaded_is_reported() {
    let installation = Installation::new("icons");
    let capsule = installation.cache_artwork("730/library_600x900.jpg");
    // The layout Steam used before it moved to a directory per application.
    let legacy = installation.cache_artwork("620_icon.jpg");

    let steam = installation.read();
    assert_eq!(
        steam.app_by_id("730").and_then(|app| app.icon()),
        Some(capsule.as_path())
    );
    assert_eq!(
        steam.app_by_id("620").and_then(|app| app.icon()),
        Some(legacy.as_path())
    );
    assert_eq!(
        steam.app_by_id("228980").and_then(|app| app.icon()),
        None,
        "an application with nothing cached has no icon rather than a made-up one"
    );
}

#[test]
fn the_small_icon_wins_over_the_larger_artwork() {
    let installation = Installation::new("icon-order");
    let legacy = installation.cache_artwork("730_icon.jpg");
    installation.cache_artwork("730/library_600x900.jpg");
    installation.cache_artwork("730/header.jpg");
    installation.cache_artwork("730/logo.png");

    let steam = installation.read();
    assert_eq!(
        steam.app_by_id("730").and_then(|app| app.icon()),
        Some(legacy.as_path())
    );
}

#[test]
fn a_manifest_that_is_not_keyvalues_is_named_and_the_other_games_still_load() {
    // Steam rewrites these files while games install, so a half-written one is
    // an ordinary state of the disk. Refusing to detect any game because of one
    // would be the wrong trade — but so would dropping it silently.
    let installation = Installation::new("malformed");
    let broken = installation
        .second_library()
        .join("steamapps")
        .join("appmanifest_999999.acf");
    fs::write(&broken, "\"AppState\"\n{\n    \"appid\" \"999999\"\n")
        .expect("the broken manifest can be written");

    let steam = installation.read();
    assert!(
        steam.app_by_id("730").is_some(),
        "the other libraries and manifests still load"
    );

    let problems: Vec<String> = steam
        .problems()
        .iter()
        .map(std::string::ToString::to_string)
        .collect();
    assert_eq!(
        problems.len(),
        1,
        "one problem, not none and not all: {problems:?}"
    );
    assert!(
        problems[0].contains("appmanifest_999999.acf"),
        "the problem should name the file: {problems:?}"
    );
    assert!(
        problems[0].contains("line 4"),
        "and the line the reader gave up on: {problems:?}"
    );
}

#[test]
fn a_manifest_missing_the_keys_that_matter_is_named_rather_than_half_believed() {
    let installation = Installation::new("shape");
    let text = fixture("appmanifest_620.acf");
    let kept: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim_start().starts_with("\"name\""))
        .collect();
    assert_eq!(
        kept.len() + 1,
        text.lines().count(),
        "exactly the `name` line should have gone"
    );
    let without_name = kept.join("\n");
    fs::write(
        installation
            .second_library()
            .join("steamapps")
            .join("appmanifest_620.acf"),
        without_name,
    )
    .expect("the edited manifest can be written");

    let steam = installation.read();
    assert!(steam.app_by_id("620").is_none());
    let problem = steam
        .problems()
        .first()
        .map(std::string::ToString::to_string)
        .expect("the missing key is reported");
    assert!(
        problem.contains("appmanifest_620.acf") && problem.contains("`name`"),
        "the problem should name the file and the key: {problem}"
    );
}

#[test]
fn a_library_on_a_drive_that_is_not_there_is_reported_rather_than_ignored() {
    // The ordinary cause is an external drive that is not plugged in. Detection
    // carries on with the libraries that are, and says which one it lost.
    let installation = Installation::new("absent-library");
    let absent = installation.directory.path().join("NotPluggedIn");
    installation.write_library_index(&[
        (FIXTURE_DEFAULT_LIBRARY, installation.root()),
        (FIXTURE_SECOND_LIBRARY, absent.clone()),
    ]);
    let _ = fs::remove_dir_all(&absent);

    let steam = installation.read();
    assert!(
        steam.app_by_id("228980").is_some(),
        "the library that is there is still read"
    );
    let problem = steam
        .problems()
        .first()
        .map(std::string::ToString::to_string)
        .expect("the missing library is reported");
    assert!(
        problem.contains("NotPluggedIn"),
        "the problem should name the library: {problem}"
    );
}

#[test]
fn a_library_index_that_is_not_keyvalues_fails_naming_the_file() {
    // Fatal, unlike a manifest: without the index there is no coherent view of
    // anything, and guessing at a directory layout would be inventing data.
    let installation = Installation::new("broken-index");
    let index = installation
        .root()
        .join("steamapps")
        .join("libraryfolders.vdf");
    fs::write(&index, "\"libraryfolders\"\n{\n\t\"0\"\n").expect("the index can be overwritten");

    let error = Steam::read_at(installation.root()).expect_err("an unreadable index is fatal");
    assert!(
        matches!(error, SteamError::Syntax { .. }),
        "expected a syntax error, got {error:?}"
    );
    let message = error.to_string();
    assert!(
        message.contains("libraryfolders.vdf"),
        "the message should name the file: {message}"
    );
}

#[test]
fn an_index_kept_in_the_older_place_is_read_from_there() {
    // Current clients write the same file to `config\` as well; a client that
    // keeps only that one is read correctly.
    let installation = Installation::new("config-index");
    let steamapps = installation
        .root()
        .join("steamapps")
        .join("libraryfolders.vdf");
    let text = fs::read_to_string(&steamapps).expect("the index was written");
    fs::remove_file(&steamapps).expect("the index can be moved");
    let config = installation.root().join("config");
    fs::create_dir_all(&config).expect("the config directory can be created");
    fs::write(config.join("libraryfolders.vdf"), text).expect("the index can be written");

    let steam = installation.read();
    assert!(
        steam.app_by_id("730").is_some(),
        "the second library is still found from the older location"
    );
}

#[test]
fn an_installation_with_no_index_at_all_is_its_own_library() {
    // A client installed and never run. Nothing is missing, so nothing is
    // reported as a problem.
    let installation = Installation::new("no-index");
    fs::remove_file(
        installation
            .root()
            .join("steamapps")
            .join("libraryfolders.vdf"),
    )
    .expect("the index can be removed");

    let steam = installation.read();
    assert_eq!(steam.libraries(), [installation.root()]);
    assert!(steam.app_by_id("228980").is_some());
    assert!(steam.app_by_id("730").is_none());
    assert!(
        steam.problems().is_empty(),
        "no index is not a problem: {:?}",
        steam.problems()
    );
}

#[test]
fn a_directory_that_is_not_there_is_refused_by_name() {
    let directory = TestDirectory::new("missing");
    let path = directory.path().join("no-such-steam");
    let error = Steam::read_at(&path).expect_err("a directory that is not there is not Steam");
    assert!(
        matches!(error, SteamError::MissingRoot { .. }),
        "expected a missing root, got {error:?}"
    );
    assert!(
        error.to_string().contains("no-such-steam"),
        "the message should name the directory: {error}"
    );
}
