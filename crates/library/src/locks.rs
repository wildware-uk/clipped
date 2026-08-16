//! Marking a sitting or a recording as one automatic cleanup may not take.
//!
//! [Issue #472](https://github.com/wildware-uk/clipped/issues/472).
//! `crate::accounting::cleanup` has always had a vocabulary of reasons a sweep
//! leaves a recording alone, and its scope named four. Two shipped. This is the
//! third, and the column it reads was added by
//! `0006_locked_media.sql` because there was nothing in the schema to read.
//!
//! # What a lock means, and what it does not
//!
//! **It protects against automatic cleanup only.** A lock says "the sweep may
//! not take this". It does not make the delete button ask twice, and it does not
//! stop anything a person does deliberately — deleting a locked recording by
//! hand works exactly as it did.
//!
//! That is a decision rather than an omission. Guarding a manual delete is a
//! different feature about a different risk: automatic cleanup deletes things
//! nobody was looking at, and a confirmation dialogue guards against a slip of
//! the hand. Putting the second behind a storage column would have the storage
//! system deciding how the Library screen behaves.
//!
//! # It survives the trash
//!
//! Lock a recording, delete it by hand, restore it, and it is still locked.
//! Nothing here makes that true and nothing has to: `locked_at` is a column on
//! the row, the trash sets and clears `deleted_at`, and neither touches the
//! other (`crate::trash`). It is asserted rather than assumed, because a
//! recording that came back unprotected would be a trap — you would have to
//! know to lock it again, and the only sign that you had not would be its
//! absence later.
//!
//! # Locking a sitting locks its recordings
//!
//! This is the one place locks differ from [`crate::favourites`], which are
//! independent by design. A favourite is a statement about one thing; a lock is
//! a statement about what may be deleted, and the unit somebody thinks in when
//! they decide that is the night, not the file.
//!
//! The mark is still **not written down through the children**. Locking a
//! session sets `sessions.locked_at` and nothing else; the sweep reads it and
//! reports [`Protection::LockedSession`](crate::accounting::cleanup::Protection::LockedSession).
//! Writing it down would make unlocking leave a trail of locks behind it, which
//! is the argument `crate::favourites` makes and it is no weaker here.
//!
//! The cost, stated plainly: a recording inside a locked sitting cannot be
//! individually unlocked, because the sitting's lock is what protects it. The
//! sweep says *which* lock, so that is visible rather than mysterious.
//!
//! # Clips cannot be locked
//!
//! Automatic cleanup deletes recordings and reads only the `recordings` table,
//! so a lock on a clip would be a mark nothing consults — and a clip already
//! protects the recording it was cut from through `Protection::SourceOfClips`.
//! [`Lockable`] is the whole vocabulary, so this is absent rather than accepted
//! and ignored.
//!
//! # Time is the caller's
//!
//! `locked_at` stores *when*, for the reason `favourited_at` does: "since when"
//! is the question somebody asks looking at a list of things cleanup would not
//! take. The instant is passed in rather than read here, so a test does not have
//! to wait for a clock.

use std::time::SystemTime;

use clipped_storage::rusqlite::{self, params};
use clipped_storage::Database;

/// What can be locked.
///
/// Clips are deliberately absent; see the module documentation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Lockable {
    /// A whole sitting, which protects every recording in it.
    Session(String),
    /// One recording.
    Recording(i64),
}

impl Lockable {
    /// The table and key column a lock for this goes in.
    const fn table(&self) -> (&'static str, &'static str) {
        match self {
            Self::Session(_) => ("sessions", "session_id"),
            Self::Recording(_) => ("recordings", "recording_id"),
        }
    }
}

impl core::fmt::Display for Lockable {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Session(id) => write!(formatter, "session {id}"),
            Self::Recording(id) => write!(formatter, "recording {id}"),
        }
    }
}

/// Locks `what`, at `at`.
///
/// Locking something already locked leaves the original instant alone: the
/// answer to "when did you lock this" is when it was first locked.
///
/// Answers whether this call is what changed it.
///
/// # Errors
///
/// Whatever SQLite reported. A target that is not in the database is **not** an
/// error and locks nothing — the row may have gone between a screen drawing it
/// and somebody clicking it, and that is not worth a failure.
pub fn lock(
    database: &Database,
    what: &Lockable,
    at: SystemTime,
) -> Result<bool, rusqlite::Error> {
    let (table, id_column) = what.table();
    let statement = format!(
        "UPDATE {table} SET locked_at = ?1 WHERE {id_column} = ?2 AND locked_at IS NULL"
    );
    let stamp = rfc3339(at);

    let changed = match what {
        Lockable::Session(id) => database
            .connection()
            .execute(&statement, params![stamp, id])?,
        Lockable::Recording(id) => database
            .connection()
            .execute(&statement, params![stamp, id])?,
    };
    Ok(changed > 0)
}

/// Unlocks `what`.
///
/// # Errors
///
/// Whatever SQLite reported.
pub fn unlock(database: &Database, what: &Lockable) -> Result<bool, rusqlite::Error> {
    let (table, id_column) = what.table();
    let statement = format!("UPDATE {table} SET locked_at = NULL WHERE {id_column} = ?1");

    let changed = match what {
        Lockable::Session(id) => database.connection().execute(&statement, params![id])?,
        Lockable::Recording(id) => database.connection().execute(&statement, params![id])?,
    };
    Ok(changed > 0)
}

/// Whether `what` is locked **by its own lock**.
///
/// A recording inside a locked sitting answers `false` here and is still
/// protected: that protection is the sitting's, and this is a question about
/// this row. What a sweep does with the pair is
/// `crate::accounting::cleanup::candidates`, and
/// [`protects`] is the question a screen asks.
///
/// `false` for something that is not there, for the reason [`lock`] gives.
///
/// # Errors
///
/// Whatever SQLite reported.
pub fn is_locked(database: &Database, what: &Lockable) -> Result<bool, rusqlite::Error> {
    let (table, id_column) = what.table();
    let statement = format!("SELECT locked_at IS NOT NULL FROM {table} WHERE {id_column} = ?1");
    let mut prepared = database.connection().prepare(&statement)?;

    let found = match what {
        Lockable::Session(id) => prepared.query_row(params![id], |row| row.get(0)),
        Lockable::Recording(id) => prepared.query_row(params![id], |row| row.get(0)),
    };
    match found {
        Ok(locked) => Ok(locked),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Whether anything is stopping a sweep taking this recording's file.
///
/// The recording's own lock **or** its sitting's, which is what the cascade
/// means. A screen drawing a padlock against a row wants this rather than
/// [`is_locked`]: showing a recording as unlocked when the sweep will not touch
/// it is a window disagreeing with the product.
///
/// `false` for a recording that is not there.
///
/// # Errors
///
/// Whatever SQLite reported.
pub fn protects(database: &Database, recording: i64) -> Result<bool, rusqlite::Error> {
    let found = database.connection().query_row(
        "SELECT r.locked_at IS NOT NULL OR COALESCE(s.locked_at IS NOT NULL, 0) \
         FROM recordings r LEFT JOIN sessions s ON s.session_id = r.session_id \
         WHERE r.recording_id = ?1",
        params![recording],
        |row| row.get(0),
    );
    match found {
        Ok(locked) => Ok(locked),
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
