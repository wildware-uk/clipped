//! The whole derivation, over a sequence of payloads, without a game.
//!
//! `crate::derive`'s own tests take one case at a time from payloads written in
//! the test. This one plays a recorded sequence back in order, from the files
//! in `tests/payloads/`, and asserts the complete list of events that comes
//! out — because a plugin that gets each case right in isolation can still
//! report a match that never happened once the cases run into each other.
//!
//! The fixtures are constructed rather than captured, and `tests/payloads/
//! README.md` says so plainly. What this proves is that the derivation matches
//! the payload shape Game State Integration is documented to produce; what it
//! cannot prove is that the shape is right. That is
//! [issue #70](https://github.com/wildware-uk/clipped/issues/70)'s first
//! acceptance criterion and it needs a machine with the game on it.

use core::time::Duration;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use clipped_events::EventKind;
use serde_json::Value;

use clipped_cs2_plugin::derive::{MatchTracker, Step, StepNote};
use clipped_cs2_plugin::payload::GsiPayload;

/// How long the plugin pretends each payload took to arrive.
///
/// A fixed step, so that every window in this file is the same and the moments
/// asserted below are arithmetic rather than timing.
const STEP: Duration = Duration::from_millis(400);

/// Reads a directory of payloads in file-name order.
fn sequence(directory: &str) -> Vec<(String, GsiPayload)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("payloads")
        .join(directory);

    let mut files: Vec<PathBuf> = fs::read_dir(&root)
        .unwrap_or_else(|error| panic!("{} should be readable: {error}", root.display()))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    files.sort();
    assert!(!files.is_empty(), "{} holds no payloads", root.display());

    files
        .into_iter()
        .map(|path| {
            let name = path
                .file_name()
                .expect("a file")
                .to_string_lossy()
                .into_owned();
            let json = fs::read(&path).expect("the payload reads");
            let payload = GsiPayload::parse(&json)
                .unwrap_or_else(|error| panic!("{name} is not a payload: {error}"));
            (name, payload)
        })
        .collect()
}

/// Plays a sequence through a tracker, one `STEP` apart.
fn play(directory: &str) -> Vec<(String, Step)> {
    let mut tracker = MatchTracker::new();
    let origin = Instant::now();
    sequence(directory)
        .into_iter()
        .enumerate()
        .map(|(index, (name, payload))| {
            let received = origin + STEP * u32::try_from(index).expect("a short sequence");
            let step = tracker.observe(&payload, received);
            (name, step)
        })
        .collect()
}

/// Everything a sequence produced, as `file: kind` lines.
fn transcript(steps: &[(String, Step)]) -> Vec<String> {
    steps
        .iter()
        .flat_map(|(name, step)| {
            step.events
                .iter()
                .map(move |event| format!("{name}: {}", event.kind.as_str()))
        })
        .collect()
}

#[test]
fn the_opening_of_a_match_derives_exactly_the_events_that_happened() {
    let steps = play("competitive_match");

    assert_eq!(
        transcript(&steps),
        vec![
            // 00_menu is the baseline and reports nothing at all.
            "01_warmup.json: match_started",
            "02_round_live.json: round_started",
            "03_double_kill.json: kill",
            "03_double_kill.json: kill",
            "04_headshot_kill.json: kill",
            "05_round_over.json: round_ended",
            "06_next_round_live.json: round_started",
            "07_death_and_assist.json: death",
            "07_death_and_assist.json: assist",
        ],
        "the events derived from the sequence are not the ones in it"
    );
}

#[test]
fn the_first_payload_of_a_session_reports_nothing_whatever_it_says() {
    let steps = play("competitive_match");
    let (name, first) = &steps[0];

    assert_eq!(name, "00_menu.json");
    assert!(first.events.is_empty());
    assert!(first.notes.contains(&StepNote::Baselined));
}

