//! Reading Battle.net's uninstall entries.
//!
//! Every case builds its entries with [`BattleNet::from_entries`] rather than
//! writing to the registry, so nothing here needs Battle.net installed — the
//! third acceptance criterion of
//! [issue #44](https://github.com/wildware-uk/clipped/issues/44).
//!
//! The command lines are the ones a real machine had, including Battle.net's
//! own, which is written by the same uninstaller and is indistinguishable from a
//! game except by its identifier.

use super::*;
use crate::catalogue::MatchStrength;

/// Overwatch's uninstall command, as the registry really holds it.
const OVERWATCH: &str = concat!(
    r#""C:\ProgramData\Battle.net\Agent\Blizzard Uninstaller.exe" "#,
    r#"--lang=enUS --uid=prometheus --displayname="Overwatch""#
);

/// Battle.net's own, which is not a game.
const CLIENT: &str = concat!(
    r#""C:\ProgramData\Battle.net\Agent\Blizzard Uninstaller.exe" "#,
    r#"--lang=enGB --uid=battle.net --displayname="Battle.net""#
);

/// What the real machine had: one game and the client.
fn one_game() -> BattleNet {
    BattleNet::from_entries([
        (OVERWATCH, r"B:\BattleNet\Overwatch", "Overwatch"),
        (CLIENT, r"C:\Program Files (x86)\Battle.net", "Battle.net"),
    ])
}

#[test]
fn the_product_identifier_comes_out_of_the_uninstall_command() {
    assert_eq!(product_uid(OVERWATCH), Some("prometheus"));
    assert_eq!(product_uid(CLIENT), Some("battle.net"));
}

