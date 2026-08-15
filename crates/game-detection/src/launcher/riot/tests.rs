//! Reading Riot's product metadata, against real files on a real disk.
//!
//! Every case writes the product directories it is about into a directory of
//! its own and points [`Riot::read_at`] at it. Nothing here needs the Riot
//! client installed, which is the third acceptance criterion of
//! [issue #44](https://github.com/wildware-uk/clipped/issues/44): a provider
//! whose behaviour could only be checked on a machine that has the launcher is
//! one nobody can change safely (AGENTS.md section 25).
//!
//! The settings written here are cut down from the real files — the ones that
//! matter are spelled exactly as Riot spells them, including the drive-letter
//! colon inside a quoted value and the trailing slash on the root that the full
//! path does not have.

use std::fs;

use super::*;
use crate::catalogue::MatchStrength;

/// An empty directory of this test's own, removed first if it survived.
fn scratch(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "clipped-riot-{}-{name}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).expect("a scratch directory can be made");
    directory
}

/// A product directory holding the settings Riot writes for something that is
/// installed somewhere of its own.
fn installed(metadata: &Path, product: &str, install: &str) {
    let directory = metadata.join(product);
    fs::create_dir_all(&directory).expect("a product directory can be made");
    let text = format!(
        "auto_patching_enabled_by_player: false\n\
         dependencies: {{}}\n\
         patching_policy: \"manual\"\n\
         product_dependency: \"teamfighttactics.live\"\n\
         product_install_full_path: \"{install}\"\n\
         product_install_root: \"C:/Riot Games\"\n\
         settings:\n    locale: \"en_GB\"\n\
         shortcut_name: \"Something.lnk\"\n\
         should_repair: false\n"
    );
    fs::write(
        directory.join(format!("{product}.product_settings.yaml")),
        text,
    )
    .expect("settings can be written");
}

/// A product directory the way Riot leaves one for a product that is not
/// separately installed: settings, and no full path in them.
///
/// This is Teamfight Tactics on the machine this was written against.
fn installed_inside_another(metadata: &Path, product: &str) {
    let directory = metadata.join(product);
    fs::create_dir_all(&directory).expect("a product directory can be made");
    let text = "auto_patching_enabled_by_player: false\n\
                patching_policy: \"manual\"\n\
                product_dependency: \"league_of_legends.live\"\n\
                product_install_root: \"C:/Riot Games/\"\n\
                should_repair: false\n";
    fs::write(
        directory.join(format!("{product}.product_settings.yaml")),
        text,
    )
    .expect("settings can be written");
}

/// A product directory with no settings at all: a game the client offers rather
/// than a game that is there.
fn offered_but_not_installed(metadata: &Path, product: &str) {
    let directory = metadata.join(product);
    fs::create_dir_all(&directory).expect("a product directory can be made");
    fs::write(directory.join(format!("{product}.lockfile")), "")
        .expect("a lockfile can be written");
    fs::write(
        directory.join(format!("{product}.preview.manifest")),
        "not read",
    )
    .expect("a manifest can be written");
}

/// The three products the real machine had settings for, written the way it
/// had them.
fn a_real_machine(name: &str) -> (PathBuf, Riot) {
    let metadata = scratch(name);
    installed(
        &metadata,
        "league_of_legends.live",
        "C:/Riot Games/League of Legends",
    );
    installed_inside_another(&metadata, "teamfighttactics.live");
    installed_inside_another(&metadata, "teamfighttactics.pbe");
    offered_but_not_installed(&metadata, "valorant.live");
    offered_but_not_installed(&metadata, "bacon.live");
    fs::create_dir_all(metadata.join("Riot Client")).expect("the client directory can be made");
    fs::write(
        metadata
            .join("Riot Client")
            .join("Riot Client.settings.yaml"),
        "locale: \"en_GB\"\n",
    )
    .expect("the client's own settings can be written");

    let riot = Riot::read_at(&metadata).expect("the directory is there");
    (metadata, riot)
}

#[test]
fn a_product_is_read_out_of_its_settings() {
    let (_metadata, riot) = a_real_machine("installed");

    assert!(riot.problems().is_empty(), "{:?}", riot.problems());
    assert_eq!(riot.apps().len(), 1, "{:?}", riot.apps());

    let app = &riot.apps()[0];
    assert_eq!(app.id(), "league_of_legends");
    assert_eq!(app.patchline(), "live");
    assert_eq!(
        app.installation_directory(),
        Path::new("C:/Riot Games/League of Legends")
    );
}

