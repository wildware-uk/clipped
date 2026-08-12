//! What can go wrong while indexing, and the difference between the two kinds.
//!
//! Indexing a library meets bad input constantly — a half-written file, a
//! folder Windows will not open, a sidecar from a build that has not shipped
//! yet — and almost none of it is a reason to stop. So there are two types:
//!
//! - [`IndexProblem`] is **per item**. It is reported, the item is skipped, and
//!   the rest of the library still indexes. Nothing is deleted to resolve one.
//! - [`IndexError`] ends the run. There is one cause: the database itself
//!   refused, which means nothing further can be written anyway.
//!
//! Both carry the path or identifier they are about, because "a sidecar could
//! not be read" without saying which is not something anybody can act on
//! (AGENTS.md sections 15 and 45).

use std::fmt;
use std::io;
use std::path::PathBuf;

use clipped_storage::StorageError;

/// Something wrong with one file, one directory or one session.
///
/// Every variant is survivable: the run carries on and the report lists what it
/// could not use.
#[derive(Debug)]
#[non_exhaustive]
pub enum IndexProblem {
    /// A session sidecar could not be read.
    UnreadableSidecar {
        /// The file.
        path: PathBuf,
        /// What the filesystem said.
        error: io::Error,
    },
    /// A session sidecar is not the JSON this build expects.
    ///
    /// The usual cause is a file that was being written when the machine lost
    /// power, which the recorder's write-and-rename makes unlikely but not
    /// impossible on a filesystem that reorders.
    MalformedSidecar {
        /// The file.
        path: PathBuf,
        /// What the parser said, or what is missing from an otherwise valid
        /// file.
        detail: String,
    },
    /// A session sidecar announces a schema version this build does not know.
    ///
    /// Half-reading it would file a session under whatever this build happened
    /// to recognise, so it is left for a build that understands it — the file
    /// is unharmed and re-indexing after an update picks it up.
    UnsupportedSidecarVersion {
        /// The file.
        path: PathBuf,
        /// The version it carries.
        found: u32,
        /// The newest version this build reads.
        supported: u32,
    },
    /// A directory inside a root could not be listed.
    UnreadableDirectory {
        /// The directory.
        path: PathBuf,
        /// What the filesystem said.
        error: io::Error,
    },
    /// A sidecar used a word the schema's vocabulary does not contain.
    ///
    /// The vocabularies are `CHECK` constraints (`docs/storage.md`), so adding
    /// a word is a migration. Until that migration ships, the column is left
    /// empty rather than the session being lost: everything else about it is
    /// still true.
    UnknownToken {
        /// The session the word came from.
        session_id: String,
        /// The column it was destined for.
        field: &'static str,
        /// The word.
        value: String,
    },
    /// A session names a game it does not identify.
    ///
    /// It is indexed as unattributed — the recording is what matters, and a
    /// session with no game is a state the schema models deliberately.
    Unattributable {
        /// The session.
        session_id: String,
        /// What is wrong with the game it named.
        detail: &'static str,
    },
    /// The database refused a session, which was rolled back on its own.
    ///
    /// The rest of the batch is unaffected: every session is written inside a
    /// savepoint for exactly this.
    SessionRefused {
        /// The session.
        session_id: String,
        /// What SQLite said.
        error: clipped_storage::rusqlite::Error,
    },
    /// The database refused one recording of a session, which was rolled back
    /// on its own.
    ///
    /// The likeliest cause is two sessions claiming the same file: `path` is
    /// unique across the whole table, because one file cannot be two
    /// recordings.
    RecordingRefused {
        /// The session it belongs to.
        session_id: String,
        /// Its ordinal within that session.
        session_index: u32,
        /// The file it names.
        path: PathBuf,
        /// What SQLite said.
        error: clipped_storage::rusqlite::Error,
    },
}

impl fmt::Display for IndexProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnreadableSidecar { path, error } => write!(
                f,
                "the session record {} could not be read, so its recordings are not indexed: \
                 {error}",
                path.display()
            ),
            Self::MalformedSidecar { path, detail } => write!(
                f,
                "the session record {} is not readable as JSON, so its recordings are not \
                 indexed: {detail}",
                path.display()
            ),
            Self::UnsupportedSidecarVersion {
                path,
                found,
                supported,
            } => write!(
                f,
                "the session record {} was written by a newer version of Clipped (format \
                 {found}; this build reads {supported}), so it has been left alone",
                path.display()
            ),
            Self::UnreadableDirectory { path, error } => write!(
                f,
                "the folder {} could not be listed, so anything in it is not indexed: {error}",
                path.display()
            ),
            Self::UnknownToken {
                session_id,
                field,
                value,
            } => write!(
                f,
                "session {session_id} describes its {field} as '{value}', which this build's \
                 library does not recognise; the rest of the session is indexed"
            ),
            Self::Unattributable { session_id, detail } => write!(
                f,
                "session {session_id} is indexed without a game because {detail}"
            ),
            Self::SessionRefused { session_id, error } => write!(
                f,
                "session {session_id} could not be written to the library index and was rolled \
                 back on its own: {error}"
            ),
            Self::RecordingRefused {
                session_id,
                session_index,
                path,
                error,
            } => write!(
                f,
                "recording {session_index} of session {session_id} ({}) could not be written to \
                 the library index and was rolled back on its own: {error}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for IndexProblem {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::UnreadableSidecar { error, .. } | Self::UnreadableDirectory { error, .. } => {
                Some(error)
            }
            Self::SessionRefused { error, .. } | Self::RecordingRefused { error, .. } => {
                Some(error)
            }
            Self::MalformedSidecar { .. }
            | Self::UnsupportedSidecarVersion { .. }
            | Self::UnknownToken { .. }
            | Self::Unattributable { .. } => None,
        }
    }
}

/// A failure that ends a reconciliation.
///
/// **None of these can cost a recording.** The media files are ordinary files
/// this crate never opens, the sidecars beside them are untouched, and a run
/// that ends here has committed whatever it had already written — every batch
/// is its own transaction — so the next run carries on rather than starting
/// again.
#[derive(Debug)]
#[non_exhaustive]
pub enum IndexError {
    /// The database refused, so nothing more can be written.
    Database(StorageError),
}

impl fmt::Display for IndexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(error) => write!(
                f,
                "the library index could not be updated, so it may be out of date; \
                 the recordings themselves are untouched: {error}"
            ),
        }
    }
}

impl std::error::Error for IndexError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
        }
    }
}

impl From<StorageError> for IndexError {
    fn from(error: StorageError) -> Self {
        Self::Database(error)
    }
}

impl From<clipped_storage::rusqlite::Error> for IndexError {
    fn from(error: clipped_storage::rusqlite::Error) -> Self {
        Self::Database(StorageError::from(error))
    }
}
