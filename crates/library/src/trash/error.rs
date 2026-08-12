//! Everything the trash can refuse to do, and what the disk looks like
//! afterwards.
//!
//! Every variant here documents the state it leaves behind, because that is the
//! only thing a caller can act on: a delete that failed is only safe if the
//! footage is still where it was, and the one case where it is not
//! ([`TrashError::Stranded`]) names both paths so a person can finish the job by
//! hand.

use std::fmt;
use std::io;
use std::path::PathBuf;

use super::TrashItem;

/// Why an operation on the trash could not be completed.
#[derive(Debug)]
#[non_exhaustive]
pub enum TrashError {
    /// The index has no such recording or clip. Nothing was touched.
    NoSuchItem {
        /// What was asked for.
        item: TrashItem,
    },
    /// It is in the trash already. Nothing was touched.
    AlreadyInTrash {
        /// What was asked for.
        item: TrashItem,
        /// When it was put there.
        deleted_at: String,
    },
    /// It is not in the trash, and the operation only applies to things that
    /// are. Nothing was touched.
    ///
    /// This is the interlock that makes permanent deletion safe: the only way to
    /// destroy footage is to destroy something that is already in the trash, so
    /// a live recording can never be reached by one call.
    NotInTrash {
        /// What was asked for.
        item: TrashItem,
    },
    /// The file and the trash are on different volumes. Nothing was touched.
    ///
    /// Deleting is a rename, never a copy — see the module documentation for why
    /// — and a rename cannot cross a volume. A library spread over two drives
    /// needs a trash on each.
    DifferentVolume {
        /// The file that was to be deleted.
        file: PathBuf,
        /// The trash it could not be moved into.
        trash: PathBuf,
    },
    /// A directory the operation needed could not be created. Nothing was
    /// touched.
    CreateDirectory {
        /// The directory.
        path: PathBuf,
        /// What the operating system said.
        source: io::Error,
    },
    /// The file could not be moved. It is still where it was.
    Move {
        /// Where it is.
        from: PathBuf,
        /// Where it was going.
        to: PathBuf,
        /// What the operating system said.
        source: io::Error,
    },
    /// The file could not be removed. It is still in the trash and its row is
    /// unchanged, so the next sweep tries again.
    Remove {
        /// The file.
        path: PathBuf,
        /// What the operating system said.
        source: io::Error,
    },
    /// The index refused the change, and the file has been put back where it
    /// was.
    ///
    /// The file always wins: a row that could not be written costs an index
    /// entry, and an index can be rebuilt from the session sidecars beside the
    /// recordings (`docs/storage.md`).
    Database(clipped_storage::rusqlite::Error),
    /// The index refused the change and the file could **not** be put back.
    ///
    /// The one state this module can leave that a user has to resolve, so both
    /// paths are named. It needs two filesystem failures in a row to reach.
    Stranded {
        /// Where the file is now.
        file: PathBuf,
        /// Where it should be.
        belongs_at: PathBuf,
        /// Why it could not be moved there.
        source: io::Error,
    },
    /// The trash is not what the user was shown when they confirmed emptying it.
    /// Nothing was touched.
    Changed {
        /// How many items the confirmation was for.
        confirmed_items: usize,
        /// How many bytes it was for.
        confirmed_bytes: u64,
        /// How many items are in the trash now.
        found_items: usize,
        /// How many bytes are in it now.
        found_bytes: u64,
    },
}

impl From<clipped_storage::rusqlite::Error> for TrashError {
    fn from(error: clipped_storage::rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

impl fmt::Display for TrashError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSuchItem { item } => write!(f, "the library has no {item}"),
            Self::AlreadyInTrash { item, deleted_at } => {
                write!(f, "{item} was already deleted, at {deleted_at}")
            }
            Self::NotInTrash { item } => write!(f, "{item} is not in the trash"),
            Self::DifferentVolume { file, trash } => write!(
                f,
                "'{}' is not on the same drive as the trash at '{}', so it cannot be moved there \
                 without being copied",
                file.display(),
                trash.display()
            ),
            Self::CreateDirectory { path, source } => {
                write!(f, "'{}' could not be created: {source}", path.display())
            }
            Self::Move { from, to, source } => write!(
                f,
                "'{}' could not be moved to '{}': {source}",
                from.display(),
                to.display()
            ),
            Self::Remove { path, source } => {
                write!(f, "'{}' could not be removed: {source}", path.display())
            }
            Self::Database(error) => write!(
                f,
                "the library index could not be updated, and the file was put back: {error}"
            ),
            Self::Stranded {
                file,
                belongs_at,
                source,
            } => write!(
                f,
                "the library index could not be updated and '{}' could not be moved back to \
                 '{}': {source}",
                file.display(),
                belongs_at.display()
            ),
            Self::Changed {
                confirmed_items,
                confirmed_bytes,
                found_items,
                found_bytes,
            } => write!(
                f,
                "the trash held {confirmed_items} item(s) and {confirmed_bytes} byte(s) when this \
                 was confirmed and holds {found_items} and {found_bytes} now, so nothing was \
                 emptied"
            ),
        }
    }
}

impl std::error::Error for TrashError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CreateDirectory { source, .. }
            | Self::Move { source, .. }
            | Self::Remove { source, .. }
            | Self::Stranded { source, .. } => Some(source),
            Self::Database(error) => Some(error),
            _ => None,
        }
    }
}
