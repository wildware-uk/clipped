//! A match, payload by payload, through everything the plugin does to one.
//!
//! The unit tests beside each module check one rule at a time against a payload
//! written for it. This checks the whole of a match against the committed
//! samples in `fixtures/`, in the order Dota would post them, and asserts the
//! **exact** event stream that comes out — because the failure this catches is
//! not a rule being wrong, it is two right rules producing an event twice or
//! not at all when they meet.
//!
//! `fixtures/README.md` says where those payloads came from and what they can
//! and cannot be evidence of. In short: they are constructed from the
//! documented shape, not captured from a running game, and no test in this
//! repository can tell you that Dota 2 posts what they say it posts.

use core::time::Duration;
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use serde_json::{json, Value};

use clipped_dota2_plugin::dota::{Notice, Watcher};
use clipped_dota2_plugin::gsi::Cadence;
use clipped_dota2_plugin::PLUGIN_ID;
use clipped_events::{EventSource, EventTime};

/// The payloads of one match, in the order they would arrive.
const MATCH: [&str; 11] = [
    "01-menu.json",
    "02-hero-selection.json",
    "03-strategy-time.json",
    "04-match-in-progress.json",
    "05-first-kill.json",
    "06-death.json",
    "07-double-kill.json",
    "08-assist-and-killing-spree.json",
    "09-radiant-wins.json",
    "10-post-game.json",
    "11-next-match.json",
];

/// A game being watched rather than played, from the middle of it to the
/// scoreboard. Two payloads because the interesting thing about a spectated
/// game is that its `map` block moves through the same states a played match
/// does.
const SPECTATED: [&str; 2] = ["spectating.json", "spectating-post-game.json"];

/// The payloads that are not part of either sequence.
const OTHERS: [&str; 1] = ["unrecognisable.json"];

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn payload(name: &str) -> Value {
    let path = fixtures().join(name);
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} should be readable: {error}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("{} should be JSON: {error}", path.display()))
}

#[test]
fn a_whole_match_produces_exactly_the_events_the_mapping_documents() {
    let mut watcher = Watcher::new();
    let mut kinds = Vec::new();
    for name in MATCH {
        for report in watcher.observe(&payload(name)).reports {
            kinds.push(format!("{name}: {}", report.kind.as_str()));
        }
    }

    assert_eq!(
        kinds,
        vec![
            "04-match-in-progress.json: match_started",
            "05-first-kill.json: kill",
            "06-death.json: death",
            "07-double-kill.json: kill",
            "07-double-kill.json: kill",
            "08-assist-and-killing-spree.json: kill",
            "08-assist-and-killing-spree.json: assist",
            "08-assist-and-killing-spree.json: dota-2.kill_streak",
            "09-radiant-wins.json: win",
            "10-post-game.json: match_ended",
        ],
        "the menu, the draft and the strategy time produce nothing, the second match starts \
         again from a baseline, and everything between is one event per thing that happened"
    );
}

#[test]
fn every_event_carries_dotas_own_words_about_it() {
    let mut watcher = Watcher::new();
    let mut reports = Vec::new();
    for name in MATCH {
        reports.extend(watcher.observe(&payload(name)).reports);
    }

    let kill = reports
        .iter()
        .find(|report| report.kind.as_str() == "kill")
        .expect("the match has a kill in it");
    assert_eq!(kill.data["kills"], json!(1));
    assert_eq!(kill.data["hero"], json!("npc_dota_hero_lina"));
    assert_eq!(kill.data["match_id"], json!("8421997461"));
    assert_eq!(kill.data["clock_time"], json!(615));

    let win = reports
        .iter()
        .find(|report| report.kind.as_str() == "win")
        .expect("the match was won");
    assert_eq!(win.data["team"], json!("radiant"));
    assert_eq!(win.data["winning_team"], json!("radiant"));

    let spree = reports
        .iter()
        .find(|report| report.kind.as_str() == "dota-2.kill_streak")
        .expect("the killing spree is reported");
    assert_eq!(spree.data["streak"], json!(3));
}

