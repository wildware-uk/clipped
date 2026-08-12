//! What the plugin makes of the payloads in `tests/fixtures`.
//!
//! League of Legends is not installed on the machine this was written on, and
//! `tests/fixtures/README.md` is explicit about what that means: the payloads
//! are constructed from the published shape of the Live Client Data API rather
//! than captured from a match, so these tests prove the derivation and not the
//! shape. Making them run against a real capture is one file and no code
//! change, which is the point of keeping the derivation a pure function of a
//! payload.

use std::time::Duration;

use clipped_league_plugin::{GameSnapshot, LeagueWatch, PollResult};
use clipped_plugins::{PluginReport, ReportedEvent};

/// One of the committed payloads.
fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!("{} should be readable: {error}", path.display());
    })
}

/// A poll that answered with `name`, taking four milliseconds.
fn answered(body: &str) -> PollResult<'_> {
    PollResult::Answered {
        body,
        round_trip: Duration::from_millis(4),
    }
}

/// How long a watch that saw this match begin has been running by the time the
/// match clock reads `game_time`.
///
/// Not zero. This plugin is attached to `League of Legends.exe`, which starts
/// before the match clock does — a loading screen is a minute of it — so an
/// attachment that saw its match begin is always older than the match it is
/// watching. A watch told otherwise is a watch being asked about a match that
/// was already under way when it arrived, which is a different question, and
/// `a_restarted_plugin_reports_nothing_the_attachment_before_it_reported`
/// (`src/watch.rs`) is where it is asked.
fn watching_since_before(game_time: f64) -> Duration {
    Duration::from_secs_f64(game_time + 90.0)
}

fn events(reports: &[PluginReport]) -> Vec<&ReportedEvent> {
    reports
        .iter()
        .filter_map(|report| match report {
            PluginReport::Event(event) => Some(event),
            _ => None,
        })
        .collect()
}

fn kinds(reports: &[PluginReport]) -> Vec<String> {
    events(reports)
        .iter()
        .map(|event| event.kind.as_str().to_owned())
        .collect()
}

