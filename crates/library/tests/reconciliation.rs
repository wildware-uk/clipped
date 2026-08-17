//! What re-indexing a session must not do to it.
//!
//! `clipped_library::index::ingest` states the rule it is held to, as a table
//! of "the two authorities": the sidecar answers which game, when and which
//! files; the filesystem answers whether a file is there and how large; and
//! **the user answers favourites, tags, bookmarks and what is in the trash**.
//! Ingestion writes the first two and never touches the third.
//!
//! That is not a stylistic rule. An upsert that wrote every column would
//! silently unfavourite a session on the next reconciliation, and the user
//! would have no way to tell what had happened (AGENTS.md section 56).
//!
//! # Why this file exists
//!
//! Both `ingest` and `presence` cited a file of this name as the thing checking
//! it. **No file of this name has ever existed** — the same shape of gap as the
//! sidecar version guard `sidecars.rs` describes, and found the same way: by
//! going to run the test that was named and finding nothing there.
//!
//! It matters more than a missing test usually does, because the property is
//! invisible when it breaks. Nothing fails, no problem is reported, and a
//! favourite the user set last week is simply gone the next time a directory is
//! scanned.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use clipped_library::index::{
    list_sessions, reconcile, IndexControl, IndexPace, IndexSettings, SessionListing,
};
use clipped_storage::Database;

mod support;

fn observed_at() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_786_545_000)
}

/// See `crates/library/tests/support/mod.rs`. Bind the answer to a variable that outlives the
/// test body: the directory goes when it does.
fn scratch_directory(name: &str) -> support::Scratch {
    support::Scratch::new(&format!("reconciliation-{name}"))
}

/// A session with one recording, one saved replay and one generated highlight.
///
/// The highlight is the interesting one: it has no `path`, because nothing has
/// exported it, and its identity is therefore its origin rather than a file.
fn sidecar(root: &Path) -> String {
    let clips = root.display().to_string().replace('\\', "\\\\");
    format!(
        r#"{{
            "schema_version": 2,
            "session_id": "counter-strike-2-20260811-143205",
            "game": {{ "kind": "known", "game_id": "counter-strike-2", "name": "Counter-Strike 2" }},
            "started_at": "2026-08-11T14:32:05+01:00",
            "ended_at": "2026-08-11T14:50:13+01:00",
            "recordings": [
                {{
                    "index": 1,
                    "output": "{clips}\\\\one.mkv",
                    "started_at": "2026-08-11T14:32:09+01:00",
                    "ended_at": "2026-08-11T14:50:13+01:00",
                    "outcome": "recorded",
                    "duration_seconds": 1084.0,
                    "starts_at_nanos": 0
                }}
            ],
            "clips": [
                {{
                    "path": "{clips}\\\\saved.mkv",
                    "created_at": "2026-08-11T14:41:52+01:00",
                    "source_recording": 1,
                    "source_start_seconds": 553.0,
                    "source_end_seconds": 583.0,
                    "duration_seconds": 30.0
                }},
                {{
                    "created_at": "2026-08-11T14:45:00+01:00",
                    "source_recording": 1,
                    "duration_seconds": 12.0,
                    "edit": "{{\"version\":1}}",
                    "origin": "highlight",
                    "origin_detail": "{{\"kind\":\"kill\",\"at\":600000000000,\"source\":\"acme-cs2\"}}"
                }}
            ],
            "events": []
        }}"#
    )
}

/// Writes the session and the files it names, and indexes the directory.
fn index(root: &Path, database: &mut Database) {
    fs::write(root.join("one.mkv"), [0u8; 128]).expect("the recording is written");
    fs::write(root.join("saved.mkv"), [0u8; 64]).expect("the saved replay is written");
    fs::write(
        root.join("clipped-counter-strike-2-20260811-143205.session.json"),
        sidecar(root),
    )
    .expect("the sidecar is written");

    let mut settings = IndexSettings::new([root.to_path_buf()]);
    settings.pace = IndexPace::foreground();
    let report = reconcile(database, &settings, &IndexControl::new(), observed_at())
        .expect("reconciliation completes");
    assert!(
        report.problems.is_empty(),
        "the session could not be indexed cleanly: {:?}",
        report.problems
    );
}

