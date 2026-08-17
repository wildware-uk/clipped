//! The scratch directory this crate's unit tests build their files in.
//!
//! Nine modules here each had a helper that made a directory under `%TEMP%` and
//! named it after the test. Four of them removed it again, five did not, and
//! `%TEMP%` on the machine this was written on held 3,312 `clipped-recorder-*`
//! and 1,787 `clipped-serve-*` directories to show for it
//! ([issue #598](https://github.com/wildware-uk/clipped/issues/598)). One copy
//! is easier to keep right than nine.
//!
//! The workspace has no `tempfile` dependency and this is not enough reason to
//! add one; `crates/library`, `crates/storage` and `crates/session` all build
//! the same thing from `std::env::temp_dir` and the process id (AGENTS.md
//! sections 10 and 55). This is `crates/library/src/test_support.rs` again,
//! with the thread identifier replaced by a counter for the reason given on
//! [`Scratch::new`].
//!
//! The integration tests next door cannot see this — a `#[cfg(test)]` module is
//! not visible from another crate — and keep their own copy in
//! `apps/recorder/tests/support/mod.rs`.

use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// A directory of one test's own, removed when the test that made it ends.
///
/// # Why it removes itself
///
/// Naming a directory after the test and the process stops two tests — here, in
/// another crate, or in another `cargo test` run — sharing one, and emptying it
/// on the way in makes each run start clean. Neither of those cleans up. The
/// process identifier is part of the name, so the next run picks a different
/// name and the last run's copy stays where it is, for good.
///
/// # Why a failing test keeps its directory
///
/// [`Drop`] runs while a panicking thread unwinds, and a failed assertion in a
/// `#[test]` is a panic, so removing unconditionally would take the evidence
/// with it — which is why several of the helpers this replaces emptied the
/// directory on the way *in* instead. Asking [`std::thread::panicking`]
/// separates the two cases: a passing test's files are worth nothing and go, a
/// failing test's are the only record of what it saw and stay, with the path
/// printed so that whoever reads the failure knows where to look.
///
/// A directory only survives this far if the test process unwound. A test that
/// is killed — a timeout, an interrupt, an abort — leaves its directory behind,
/// and nothing here can change that (AGENTS.md section 54).
///
/// # Holding one
///
/// Bind it to something that outlives the test body. In particular a
/// `Database`, or anything holding one — [`crate::library::LibraryReader`]
/// caches one after its first read — has to be dropped first, because Windows
/// refuses to remove a file that is still open. Bindings in one `let` and
/// fields in one struct are dropped in the order that puts the database first:
/// `let (directory, reader) = …` is the right way round, and a struct declaring
/// `reader` above `directory` is too.
pub(crate) struct Scratch(PathBuf);

impl Scratch {
    /// An empty `%TEMP%\clipped-{label}-{process}-{n}`.
    ///
    /// The counter is what `crates/library` spends a thread identifier on, and
    /// it is stricter: two directories made by one test — several tests here
    /// want a source and a destination — would land on the same thread, and the
    /// second call, which empties the directory it is given, would delete the
    /// first one's files out from under it.
    pub(crate) fn new(label: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);

        let path = std::env::temp_dir().join(format!(
            "clipped-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        // Process identifiers are reused, so a directory of this name can
        // survive from an earlier run that was killed. Emptying it means a test
        // never reads a file it did not write.
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a scratch directory can be created");
        Self(path)
    }
}

impl Deref for Scratch {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl std::fmt::Debug for Scratch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("scratch directory kept for diagnosis: {}", self.0.display());
            return;
        }
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            // Said rather than swallowed: a scratch directory that cannot be
            // removed is the defect this type exists for, and the usual cause
            // is a database still open in the test that made it. The old
            // helpers wrote `let _ = fs::remove_dir_all(…)` and so reported
            // success without having succeeded.
            eprintln!(
                "scratch directory could not be removed: {} ({error})",
                self.0.display()
            );
        }
    }
}
