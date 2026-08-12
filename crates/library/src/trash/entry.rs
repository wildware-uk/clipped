//! What the trash holds, and what each operation on it answers with.
//!
//! Two tables in the schema hold a path to a media file the library does not
//! own — `recordings` and `clips` — and both carry `deleted_at` and
//! `deleted_from`, so both are trashed the same way. [`TrashItem`] is the pair
//! of "which table" and "which row", which is all this module needs to identify
//! something.

use std::fmt;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use super::retention::{self, Retention};

/// Which of the two file-holding tables an item lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ItemKind {
    /// A captured session's media file.
    Recording,
    /// A shorter file cut from one, or a saved replay.
    Clip,
}

impl ItemKind {
    /// The table this kind lives in.
    ///
    /// Every statement in this module is built from this and
    /// [`id_column`](Self::id_column), which are `&'static str` constants rather
    /// than anything a caller supplies — the two tables differ only in their
    /// names, and writing the queries twice would be two places for one rule to
    /// drift.
    pub(crate) const fn table(self) -> &'static str {
        match self {
            Self::Recording => "recordings",
            Self::Clip => "clips",
        }
    }

    /// The primary key column of that table.
    pub(crate) const fn id_column(self) -> &'static str {
        match self {
            Self::Recording => "recording_id",
            Self::Clip => "clip_id",
        }
    }

    /// Every kind, so that a sweep over the whole trash cannot forget one.
    pub(crate) const ALL: [Self; 2] = [Self::Recording, Self::Clip];
}

impl fmt::Display for ItemKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Recording => f.write_str("recording"),
            Self::Clip => f.write_str("clip"),
        }
    }
}

/// One recording or clip, named the way the index names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TrashItem {
    /// Which table it is in.
    pub kind: ItemKind,
    /// Its primary key.
    pub id: i64,
}

impl TrashItem {
    /// The recording with this `recording_id`.
    #[must_use]
    pub const fn recording(id: i64) -> Self {
        Self {
            kind: ItemKind::Recording,
            id,
        }
    }

    /// The clip with this `clip_id`.
    #[must_use]
    pub const fn clip(id: i64) -> Self {
        Self {
            kind: ItemKind::Clip,
            id,
        }
    }
}

impl fmt::Display for TrashItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.kind, self.id)
    }
}

/// One item in the trash, as the trash screen shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashEntry {
    /// Which recording or clip it is.
    pub item: TrashItem,
    /// Where the file is now, inside the trash.
    ///
    /// Equal to [`original_path`](Self::original_path) in the one case where
    /// there was no file to move: an item whose media the user had already
    /// deleted from Explorer before deleting it here.
    pub path: PathBuf,
    /// Where it was, and where restoring puts it back.
    pub original_path: PathBuf,
    /// When it was deleted, as RFC 3339 with an offset — the form every
    /// timestamp in the schema takes.
    pub deleted_at: String,
    /// What the file measured when the index last saw it.
    pub size_bytes: Option<i64>,
}

impl TrashEntry {
    /// How long is left before `retention` expires this entry.
    ///
    /// `Some(Duration::ZERO)` once it has expired, and `None` when
    /// [`deleted_at`](Self::deleted_at) cannot be read as a moment — which is
    /// also the answer [`has_expired`](Self::has_expired) gives, because
    /// destroying footage on the strength of a timestamp nothing can parse is
    /// exactly the deletion nobody asked for.
    #[must_use]
    pub fn remaining(&self, retention: Retention, now: SystemTime) -> Option<Duration> {
        retention::remaining(&self.deleted_at, retention, now)
    }

    /// Whether `retention` has expired this entry by `now`.
    #[must_use]
    pub fn has_expired(&self, retention: Retention, now: SystemTime) -> bool {
        retention::has_expired(&self.deleted_at, retention, now)
    }

    /// Its size as a count of bytes, treating an unmeasured file as nothing.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.size_bytes.unwrap_or(0).max(0).unsigned_abs()
    }
}

/// Where a restored item ended up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreOutcome {
    /// Which recording or clip it is.
    pub item: TrashItem,
    /// Where the file is now, and what the index now says.
    pub path: PathBuf,
    /// Where it was before it was deleted.
    pub original_path: PathBuf,
    /// Whether there was a file to move back.
    ///
    /// `false` for an item whose media had already gone before it was deleted:
    /// the row is restored to the library and reports itself missing, which is
    /// the truth rather than a broken row with no explanation.
    pub file_restored: bool,
}

impl RestoreOutcome {
    /// Whether the file had to go somewhere other than where it came from.
    ///
    /// True when something else was occupying the original location. The file is
    /// never overwritten, so a restore that would have collided lands beside it
    /// under a name that says so, and this is how a screen knows to mention it.
    #[must_use]
    pub fn diverted(&self) -> bool {
        self.path != self.original_path
    }
}

/// What became of the file when an item was destroyed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileOutcome {
    /// It was deleted from the trash.
    Deleted,
    /// There was nothing there to delete.
    AlreadyGone,
    /// It is not inside the trash directory, so it was left exactly where it is.
    LeftInPlace,
}

/// One item destroyed: retention expired, or the user asked for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removal {
    /// Which recording or clip it was.
    pub item: TrashItem,
    /// The file it named.
    pub path: PathBuf,
    /// What it measured.
    pub size_bytes: Option<i64>,
    /// What happened to the file.
    pub file: FileOutcome,
}

