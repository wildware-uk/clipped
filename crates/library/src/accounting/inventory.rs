//! What a measurement found, and how much of it it managed to see.
//!
//! An inventory is one file entry per file, keyed by path, plus an honest
//! statement of whether the measurement finished. Both halves matter:
//!
//! - **One entry per file**, rather than a running total, because issue #111 has
//!   to be able to ask which files a cleanup would take first, and a total
//!   cannot answer that. The entries are also what makes the figures
//!   attributable to a game and a session once they are reconciled against the
//!   index (`crate::accounting::reconcile`).
//! - **Keyed by path**, so a file that is somehow reached twice is counted once.
//!   `StorageRoots` already refuses overlapping roots, which is the case that
//!   would otherwise cause it; this is the second line.
//! - **[`Completeness`]**, because a partial walk under-reports, and usage that
//!   is quietly too low is the input that makes a quota say "plenty of room"
//!   while the disk fills. Every consumer of a figure can see whether the
//!   measurement behind it finished.
//!
//! The sizes are **logical file lengths** — what the file contains — and not the
//! number of bytes the volume allocates for it. `docs/storage-management.md`
//! documents the difference and how large it can get; in short, a filesystem
//! rounds each file up to a cluster, so real consumption is a little higher than
//! the figure here, and sparse or compressed files make it lower.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::accounting::StorageCategory;

/// One file, as the filesystem described it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    path: PathBuf,
    category: StorageCategory,
    bytes: u64,
    modified: Option<SystemTime>,
}

impl FileEntry {
    /// A file of `bytes` at `path`, holding `category`, last modified at
    /// `modified`.
    ///
    /// `modified` is optional because a filesystem is allowed not to know: it is
    /// `None` on a mount that does not record modification times, and an entry
    /// without one is simply never selected by an age limit rather than being
    /// treated as infinitely old (see [`FileEntry::age`]).
    #[must_use]
    pub fn new(
        path: impl Into<PathBuf>,
        category: StorageCategory,
        bytes: u64,
        modified: Option<SystemTime>,
    ) -> Self {
        Self {
            path: path.into(),
            category,
            bytes,
            modified,
        }
    }

    /// Where the file is.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// What kind of file it is, taken from the root it was found under.
    #[must_use]
    pub const fn category(&self) -> StorageCategory {
        self.category
    }

    /// Its length in bytes.
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }

    /// When it was last modified, if the filesystem said.
    #[must_use]
    pub const fn modified(&self) -> Option<SystemTime> {
        self.modified
    }

    /// How old it is at `now`.
    ///
    /// `None` when the filesystem reported no modification time, and also when
    /// the recorded time is in the *future* — a clock that has been put back, or
    /// a file copied from a machine whose clock was ahead. A negative age
    /// clamped to zero would be an invented figure; refusing to give one keeps
    /// an age limit from acting on a number nobody measured (AGENTS.md
    /// section 27).
    #[must_use]
    pub fn age(&self, now: SystemTime) -> Option<Duration> {
        now.duration_since(self.modified?).ok()
    }
}

/// Why a measurement did not see everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PartialReason {
    /// The scan reached its time budget before it ran out of directories.
    TimeBudgetExhausted,
    /// The caller asked it to stop.
    Cancelled,
    /// A declared root could not be read at all — the drive holding it is not
    /// connected, or the path is not reachable.
    RootUnavailable,
    /// A directory below a root could not be read, most often for want of
    /// permission.
    DirectoryUnreadable,
}

impl PartialReason {
    /// A short lower-case name, for logs and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TimeBudgetExhausted => "time-budget-exhausted",
            Self::Cancelled => "cancelled",
            Self::RootUnavailable => "root-unavailable",
            Self::DirectoryUnreadable => "directory-unreadable",
        }
    }
}

impl core::fmt::Display for PartialReason {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Whether a measurement saw everything it was asked to.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Completeness {
    /// Every declared root was walked to the end.
    Complete,
    /// Something stopped it. The reasons are kept in the order they happened,
    /// and there may be more than one — a scan can lose a root *and* run out of
    /// time.
    Partial(Vec<PartialReason>),
}

impl Completeness {
    /// Whether the measurement finished.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        matches!(self, Self::Complete)
    }

    /// Why it did not, or an empty slice if it did.
    #[must_use]
    pub fn reasons(&self) -> &[PartialReason] {
        match self {
            Self::Complete => &[],
            Self::Partial(reasons) => reasons,
        }
    }
}

/// A declared root the scan could not read.
///
/// Kept rather than folded into an error, because a library spread over two
/// drives with one of them disconnected is a normal state that a user should be
/// told about precisely — "your D: drive is not connected" rather than "storage
/// unavailable" (AGENTS.md section 45).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnavailableRoot {
    path: PathBuf,
    category: StorageCategory,
    reason: String,
}

