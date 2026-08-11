//! Walking the declared roots and adding up what is there.
//!
//! The measurement itself: one directory enumeration per directory, one length
//! per file, and no file's contents read. It is an ordinary synchronous walk and
//! it spawns nothing — the caller owns the thread, which is what lets the
//! desktop application run it in the background and the recorder never run it at
//! all while capturing (AGENTS.md section 20).
//!
//! # What it refuses to do
//!
//! **It does not follow links.** A symbolic link, a junction or any other
//! reparse point is counted as nothing and not descended into. Following them
//! would count a file twice through two paths, and a link pointing at an
//! ancestor would walk for ever.
//!
//! **It does not fail as a whole.** A drive that is not connected, or a
//! directory permission denies, does not abandon the measurement of everything
//! else: the inventory reports what it saw and says it is partial, and the
//! unreadable roots are listed individually. A user with a two-drive library and
//! one drive unplugged is owed the half that is there (AGENTS.md section 16).
//!
//! # Being interrupted
//!
//! [`ScanOptions::with_time_budget`] stops the walk once it has spent long
//! enough, and [`scan_until`] stops it when the caller's closure says to. Both
//! produce a partial inventory rather than a truncated total that looks
//! complete. The budget is checked before each directory and every
//! [`ENTRIES_PER_BUDGET_CHECK`] entries within one, so a single enormous
//! directory cannot outrun it either.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use crate::accounting::inventory::{
    Completeness, FileEntry, PartialReason, StorageInventory, UnavailableRoot,
};
use crate::accounting::roots::StorageRoots;
use crate::accounting::StorageCategory;

/// How many directory entries are read between two checks of the time budget.
///
/// Reading a clock is cheap but not free, and a directory of a few hundred
/// entries is enumerated in well under a millisecond, so checking every entry
/// would be measuring the measurement. Checking every 256 bounds the overshoot
/// at a fraction of a millisecond.
pub const ENTRIES_PER_BUDGET_CHECK: usize = 256;

/// How long a scan may take, and how it may be stopped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScanOptions {
    time_budget: Option<Duration>,
}

impl ScanOptions {
    /// A scan with no time budget, which runs until it has seen everything.
    #[must_use]
    pub const fn new() -> Self {
        Self { time_budget: None }
    }

    /// Stops the scan once it has run for `budget`.
    ///
    /// The result is a partial inventory, not a wrong one: what was measured is
    /// accurate and the inventory says it is incomplete. A budget of zero stops
    /// before the first directory, which is a useful thing to be able to ask
    /// for in a test and harmless in production.
    #[must_use]
    pub const fn with_time_budget(self, budget: Duration) -> Self {
        Self {
            time_budget: Some(budget),
        }
    }

    /// The configured budget, if there is one.
    #[must_use]
    pub const fn time_budget(&self) -> Option<Duration> {
        self.time_budget
    }
}

/// What a scan found and what it cost.
#[derive(Debug, Clone)]
pub struct ScanReport {
    inventory: StorageInventory,
    elapsed: Duration,
    directories_seen: u64,
    links_skipped: u64,
}

impl ScanReport {
    /// What was found.
    #[must_use]
    pub const fn inventory(&self) -> &StorageInventory {
        &self.inventory
    }

    /// Takes the inventory out of the report.
    #[must_use]
    pub fn into_inventory(self) -> StorageInventory {
        self.inventory
    }

    /// How long the walk took.
    ///
    /// The figure `docs/storage-management.md` quotes, and the one to watch when
    /// deciding how often to rescan.
    #[must_use]
    pub const fn elapsed(&self) -> Duration {
        self.elapsed
    }

    /// How many files were measured.
    #[must_use]
    pub fn files_seen(&self) -> usize {
        self.inventory.file_count()
    }

    /// How many directories were enumerated.
    #[must_use]
    pub const fn directories_seen(&self) -> u64 {
        self.directories_seen
    }