#[test]
fn a_product_directory_is_not_an_installation() {
    // The most misleading thing about this metadata: `valorant.live` is a
    // directory on a machine with no Valorant on it. A provider that read the
    // directory listing and stopped would claim two games that are not there,
    // and every one of those claims would be a wrong answer about what somebody
    // is playing rather than a missing one.
    let (_metadata, riot) = a_real_machine("offered");

    let ids: Vec<&str> = riot.apps().iter().map(RiotApp::id).collect();
    assert_eq!(
        ids,
        vec!["league_of_legends"],
        "only the product with settings saying where it is counts as installed"
    );
}

#[test]
fn a_product_installed_inside_another_is_skipped_without_being_called_a_fault() {
    // Teamfight Tactics has settings and no `product_install_full_path`,
    // because it is played from League's client in League's directory. Both its
    // patchlines are like that, so calling it a fault would put two warnings on
    // every machine with League on it — and a diagnostics screen that cries
    // wolf on a healthy machine is one people stop reading.
    let (_metadata, riot) = a_real_machine("inside");

    assert!(
        riot.problems().is_empty(),
        "a normal installation reported problems: {:?}",
        riot.problems()
    );
    assert!(
        !riot.apps().iter().any(|app| app.id() == "teamfighttactics"),
        "a product with no directory of its own cannot claim a process"
    );
}

#[test]
fn the_install_root_is_not_used_when_the_full_path_is_missing() {
    // The tempting fallback, and the reason it is not taken: the root holds
    // every Riot game, so a product claiming it would claim League's process
    // and answer `teamfighttactics`. A wrong game is worse than no game.
    let metadata = scratch("root-fallback");
    installed_inside_another(&metadata, "teamfighttactics.live");

    let riot = Riot::read_at(&metadata).expect("the directory is there");

    assert!(
        riot.app_for(r"C:\Riot Games\League of Legends\LeagueClient.exe")
            .is_none(),
        "the install root must not stand in for a directory of the product's own"
    );
}

#[test]
fn a_directory_that_is_not_a_product_is_passed_over() {
    // `Riot Client` has settings of its own, and they are not a product's. The
    // rule that skips it is the shape of the directory name, which is worth
    // asserting rather than the name itself: naming it would leave the next
    // non-product directory Riot adds claiming something.
    let metadata = scratch("not-a-product");
    fs::create_dir_all(metadata.join("Riot Client")).expect("it can be made");
    fs::write(
        metadata
            .join("Riot Client")
            .join("Riot Client.settings.yaml"),
        "product_install_full_path: \"C:/Riot Games/Riot Client\"\n",
    )
    .expect("it can be written");

    let riot = Riot::read_at(&metadata).expect("the directory is there");

    assert!(riot.apps().is_empty(), "{:?}", riot.apps());
    assert!(riot.problems().is_empty(), "{:?}", riot.problems());
}

#[test]
fn two_patchlines_of_one_product_are_two_installations_with_one_identity() {
    // `live` and `pbe` are the same game to anybody watching a recording, so a
    // catalogue entry naming the product matches a player on either. They are
    // in different directories, so nothing is ambiguous about which is running.
    let metadata = scratch("patchlines");
    installed(
        &metadata,
        "league_of_legends.live",
        "C:/Riot Games/League of Legends",
    );
    installed(
        &metadata,
        "league_of_legends.pbe",
        "C:/Riot Games/League of Legends (PBE)",
    );

    let riot = Riot::read_at(&metadata).expect("the directory is there");

    assert_eq!(riot.apps().len(), 2);
    assert!(
        riot.apps()
            .iter()
            .all(|app| app.id() == "league_of_legends"),
        "the patchline is not part of the identity: {:?}",
        riot.apps()
    );

    let live = riot
        .app_for(r"C:\Riot Games\League of Legends\LeagueClient.exe")
        .expect("the live directory claims it");
    assert_eq!(live.patchline(), "live");

    let pbe = riot
        .app_for(r"C:\Riot Games\League of Legends (PBE)\LeagueClient.exe")
        .expect("the pbe directory claims it");
    assert_eq!(pbe.patchline(), "pbe");
}

#[test]
fn the_identity_is_the_part_before_the_first_dot_not_the_last() {
    // `league_of_legends.live.game_patch` is a component of League rather than a
    // product called `league_of_legends.live`. Splitting on the last dot would
    // make it one, and the identity it handed the catalogue would match nothing.
    let metadata = scratch("first-dot");
    installed(
        &metadata,
        "league_of_legends.live.game_patch",
        "C:/Riot Games/League of Legends/Patcher",
    );

    let riot = Riot::read_at(&metadata).expect("the directory is there");

    assert_eq!(riot.apps()[0].id(), "league_of_legends");
    assert_eq!(riot.apps()[0].patchline(), "live.game_patch");
}

