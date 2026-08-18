//! The migration framework, and the migrations themselves.
//!
//! # The rules
//!
//! 1. **Migrations are append-only.** A migration that has shipped is never
//!    edited; the next change is the next file. [`CHECKSUMS`] pins the bytes of
//!    every released migration and a test fails if one of them moves, because
//!    an edited migration means two users at the same schema version with
//!    different schemas — which is the failure nobody notices until a query
//!    works on one machine and not another.
//! 2. **A migration is all or nothing.** Each runs inside its own transaction.
//!    A failure rolls that migration back and stops, leaving the database at
//!    the last version some build of Clipped understood. There is no half-way
//!    state to recover from (AGENTS.md section 31).
//! 3. **A copy is taken first.** Before the first migration is applied to a
//!    database that already holds a schema, the whole database is copied beside
//!    itself as `<name>.pre-v<from>.db`. If the copy cannot be written,
//!    nothing is migrated. Recordings are irreplaceable and their index is what
//!    finds them (AGENTS.md section 56).
//! 4. **A newer database is refused, not touched.** A build that does not
//!    understand a schema must not write to it. This is the same rule the game
//!    catalogue's overlay follows for the same reason.
//! 5. **Migrations run with foreign keys off and are checked afterwards.**
//!    Rebuilding a table in SQLite means creating, copying, dropping and
//!    renaming, which is impossible with enforcement on. `PRAGMA
//!    foreign_key_check` runs inside the same transaction, so a rebuild that
//!    dropped rows out from under their children is rolled back rather than
//!    committed.
//!
//! # Writing one
//!
//! Add `migrations/000N_<name>.sql`, add it to [`MIGRATIONS`], run the tests,
//! and paste the checksum the failing test prints into [`CHECKSUMS`]. The SQL
//! must not contain `BEGIN`, `COMMIT` or `PRAGMA foreign_keys`: the framework
//! owns the transaction and the pragma, and a migration that took either into
//! its own hands would defeat rules 2 and 5.

use std::fmt;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use tracing::{info, warn};

use crate::error::StorageError;

/// One schema change, as a version, a name and the SQL that performs it.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    /// The schema version this migration produces. Strictly ascending across
    /// [`MIGRATIONS`], starting at 1.
    pub version: u32,
    /// The name in the file it came from, for diagnostics.
    pub name: &'static str,
    /// The statements, run as one batch inside one transaction.
    pub sql: &'static str,
}

/// Every migration this build knows, in order.
///
/// The list is the schema. A database is at version *n* when the first *n* of
/// these have been applied to it, and nothing else counts as a schema change.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial",
        sql: include_str!("../migrations/0001_initial.sql"),
    },
    Migration {
        version: 2,
        name: "manual_session_end_reason",
        sql: include_str!("../migrations/0002_manual_session_end_reason.sql"),
    },
    Migration {
        version: 3,
        name: "game_events",
        sql: include_str!("../migrations/0003_game_events.sql"),
    },
    Migration {
        version: 4,
        name: "clips_without_a_file",
        sql: include_str!("../migrations/0004_clips_without_a_file.sql"),
    },
    Migration {
        version: 5,
        name: "recording_spans",
        sql: include_str!("../migrations/0005_recording_spans.sql"),
    },
    Migration {
        version: 6,
        name: "recovered_recording_outcomes",
        sql: include_str!("../migrations/0006_recovered_recording_outcomes.sql"),
    },
    Migration {
        version: 7,
        name: "locked_media",
        sql: include_str!("../migrations/0007_locked_media.sql"),
    },
];

/// The checksum of every migration that has been released.
///
/// This exists so that editing a shipped migration is a test failure rather
/// than a support case. `migrations_are_append_only` compares each entry
/// against the file that is compiled in, which is why it is a test-only
/// constant: it is a fact about the repository rather than something the
/// running application consults.
#[cfg(test)]
const CHECKSUMS: &[(u32, &str)] = &[
    (1, "afab7973dd06f9d1"),
    (2, "58c7954428d814fa"),
    (3, "d2cf76386a1d8bbe"),
    (4, "86f00947424c4989"),
    (5, "55ed6d143c4f7fe7"),
    (6, "75c5a5483a721a71"),
    (7, "fd00bcc13fc0387c"),
];

/// The newest schema version this build understands.
pub const SCHEMA_VERSION: u32 = latest_version(MIGRATIONS);

/// The highest version in a migration list.
const fn latest_version(migrations: &[Migration]) -> u32 {
    if migrations.is_empty() {
        0
    } else {
        migrations[migrations.len() - 1].version
    }
}