    /// How many links were skipped rather than followed.
    ///
    /// Reported rather than hidden: a user who has junctioned their recordings
    /// somewhere else will see a total that omits them, and this is the number
    /// that explains why.
    #[must_use]
    pub const fn links_skipped(&self) -> u64 {
        self.links_skipped
    }

    /// Whether the walk saw everything, and why not if it did not.
    #[must_use]
    pub fn completeness(&self) -> Completeness {
        self.inventory.completeness()
    }
}

/// Measures every declared root.
///
/// Never fails: an unreadable root is reported inside the inventory. See the
/// module documentation for what it refuses to do.
#[must_use]
pub fn scan(roots: &StorageRoots, options: &ScanOptions) -> ScanReport {
    scan_until(roots, options, &|| false)
}

/// Measures every declared root, stopping early if `stop` returns `true`.
///
/// `stop` is called before each directory and every
/// [`ENTRIES_PER_BUDGET_CHECK`] entries within one. It is how the desktop
/// application cancels a scan a user navigated away from, and how a process
/// shutting down stops one it started.
#[must_use]
pub fn scan_until(
    roots: &StorageRoots,
    options: &ScanOptions,
    stop: &dyn Fn() -> bool,
) -> ScanReport {
    let started = Instant::now();
    let mut walk = Walk {
        inventory: StorageInventory::new(),
        directories_seen: 0,
        links_skipped: 0,
        started,
        budget: options.time_budget(),
        stop,
    };

    for root in roots.roots() {
        if walk.should_stop() {
            break;
        }
        walk.root(root.path(), root.category());
    }

    let elapsed = started.elapsed();
    let report = ScanReport {
        inventory: walk.inventory,
        elapsed,
        directories_seen: walk.directories_seen,
        links_skipped: walk.links_skipped,
    };

    tracing::debug!(
        files = report.files_seen(),
        directories = report.directories_seen(),
        bytes = report.inventory().total_bytes(),
        elapsed_ms = elapsed.as_millis(),
        complete = report.inventory().is_complete(),
        "storage accounting scan finished"
    );

    report
}

/// One walk in progress.
struct Walk<'a> {
    inventory: StorageInventory,
    directories_seen: u64,
    links_skipped: u64,
    started: Instant,
    budget: Option<Duration>,
    stop: &'a dyn Fn() -> bool,
}