impl UnavailableRoot {
    /// A root at `path` holding `category` that could not be read, because
    /// `reason`.
    #[must_use]
    pub fn new(
        path: impl Into<PathBuf>,
        category: StorageCategory,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            category,
            reason: reason.into(),
        }
    }

    /// The root that could not be read.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// What it was declared to hold.
    #[must_use]
    pub const fn category(&self) -> StorageCategory {
        self.category
    }

    /// What the operating system said about it.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// Every file a measurement found, and how complete the measurement was.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StorageInventory {
    files: BTreeMap<PathBuf, FileEntry>,
    partial_reasons: Vec<PartialReason>,
    unavailable_roots: Vec<UnavailableRoot>,
}

impl StorageInventory {
    /// An empty inventory that is considered complete.
    ///
    /// A library with no files is a real state — a first run — and is not the
    /// same as a measurement that failed.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a file, or replaces the entry for a path already present.
    ///
    /// This is the incremental path as well as what a scan uses: a recorder that
    /// has just finished writing a file adds it here and the totals stay current
    /// without walking the library again.
    pub fn record_added(&mut self, entry: FileEntry) {
        self.files.insert(entry.path().to_path_buf(), entry);
    }

    /// Removes the entry for `path`, returning it if there was one.
    ///
    /// The counterpart of [`record_added`](Self::record_added) — for a file the
    /// application itself moved to the trash or deleted. Nothing here touches the
    /// filesystem; this only forgets a measurement.
    pub fn record_removed(&mut self, path: &Path) -> Option<FileEntry> {
        self.files.remove(path)
    }

    /// Records that the measurement was cut short, and why.
    ///
    /// Repeating a reason already recorded does nothing: a scan that loses two
    /// roots is still "a root was unavailable" once.
    pub fn record_partial(&mut self, reason: PartialReason) {
        if !self.partial_reasons.contains(&reason) {
            self.partial_reasons.push(reason);
        }
    }

    /// Records a root that could not be read, which also makes the measurement
    /// partial.
    pub fn record_unavailable_root(&mut self, root: UnavailableRoot) {
        self.unavailable_roots.push(root);
        self.record_partial(PartialReason::RootUnavailable);
    }

    /// Whether the measurement saw everything, and why not if it did not.
    #[must_use]
    pub fn completeness(&self) -> Completeness {
        if self.partial_reasons.is_empty() {
            Completeness::Complete
        } else {
            Completeness::Partial(self.partial_reasons.clone())
        }
    }