fn problems(reports: &[PluginReport]) -> Vec<String> {
    reports
        .iter()
        .filter_map(|report| match report {
            PluginReport::Problem { message } => Some(message.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn three_polls_of_one_match_report_each_event_once_and_in_order() {
    // The three payloads are the same match at three moments, so reading them
    // in order is a match being played. Nothing here resets between them: this
    // is one `LeagueWatch`, as one attachment would be.
    let mut watch = LeagueWatch::new();

    let attached = watching_since_before(4.6);

    let opening = fixture("match_started.json");
    assert_eq!(
        kinds(&watch.observe(answered(&opening), attached)),
        vec!["match_started"]
    );

    let middle = fixture("kills_deaths_assists.json");
    assert_eq!(
        kinds(&watch.observe(answered(&middle), attached + Duration::from_secs(908))),
        vec!["kill", "death", "assist"],
        "the player's own kill, death and assist, and none of the four events they are not in"
    );

    let finished = fixture("ended_in_a_win.json");
    assert_eq!(
        kinds(&watch.observe(answered(&finished), attached + Duration::from_secs(1_798))),
        vec!["kill", "match_ended", "win"],
        "the last kill, and how it ended"
    );

    assert!(
        watch
            .observe(answered(&finished), attached + Duration::from_secs(1_799))
            .is_empty(),
        "a match that has ended and is polled again is not a second match"
    );
}

#[test]
fn a_kill_is_placed_by_the_match_clock_and_carries_the_games_own_words() {
    let mut watch = LeagueWatch::new();
    let reports = watch.observe(
        PollResult::Answered {
            body: &fixture("kills_deaths_assists.json"),
            round_trip: Duration::from_millis(12),
        },
        watching_since_before(912.4),
    );
    let kill = events(&reports)[1];

    assert_eq!(kill.kind.as_str(), "kill");
    assert_eq!(
        Duration::from_nanos(kill.ago_ns),
        Duration::from_secs_f64(912.4 - 213.4),
        "the kill is where the match clock in the same payload puts it, not where the poll was"
    );
    assert_eq!(kill.data["VictimName"], "Kestrel#EUW");
    assert_eq!(kill.data["Assisters"][0], "Bramble#EU1");
    assert_eq!(
        kill.data["EventTime"], 213.4,
        "the match-relative time is the one thing the envelope cannot carry, so it stays"
    );
    assert!(
        !kill.data.contains_key("EventName"),
        "the kind is what `EventName` became; keeping it invites somebody to switch on it"
    );
}

#[test]
fn a_match_with_no_kills_in_it_still_reports_how_it_ended() {
    let mut watch = LeagueWatch::new();
    let reports = watch.observe(
        answered(&fixture("ended_in_a_loss.json")),
        watching_since_before(1205.9),
    );
    assert_eq!(
        kinds(&reports),
        vec!["match_started", "match_ended", "loss"]
    );
}

#[test]
fn a_client_that_reports_summoner_names_is_still_the_player() {
    // The same match seen through a client that gives no Riot ID. Nothing in
    // the derivation changes; the identity is matched by the names it was
    // given.
    let mut watch = LeagueWatch::new();
    let reports = watch.observe(
        answered(&fixture("summoner_names_only.json")),
        watching_since_before(401.0),
    );
    assert_eq!(kinds(&reports), vec!["match_started", "kill", "death"]);
    assert!(problems(&reports).is_empty(), "{reports:?}");
}

#[test]
fn spectating_reports_the_match_and_says_why_it_cannot_report_a_kill() {
    // Without an active player there is no way to tell a kill from a death, and
    // a plugin that quietly reported neither would look exactly like one that
    // was working (AGENTS.md section 45).
    let mut watch = LeagueWatch::new();
    let reports = watch.observe(
        answered(&fixture("no_active_player.json")),
        watching_since_before(220.1),
    );

    assert_eq!(kinds(&reports), vec!["match_started"]);
    assert_eq!(problems(&reports).len(), 1, "{reports:?}");
    assert!(problems(&reports)[0].contains("who is playing"));
}

#[test]
fn a_payload_from_a_later_patch_is_read_for_what_it_still_says() {
    // The forward-compatibility claim, on one payload: a section this build has
    // never heard of, fields inside sections it has, an event name that did not
    // exist when this was written, and one entry that cannot be read at all.
    // What survives is the kill.
    let payload = fixture("later_patch.json");

    let snapshot = GameSnapshot::parse(&payload).expect("a later patch is still a snapshot");
    assert_eq!(
        snapshot.unreadable_entries(),
        1,
        "the entry with no identifier is counted rather than dropped in silence"
    );
    assert_eq!(snapshot.events().len(), 3);

    let mut watch = LeagueWatch::new();
    let reports = watch.observe(answered(&payload), watching_since_before(410.9));
    assert_eq!(
        kinds(&reports),
        vec!["match_started", "kill"],
        "an event name this build does not know is not a mark it can place, and not a failure"
    );
    assert_eq!(
        events(&reports)[1].data["KillType"],
        "SOMETHING_NEW",
        "a field added by a patch still reaches the recording"
    );

    let said = problems(&reports);
    assert_eq!(said.len(), 1, "{reports:?}");
    assert!(
        said[0].contains("could not be read"),
        "the skipped entry is said out loud rather than only counted: {said:?}"
    );
}

#[test]
fn the_endpoints_own_no_game_answer_is_not_a_match() {
    // Reading it as an empty match would put a `match_started` on a timeline
    // for a game nobody played.
    let body = fixture("not_in_a_game.json");
    assert!(GameSnapshot::parse(&body).is_err());

    let mut watch = LeagueWatch::new();
    assert!(watch.observe(answered(&body), Duration::ZERO).is_empty());
}

#[test]
fn every_committed_payload_is_the_shape_its_test_says_it_is() {
    // A fixture that quietly stopped being valid JSON, or that was edited into
    // a shape nothing reads, would leave the tests above passing on a file
    // nobody had noticed was wrong.
    for (name, events) in [
        ("match_started.json", 1),
        ("kills_deaths_assists.json", 9),
        ("ended_in_a_win.json", 11),
        ("ended_in_a_loss.json", 3),
        ("no_active_player.json", 2),
        ("summoner_names_only.json", 3),
        ("later_patch.json", 3),
    ] {
        let snapshot = GameSnapshot::parse(&fixture(name))
            .unwrap_or_else(|error| panic!("{name} should be a snapshot: {error}"));
        assert_eq!(snapshot.events().len(), events, "in {name}");
        assert!(snapshot.game_time() > 0.0, "in {name}");
    }
}
