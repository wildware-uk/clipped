//! Reading Ubisoft Connect's install entries.
//!
//! Every case builds the entries it is about with [`Ubisoft::from_installs`]
//! rather than writing to the registry, so nothing here needs Ubisoft Connect
//! installed — the third acceptance criterion of
//! [issue #44](https://github.com/wildware-uk/clipped/issues/44): a provider
//! whose behaviour could only be checked on a machine that has the launcher is
//! one nobody can change safely (AGENTS.md section 25).
//!
//! The values are spelled the way the registry really spells them, taken from a
//! machine with two Ubisoft games on it: forward slashes and a trailing one.
//! Writing them the way a Windows path is usually written would make these
//! tests agree with a provider that never worked
//! ([issue #459](https://github.com/wildware-uk/clipped/issues/459) is what that
//! costs).

use super::*;
use crate::catalogue::MatchStrength;

/// The launcher's own directory, as `InstallDir` spells it.
const GAMES: &str = "C:/Program Files (x86)/Ubisoft/Ubisoft Game Launcher/games";

/// The two games the real machine had, and the way it had them.
fn two_games() -> Ubisoft {
    Ubisoft::from_installs([
        (
            "15657".to_owned(),
            format!("{GAMES}/XDefiant/"),
            Some("XDefiant".to_owned()),
        ),
        (
            "5595".to_owned(),
            format!("{GAMES}/Trackmania/"),
            Some("Trackmania".to_owned()),
        ),
    ])
}

#[test]
fn a_games_own_executable_is_claimed_by_its_identifier() {
    let ubisoft = two_games();

    let app = ubisoft
        .app_for(
            r"C:\Program Files (x86)\Ubisoft\Ubisoft Game Launcher\games\XDefiant\XDefiant.exe",
        )
        .expect("the game's own executable is in its install directory");

    assert_eq!(app.id(), "15657");
    assert_eq!(app.name(), "XDefiant");
}

#[test]
fn a_path_the_process_table_spells_with_backslashes_matches_a_value_spelled_with_forward_ones() {
    // The registry writes `C:/Program Files (x86)/...` and a running process
    // reports `C:\Program Files (x86)\...`. If the provider compared these as
    // strings it would claim nothing at all on a real machine, and every test
    // above would still pass if they were written in one spelling.
    let ubisoft = Ubisoft::from_installs([(
        "5595".to_owned(),
        format!("{GAMES}/Trackmania/"),
        Some("Trackmania".to_owned()),
    )]);

    assert!(
        ubisoft
            .app_for(
                r"C:\Program Files (x86)\Ubisoft\Ubisoft Game Launcher\games\Trackmania\Trackmania.exe"
            )
            .is_some(),
        "the two spellings name the same directory"
    );
}

#[test]
fn something_the_game_started_deeper_in_its_own_directory_is_still_that_game() {
    // XDefiant ships its anti-cheat in a subdirectory and starts it itself. It
    // is not a separate game and must not stop being XDefiant.
    let ubisoft = two_games();

    let app = ubisoft
        .app_for(
            r"C:\Program Files (x86)\Ubisoft\Ubisoft Game Launcher\games\XDefiant\BattlEye\BEService_x64.exe",
        )
        .expect("a program below the install directory belongs to it");

    assert_eq!(app.id(), "15657");
}

#[test]
fn a_process_outside_every_install_directory_is_claimed_by_nothing() {
    let ubisoft = two_games();

    assert!(
        ubisoft
            .app_for(r"D:\Steam\steamapps\common\cs2\game\bin\win64\cs2.exe")
            .is_none(),
        "a game installed by somebody else is not Ubisoft's to claim"
    );
}

#[test]
fn the_install_directory_itself_is_not_a_program_inside_it() {
    // The guard that keeps a directory from claiming itself. Without it, a path
    // exactly equal to the install directory would match, and the launcher's
    // own `games` directory would swallow anything compared against it.
    let ubisoft = two_games();

    assert!(
        ubisoft
            .app_for(r"C:\Program Files (x86)\Ubisoft\Ubisoft Game Launcher\games\XDefiant")
            .is_none(),
        "a directory is not a program in that directory"
    );
}

#[test]
fn a_directory_inside_another_is_claimed_by_the_deeper_one() {
    // One install directory sitting inside another. The more specific answer is
    // the right one, and the order they are declared in must not decide it —
    // so this asserts it from the shallower-first order, which is the one a
    // naive first-match implementation gets wrong.
    let ubisoft = Ubisoft::from_installs([
        ("1".to_owned(), GAMES.to_owned(), Some("Outer".to_owned())),
        (
            "2".to_owned(),
            format!("{GAMES}/Trackmania"),
            Some("Trackmania".to_owned()),
        ),
    ]);

    let app = ubisoft
        .app_for(
            r"C:\Program Files (x86)\Ubisoft\Ubisoft Game Launcher\games\Trackmania\Trackmania.exe",
        )
        .expect("something claims it");

    assert_eq!(app.id(), "2", "the deeper install directory is the answer");
}

