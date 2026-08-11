//! Matching what is on disk against what the index believes is on disk.
//!
//! A walk of the filesystem knows sizes and knows nothing else: it cannot say
//! which game a file belongs to, which session it came from, or whether the user
//! marked it a favourite. The index knows all of that and does not know sizes
//! reliably, because it is not told when a user deletes a recording in Explorer,
//! restores an old backup, or drops a video into the recordings folder by hand.
//!
//! Reconciliation puts the two together under one rule, stated in the module
//! documentation of `crate::accounting` and implemented here:
//!
//! > **The filesystem is the authority for bytes. The index is the authority for
//! > meaning.**
//!
//! So every byte counted comes from the disk, every attribution comes from the
//! index, and the four ways they can disagree are each reported rather than
//! averaged away:
//!
//! - **Matched** — counted at the size on disk, attributed to its game and
//!   session.
//! - **Matched, sizes differ** — counted at the size on disk, and listed as a
//!   disagreement so the indexer can correct its row.
//! - **Missing** — indexed, not on disk. Counted as nothing: a quota that
//!   believed a stale row would delete real recordings to make room that already
//!   exists.
//! - **Untracked** — on disk, not indexed. Counted, attributed to nothing. A
//!   figure that ignored these would say 40 GB while the disk filled.
//!
//! Nothing here changes the index. Healing it — deleting the rows for files that
//! are gone, indexing the ones that are not known — belongs to the indexer, and
//! this type is the evidence it works from.
//!
//! # Identifiers
//!
//! Games and sessions are named by opaque strings supplied by whoever holds the
//! index. This module deliberately does not know what a game *is*: the catalogue
//! lives in `clipped-game-detection`, which is a sibling layer that
//! `clipped-library` may not depend on, and the schema that will hold these rows
//! is [issue #55](https://github.com/wildware-uk/clipped/issues/55) and does not
//! exist yet. A string keeps accounting usable by both without either one
//! deciding the other's types.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::accounting::inventory::{FileEntry, StorageInventory};
use crate::accounting::StorageCategory;

/// What a file belongs to, as far as the index is concerned.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Attribution {
    game: Option<String>,
    session: Option<String>,
}

impl Attribution {
    /// Belonging to nothing in particular.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Belonging to a game.
    #[must_use]
    pub fn for_game(game: impl Into<String>) -> Self {
        Self {
            game: Some(game.into()),
            session: None,
        }
    }

    /// Belonging to a session of that game as well.
    #[must_use]
    pub fn in_session(self, session: impl Into<String>) -> Self {
        Self {
            session: Some(session.into()),
            ..self
        }
    }

    /// The game, if the index named one.
    #[must_use]
    pub fn game(&self) -> Option<&str> {
        self.game.as_deref()
    }

    /// The session, if the index named one.
    #[must_use]
    pub fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }
}

/// One file as the index describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedItem {
    path: PathBuf,
    category: StorageCategory,
    bytes: Option<u64>,
    attribution: Attribution,
}

impl IndexedItem {
    /// A file the index holds a row for.
    ///
    /// `bytes` is what the index believes the file weighs, and is `None` when it
    /// does not record one. It is never used as a total — only to notice that
    /// the row and the disk disagree.
    #[must_use]
    pub fn new(
        path: impl Into<PathBuf>,
        category: StorageCategory,
        bytes: Option<u64>,
        attribution: Attribution,
    ) -> Self {
        Self {
            path: path.into(),
            category,
            bytes,
            attribution,
        }
    }

    /// Where the index says the file is.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// What kind of file the index says it is.
    #[must_use]
    pub const fn category(&self) -> StorageCategory {
        self.category
    }

    /// What the index believes it weighs.
    #[must_use]
    pub const fn bytes(&self) -> Option<u64> {
        self.bytes
    }

    /// What the index says it belongs to.
    #[must_use]
    pub const fn attribution(&self) -> &Attribution {
        &self.attribution
    }
}

