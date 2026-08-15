//! The contract between the recorder that writes session sidecars and the index
//! that reads them.
//!
//! `clipped-session` sits four layers above `clipped-library`
//! (`tests/integration/tests/workspace_layering.rs`), so the writer's types are
//! not reachable from the reader and the compiler cannot hold the two in step.
//! Nothing else can either — which is exactly how a file format quietly grows a
//! field one end writes and the other discards. These two tests are what stands
//! in for the type system:
//!
//! - **The documented example.** `docs/sessions.md` prints the file the recorder
//!   writes. This test reads that document, indexes the example printed in it,
//!   and checks every field arrives in the right column. A change to the format
//!   that updates the documentation (AGENTS.md section 7) fails here until the
//!   reader is updated with it.
//! - **A file the real writer produced.**
//!   `fixtures/written-by-the-recorder.session.json` was captured from
//!   `clipped_session::automatic::sidecar::write` — the actual writer, running
//!   in `clipped-session`'s own test
//!   `the_sessions_record_is_on_disk_before_anything_needs_it` — and committed
//!   verbatim, with one modification: the temporary directory the capture ran in
//!   was replaced with `D:\clips`, so that no machine path is committed
//!   (AGENTS.md section 9). Nothing else in it was touched.
//!
//! Neither test is a restatement of the reader's own assumptions: one comes from
//! the prose that documents the format and the other from the code that writes
//! it.
//!
//! There is a third thing the compiler cannot hold in step, and it lived
//! unguarded until issue #71 moved the format: **the version number itself**.
//! `SUPPORTED_SCHEMA_VERSION` here and `SCHEMA_VERSION` in the writer are two
//! constants, and the comment on the first used to claim a test in this file
//! called `the_documented_sidecar_is_the_one_this_build_reads` was keeping them
//! equal. No test of that name has ever existed. The drift it named is the
//! worst kind — the recorder writes a sidecar every session and a reader one
//! version behind refuses *every* one of them — so the guard now exists, in
//! `clipped_session::automatic::sidecar`, which is the crate that can see both
//! numbers.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use clipped_library::index::{reconcile, IndexControl, IndexPace, IndexSettings};
use clipped_storage::Database;
use serde_json::Value;

fn observed_at() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_786_545_000)
}