impl Removal {
    /// The bytes this returned to the volume, which is nothing unless the file
    /// was actually deleted.
    #[must_use]
    pub fn bytes_reclaimed(&self) -> u64 {
        match self.file {
            FileOutcome::Deleted => self.size_bytes.unwrap_or(0).max(0).unsigned_abs(),
            FileOutcome::AlreadyGone | FileOutcome::LeftInPlace => 0,
        }
    }
}

/// One item a sweep could not destroy.
///
/// A single file that will not unlink — an antivirus scanner holding it open is
/// the usual reason — must not stop the rest of the sweep, so failures are
/// collected rather than returned.
#[derive(Debug)]
pub struct ExpiryFailure {
    /// Which recording or clip it was.
    pub item: TrashItem,
    /// Why it could not be destroyed.
    pub error: super::TrashError,
}

/// What one sweep of the trash did.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct ExpiryReport {
    /// What was destroyed.
    pub removed: Vec<Removal>,
    /// What could not be, and why. The next sweep tries these again.
    pub failures: Vec<ExpiryFailure>,
}

impl ExpiryReport {
    /// How many bytes the volume got back.
    #[must_use]
    pub fn bytes_reclaimed(&self) -> u64 {
        self.removed
            .iter()
            .map(Removal::bytes_reclaimed)
            .fold(0u64, u64::saturating_add)
    }
}

impl fmt::Display for ExpiryReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} item(s) removed from the trash, {} byte(s) reclaimed, {} failure(s)",
            self.removed.len(),
            self.bytes_reclaimed(),
            self.failures.len()
        )
    }
}

/// The user's agreement to empty the trash, carrying what they were shown.
///
/// SPEC.md section 28's trash screen has an "empty trash" button and issue #94
/// asks that it be confirmed. A boolean argument would satisfy that literally
/// and mean nothing: the interesting failure is not "the code forgot to ask" but
/// "the user agreed to destroy the twelve things they were looking at and
/// something else had arrived by the time they clicked". So the confirmation
/// carries the figures the dialogue quoted, and
/// [`Trash::empty`](super::Trash::empty) refuses if the trash no longer matches
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmptyTrash {
    pub(crate) items: usize,
    pub(crate) bytes: u64,
}

impl EmptyTrash {
    /// The user has agreed to destroy `items` items totalling `bytes` bytes.
    ///
    /// Both figures come from the listing the user was shown
    /// ([`Trash::list`](super::Trash::list)), not from a fresh query — the whole
    /// point is that they describe the moment somebody looked.
    #[must_use]
    pub const fn confirmed(items: usize, bytes: u64) -> Self {
        Self { items, bytes }
    }

    /// The confirmation for exactly this listing.
    #[must_use]
    pub fn for_listing(entries: &[TrashEntry]) -> Self {
        Self {
            items: entries.len(),
            bytes: entries
                .iter()
                .map(TrashEntry::bytes)
                .fold(0u64, u64::saturating_add),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(size_bytes: Option<i64>) -> TrashEntry {
        TrashEntry {
            item: TrashItem::recording(1),
            path: PathBuf::from("trash"),
            original_path: PathBuf::from("original"),
            deleted_at: "2026-08-12T09:00:00+01:00".to_owned(),
            size_bytes,
        }
    }

    #[test]
    fn the_two_tables_that_hold_a_file_are_the_two_kinds() {
        assert_eq!(ItemKind::Recording.table(), "recordings");
        assert_eq!(ItemKind::Recording.id_column(), "recording_id");
        assert_eq!(ItemKind::Clip.table(), "clips");
        assert_eq!(ItemKind::Clip.id_column(), "clip_id");
        assert_eq!(ItemKind::ALL.len(), 2);
    }

    #[test]
    fn an_unmeasured_file_counts_as_nothing_rather_than_refusing_to_count() {
        assert_eq!(entry(None).bytes(), 0);
        assert_eq!(entry(Some(4096)).bytes(), 4096);
    }

    #[test]
    fn only_a_file_that_was_actually_deleted_reclaims_anything() {
        let removal = |file| Removal {
            item: TrashItem::recording(1),
            path: PathBuf::from("x"),
            size_bytes: Some(1_000),
            file,
        };

        assert_eq!(removal(FileOutcome::Deleted).bytes_reclaimed(), 1_000);
        assert_eq!(removal(FileOutcome::AlreadyGone).bytes_reclaimed(), 0);
        assert_eq!(removal(FileOutcome::LeftInPlace).bytes_reclaimed(), 0);
    }

    #[test]
    fn a_confirmation_describes_the_listing_it_was_taken_from() {
        let listing = [entry(Some(1_024)), entry(Some(2_048)), entry(None)];

        let confirmation = EmptyTrash::for_listing(&listing);

        assert_eq!(confirmation, EmptyTrash::confirmed(3, 3_072));
    }

    #[test]
    fn a_restore_that_landed_where_it_came_from_is_not_a_diversion() {
        let outcome = |path: &str| RestoreOutcome {
            item: TrashItem::recording(1),
            path: PathBuf::from(path),
            original_path: PathBuf::from("a.mkv"),
            file_restored: true,
        };

        assert!(!outcome("a.mkv").diverted());
        assert!(outcome("a (restored).mkv").diverted());
    }
}