#[test]
fn the_games_own_words_travel_in_the_payload_and_nowhere_else() {
    let steps = play("competitive_match");
    let data = |file: &str, index: usize| -> Value {
        let (_, step) = steps
            .iter()
            .find(|(name, _)| name == file)
            .unwrap_or_else(|| panic!("{file} is in the sequence"));
        Value::Object(step.events[index].data.clone())
    };

    assert_eq!(
        data("01_warmup.json", 0),
        serde_json::json!({"map": "de_dust2", "mode": "competitive"})
    );
    assert_eq!(
        data("02_round_live.json", 0),
        serde_json::json!({"round": 0})
    );
    assert_eq!(
        data("04_headshot_kill.json", 0),
        serde_json::json!({"headshot": true}),
        "one kill, one more headshot in the round: attributable, and attributed"
    );
    assert_eq!(
        data("05_round_over.json", 0),
        serde_json::json!({"round": 0, "win_team": "CT"})
    );

    // Two kills between two payloads, one of them a headshot. Which one is not
    // in the payload, so neither event claims it.
    assert_eq!(data("03_double_kill.json", 0), serde_json::json!({}));
    assert_eq!(data("03_double_kill.json", 1), serde_json::json!({}));
}

#[test]
fn every_event_sits_in_the_middle_of_the_window_it_could_have_happened_in() {
    let steps = play("competitive_match");

    for (name, step) in &steps {
        for event in &step.events {
            assert_eq!(
                event.precision,
                STEP / 2,
                "{name} claimed a precision the payloads cannot support"
            );
        }
        assert!(
            step.events.windows(2).all(|pair| pair[0].at == pair[1].at),
            "{name} separated events one payload cannot separate"
        );
    }
}

#[test]
fn a_match_ending_reports_the_round_the_match_and_the_result_in_that_order() {
    let steps = play("match_end");

    assert_eq!(
        transcript(&steps),
        vec![
            "01_gameover.json: kill",
            "01_gameover.json: round_ended",
            "01_gameover.json: match_ended",
            "01_gameover.json: win",
        ],
        "what opened, then what happened inside it, then what closed"
    );
}

#[test]
fn replaying_a_payload_out_of_order_invents_nothing() {
    // The failure this guard exists for: each post is its own connection to a
    // loopback port, so the operating system is free to hand them over in a
    // different order than the game sent them. Playing the sequence with one
    // payload delivered twice — once late — must produce the same events.
    let ordered = play("competitive_match");
    let expected = transcript(&ordered);

    let payloads = sequence("competitive_match");
    let mut tracker = MatchTracker::new();
    let origin = Instant::now();
    let mut produced: Vec<String> = Vec::new();
    let mut stale = 0_usize;
    let mut clock = 0_u32;

    for (index, (name, payload)) in payloads.iter().enumerate() {
        let step = tracker.observe(payload, origin + STEP * clock);
        clock += 1;
        produced.extend(
            step.events
                .iter()
                .map(|event| format!("{name}: {}", event.kind.as_str())),
        );

        // Redeliver the payload before this one, as a duplicate arriving late.
        if let Some((_, earlier)) = index.checked_sub(1).and_then(|before| payloads.get(before)) {
            let late = tracker.observe(earlier, origin + STEP * clock);
            clock += 1;
            assert!(
                late.events.is_empty(),
                "a payload that arrived late invented {:?}",
                late.kinds()
            );
            if late
                .notes
                .iter()
                .any(|note| matches!(note, StepNote::Stale { .. }))
            {
                stale += 1;
            }
        }
    }

    assert_eq!(
        produced, expected,
        "delivering payloads out of order changed what was reported"
    );
    assert!(
        stale >= 6,
        "the ordering guard should have caught the late payloads, and caught {stale}"
    );
}

#[test]
fn a_spectated_teammates_payload_in_the_middle_of_a_match_reports_nothing() {
    let teammate = GsiPayload::parse(
        &fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests")
                .join("payloads")
                .join("spectating_teammate.json"),
        )
        .expect("the fixture reads"),
    )
    .expect("the fixture is a payload");

    let mut tracker = MatchTracker::new();
    let origin = Instant::now();
    for (index, (_, payload)) in sequence("competitive_match").into_iter().enumerate() {
        tracker.observe(
            &payload,
            origin + STEP * u32::try_from(index).expect("short"),
        );
    }

    let step = tracker.observe(&teammate, origin + STEP * 9);
    assert!(
        !step
            .events
            .iter()
            .any(|event| matches!(event.kind, EventKind::Kill | EventKind::Death)),
        "a teammate's twenty-one kills were reported as the player's: {:?}",
        step.kinds()
    );
    assert!(step.notes.contains(&StepNote::AboutAnotherPlayer));
}