fn scratch_directory(name: &str) -> PathBuf {
    let directory =
        std::env::temp_dir().join(format!("clipped-sidecars-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).expect("a scratch directory can be created");
    directory
}

/// Indexes a directory and answers the database.
fn index(root: &Path) -> Database {
    let mut database = Database::open(root.join("library.db")).expect("a database");
    let mut settings = IndexSettings::new([root.to_path_buf()]);
    settings.pace = IndexPace::foreground();
    let report = reconcile(
        &mut database,
        &settings,
        &IndexControl::new(),
        observed_at(),
    )
    .expect("reconciliation completes");
    assert!(
        report.problems.is_empty(),
        "the sidecar could not be indexed cleanly: {:?}",
        report.problems
    );
    assert_eq!(report.sessions_indexed, 1, "{report}");
    database
}

/// The JSON block `docs/sessions.md` prints as "the file".
fn documented_sidecar() -> String {
    let document = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("docs")
        .join("sessions.md");
    let text = fs::read_to_string(&document)
        .unwrap_or_else(|error| panic!("{} can be read: {error}", document.display()));

    let block = text
        .split("```json")
        .find(|block| block.contains("\"schema_version\""))
        .and_then(|block| block.split("```").next())
        .unwrap_or_else(|| {
            panic!(
                "{} no longer prints the session sidecar as a ```json block, so the \
                 contract between the recorder and the library index is undocumented",
                document.display()
            )
        });
    block.to_owned()
}

/// The game event the documentation prints reaches a row, field for field.
///
/// Separate from the whole-file test below because a document this build cannot
/// read is *skipped* by the ingest rather than refused — which is the right
/// behaviour for a file a newer build wrote, and which means a wrong example
/// would otherwise index cleanly and silently produce nothing. This asserts the
/// row, so the shape printed in `docs/sessions.md` has to be the real one.
#[test]
fn the_documented_game_event_reaches_a_row() {
    let root = scratch_directory("documented-game-event");
    let text = documented_sidecar().replace(
        "D:\\\\clips",
        &root.display().to_string().replace('\\', "\\\\"),
    );
    fs::write(
        root.join("clipped-counter-strike-2-20260811-143205.session.json"),
        &text,
    )
    .expect("the documented sidecar can be written");
    fs::write(
        root.join("clipped-counter-strike-2-20260811-143205.mkv"),
        [0u8; 128],
    )
    .expect("the recording it names can be written");
    fs::write(
        root.join("clipped-counter-strike-2-20260811-143205-replay-1.mkv"),
        [0u8; 64],
    )
    .expect("the clip it names can be written");

    let database = index(&root);

    let (session, recording, at_nanos, kind, source, document): (
        String,
        Option<i64>,
        i64,
        String,
        String,
        String,
    ) = database
        .connection()
        .query_row(
            "SELECT session_id, recording_id, at_nanos, kind, source, document FROM game_events",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .expect("the documented game event was indexed");

    assert_eq!(session, "counter-strike-2-20260811-143205");
    assert_eq!(at_nanos, 137_000_000_000);
    assert_eq!(kind, "kill");
    assert_eq!(source, "cs2");
    assert_eq!(
        recording, None,
        "an event was placed in a recording, which nothing can do yet: the sidecar \
         records no media-timeline span per recording (see `write_game_events`)"
    );

    // The payload survives being stored, which is the whole argument for
    // `document` being the authority rather than the columns beside it.
    let stored: Value = serde_json::from_str(&document).expect("the stored document is JSON");
    assert_eq!(stored["data"]["weapon"], Value::from("ak47"));
    assert_eq!(stored["data"]["headshot"], Value::from(true));
}

/// The documented file is the file this build reads, field for field.
#[test]
fn the_documented_session_record_is_the_one_this_build_indexes() {
    let root = scratch_directory("documented");
    let text = documented_sidecar();
    let written: Value = serde_json::from_str(&text)
        .expect("the example printed in docs/sessions.md is not valid JSON");

    // The example names `D:\clips`, which is not where this test is running, so
    // the paths are pointed at the directory being indexed. Nothing else about
    // the file is changed.
    let text = text.replace(
        "D:\\\\clips",
        &root.display().to_string().replace('\\', "\\\\"),
    );
    fs::write(
        root.join("clipped-counter-strike-2-20260811-143205.session.json"),
        &text,
    )
    .expect("the documented sidecar can be written");
    fs::write(
        root.join("clipped-counter-strike-2-20260811-143205.mkv"),
        [0u8; 128],
    )
    .expect("the recording it names can be written");
    fs::write(
        root.join("clipped-counter-strike-2-20260811-143205-replay-1.mkv"),
        [0u8; 64],
    )
    .expect("the clip it names can be written");

    let database = index(&root);

    let (session_id, game_id, started, ended, reason): (String, String, String, String, String) =
        database
            .connection()
            .query_row(
                "SELECT session_id, game_id, started_at, ended_at, end_reason FROM sessions",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .expect("the documented session is indexed");
    assert_eq!(session_id, written["session_id"].as_str().expect("an id"));
    assert_eq!(
        game_id,
        written["game"]["game_id"].as_str().expect("a game")
    );
    assert_eq!(started, written["started_at"].as_str().expect("a start"));
    assert_eq!(ended, written["ended_at"].as_str().expect("an end"));
    assert_eq!(
        reason,
        written["events"]
            .as_array()
            .expect("events")
            .iter()
            .find(|event| event["event"] == "session-ended")
            .and_then(|event| event["reason"].as_str())
            .expect("the documented file ends its session with a reason")
    );

    let recording = &written["recordings"][0];
    let recording_row_id: i64 = database
        .connection()
        .query_row("SELECT recording_id FROM recordings", [], |row| row.get(0))
        .expect("the documented recording is indexed");
    let (index, outcome, end_reason, frames, width, height, duration, size): (
        i64,
        String,
        String,
        i64,
        i64,
        i64,
        f64,
        i64,
    ) = database
        .connection()
        .query_row(
            "SELECT session_index, outcome, end_reason, frames_encoded, width, height, \
                    duration_seconds, size_bytes FROM recordings",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )
        .expect("the documented recording is indexed");
    assert_eq!(index, recording["index"].as_i64().expect("an index"));
    assert_eq!(outcome, recording["outcome"].as_str().expect("an outcome"));
    assert_eq!(
        end_reason,
        recording["end_reason"].as_str().expect("an end reason")
    );
    assert_eq!(
        frames,
        recording["frames_encoded"].as_i64().expect("a frame count")
    );
    assert_eq!(width, recording["width"].as_i64().expect("a width"));
    assert_eq!(height, recording["height"].as_i64().expect("a height"));
    assert!(
        (duration - recording["duration_seconds"].as_f64().expect("a duration")).abs()
            < f64::EPSILON
    );
    assert_eq!(
        size, 128,
        "the size comes from the file, not from the record"
    );

    let events: i64 = database
        .connection()
        .query_row("SELECT COUNT(*) FROM session_events", [], |row| row.get(0))
        .expect("the events can be counted");
    assert_eq!(
        events,
        written["events"].as_array().expect("events").len() as i64,
        "an event printed in docs/sessions.md did not reach the index"
    );

    // The clip the documented session saved out of its recording
    // ([issue #38](https://github.com/wildware-uk/clipped/issues/38)). This is
    // the whole of "a replay reaches the library the same way a recording
    // does": the recorder writes it into the session's own record, and the
    // index reads it out of there into the table that was designed for it. A
    // build that wrote clips and never indexed them would leave the user with
    // files their library has never heard of.
    let clip = &written["clips"][0];
    let (path, recording_id, created, start, end, duration, size): (
        String,
        i64,
        String,
        f64,
        f64,
        f64,
        i64,
    ) = database
        .connection()
        .query_row(
            "SELECT path, source_recording_id, created_at, source_start_seconds, \
                    source_end_seconds, duration_seconds, size_bytes FROM clips",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .expect("the documented clip is indexed");
    assert!(
        path.ends_with("clipped-counter-strike-2-20260811-143205-replay-1.mkv"),
        "the clip's own path has to reach the row, or nothing can play it: {path}"
    );
    assert_eq!(
        recording_id, recording_row_id,
        "a clip has to point at the recording it was cut from, or \"what depends on this \
         recording?\" cannot be answered before a deletion (#111)"
    );
    assert_eq!(created, clip["created_at"].as_str().expect("a time"));
    assert!(
        (start - clip["source_start_seconds"].as_f64().expect("a start")).abs() < f64::EPSILON
            && (end - clip["source_end_seconds"].as_f64().expect("an end")).abs() < f64::EPSILON,
        "the clip's bounds in its recording have to survive being indexed"
    );
    assert!(
        (duration - clip["duration_seconds"].as_f64().expect("a duration")).abs() < f64::EPSILON
    );
    assert_eq!(
        size, 64,
        "the clip's size comes from the file, exactly as a recording's does"
    );
}

/// A file the writer itself produced, indexed end to end.
#[test]
fn a_session_record_the_recorder_wrote_indexes_without_a_single_problem() {
    let root = scratch_directory("real-writer");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("written-by-the-recorder.session.json");
    let text = fs::read_to_string(&fixture).expect("the fixture can be read");

    // Same as above: the recorder wrote absolute paths, and they are pointed at
    // this test's directory so that the file it names is really there.
    let text = text.replace(
        "D:\\\\clips",
        &root.display().to_string().replace('\\', "\\\\"),
    );
    fs::write(
        root.join("clipped-test-game-20260811-153205.session.json"),
        &text,
    )
    .expect("the fixture can be written");
    fs::write(
        root.join("clipped-test-game-20260811-153205.mkv"),
        [0u8; 4096],
    )
    .expect("the recording it names can be written");

    let database = index(&root);

    let (name, sessions): (String, i64) = database
        .connection()
        .query_row(
            "SELECT games.name, COUNT(sessions.session_id) FROM games \
             JOIN sessions ON sessions.game_id = games.game_id GROUP BY games.game_id",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("the game is indexed");
    assert_eq!(name, "Test Game");
    assert_eq!(sessions, 1);

    let (outcome, frames, width, height, end_reason, size): (String, i64, i64, i64, String, i64) =
        database
            .connection()
            .query_row(
                "SELECT outcome, frames_encoded, width, height, end_reason, size_bytes \
             FROM recordings",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .expect("the recording is indexed");
    assert_eq!(outcome, "recorded");
    assert_eq!(frames, 181);
    assert_eq!((width, height), (1280, 720));
    assert_eq!(end_reason, "target-lost");
    assert_eq!(size, 4096);

    let missing: Option<String> = database
        .connection()
        .query_row("SELECT missing_since FROM recordings", [], |row| row.get(0))
        .expect("the recording is indexed");
    assert_eq!(missing, None, "a file that is there was marked missing");

    let kinds: Vec<String> = {
        let mut statement = database
            .connection()
            .prepare("SELECT kind FROM session_events ORDER BY at, event_id")
            .expect("the events can be read");
        let kinds = statement
            .query_map([], |row| row.get::<_, String>(0))
            .expect("the query runs")
            .map(|kind| kind.expect("a kind"))
            .collect();
        kinds
    };
    assert_eq!(
        kinds,
        [
            "session-started",
            "recording-started",
            "game-exited",
            "recording-ended",
            "session-ended",
        ],
        "the session's history did not survive being indexed"
    );

    let ended: String = database
        .connection()
        .query_row("SELECT end_reason FROM sessions", [], |row| row.get(0))
        .expect("the session is indexed");
    assert_eq!(ended, "game-exited");
}

/// A session record for a recording somebody asked for, indexed end to end.
///
/// `fixtures/written-by-the-window.session.json` was captured the same way its
/// sibling was: from `clipped_session::automatic`'s own writer, running in that
/// crate's tests, with the temporary directory replaced by `D:\clips` and
/// nothing else touched. Its `clips` entry and `replay-saved` event were
/// captured from the same writer a second time, when
/// [issue #38](https://github.com/wildware-uk/clipped/issues/38) taught it to
/// record a saved replay, and pasted in with the same substitution. It is the file the recorder writes when a recording is
/// started over the protocol rather than by a game launching
/// ([issue #402](https://github.com/wildware-uk/clipped/issues/402)).
///
/// Three things about it are new to this reader, and a build that had only been
/// taught one of them would index part of the sitting: a game of kind
/// `unidentified`, a session that ended because its recording did, and a clip
/// saved out of the recording's replay buffer
/// ([issue #38](https://github.com/wildware-uk/clipped/issues/38)). All three
/// are in the fixture, so all three are checked by indexing it.
#[test]
fn a_session_record_the_window_produced_indexes_without_a_single_problem() {
    let root = scratch_directory("window-writer");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("written-by-the-window.session.json");
    let text = fs::read_to_string(&fixture).expect("the fixture can be read");
    let text = text.replace(
        "D:\\\\clips",
        &root.display().to_string().replace('\\', "\\\\"),
    );
    fs::write(
        root.join("clipped-unattributed-20260811-153205.session.json"),
        &text,
    )
    .expect("the fixture can be written");
    fs::write(
        root.join("clipped-unattributed-20260811-153205.mkv"),
        [0u8; 2048],
    )
    .expect("the recording it names can be written");
    fs::write(
        root.join("clipped-unattributed-20260811-153205-replay-1.mkv"),
        [0u8; 512],
    )
    .expect("the clip it names can be written");

    // `index` refuses any problem at all, so a `kind` or an end reason this
    // build could not interpret fails here rather than being indexed quietly.
    let database = index(&root);

    let (session_id, game_id, end_reason): (String, Option<String>, Option<String>) = database
        .connection()
        .query_row(
            "SELECT session_id, game_id, end_reason FROM sessions",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("the session is indexed");
    assert_eq!(session_id, "unattributed-20260811-153205");
    assert_eq!(
        game_id, None,
        "nothing identified a game, and inventing one would be worse than saying so"
    );
    assert_eq!(
        end_reason.as_deref(),
        Some("recording-ended"),
        "the reason a session opened for one recording ends has to survive the vocabulary \
         the column constrains"
    );

    let games: i64 = database
        .connection()
        .query_row("SELECT COUNT(*) FROM games", [], |row| row.get(0))
        .expect("the games can be counted");
    assert_eq!(games, 0, "an unattributed session must not invent a game");

    let (outcome, frames, size, missing): (String, i64, i64, Option<String>) = database
        .connection()
        .query_row(
            "SELECT outcome, frames_encoded, size_bytes, missing_since FROM recordings",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("the recording is indexed");
    assert_eq!(outcome, "recorded");
    assert_eq!(frames, 181);
    assert_eq!(
        size, 2048,
        "the size comes from the file, not from the record"
    );
    assert_eq!(missing, None);

    // The clip the recorder saved out of that recording's replay buffer. It is
    // keyed on its own path and points back at the recording it was cut from,
    // which is what makes "this clip came from that file" a query rather than a
    // scan (`crates/storage/migrations/0001_initial.sql`).
    let (clip_path, source, complete_size, clip_duration): (String, i64, i64, f64) = database
        .connection()
        .query_row(
            "SELECT clips.path, clips.source_recording_id, clips.size_bytes,                     clips.duration_seconds              FROM clips JOIN recordings ON recordings.recording_id = clips.source_recording_id",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("the clip is indexed, and against the recording it came from");
    assert!(
        clip_path.ends_with("clipped-unattributed-20260811-153205-replay-1.mkv"),
        "{clip_path}"
    );
    assert!(
        source > 0,
        "a clip has to name the recording it was cut from"
    );
    assert_eq!(complete_size, 512, "the clip's size comes from its file");
    assert!((clip_duration - 29.983).abs() < f64::EPSILON);

    // And re-indexing the same sidecar must not produce a second clip: the
    // path is the natural key, so a library re-walked every start-up would
    // otherwise grow a row per run (`clipped_library::index::ingest`).
    let database = index(&root);
    let clips: i64 = database
        .connection()
        .query_row("SELECT COUNT(*) FROM clips", [], |row| row.get(0))
        .expect("the clips can be counted");
    assert_eq!(clips, 1, "re-indexing wrote the clip a second time");
}

/// A `game.kind` a newer recorder invented costs the attribution and not the
/// sitting.
///
/// The forward-compatibility promise `crates/session/src/automatic/sidecar.rs`
/// makes when it adds a kind without changing `schema_version`. Without it, a
/// user who downgraded Clipped would find every session written by the newer
/// build missing from their library rather than merely unattributed.
#[test]
fn a_game_kind_this_build_has_never_heard_of_is_indexed_unattributed_and_reported() {
    let root = scratch_directory("unknown-kind");
    let text = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("written-by-the-window.session.json"),
    )
    .expect("the fixture can be read")
    .replace("\"unidentified\"", "\"invented-by-a-later-build\"")
    .replace(
        "D:\\\\clips",
        &root.display().to_string().replace('\\', "\\\\"),
    );
    fs::write(
        root.join("clipped-unattributed-20260811-153205.session.json"),
        &text,
    )
    .expect("the sidecar can be written");
    fs::write(
        root.join("clipped-unattributed-20260811-153205.mkv"),
        [0u8; 2048],
    )
    .expect("the recording it names can be written");

    let mut database = Database::open(root.join("library.db")).expect("a database");
    let mut settings = IndexSettings::new([root.to_path_buf()]);
    settings.pace = IndexPace::foreground();
    let report = reconcile(
        &mut database,
        &settings,
        &IndexControl::new(),
        observed_at(),
    )
    .expect("reconciliation completes");

    assert_eq!(
        report.sessions_indexed, 1,
        "the sitting is worth more than the one word that could not be read"
    );
    assert_eq!(report.recordings_indexed, 1);
    assert_eq!(
        report.problems.len(),
        1,
        "a kind this build cannot interpret must be reported rather than swallowed: {:?}",
        report.problems
    );

    let game_id: Option<String> = database
        .connection()
        .query_row("SELECT game_id FROM sessions", [], |row| row.get(0))
        .expect("the session is indexed");
    assert_eq!(game_id, None);
}
