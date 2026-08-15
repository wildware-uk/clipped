//! Choosing what to delete when a limit is breached, and refusing to delete the
//! rest.
//!
//! [Issue #111](https://github.com/wildware-uk/clipped/issues/111). The rest of
//! [`crate::accounting`] measures what the library occupies and never deletes
//! anything; this is the part that acts on it, and it is the most dangerous
//! thing the application does.
//!
//! Three things shape it.
//!
//! **Nothing is deleted, only trashed.** Every removal goes through
//! [`crate::trash`], which moves the file and keeps the row, so every automatic
//! deletion is recoverable by the person it happened to (SPEC.md section 28).
//! Emptying the trash stays a thing somebody chooses.
//!
//! **The plan and the deletion are the same code.** [`plan`] decides, [`apply`]
//! carries it out, and a dry run is [`plan`] on its own. A dry run computed
//! differently from the real thing is a dry run that lies.
//!
//! **Every candidate carries a verdict.** [`CleanupPlan`] lists what would go
//! *and* what would not with the rule that saved it, so a log line and a "review
//! large recordings" screen come from the same place. A plan that only listed
//! the doomed could not explain why the disk is still full.
//!
//! # What protects a recording
//!
//! Two rules, and both are in the data model:
//!
//! - **A favourite is never deleted automatically.** `favourited_at` is the
//!   user saying this one matters — and a recording whose *session* is
//!   favourited is protected too, which is what favouriting a sitting means
//!   ([`crate::favourites`] documents the rule and why the mark is not written
//!   down through the children).
//! - **Neither is a recording some clip was cut from.** The clip survives its
//!   source going (`ON DELETE SET NULL`), but it stops being possible to go back
//!   to the moment it came from, which is not a thing to do to somebody without
//!   asking.
//!
//! The issue's scope names two more — locked recordings, and recordings being
//! edited — and **neither exists to be read**: there is no `locked` column in
//! the schema and nothing records that an edit document is open. They are not
//! silently ignored here; [`Protection`] is the whole vocabulary, and adding one
//! is adding a variant and the rule that produces it.
//!
//! # What is deleted, and in what order
//!
//! Recordings, oldest first, and only recordings. A clip is small and is the
//! thing somebody kept deliberately; a session row with its recordings gone
//! still describes the session it was.
//!
//! The sweep stops as soon as the limits are satisfied, so the newest material
//! survives. If it runs out of unprotected recordings while a limit is still
//! breached, it says so ([`CleanupPlan::still_over_limit`]) rather than
//! reporting success — a disk that is still full after a cleanup is something
//! somebody has to be told about.

use core::time::Duration;
use std::path::PathBuf;
use std::time::SystemTime;

use clipped_storage::Database;

use crate::accounting::limits::StorageLimits;
use crate::trash::{Trash, TrashError, TrashItem};

/// Why a recording was not deleted.
///
/// The whole vocabulary. A recording with no [`Protection`] against it is one
/// this may take, so a rule that is not here is a rule that is not applied —
/// which is the point: the module documentation names the two the schema cannot
/// express, and they are absent rather than assumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protection {
    /// The user marked it a favourite.
    Favourite,
    /// The session it belongs to is a favourite.
    ///
    /// Distinct from [`Self::Favourite`] because the reason a person is given
    /// should be the one that is true: "you favourited this sitting" explains
    /// something they did, and "it is a favourite" would send them looking for a
    /// star that is not on this row.
    FavouriteSession,
    /// Clips were cut from it.
    SourceOfClips {
        /// How many, for a message that says what would be orphaned.
        clips: u32,
    },
    /// It is already in the trash, so there is nothing to send there.
    AlreadyDeleted,
    /// Its file is not on disk, so deleting it would reclaim nothing.
    Missing,
}

impl core::fmt::Display for Protection {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Favourite => formatter.write_str("it is a favourite"),
            Self::FavouriteSession => {
                formatter.write_str("the sitting it belongs to is a favourite")
            }
            Self::SourceOfClips { clips } => {
                write!(formatter, "{clips} clips were cut from it")
            }
            Self::AlreadyDeleted => formatter.write_str("it is already in the trash"),
            Self::Missing => formatter.write_str("its file is not on disk"),
        }
    }
}

/// One recording the sweep considered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Which recording it is.
    pub item: TrashItem,
    /// Where its file is.
    pub path: PathBuf,
    /// What it occupies, or zero when nothing has measured it.
    pub size_bytes: u64,
    /// When it started, which is the order this deletes in.
    pub started_at: String,
    /// Why it cannot be deleted, if it cannot.
    pub protection: Option<Protection>,
}

