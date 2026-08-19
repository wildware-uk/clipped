//! Reading the gaming services package repository.
//!
//! Every case builds its entries with [`Xbox::from_packages`] rather than
//! writing to the registry, so nothing here needs an Xbox game installed — the
//! third acceptance criterion of
//! [issue #44](https://github.com/wildware-uk/clipped/issues/44).
//!
//! The package names are the ones a real machine had, including the one that
//! breaks the obvious parse. Inventing tidier names would have produced a
//! provider that passed every test and lost a game in six on contact with a
//! real installation, which is what
//! [issue #459](https://github.com/wildware-uk/clipped/issues/459) cost.

use super::*;
use crate::catalogue::MatchStrength;

/// The packages the real machine had, spelled as its registry spelled them.
fn six_packages() -> Xbox {
    Xbox::from_packages([
        (
            "BethesdaSoftworks.ProjectAltar_1.0.12.0_x64__3275kfvn8vcwc",
            r"\\?\B:\WindowsApps\BethesdaSoftworks.ProjectAltar_1.0.12.0_x64__3275kfvn8vcwc\",
        ),
        (
            "Microsoft.Limitless_1.8.14.0_x64__8wekyb3d8bbwe",
            r"\\?\B:\WindowsApps\Microsoft.Limitless_1.8.14.0_x64__8wekyb3d8bbwe\",
        ),
        (
            "38985CA0.COREBase_1.0.203.0_x64_ww_5bkah9njm3e9g",
            r"\\?\B:\WindowsApps\38985CA0.COREBase_1.0.203.0_x64_ww_5bkah9njm3e9g\",
        ),
        (
            "Microsoft.4297127D64EC6_2.6.2.0_x64__8wekyb3d8bbwe",
            r"\\?\C:\Program Files\WindowsApps\Microsoft.4297127D64EC6_2.6.2.0_x64__8wekyb3d8bbwe\",
        ),
    ])
}

#[test]
fn a_package_with_a_resource_qualifier_still_yields_a_family_name() {
    // The trap. `38985CA0.COREBase_1.0.203.0_x64_ww_5bkah9njm3e9g` carries a
    // resource qualifier and has a *single* underscore before the publisher,
    // where every other package on the same machine has two. Splitting on `__`
    // silently drops it.
    assert_eq!(
        family_name("38985CA0.COREBase_1.0.203.0_x64_ww_5bkah9njm3e9g").as_deref(),
        Some("38985CA0.COREBase_5bkah9njm3e9g"),
        "a `__` split loses this one"
    );

    // And the ordinary shape still works.
    assert_eq!(
        family_name("Microsoft.Limitless_1.8.14.0_x64__8wekyb3d8bbwe").as_deref(),
        Some("Microsoft.Limitless_8wekyb3d8bbwe")
    );
}

#[test]
fn the_derived_family_name_is_the_one_the_repository_itself_uses() {
    // Checkable rather than assumed: the same machine's repository has a
    // `GameSave` entry keyed by the family name, and this is that key.
    assert_eq!(
        family_name("38985CA0.COREBase_1.0.203.0_x64_ww_5bkah9njm3e9g").as_deref(),
        Some("38985CA0.COREBase_5bkah9njm3e9g")
    );
}

#[test]
fn something_that_is_not_a_package_full_name_yields_nothing() {
    assert_eq!(family_name("").as_deref(), None);
    assert_eq!(family_name("NoUnderscoresHere").as_deref(), None);
    assert_eq!(family_name("_leadingunderscore").as_deref(), None);
    assert_eq!(family_name("trailing_").as_deref(), None);
}

#[test]
fn an_extended_length_root_matches_a_path_a_process_reports() {
    // The registry writes `\\?\B:\WindowsApps\...` and a running process
    // reports `B:\WindowsApps\...`. Comparing them without stripping the prefix
    // claims nothing at all on a real machine.
    let xbox = six_packages();

    let app = xbox
        .app_for(r"B:\WindowsApps\Microsoft.Limitless_1.8.14.0_x64__8wekyb3d8bbwe\Limitless.exe")
        .expect("the prefix is not part of the path a process reports");

    assert_eq!(app.family_name(), "Microsoft.Limitless_8wekyb3d8bbwe");
}

#[test]
fn a_game_on_another_drive_is_found_too() {
    // Xbox games are not all under one root: this machine had four on `B:` and
    // one on `C:`. A provider that assumed `C:\XboxGames` or
    // `C:\Program Files\WindowsApps` would find a fraction of them.
    let xbox = six_packages();

    let app = xbox
        .app_for(
            r"C:\Program Files\WindowsApps\Microsoft.4297127D64EC6_2.6.2.0_x64__8wekyb3d8bbwe\game.exe",
        )
        .expect("the repository is what knows where each game went");

    assert_eq!(app.family_name(), "Microsoft.4297127D64EC6_8wekyb3d8bbwe");
}