/// A file that is both on disk and in the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchedFile {
    entry: FileEntry,
    indexed_bytes: Option<u64>,
    attribution: Attribution,
}

impl MatchedFile {
    /// The file as the filesystem described it, which is where its size comes
    /// from.
    #[must_use]
    pub const fn entry(&self) -> &FileEntry {
        &self.entry
    }

    /// What it belongs to.
    #[must_use]
    pub const fn attribution(&self) -> &Attribution {
        &self.attribution
    }

    /// What the index believed it weighed.
    #[must_use]
    pub const fn indexed_bytes(&self) -> Option<u64> {
        self.indexed_bytes
    }

    /// Whether the index and the disk disagree about its size.
    ///
    /// A recording still being written disagrees with a row written when it
    /// started, so this is ordinary rather than alarming; it is reported so that
    /// the indexer can correct the row, and never so that the figure is doubted.
    #[must_use]
    pub fn size_disagrees(&self) -> bool {
        self.indexed_bytes
            .is_some_and(|indexed| indexed != self.entry.bytes())
    }
}

/// The result of comparing an inventory against an index.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reconciliation {
    matched: Vec<MatchedFile>,
    untracked: Vec<FileEntry>,
    missing: Vec<IndexedItem>,
}

impl Reconciliation {
    /// Compares `inventory` against `index`.
    ///
    /// Paths are compared as the two sides spell them, case-insensitively on
    /// Windows: a row written as `D:\Clipped\a.mkv` and a walk that produced
    /// `d:\clipped\a.mkv` are the same file, and treating them as two would
    /// report the library as entirely untracked.
    #[must_use]
    pub fn of(inventory: &StorageInventory, index: impl IntoIterator<Item = IndexedItem>) -> Self {
        let mut remaining: BTreeMap<String, IndexedItem> = index
            .into_iter()
            .map(|item| (comparison_key(item.path()), item))
            .collect();

        let mut matched = Vec::new();
        let mut untracked = Vec::new();

        for entry in inventory.files() {
            match remaining.remove(&comparison_key(entry.path())) {
                Some(item) => matched.push(MatchedFile {
                    entry: entry.clone(),
                    indexed_bytes: item.bytes(),
                    attribution: item.attribution().clone(),
                }),
                None => untracked.push(entry.clone()),
            }
        }

        Self {
            matched,
            untracked,
            missing: remaining.into_values().collect(),
        }
    }

    /// The files that are both on disk and in the index.
    #[must_use]
    pub fn matched(&self) -> &[MatchedFile] {
        &self.matched
    }

    /// The files on disk that the index does not know about.
    #[must_use]
    pub fn untracked(&self) -> &[FileEntry] {
        &self.untracked
    }

    /// The index's rows for files that are not on disk.
    ///
    /// Evidence for the indexer, not for the totals: none of these is counted.
    #[must_use]
    pub fn missing(&self) -> &[IndexedItem] {
        &self.missing
    }

    /// The matched files whose size the index has wrong.
    pub fn disagreements(&self) -> impl Iterator<Item = &MatchedFile> {
        self.matched
            .iter()
            .filter(|matched| matched.size_disagrees())
    }

