//! The separation between the metadata database and the media it refers to.
//!
//! AGENTS.md section 17: *a metadata database failure should not corrupt video
//! files*. That is a claim about what this crate can reach, so this test drives
//! it from outside — through the public API only, the way the recorder will —
//! and puts a recording file in the same directory as the database while every
//! failure the crate has is provoked in turn.
//!
//! The recording is a file of known bytes rather than real media. What is being
//! tested is whether anything touches it at all, and a byte comparison answers
//! that more sharply than a container check: a file that has been opened for
//! writing and closed again is still a valid MKV.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use clipped_storage::rusqlite::Connection;
use clipped_storage::{Database, StorageError, WriteSettings, Writer};

/// The bytes standing in for a recording nobody can make again.
const RECORDING: &[u8] = b"not really Matroska, but it is the user's and it is irreplaceable";

/// A scratch directory of this test's own.
///
/// Duplicated from the crate's internal test support rather than shared with
/// it: `#[cfg(test)]` modules are not visible to an integration test, and a
/// twelve-line helper is not worth a public API that exists for tests
/// (AGENTS.md section 44).
fn scratch_directory(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "clipped-storage-separation-{}-{name}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).expect("a scratch directory can be created");
    directory
}

/// Everything about the recording that would change if something wrote to it.
fn recording_state(path: &Path) -> (Vec<u8>, SystemTime) {
    let bytes = fs::read(path).expect("the recording can be read");
    let modified = fs::metadata(path)
        .expect("the recording can be inspected")
        .modified()
        .expect("the platform reports a modification time");
    (bytes, modified)
}

fn write_recording(directory: &Path) -> PathBuf {
    let path = directory.join("clipped-counter-strike-2-20260811-143205.mkv");
    fs::write(&path, RECORDING).expect("the recording can be written");
    path
}

#[test]
fn no_database_failure_touches_the_recording_beside_it() {
    let directory = scratch_directory("failures");
    let recording = write_recording(&directory);
    let before = recording_state(&recording);
    // Windows records modification times with enough resolution that an
    // immediate rewrite would be visible, but not so much that a comparison is
    // safe without a moment's separation.
    std::thread::sleep(Duration::from_millis(20));

    // 1. A database that belongs to something else, at the path Clipped was
    //    given. Refused, and neither file is written to.
    let foreign = directory.join("someone-elses.db");
    {
        let other = Connection::open(&foreign).expect("a database can be created");
        other
            .execute_batch("CREATE TABLE important (id INTEGER PRIMARY KEY);")
            .expect("it can be filled");
    }
    match Database::open(&foreign) {
        Err(StorageError::NotAClippedDatabase { .. }) => {}
        other => panic!("expected a foreign database to be refused, got {other:?}"),
    }
    assert_eq!(
        recording_state(&recording),
        before,
        "after a foreign database"
    );

    // 2. A database written by a newer build.
    let library = directory.join("library.db");
    {
        let database = Database::open(&library).expect("a database can be created");
        database
            .connection()
            .execute(
                "INSERT INTO schema_migrations (version, name, checksum, applied_at) \
                 VALUES (9999, 'from-the-future', 'ffffffffffffffff', '2030-01-01T00:00:00Z')",
                [],
            )
            .expect("a future version can be written for the test");
    }
    match Database::open(&library) {
        Err(StorageError::FromNewerVersion { .. }) => {}
        other => panic!("expected a newer database to be refused, got {other:?}"),
    }
    assert_eq!(
        recording_state(&recording),
        before,
        "after a newer database"
    );

    // 3. A database file that is not a database at all.
    let corrupt = directory.join("corrupt.db");
    fs::write(
        &corrupt,
        b"this is not an SQLite file, it is a hundred and one bytes of nothing",
    )
    .expect("the corrupt file can be written");
    assert!(
        Database::open(&corrupt).is_err(),
        "a corrupt database was opened without complaint"
    );
    assert_eq!(
        recording_state(&recording),
        before,
        "after a corrupt database"
    );

    // 4. Writes that fail against a working database: a row that refers to a
    //    game that is not there, through the queue a recording thread would
    //    use.
    let working = directory.join("working.db");
    let database = Database::open(&working).expect("a database can be created");
    let writer = Writer::spawn(database, WriteSettings::default());
    writer
        .queue()
        .submit("orphan", |connection| {
            connection.execute(
                "INSERT INTO sessions (session_id, game_id, started_at) \
                 VALUES ('x-20260811-143205', 'never-played', '2026-08-11T14:32:05+01:00')",
                [],
            )?;
            Ok(())
        })
        .expect("it queues");
    let stats = writer.stop().expect("the writer stops");
    assert_eq!(stats.failed, 1, "the write should have failed: {stats:?}");
    assert_eq!(recording_state(&recording), before, "after a failed write");

    // 5. Deleting the index outright. This is the worst case — the user's
    //    library is gone — and it is still only an index: the recording is
    //    where it was, and a new database opens beside it.
    fs::remove_file(&working).expect("the database can be deleted");
    let replacement = Database::open(&working).expect("a database can be created again");
    assert_eq!(
        replacement.schema_version(),
        clipped_storage::SCHEMA_VERSION
    );
    assert_eq!(
        recording_state(&recording),
        before,
        "after losing the database"
    );

    assert_eq!(
        fs::read(&recording).expect("the recording is still there"),
        RECORDING,
        "the recording's bytes changed"
    );
}

