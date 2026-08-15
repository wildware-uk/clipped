//! The Riot provider and the catalogue, working together.
//!
//! `launcher/riot/tests.rs` checks what the provider reads and what it claims.
//! This checks the thing the provider exists for — that the identity it
//! produces is what the catalogue matches on — and the one place where that is
//! deliberately *not* wanted.
//!
//! Both cases build the products they are about with [`Riot::from_products`],
//! so nothing here needs the Riot client installed
//! ([issue #44](https://github.com/wildware-uk/clipped/issues/44), third
//! acceptance criterion).

use std::path::PathBuf;

use clipped_game_detection::catalogue::{
    Catalogue, EntrySource, Match, MatchStrength, ProcessCandidate,
};
use clipped_game_detection::launcher::riot::Riot;

/// Where League really installs, spelled the way Riot's settings spell it.
const LEAGUE: &str = "C:/Riot Games/League of Legends";

/// The provider, holding the one product a real machine had.
fn riot() -> Riot {
    Riot::from_products([(
        "league_of_legends".to_owned(),
        "live".to_owned(),
        PathBuf::from(LEAGUE),
    )])
}

#[test]
fn riot_names_the_game_behind_an_executable_the_catalogue_cannot_place() {
    // The whole point of the launcher rung: a process the catalogue has nothing
    // to say about, in a directory Riot knows, is still the game.
    //
    // The catalogue is written here rather than taken from the seed because the
    // seed deliberately records no `app_id` for League — see the test below for
    // why — and this has to check the provider against a catalogue that uses
    // what it produces.
    let catalogue = Catalogue::parse(
        r#"
schema_version = 1

[[game]]
game_id = "league-of-legends"
name = "League of Legends"

[[game.executables]]
name = "League of Legends.exe"

[game.launcher]
kind = "riot"
app_id = "league_of_legends"
"#,
        EntrySource::Seed,
    )
    .expect("the catalogue parses");

    let path = format!(r"{LEAGUE}\Game\some-anticheat.exe").replace('/', r"\");

    let unaided =
        catalogue.match_process(&ProcessCandidate::new("some-anticheat.exe").with_path(&path));
    assert_eq!(
        unaided,
        Match::None,
        "the catalogue alone should have nothing to say about this process"
    );

    let outcome = catalogue.match_process(&riot().candidate_for("some-anticheat.exe", &path));
    let Match::One { entry, strength } = outcome else {
        panic!("expected Riot's identity to place the process, got {outcome:?}");
    };
    assert_eq!(entry.game_id().as_str(), "league-of-legends");
    assert_eq!(strength, MatchStrength::LauncherIdentity);
}

#[test]
fn the_league_client_is_not_a_league_session_even_with_riots_identity_attached() {
    // `LeagueClient.exe`, `LeagueClientUx.exe` and `LeagueClientUxRender.exe`
    // all live in League's install directory, and all of them run for as long
    // as somebody has the shop open — including while they are playing
    // something else entirely. The seed entry says so in a comment and lists
    // only `League of Legends.exe`.
    //
    // The launcher rung matches an *entry*, not an executable: an `app_id` on
    // that entry would make every one of those processes a League session and
    // undo the choice the comment records. That is why the shipped catalogue
    // has no `app_id` for League, and this is the property that must survive
    // somebody adding one — the test above shows what adding one is worth, so
    // the two together say "yes, and not like this".
    let catalogue = Catalogue::seed().expect("the shipped seed data is valid");
    let path = format!(r"{LEAGUE}\LeagueClient.exe").replace('/', r"\");

    let outcome = catalogue.match_process(&riot().candidate_for("LeagueClient.exe", &path));

    assert_eq!(
        outcome,
        Match::None,
        "the client is not a game, however confidently Riot claims the directory"
    );
}

#[test]
fn a_process_riot_does_not_know_reaches_the_other_rungs_unchanged() {
    // The half that keeps a provider from making detection worse: a game
    // installed without Riot has to match exactly as well as it did before
    // there was a Riot provider.
    let catalogue = Catalogue::seed().expect("the shipped seed data is valid");
    let path = r"D:\Steam\steamapps\common\Portal 2\portal2.exe";

    let outcome = catalogue.match_process(&riot().candidate_for("portal2.exe", path));

    assert_eq!(
        outcome.entry().map(|entry| entry.game_id().as_str()),
        Some("portal-2"),
        "an unclaimed process still carries its name and path"
    );
}