impl Walk<'_> {
    /// Whether the walk has run out of budget or been asked to stop, recording
    /// which of the two it was.
    fn should_stop(&mut self) -> bool {
        if self
            .budget
            .is_some_and(|budget| self.started.elapsed() >= budget)
        {
            self.inventory
                .record_partial(PartialReason::TimeBudgetExhausted);
            return true;
        }

        if (self.stop)() {
            self.inventory.record_partial(PartialReason::Cancelled);
            return true;
        }

        false
    }

    /// Walks one root, or records why it could not be.
    fn root(&mut self, path: &Path, category: StorageCategory) {
        match fs::metadata(path) {
            Ok(metadata) if metadata.is_dir() => self.directory_tree(path, category),
            Ok(_) => {
                // A root that is a file is a misconfiguration rather than a
                // missing drive, and it is not silently worth zero.
                self.unavailable(path, category, "the storage root is not a directory");
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if volume_is_reachable(path) {
                    // The ordinary first-run state: nothing has been recorded
                    // yet, so the directory has not been created. Worth zero,
                    // and the measurement is complete.
                    tracing::debug!(
                        root = %path.display(),
                        category = category.as_str(),
                        "storage root does not exist yet; counted as empty"
                    );
                } else {
                    // The drive is not connected. Counting this as zero would
                    // report a library that has vanished, which is the input
                    // that would make a cleanup delete what is left elsewhere.
                    self.unavailable(path, category, error.to_string());
                }
            }
            Err(error) => self.unavailable(path, category, error.to_string()),
        }
    }

    /// Records a root that could not be read.
    fn unavailable(&mut self, path: &Path, category: StorageCategory, reason: impl Into<String>) {
        let reason = reason.into();
        tracing::warn!(
            root = %path.display(),
            category = category.as_str(),
            reason = %reason,
            "storage root could not be read; the reported total is incomplete"
        );
        self.inventory
            .record_unavailable_root(UnavailableRoot::new(path, category, reason));
    }

    /// Walks a directory and everything below it, breadth first.
    ///
    /// An explicit stack rather than recursion: a library is user data and its
    /// depth is not this code's to assume, and a deep enough tree would exhaust
    /// the stack of whichever thread the caller chose.
    fn directory_tree(&mut self, root: &Path, category: StorageCategory) {
        let mut pending = vec![root.to_path_buf()];

        while let Some(directory) = pending.pop() {
            if self.should_stop() {
                return;
            }

            let entries = match fs::read_dir(&directory) {
                Ok(entries) => entries,
                Err(error) => {
                    tracing::warn!(
                        directory = %directory.display(),
                        reason = %error,
                        "a directory could not be read; the reported total is incomplete"
                    );
                    self.inventory
                        .record_partial(PartialReason::DirectoryUnreadable);
                    continue;
                }
            };

            self.directories_seen += 1;

            for (seen, entry) in entries.enumerate() {
                if seen % ENTRIES_PER_BUDGET_CHECK == ENTRIES_PER_BUDGET_CHECK - 1
                    && self.should_stop()
                {
                    return;
                }

                let Ok(entry) = entry else {
                    self.inventory
                        .record_partial(PartialReason::DirectoryUnreadable);
                    continue;
                };

                // `file_type` here is the type of the entry itself rather than
                // of whatever it points at, which is what makes a link
                // recognisable as one.
                let Ok(file_type) = entry.file_type() else {
                    self.inventory
                        .record_partial(PartialReason::DirectoryUnreadable);
                    continue;
                };

                if file_type.is_symlink() {
                    self.links_skipped += 1;
                    continue;
                }

                if file_type.is_dir() {
                    pending.push(entry.path());
                    continue;
                }

                let Ok(metadata) = entry.metadata() else {
                    self.inventory
                        .record_partial(PartialReason::DirectoryUnreadable);
                    continue;
                };

                self.inventory.record_added(FileEntry::new(
                    entry.path(),
                    category,
                    metadata.len(),
                    metadata.modified().ok(),
                ));
            }
        }
    }
}

/// Whether the volume a path names can be read at all.
///
/// The difference between "this directory has not been created yet", which is
/// worth zero bytes and is a complete answer, and "this drive is not connected",
/// which is worth nothing at all and must not be reported as zero.
fn volume_is_reachable(path: &Path) -> bool {
    volume_root(path).is_some_and(|volume| volume.exists())
}