    /// Whether the measurement saw everything.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.partial_reasons.is_empty()
    }

    /// The roots that could not be read.
    #[must_use]
    pub fn unavailable_roots(&self) -> &[UnavailableRoot] {
        &self.unavailable_roots
    }

    /// Every file, ordered by path.
    pub fn files(&self) -> impl ExactSizeIterator<Item = &FileEntry> {
        self.files.values()
    }

    /// How many files were found.
    #[must_use]
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// The total, in bytes.
    ///
    /// Saturating rather than wrapping. Two exabytes of recordings is not a
    /// situation to be correct about, but silently reporting a small number
    /// because a total wrapped is a situation to avoid.
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.files
            .values()
            .fold(0u64, |total, entry| total.saturating_add(entry.bytes()))
    }

    /// The total for one category, in bytes.
    #[must_use]
    pub fn bytes_for(&self, category: StorageCategory) -> u64 {
        self.files
            .values()
            .filter(|entry| entry.category() == category)
            .fold(0u64, |total, entry| total.saturating_add(entry.bytes()))
    }

    /// The total for every category that has any files, in bytes.
    #[must_use]
    pub fn bytes_by_category(&self) -> BTreeMap<StorageCategory, u64> {
        let mut totals = BTreeMap::new();
        for entry in self.files.values() {
            let total: &mut u64 = totals.entry(entry.category()).or_default();
            *total = total.saturating_add(entry.bytes());
        }
        totals
    }

    /// Every file, oldest first, with files of unknown age last.
    ///
    /// This is the order SPEC.md section 27 describes a cleanup working in, and
    /// it is as far as this module goes towards issue #111: it says which files
    /// are oldest, not which files may be deleted. Ties break on the path, so
    /// the order is deterministic for two files written in the same millisecond.
    ///
    /// A file whose modification time the filesystem did not report sorts last
    /// rather than first: an unknown age must not make something the first
    /// candidate for deletion.
    #[must_use]
    pub fn oldest_first(&self) -> Vec<&FileEntry> {
        let mut entries: Vec<&FileEntry> = self.files.values().collect();
        entries.sort_by(|left, right| match (left.modified(), right.modified()) {
            (Some(left_time), Some(right_time)) => left_time
                .cmp(&right_time)
                .then_with(|| left.path().cmp(right.path())),
            (Some(_), None) => core::cmp::Ordering::Less,
            (None, Some(_)) => core::cmp::Ordering::Greater,
            (None, None) => left.path().cmp(right.path()),
        });
        entries
    }

    /// The files older than `age` at `now`, and what they occupy.
    ///
    /// Returns the count and the total in bytes. What the maximum recording age
    /// setting is *for*: a screen can say "31 recordings, 84 GB, older than 90
    /// days" without anything having been deleted, which is the "review large
    /// recordings" path issue #111 asks for, one ticket early and without a
    /// deletion behind it.
    #[must_use]
    pub fn older_than(&self, age: Duration, now: SystemTime) -> (usize, u64) {
        self.files
            .values()
            .filter(|entry| entry.age(now).is_some_and(|entry_age| entry_age > age))
            .fold((0usize, 0u64), |(count, bytes), entry| {
                (count + 1, bytes.saturating_add(entry.bytes()))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path the platform running the test considers absolute.
    fn path(tail: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!(r"D:\Clipped\{tail}"))
        } else {
            PathBuf::from(format!("/clipped/{tail}"))
        }
    }

    /// A fixed instant to measure ages against, so nothing here depends on the
    /// wall clock (AGENTS.md section 25).
    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    fn entry(name: &str, bytes: u64, age: Option<Duration>) -> FileEntry {
        FileEntry::new(
            path(name),
            StorageCategory::Recordings,
            bytes,
            age.map(|age| now() - age),
        )
    }

    #[test]
    fn a_new_inventory_is_empty_and_complete() {
        let inventory = StorageInventory::new();

        assert_eq!(inventory.total_bytes(), 0);
        assert_eq!(inventory.file_count(), 0);
        assert!(inventory.is_complete());
        assert!(inventory.completeness().is_complete());
    }

    #[test]
    fn the_total_is_the_sum_of_the_files() {
        let mut inventory = StorageInventory::new();
        inventory.record_added(entry("a.mkv", 1_000, None));
        inventory.record_added(entry("b.mkv", 2_500, None));

        assert_eq!(inventory.total_bytes(), 3_500);
        assert_eq!(inventory.file_count(), 2);
    }

    #[test]
    fn the_same_path_twice_is_one_file() {
        // The double-counting failure, at the level below overlapping roots.
        let mut inventory = StorageInventory::new();
        inventory.record_added(entry("a.mkv", 1_000, None));
        inventory.record_added(entry("a.mkv", 1_000, None));

        assert_eq!(inventory.file_count(), 1);
        assert_eq!(inventory.total_bytes(), 1_000);
    }

    #[test]
    fn recording_a_file_again_replaces_its_size_rather_than_adding_to_it() {
        // A recording still being written grows. Re-recording it must correct
        // the figure, not double it.
        let mut inventory = StorageInventory::new();
        inventory.record_added(entry("a.mkv", 1_000, None));
        inventory.record_added(entry("a.mkv", 9_000, None));

        assert_eq!(inventory.total_bytes(), 9_000);
    }

    #[test]
    fn removing_a_file_returns_it_and_takes_its_bytes_out_of_the_total() {
        let mut inventory = StorageInventory::new();
        inventory.record_added(entry("a.mkv", 1_000, None));
        inventory.record_added(entry("b.mkv", 2_000, None));

        let removed = inventory
            .record_removed(&path("a.mkv"))
            .expect("the file was there");

        assert_eq!(removed.bytes(), 1_000);
        assert_eq!(inventory.total_bytes(), 2_000);
        assert!(inventory.record_removed(&path("a.mkv")).is_none());
    }

    #[test]
    fn totals_are_reported_per_category() {
        let mut inventory = StorageInventory::new();
        inventory.record_added(FileEntry::new(
            path("a.mkv"),
            StorageCategory::Recordings,
            10_000,
            None,
        ));
        inventory.record_added(FileEntry::new(
            path("a.jpg"),
            StorageCategory::Thumbnails,
            500,
            None,
        ));
        inventory.record_added(FileEntry::new(
            path("b.jpg"),
            StorageCategory::Thumbnails,
            700,
            None,
        ));

        assert_eq!(inventory.bytes_for(StorageCategory::Recordings), 10_000);
        assert_eq!(inventory.bytes_for(StorageCategory::Thumbnails), 1_200);
        assert_eq!(inventory.bytes_for(StorageCategory::Clips), 0);

        let by_category = inventory.bytes_by_category();
        assert_eq!(by_category.len(), 2);
        assert_eq!(by_category[&StorageCategory::Recordings], 10_000);
        assert_eq!(by_category[&StorageCategory::Thumbnails], 1_200);
        assert_eq!(
            by_category.values().sum::<u64>(),
            inventory.total_bytes(),
            "a breakdown that does not add up to the total is a breakdown nobody can trust"
        );
    }

    #[test]
    fn a_total_that_would_overflow_saturates_rather_than_wrapping() {
        let mut inventory = StorageInventory::new();
        inventory.record_added(entry("a.mkv", u64::MAX, None));
        inventory.record_added(entry("b.mkv", 1_000, None));

        assert_eq!(inventory.total_bytes(), u64::MAX);
    }

    #[test]
    fn a_partial_measurement_says_so_and_says_why() {
        let mut inventory = StorageInventory::new();
        inventory.record_partial(PartialReason::TimeBudgetExhausted);

        assert!(!inventory.is_complete());
        assert_eq!(
            inventory.completeness().reasons(),
            &[PartialReason::TimeBudgetExhausted]
        );
    }

    #[test]
    fn the_same_reason_twice_is_recorded_once() {
        let mut inventory = StorageInventory::new();
        inventory.record_partial(PartialReason::DirectoryUnreadable);
        inventory.record_partial(PartialReason::DirectoryUnreadable);

        assert_eq!(inventory.completeness().reasons().len(), 1);
    }

    #[test]
    fn an_unavailable_root_makes_the_measurement_partial_and_is_kept_in_full() {
        // "Your D: drive is not connected" needs the drive and the reason, not
        // just a flag (AGENTS.md section 45).
        let mut inventory = StorageInventory::new();
        inventory.record_unavailable_root(UnavailableRoot::new(
            path("Recordings"),
            StorageCategory::Recordings,
            "the device is not ready",
        ));

        assert!(!inventory.is_complete());
        assert_eq!(
            inventory.completeness().reasons(),
            &[PartialReason::RootUnavailable]
        );
        assert_eq!(inventory.unavailable_roots().len(), 1);
        assert_eq!(
            inventory.unavailable_roots()[0].path(),
            path("Recordings").as_path()
        );
        assert_eq!(
            inventory.unavailable_roots()[0].reason(),
            "the device is not ready"
        );
    }

    #[test]
    fn the_oldest_file_comes_first_and_an_unknown_age_comes_last() {
        let mut inventory = StorageInventory::new();
        inventory.record_added(entry("recent.mkv", 1, Some(Duration::from_secs(60))));
        inventory.record_added(entry(
            "ancient.mkv",
            1,
            Some(Duration::from_secs(90 * 86400)),
        ));
        inventory.record_added(entry("undated.mkv", 1, None));

        let order: Vec<_> = inventory
            .oldest_first()
            .into_iter()
            .map(|entry| {
                entry
                    .path()
                    .file_name()
                    .expect("every entry has a name")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();

        assert_eq!(order, vec!["ancient.mkv", "recent.mkv", "undated.mkv"]);
    }

    #[test]
    fn files_written_in_the_same_instant_are_ordered_deterministically() {
        let mut inventory = StorageInventory::new();
        let same = Some(Duration::from_secs(3600));
        inventory.record_added(entry("b.mkv", 1, same));
        inventory.record_added(entry("a.mkv", 1, same));

        let order: Vec<_> = inventory
            .oldest_first()
            .into_iter()
            .map(FileEntry::path)
            .collect();

        assert_eq!(
            order,
            vec![path("a.mkv").as_path(), path("b.mkv").as_path()]
        );
    }

    #[test]
    fn only_files_older_than_the_age_are_counted_against_it() {
        let mut inventory = StorageInventory::new();
        inventory.record_added(entry(
            "old.mkv",
            5_000,
            Some(Duration::from_secs(91 * 86400)),
        ));
        inventory.record_added(entry(
            "exactly.mkv",
            7_000,
            Some(Duration::from_secs(90 * 86400)),
        ));
        inventory.record_added(entry("new.mkv", 9_000, Some(Duration::from_secs(86400))));
        inventory.record_added(entry("undated.mkv", 11_000, None));

        let (count, bytes) = inventory.older_than(Duration::from_secs(90 * 86400), now());

        // Exactly at the limit is not over it, and a file with no modification
        // time is never selected by an age limit.
        assert_eq!(count, 1);
        assert_eq!(bytes, 5_000);
    }

    #[test]
    fn a_file_dated_in_the_future_has_no_age_rather_than_a_negative_one() {
        // A clock put back, or a file copied from a machine that was ahead.
        let future = FileEntry::new(
            path("tomorrow.mkv"),
            StorageCategory::Recordings,
            1_000,
            Some(now() + Duration::from_secs(86400)),
        );

        assert_eq!(future.age(now()), None);

        let mut inventory = StorageInventory::new();
        inventory.record_added(future);
        assert_eq!(inventory.older_than(Duration::from_secs(1), now()), (0, 0));
    }
}