#[test]
fn a_path_the_process_table_spells_with_backslashes_matches_settings_spelled_with_forward_ones() {
    // Riot writes `C:/Riot Games/...` and a running process reports
    // `C:\Riot Games\...`. Compared as strings this provider would claim nothing
    // at all on a real machine, and every test that wrote both spellings the
    // same way would still pass.
    let (_metadata, riot) = a_real_machine("spelling");

    assert!(
        riot.app_for(r"C:\Riot Games\League of Legends\Game\League of Legends.exe")
            .is_some(),
        "the two spellings name the same directory"
    );
}

#[test]
fn something_deeper_in_the_game_directory_is_still_that_game() {
    // League starts its own game process out of a subdirectory of the client's
    // install directory, and Vanguard sits in another. Neither is a separate
    // game, and neither must stop being League.
    let (_metadata, riot) = a_real_machine("deeper");

    let app = riot
        .app_for(r"C:\Riot Games\League of Legends\Game\League of Legends.exe")
        .expect("a program below the install directory belongs to it");
    assert_eq!(app.id(), "league_of_legends");
}

#[test]
fn a_process_outside_every_product_is_claimed_by_nothing() {
    let (_metadata, riot) = a_real_machine("outside");

    assert!(
        riot.app_for(r"D:\Steam\steamapps\common\cs2\game\bin\win64\cs2.exe")
            .is_none(),
        "a game installed by somebody else is not Riot's to claim"
    );
}

#[test]
fn the_install_directory_itself_is_not_a_program_inside_it() {
    let (_metadata, riot) = a_real_machine("itself");

    assert!(
        riot.app_for(r"C:\Riot Games\League of Legends").is_none(),
        "a directory is not a program in that directory"
    );
}

#[test]
fn a_directory_inside_another_is_claimed_by_the_deeper_one() {
    // Declared shallower-first, which is the order a first-match implementation
    // gets wrong.
    let metadata = scratch("nested");
    installed(&metadata, "aaa.live", "C:/Riot Games");
    installed(
        &metadata,
        "league_of_legends.live",
        "C:/Riot Games/League of Legends",
    );

    let riot = Riot::read_at(&metadata).expect("the directory is there");

    let app = riot
        .app_for(r"C:\Riot Games\League of Legends\LeagueClient.exe")
        .expect("something claims it");
    assert_eq!(
        app.id(),
        "league_of_legends",
        "the deeper install directory is the answer"
    );
}

#[test]
fn two_products_claiming_one_directory_are_refused_rather_than_guessed_between() {
    // Nothing in this metadata breaks the tie — the settings record no
    // executable — so choosing would hand the catalogue an identity for the
    // wrong game (issue #459 is what that costs).
    let metadata = scratch("tie");
    installed(&metadata, "one.live", "C:/Riot Games/League of Legends");
    installed(&metadata, "two.live", "C:/Riot Games/League of Legends");

    let riot = Riot::read_at(&metadata).expect("the directory is there");

    assert!(
        riot.app_for(r"C:\Riot Games\League of Legends\LeagueClient.exe")
            .is_none(),
        "two products claiming one directory cannot be told apart, and saying so is the point"
    );
}

#[test]
fn a_settings_file_that_cannot_be_read_costs_no_other_product() {
    // An update interrupted, a drive removed, a file locked. One of those must
    // not cost the user every other game Riot knows about — and it has to name
    // itself, or a diagnostics screen says only that something was wrong.
    let metadata = scratch("unreadable");
    installed(
        &metadata,
        "league_of_legends.live",
        "C:/Riot Games/League of Legends",
    );
    // A directory where the settings file should be: `read_to_string` fails on
    // it on every platform, without needing permissions this test cannot set.
    let broken = metadata.join("broken.live");
    fs::create_dir_all(broken.join("broken.live.product_settings.yaml")).expect("it can be made");

    let riot = Riot::read_at(&metadata).expect("the directory is there");

    assert_eq!(riot.apps().len(), 1, "the good product survives");
    assert_eq!(riot.apps()[0].id(), "league_of_legends");

    let problems = riot.problems();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].to_string().contains("broken.live"),
        "a problem has to name the file it is about: {}",
        problems[0]
    );
}

#[test]
fn a_metadata_directory_that_is_not_there_says_which_one() {
    let missing = scratch("missing").join("nothing-here");

    let error = Riot::read_at(&missing).expect_err("there is nothing to read");

    assert!(
        error.to_string().contains("nothing-here"),
        "an error nobody can act on: {error}"
    );
}