/// What migrating a database did to it.
///
/// Reported rather than only logged because a user should be told when their
/// library was upgraded and where the copy taken beforehand went: that path is
/// the recovery route if the upgrade turns out to have been wrong, and nobody
/// can use a backup they were never told about (AGENTS.md section 45).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationOutcome {
    /// The schema version the database was at when it was opened. Zero for a
    /// database that did not exist.
    pub from: u32,
    /// The schema version it is at now.
    pub to: u32,
    /// The versions applied, in order. Empty when the database was already
    /// current.
    pub applied: Vec<u32>,
    /// Where the copy taken before migrating was written, when one was taken.
    /// `None` for a new database — there was nothing to copy — and for a
    /// database that needed no migration.
    pub backup: Option<PathBuf>,
}

impl fmt::Display for MigrationOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.applied.is_empty() {
            write!(f, "schema {} (unchanged)", self.to)
        } else {
            write!(f, "schema {} migrated to {}", self.from, self.to)
        }
    }
}

/// The schema version a database is at.
///
/// Zero when it has never been migrated, which is also what an empty file
/// reports. There is exactly one source of this answer — the `schema_migrations`
/// table — because a second one (`PRAGMA user_version`, say) is a second thing
/// that can disagree.
pub(crate) fn schema_version(connection: &Connection) -> rusqlite::Result<u32> {
    if !table_exists(connection, "schema_migrations")? {
        return Ok(0);
    }
    connection.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )
}

/// Whether a table of this name exists.
pub(crate) fn table_exists(connection: &Connection, name: &str) -> rusqlite::Result<bool> {
    connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |row| row.get::<_, i64>(0).map(|count| count > 0),
    )
}

/// Brings `connection` up to the newest version in `migrations`.
///
/// `path` is where the database lives, and is `None` only for an in-memory one;
/// it is what a backup is written beside.
///
/// # Errors
///
/// [`StorageError::FromNewerVersion`] if the database is ahead of this build,
/// in which case nothing is written to it at all. [`StorageError::Backup`] if
/// the copy taken beforehand could not be written, again without having changed
/// anything. [`StorageError::Migration`] or
/// [`StorageError::MigrationBrokeReferences`] if a migration failed, in which
/// case that migration was rolled back and the database is at the version
/// before it.
pub(crate) fn migrate(
    connection: &mut Connection,
    path: Option<&Path>,
    migrations: &[Migration],
) -> Result<MigrationOutcome, StorageError> {
    let from = schema_version(connection)?;
    let target = latest_version(migrations);

    if from > target {
        return Err(StorageError::FromNewerVersion {
            path: path.unwrap_or_else(|| Path::new(":memory:")).to_path_buf(),
            found: from,
            supported: target,
        });
    }

    report_altered_migrations(connection, migrations, from)?;

    let pending: Vec<&Migration> = migrations
        .iter()
        .filter(|migration| migration.version > from)
        .collect();
    if pending.is_empty() {
        return Ok(MigrationOutcome {
            from,
            to: from,
            applied: Vec::new(),
            backup: None,
        });
    }

    // Only a database that already holds a schema is worth copying. A new one
    // has nothing in it, and writing an empty backup beside every fresh install
    // would teach people to ignore the file.
    let backup = match path {
        Some(path) if from > 0 => back_up(connection, path)?,
        _ => None,
    };

    create_bookkeeping(connection)?;

    // Rule 5. This has to happen outside a transaction: SQLite silently ignores
    // `PRAGMA foreign_keys` inside one, which would make the setting look
    // applied and not be.
    connection.pragma_update(None, "foreign_keys", false)?;
    let result = apply_each(connection, &pending);
    connection.pragma_update(None, "foreign_keys", true)?;
    let applied = result?;

    let to = schema_version(connection)?;
    info!(
        from,
        to,
        applied = applied.len(),
        backup = backup.as_ref().map(|path| path.display().to_string()),
        "applied database migrations"
    );

    Ok(MigrationOutcome {
        from,
        to,
        applied,
        backup,
    })
}

/// Applies each pending migration in its own transaction.
///
/// Returns the versions this connection applied. On failure the migration that
/// failed has been rolled back and the ones before it are committed, which is
/// the point: every intermediate state is a schema version some build
/// understood.
///
/// # Two connections opening the same new database
///
/// This is an ordinary situation rather than an exotic one: one process holds
/// more than one connection to its library — `apps/recorder` reads the index on
/// the window's behalf and reconciles it on a thread of its own — and a database
/// that does not exist yet is migrated by whichever of them opens it first.
///
/// Two things make that safe, and both are needed. The transaction is
/// **immediate**, so the second connection waits for the first rather than
/// reading a schema that is about to change under it; SQLite's deferred default
/// would let both decide to create the same table. And the version is read again
/// **inside** the transaction, because the pending list was worked out before
/// the wait: without that, the second connection would wait politely and then
/// run a migration that had already been applied, which is the
/// `table games already exists` that issue #402 found the moment `serve` grew a
/// second connection.
fn apply_each(
    connection: &mut Connection,
    pending: &[&Migration],
) -> Result<Vec<u32>, StorageError> {
    let mut applied = Vec::with_capacity(pending.len());

    for migration in pending {
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|source| failure(migration, source))?;

        let already = schema_version(&transaction).map_err(|source| failure(migration, source))?;
        if migration.version <= already {
            // Applied by another connection while this one waited for the write
            // lock. Dropping the transaction rolls back nothing, because nothing
            // has been written.
            drop(transaction);
            continue;
        }

        transaction
            .execute_batch(migration.sql)
            .map_err(|source| failure(migration, source))?;

        let violations =
            count_broken_references(&transaction).map_err(|source| failure(migration, source))?;
        if violations > 0 {
            // Dropping the transaction rolls it back.
            return Err(StorageError::MigrationBrokeReferences {
                version: migration.version,
                name: migration.name,
                violations,
            });
        }

        transaction
            .execute(
                "INSERT INTO schema_migrations (version, name, checksum, applied_at) \
                 VALUES (?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))",
                rusqlite::params![migration.version, migration.name, checksum(migration.sql)],
            )
            .map_err(|source| failure(migration, source))?;

        transaction
            .commit()
            .map_err(|source| failure(migration, source))?;

        applied.push(migration.version);
    }

    Ok(applied)
}

