//! Proves that measuring a library does not put the user's path in the log.
//!
//! A storage root is `C:\Users\<account>\Videos\Clipped\Recordings`, so every
//! path storage accounting could log names the account and the folders somebody
//! chose. AGENTS.md section 13 forbids that and `docs/logging.md` lists exactly
//! this shape of path in its forbidden set, which is what `RedactedPath` exists
//! for.
//!
//! These tests drive a real scan into a real subscriber and assert on the bytes
//! that would have been written to a log file. Each one also asserts that the
//! scan reached the state whose log line is being checked — an unavailable root,
//! a root that has not been created, a directory that could not be read — so
//! that a test cannot pass by the log line never happening.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use clipped_library::accounting::{
    scan, scan_until, PartialReason, ScanOptions, StorageCategory, StorageRoots,
};
use tracing_subscriber::fmt::MakeWriter;

/// Collects everything a subscriber writes, so a test can assert on it.
#[derive(Clone, Default)]
struct CapturedLog(Arc<Mutex<Vec<u8>>>);

impl CapturedLog {
    fn contents(&self) -> String {
        String::from_utf8(
            self.0
                .lock()
                .expect("the capture buffer is not poisoned")
                .clone(),
        )
        .expect("the subscriber writes UTF-8")
    }
}

impl io::Write for CapturedLog {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .expect("the capture buffer is not poisoned")
            .extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'writer> MakeWriter<'writer> for CapturedLog {
    type Writer = Self;

    fn make_writer(&'writer self) -> Self::Writer {
        self.clone()
    }
}

/// Runs `body` against a subscriber local to this thread and returns what it
/// wrote. Thread-local rather than global so these tests stay independent of
/// each other and of test ordering.
fn captured<T>(body: impl FnOnce() -> T) -> (T, String) {
    let captured = CapturedLog::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(captured.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .finish();

    let value = tracing::subscriber::with_default(subscriber, body);
    (value, captured.contents())
}

/// A library under the system temporary directory, laid out the way a real one
/// is — `<somebody>/Videos/Clipped/...` — and removed when it is dropped.
///
/// The directory names are the point: they stand in for an account name and the
/// folders a user chose, and every one of them is asserted absent from the log.
struct PrivateLibrary {
    /// The outermost directory this created, and what is removed again. Kept
    /// separately from the library below it so that dropping does not leave the
    /// account-shaped directories behind on a shared machine.
    owned: PathBuf,
    library: PathBuf,
}

impl PrivateLibrary {
    const ACCOUNT: &'static str = "alice-a1b2c3";

    fn new(label: &str) -> Self {
        let owned = std::env::temp_dir().join(format!(
            "clipped-accounting-privacy-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let library = owned.join(Self::ACCOUNT).join("Videos").join("Clipped");
        let _ = fs::remove_dir_all(&owned);
        fs::create_dir_all(&library).expect("the temporary directory can be created");
        Self { owned, library }
    }

    fn path(&self) -> &Path {
        &self.library
    }

    /// Everything that must not reach the log: the account name, the folder
    /// names, and the whole path in the form it would be printed in.
    ///
    /// The temporary directory on Windows is itself inside the running user's
    /// profile, so its own text is in this list too.
    fn forbidden(&self) -> Vec<String> {
        vec![
            Self::ACCOUNT.to_owned(),
            "Videos".to_owned(),
            self.library.display().to_string(),
            std::env::temp_dir().display().to_string(),
        ]
    }

    fn assert_nothing_leaked(&self, log: &str) {
        assert!(!log.is_empty(), "nothing was logged at all");
        for leaked in self.forbidden() {
            assert!(!log.contains(&leaked), "the log leaked {leaked:?}:\n{log}");
        }
    }
}

impl Drop for PrivateLibrary {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.owned);
    }
}

#[test]
fn an_unreadable_root_is_reported_without_naming_where_it_is() {
    // The warning a user's own log file carries on the ordinary disconnected
    // drive path. A root that is a file reaches the same site without needing a
    // drive letter this machine may not have spare.
    let library = PrivateLibrary::new("unreadable-root");
    let root = library.path().join("Recordings");
    fs::write(&root, b"not a directory").expect("the file can be written");

    let roots = StorageRoots::new()
        .with(StorageCategory::Recordings, &root)
        .expect("an absolute path");

    let (report, log) = captured(|| scan(&roots, &ScanOptions::new()));

    assert_eq!(
        report.inventory().unavailable_roots().len(),
        1,
        "this test is only meaningful if the log site was reached"
    );
    assert!(
        log.contains("storage root could not be read"),
        "the warning is what is being checked:\n{log}"
    );
    assert!(
        log.contains("root=Recordings#"),
        "the reduced form identifies the root without describing it:\n{log}"
    );
    library.assert_nothing_leaked(&log);
}

#[test]
fn a_root_that_does_not_exist_yet_is_reported_without_naming_where_it_is() {
    // The first-run path, logged at debug. Quieter than the warning above and
    // just as capable of carrying an account name into a log file.
    let library = PrivateLibrary::new("absent-root");
    let root = library.path().join("Recordings");

    let roots = StorageRoots::new()
        .with(StorageCategory::Recordings, &root)
        .expect("an absolute path");

    let (report, log) = captured(|| scan(&roots, &ScanOptions::new()));

    assert!(
        report.inventory().is_complete(),
        "this test is only meaningful if the log site was reached: {:?}",
        report.completeness()
    );
    assert!(
        log.contains("storage root does not exist yet"),
        "the debug line is what is being checked:\n{log}"
    );
    assert!(log.contains("root=Recordings#"), "{log}");
    library.assert_nothing_leaked(&log);
}

#[test]
fn a_directory_that_cannot_be_read_is_reported_without_naming_where_it_is() {
    // A directory removed while the scan is walking, which is what a user
    // tidying up in Explorer during a scan produces. The removal happens from
    // the stop closure, which the walk calls before each directory, so the
    // failure is deterministic rather than a race this test hopes for.
    let library = PrivateLibrary::new("unreadable-directory");
    let first = library.path().join("session-a");
    let second = library.path().join("session-b");
    fs::create_dir_all(&first).expect("the directory can be created");
    fs::create_dir_all(&second).expect("the directory can be created");
    fs::write(second.join("kept.mkv"), b"x").expect("the file can be written");

    let roots = StorageRoots::new()
        .with(StorageCategory::Recordings, library.path())
        .expect("an absolute path");

    let asked = AtomicUsize::new(0);
    let (report, log) = captured(|| {
        scan_until(&roots, &ScanOptions::new(), &|| {
            // The walk asks before the root, then again before reading the
            // root's own entries. By the third question the root has been
            // enumerated and both subdirectories are queued, so removing one
            // leaves the walk holding a directory that is no longer there —
            // whichever of the two it takes first.
            if asked.fetch_add(1, Ordering::Relaxed) == 2 {
                let _ = fs::remove_dir_all(&first);
            }
            false
        })
    });

    assert!(
        report
            .completeness()
            .reasons()
            .contains(&PartialReason::DirectoryUnreadable),
        "this test is only meaningful if the log site was reached: {:?}",
        report.completeness()
    );
    assert!(
        log.contains("a directory could not be read"),
        "the warning is what is being checked:\n{log}"
    );
    assert!(log.contains("directory=session-a#"), "{log}");
    library.assert_nothing_leaked(&log);
}