#[test]
fn a_command_with_no_identifier_yields_nothing() {
    assert_eq!(product_uid(""), None);
    assert_eq!(product_uid(r#""C:\thing.exe" --lang=enUS"#), None);
    assert_eq!(product_uid("--uid="), None);
}

#[test]
fn the_identifier_stops_at_the_next_flag() {
    // `--uid=prometheus --displayname="Overwatch"` must not swallow the rest of
    // the line, which is what a `split_once(' ')` on the wrong side would do.
    assert_eq!(
        product_uid(r#"x --uid=prometheus --displayname="Overwatch""#),
        Some("prometheus")
    );
}

#[test]
fn a_games_own_executable_is_claimed_by_its_product_identifier() {
    let battle_net = one_game();

    let app = battle_net
        .app_for(r"B:\BattleNet\Overwatch\_retail_\Overwatch.exe")
        .expect("the game's own executable is in its install directory");

    assert_eq!(app.uid(), "prometheus");
    assert_eq!(app.name(), "Overwatch");
}

#[test]
fn the_launcher_is_not_a_game_and_does_not_claim_its_own_directory() {
    // Battle.net's uninstall entry is written by the same uninstaller and looks
    // identical down to the flags. Left in, every process under the client's
    // directory would be reported as a game called Battle.net.
    let battle_net = one_game();

    assert_eq!(battle_net.apps().len(), 1, "the client is not a game");
    assert!(
        battle_net
            .app_for(r"C:\Program Files (x86)\Battle.net\Battle.net.exe")
            .is_none(),
        "the launcher's own process is not a game"
    );
}

#[test]
fn an_entry_from_another_installer_is_ignored() {
    // The uninstall key holds hundreds of entries that are nothing to do with
    // Battle.net, and the marker is the uninstaller's own name.
    let battle_net = BattleNet::from_entries([(
        r#""C:\Program Files\Something\uninstall.exe" --uid=notablizzardgame"#,
        r"C:\Program Files\Something",
        "Something Else",
    )]);

    assert!(battle_net.apps().is_empty());
    assert!(
        battle_net.problems().is_empty(),
        "somebody else's installer is not a Battle.net problem"
    );
}

#[test]
fn something_the_game_started_deeper_in_its_own_directory_is_still_that_game() {
    let battle_net = one_game();

    let app = battle_net
        .app_for(r"B:\BattleNet\Overwatch\_retail_\bin\anticheat.exe")
        .expect("a program below the install directory belongs to it");

    assert_eq!(app.uid(), "prometheus");
}

#[test]
fn a_process_outside_every_install_directory_is_claimed_by_nothing() {
    let battle_net = one_game();

    assert!(
        battle_net
            .app_for(r"D:\Steam\steamapps\common\cs2\game\cs2.exe")
            .is_none(),
        "a game installed by somebody else is not Battle.net's to claim"
    );
}

#[test]
fn the_install_directory_itself_is_not_a_program_inside_it() {
    let battle_net = one_game();

    assert!(
        battle_net.app_for(r"B:\BattleNet\Overwatch").is_none(),
        "a directory is not a program in that directory"
    );
}

#[test]
fn two_products_claiming_one_directory_are_refused_rather_than_guessed_between() {
    let battle_net = BattleNet::from_entries([
        (
            r#""…\Blizzard Uninstaller.exe" --uid=one"#,
            r"B:\BattleNet\Shared",
            "One",
        ),
        (
            r#""…\Blizzard Uninstaller.exe" --uid=two"#,
            r"B:\BattleNet\Shared",
            "Two",
        ),
    ]);

    assert!(
        battle_net
            .app_for(r"B:\BattleNet\Shared\game.exe")
            .is_none(),
        "two products claiming one directory cannot be told apart"
    );
}

#[test]
fn an_entry_that_names_no_directory_is_reported_and_costs_no_other_game() {
    let battle_net = BattleNet::from_entries([
        (r#""…\Blizzard Uninstaller.exe" --uid=gone"#, "", "Gone"),
        (OVERWATCH, r"B:\BattleNet\Overwatch", "Overwatch"),
    ]);

    assert_eq!(battle_net.apps().len(), 1, "the good game survives");
    let problems = battle_net.problems();
    assert_eq!(problems.len(), 1, "and the bad entry is reported");
    assert!(
        problems[0].to_string().contains("gone"),
        "a problem has to name what it is about: {}",
        problems[0]
    );
}

#[test]
fn a_game_with_no_display_name_is_still_detected_and_named_after_its_identifier() {
    let battle_net = BattleNet::from_entries([(OVERWATCH, r"B:\BattleNet\Overwatch", "   ")]);

    let app = battle_net
        .app_for(r"B:\BattleNet\Overwatch\_retail_\Overwatch.exe")
        .expect("a nameless game is still an installed game");

    assert_eq!(app.uid(), "prometheus");
    assert_eq!(app.name(), "prometheus");
}

#[test]
fn a_process_inside_a_battle_net_game_reaches_the_catalogue_as_one() {
    let battle_net = one_game();

    let candidate = battle_net.candidate_for(
        "Overwatch.exe",
        r"B:\BattleNet\Overwatch\_retail_\Overwatch.exe",
    );

    assert_eq!(
        candidate.launcher(),
        Some((LauncherKind::BattleNet, "prometheus")),
        "the product identifier is what the catalogue matches on"
    );
    assert!(
        MatchStrength::LauncherIdentity > MatchStrength::ExecutableName,
        "the launcher rung has to outrank the name rung for any of this to matter"
    );
}

#[test]
fn a_process_battle_net_does_not_know_about_carries_no_launcher_identity() {
    let battle_net = one_game();
    let candidate =
        battle_net.candidate_for("cs2.exe", r"D:\Steam\steamapps\common\cs2\game\cs2.exe");

    assert_eq!(candidate.launcher(), None);
}

#[test]
fn a_machine_without_battle_net_is_not_a_failure() {
    // `discover` runs everywhere, so this cannot assert which answer it gets —
    // only that neither is an error, and that anything returned carries what a
    // candidate is built from.
    let found = BattleNet::discover().expect("an absent launcher is not a failure");
    if let Some(battle_net) = found {
        for app in battle_net.apps() {
            assert!(!app.uid().trim().is_empty(), "an app with no identifier");
            assert_ne!(app.uid(), CLIENT_UID, "the launcher is not a game");
            assert!(
                !app.installation_directory().as_os_str().is_empty(),
                "an app with no install directory reached `apps`"
            );
        }
    }
}