/// The error a failed migration reports.
fn failure(migration: &Migration, source: rusqlite::Error) -> StorageError {
    StorageError::Migration {
        version: migration.version,
        name: migration.name,
        source,
    }
}

/// How many rows refer to rows that do not exist.
fn count_broken_references(connection: &Connection) -> rusqlite::Result<usize> {
    let mut statement = connection.prepare("PRAGMA foreign_key_check")?;
    let mut rows = statement.query([])?;
    let mut violations = 0;
    while rows.next()?.is_some() {
        violations += 1;
    }
    Ok(violations)
}

/// Copies the whole database beside itself before it is migrated.
///
/// `VACUUM INTO` rather than a file copy: it asks SQLite for a consistent
/// snapshot of the database *as a database*, so the write-ahead log is folded
/// in and the result is a single file that opens. Copying the file by hand
/// would take whichever half of a WAL-mode database happened to be on disk.
///
/// A backup that already exists is kept as it is. Reaching here twice from the
/// same version means the previous attempt did not finish, and the older copy
/// is the one further from whatever went wrong. Anything at that path that is
/// not a file is not a backup and is not treated as one: `VACUUM INTO` is asked
/// to write it, fails, and the migration stops.
///
/// # Two connections arriving together
///
/// The check above and the write below are not one operation, and
/// `apps/recorder` holds two connections that open the same database at the
/// same moment. Both can find no backup, both can ask for one, and the second
/// `VACUUM INTO` then fails — not with anything resembling "the file is
/// there", but with `table schema_migrations already exists`, because it
/// starts creating tables in a destination that turns out not to be empty.
///
/// Propagating that failed the migration, which reached the window as "the
/// recording library could not be opened" — the same sentence, and the same
/// cause, as the race
/// [`two_connections_opening_the_same_new_database_both_get_a_working_schema`]
/// was written for, arrived at through the copy rather than through the schema
/// ([issue #635](https://github.com/wildware-uk/clipped/issues/635)). It needs
/// a *pending* migration to reach, so the people it happened to were not
/// first-run users but everyone upgrading across a schema version.
///
/// The recovery asks the same question again rather than reading the message:
/// if the destination is a file **now** and was not before, a peer wrote it
/// while this thread was asking, which is the case the paragraph above already
/// decides — keep it. If it still is not a file, the failure is a real one and
/// is propagated. Matching on SQLite's wording would break the day it changes,
/// and would also swallow a genuine "that table exists" from a destination
/// somebody else's program is writing.
fn back_up(connection: &Connection, path: &Path) -> Result<Option<PathBuf>, StorageError> {
    let from = schema_version(connection)?;
    let destination = backup_path(path, from);

    if destination.is_file() {
        warn!(
            backup = %destination.display(),
            "a copy from this schema version already exists and was kept; \
             the database has not been copied again"
        );
        return Ok(Some(destination));
    }

    if let Err(source) = connection.execute("VACUUM INTO ?1", [&destination.to_string_lossy()]) {
        // `is_file` rather than `exists`: a directory that appeared at this
        // path is not a backup, and the doc comment above says so. It has to
        // fail here exactly as it would have failed above.
        if destination.is_file() {
            warn!(
                backup = %destination.display(),
                "another connection copied the database from this schema version while this one \
                 was asking, and its copy was kept"
            );
            return Ok(Some(destination));
        }

        return Err(StorageError::Backup {
            destination,
            source,
        });
    }

    info!(backup = %destination.display(), from, "copied the database before migrating it");
    Ok(Some(destination))
}

/// Where the copy of a database at `version` goes: beside it, named after the
/// version it is a copy of.
///
/// Beside it rather than in a backups directory so that a user who finds one
/// file finds the other, and named after the version rather than the time so
/// that repeated attempts at the same upgrade do not fill a disk with copies.
pub(crate) fn backup_path(path: &Path, version: u32) -> PathBuf {
    let name = path.file_stem().map_or_else(
        || String::from("clipped"),
        |stem| stem.to_string_lossy().into_owned(),
    );
    path.with_file_name(format!("{name}.pre-v{version}.db"))
}