/// How many clips there are, and how many of them are favourited.
fn clips(database: &Database) -> (i64, i64) {
    database
        .connection()
        .query_row(
            "SELECT COUNT(*), COUNT(favourited_at) FROM clips",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("the clips can be counted")
}

#[test]
fn re_indexing_keeps_what_the_user_did_and_does_not_duplicate_a_clip_with_no_file() {
    let directory = scratch_directory("twice");
    let mut database = Database::open(directory.join("library.db")).expect("a database");

    index(&directory, &mut database);
    assert_eq!(
        clips(&database),
        (2, 0),
        "the saved replay and the generated highlight are both indexed"
    );

    // What the user did: favourite both, and name the highlight. Nothing else
    // in the system writes these three columns.
    database
        .connection()
        .execute(
            "UPDATE clips SET favourited_at = '2026-08-12T10:00:00+01:00', \
             title = COALESCE(title, 'Ace on Mirage')",
            [],
        )
        .expect("the user's marks are written");

    index(&directory, &mut database);

    let (count, favourited) = clips(&database);
    assert_eq!(
        count, 2,
        "re-indexing the same session produced a second copy of a clip — the clip with no file \
         has no path to be matched on, and matching it by its origin is what stops this"
    );
    assert_eq!(
        favourited, 2,
        "re-indexing unfavourited a clip: the sidecar does not know about favourites and must \
         not be able to clear one (AGENTS.md section 56)"
    );

    let title: String = database
        .connection()
        .query_row("SELECT title FROM clips WHERE path IS NULL", [], |row| {
            row.get(0)
        })
        .expect("the highlight is still there");
    assert_eq!(
        title, "Ace on Mirage",
        "re-indexing overwrote a title the user chose with the sidecar's absence of one"
    );
}

#[test]
fn a_generated_highlight_keeps_its_document_and_why_it_exists() {
    let directory = scratch_directory("highlight-columns");
    let mut database = Database::open(directory.join("library.db")).expect("a database");

    index(&directory, &mut database);

    let (path, edit, origin, detail): (Option<String>, String, String, String) = database
        .connection()
        .query_row(
            "SELECT path, edit, origin, origin_detail FROM clips WHERE origin = 'highlight'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("the highlight was indexed");

    assert_eq!(path, None, "a clip nothing has exported was given a file");
    assert_eq!(edit, r#"{"version":1}"#);
    assert_eq!(origin, "highlight");
    assert!(
        detail.contains("acme-cs2"),
        "what caused the clip is what identifies it, and it did not survive: {detail}"
    );
}

#[test]
fn a_saved_replay_is_still_filed_as_one_and_still_found_by_its_file() {
    // The other half: the change that gave a clip a second natural key must not
    // have moved the first one. A clip with a file is identified by it, which is
    // what `clips.path` being UNIQUE is for.
    let directory = scratch_directory("saved-replay");
    let mut database = Database::open(directory.join("library.db")).expect("a database");

    index(&directory, &mut database);

    let (origin, source): (String, Option<i64>) = database
        .connection()
        .query_row(
            "SELECT origin, source_recording_id FROM clips WHERE path IS NOT NULL",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("the saved replay was indexed");

    assert_eq!(
        origin, "replay-buffer",
        "a clip with a file and no stated origin is a saved replay, because until clips with no \
         file could be stored that is the only kind there was"
    );
    assert!(
        source.is_some(),
        "the recording it was cut from was not linked"
    );
}

#[test]
fn a_clip_with_no_file_is_listed_rather_than_failing_the_listing() {
    // The read path, not the SQL. Every other test here asks the database for
    // the pathless clip's columns directly, which is exactly why nothing caught
    // that `clips_of` read `path` into a `String`: a `NULL` there is an error
    // in `rusqlite`, not an empty string, and it fails the whole call rather
    // than the one clip (issue #591). A library screen then shows an error
    // instead of a library, for a highlight the application generated itself.
    let directory = scratch_directory("listing");
    let mut database = Database::open(directory.join("library.db")).expect("a database");

    index(&directory, &mut database);

    let page = list_sessions(
        &database,
        &SessionListing {
            limit: 10,
            after: None,
            query: None,
        },
    )
    .expect(
        "listing a sitting with a clip nothing has exported failed the whole call — a clip with \
         no file is a clip the user made, and it must not cost them their library (AGENTS.md \
         section 56)",
    );

    let session = page
        .sessions
        .first()
        .expect("the sitting is in the listing");

    // Nothing else in the sitting may be lost by tolerating the pathless clip.
    assert_eq!(
        session.recordings.len(),
        1,
        "the recording went missing from the listing"
    );
    assert_eq!(
        session.clips.len(),
        2,
        "a clip with no file is a clip somebody made and must be listed, not filtered away: \
         {:?}",
        session.clips
    );

    let saved = session
        .clips
        .iter()
        .find(|clip| clip.path.is_some())
        .expect("the saved replay still carries its file");
    assert!(
        saved
            .path
            .as_deref()
            .is_some_and(|path| path.ends_with("saved.mkv")),
        "the saved replay's file was not the one the sidecar named: {:?}",
        saved.path
    );

    let highlight = session
        .clips
        .iter()
        .find(|clip| clip.path.is_none())
        .expect("the generated highlight is listed, with no file rather than no clip");
    assert_eq!(
        highlight.missing_since, None,
        "a clip nothing has exported has no file to have gone, and must not be shown as one \
         whose file was lost"
    );
}