impl Candidate {
    /// Whether this sweep may take it.
    #[must_use]
    pub const fn is_deletable(&self) -> bool {
        self.protection.is_none()
    }
}

/// What a cleanup would do, before it does any of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupPlan {
    /// What would be sent to the trash, oldest first.
    pub deletions: Vec<Candidate>,
    /// What would be kept, and why.
    pub protected: Vec<Candidate>,
    /// What the deletions would free.
    pub reclaimed_bytes: u64,
    /// What still has to go after all of them, or zero if the limits are met.
    ///
    /// Non-zero means the sweep ran out of things it is allowed to delete. The
    /// caller has to say so rather than report a cleanup that worked.
    pub still_over_limit: u64,
}

impl CleanupPlan {
    /// A plan that would do nothing, because nothing needs doing.
    #[must_use]
    pub const fn nothing_to_do() -> Self {
        Self {
            deletions: Vec::new(),
            protected: Vec::new(),
            reclaimed_bytes: 0,
            still_over_limit: 0,
        }
    }

    /// Whether this plan would delete anything at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.deletions.is_empty()
    }
}

/// What a cleanup actually did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupOutcome {
    /// What went to the trash.
    pub deleted: Vec<TrashItem>,
    /// What it freed.
    pub reclaimed_bytes: u64,
    /// What could not be sent, with the reason the trash gave.
    ///
    /// A cleanup carries on past one of these: a recording whose file has gone
    /// between the plan and the sweep must not stop the rest.
    pub refused: Vec<(TrashItem, String)>,
}

/// How much has to be freed for `limits` to be met.
///
/// `usage` is what the library occupies and `free` is what is left on the
/// volume. Both are measured by [`crate::accounting`]; neither is guessed here.
#[must_use]
pub fn excess(limits: &StorageLimits, usage: u64, free: u64) -> u64 {
    let over_quota = limits
        .maximum_usage()
        .map_or(0, |maximum| usage.saturating_sub(maximum));
    let under_free = limits
        .minimum_free_space()
        .map_or(0, |minimum| minimum.saturating_sub(free));
    over_quota.max(under_free)
}

/// Every recording the library knows about, with the protections against it.
///
/// One query rather than one per rule: a recording is read once with its
/// favourite mark, its clip count and its file's state, so a plan cannot be
/// built from a half-consistent view of the database.
///
/// # Errors
///
/// Whatever SQLite reported.
pub fn candidates(database: &Database) -> Result<Vec<Candidate>, clipped_storage::rusqlite::Error> {
    let mut statement = database.connection().prepare(
        "SELECT r.recording_id, r.path, COALESCE(r.size_bytes, 0), r.started_at, \
                r.favourited_at IS NOT NULL, r.deleted_at IS NOT NULL, \
                r.missing_since IS NOT NULL, \
                (SELECT COUNT(*) FROM clips c WHERE c.source_recording_id = r.recording_id), \
                COALESCE(s.favourited_at IS NOT NULL, 0) \
         FROM recordings r LEFT JOIN sessions s ON s.session_id = r.session_id",
    )?;

    let rows = statement.query_map([], |row| {
        let id: i64 = row.get(0)?;
        let path: String = row.get(1)?;
        let size_bytes: i64 = row.get(2)?;
        let started_at: String = row.get(3)?;
        let favourite: bool = row.get(4)?;
        let deleted: bool = row.get(5)?;
        let missing: bool = row.get(6)?;
        let clips: i64 = row.get(7)?;
        let session_favourite: bool = row.get(8)?;

        // In the order somebody would want to be told. "It is a favourite"
        // explains a decision; "its file is not on disk" explains a
        // measurement, and the first is the more useful of the two.
        let protection = if favourite {
            Some(Protection::Favourite)
        } else if session_favourite {
            Some(Protection::FavouriteSession)
        } else if clips > 0 {
            Some(Protection::SourceOfClips {
                clips: u32::try_from(clips).unwrap_or(u32::MAX),
            })
        } else if deleted {
            Some(Protection::AlreadyDeleted)
        } else if missing {
            Some(Protection::Missing)
        } else {
            None
        };

        Ok(Candidate {
            item: TrashItem::recording(id),
            path: PathBuf::from(path),
            size_bytes: u64::try_from(size_bytes).unwrap_or(0),
            started_at,
            protection,
        })
    })?;

    rows.collect()
}