/// Warns about any applied migration whose SQL differs from this build's.
///
/// This is a diagnostic, not a refusal. The situation should be impossible —
/// `migrations_are_append_only` fails before such a build could ship — and the
/// cost of being wrong about that is a user locked out of their own library,
/// which is a worse outcome than a warning in the log (AGENTS.md section 56).
fn report_altered_migrations(
    connection: &Connection,
    migrations: &[Migration],
    from: u32,
) -> Result<(), StorageError> {
    if from == 0 {
        return Ok(());
    }

    let mut statement =
        connection.prepare("SELECT version, checksum FROM schema_migrations ORDER BY version")?;
    let recorded = statement.query_map([], |row| {
        Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?))
    })?;

    for entry in recorded {
        let (version, recorded) = entry?;
        let Some(migration) = migrations
            .iter()
            .find(|migration| migration.version == version)
        else {
            continue;
        };
        let built = checksum(migration.sql);
        if built != recorded {
            warn!(
                version,
                name = migration.name,
                recorded = %recorded,
                built = %built,
                "this database was migrated by a build whose migration text differs from this \
                 one's; the schema may not be what this build expects"
            );
        }
    }

    Ok(())
}

/// The checksum a migration's text is recorded under.
///
/// FNV-1a, sixteen hex digits. It is a fingerprint of a file that is in the
/// repository and not a defence against anybody: the question it answers is
/// "did this text change between releases", and a cryptographic hash would be a
/// dependency bought for nothing.
///
/// Carriage returns are skipped. `.gitattributes` normalises the repository to
/// LF, but a checkout with `core.autocrlf` on would otherwise compile in
/// different bytes and give the same migration a different fingerprint on one
/// contributor's machine — which is precisely the kind of "works here" that
/// makes a determinism check worse than useless.
fn checksum(sql: &str) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET;
    for byte in sql.as_bytes().iter().filter(|byte| **byte != b'\r') {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:016x}")
}

