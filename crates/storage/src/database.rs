//! Opening the database, and the connection settings that make the
//! recorder-writes/UI-reads model work.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, Transaction};
use tracing::warn;

use crate::error::StorageError;
use crate::migrations::{self, MigrationOutcome, MIGRATIONS, SCHEMA_VERSION};

/// The value Clipped stamps into an SQLite file header to claim it.
///
/// `CLPD` as four bytes. Every SQLite database carries this field and it is zero
/// unless somebody set it, so it is what lets [`Database::open`] tell "a file
/// Clipped made" from "somebody else's database at a path we were handed by
/// mistake" — and refuse the second rather than writing a schema over it.
pub const APPLICATION_ID: i32 = 0x434C_5044;

/// How long a connection waits for another one to finish writing before giving
/// up.
///
/// Five seconds. Write-ahead logging means a reader never waits for a writer at
/// all, so this only applies to two writers, and the only way that happens is a
/// second Clipped process during an upgrade or a shutdown. Five seconds is long
/// enough to ride out a checkpoint and short enough that a stuck process is a
/// visible error rather than a hang.
const BUSY_TIMEOUT_MILLISECONDS: u32 = 5_000;

/// An open Clipped database.
///
/// # Concurrency
///
/// **One writer, any number of readers, across processes.** The recorder owns
/// the writing connection; the desktop application reads through
/// [`Database::open_read_only`]. That works because the database is opened in
/// write-ahead logging mode, in which readers see a consistent snapshot while a
/// write is in progress instead of waiting for it, and the writer never waits
/// for a reader.
///
/// A [`Writer`](crate::Writer) is what keeps a thread that must not stall — the
/// recording loop — from waiting on any of this: it hands work to a queue and a
/// background thread commits it in batches.
///
/// # What this type does not do
///
/// It does not wrap SQL. Callers prepare their own statements against the schema
/// in `migrations/`, because a hand-written accessor per column would be a
/// second schema to keep in step with the first, and the crate that interprets
/// library data is `clipped-library` rather than this one. What this crate owns
/// is the file: where it is, what state it is in, and that it is never left
/// half-migrated.
#[derive(Debug)]
pub struct Database {
    connection: Connection,
    path: PathBuf,
    migration: MigrationOutcome,
}

impl Database {
    /// Opens the database at `path` for writing, creating and migrating it as
    /// needed.
    ///
    /// The parent directory is created if it does not exist. Callers choose the
    /// path: this crate is the bottom of the stack and does not read the
    /// environment, so the per-user location — `%LOCALAPPDATA%\Clipped` for
    /// everything else Clipped keeps — is resolved by the process that opens
    /// the database.
    ///
    /// # Errors
    ///
    /// [`StorageError::NotAClippedDatabase`] if the file is an SQLite database
    /// belonging to something else, [`StorageError::FromNewerVersion`] if it
    /// was written by a newer build, and the migration errors if the upgrade
    /// could not be completed. In each of those cases the file is left as it
    /// was found.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref().to_path_buf();

        if let Some(directory) = path.parent() {
            if !directory.as_os_str().is_empty() {
                fs::create_dir_all(directory).map_err(|source| StorageError::CreateDirectory {
                    directory: directory.to_path_buf(),
                    source,
                })?;
            }
        }