#[test]
fn a_recording_is_a_path_and_facts_about_it_and_nothing_more() {
    // The other half of the separation: what a recording *is* in this schema.
    // If a row could ever hold the media itself, everything above would be a
    // test of the wrong thing (AGENTS.md section 31 forbids media blobs).
    let directory = scratch_directory("no-blobs");
    let recording = write_recording(&directory);
    let database = Database::open(directory.join("library.db")).expect("a database");

    database
        .connection()
        .execute_batch(
            "INSERT INTO games (game_id, name, first_seen_at) \
             VALUES ('counter-strike-2', 'Counter-Strike 2', '2026-08-11T14:32:05+01:00');\
             INSERT INTO sessions (session_id, game_id, started_at) \
             VALUES ('counter-strike-2-20260811-143205', 'counter-strike-2', \
                     '2026-08-11T14:32:05+01:00');",
        )
        .expect("a game and a session can be written");
    database
        .connection()
        .execute(
            "INSERT INTO recordings (session_id, session_index, path, started_at, size_bytes) \
             VALUES ('counter-strike-2-20260811-143205', 1, ?1, '2026-08-11T14:32:09+01:00', ?2)",
            clipped_storage::rusqlite::params![
                recording.to_string_lossy(),
                i64::try_from(RECORDING.len()).expect("it fits")
            ],
        )
        .expect("the recording can be indexed");

    // No column in the schema is a BLOB. Every table is STRICT, so a declared
    // type is the type, and this is a statement about the whole schema rather
    // than about the tables somebody remembered to check.
    let mut statement = database
        .connection()
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")
        .expect("the tables can be listed");
    let tables: Vec<String> = statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("the tables can be read")
        .map(|name| name.expect("a table name"))
        .collect();
    for table in &tables {
        let mut columns = database
            .connection()
            .prepare(&format!("PRAGMA table_info({table})"))
            .expect("the columns can be listed");
        let types: Vec<String> = columns
            .query_map([], |row| row.get::<_, String>(2))
            .expect("the column types can be read")
            .map(|kind| kind.expect("a column type"))
            .collect();
        assert!(
            !types.iter().any(|kind| kind.eq_ignore_ascii_case("BLOB")),
            "{table} has a BLOB column, so media could be stored in the database"
        );
    }

    // And the database is a small fraction of the media it indexes, which is
    // the observable consequence of that.
    let indexed: String = database
        .connection()
        .query_row("SELECT path FROM recordings", [], |row| row.get(0))
        .expect("the row can be read");
    assert_eq!(Path::new(&indexed), recording);
}