/// Creates the table the framework records itself in.
///
/// Not a migration, because it is what says which migrations have run. It is
/// created before the first one and never changes shape; a change to *it* would
/// have no version to be recorded under.
pub(crate) fn create_bookkeeping(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version    INTEGER PRIMARY KEY,
             name       TEXT NOT NULL,
             checksum   TEXT NOT NULL,
             applied_at TEXT NOT NULL
         ) STRICT;",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{scratch_directory, table_names};

    /// A migration list with three versions, the third of which fails halfway
    /// through. It exists so that the framework can be tested across more
    /// versions than the schema has, and so that a failure can be provoked
    /// without shipping a broken migration.
    fn fixture() -> Vec<Migration> {
        vec![
            Migration {
                version: 1,
                name: "first",
                sql: "CREATE TABLE first (id INTEGER PRIMARY KEY, value TEXT NOT NULL) STRICT;",
            },
            Migration {
                version: 2,
                name: "second",
                sql: "CREATE TABLE second (id INTEGER PRIMARY KEY) STRICT;\n\
                      INSERT INTO first (id, value) VALUES (1, 'kept');",
            },
            Migration {
                version: 3,
                name: "third",
                sql: "CREATE TABLE third (id INTEGER PRIMARY KEY) STRICT;\n\
                      INSERT INTO first (id, value) VALUES (2, 'also kept');\n\
                      INSERT INTO nowhere (id) VALUES (1);",
            },
        ]
    }

    fn migrated(connection: &mut Connection, upto: u32) -> MigrationOutcome {
        let list: Vec<Migration> = fixture()
            .into_iter()
            .filter(|migration| migration.version <= upto)
            .collect();
        migrate(connection, None, &list).expect("the fixture migrations apply")
    }

    fn in_memory() -> Connection {
        Connection::open_in_memory().expect("SQLite can open a database in memory")
    }

    #[test]
    fn the_shipped_migrations_are_numbered_from_one_and_ascend_without_gaps() {
        // The framework picks pending migrations by comparing versions, so a
        // repeated or out-of-order version would silently skip one.
        for (index, migration) in MIGRATIONS.iter().enumerate() {
            let expected = u32::try_from(index).expect("the list is short") + 1;
            assert_eq!(
                migration.version, expected,
                "migration {} is out of order",
                migration.name
            );
        }
        assert_eq!(
            SCHEMA_VERSION,
            u32::try_from(MIGRATIONS.len()).expect("the list is short")
        );
    }

    #[test]
    fn migrations_are_append_only() {
        // Editing a migration that has shipped means two users at the same
        // schema version with different schemas. This is the test that stops
        // it, so it prints what to paste when a *new* migration is added.
        assert_eq!(
            CHECKSUMS.len(),
            MIGRATIONS.len(),
            "every migration needs a checksum; add ({}, \"{}\")",
            MIGRATIONS[MIGRATIONS.len() - 1].version,
            checksum(MIGRATIONS[MIGRATIONS.len() - 1].sql)
        );

        for (migration, (version, recorded)) in MIGRATIONS.iter().zip(CHECKSUMS) {
            assert_eq!(migration.version, *version);
            assert_eq!(
                &checksum(migration.sql),
                recorded,
                "migration {} ({}) has been edited since it was released; \
                 add a new migration instead. Its checksum is now {}",
                migration.version,
                migration.name,
                checksum(migration.sql)
            );
        }
    }

    #[test]
    fn a_shipped_migration_never_takes_the_transaction_or_the_pragma_into_its_own_hands() {
        // Rules 2 and 5 are properties of the framework, and a migration
        // containing COMMIT or its own foreign key pragma would quietly break
        // both for everything after it in the same batch.
        for migration in MIGRATIONS {
            let sql = migration.sql.to_lowercase();
            for forbidden in [
                "begin;",
                "begin ",
                "commit",
                "rollback",
                "pragma foreign_keys",
            ] {
                assert!(
                    !sql.contains(forbidden),
                    "migration {} contains `{forbidden}`, which the framework owns",
                    migration.version
                );
            }
        }
    }

    #[test]
    fn a_new_database_is_migrated_to_the_newest_version() {
        let mut connection = in_memory();
        let outcome = migrated(&mut connection, 2);

        assert_eq!(outcome.from, 0);
        assert_eq!(outcome.to, 2);
        assert_eq!(outcome.applied, vec![1, 2]);
        assert_eq!(outcome.backup, None, "a new database has nothing to copy");
        assert_eq!(schema_version(&connection).expect("a version"), 2);
    }

    #[test]
    fn migrating_from_every_earlier_version_reaches_the_same_schema() {
        // The property that matters for an upgrade: a database that has been
        // through the migrations must be indistinguishable from one created
        // today. Looping over every version means each new migration is covered
        // by this test on the day it is added, rather than when somebody
        // remembers to write a test for it.
        let list = fixture();
        let list = &list[..2];
        let fresh = {
            let mut connection = in_memory();
            migrate(&mut connection, None, list).expect("a fresh database migrates");
            schema_of(&connection)
        };

        for start in 0..=latest_version(list) {
            let mut connection = in_memory();
            migrate(&mut connection, None, &list[..start as usize])
                .expect("an older database can be built");
            assert_eq!(schema_version(&connection).expect("a version"), start);

            let outcome =
                migrate(&mut connection, None, list).expect("an older database migrates forwards");

            assert_eq!(outcome.from, start);
            assert_eq!(outcome.to, latest_version(list));
            assert_eq!(
                schema_of(&connection),
                fresh,
                "a database migrated from version {start} differs from a fresh one"
            );
        }
    }

    #[test]
    fn every_shipped_version_migrates_forwards_to_the_current_schema() {
        // The same property, against the migrations that actually ship. With
        // one migration this covers 0 -> 1 and 1 -> 1; it grows by itself.
        let fresh = {
            let mut connection = in_memory();
            migrate(&mut connection, None, MIGRATIONS).expect("a fresh database migrates");
            schema_of(&connection)
        };

        for start in 0..=SCHEMA_VERSION {
            let mut connection = in_memory();
            migrate(&mut connection, None, &MIGRATIONS[..start as usize])
                .expect("an older database can be built");

            let outcome = migrate(&mut connection, None, MIGRATIONS)
                .expect("an older database migrates forwards");

            assert_eq!(outcome.to, SCHEMA_VERSION);
            assert_eq!(
                schema_of(&connection),
                fresh,
                "a database migrated from version {start} differs from a fresh one"
            );
        }
    }

    #[test]
    fn a_failing_migration_is_rolled_back_and_leaves_the_version_before_it() {
        let mut connection = in_memory();
        migrated(&mut connection, 2);

        let list = fixture();
        let error = migrate(&mut connection, None, &list)
            .expect_err("the third fixture migration cannot apply");

        match error {
            StorageError::Migration { version, name, .. } => {
                assert_eq!(version, 3);
                assert_eq!(name, "third");
            }
            other => panic!("expected the third migration to fail, got {other:?}"),
        }

        assert_eq!(
            schema_version(&connection).expect("a version"),
            2,
            "a failed migration must leave the version before it"
        );
        let tables = table_names(&connection);
        assert!(
            !tables.contains(&"third".to_owned()),
            "the failed migration's table survived the rollback: {tables:?}"
        );
        let rows: i64 = connection
            .query_row("SELECT COUNT(*) FROM first", [], |row| row.get(0))
            .expect("the table from migration 1 is still there");
        assert_eq!(
            rows, 1,
            "the failed migration's insert survived the rollback"
        );
    }

    #[test]
    fn a_failed_migration_can_be_retried_once_the_migration_is_fixed() {
        // Recoverable where practical (AGENTS.md section 31): the database is
        // not poisoned by a failure, and a build carrying the corrected
        // migration finishes the upgrade.
        let mut connection = in_memory();
        migrated(&mut connection, 2);
        let broken = fixture();
        migrate(&mut connection, None, &broken).expect_err("it fails the first time");

        let mut fixed = fixture();
        fixed[2].sql = "CREATE TABLE third (id INTEGER PRIMARY KEY) STRICT;";
        let outcome = migrate(&mut connection, None, &fixed).expect("the corrected migration runs");

        assert_eq!(outcome.applied, vec![3]);
        assert_eq!(schema_version(&connection).expect("a version"), 3);
    }

    #[test]
    fn a_migration_that_breaks_references_is_rolled_back() {
        let mut connection = in_memory();
        let list = vec![
            Migration {
                version: 1,
                name: "parents",
                sql: "CREATE TABLE parents (id INTEGER PRIMARY KEY) STRICT;\n\
                      CREATE TABLE children (\
                          id INTEGER PRIMARY KEY, \
                          parent INTEGER NOT NULL REFERENCES parents (id)\
                      ) STRICT;\n\
                      INSERT INTO parents (id) VALUES (1);\n\
                      INSERT INTO children (id, parent) VALUES (1, 1);",
            },
            // The mistake this rule exists to catch: a rebuild that takes the
            // parents away and leaves the children pointing at nothing. With
            // enforcement off - which is how a rebuild has to run - SQLite
            // accepts it without complaint.
            Migration {
                version: 2,
                name: "orphans-the-children",
                sql: "DELETE FROM parents;",
            },
        ];

        migrate(&mut connection, None, &list[..1]).expect("the first migration applies");
        let error = migrate(&mut connection, None, &list).expect_err("the second is refused");

        match error {
            StorageError::MigrationBrokeReferences {
                version,
                violations,
                ..
            } => {
                assert_eq!(version, 2);
                assert_eq!(violations, 1);
            }
            other => panic!("expected a broken-reference failure, got {other:?}"),
        }

        assert_eq!(schema_version(&connection).expect("a version"), 1);
        let parents: i64 = connection
            .query_row("SELECT COUNT(*) FROM parents", [], |row| row.get(0))
            .expect("the parents table is intact");
        assert_eq!(parents, 1, "the rolled-back deletion took effect anyway");
    }

    #[test]
    fn a_database_from_a_newer_build_is_refused_and_left_alone() {
        let mut connection = in_memory();
        migrated(&mut connection, 2);
        connection
            .execute(
                "INSERT INTO schema_migrations (version, name, checksum, applied_at) \
                 VALUES (99, 'from-the-future', 'ffffffffffffffff', '2030-01-01T00:00:00Z')",
                [],
            )
            .expect("a future version can be written for the test");

        let list = fixture();
        let error = migrate(&mut connection, None, &list[..2])
            .expect_err("a newer database is not migrated");

        match error {
            StorageError::FromNewerVersion {
                found, supported, ..
            } => {
                assert_eq!(found, 99);
                assert_eq!(supported, 2);
            }
            other => panic!("expected a newer-version refusal, got {other:?}"),
        }
        assert_eq!(
            schema_version(&connection).expect("a version"),
            99,
            "the refusal must not have changed the database"
        );
    }

    #[test]
    fn a_database_that_is_already_current_is_not_touched() {
        let mut connection = in_memory();
        migrated(&mut connection, 2);
        let before = schema_of(&connection);

        let list = fixture();
        let outcome = migrate(&mut connection, None, &list[..2]).expect("nothing to do");

        assert_eq!(outcome.applied, Vec::<u32>::new());
        assert_eq!(outcome.from, 2);
        assert_eq!(outcome.to, 2);
        assert_eq!(schema_of(&connection), before);
    }

    #[test]
    fn migrating_an_existing_database_copies_it_first() {
        let directory = scratch_directory("migration-backup");
        let path = directory.join("library.db");
        let list = fixture();

        {
            let mut connection = Connection::open(&path).expect("the database can be created");
            migrate(&mut connection, Some(&path), &list[..1]).expect("version 1 applies");
            connection
                .execute(
                    "INSERT INTO first (id, value) VALUES (7, 'irreplaceable')",
                    [],
                )
                .expect("a row can be written");
        }

        let mut connection = Connection::open(&path).expect("the database reopens");
        let outcome = migrate(&mut connection, Some(&path), &list[..2]).expect("version 2 applies");

        let backup = outcome
            .backup
            .expect("an existing database is copied first");
        assert_eq!(backup, backup_path(&path, 1));
        assert!(backup.is_file(), "{} was not written", backup.display());

        // The copy has to be a database, at the old version, with the user's
        // row in it. A file that exists and does not open is not a backup.
        let copy = Connection::open(&backup).expect("the copy opens as a database");
        assert_eq!(schema_version(&copy).expect("a version"), 1);
        let value: String = copy
            .query_row("SELECT value FROM first WHERE id = 7", [], |row| row.get(0))
            .expect("the row survived into the copy");
        assert_eq!(value, "irreplaceable");
        assert!(
            !table_names(&copy).contains(&"second".to_owned()),
            "the copy is of the database after migrating, not before"
        );
    }

    #[test]
    fn a_backup_that_cannot_be_written_stops_the_migration_before_it_starts() {
        let directory = scratch_directory("migration-backup-refused");
        let path = directory.join("library.db");
        let list = fixture();

        let mut connection = Connection::open(&path).expect("the database can be created");
        migrate(&mut connection, Some(&path), &list[..1]).expect("version 1 applies");

        // A directory where the copy wants to be a file. `VACUUM INTO` cannot
        // write it, which is the same shape of failure as a full disk or a
        // read-only folder without needing either.
        std::fs::create_dir_all(backup_path(&path, 1)).expect("the obstruction can be created");

        let error =
            migrate(&mut connection, Some(&path), &list[..2]).expect_err("the copy cannot be made");

        match error {
            StorageError::Backup { destination, .. } => {
                assert_eq!(destination, backup_path(&path, 1));
            }
            other => panic!("expected the copy to stop the migration, got {other:?}"),
        }
        assert_eq!(
            schema_version(&connection).expect("a version"),
            1,
            "the migration ran despite the copy failing"
        );
        assert!(
            !table_names(&connection).contains(&"second".to_owned()),
            "the migration ran despite the copy failing"
        );
    }

    #[test]
    fn two_connections_migrating_the_same_database_share_one_backup() {
        // The other half of the race
        // `two_connections_opening_the_same_new_database_both_get_a_working_schema`
        // holds. That one opens a database with no schema, so `back_up` returns
        // before it writes anything and the copy is never contended. This one
        // gives them a database at version 1 with version 2 pending, which is
        // the only way to reach the copy at all — and therefore what every user
        // upgrading across a schema version does, with the two connections
        // `apps/recorder` holds.
        //
        // Before this was fixed the loser's `VACUUM INTO` failed with
        // `table schema_migrations already exists`, because it starts creating
        // tables in a destination another thread had just filled. That became
        // `StorageError::Backup`, which failed the migration, which reached the
        // window as "the recording library could not be opened" — the same
        // sentence the sibling test exists to prevent (issue #635).
        let directory = scratch_directory("concurrent-backup");
        let path = directory.join("library.db");
        let list = fixture();

        let mut first = Connection::open(&path).expect("the database can be created");
        migrate(&mut first, Some(&path), &list[..1]).expect("version 1 applies");
        drop(first);

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let openers: Vec<std::thread::JoinHandle<Result<MigrationOutcome, StorageError>>> = (0..4)
            .map(|_| {
                let path = path.clone();
                let barrier = std::sync::Arc::clone(&barrier);
                let list = list.clone();
                std::thread::spawn(move || {
                    let mut connection = Connection::open(&path).expect("the file can be opened");
                    connection
                        .busy_timeout(std::time::Duration::from_secs(30))
                        .expect("a busy timeout can be set");
                    barrier.wait();
                    migrate(&mut connection, Some(&path), &list[..2])
                })
            })
            .collect();

        for opener in openers {
            let outcome = opener
                .join()
                .expect("the thread did not panic")
                .expect("a connection that lost the race still has to end up migrated");
            assert_eq!(
                outcome.to, 2,
                "every connection ends at the version it was given"
            );
            assert_eq!(
                outcome.backup.as_deref(),
                Some(backup_path(&path, 1).as_path()),
                "every connection names the one copy, whether it wrote it or found it"
            );
        }

        // One copy, of the database as it was before the migration.
        let copy = Connection::open(backup_path(&path, 1)).expect("the copy opens as a database");
        assert_eq!(
            schema_version(&copy).expect("the copy has a version"),
            1,
            "the copy is of the database before migrating, not after"
        );
    }

    /// Every object in the schema, in a stable order.
    ///
    /// `sqlite_master` holds the exact text of each `CREATE`, so comparing this
    /// between two databases compares their schemas rather than their table
    /// names.
    #[test]
    fn two_connections_opening_the_same_new_database_both_get_a_working_schema() {
        // `apps/recorder` holds two: one answers the window's questions about
        // the library and one reconciles it against the disk. On a machine with
        // no library yet they open it at the same moment, and before this was
        // held by a test the second one failed with `table games already exists`
        // — which reached the window as "the recording library could not be
        // opened", on exactly the first run, for exactly the users who had never
        // recorded anything.
        let directory = scratch_directory("concurrent-open");
        let path = directory.join("library.db");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));

        let openers: Vec<std::thread::JoinHandle<Result<MigrationOutcome, StorageError>>> = (0..4)
            .map(|_| {
                let path = path.clone();
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let mut connection = Connection::open(&path).expect("the file can be opened");
                    connection
                        .busy_timeout(std::time::Duration::from_secs(30))
                        .expect("a busy timeout can be set");
                    barrier.wait();
                    migrate(&mut connection, Some(&path), MIGRATIONS)
                })
            })
            .collect();

        let mut applied = 0;
        for opener in openers {
            let outcome = opener
                .join()
                .expect("the thread did not panic")
                .expect("every connection has to end up with a usable schema");
            assert_eq!(outcome.to, SCHEMA_VERSION);
            applied += outcome.applied.len();
        }
        assert_eq!(
            applied,
            MIGRATIONS.len(),
            "each migration must be applied once in total, whichever connection got there first"
        );

        let connection = Connection::open(&path).expect("the file can be opened");
        assert!(table_names(&connection).contains(&"sessions".to_owned()));
    }

    #[test]
    fn rebuilding_sessions_keeps_every_row_and_everything_hanging_off_it() {
        // Migration 2 drops and re-creates `sessions`, which is the shape of
        // change AGENTS.md section 56 is about: a library that lost a row here
        // has lost the route to somebody's recordings, and the recordings are
        // what cannot be made again. `every_shipped_version_migrates_forwards`
        // proves the *schema* survives; this proves the rows do, including the
        // one column no sidecar can rewrite — a favourite the user set.
        let mut connection = in_memory();
        migrate(&mut connection, None, &MIGRATIONS[..1]).expect("a version 1 database is built");

        connection
            .execute_batch(
                "INSERT INTO games (game_id, name, first_seen_at) \
                     VALUES ('cs2', 'Counter-Strike 2', '2026-08-11T20:14:00+01:00'); \
                 INSERT INTO sessions \
                     (session_id, game_id, started_at, ended_at, end_reason, sidecar_path, \
                      favourited_at) \
                     VALUES ('cs2-20260811-201400', 'cs2', '2026-08-11T20:14:00+01:00', \
                             '2026-08-11T21:00:00+01:00', 'game-exited', \
                             'D:\\clips\\cs2.session.json', '2026-08-12T09:00:00+01:00'); \
                 INSERT INTO session_events (session_id, at, kind) \
                     VALUES ('cs2-20260811-201400', '2026-08-11T20:14:00+01:00', \
                             'session-started'); \
                 INSERT INTO session_game_candidates (session_id, game_id) \
                     VALUES ('cs2-20260811-201400', 'half-life-2'); \
                 INSERT INTO recordings \
                     (session_id, session_index, path, started_at, size_bytes) \
                     VALUES ('cs2-20260811-201400', 1, 'D:\\clips\\cs2.mkv', \
                             '2026-08-11T20:14:00+01:00', 1024);",
            )
            .expect("a version 1 library can be filled");

        migrate(&mut connection, None, MIGRATIONS).expect("it migrates forwards");

        let session: (String, Option<String>, Option<String>, Option<String>) = connection
            .query_row(
                "SELECT session_id, game_id, end_reason, favourited_at FROM sessions",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("the session survived");
        assert_eq!(session.0, "cs2-20260811-201400");
        assert_eq!(session.1.as_deref(), Some("cs2"));
        assert_eq!(session.2.as_deref(), Some("game-exited"));
        assert_eq!(
            session.3.as_deref(),
            Some("2026-08-12T09:00:00+01:00"),
            "a favourite is the user's own and is not derived from any file"
        );

        for table in ["session_events", "session_game_candidates", "recordings"] {
            let rows: i64 = connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("the table can be counted");
            assert_eq!(rows, 1, "{table} lost its row when `sessions` was rebuilt");
        }

        // The reason the migration exists, and the check that would fail if the
        // rebuilt table had been created with the old vocabulary.
        connection
            .execute("UPDATE sessions SET end_reason = 'recording-ended'", [])
            .expect("a session may now end because its recording did");
    }

    #[test]
    fn a_session_may_not_end_for_a_reason_nothing_writes() {
        // The other half: the column is a vocabulary rather than free text, so
        // a writer's typo is a failed insert instead of a session no filter
        // ever matches. Without this, the migration above would pass just as
        // well having dropped the constraint altogether.
        let mut connection = in_memory();
        migrate(&mut connection, None, MIGRATIONS).expect("a fresh database migrates");

        connection
            .execute(
                "INSERT INTO sessions (session_id, started_at, end_reason) \
                 VALUES ('s', '2026-08-11T20:14:00+01:00', 'recording-finished')",
                [],
            )
            .expect_err("`recording-finished` is not a reason anything writes");
    }

    fn schema_of(connection: &Connection) -> Vec<(String, String, String)> {
        let mut statement = connection
            .prepare(
                "SELECT type, name, COALESCE(sql, '') FROM sqlite_master \
                 WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
            )
            .expect("the schema can be read");
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("the schema can be listed");
        rows.map(|row| row.expect("a schema row")).collect()
    }
}