#[test]
fn every_event_this_plugin_can_produce_is_one_the_host_accepts() {
    // The host refuses an event whose kind claims a word in the project's
    // vocabulary, whose confidence is outside 0 to 1, or whose payload is over
    // the limit (`docs/plugin-api.md`, "The wire"). A plugin that produced one
    // would lose that event silently at run time, which is exactly the sort of
    // thing a fixture-driven test should find first.
    let source = EventSource::plugin(PLUGIN_ID).expect("this plugin's identifier is a source");
    let start = Instant::now();
    let mut cadence = Cadence::opened_at(start);
    let mut watcher = Watcher::new();

    let mut accepted = 0;
    for (interval, name) in MATCH.iter().enumerate() {
        let interval = u64::try_from(interval).expect("eleven payloads");
        let window = cadence.observe(start + Duration::from_millis(200 * (interval + 1)));
        for report in watcher.observe(&payload(name)).reports {
            let reported = window.report(report.kind, report.data);
            let event = reported
                .into_event(&source, EventTime::from_media_nanos(60_000_000_000))
                .unwrap_or_else(|refusal| {
                    panic!("{name} produced an event the host refuses: {refusal}")
                });

            assert_eq!(event.source().as_str(), PLUGIN_ID);
            assert_eq!(
                event.timing().precision(),
                Duration::from_millis(100),
                "a payload 200 ms after the last one places its events 100 ms ago, give or take \
                 100 ms"
            );
            assert_eq!(
                event.timing().at(),
                EventTime::from_media_nanos(59_900_000_000)
            );
            accepted += 1;
        }
    }
    assert_eq!(accepted, 10, "the whole match should have been accepted");
}

#[test]
fn a_spectated_game_reports_nothing_and_says_so_once() {
    // The two payloads cross `GAME_IN_PROGRESS` to `POST_GAME`, which is the
    // transition a *played* match ends on. That is the whole point of the pair:
    // while spectating, `map` is an ordinary description of a real match and
    // only `player` gives away that none of it is about the person at this
    // computer, so a plugin that gated the counters alone would still put
    // somebody else's match on this user's timeline.
    let mut watcher = Watcher::new();

    let first = watcher.observe(&payload(SPECTATED[0]));
    assert!(
        first.reports.is_empty(),
        "somebody else's kills are not the player's: {:?}",
        kinds(&first)
    );
    assert_eq!(first.notice, Some(Notice::Spectating));

    let ended = watcher.observe(&payload(SPECTATED[1]));
    assert!(
        ended.reports.is_empty(),
        "somebody else's match did not end on this user's timeline: {:?}",
        kinds(&ended)
    );
    assert!(
        ended.notice.is_none(),
        "a fact about the session is said once, not once per payload"
    );

    // And the same pair read the other way round, because a user can start
    // watching at the scoreboard as easily as before the horn.
    let mut watcher = Watcher::new();
    watcher.observe(&payload(SPECTATED[1]));
    let back = watcher.observe(&payload(SPECTATED[0]));
    assert!(
        back.reports.is_empty(),
        "nor did it start on it: {:?}",
        kinds(&back)
    );
}

fn kinds(observed: &clipped_dota2_plugin::dota::Observed) -> Vec<&str> {
    observed
        .reports
        .iter()
        .map(|report| report.kind.as_str())
        .collect()
}

#[test]
fn a_payload_this_plugin_cannot_read_produces_nothing_rather_than_nonsense() {
    // A Dota that renamed its fields, or a component this plugin does not
    // subscribe to arriving on its own. The plugin has to keep running and
    // report nothing, rather than reporting twelve kills from a field called
    // `eliminations` (AGENTS.md section 27).
    let mut watcher = Watcher::new();
    let strange = payload("unrecognisable.json");
    assert!(watcher.observe(&strange).reports.is_empty());
    assert!(watcher.observe(&strange).reports.is_empty());
    assert!(watcher.observe(&strange).notice.is_none());
}

#[test]
fn every_committed_payload_is_used_by_a_test() {
    // A fixture nobody reads is a payload somebody wrote a test for and then
    // renamed. This is what makes adding one to the directory a decision rather
    // than an accident.
    let mut committed: Vec<String> = fs::read_dir(fixtures())
        .expect("the fixture directory is there")
        .map(|entry| {
            entry
                .expect("a directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.ends_with(".json"))
        .collect();
    committed.sort();

    let mut used: Vec<String> = MATCH
        .iter()
        .chain(SPECTATED.iter())
        .chain(OTHERS.iter())
        .map(|name| (*name).to_owned())
        .collect();
    used.sort();

    assert_eq!(committed, used);
}