        let mut connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_URI,
        )
        .map_err(|source| StorageError::Open {
            path: path.clone(),
            source,
        })?;

        configure(&connection)?;
        enable_write_ahead_logging(&connection, &path)?;
        claim(&connection, &path, true)?;

        let migration = migrations::migrate(&mut connection, Some(&path), MIGRATIONS)?;

        Ok(Self {
            connection,
            path,
            migration,
        })
    }

    /// Opens the database at `path` for reading only.
    ///
    /// The connection cannot write: `PRAGMA query_only` refuses every statement
    /// that would. Readers never migrate — the process that owns writing does
    /// that, once — so a database that is not already at this build's schema
    /// version is reported rather than upgraded.
    ///
    /// The file is opened with write permission even though nothing may write
    /// through it, and that is deliberate. An `SQLITE_OPEN_READONLY` connection
    /// to a write-ahead-logging database cannot create the shared-memory index
    /// it needs, so it fails outright on a database whose `-shm` file is not
    /// already there — which is exactly the state a viewer meets when it opens
    /// the library before the recorder has. `query_only` gives the guarantee
    /// that flag was wanted for, without that failure mode.
    ///
    /// # Errors
    ///
    /// [`StorageError::MigrationRequired`] if the database is older than this
    /// build, [`StorageError::FromNewerVersion`] if it is newer, and
    /// [`StorageError::Open`] if there is no database there at all.
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref().to_path_buf();

        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_URI,
        )
        .map_err(|source| StorageError::Open {
            path: path.clone(),
            source,
        })?;

        configure(&connection)?;
        claim(&connection, &path, false)?;
        connection.pragma_update(None, "query_only", true)?;

        let found = migrations::schema_version(&connection)?;
        if found > SCHEMA_VERSION {
            return Err(StorageError::FromNewerVersion {
                path,
                found,
                supported: SCHEMA_VERSION,
            });
        }
        if found < SCHEMA_VERSION {
            return Err(StorageError::MigrationRequired {
                path,
                found,
                expected: SCHEMA_VERSION,
            });
        }

        Ok(Self {
            connection,
            path,
            migration: MigrationOutcome {
                from: found,
                to: found,
                applied: Vec::new(),
                backup: None,
            },
        })
    }

    /// Where this database lives.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The schema version the database is at, which is always
    /// [`SCHEMA_VERSION`](crate::SCHEMA_VERSION) once it is open.
    #[must_use]
    pub fn schema_version(&self) -> u32 {
        self.migration.to
    }

    /// What opening it did: which migrations ran, and where the copy taken
    /// before them was written.
    #[must_use]
    pub fn migration(&self) -> &MigrationOutcome {
        &self.migration
    }

    /// The underlying connection, for reading.
    #[must_use]
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// The underlying connection, for writing outside a transaction of this
    /// crate's making.
    #[must_use]
    pub fn connection_mut(&mut self) -> &mut Connection {
        &mut self.connection
    }

    /// Begins a transaction.
    ///
    /// Dropping it rolls back. Nothing in a library index is worth writing
    /// half of: a recording row without the session it belongs to is a row no
    /// query will ever return.
    ///
    /// # Errors
    ///
    /// Whatever SQLite said, usually another writer holding the database for
    /// longer than the busy timeout.
    pub fn transaction(&mut self) -> Result<Transaction<'_>, StorageError> {
        Ok(self.connection.transaction()?)
    }
}

/// The settings every connection gets, whichever way it was opened.
fn configure(connection: &Connection) -> Result<(), StorageError> {
    connection.busy_timeout(std::time::Duration::from_millis(u64::from(
        BUSY_TIMEOUT_MILLISECONDS,
    )))?;

    // Referential integrity is off by default in SQLite, per connection, which
    // means a schema full of REFERENCES clauses enforces nothing unless every
    // connection asks. Migrations turn it off deliberately and turn it back on.
    connection.pragma_update(None, "foreign_keys", true)?;
    Ok(())
}

/// Puts the database into write-ahead logging mode, which is what lets the
/// desktop application read while the recorder writes.
///
/// A database that will not take the mode is a warning rather than a failure,
/// in **both** of the ways it can refuse: SQLite may answer with a mode other
/// than `wal`, and the statement may fail outright. Write-ahead logging needs
/// shared memory the two connections can both map, and a filesystem that cannot
/// provide it — a network share is the usual one — is met either way depending
/// on which layer notices. Refusing to open would cost the user their library
/// over a property of where they keep it (AGENTS.md section 16). Readers then
/// block during writes, which for a metadata index is slow rather than broken.
///
/// `synchronous` follows the mode it actually got, which is why the two live in
/// one function. `NORMAL` is only safe from corruption *in write-ahead logging
/// mode*; in a rollback journal it trades the file's integrity for the same
/// speed, which is not a trade to make silently on a database somebody's
/// library is indexed in.
fn enable_write_ahead_logging(connection: &Connection, path: &Path) -> Result<(), StorageError> {
    let enabled = match connection.query_row("PRAGMA journal_mode = WAL", [], |row| {
        row.get::<_, String>(0)
    }) {
        Ok(mode) if mode.eq_ignore_ascii_case("wal") => true,
        Ok(mode) => {
            warn!(
                database = %path.display(),
                mode = %mode,
                "the database could not be put into write-ahead logging mode, so readers will \
                 wait for writes; this usually means it is on a network share"
            );
            false
        }
        Err(error) => {
            warn!(
                database = %path.display(),
                %error,
                "the database refused write-ahead logging mode, so readers will wait for \
                 writes; this usually means it is on a network share"
            );
            false
        }
    };

    if enabled {
        // NORMAL is the setting write-ahead logging exists for: a commit does
        // not wait for the disk to acknowledge it, and a power failure can lose
        // the most recent transactions but cannot corrupt the file. Losing the
        // last few metadata writes costs a re-index from the sidecars beside
        // the recordings; FULL would cost a disk flush on every commit made
        // while a game is running.
        connection.pragma_update(None, "synchronous", "NORMAL")?;
    }
    Ok(())
}