/// Decides what a cleanup would take, without taking any of it.
///
/// The candidates are every recording the library knows about, in any order;
/// this sorts them. A caller reads them from the database with
/// [`candidates`].
///
/// The age limit is applied first and separately: a recording older than the
/// maximum is deleted whether or not the disk is full, because that is what a
/// maximum age means. The size limits then take as many of the oldest remaining
/// as they need.
#[must_use]
pub fn plan(
    limits: &StorageLimits,
    mut candidates: Vec<Candidate>,
    usage: u64,
    free: u64,
    now: SystemTime,
) -> CleanupPlan {
    // Oldest first, which is the order the issue specifies and the order a
    // person would expect: the newest recording is the one they are most likely
    // to want.
    candidates.sort_by(|left, right| left.started_at.cmp(&right.started_at));

    let mut deletions = Vec::new();
    let mut protected = Vec::new();
    let mut reclaimed = 0_u64;
    let mut wanted = excess(limits, usage, free);

    for candidate in candidates {
        if candidate.protection.is_some() {
            protected.push(candidate);
            continue;
        }

        let too_old = limits
            .maximum_age()
            .is_some_and(|maximum| older_than(&candidate.started_at, maximum, now));
        if !too_old && wanted == 0 {
            // Everything from here on is newer and the limits are met.
            protected.push(candidate);
            continue;
        }

        reclaimed = reclaimed.saturating_add(candidate.size_bytes);
        wanted = wanted.saturating_sub(candidate.size_bytes);
        deletions.push(candidate);
    }

    CleanupPlan {
        deletions,
        protected,
        reclaimed_bytes: reclaimed,
        still_over_limit: wanted,
    }
}

/// Whether a recording that started at `started_at` is older than `maximum`.
///
/// A timestamp that will not parse is treated as **not** too old. The
/// alternative — deleting a recording because its timestamp is unreadable — is
/// the worst possible reading of an unreadable field (AGENTS.md section 56).
fn older_than(started_at: &str, maximum: Duration, now: SystemTime) -> bool {
    let Some(started) = parse_timestamp(started_at) else {
        return false;
    };
    now.duration_since(started).is_ok_and(|age| age > maximum)
}

/// Reads one of the database's timestamps.
///
/// They are RFC 3339 as `clipped-storage` writes them.
fn parse_timestamp(value: &str) -> Option<SystemTime> {
    let parsed =
        time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).ok()?;
    let seconds = parsed.unix_timestamp();
    if seconds < 0 {
        return None;
    }
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds.unsigned_abs()))
}

/// Carries out a plan, sending every deletion to the trash.
///
/// Nothing is unlinked. Each recording is moved into the trash with its row
/// kept, so the whole of this is recoverable.
///
/// Every deletion is logged at `info` with the reason it was chosen, and every
/// refusal at `warn`. A refusal does not stop the sweep: a recording whose file
/// went between the plan and now must not cost the rest of the cleanup.
///
/// # Errors
///
/// Never. A trash failure for one item is recorded in
/// [`CleanupOutcome::refused`] and the rest carry on; the signature returns a
/// result only so that a caller can be given one place to look.
pub fn apply(
    plan: &CleanupPlan,
    trash: &Trash,
    database: &mut Database,
    at: SystemTime,
) -> Result<CleanupOutcome, TrashError> {
    let mut deleted = Vec::new();
    let mut refused = Vec::new();
    let mut reclaimed = 0_u64;

    for candidate in &plan.deletions {
        match trash.send(database, candidate.item, at) {
            Ok(_) => {
                tracing::info!(
                    item = %candidate.item,
                    path = %clipped_logging::RedactedPath::new(&candidate.path),
                    size_bytes = candidate.size_bytes,
                    "a storage limit was over, so this recording was moved to the trash"
                );
                reclaimed = reclaimed.saturating_add(candidate.size_bytes);
                deleted.push(candidate.item);
            }
            Err(error) => {
                tracing::warn!(
                    item = %candidate.item,
                    %error,
                    "this recording was chosen for automatic cleanup and could not be moved \
                     to the trash; the rest of the cleanup carries on"
                );
                refused.push((candidate.item, error.to_string()));
            }
        }
    }

    if plan.still_over_limit > 0 {
        tracing::warn!(
            short_bytes = plan.still_over_limit,
            protected = plan.protected.len(),
            "automatic cleanup ran out of recordings it is allowed to delete and the storage \
             limit is still over; the remaining recordings are favourites or have clips cut \
             from them"
        );
    }

    Ok(CleanupOutcome {
        deleted,
        reclaimed_bytes: reclaimed,
        refused,
    })
}

#[cfg(test)]
mod tests;