#[test]
fn a_process_inside_a_riot_installation_reaches_the_catalogue_as_one() {
    let (_metadata, riot) = a_real_machine("candidate");

    let candidate = riot.candidate_for(
        "LeagueClient.exe",
        r"C:\Riot Games\League of Legends\LeagueClient.exe",
    );

    assert_eq!(
        candidate.launcher(),
        Some((LauncherKind::Riot, "league_of_legends")),
        "the identity is what lets the catalogue match a game whose executable it does not know"
    );
}

#[test]
fn a_process_riot_does_not_know_about_carries_no_launcher_identity() {
    // The half that keeps this from making detection worse: an absent identity
    // is what makes the catalogue fall back to its path and name rungs, so a
    // game installed without Riot matches exactly as well as it did before.
    let (_metadata, riot) = a_real_machine("unknown");

    let candidate = riot.candidate_for("cs2.exe", r"D:\Steam\steamapps\common\cs2\game\cs2.exe");

    assert_eq!(candidate.launcher(), None);
}

#[test]
fn the_launcher_identity_is_the_rung_the_catalogue_matches_it_on() {
    // What the whole provider is for: reaching
    // `MatchStrength::LauncherIdentity`, the rung above every other, so that a
    // game whose process is called something generic is still identified.
    let (_metadata, riot) = a_real_machine("rung");
    let candidate = riot.candidate_for(
        "LeagueClient.exe",
        r"C:\Riot Games\League of Legends\LeagueClient.exe",
    );

    let (kind, id) = candidate.launcher().expect("Riot claimed it");
    assert_eq!(kind, LauncherKind::Riot);
    assert_eq!(id, "league_of_legends");
    assert!(
        MatchStrength::LauncherIdentity > MatchStrength::ExecutableName,
        "the launcher rung has to outrank the name rung for any of this to matter"
    );
}

#[test]
fn a_machine_without_the_riot_client_is_not_a_failure() {
    // `discover` on a machine with no Riot metadata answers `Ok(None)`. This
    // runs everywhere, so it cannot assert which of the two it gets — what it
    // does assert is that neither is an error, because reporting "not
    // installed" as a fault would put a warning on most machines.
    let found = Riot::discover().expect("an absent launcher is not a failure");
    if let Some(riot) = found {
        for app in riot.apps() {
            assert!(!app.id().trim().is_empty(), "an app with no identifier");
            assert!(
                !app.installation_directory().as_os_str().is_empty(),
                "an app with no install directory reached `apps`"
            );
        }
    }
}

#[test]
fn from_products_builds_what_reading_the_metadata_would_have() {
    let riot = Riot::from_products([
        (
            "valorant".to_owned(),
            "live".to_owned(),
            PathBuf::from("C:/Riot Games/VALORANT"),
        ),
        (
            "league_of_legends".to_owned(),
            "live".to_owned(),
            PathBuf::from("C:/Riot Games/League of Legends"),
        ),
    ]);

    let ids: Vec<&str> = riot.apps().iter().map(RiotApp::id).collect();
    assert_eq!(
        ids,
        vec!["league_of_legends", "valorant"],
        "the order is the products', not the caller's, so a diagnostics list is stable"
    );
    assert_eq!(
        riot.app_for(r"C:\Riot Games\VALORANT\live\VALORANT-Win64-Shipping.exe")
            .map(RiotApp::id),
        Some("valorant")
    );
}

#[test]
fn the_value_is_read_past_the_colon_in_a_drive_letter() {
    // `product_install_full_path: "C:/Riot Games/League of Legends"` has two
    // colons in it. Splitting on the last one, or on all of them, gets a path
    // starting `/Riot Games` that claims nothing.
    assert_eq!(
        install_path("product_install_full_path: \"C:/Riot Games/League of Legends\"\n").as_deref(),
        Some("C:/Riot Games/League of Legends")
    );
}

#[test]
fn a_key_that_is_not_the_one_asked_for_does_not_answer() {
    // `product_install_root` is on every product, including the ones with no
    // full path. A reader matching a prefix or a substring would take it.
    assert_eq!(
        install_path("product_install_root: \"C:/Riot Games/\"\n"),
        None
    );
    assert_eq!(
        install_path("not_product_install_full_path: \"C:/Elsewhere\"\n"),
        None
    );
}

#[test]
fn a_value_without_quotes_is_taken_and_an_empty_one_is_not() {
    // Riot quotes them. Nothing guarantees it always will, and an empty value
    // is a path that claims the whole filesystem if it is believed.
    assert_eq!(
        install_path("product_install_full_path: C:/Riot Games/VALORANT\n").as_deref(),
        Some("C:/Riot Games/VALORANT")
    );
    assert_eq!(install_path("product_install_full_path: \"\"\n"), None);
    assert_eq!(install_path("product_install_full_path:\n"), None);
}