#[test]
fn something_the_game_started_deeper_in_its_own_directory_is_still_that_game() {
    let xbox = six_packages();

    let app = xbox
        .app_for(
            r"B:\WindowsApps\BethesdaSoftworks.ProjectAltar_1.0.12.0_x64__3275kfvn8vcwc\Engine\Binaries\Win64\anticheat.exe",
        )
        .expect("a program below the install directory belongs to it");

    assert_eq!(
        app.family_name(),
        "BethesdaSoftworks.ProjectAltar_3275kfvn8vcwc"
    );
}

#[test]
fn a_process_outside_every_package_is_claimed_by_nothing() {
    let xbox = six_packages();

    assert!(
        xbox.app_for(r"D:\Steam\steamapps\common\cs2\game\cs2.exe")
            .is_none(),
        "a game installed by somebody else is not Xbox's to claim"
    );
}

#[test]
fn the_install_directory_itself_is_not_a_program_inside_it() {
    let xbox = six_packages();

    assert!(
        xbox.app_for(r"B:\WindowsApps\Microsoft.Limitless_1.8.14.0_x64__8wekyb3d8bbwe")
            .is_none(),
        "a directory is not a program in that directory"
    );
}

#[test]
fn two_packages_claiming_one_directory_are_refused_rather_than_guessed_between() {
    // Nothing in the repository could break this tie, so it is not broken.
    let xbox = Xbox::from_packages([
        (
            "A.Game_1.0.0.0_x64__aaaaaaaaaaaaa",
            r"\\?\B:\WindowsApps\shared\",
        ),
        (
            "B.Game_1.0.0.0_x64__bbbbbbbbbbbbb",
            r"\\?\B:\WindowsApps\shared\",
        ),
    ]);

    assert!(
        xbox.app_for(r"B:\WindowsApps\shared\game.exe").is_none(),
        "two packages claiming one directory cannot be told apart"
    );
}

#[test]
fn an_entry_that_names_no_directory_is_reported_and_costs_no_other_game() {
    let xbox = Xbox::from_packages([
        ("Gone.Game_1.0.0.0_x64__ccccccccccccc", ""),
        (
            "Microsoft.Limitless_1.8.14.0_x64__8wekyb3d8bbwe",
            r"\\?\B:\WindowsApps\Microsoft.Limitless_1.8.14.0_x64__8wekyb3d8bbwe\",
        ),
    ]);

    assert_eq!(xbox.apps().len(), 1, "the good game survives");
    let problems = xbox.problems();
    assert_eq!(problems.len(), 1, "and the bad entry is reported");
    assert!(
        problems[0].to_string().contains("Gone.Game"),
        "a problem has to name what it is about: {}",
        problems[0]
    );
}

#[test]
fn a_name_worth_showing_somebody_is_taken_from_the_family_name() {
    let xbox = six_packages();
    let app = xbox
        .app_for(
            r"B:\WindowsApps\BethesdaSoftworks.ProjectAltar_1.0.12.0_x64__3275kfvn8vcwc\game.exe",
        )
        .expect("a claim");

    assert_eq!(
        app.name(),
        "ProjectAltar",
        "the publisher prefix and the hash are not a name"
    );
}

#[test]
fn a_process_inside_an_xbox_package_reaches_the_catalogue_as_one() {
    let xbox = six_packages();

    let candidate = xbox.candidate_for(
        "Limitless.exe",
        r"B:\WindowsApps\Microsoft.Limitless_1.8.14.0_x64__8wekyb3d8bbwe\Limitless.exe",
    );

    assert_eq!(
        candidate.launcher(),
        Some((LauncherKind::Xbox, "Microsoft.Limitless_8wekyb3d8bbwe")),
        "the family name is the identity, because the full name changes on every update"
    );
    assert!(
        MatchStrength::LauncherIdentity > MatchStrength::ExecutableName,
        "the launcher rung has to outrank the name rung for any of this to matter"
    );
}

#[test]
fn a_process_xbox_does_not_know_about_carries_no_launcher_identity() {
    let xbox = six_packages();
    let candidate = xbox.candidate_for("cs2.exe", r"D:\Steam\steamapps\common\cs2\game\cs2.exe");

    assert_eq!(candidate.launcher(), None);
}