#[test]
fn two_identifiers_claiming_one_directory_are_refused_rather_than_guessed_between() {
    // Ubisoft's registry records no executable, so there is nothing to break
    // this tie with. Choosing would hand the catalogue a launcher identity for
    // the wrong game, which is exactly the defect #459 was: an arbitrary
    // tie-break that looked right until real data had a tie in it.
    let ubisoft = Ubisoft::from_installs([
        (
            "5595".to_owned(),
            format!("{GAMES}/Trackmania/"),
            Some("Trackmania".to_owned()),
        ),
        (
            "9999".to_owned(),
            format!("{GAMES}/Trackmania/"),
            Some("Trackmania Deluxe".to_owned()),
        ),
    ]);

    assert!(
        ubisoft
            .app_for(
                r"C:\Program Files (x86)\Ubisoft\Ubisoft Game Launcher\games\Trackmania\Trackmania.exe"
            )
            .is_none(),
        "two games claiming one directory cannot be told apart, and saying so is the point"
    );
}

#[test]
fn a_game_with_no_readable_name_is_still_detected_and_named_after_its_identifier() {
    // `DisplayName` is in the uninstall key, which is somebody else's namespace
    // and can be missing. Losing the game over it would be the wrong trade:
    // the identifier is what the catalogue matches on.
    let ubisoft =
        Ubisoft::from_installs([("5595".to_owned(), format!("{GAMES}/Trackmania/"), None)]);

    let app = ubisoft
        .app_for(
            r"C:\Program Files (x86)\Ubisoft\Ubisoft Game Launcher\games\Trackmania\Trackmania.exe",
        )
        .expect("a nameless game is still an installed game");

    assert_eq!(app.id(), "5595");
    assert_eq!(app.name(), "5595", "the identifier stands in for the name");
}

#[test]
fn an_empty_name_falls_back_the_same_way_an_absent_one_does() {
    let ubisoft = Ubisoft::from_installs([(
        "5595".to_owned(),
        format!("{GAMES}/Trackmania/"),
        Some("   ".to_owned()),
    )]);

    assert_eq!(ubisoft.apps()[0].name(), "5595");
}

#[test]
fn a_key_that_names_no_directory_is_reported_and_costs_no_other_game() {
    // Ubisoft leaves an install key behind between an uninstall and the
    // launcher next tidying up. One of those must not cost the user every other
    // game the launcher knows about.
    let ubisoft = Ubisoft::from_installs([
        ("404".to_owned(), String::new(), Some("Gone".to_owned())),
        (
            "5595".to_owned(),
            format!("{GAMES}/Trackmania/"),
            Some("Trackmania".to_owned()),
        ),
    ]);

    assert_eq!(ubisoft.apps().len(), 1, "the good game survives");
    assert_eq!(ubisoft.apps()[0].id(), "5595");

    let problems = ubisoft.problems();
    assert_eq!(problems.len(), 1, "and the bad key is reported");
    assert!(
        problems[0].to_string().contains("404"),
        "a problem has to name the key it is about: {}",
        problems[0]
    );
}

#[test]
fn a_process_inside_a_ubisoft_installation_reaches_the_catalogue_as_one() {
    let ubisoft = two_games();

    let candidate = ubisoft.candidate_for(
        "XDefiant.exe",
        r"C:\Program Files (x86)\Ubisoft\Ubisoft Game Launcher\games\XDefiant\XDefiant.exe",
    );

    assert_eq!(
        candidate.launcher(),
        Some((LauncherKind::Ubisoft, "15657")),
        "the identity is what lets the catalogue match a game whose executable it does not know"
    );
}

#[test]
fn a_process_ubisoft_does_not_know_about_carries_no_launcher_identity() {
    // The half that keeps this from making detection worse: an absent identity
    // is what makes the catalogue fall back to its path and name rungs, so a
    // game installed without Ubisoft matches exactly as well as it did before.
    let ubisoft = two_games();

    let candidate = ubisoft.candidate_for("cs2.exe", r"D:\Steam\steamapps\common\cs2\game\cs2.exe");

    assert_eq!(candidate.launcher(), None);
}

#[test]
fn the_launcher_identity_is_the_rung_the_catalogue_matches_it_on() {
    // What the whole provider is for: reaching
    // `MatchStrength::LauncherIdentity`, the rung above every other, so that a
    // game whose process is called something generic is still identified.
    let ubisoft = two_games();
    let candidate = ubisoft.candidate_for(
        "XDefiant.exe",
        r"C:\Program Files (x86)\Ubisoft\Ubisoft Game Launcher\games\XDefiant\XDefiant.exe",
    );

    let (kind, id) = candidate.launcher().expect("Ubisoft claimed it");
    assert_eq!(kind, LauncherKind::Ubisoft);
    assert_eq!(id, "15657");
    assert!(
        MatchStrength::LauncherIdentity > MatchStrength::ExecutableName,
        "the launcher rung has to outrank the name rung for any of this to matter"
    );
}

#[test]
fn a_machine_without_ubisoft_connect_is_not_a_failure() {
    // `discover` on a machine with no Ubisoft key answers `Ok(None)`. This runs
    // everywhere, so it cannot assert which of the two it gets — what it does
    // assert is that neither is an error, because reporting "not installed" as
    // a fault would put a warning on most machines.
    let found = Ubisoft::discover().expect("an absent launcher is not a failure");
    if let Some(ubisoft) = found {
        // On a machine that does have it, every app that came back has to carry
        // the two things a candidate is built from.
        for app in ubisoft.apps() {
            assert!(!app.id().trim().is_empty(), "an app with no identifier");
            assert!(
                !app.installation_directory().as_os_str().is_empty(),
                "an app with no install directory reached `apps`"
            );
        }
    }
}
