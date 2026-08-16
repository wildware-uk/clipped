//! Reconciliation against a real database, real files, and users behaving as
//! users do.
//!
//! Everything here runs anywhere: a library is a directory and a database, not
//! a GPU. What each test is *for* is written above it, because the value of
//! this file is not that indexing works on a tidy library — it is what happens
//! to somebody's recordings when it does not (AGENTS.md sections 16 and 56).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use clipped_library::index::{
    game_summaries, reconcile, IndexControl, IndexPace, IndexProblem, IndexReport, IndexSettings,
};
use clipped_storage::rusqlite::OptionalExtension;
use clipped_storage::Database;
use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// The moment a run is stamped with, so that what is written does not depend on
/// when the test ran (AGENTS.md section 25).
fn observed_at() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_786_545_000)
}

/// A later one, for the second of two runs.
fn observed_later() -> SystemTime {
    observed_at() + Duration::from_secs(3_600)
}

/// A library on disk: a folder of recordings and the index of it.
struct Library {
    root: PathBuf,
    database: PathBuf,
}

impl Library {
    fn new(name: &str) -> Self {
        let directory =
            std::env::temp_dir().join(format!("clipped-indexing-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        let root = directory.join("clips");
        fs::create_dir_all(&root).expect("a scratch library can be created");
        Self {
            root,
            database: directory.join("library.db"),
        }
    }

    fn open(&self) -> Database {
        Database::open(&self.database).expect("the database can be opened")
    }

    fn settings(&self) -> IndexSettings {
        let mut settings = IndexSettings::new([self.root.clone()]);
        // The tests are not measuring the pace, and a rest between every batch
        // would only make them slow.
        settings.pace = IndexPace::foreground();
        settings
    }

    /// Writes a sidecar and the files it names, and answers where the sidecar
    /// went.
    fn add(&self, session: &SessionFixture) -> PathBuf {
        for recording in &session.recordings {
            if recording.present {
                fs::write(self.root.join(&recording.file), vec![0u8; recording.size])
                    .expect("a recording can be written");
            }
        }
        let path = self
            .root
            .join(format!("clipped-{}.session.json", session.id));
        fs::write(&path, session.json(&self.root).to_string()).expect("a sidecar can be written");
        path
    }

    fn file(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn run(&self, database: &mut Database, at: SystemTime) -> IndexReport {
        reconcile(database, &self.settings(), &IndexControl::new(), at)
            .expect("reconciliation completes")
    }
}

/// One session, as a sidecar would describe it.
struct SessionFixture {
    id: String,
    game: Value,
    started_at: String,
    ended_at: Option<String>,
    recordings: Vec<RecordingFixture>,
    events: Vec<Value>,
}

struct RecordingFixture {
    index: u32,
    file: String,
    size: usize,
    /// Whether the file is written to disk at all — a session whose recording
    /// the user has already deleted.
    present: bool,
}

impl SessionFixture {
    /// A finished session of one game with one recording, which is the
    /// overwhelmingly common shape (`docs/sessions.md`).
    fn new(id: &str, game_id: &str, game_name: &str, started_at: &str) -> Self {
        Self {
            id: id.to_owned(),
            game: json!({ "kind": "known", "game_id": game_id, "name": game_name }),
            started_at: started_at.to_owned(),
            ended_at: None,
            recordings: Vec::new(),
            events: Vec::new(),
        }
    }

    fn ended(mut self, at: &str, reason: &str) -> Self {
        self.ended_at = Some(at.to_owned());
        self.events
            .push(json!({ "at": at, "event": "session-ended", "reason": reason }));
        self
    }

    fn recording(mut self, index: u32, file: &str, size: usize) -> Self {
        self.recordings.push(RecordingFixture {
            index,
            file: file.to_owned(),
            size,
            present: true,
        });
        self
    }

    fn event(mut self, event: Value) -> Self {
        self.events.push(event);
        self
    }

    fn json(&self, directory: &Path) -> Value {
        let recordings: Vec<Value> = self
            .recordings
            .iter()
            .map(|recording| {
                json!({
                    "index": recording.index,
                    "output": directory.join(&recording.file).display().to_string(),
                    "started_at": self.started_at,
                    "ended_at": self.ended_at,
                    "outcome": "recorded",
                    "frames_encoded": 65_040,
                    "duration_seconds": 1084.0,
                    "width": 2560,
                    "height": 1440,
                    "end_reason": "target-lost",
                })
            })
            .collect();

        json!({
            "schema_version": 1,
            "session_id": self.id,
            "game": self.game,
            "started_at": self.started_at,
            "ended_at": self.ended_at,
            "recordings": recordings,
            "clips": [],
            "bookmarks": [],
            "events": self.events,
        })
    }
}

/// Every row of the tables ingestion writes, as text, for comparing one run
/// against another.
///
/// `session_events.event_id` is deliberately absent: events have no natural key
/// and are rewritten wholesale for the session being ingested, so their
/// identifiers are not stable and nothing refers to them.
fn snapshot(database: &Database) -> Vec<String> {
    let mut rows = Vec::new();
    for query in [
        "SELECT 'game', game_id, name, first_seen_at, COALESCE(last_played_at, '-') FROM games \
         ORDER BY game_id",
        "SELECT 'session', session_id, COALESCE(game_id, '-'), started_at, \
            COALESCE(ended_at, '-'), COALESCE(end_reason, '-'), COALESCE(sidecar_path, '-'), \
            COALESCE(favourited_at, '-') FROM sessions ORDER BY session_id",
        // Every column ingestion writes, and not a chosen few: a snapshot blind
        // to a column is a test that cannot see the code silently dropping it.
        "SELECT 'recording', CAST(recording_id AS TEXT), session_id, CAST(session_index AS TEXT), \
            path, started_at, COALESCE(ended_at, '-'), COALESCE(outcome, '-'), \
            COALESCE(end_reason, '-'), COALESCE(CAST(duration_seconds AS TEXT), '-'), \
            COALESCE(CAST(frames_encoded AS TEXT), '-'), COALESCE(CAST(width AS TEXT), '-'), \
            COALESCE(CAST(height AS TEXT), '-'), COALESCE(CAST(size_bytes AS TEXT), '-'), \
            COALESCE(missing_since, '-'), COALESCE(favourited_at, '-'), \
            COALESCE(deleted_at, '-') FROM recordings ORDER BY recording_id",
        "SELECT 'event', session_id, at, kind, COALESCE(detail, '-') FROM session_events \
         ORDER BY session_id, at, kind",
        "SELECT 'candidate', session_id, game_id FROM session_game_candidates \
         ORDER BY session_id, game_id",
    ] {
        let mut statement = database
            .connection()
            .prepare(query)
            .expect("the snapshot query is valid");
        let mut answered = statement.query([]).expect("the query runs");
        while let Some(row) = answered.next().expect("a row") {
            let mut columns = Vec::new();
            for index in 0.. {
                match row.get::<_, String>(index) {
                    Ok(value) => columns.push(value),
                    Err(_) => break,
                }
            }
            rows.push(columns.join(" | "));
        }
    }
    rows
}

fn column_of_recording(database: &Database, path: &Path, column: &str) -> Option<String> {
    database
        .connection()
        .query_row(
            &format!("SELECT CAST({column} AS TEXT) FROM recordings WHERE path = ?1"),
            [path.display().to_string()],
            |row| row.get::<_, Option<String>>(0),
        )
        .expect("the recording is in the index")
}

fn count(database: &Database, table: &str) -> i64 {
    database
        .connection()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .expect("the rows can be counted")
}

/// Milestone 6's playable result, and the first acceptance criterion of
/// issue #56: after a session, its recordings are in the library under the game
/// that was played.
#[test]
fn recordings_appear_organised_by_game_after_a_session() {
    let library = Library::new("organised-by-game");
    library.add(
        &SessionFixture::new(
            "counter-strike-2-20260811-143205",
            "counter-strike-2",
            "Counter-Strike 2",
            "2026-08-11T14:32:05+01:00",
        )
        .recording(1, "clipped-counter-strike-2-20260811-143205.mkv", 4096)
        .recording(2, "clipped-counter-strike-2-20260811-143205-2.mkv", 2048)
        .ended("2026-08-11T15:31:21+01:00", "game-exited"),
    );
    library.add(
        &SessionFixture::new(
            "dota-2-20260810-190000",
            "dota-2",
            "Dota 2",
            "2026-08-10T19:00:00+01:00",
        )
        .recording(1, "clipped-dota-2-20260810-190000.mkv", 1024)
        .ended("2026-08-10T20:00:00+01:00", "game-exited"),
    );
    let mut database = library.open();

    let report = library.run(&mut database, observed_at());

    assert_eq!(report.sessions_indexed, 2, "{report}");
    assert_eq!(report.recordings_indexed, 3, "{report}");
    assert!(report.problems.is_empty(), "{:?}", report.problems);

    let summaries = game_summaries(&database).expect("the games view can be built");
    let names: Vec<Option<&str>> = summaries
        .iter()
        .map(|summary| summary.game_id.as_deref())
        .collect();
    assert_eq!(names, [Some("counter-strike-2"), Some("dota-2")]);

    let counter_strike = &summaries[0];
    assert_eq!(counter_strike.name.as_deref(), Some("Counter-Strike 2"));
    assert_eq!(counter_strike.sessions, 1);
    assert_eq!(counter_strike.recordings, 2);
    assert_eq!(counter_strike.bytes, 4096 + 2048);
    assert_eq!(counter_strike.missing, 0);
    assert_eq!(
        counter_strike.last_played_at.as_deref(),
        Some("2026-08-11T15:31:21+01:00")
    );
    assert_eq!(summaries[1].bytes, 1024);

    // The session's own facts, not only the counts.
    let (game, ended, reason): (String, String, String) = database
        .connection()
        .query_row(
            "SELECT game_id, ended_at, end_reason FROM sessions \
             WHERE session_id = 'counter-strike-2-20260811-143205'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("the session is indexed");
    assert_eq!(game, "counter-strike-2");
    assert_eq!(ended, "2026-08-11T15:31:21+01:00");
    assert_eq!(reason, "game-exited");

    let (width, frames, duration): (i64, i64, f64) = database
        .connection()
        .query_row(
            "SELECT width, frames_encoded, duration_seconds FROM recordings \
             WHERE session_index = 2",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("the second recording is indexed");
    assert_eq!((width, frames), (2560, 65_040));
    assert!((duration - 1084.0).abs() < f64::EPSILON);
}

/// `docs/storage.md`'s promise: the database is derived, and losing it costs an
/// index rather than a library. This is that promise as a test — the database
/// is deleted outright and rebuilt from nothing but the files on disk.
#[test]
fn the_index_can_be_rebuilt_from_the_sidecars_alone() {
    let library = Library::new("rebuild");
    library.add(
        &SessionFixture::new(
            "counter-strike-2-20260811-143205",
            "counter-strike-2",
            "Counter-Strike 2",
            "2026-08-11T14:32:05+01:00",
        )
        .recording(1, "one.mkv", 4096)
        .event(
            json!({ "at": "2026-08-11T14:32:05+01:00", "event": "session-started",
                       "pid": 4242, "image_name": "cs2.exe" }),
        )
        .ended("2026-08-11T15:31:21+01:00", "game-exited"),
    );
    let mut database = library.open();
    library.run(&mut database, observed_at());
    let before = snapshot(&database);
    assert!(!before.is_empty());

    drop(database);
    fs::remove_file(&library.database).expect("the index can be deleted");
    let _ = fs::remove_file(library.database.with_extension("db-wal"));
    let _ = fs::remove_file(library.database.with_extension("db-shm"));
    let mut rebuilt = library.open();
    library.run(&mut rebuilt, observed_at());

    assert_eq!(
        snapshot(&rebuilt),
        before,
        "an index rebuilt from the sidecars is not the index that was lost"
    );
}

/// The second acceptance criterion: a file the user deleted in Explorer is
/// marked, the row survives, and nothing errors.
#[test]
fn a_file_deleted_behind_the_applications_back_is_marked_missing_and_the_row_is_kept() {
    let library = Library::new("deleted-file");
    library.add(
        &SessionFixture::new(
            "counter-strike-2-20260811-143205",
            "counter-strike-2",
            "Counter-Strike 2",
            "2026-08-11T14:32:05+01:00",
        )
        .recording(1, "one.mkv", 4096)
        .recording(2, "two.mkv", 2048)
        .ended("2026-08-11T15:31:21+01:00", "game-exited"),
    );
    let mut database = library.open();
    library.run(&mut database, observed_at());
    let deleted = library.file("one.mkv");
    let kept = library.file("two.mkv");

    fs::remove_file(&deleted).expect("the user deletes a recording");
    let report = library.run(&mut database, observed_later());

    assert_eq!(report.recordings_newly_missing, 1, "{report}");
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert_eq!(
        count(&database, "recordings"),
        2,
        "a missing file must not remove its row: somebody may plug the drive back in"
    );
    let marked = column_of_recording(&database, &deleted, "missing_since")
        .expect("the deleted recording is not marked missing");
    assert_eq!(
        OffsetDateTime::parse(&marked, &Rfc3339)
            .expect("the mark is RFC 3339, as every other timestamp in the schema is")
            .unix_timestamp(),
        observed_later()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("after the epoch")
            .as_secs() as i64,
        "the mark should be the moment the run was stamped with, and is {marked}"
    );
    assert_eq!(
        column_of_recording(&database, &kept, "missing_since"),
        None,
        "a recording that is still there was marked missing"
    );
    assert!(kept.exists(), "reconciliation deleted a recording");

    // What a library screen shows: the space the missing file is not occupying
    // is not counted, and it is counted as missing instead.
    let summaries = game_summaries(&database).expect("the games view can be built");
    assert_eq!(summaries[0].bytes, 2048);
    assert_eq!(summaries[0].missing, 1);
    assert_eq!(summaries[0].recordings, 2);
}

/// "Missing since Tuesday" is the useful fact. A run that re-stamped the mark
/// every time would answer "missing since a second ago" forever.
#[test]
fn a_missing_recording_keeps_the_moment_it_first_went_missing() {
    let library = Library::new("missing-since");
    library.add(
        &SessionFixture::new(
            "game-20260811-143205",
            "game",
            "Game",
            "2026-08-11T14:32:05+01:00",
        )
        .recording(1, "one.mkv", 4096)
        .ended("2026-08-11T15:31:21+01:00", "game-exited"),
    );
    let mut database = library.open();
    library.run(&mut database, observed_at());
    let gone = library.file("one.mkv");
    fs::remove_file(&gone).expect("the user deletes a recording");

    library.run(&mut database, observed_at());
    let first = column_of_recording(&database, &gone, "missing_since");
    let second_report = library.run(&mut database, observed_later());

    assert_eq!(
        column_of_recording(&database, &gone, "missing_since"),
        first,
        "the second run moved the date the recording went missing"
    );
    assert_eq!(
        second_report.recordings_newly_missing, 0,
        "the same loss was reported twice"
    );
}

/// The other half of the same rule, and the reason a missing recording is never
/// deleted: external drives come back.
#[test]
fn a_recording_that_comes_back_is_unmarked_and_measured_again() {
    let library = Library::new("returned");
    library.add(
        &SessionFixture::new(
            "game-20260811-143205",
            "game",
            "Game",
            "2026-08-11T14:32:05+01:00",
        )
        .recording(1, "one.mkv", 4096)
        .ended("2026-08-11T15:31:21+01:00", "game-exited"),
    );
    let mut database = library.open();
    library.run(&mut database, observed_at());
    let path = library.file("one.mkv");
    fs::remove_file(&path).expect("the user moves a recording away");
    library.run(&mut database, observed_at());
    assert!(column_of_recording(&database, &path, "missing_since").is_some());

    fs::write(&path, vec![0u8; 8192]).expect("the recording comes back, larger");
    let report = library.run(&mut database, observed_later());

    assert_eq!(report.recordings_returned, 1, "{report}");
    assert_eq!(column_of_recording(&database, &path, "missing_since"), None);
    assert_eq!(
        column_of_recording(&database, &path, "size_bytes").as_deref(),
        Some("8192"),
        "a recording that came back was not measured again"
    );
}

/// The case ingestion cannot see: the sidecar itself is gone, so nothing walks
/// past the recording it described. Without the second pass the row would stay
/// as it was for ever.
#[test]
fn a_recording_whose_session_record_has_also_gone_is_still_marked_missing() {
    let library = Library::new("orphan-row");
    let sidecar = library.add(
        &SessionFixture::new(
            "game-20260811-143205",
            "game",
            "Game",
            "2026-08-11T14:32:05+01:00",
        )
        .recording(1, "one.mkv", 4096)
        .ended("2026-08-11T15:31:21+01:00", "game-exited"),
    );
    let mut database = library.open();
    library.run(&mut database, observed_at());

    let path = library.file("one.mkv");
    fs::remove_file(&path).expect("the user deletes the recording");
    fs::remove_file(&sidecar).expect("and its session record with it");
    let report = library.run(&mut database, observed_later());

    assert_eq!(report.sidecars_found, 0, "{report}");
    assert_eq!(report.recordings_newly_missing, 1, "{report}");
    assert!(column_of_recording(&database, &path, "missing_since").is_some());
    assert_eq!(
        count(&database, "sessions"),
        1,
        "a session whose sidecar was deleted must keep its row: the file it \
         described may still exist elsewhere"
    );
}

/// The unplugged drive. Nothing under a root that could not be read is evidence
/// of anything, and marking a thousand recordings missing because a USB disk
/// was asleep is exactly the behaviour AGENTS.md section 56 forbids.
#[test]
fn nothing_under_a_root_that_could_not_be_reached_is_marked_missing() {
    let library = Library::new("unavailable-root");
    library.add(
        &SessionFixture::new(
            "game-20260811-143205",
            "game",
            "Game",
            "2026-08-11T14:32:05+01:00",
        )
        .recording(1, "one.mkv", 4096)
        .ended("2026-08-11T15:31:21+01:00", "game-exited"),
    );
    let mut database = library.open();
    library.run(&mut database, observed_at());
    let path = library.file("one.mkv");

    // The drive is not there any more: the whole root, files and sidecar with
    // it, cannot be reached.
    fs::remove_dir_all(&library.root).expect("the drive goes away");
    let report = library.run(&mut database, observed_later());

    assert_eq!(report.unavailable_roots.len(), 1, "{report}");
    assert_eq!(report.unavailable_roots[0].path, library.root);
    assert_eq!(
        report.recordings_newly_missing, 0,
        "a root that could not be read was treated as evidence that its files are gone"
    );
    assert_eq!(column_of_recording(&database, &path, "missing_since"), None);
    assert_eq!(count(&database, "recordings"), 1);
}

/// Reconciliation runs at every start-up, so a second run that changed anything
/// would be a library that never settles — and rows that change for no reason
/// are rows a synchronisation, a backup or a screen cannot cache.
#[test]
fn re_indexing_an_unchanged_library_changes_nothing() {
    let library = Library::new("idempotent");
    library.add(
        &SessionFixture::new(
            "counter-strike-2-20260811-143205",
            "counter-strike-2",
            "Counter-Strike 2",
            "2026-08-11T14:32:05+01:00",
        )
        .recording(1, "one.mkv", 4096)
        .event(
            json!({ "at": "2026-08-11T14:32:05+01:00", "event": "session-started",
                       "pid": 4242, "image_name": "cs2.exe" }),
        )
        .ended("2026-08-11T15:31:21+01:00", "game-exited"),
    );
    library.add(
        &SessionFixture::new(
            "dota-2-20260810-190000",
            "dota-2",
            "Dota 2",
            "2026-08-10T19:00:00+01:00",
        )
        .recording(1, "two.mkv", 512),
    );
    let mut database = library.open();
    library.run(&mut database, observed_at());
    let first = snapshot(&database);

    let report = library.run(&mut database, observed_later());
    let second = snapshot(&database);

    assert_eq!(second, first, "a second run rewrote the library");
    assert_eq!(report.recordings_newly_missing, 0);
    assert_eq!(report.recordings_returned, 0);
    assert_eq!(report.problems.len(), 0, "{:?}", report.problems);
}

/// The failure mode an upsert invites: writing every column on every run, so
/// that the user's own data is quietly replaced by what the recorder happened
/// to write. Favourites, tags and bookmarks are not the recorder's to say
/// anything about.
#[test]
fn re_indexing_does_not_lose_a_favourite_a_tag_or_a_bookmark() {
    let library = Library::new("user-data");
    library.add(
        &SessionFixture::new(
            "game-20260811-143205",
            "game",
            "Game",
            "2026-08-11T14:32:05+01:00",
        )
        .recording(1, "one.mkv", 4096)
        .ended("2026-08-11T15:31:21+01:00", "game-exited"),
    );
    let mut database = library.open();
    library.run(&mut database, observed_at());

    database
        .connection()
        .execute_batch(
            "UPDATE sessions SET favourited_at = '2026-08-11T16:00:00+01:00';\
             UPDATE recordings SET favourited_at = '2026-08-11T16:00:00+01:00';\
             INSERT INTO tags (name) VALUES ('ace');\
             INSERT INTO recording_tags (recording_id, tag_id) \
                 SELECT recording_id, 1 FROM recordings;\
             INSERT INTO bookmarks (recording_id, at_seconds, label, created_at) \
                 SELECT recording_id, 61.5, 'the shot', '2026-08-11T16:00:00+01:00' \
                 FROM recordings;",
        )
        .expect("the user favourites, tags and bookmarks their recording");

    library.run(&mut database, observed_later());

    let favourited_sessions: i64 = database
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE favourited_at IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .expect("sessions can be counted");
    assert_eq!(favourited_sessions, 1, "re-indexing unfavourited a session");
    let favourited_recordings: i64 = database
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM recordings WHERE favourited_at IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .expect("recordings can be counted");
    assert_eq!(
        favourited_recordings, 1,
        "re-indexing unfavourited a recording"
    );
    assert_eq!(
        count(&database, "recording_tags"),
        1,
        "re-indexing lost a tag"
    );
    assert_eq!(
        count(&database, "bookmarks"),
        1,
        "re-indexing lost a bookmark"
    );

    let summaries = game_summaries(&database).expect("the games view can be built");
    assert_eq!(summaries[0].favourites, 2);
}

/// One bad file is one bad file. A library of three hundred sessions must not
/// be lost to a truncated one, and the user has to be told which file it was.
#[test]
fn an_unreadable_session_record_does_not_cost_the_rest_of_the_library() {
    let library = Library::new("bad-sidecar");
    library.add(
        &SessionFixture::new(
            "good-20260811-143205",
            "game",
            "Game",
            "2026-08-11T14:32:05+01:00",
        )
        .recording(1, "one.mkv", 4096)
        .ended("2026-08-11T15:31:21+01:00", "game-exited"),
    );
    let broken = library.root.join("clipped-broken.session.json");
    fs::write(&broken, r#"{ "schema_version": 1, "session_id": "half"#)
        .expect("a truncated sidecar can be written");
    let mut database = library.open();

    let report = library.run(&mut database, observed_at());

    assert_eq!(report.sidecars_found, 2, "{report}");
    assert_eq!(report.sessions_indexed, 1, "{report}");
    match report.problems.as_slice() {
        [IndexProblem::MalformedSidecar { path, .. }] => assert_eq!(path, &broken),
        other => panic!("expected one malformed sidecar, got {other:?}"),
    }
    assert!(
        report.problems[0].to_string().contains("clipped-broken"),
        "the message has to name the file: {}",
        report.problems[0]
    );
    assert_eq!(count(&database, "sessions"), 1);
}

/// An old build must not half-read a file a new one wrote, and must not damage
/// it either. Re-indexing after an update picks it up.
#[test]
fn a_session_record_from_a_newer_recorder_is_left_for_a_newer_build() {
    let library = Library::new("newer-sidecar");
    let path = library.root.join("clipped-future.session.json");
    let future = json!({
        "schema_version": 99,
        "session_id": "future-20260811-143205",
        "game": { "kind": "known", "game_id": "game", "name": "Game" },
        "started_at": "2026-08-11T14:32:05+01:00",
    });
    fs::write(&path, future.to_string()).expect("a sidecar from the future can be written");
    let mut database = library.open();

    let report = library.run(&mut database, observed_at());

    match report.problems.as_slice() {
        [IndexProblem::UnsupportedSidecarVersion { found, .. }] => assert_eq!(*found, 99),
        other => panic!("expected an unsupported version, got {other:?}"),
    }
    assert_eq!(count(&database, "sessions"), 0);
    assert_eq!(
        serde_json::from_str::<Value>(&fs::read_to_string(&path).expect("it is still there"))
            .expect("still JSON"),
        future,
        "a file this build does not understand was modified"
    );
}

/// `recordings.path` is unique because one file cannot be two recordings. The
/// session that loses the race must still be indexed, and the recording that
/// could not be written must be named.
#[test]
fn two_sessions_claiming_one_file_lose_only_the_recording_that_could_not_be_written() {
    let library = Library::new("duplicate-path");
    library.add(
        &SessionFixture::new(
            "first-20260811-143205",
            "game",
            "Game",
            "2026-08-11T14:32:05+01:00",
        )
        .recording(1, "shared.mkv", 4096)
        .ended("2026-08-11T15:31:21+01:00", "game-exited"),
    );
    library.add(
        &SessionFixture::new(
            "second-20260811-163205",
            "game",
            "Game",
            "2026-08-11T16:32:05+01:00",
        )
        .recording(1, "shared.mkv", 4096)
        .recording(2, "its-own.mkv", 1024)
        .ended("2026-08-11T17:31:21+01:00", "game-exited"),
    );
    let mut database = library.open();

    let report = library.run(&mut database, observed_at());

    assert_eq!(report.sessions_indexed, 2, "{report}");
    assert_eq!(count(&database, "sessions"), 2);
    match report.problems.as_slice() {
        [IndexProblem::RecordingRefused { path, .. }] => {
            assert_eq!(path, &library.file("shared.mkv"));
        }
        other => panic!("expected one refused recording, got {other:?}"),
    }
    assert_eq!(
        count(&database, "recordings"),
        2,
        "the second session's own recording should still be indexed"
    );
}

/// A media file with no session record cannot be attributed to a game without
/// inventing the answer, and an index that guessed would file somebody's
/// footage under a game they were not playing (AGENTS.md section 27).
#[test]
fn media_with_no_session_record_is_reported_and_never_invented() {
    let library = Library::new("unindexed-media");
    library.add(
        &SessionFixture::new(
            "game-20260811-143205",
            "game",
            "Game",
            "2026-08-11T14:32:05+01:00",
        )
        .recording(1, "one.mkv", 4096)
        .ended("2026-08-11T15:31:21+01:00", "game-exited"),
    );
    fs::write(library.file("holiday.mp4"), [0u8; 16]).expect("a file of the user's own");
    fs::write(library.file("clipped-orphan.mkv"), [0u8; 16])
        .expect("a recording whose sidecar was lost");
    let mut database = library.open();

    let report = library.run(&mut database, observed_at());

    assert_eq!(report.unindexed_media, 2, "{report}");
    assert_eq!(
        report.unindexed_sample,
        [
            library.file("clipped-orphan.mkv"),
            library.file("holiday.mp4")
        ]
    );
    assert_eq!(
        count(&database, "recordings"),
        1,
        "a row was invented for a file no session claims"
    );
    assert_eq!(count(&database, "games"), 1);
}

/// The catalogue refuses to guess between two games that match equally well, and
/// so does the index: the session is filed under no game, with every candidate
/// kept (`docs/sessions.md`).
#[test]
fn an_unattributed_session_keeps_every_candidate_and_claims_no_game() {
    let library = Library::new("ambiguous");
    let mut session = SessionFixture::new(
        "unattributed-20260811-143205",
        "ignored",
        "Ignored",
        "2026-08-11T14:32:05+01:00",
    )
    .recording(1, "one.mkv", 4096)
    .ended("2026-08-11T15:31:21+01:00", "game-exited");
    session.game = json!({ "kind": "ambiguous", "candidates": ["half-life-2", "team-fortress-2"] });
    library.add(&session);
    let mut database = library.open();

    library.run(&mut database, observed_at());

    let game_id: Option<String> = database
        .connection()
        .query_row("SELECT game_id FROM sessions", [], |row| row.get(0))
        .expect("the session is indexed");
    assert_eq!(
        game_id, None,
        "the index guessed a game the catalogue would not"
    );
    assert_eq!(count(&database, "games"), 0);

    let mut statement = database
        .connection()
        .prepare("SELECT game_id FROM session_game_candidates ORDER BY game_id")
        .expect("the candidates can be read");
    let candidates: Vec<String> = statement
        .query_map([], |row| row.get(0))
        .expect("the query runs")
        .map(|candidate| candidate.expect("a candidate"))
        .collect();
    assert_eq!(candidates, ["half-life-2", "team-fortress-2"]);

    // The unattributed sessions are still a group a screen can show.
    let summaries = game_summaries(&database).expect("the games view can be built");
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].game_id, None);
    assert_eq!(summaries[0].sessions, 1);
    assert_eq!(summaries[0].recordings, 1);
}

/// `session_events` is the vocabulary the sidecar already writes, and the point
/// of storing the rest of an event as JSON is that an event kind from a later
/// recorder still arrives with everything it carried.
#[test]
fn the_events_of_a_session_reach_the_index_with_everything_they_carried() {
    let library = Library::new("events");
    library.add(
        &SessionFixture::new(
            "game-20260811-143205",
            "game",
            "Game",
            "2026-08-11T14:32:05+01:00",
        )
        .event(
            json!({ "at": "2026-08-11T14:32:05+01:00", "event": "session-started",
                           "pid": 4242, "image_name": "cs2.exe" }),
        )
        .event(
            json!({ "at": "2026-08-11T14:40:00+01:00", "event": "system-resumed",
                           "gap_seconds": 28_800 }),
        )
        .event(
            json!({ "at": "2026-08-11T14:45:00+01:00", "event": "a-later-recorders-event",
                           "something": "this build has never heard of" }),
        )
        .ended("2026-08-11T15:31:21+01:00", "game-exited"),
    );
    let mut database = library.open();

    library.run(&mut database, observed_at());

    assert_eq!(count(&database, "session_events"), 4);
    let detail: String = database
        .connection()
        .query_row(
            "SELECT detail FROM session_events WHERE kind = 'session-started'",
            [],
            |row| row.get(0),
        )
        .expect("the event is indexed");
    let detail: Value = serde_json::from_str(&detail).expect("the detail is JSON");
    assert_eq!(detail["pid"], json!(4242));
    assert_eq!(detail["image_name"], json!("cs2.exe"));

    let unknown: String = database
        .connection()
        .query_row(
            "SELECT detail FROM session_events WHERE kind = 'a-later-recorders-event'",
            [],
            |row| row.get(0),
        )
        .expect("an event this build does not know is still indexed");
    assert_eq!(
        serde_json::from_str::<Value>(&unknown).expect("the detail is JSON")["something"],
        json!("this build has never heard of"),
        "an event from a later recorder lost what it was carrying"
    );
}

/// A word outside the schema's vocabulary would be refused by a `CHECK`
/// constraint and take the whole session with it. Losing one column is much
/// better than losing a sitting.
#[test]
fn a_word_the_schema_does_not_know_costs_a_column_and_not_the_session() {
    let library = Library::new("unknown-token");
    let path = library.root.join("clipped-odd.session.json");
    fs::write(library.file("one.mkv"), [0u8; 32]).expect("a recording");
    let session = json!({
        "schema_version": 1,
        "session_id": "game-20260811-143205",
        "game": { "kind": "known", "game_id": "game", "name": "Game" },
        "started_at": "2026-08-11T14:32:05+01:00",
        "ended_at": "2026-08-11T15:31:21+01:00",
        "recordings": [{
            "index": 1,
            "output": library.file("one.mkv").display().to_string(),
            "started_at": "2026-08-11T14:32:05+01:00",
            "ended_at": "2026-08-11T15:31:21+01:00",
            "outcome": "recorded-in-a-way-this-build-has-not-heard-of",
        }],
        "events": [{ "at": "2026-08-11T15:31:21+01:00", "event": "session-ended",
                     "reason": "abducted-by-aliens" }],
    });
    fs::write(&path, session.to_string()).expect("the sidecar can be written");
    let mut database = library.open();

    let report = library.run(&mut database, observed_at());

    assert_eq!(report.sessions_indexed, 1, "{report}");
    assert_eq!(report.recordings_indexed, 1, "{report}");
    assert_eq!(report.problems.len(), 2, "{:?}", report.problems);
    let outcome: Option<String> =
        column_of_recording(&database, &library.file("one.mkv"), "outcome");
    assert_eq!(outcome, None, "an unknown word was written into the index");
    let size = column_of_recording(&database, &library.file("one.mkv"), "size_bytes");
    assert_eq!(
        size.as_deref(),
        Some("32"),
        "the rest of the recording should still be indexed"
    );
}

/// The two words `clipped-recorder recover` writes are not "the schema does
/// not know" words, unlike the one above. `crates/storage/migrations/
/// 0006_recovered_recording_outcomes.sql` widened the same CHECK constraint
/// `RECORDING_OUTCOMES` (`crates/library/src/index/ingest.rs`) lists, so a
/// recovered fragment's outcome has to survive being reconciled -- not merely
/// be accepted by the Rust-side list while the database still refuses the
/// word underneath it (issue #451). Both directions are asserted: no
/// `IndexProblem`, and the column actually holds the word rather than having
/// degraded to `NULL`.
#[test]
fn a_recovered_recordings_outcome_is_indexed_without_a_problem() {
    for outcome in ["interrupted", "discarded"] {
        let library = Library::new(&format!("recovered-outcome-{outcome}"));
        let path = library.root.join("clipped-cs2.session.json");
        fs::write(library.file("one.mkv"), [0u8; 32]).expect("a recording");
        let session = json!({
            "schema_version": 1,
            "session_id": "cs2-20260811-143205",
            "game": { "kind": "known", "game_id": "cs2", "name": "Counter-Strike 2" },
            "started_at": "2026-08-11T14:32:05+01:00",
            "ended_at": "2026-08-11T14:40:00+01:00",
            "recordings": [{
                "index": 1,
                "output": library.file("one.mkv").display().to_string(),
                "started_at": "2026-08-11T14:32:05+01:00",
                "ended_at": "2026-08-11T14:40:00+01:00",
                "outcome": outcome,
            }],
            "events": [{ "at": "2026-08-11T14:40:00+01:00", "event": "recording-ended",
                         "index": 1, "outcome": outcome }],
        });
        fs::write(&path, session.to_string()).expect("the sidecar can be written");
        let mut database = library.open();

        let report = library.run(&mut database, observed_at());

        assert!(
            report.problems.is_empty(),
            "{outcome} should be a known word: {:?}",
            report.problems
        );
        assert_eq!(
            column_of_recording(&database, &library.file("one.mkv"), "outcome").as_deref(),
            Some(outcome),
            "the word should round-trip into the index rather than degrade to NULL"
        );
    }
}

/// Cancelling has to be prompt and has to be safe: what was committed stays
/// committed, and the next run carries on rather than starting again.
#[test]
fn a_cancelled_run_keeps_what_it_had_already_written() {
    let library = Library::new("cancelled");
    for index in 0..40 {
        library.add(
            &SessionFixture::new(
                &format!("game-20260811-{index:06}"),
                "game",
                "Game",
                "2026-08-11T14:32:05+01:00",
            )
            .recording(1, &format!("recording-{index}.mkv"), 16)
            .ended("2026-08-11T15:31:21+01:00", "game-exited"),
        );
    }
    let mut database = library.open();
    let mut settings = library.settings();
    // One session per transaction and a pause between them, so that the run is
    // long enough to be stopped in the middle of and not only before it starts.
    settings.pace = IndexPace {
        batch: 1,
        page: 8,
        rest: Duration::from_millis(20),
    };
    let reader = Database::open_read_only(&library.database).expect("a reader can open it");
    let control = IndexControl::new();

    let stop = control.clone();
    let watcher = std::thread::spawn(move || {
        // Cancel as soon as there is evidence that something has been
        // committed, which is what makes "keeps what it had written" a
        // statement about a run that was actually interrupted.
        loop {
            let sessions: i64 = reader
                .connection()
                .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
                .expect("a reader is never blocked");
            if sessions > 0 {
                stop.cancel();
                return sessions;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    });
    let report = reconcile(&mut database, &settings, &control, observed_at())
        .expect("a cancelled run is not a failure");
    let committed_when_cancelled = watcher.join().expect("the watching thread does not panic");

    assert!(report.cancelled, "the run was not stopped: {report}");
    assert!(
        report.sessions_indexed < 40,
        "the run was cancelled and still indexed everything: {report}"
    );
    assert!(committed_when_cancelled > 0);
    assert_eq!(
        count(&database, "sessions") as usize,
        report.sessions_indexed,
        "a cancelled run lost sessions it had already committed"
    );

    // And the next run finishes the job rather than starting again.
    let finished = library.run(&mut database, observed_later());
    assert_eq!(count(&database, "sessions"), 40, "{finished}");
}

/// AGENTS.md section 20 in its strongest available form: a screen reading the
/// library is not blocked while a reconciliation writes to it. Write-ahead
/// logging is what makes that true, and short transactions are what keep it
/// true when the writer is busy for a long time.
#[test]
fn a_library_screen_can_read_while_a_reconciliation_writes() {
    let library = Library::new("concurrent-reader");
    for index in 0..300 {
        library.add(
            &SessionFixture::new(
                &format!("game-20260811-{index:06}"),
                "game",
                "Game",
                "2026-08-11T14:32:05+01:00",
            )
            .recording(1, &format!("recording-{index}.mkv"), 16)
            .ended("2026-08-11T15:31:21+01:00", "game-exited"),
        );
    }
    let mut database = library.open();
    // The reader needs the database to exist and be migrated before it can open
    // it, which is the recorder's job and has just happened.
    let reader = Database::open_read_only(&library.database).expect("a reader can open it");

    let settings = IndexSettings::new([library.root.clone()]);
    let control = IndexControl::new();
    let indexing = std::thread::spawn(move || {
        reconcile(&mut database, &settings, &control, observed_at()).expect("indexing completes")
    });

    let mut reads = 0u32;
    let mut slowest = Duration::ZERO;
    while !indexing.is_finished() {
        let started = Instant::now();
        let sessions: i64 = reader
            .connection()
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
            .expect("a reader is never refused while a reconciliation writes");
        slowest = slowest.max(started.elapsed());
        assert!(sessions >= 0);
        reads += 1;
    }
    let report = indexing.join().expect("the indexing thread does not panic");

    assert_eq!(report.sessions_indexed, 300, "{report}");
    assert!(
        reads > 10,
        "the reader managed only {reads} reads during a run of {:?}; it was being blocked",
        report.duration
    );
    // The property that keeps the recorder's own writes moving: the run is
    // committed in batches, so nothing waits for more than one of them. 300
    // sessions at the background pace is 19 transactions of sessions before the
    // pass over rows adds any of its own.
    assert!(
        report.transactions >= 19,
        "the run was not committed in batches, so anything else with a row to write \
         waited for all of it: {report}"
    );
    assert!(
        report.longest_transaction < report.duration,
        "one transaction covered the whole run: {report}"
    );
    println!(
        "indexed 300 sessions in {:?} ({} transactions, longest {:?}); \
         the reader completed {reads} queries, slowest {slowest:?}",
        report.duration, report.transactions, report.longest_transaction
    );
}

/// A game can be renamed in the catalogue between two sessions, and the newer
/// name is the one to show — but sidecars are ingested in whatever order the
/// walk met them, so "newer" has to mean the session, not the file.
#[test]
fn the_name_a_game_is_shown_under_is_the_one_its_most_recent_session_used() {
    let library = Library::new("renamed-game");
    library.add(
        &SessionFixture::new(
            "game-20260810-100000",
            "game",
            "Old Name",
            "2026-08-10T10:00:00+01:00",
        )
        .recording(1, "one.mkv", 16)
        .ended("2026-08-10T11:00:00+01:00", "game-exited"),
    );
    library.add(
        &SessionFixture::new(
            "game-20260811-100000",
            "game",
            "New Name",
            "2026-08-11T10:00:00+01:00",
        )
        .recording(1, "two.mkv", 16)
        .ended("2026-08-11T11:00:00+01:00", "game-exited"),
    );
    let mut database = library.open();

    library.run(&mut database, observed_at());

    let summaries = game_summaries(&database).expect("the games view can be built");
    assert_eq!(summaries[0].name.as_deref(), Some("New Name"));
    assert_eq!(
        summaries[0].first_seen_at.as_deref(),
        Some("2026-08-10T10:00:00+01:00"),
        "the first time a game was seen must not move forwards"
    );
    assert_eq!(
        summaries[0].last_played_at.as_deref(),
        Some("2026-08-11T11:00:00+01:00")
    );
    assert_eq!(summaries[0].sessions, 2);
}

/// A clip's file is reconciled the same way a recording's is. Nothing in this
/// build can create a clip (`docs/storage.md`), so the row is written by hand —
/// but the code path that judges it is the one #37's clips will meet.
#[test]
fn a_clip_whose_file_has_gone_is_marked_missing_too() {
    let library = Library::new("clips");
    library.add(
        &SessionFixture::new(
            "game-20260811-143205",
            "game",
            "Game",
            "2026-08-11T14:32:05+01:00",
        )
        .recording(1, "one.mkv", 4096)
        .ended("2026-08-11T15:31:21+01:00", "game-exited"),
    );
    let clip = library.file("clip.mkv");
    fs::write(&clip, [0u8; 64]).expect("a clip can be written");
    let mut database = library.open();
    library.run(&mut database, observed_at());
    database
        .connection()
        .execute(
            "INSERT INTO clips (session_id, path, created_at, size_bytes) \
             VALUES ('game-20260811-143205', ?1, '2026-08-11T15:00:00+01:00', 64)",
            [clip.display().to_string()],
        )
        .expect("a clip row can be written");

    fs::remove_file(&clip).expect("the user deletes the clip");
    let report = library.run(&mut database, observed_later());

    assert_eq!(report.clips_newly_missing, 1, "{report}");
    assert_eq!(count(&database, "clips"), 1, "a clip row was deleted");
    let missing: Option<String> = database
        .connection()
        .query_row("SELECT missing_since FROM clips", [], |row| row.get(0))
        .optional()
        .expect("the clip can be read")
        .flatten();
    assert!(missing.is_some(), "the deleted clip is not marked missing");

    let summaries = game_summaries(&database).expect("the games view can be built");
    assert_eq!(summaries[0].clips, 1);
    assert_eq!(
        summaries[0].bytes, 4096,
        "the space a missing clip is not occupying was counted"
    );
}