    /// What the library occupies, in bytes: everything on disk, matched or not.
    ///
    /// Always equal to the inventory's own total. Reconciliation attributes
    /// bytes; it never adds or removes any.
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        let matched = self
            .matched
            .iter()
            .fold(0u64, |total, file| total.saturating_add(file.entry.bytes()));
        self.untracked
            .iter()
            .fold(matched, |total, entry| total.saturating_add(entry.bytes()))
    }

    /// What each game occupies, in bytes.
    ///
    /// SPEC.md section 17's games view — "Counter-Strike 2, 217 sessions, 83 GB"
    /// — and the breakdown the storage settings screen shows. Files the index
    /// attributes to no game are not here; they are
    /// [`unattributed_bytes`](Self::unattributed_bytes).
    #[must_use]
    pub fn bytes_by_game(&self) -> BTreeMap<String, u64> {
        self.totals_by(|attribution| attribution.game())
    }

    /// What each session occupies, in bytes.
    #[must_use]
    pub fn bytes_by_session(&self) -> BTreeMap<String, u64> {
        self.totals_by(|attribution| attribution.session())
    }

    /// What each category occupies, in bytes, counting untracked files too.
    #[must_use]
    pub fn bytes_by_category(&self) -> BTreeMap<StorageCategory, u64> {
        let mut totals: BTreeMap<StorageCategory, u64> = BTreeMap::new();

        for entry in self
            .matched
            .iter()
            .map(MatchedFile::entry)
            .chain(self.untracked.iter())
        {
            let total = totals.entry(entry.category()).or_default();
            *total = total.saturating_add(entry.bytes());
        }

        totals
    }

    /// What belongs to no game, in bytes.
    ///
    /// Files the index has never heard of, plus indexed files it attributes to
    /// nothing. Shown rather than hidden: a user whose reported usage is 40 GB
    /// short of what Explorer says is owed the difference, and this is usually
    /// where it is.
    #[must_use]
    pub fn unattributed_bytes(&self) -> u64 {
        let indexed_without_a_game = self
            .matched
            .iter()
            .filter(|file| file.attribution().game().is_none())
            .fold(0u64, |total, file| total.saturating_add(file.entry.bytes()));

        self.untracked
            .iter()
            .fold(indexed_without_a_game, |total, entry| {
                total.saturating_add(entry.bytes())
            })
    }

    /// What the index believes exists but is not on disk, in bytes.
    ///
    /// Zero for a healthy library. A large figure means the index is well behind
    /// the filesystem — the user has been deleting recordings by hand — and is
    /// the number that says how far.
    #[must_use]
    pub fn missing_bytes(&self) -> u64 {
        self.missing.iter().fold(0u64, |total, item| {
            total.saturating_add(item.bytes().unwrap_or(0))
        })
    }

    /// Totals grouped by whichever part of the attribution `key` selects.
    fn totals_by(&self, key: impl Fn(&Attribution) -> Option<&str>) -> BTreeMap<String, u64> {
        let mut totals: BTreeMap<String, u64> = BTreeMap::new();

        for file in &self.matched {
            if let Some(name) = key(file.attribution()) {
                let total = totals.entry(name.to_owned()).or_default();
                *total = total.saturating_add(file.entry.bytes());
            }
        }

        totals
    }
}

/// The key two paths are compared by.
///
/// Windows filenames are case-insensitive, so the index and the walk may spell
/// the same file differently and must still match. `to_lowercase` rather than an
/// ASCII fold because a path here may contain a user's own directory names in
/// any script; it is a best effort in the same sense NTFS's own folding is, and
/// the cost of the residue is a file reported as untracked rather than a wrong
/// total.
#[cfg(windows)]
fn comparison_key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