#[test]
fn a_machine_without_any_xbox_games_is_not_a_failure() {
    // `discover` runs everywhere, so this cannot assert which answer it gets —
    // what it asserts is that neither is an error, and that anything returned
    // carries the two things a candidate is built from.
    let found = Xbox::discover().expect("an absent launcher is not a failure");
    if let Some(xbox) = found {
        for app in xbox.apps() {
            assert!(
                !app.family_name().trim().is_empty(),
                "an app with no identifier"
            );
            assert!(
                !app.installation_directory().as_os_str().is_empty(),
                "an app with no install directory reached `apps`"
            );
            assert!(
                !app.installation_directory()
                    .to_string_lossy()
                    .starts_with(r"\\?\"),
                "the extended-length prefix has to be stripped before anything compares it"
            );
        }
    }
}

/// A package the Xbox app put on a drive other than the system one, spelled as
/// the repository spells it.
///
/// The full name is one a real machine had — including the single underscore
/// before the publisher that breaks the obvious family-name parse.
fn on_a_second_drive() -> Xbox {
    Xbox::from_packages([(
        "38985CA0.COREBase_1.0.203.0_x64_ww_5bkah9njm3e9g",
        r"\\?\B:\WindowsApps\38985CA0.COREBase_1.0.203.0_x64_ww_5bkah9njm3e9g\",
    )])
}

#[test]
fn a_package_on_another_drive_is_claimed_at_the_path_a_process_reports() {
    // Issue #616. `Root` records where the files went; Windows leaves a reparse
    // point where the package would have been on the system drive, and **every
    // packaged process reports the system-drive spelling** — 14 of 14 on the
    // machine that found this. Comparing paths as text, the registered spelling
    // alone claims nothing a process ever says, so the launcher rung never
    // fired for a game on a second drive.
    //
    // That rung matters more for Xbox than anywhere else: every packaged game
    // declares `gamelaunchhelper.exe` as the program the Store starts, so on
    // the executable name alone several games are one process.
    let xbox = on_a_second_drive();
    let reported = concat!(
        r"C:\Program Files\WindowsApps",
        r"\38985CA0.COREBase_1.0.203.0_x64_ww_5bkah9njm3e9g\gamelaunchhelper.exe"
    );

    let candidate = xbox.candidate_for("gamelaunchhelper.exe", reported);

    assert_eq!(
        candidate.launcher(),
        Some((LauncherKind::Xbox, "38985CA0.COREBase_5bkah9njm3e9g")),
        "a process reports the system-drive spelling, whichever drive the files are on"
    );
}

#[test]
fn the_registered_spelling_still_claims_it() {
    // The other half. A package on the system drive is registered and reported
    // at the same path, and a fix that only added the mirrored spelling would
    // break that without this.
    let xbox = on_a_second_drive();
    let registered = concat!(
        r"B:\WindowsApps",
        r"\38985CA0.COREBase_1.0.203.0_x64_ww_5bkah9njm3e9g\gamelaunchhelper.exe"
    );

    let candidate = xbox.candidate_for("gamelaunchhelper.exe", registered);

    assert_eq!(
        candidate.launcher(),
        Some((LauncherKind::Xbox, "38985CA0.COREBase_5bkah9njm3e9g")),
        "the path the repository records still claims the package"
    );
}

#[test]
fn two_spellings_of_one_package_are_one_claimant_and_not_a_tie() {
    // The failure mode of offering two directories per package: `app_for`
    // refuses when more than one *package* claims a path, and two spellings of
    // the same package must not look like two packages — that would turn a game
    // that used to be identified into one that is refused as ambiguous.
    let xbox = on_a_second_drive();
    let reported = concat!(
        r"C:\Program Files\WindowsApps",
        r"\38985CA0.COREBase_1.0.203.0_x64_ww_5bkah9njm3e9g\gamelaunchhelper.exe"
    );

    assert!(
        xbox.app_for(reported).is_some(),
        "one package under two names is one claimant"
    );
}

#[test]
fn a_package_that_is_not_installed_claims_nothing_under_either_spelling() {
    let xbox = on_a_second_drive();

    for path in [
        concat!(
            r"C:\Program Files\WindowsApps",
            r"\Nothing.AtAll_1.0.0.0_x64__aaaaaaaaaaaaa\x.exe"
        ),
        r"B:\WindowsApps\Nothing.AtAll_1.0.0.0_x64__aaaaaaaaaaaaa\x.exe",
    ] {
        assert_eq!(
            xbox.candidate_for("x.exe", path).launcher(),
            None,
            "nothing installed this, under either spelling: {path}"
        );
    }
}
