//! Marking a session, a recording or a clip as one to keep.
//!
//! [Issue #58](https://github.com/wildware-uk/clipped/issues/58). The database
//! has carried `favourited_at` on all three tables since the first migration,
//! and everything that *reads* a library already reports it — [`crate::index`]
//! puts it on every row it returns and [`crate::search`] has a `favourite`
//! filter. **Nothing could set one.** This is the half that was missing.
//!
//! Reaching it is the other half, and for a while this module was itself the
//! thing nobody called: the recorder answers `set_favourite` with it
//! (`apps/recorder/src/library.rs`) and the Library screen's Keep control is
//! what sends one, so a mark a user makes ends up here.
//!
//! # What favouriting a session means
//!
//! The issue asks for this to be documented rather than left to be discovered,
//! and there are two defensible answers. This is the one taken:
//!
//! **A session's favourite is its own, and it protects the recordings in it.**
//! Marking a session does not write `favourited_at` on its recordings — they are
//! not individually favourite, and unfavouriting the session must not leave a
//! trail of marks behind it — but automatic cleanup treats a recording whose
//! session is favourited as protected
//! ([`crate::accounting::cleanup`]).
//!
//! The alternative — writing the mark down through the children — was rejected
//! because it cannot be undone faithfully. A user who favourites a session,
//! favourites one recording inside it deliberately, and then unfavourites the
//! session would expect that one recording to survive; a cascade either clears
//! it or has to remember which marks it made.
//!
//! # Time is the caller's
//!
//! `favourited_at` stores *when*, so the library can order by it later, and the
//! instant is passed in rather than read here — every other write in this crate
//! takes its `at` the same way, so a test does not have to wait for a clock.

use std::time::SystemTime;

use clipped_storage::rusqlite::{self, params};
use clipped_storage::Database;

/// What can be favourited.
///
/// Screenshots are named in the issue's scope and are deliberately absent: the
/// schema has no table for them, so there is nowhere to put the mark. Adding one
/// is adding a variant here and a column there.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Favourite {
    /// A whole sitting.
    Session(String),
    /// One media file.
    Recording(i64),
    /// One clip cut from a recording.
    Clip(i64),
}

impl Favourite {
    /// The table and column a mark for this goes in.
    const fn table(&self) -> (&'static str, &'static str) {
        match self {
            Self::Session(_) => ("sessions", "session_id"),
            Self::Recording(_) => ("recordings", "recording_id"),
            Self::Clip(_) => ("clips", "clip_id"),
        }
    }
}

impl core::fmt::Display for Favourite {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Session(id) => write!(formatter, "session {id}"),
            Self::Recording(id) => write!(formatter, "recording {id}"),
            Self::Clip(id) => write!(formatter, "clip {id}"),
        }
    }
}

/// Marks `what` as a favourite, at `at`.
///
/// Marking something already marked leaves the original instant alone: the
/// answer to "when did you favourite this" is when it was first favourited, and
/// a second click on a full star should not silently change it.
///
/// # Errors
///
/// Whatever SQLite reported. A target that is not in the database is **not** an
/// error and marks nothing — the row may have been removed between a screen
/// drawing it and somebody clicking it, and that is not worth a failure.
pub fn mark(
    database: &Database,
    what: &Favourite,
    at: SystemTime,
) -> Result<bool, rusqlite::Error> {
    let (table, id_column) = what.table();
    let statement = format!(
        "UPDATE {table} SET favourited_at = ?1 \
         WHERE {id_column} = ?2 AND favourited_at IS NULL"
    );
    let stamp = rfc3339(at);

    let changed = match what {
        Favourite::Session(id) => database
            .connection()
            .execute(&statement, params![stamp, id])?,
        Favourite::Recording(id) | Favourite::Clip(id) => database
            .connection()
            .execute(&statement, params![stamp, id])?,
    };
    Ok(changed > 0)
}

/// Clears the mark on `what`.
///
/// # Errors
///
/// Whatever SQLite reported.
pub fn unmark(database: &Database, what: &Favourite) -> Result<bool, rusqlite::Error> {
    let (table, id_column) = what.table();
    let statement = format!("UPDATE {table} SET favourited_at = NULL WHERE {id_column} = ?1");

    let changed = match what {
        Favourite::Session(id) => database.connection().execute(&statement, params![id])?,
        Favourite::Recording(id) | Favourite::Clip(id) => {
            database.connection().execute(&statement, params![id])?
        }
    };
    Ok(changed > 0)
}

/// Whether `what` is favourited.
///
/// `false` for something that is not there, for the reason [`mark`] gives.
///
/// # Errors
///
/// Whatever SQLite reported.
pub fn is_marked(database: &Database, what: &Favourite) -> Result<bool, rusqlite::Error> {
    let (table, id_column) = what.table();
    let statement = format!("SELECT favourited_at IS NOT NULL FROM {table} WHERE {id_column} = ?1");
    let mut prepared = database.connection().prepare(&statement)?;

    let found = match what {
        Favourite::Session(id) => prepared.query_row(params![id], |row| row.get(0)),
        Favourite::Recording(id) | Favourite::Clip(id) => {
            prepared.query_row(params![id], |row| row.get(0))
        }
    };
    match found {
        Ok(marked) => Ok(marked),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
        Err(error) => Err(error),
    }
}

/// An instant as the database writes them.
fn rfc3339(at: SystemTime) -> String {
    let seconds = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| i64::try_from(since.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    time::OffsetDateTime::from_unix_timestamp(seconds)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

#[cfg(test)]
mod tests;