/// Checks that the file is Clipped's, and claims it when it is new.
///
/// `writable` says whether this connection may stamp an empty file.
fn claim(connection: &Connection, path: &Path, writable: bool) -> Result<(), StorageError> {
    let application_id: i32 =
        connection.query_row("PRAGMA application_id", [], |row| row.get(0))?;
    if application_id == APPLICATION_ID {
        return Ok(());
    }

    let objects: i64 =
        connection.query_row("SELECT COUNT(*) FROM sqlite_master", [], |row| row.get(0))?;
    if objects > 0 {
        return Err(StorageError::NotAClippedDatabase {
            path: path.to_path_buf(),
            application_id,
        });
    }

    if writable {
        connection.pragma_update(None, "application_id", APPLICATION_ID)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rusqlite::params;

    use super::*;
    use crate::test_support::{scratch_directory, table_names};

    #[test]
    fn opening_a_new_database_creates_the_schema_and_claims_the_file() {
        let directory = scratch_directory("open-new");
        let path = directory.join("nested").join("library.db");

        let database = Database::open(&path).expect("a new database can be opened");

        assert_eq!(database.schema_version(), SCHEMA_VERSION);
        assert_eq!(database.migration().from, 0);
        // Every migration, because a new database starts at nothing. Written
        // against the list rather than against a literal so that adding one is
        // a migration to write and not also a number to remember here.
        assert_eq!(
            database.migration().applied,
            (1..=SCHEMA_VERSION).collect::<Vec<u32>>()
        );
        assert_eq!(database.migration().backup, None);

        let tables = table_names(database.connection());
        for expected in [
            "games",
            "sessions",
            "session_game_candidates",
            "recordings",
            "clips",
            "bookmarks",
            "session_events",
            "tags",
            "recording_tags",
            "clip_tags",
            "settings",
            "game_settings",
            "schema_migrations",
        ] {
            assert!(tables.contains(&expected.to_owned()), "missing {expected}");
        }

        let application_id: i32 = database
            .connection()
            .query_row("PRAGMA application_id", [], |row| row.get(0))
            .expect("the header can be read");
        assert_eq!(application_id, APPLICATION_ID);
    }

    #[test]
    fn opening_it_again_changes_nothing() {
        let directory = scratch_directory("open-again");
        let path = directory.join("library.db");
        drop(Database::open(&path).expect("a new database"));

        let database = Database::open(&path).expect("it reopens");

        assert_eq!(database.migration().applied, Vec::<u32>::new());
        assert_eq!(database.migration().backup, None);
        assert_eq!(database.schema_version(), SCHEMA_VERSION);
    }

    #[test]
    fn a_database_belonging_to_something_else_is_refused_rather_than_migrated() {
        let directory = scratch_directory("foreign-database");
        let path = directory.join("someone-elses.db");
        {
            let other = Connection::open(&path).expect("a database can be created");
            other
                .execute_batch(
                    "CREATE TABLE important (id INTEGER PRIMARY KEY, value TEXT);\
                     INSERT INTO important (id, value) VALUES (1, 'irreplaceable');",
                )
                .expect("it can be filled");
        }

        let error = Database::open(&path).expect_err("a foreign database is refused");
        match error {
            StorageError::NotAClippedDatabase { .. } => {}
            other => panic!("expected a refusal, got {other:?}"),
        }

        // The point of refusing is that the other application's data is still
        // there afterwards.
        let other = Connection::open(&path).expect("it still opens");
        let value: String = other
            .query_row("SELECT value FROM important WHERE id = 1", [], |row| {
                row.get(0)
            })
            .expect("the row is still there");
        assert_eq!(value, "irreplaceable");
        assert!(
            !table_names(&other).contains(&"schema_migrations".to_owned()),
            "Clipped wrote its schema into somebody else's database"
        );
    }

    #[test]
    fn a_database_from_a_newer_build_is_refused_by_both_ways_of_opening_it() {
        let directory = scratch_directory("newer-build");
        let path = directory.join("library.db");
        {
            let database = Database::open(&path).expect("a database at this build's version");
            database
                .connection()
                .execute(
                    "INSERT INTO schema_migrations (version, name, checksum, applied_at) \
                     VALUES (?1, 'from-the-future', 'ffffffffffffffff', '2030-01-01T00:00:00Z')",
                    [SCHEMA_VERSION + 5],
                )
                .expect("a future version can be written for the test");
        }

        for error in [
            Database::open(&path).expect_err("writing is refused"),
            Database::open_read_only(&path).expect_err("reading is refused"),
        ] {
            match error {
                StorageError::FromNewerVersion {
                    found, supported, ..
                } => {
                    assert_eq!(found, SCHEMA_VERSION + 5);
                    assert_eq!(supported, SCHEMA_VERSION);
                }
                other => panic!("expected a newer-version refusal, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_reader_can_read_while_a_writer_holds_a_transaction_open() {
        // The concurrency model in one test: the desktop application reads a
        // consistent snapshot rather than waiting for the recorder's write.
        let directory = scratch_directory("reader-and-writer");
        let path = directory.join("library.db");
        let mut writer = Database::open(&path).expect("a writer");
        writer
            .connection()
            .execute(
                "INSERT INTO games (game_id, name, first_seen_at) \
                 VALUES ('counter-strike-2', 'Counter-Strike 2', '2026-08-11T14:32:05+01:00')",
                [],
            )
            .expect("a game can be written");

        // The whole model rests on this: in the default rollback-journal mode a
        // reader and a writer contend for the file, and none of what follows
        // would be a property anybody could rely on.
        let mode: String = writer
            .connection()
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("the journal mode can be read");
        assert_eq!(mode.to_lowercase(), "wal");

        let reader = Database::open_read_only(&path).expect("a reader");
        let transaction = writer.transaction().expect("a transaction");
        transaction
            .execute(
                "INSERT INTO games (game_id, name, first_seen_at) \
                 VALUES ('dota-2', 'Dota 2', '2026-08-11T15:00:00+01:00')",
                [],
            )
            .expect("a second game can be written inside it");

        let visible: i64 = reader
            .connection()
            .query_row("SELECT COUNT(*) FROM games", [], |row| row.get(0))
            .expect("the reader is not blocked by the open transaction");
        assert_eq!(visible, 1, "the reader saw an uncommitted write");

        transaction.commit().expect("the write commits");
        let visible: i64 = reader
            .connection()
            .query_row("SELECT COUNT(*) FROM games", [], |row| row.get(0))
            .expect("the reader can read again");
        assert_eq!(visible, 2);
    }

    #[test]
    fn a_reader_of_a_database_that_has_not_been_migrated_is_told_so_rather_than_migrating_it() {
        // The desktop application opening the library before the recorder ever
        // has. Readers do not migrate — one process owns that — so this has to
        // be a message rather than a race between two processes running the
        // same upgrade.
        let directory = scratch_directory("reader-needs-migration");
        let path = directory.join("library.db");
        drop(Connection::open(&path).expect("an empty database file can be created"));

        let error = Database::open_read_only(&path).expect_err("an unmigrated database is refused");

        match error {
            StorageError::MigrationRequired {
                found, expected, ..
            } => {
                assert_eq!(found, 0);
                assert_eq!(expected, SCHEMA_VERSION);
            }
            other => panic!("expected a migration to be required, got {other:?}"),
        }
        assert!(
            table_names(&Connection::open(&path).expect("it opens")).is_empty(),
            "a reader migrated the database"
        );
    }

    #[test]
    fn a_reader_cannot_write() {
        let directory = scratch_directory("reader-is-read-only");
        let path = directory.join("library.db");
        drop(Database::open(&path).expect("a database"));

        let reader = Database::open_read_only(&path).expect("a reader");
        let refused = reader.connection().execute(
            "INSERT INTO games (game_id, name, first_seen_at) \
             VALUES ('dota-2', 'Dota 2', '2026-08-11T15:00:00+01:00')",
            [],
        );

        assert!(
            refused.is_err(),
            "a read-only connection accepted a write: {refused:?}"
        );
    }

    #[test]
    fn the_schema_enforces_what_it_declares() {
        // A schema whose foreign keys and CHECK constraints are not enforced is
        // documentation. `foreign_keys` is off by default in SQLite and per
        // connection, so this is a property of `configure`, not of the SQL.
        let directory = scratch_directory("constraints");
        let path = directory.join("library.db");
        let database = Database::open(&path).expect("a database");

        let orphan = database.connection().execute(
            "INSERT INTO sessions (session_id, game_id, started_at) \
             VALUES ('x-20260811-143205', 'never-played', '2026-08-11T14:32:05+01:00')",
            [],
        );
        assert!(
            orphan.is_err(),
            "a session referred to a game that is not there"
        );

        database
            .connection()
            .execute(
                "INSERT INTO games (game_id, name, first_seen_at) \
                 VALUES ('counter-strike-2', 'Counter-Strike 2', '2026-08-11T14:32:05+01:00')",
                [],
            )
            .expect("the game can be written");
        database
            .connection()
            .execute(
                "INSERT INTO sessions (session_id, game_id, started_at) \
                 VALUES ('counter-strike-2-20260811-143205', 'counter-strike-2', \
                         '2026-08-11T14:32:05+01:00')",
                [],
            )
            .expect("the session can be written");

        let invalid_outcome = database.connection().execute(
            "INSERT INTO recordings (session_id, session_index, path, started_at, outcome) \
             VALUES ('counter-strike-2-20260811-143205', 1, 'D:\\clips\\a.mkv', \
                     '2026-08-11T14:32:09+01:00', 'sort-of-recorded')",
            [],
        );
        assert!(
            invalid_outcome.is_err(),
            "an outcome outside the vocabulary was accepted"
        );

        let mistyped = database.connection().execute(
            "INSERT INTO recordings (session_id, session_index, path, started_at, frames_encoded) \
             VALUES ('counter-strike-2-20260811-143205', 1, 'D:\\clips\\a.mkv', \
                     '2026-08-11T14:32:09+01:00', 'sixty thousand')",
            [],
        );
        assert!(
            mistyped.is_err(),
            "a STRICT table accepted text in an INTEGER column"
        );
    }

    /// A session with one recording, for the game-event tests below.
    ///
    /// Returns the recording's id.
    fn a_session_with_a_recording(database: &Database) -> i64 {
        let connection = database.connection();
        connection
            .execute_batch(
                "INSERT INTO games (game_id, name, first_seen_at) \
                 VALUES ('counter-strike-2', 'Counter-Strike 2', '2026-08-11T14:32:05+01:00'); \
                 INSERT INTO sessions (session_id, game_id, started_at) \
                 VALUES ('counter-strike-2-20260811-143205', 'counter-strike-2', \
                         '2026-08-11T14:32:05+01:00'); \
                 INSERT INTO recordings (session_id, session_index, path, started_at) \
                 VALUES ('counter-strike-2-20260811-143205', 1, 'D:\\clips\\a.mkv', \
                         '2026-08-11T14:32:09+01:00');",
            )
            .expect("the session and its recording can be written");
        connection.last_insert_rowid()
    }

    #[test]
    fn a_game_event_belongs_to_a_recording_or_to_the_session_alone() {
        // Both shapes, because issue #71's second acceptance criterion is the
        // one with no file: an event heard before the recorder started, in the
        // gap between two segments, or during a replay-buffer-only session that
        // never wrote anything. A schema that forced a recording could not say
        // it happened at all.
        let directory = scratch_directory("game-events-shapes");
        let database = Database::open(directory.join("library.db")).expect("a database");
        let recording = a_session_with_a_recording(&database);

        database
            .connection()
            .execute(
                "INSERT INTO game_events \
                     (session_id, recording_id, at_nanos, kind, source, document) \
                 VALUES ('counter-strike-2-20260811-143205', ?1, 4200000000, 'kill', 'cs2', \
                         '{\"schema\":1,\"kind\":\"kill\"}')",
                params![recording],
            )
            .expect("an event in a recording can be written");
        database
            .connection()
            .execute(
                "INSERT INTO game_events \
                     (session_id, recording_id, at_nanos, kind, source, document) \
                 VALUES ('counter-strike-2-20260811-143205', NULL, -1500000000, 'match-started', \
                         'cs2', '{\"schema\":1,\"kind\":\"match-started\"}')",
                [],
            )
            .expect("an event belonging to no file can be written");

        // Negative nanoseconds survive as negative: an `EventTime` is signed
        // because a moment can precede the timeline's origin, and a column that
        // silently made that positive would move the event.
        let earliest: i64 = database
            .connection()
            .query_row("SELECT MIN(at_nanos) FROM game_events", [], |row| {
                row.get(0)
            })
            .expect("the events can be read");
        assert_eq!(earliest, -1_500_000_000, "a moment before the origin moved");
    }

    #[test]
    fn deleting_a_recording_keeps_what_happened_during_it() {
        // AGENTS.md section 56. The events are the session's, not the file's:
        // deleting a recording must not silently take the record of the match
        // that was played. They stop claiming a position in a file that has
        // gone, and that is all.
        let directory = scratch_directory("game-events-recording-deleted");
        let database = Database::open(directory.join("library.db")).expect("a database");
        let recording = a_session_with_a_recording(&database);
        database
            .connection()
            .execute(
                "INSERT INTO game_events \
                     (session_id, recording_id, at_nanos, kind, source, document) \
                 VALUES ('counter-strike-2-20260811-143205', ?1, 4200000000, 'kill', 'cs2', '{}')",
                params![recording],
            )
            .expect("the event can be written");

        database
            .connection()
            .execute(
                "DELETE FROM recordings WHERE recording_id = ?1",
                params![recording],
            )
            .expect("the recording can be deleted");

        let (kept, orphaned): (i64, i64) = database
            .connection()
            .query_row(
                "SELECT COUNT(*), COUNT(recording_id) FROM game_events",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("the events can be counted");
        assert_eq!(kept, 1, "deleting a recording took the event with it");
        assert_eq!(orphaned, 0, "the event still points at a deleted recording");
    }

    #[test]
    fn deleting_a_session_takes_its_events_but_an_open_vocabulary_is_accepted() {
        let directory = scratch_directory("game-events-session-deleted");
        let database = Database::open(directory.join("library.db")).expect("a database");
        a_session_with_a_recording(&database);

        // A kind this build has never met is stored, not refused. The whole
        // argument for `document` being the authority is that an event survives
        // a build that does not understand it, and a vocabulary CHECK here
        // would refuse exactly those events.
        database
            .connection()
            .execute(
                "INSERT INTO game_events (session_id, at_nanos, kind, source, document) \
                 VALUES ('counter-strike-2-20260811-143205', 1, 'acme.tournament/ace', \
                         'acme-plugin', '{\"schema\":9}')",
                [],
            )
            .expect("a kind this build does not know is still stored");

        let empty = database.connection().execute(
            "INSERT INTO game_events (session_id, at_nanos, kind, source, document) \
             VALUES ('counter-strike-2-20260811-143205', 1, '', 'cs2', '{}')",
            [],
        );
        assert!(empty.is_err(), "an event with no kind at all was accepted");

        database
            .connection()
            .execute(
                "DELETE FROM sessions WHERE session_id = 'counter-strike-2-20260811-143205'",
                [],
            )
            .expect("the session can be deleted");

        let left: i64 = database
            .connection()
            .query_row("SELECT COUNT(*) FROM game_events", [], |row| row.get(0))
            .expect("the events can be counted");
        assert_eq!(left, 0, "a deleted session left its events behind");
    }

    #[test]
    fn a_database_that_needed_no_migration_still_enforces_its_references() {
        // The interesting case, and one a mutation caught: migrating a database
        // turns referential integrity back on at the end, so a *newly created*
        // one has it whether `configure` sets it or not. A database that is
        // already current takes the short path out of `migrate` and never
        // reaches that line, so this is the only connection whose enforcement
        // comes from `configure` alone.
        let directory = scratch_directory("reopened-enforces");
        let path = directory.join("library.db");
        drop(Database::open(&path).expect("a database"));

        let database = Database::open(&path).expect("it reopens");

        assert_eq!(database.migration().applied, Vec::<u32>::new());
        let on: i64 = database
            .connection()
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .expect("the pragma can be read");
        assert_eq!(on, 1, "a reopened database does not enforce its references");
        let orphan = database.connection().execute(
            "INSERT INTO sessions (session_id, game_id, started_at) \
             VALUES ('x-20260811-143205', 'never-played', '2026-08-11T14:32:05+01:00')",
            [],
        );
        assert!(
            orphan.is_err(),
            "a session referred to a game that is not there"
        );
    }

    #[test]
    fn a_database_that_refuses_write_ahead_logging_is_still_opened() {
        // The documented tolerance, exercised. SQLite refuses a journal mode
        // change from inside a transaction with an *error* rather than an
        // answer, which is the shape a filesystem that cannot map the shared
        // memory the mode needs produces too, and the one an ordinary open
        // cannot reach. If this propagated, a user on a network share would
        // lose their library to a pragma.
        let directory = scratch_directory("wal-refused");
        let path = directory.join("library.db");
        let mut connection = Connection::open(&path).expect("a database can be created");
        let transaction = connection.transaction().expect("a transaction can begin");
        assert!(
            transaction
                .query_row("PRAGMA journal_mode = WAL", [], |row| row
                    .get::<_, String>(0))
                .is_err(),
            "this test's premise is gone: SQLite accepted the mode change inside a transaction"
        );

        enable_write_ahead_logging(&transaction, &path)
            .expect("a database that refuses write-ahead logging is a warning, not a failure");
    }

    #[test]
    fn durability_is_only_relaxed_where_write_ahead_logging_makes_it_safe() {
        // `synchronous = NORMAL` cannot corrupt a WAL-mode database and can
        // corrupt one in a rollback journal, so the setting has to follow the
        // mode that was actually granted rather than the one that was asked
        // for.
        const FULL: i64 = 2;
        const NORMAL: i64 = 1;

        let directory = scratch_directory("wal-durability");
        let path = directory.join("library.db");
        let database = Database::open(&path).expect("a database");
        let granted: i64 = database
            .connection()
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .expect("the pragma can be read");
        assert_eq!(
            granted, NORMAL,
            "a WAL database is paying for a disk flush on every commit"
        );

        // An in-memory database answers `PRAGMA journal_mode = WAL` with
        // `memory` instead of failing — the other way a database refuses, and
        // the same answer a filesystem without shared memory gives.
        let refusing = Connection::open_in_memory().expect("SQLite opens a database in memory");
        let before: i64 = refusing
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .expect("the pragma can be read");
        assert_eq!(before, FULL, "this test's premise is gone");

        enable_write_ahead_logging(&refusing, Path::new(":memory:")).expect("it is tolerated");

        let mode: String = refusing
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("the journal mode can be read");
        assert!(
            !mode.eq_ignore_ascii_case("wal"),
            "this test's premise is gone: the database took write-ahead logging"
        );
        let after: i64 = refusing
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .expect("the pragma can be read");
        assert_eq!(
            after, FULL,
            "durability was relaxed on a database that is not in write-ahead logging mode"
        );
    }

    #[test]
    fn deleting_a_game_cannot_take_a_users_sessions_with_it() {
        let directory = scratch_directory("delete-restrict");
        let path = directory.join("library.db");
        let database = Database::open(&path).expect("a database");
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

        let refused = database
            .connection()
            .execute("DELETE FROM games WHERE game_id = 'counter-strike-2'", []);

        assert!(
            refused.is_err(),
            "deleting a game took its sessions with it"
        );
        let sessions: i64 = database
            .connection()
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
            .expect("the sessions can be counted");
        assert_eq!(sessions, 1);
    }
}
