//! The error type returned by fallible operations in this crate.

use std::fmt;
use std::io;
use std::path::PathBuf;

/// Something went wrong opening, migrating or writing to the database.
///
/// Every variant that can be reported to a user names the database file,
/// because every one of them is a state a person may have to act on — a file
/// that is not ours, a build that is too old, a disk that is full — and "the
/// database could not be opened" without saying which file is not something
/// anybody can do anything with (AGENTS.md sections 15 and 45).
///
/// None of these ever means a recording was lost. The media files are ordinary
/// files this crate never opens; see the crate documentation.
#[derive(Debug)]
#[non_exhaustive]
pub enum StorageError {
    /// The directory the database lives in could not be created.
    CreateDirectory {
        /// The directory that could not be created.
        directory: PathBuf,
        /// The underlying filesystem error.
        source: io::Error,
    },
    /// SQLite could not open the file.
    Open {
        /// The database that could not be opened.
        path: PathBuf,
        /// What SQLite said.
        source: rusqlite::Error,
    },
    /// The file exists, holds tables, and is not a Clipped database.
    ///
    /// Refused rather than migrated. Writing Clipped's schema into somebody
    /// else's database would destroy it, and the most likely way to arrive here
    /// is a mistyped path (AGENTS.md section 56).
    NotAClippedDatabase {
        /// The file that was opened.
        path: PathBuf,
        /// The `application_id` found in its header. Zero means "no
        /// application claimed this file", which every SQLite database has by
        /// default.
        application_id: i32,
    },
    /// The database was written by a newer build of Clipped than this one.
    ///
    /// Refused, and **the file is not modified**. An older build that migrated
    /// a newer schema — or simply wrote to it — is how a user loses the library
    /// they built on the machine that was up to date.
    FromNewerVersion {
        /// The database that was opened.
        path: PathBuf,
        /// The schema version found in it.
        found: u32,
        /// The newest schema version this build understands.
        supported: u32,
    },
    /// A read-only connection was asked for a database that has not been
    /// migrated to this build's schema version.
    ///
    /// Readers never migrate: the writer does, once, and a second process
    /// racing it through the same upgrade is not a situation worth having.
    MigrationRequired {
        /// The database that was opened.
        path: PathBuf,
        /// The schema version found in it.
        found: u32,
        /// The schema version this build expects.
        expected: u32,
    },
    /// A migration failed and was rolled back.
    ///
    /// The database is left at the version before it, which is a version some
    /// build of Clipped understood. Nothing is half-applied.
    Migration {
        /// The version that was being applied.
        version: u32,
        /// Its name, as it appears in `migrations/`.
        name: &'static str,
        /// What SQLite said.
        source: rusqlite::Error,
    },
    /// A migration completed but left rows referring to rows that do not
    /// exist, so it was rolled back.
    ///
    /// Checked explicitly because migrations run with foreign key enforcement
    /// off — the standard way to rebuild a table in SQLite — and a rebuild that
    /// forgets to carry the children across is silent without this.
    MigrationBrokeReferences {
        /// The version that was being applied.
        version: u32,
        /// Its name, as it appears in `migrations/`.
        name: &'static str,
        /// How many dangling references it left.
        violations: usize,
    },
    /// The copy taken before migrating could not be written.
    ///
    /// Migration stops here, without having changed the database. A backup that
    /// cannot be made is a reason not to start, not a reason to carry on
    /// without one (AGENTS.md section 56).
    Backup {
        /// Where the copy was to be written.
        destination: PathBuf,
        /// What SQLite said.
        source: rusqlite::Error,
    },
    /// The background writer thread panicked, so what it was holding was lost.
    ///
    /// A queued write is the caller's closure — this crate does not know what a
    /// row means — and it runs on a thread of its own, so a panic inside one
    /// takes the writer down and the batch it was in with it. It has a variant
    /// of its own because whoever finds this in a user's log has to be sent
    /// looking for a panic, and an SQLite error would send them looking for a
    /// statement that was never issued (AGENTS.md section 15).
    ///
    /// Nothing that was already committed is affected, and no media file is:
    /// the writer only ever wrote metadata.
    WriterPanicked,
    /// SQLite refused a statement outside migration.
    Sqlite(rusqlite::Error),
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CreateDirectory { directory, .. } => write!(
                f,
                "the directory for the Clipped database could not be created: {}",
                directory.display()
            ),
            Self::Open { path, .. } => {
                write!(
                    f,
                    "the Clipped database could not be opened: {}",
                    path.display()
                )
            }
            Self::NotAClippedDatabase {
                path,
                application_id,
            } => write!(
                f,
                "{} is an SQLite database that does not belong to Clipped \
                 (application_id {application_id}); it has been left alone",
                path.display()
            ),
            Self::FromNewerVersion {
                path,
                found,
                supported,
            } => write!(
                f,
                "{} was written by a newer version of Clipped (schema {found}; \
                 this build understands {supported}); update Clipped to open it",
                path.display()
            ),
            Self::MigrationRequired {
                path,
                found,
                expected,
            } => write!(
                f,
                "{} is at schema {found} and this build reads schema {expected}; \
                 it has to be opened for writing once so that it can be migrated",
                path.display()
            ),
            Self::Migration {
                version,
                name,
                source,
            } => write!(
                f,
                "database migration {version} ({name}) failed and was rolled back: {source}"
            ),
            Self::MigrationBrokeReferences {
                version,
                name,
                violations,
            } => write!(
                f,
                "database migration {version} ({name}) left {violations} row(s) \
                 referring to rows that do not exist, and was rolled back"
            ),
            Self::Backup {
                destination,
                source,
            } => write!(
                f,
                "the copy taken before migrating could not be written to {}, \
                 so the database was left unchanged: {source}",
                destination.display()
            ),
            Self::WriterPanicked => write!(
                f,
                "the Clipped database writer thread panicked, so the writes it had not yet \
                 committed were lost; recordings are unaffected"
            ),
            Self::Sqlite(source) => write!(f, "the Clipped database refused a statement: {source}"),
        }
    }
}

impl std::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CreateDirectory { source, .. } => Some(source),
            Self::Open { source, .. }
            | Self::Migration { source, .. }
            | Self::Backup { source, .. }
            | Self::Sqlite(source) => Some(source),
            Self::NotAClippedDatabase { .. }
            | Self::FromNewerVersion { .. }
            | Self::MigrationRequired { .. }
            | Self::MigrationBrokeReferences { .. }
            | Self::WriterPanicked => None,
        }
    }
}

impl From<rusqlite::Error> for StorageError {
    fn from(source: rusqlite::Error) -> Self {
        Self::Sqlite(source)
    }
}