/// The volume a path is on: `D:\` for `D:\Clipped\Recordings`, `/` for
/// `/clipped/recordings`, and the share for a UNC path.
///
/// `None` for a relative path, which a storage root cannot be
/// ([`StorageRoots::with`](crate::accounting::StorageRoots::with) refuses one).
fn volume_root(path: &Path) -> Option<PathBuf> {
    let mut root = PathBuf::new();

    for component in path.components() {
        match component {
            Component::Prefix(prefix) => root.push(prefix.as_os_str()),
            Component::RootDir => {
                root.push(std::path::MAIN_SEPARATOR_STR);
                return Some(root);
            }
            _ => break,
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A library of this test's own, removed when it is dropped.
    ///
    /// Named for the test, the process and the thread so that tests running in
    /// parallel — here and in the seven other crates of this workspace — cannot
    /// share one.
    #[derive(Debug)]
    struct TestLibrary(PathBuf);

    impl TestLibrary {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "clipped-accounting-{label}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("the temporary directory can be created");
            Self(path)
        }

        /// Writes a file of exactly `bytes` bytes at `relative`, creating the
        /// directories above it, and returns its path.
        fn file(&self, relative: &str, bytes: usize) -> PathBuf {
            let path = self.0.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("the parent directory can be created");
            }
            let mut file = fs::File::create(&path).expect("the file can be created");
            file.write_all(&vec![b'x'; bytes])
                .expect("the bytes can be written");
            path
        }

        fn directory(&self, relative: &str) -> PathBuf {
            let path = self.0.join(relative);
            fs::create_dir_all(&path).expect("the directory can be created");
            path
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestLibrary {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A drive letter no volume is mounted on, or `None` on a machine that has
    /// them all.
    fn unmounted_drive() -> Option<PathBuf> {
        (b'D'..=b'Z')
            .rev()
            .map(|letter| PathBuf::from(format!(r"{}:\", letter as char)))
            .find(|root| !root.exists())
    }

    #[test]
    fn the_reported_total_is_exactly_the_bytes_that_were_written() {
        // The acceptance criterion, at the level a test can be exact about: the
        // figure is compared against the sizes this test itself wrote, not
        // against a second reading of the same metadata.
        // docs/storage-management.md documents how this relates to what the
        // volume actually allocates.
        //
        // None of these sizes is a multiple of a 4 KiB cluster, deliberately. An
        // implementation that reported allocated size rather than file length
        // would agree with this test on round numbers and disagree here, which
        // is the difference the documented tolerance is about.
        let library = TestLibrary::new("exact-total");
        library.file("Counter-Strike 2/session-1/match-1.mkv", 1_000_003);
        library.file("Counter-Strike 2/session-1/match-2.mkv", 524_289);
        library.file("Minecraft/session-7/full.mkv", 2_000_001);

        let roots = StorageRoots::new()
            .with(StorageCategory::Recordings, library.path())
            .expect("an absolute path");
        let report = scan(&roots, &ScanOptions::new());

        assert_eq!(report.inventory().total_bytes(), 3_524_293);
        assert_eq!(report.files_seen(), 3);
        assert!(report.inventory().is_complete());
    }

    #[test]
    fn an_empty_file_is_counted_as_a_file_and_no_bytes() {
        let library = TestLibrary::new("empty-file");
        library.file("empty.mkv", 0);

        let roots = StorageRoots::new()
            .with(StorageCategory::Recordings, library.path())
            .expect("an absolute path");
        let report = scan(&roots, &ScanOptions::new());

        assert_eq!(report.files_seen(), 1);
        assert_eq!(report.inventory().total_bytes(), 0);
    }

    #[test]
    fn nested_directories_are_walked_and_directories_themselves_weigh_nothing() {
        let library = TestLibrary::new("nested");
        library.file("a/b/c/d/deep.mkv", 4_096);
        library.directory("a/b/empty");

        let roots = StorageRoots::new()
            .with(StorageCategory::Recordings, library.path())
            .expect("an absolute path");
        let report = scan(&roots, &ScanOptions::new());

        assert_eq!(report.files_seen(), 1);
        assert_eq!(report.inventory().total_bytes(), 4_096);
        assert!(
            report.directories_seen() >= 5,
            "the root and four levels below it: {}",
            report.directories_seen()
        );
    }

    #[test]
    fn each_root_gives_its_category_to_what_is_found_under_it() {
        let library = TestLibrary::new("categories");
        library.file("recordings/a.mkv", 10_000);
        library.file("thumbnails/a.jpg", 250);
        library.file("thumbnails/b.jpg", 350);

        let roots = StorageRoots::new()
            .with(
                StorageCategory::Recordings,
                library.path().join("recordings"),
            )
            .expect("an absolute path")
            .with(
                StorageCategory::Thumbnails,
                library.path().join("thumbnails"),
            )
            .expect("a second root");
        let report = scan(&roots, &ScanOptions::new());

        assert_eq!(
            report.inventory().bytes_for(StorageCategory::Recordings),
            10_000
        );
        assert_eq!(
            report.inventory().bytes_for(StorageCategory::Thumbnails),
            600
        );
        assert_eq!(report.inventory().total_bytes(), 10_600);
    }

    #[test]
    fn a_root_that_has_not_been_created_yet_is_empty_rather_than_unavailable() {
        // First run: no recording has been made, so nothing created the
        // directory. That is a complete measurement of nothing.
        let library = TestLibrary::new("not-created");
        let roots = StorageRoots::new()
            .with(
                StorageCategory::Recordings,
                library.path().join("nothing-here"),
            )
            .expect("an absolute path");

        let report = scan(&roots, &ScanOptions::new());

        assert!(
            report.inventory().is_complete(),
            "{:?}",
            report.completeness()
        );
        assert_eq!(report.inventory().total_bytes(), 0);
        assert!(report.inventory().unavailable_roots().is_empty());
    }

    #[test]
    fn a_root_on_a_drive_that_is_not_there_is_reported_rather_than_counted_as_zero() {
        // The dangerous case. A disconnected recording drive that measured zero
        // would say the library had shrunk to nothing, and everything built on
        // that figure — the quota, and issue #111's cleanup — would act on it.
        let Some(drive) = unmounted_drive() else {
            eprintln!("skipping: every drive letter on this machine is in use");
            return;
        };

        let roots = StorageRoots::new()
            .with(StorageCategory::Recordings, drive.join("Clipped"))
            .expect("an absolute path");
        let report = scan(&roots, &ScanOptions::new());

        assert!(!report.inventory().is_complete());
        assert_eq!(
            report.completeness().reasons(),
            &[PartialReason::RootUnavailable]
        );
        assert_eq!(report.inventory().unavailable_roots().len(), 1);
        assert_eq!(
            report.inventory().unavailable_roots()[0].category(),
            StorageCategory::Recordings
        );
    }

    #[test]
    fn one_unavailable_root_does_not_stop_the_others_being_measured() {
        let Some(drive) = unmounted_drive() else {
            eprintln!("skipping: every drive letter on this machine is in use");
            return;
        };
        let library = TestLibrary::new("one-drive-missing");
        library.file("clips/a.mkv", 8_192);

        let roots = StorageRoots::new()
            .with(StorageCategory::Recordings, drive.join("Clipped"))
            .expect("an absolute path")
            .with(StorageCategory::Clips, library.path().join("clips"))
            .expect("a second root");
        let report = scan(&roots, &ScanOptions::new());

        assert_eq!(report.inventory().total_bytes(), 8_192);
        assert!(!report.inventory().is_complete());
    }

    #[test]
    fn a_root_that_is_a_file_is_a_misconfiguration_rather_than_an_empty_library() {
        let library = TestLibrary::new("root-is-a-file");
        let file = library.file("a-file.mkv", 128);

        let roots = StorageRoots::new()
            .with(StorageCategory::Recordings, file)
            .expect("an absolute path");
        let report = scan(&roots, &ScanOptions::new());

        assert!(!report.inventory().is_complete());
        assert_eq!(report.inventory().unavailable_roots().len(), 1);
    }

    #[test]
    fn a_scan_with_no_budget_left_stops_before_it_starts_and_says_why() {
        let library = TestLibrary::new("no-budget");
        library.file("a.mkv", 1_024);

        let roots = StorageRoots::new()
            .with(StorageCategory::Recordings, library.path())
            .expect("an absolute path");
        let report = scan(&roots, &ScanOptions::new().with_time_budget(Duration::ZERO));

        assert_eq!(report.files_seen(), 0);
        assert!(!report.inventory().is_complete());
        assert_eq!(
            report.completeness().reasons(),
            &[PartialReason::TimeBudgetExhausted]
        );
    }

    #[test]
    fn a_generous_budget_does_not_interrupt_anything() {
        let library = TestLibrary::new("generous-budget");
        library.file("a.mkv", 1_024);
        library.file("b/c.mkv", 2_048);

        let roots = StorageRoots::new()
            .with(StorageCategory::Recordings, library.path())
            .expect("an absolute path");
        let report = scan(
            &roots,
            &ScanOptions::new().with_time_budget(Duration::from_secs(60)),
        );

        assert!(report.inventory().is_complete());
        assert_eq!(report.inventory().total_bytes(), 3_072);
    }

    #[test]
    fn a_cancelled_scan_keeps_what_it_measured_and_is_marked_partial() {
        let library = TestLibrary::new("cancelled");
        library.file("top.mkv", 1_000);
        for index in 0..8 {
            library.file(&format!("session-{index}/a.mkv"), 1_000);
        }

        let roots = StorageRoots::new()
            .with(StorageCategory::Recordings, library.path())
            .expect("an absolute path");

        // Stops at the second directory the walk asks about, so some of the
        // library has been measured and some has not.
        let asked = AtomicUsize::new(0);
        let report = scan_until(&roots, &ScanOptions::new(), &|| {
            asked.fetch_add(1, Ordering::Relaxed) >= 2
        });

        assert!(!report.inventory().is_complete());
        assert_eq!(report.completeness().reasons(), &[PartialReason::Cancelled]);
        assert_eq!(
            report.files_seen(),
            1,
            "the top-level file was measured before the stop, and nothing below it was"
        );
        assert_eq!(report.inventory().total_bytes(), 1_000);
    }

    #[test]
    fn a_scan_that_is_never_asked_to_stop_measures_everything() {
        let library = TestLibrary::new("never-cancelled");
        library.file("a.mkv", 4_000);

        let roots = StorageRoots::new()
            .with(StorageCategory::Recordings, library.path())
            .expect("an absolute path");
        let report = scan_until(&roots, &ScanOptions::new(), &|| false);

        assert!(report.inventory().is_complete());
        assert_eq!(report.inventory().total_bytes(), 4_000);
    }

    /// Creates a directory link at `link` pointing at `target`, or reports why
    /// it could not.
    ///
    /// A symbolic link needs Developer Mode or an elevated shell, and a plain
    /// user account on a default Windows install has neither. A **junction**
    /// needs no privilege at all, is the same kind of reparse point as far as a
    /// walk is concerned, and is what a user who moved their recordings to
    /// another drive is most likely to have made — so the test falls back to one
    /// rather than skipping, and only skips if both are refused.
    #[cfg(windows)]
    fn link_directory(link: &Path, target: &Path) -> Result<(), String> {
        if std::os::windows::fs::symlink_dir(target, link).is_ok() {
            return Ok(());
        }

        let output = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .map_err(|error| format!("mklink could not be run: {error}"))?;

        if output.status.success() && link.exists() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&output.stdout).trim().to_owned())
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_directory_link_is_skipped_rather_than_followed() {
        // Following one would count the same files twice, and a link pointing
        // at an ancestor would walk for ever.
        let library = TestLibrary::new("links");
        library.file("real/a.mkv", 5_000);

        if let Err(reason) =
            link_directory(&library.path().join("link"), &library.path().join("real"))
        {
            eprintln!("skipping: no directory link could be created ({reason})");
            return;
        }

        let roots = StorageRoots::new()
            .with(StorageCategory::Recordings, library.path())
            .expect("an absolute path");
        let report = scan(&roots, &ScanOptions::new());

        assert_eq!(
            report.files_seen(),
            1,
            "the file behind the link is one file"
        );
        assert_eq!(report.inventory().total_bytes(), 5_000, "counted once");
        assert_eq!(report.links_skipped(), 1);
    }

    #[test]
    fn a_volume_root_is_the_drive_or_the_share() {
        #[cfg(windows)]
        {
            assert_eq!(
                volume_root(Path::new(r"D:\Clipped\Recordings")),
                Some(PathBuf::from(r"D:\"))
            );
            assert_eq!(
                volume_root(Path::new(r"\\server\share\Clipped")),
                Some(PathBuf::from(r"\\server\share\"))
            );
        }

        #[cfg(not(windows))]
        assert_eq!(
            volume_root(Path::new("/clipped/recordings")),
            Some(PathBuf::from("/"))
        );

        assert_eq!(volume_root(Path::new("relative/path")), None);
    }

    #[test]
    fn the_volume_under_the_temporary_directory_is_reachable() {
        assert!(volume_is_reachable(&std::env::temp_dir()));
    }

    #[test]
    fn no_root_at_all_measures_nothing_and_is_complete() {
        let report = scan(&StorageRoots::new(), &ScanOptions::new());

        assert_eq!(report.files_seen(), 0);
        assert!(report.inventory().is_complete());
    }
}