/// The key two paths are compared by: the path itself, on a case-sensitive
/// filesystem.
#[cfg(not(windows))]
fn comparison_key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::{Duration, SystemTime};

    fn path(name: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!(r"D:\Clipped\{name}"))
        } else {
            PathBuf::from(format!("/clipped/{name}"))
        }
    }

    fn modified() -> Option<SystemTime> {
        Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000))
    }

    fn on_disk(name: &str, bytes: u64) -> FileEntry {
        FileEntry::new(path(name), StorageCategory::Recordings, bytes, modified())
    }

    fn inventory_of(files: impl IntoIterator<Item = FileEntry>) -> StorageInventory {
        let mut inventory = StorageInventory::new();
        for file in files {
            inventory.record_added(file);
        }
        inventory
    }

    fn indexed(name: &str, bytes: Option<u64>, game: &str, session: &str) -> IndexedItem {
        IndexedItem::new(
            path(name),
            StorageCategory::Recordings,
            bytes,
            Attribution::for_game(game).in_session(session),
        )
    }

    #[test]
    fn a_library_the_index_agrees_with_is_entirely_matched() {
        let inventory = inventory_of([on_disk("a.mkv", 1_000), on_disk("b.mkv", 2_000)]);
        let index = vec![
            indexed("a.mkv", Some(1_000), "cs2", "session-1"),
            indexed("b.mkv", Some(2_000), "cs2", "session-1"),
        ];

        let reconciliation = Reconciliation::of(&inventory, index);

        assert_eq!(reconciliation.matched().len(), 2);
        assert!(reconciliation.untracked().is_empty());
        assert!(reconciliation.missing().is_empty());
        assert_eq!(reconciliation.disagreements().count(), 0);
        assert_eq!(reconciliation.total_bytes(), 3_000);
    }

    #[test]
    fn a_file_the_index_has_never_heard_of_is_still_counted() {
        // A user's own video dropped into the recordings folder, or a recording
        // whose row was never written because the machine lost power. Omitting
        // it makes the reported figure smaller than the disk.
        let inventory = inventory_of([on_disk("a.mkv", 1_000), on_disk("stranger.mkv", 5_000)]);
        let index = vec![indexed("a.mkv", Some(1_000), "cs2", "session-1")];

        let reconciliation = Reconciliation::of(&inventory, index);

        assert_eq!(reconciliation.untracked().len(), 1);
        assert_eq!(reconciliation.total_bytes(), 6_000);
        assert_eq!(reconciliation.unattributed_bytes(), 5_000);
    }

    #[test]
    fn a_row_for_a_file_that_is_gone_is_reported_and_counted_as_nothing() {
        // The user deleted it in Explorer. Trusting the row would report 41 GB
        // of a 1 GB library, and issue #111 would delete real recordings to get
        // under a limit that was never breached.
        let inventory = inventory_of([on_disk("a.mkv", 1_000)]);
        let index = vec![
            indexed("a.mkv", Some(1_000), "cs2", "session-1"),
            indexed("deleted.mkv", Some(40_000_000_000), "cs2", "session-0"),
        ];

        let reconciliation = Reconciliation::of(&inventory, index);

        assert_eq!(reconciliation.total_bytes(), 1_000);
        assert_eq!(reconciliation.missing().len(), 1);
        assert_eq!(reconciliation.missing_bytes(), 40_000_000_000);
        assert_eq!(
            reconciliation.missing()[0].path(),
            path("deleted.mkv").as_path()
        );
    }

    #[test]
    fn when_the_sizes_disagree_the_disk_wins_and_the_row_is_listed() {
        let inventory = inventory_of([on_disk("a.mkv", 9_000)]);
        let index = vec![indexed("a.mkv", Some(1_000), "cs2", "session-1")];

        let reconciliation = Reconciliation::of(&inventory, index);

        assert_eq!(reconciliation.total_bytes(), 9_000);
        assert_eq!(reconciliation.disagreements().count(), 1);
        assert_eq!(
            reconciliation.matched()[0].indexed_bytes(),
            Some(1_000),
            "the row is reported as it was, so the indexer can correct it"
        );
    }

    #[test]
    fn an_index_that_records_no_size_disagrees_with_nothing() {
        let inventory = inventory_of([on_disk("a.mkv", 9_000)]);
        let index = vec![indexed("a.mkv", None, "cs2", "session-1")];

        let reconciliation = Reconciliation::of(&inventory, index);

        assert_eq!(reconciliation.disagreements().count(), 0);
        assert_eq!(reconciliation.total_bytes(), 9_000);
    }

    #[test]
    fn reconciliation_never_changes_the_total() {
        // The invariant the whole design rests on: attribution moves bytes
        // between columns and never invents or loses one.
        let inventory = inventory_of([
            on_disk("a.mkv", 1_000),
            on_disk("b.mkv", 2_000),
            on_disk("c.mkv", 4_000),
        ]);
        let index = vec![
            indexed("a.mkv", Some(1_000), "cs2", "session-1"),
            indexed("gone.mkv", Some(8_000), "cs2", "session-1"),
        ];

        let reconciliation = Reconciliation::of(&inventory, index);

        assert_eq!(reconciliation.total_bytes(), inventory.total_bytes());
    }

    #[test]
    fn usage_is_reported_per_game_and_per_session() {
        let inventory = inventory_of([
            on_disk("a.mkv", 1_000),
            on_disk("b.mkv", 2_000),
            on_disk("c.mkv", 4_000),
        ]);
        let index = vec![
            indexed("a.mkv", Some(1_000), "cs2", "session-1"),
            indexed("b.mkv", Some(2_000), "cs2", "session-2"),
            indexed("c.mkv", Some(4_000), "minecraft", "session-3"),
        ];

        let reconciliation = Reconciliation::of(&inventory, index);

        let by_game = reconciliation.bytes_by_game();
        assert_eq!(by_game["cs2"], 3_000);
        assert_eq!(by_game["minecraft"], 4_000);

        let by_session = reconciliation.bytes_by_session();
        assert_eq!(by_session["session-1"], 1_000);
        assert_eq!(by_session["session-2"], 2_000);
        assert_eq!(by_session["session-3"], 4_000);
    }

    #[test]
    fn an_indexed_file_belonging_to_no_game_is_unattributed_rather_than_missing() {
        let inventory = inventory_of([on_disk("clipped.log", 500)]);
        let index = vec![IndexedItem::new(
            path("clipped.log"),
            StorageCategory::Logs,
            Some(500),
            Attribution::none(),
        )];

        let reconciliation = Reconciliation::of(&inventory, index);

        assert_eq!(reconciliation.matched().len(), 1);
        assert!(reconciliation.bytes_by_game().is_empty());
        assert_eq!(reconciliation.unattributed_bytes(), 500);
    }

    #[test]
    fn a_category_breakdown_counts_untracked_files_too() {
        let inventory = inventory_of([
            on_disk("a.mkv", 1_000),
            FileEntry::new(path("a.jpg"), StorageCategory::Thumbnails, 300, modified()),
        ]);
        let index = vec![indexed("a.mkv", Some(1_000), "cs2", "session-1")];

        let reconciliation = Reconciliation::of(&inventory, index);
        let by_category = reconciliation.bytes_by_category();

        assert_eq!(by_category[&StorageCategory::Recordings], 1_000);
        assert_eq!(by_category[&StorageCategory::Thumbnails], 300);
        assert_eq!(
            by_category.values().sum::<u64>(),
            reconciliation.total_bytes()
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_row_spelled_in_a_different_case_matches_the_file_on_disk() {
        // Windows filenames are case-insensitive. Treating these as two files
        // would report a fully indexed library as entirely untracked.
        let inventory = inventory_of([FileEntry::new(
            r"D:\Clipped\Recordings\A.mkv",
            StorageCategory::Recordings,
            1_000,
            modified(),
        )]);
        let index = vec![IndexedItem::new(
            r"d:\clipped\recordings\a.mkv",
            StorageCategory::Recordings,
            Some(1_000),
            Attribution::for_game("cs2"),
        )];

        let reconciliation = Reconciliation::of(&inventory, index);

        assert_eq!(reconciliation.matched().len(), 1);
        assert!(reconciliation.untracked().is_empty());
        assert!(reconciliation.missing().is_empty());
    }

    #[test]
    fn an_empty_index_leaves_everything_untracked_and_counted() {
        let inventory = inventory_of([on_disk("a.mkv", 1_000)]);

        let reconciliation = Reconciliation::of(&inventory, Vec::new());

        assert_eq!(reconciliation.untracked().len(), 1);
        assert_eq!(reconciliation.total_bytes(), 1_000);
        assert_eq!(reconciliation.unattributed_bytes(), 1_000);
    }
}
