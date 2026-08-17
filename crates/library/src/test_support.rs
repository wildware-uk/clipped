//! Scaffolding the tests in this crate share.
//!
//! A library is a database, a folder of recordings and a trash beside them, so
//! most of what is worth testing here needs real directories on a real disk.
//! The workspace has no `tempfile` dependency and this is not enough reason to
//! add one; `crates/storage` and `crates/session` build the same thing from
//! `std::env::temp_dir` and the process id (AGENTS.md sections 10 and 55).
//!
//! This module used to be `index::test_support` and moved up when the trash,
//! the locks, the favourites and the thumbnail scan all wanted the same
//! directory: `trash::vault` was already reaching into `index` for it.

use std::ops::Deref;
use std::path::{Path, PathBuf};

/// A directory of this test's own, removed when the test that made it ends.
///
/// # Why it removes itself
///
/// Naming it after the test, the process and the thread stops two tests — here,
/// in another crate, or in another `cargo test` run — sharing one, and emptying
/// it on the way in makes each run start clean. Neither of those cleans up.
/// The process identifier is part of the name, so the next run picks a
/// different name and the last run's copy stays where it is, for good. Counted
/// on one developer's machine, that came to roughly twenty-two thousand
/// abandoned scratch libraries under `%TEMP%`, four thousand of them from the
/// trash suite alone
/// ([issue #595](https://github.com/wildware-uk/clipped/issues/595)).
///
/// # Why a failing test keeps its directory
///
/// [`Drop`] runs while a panicking thread unwinds, and a failed assertion in a
/// `#[test]` is a panic, so removing unconditionally would take the evidence
/// with it — which is why the helpers this replaces cleaned up on the way in
/// instead. Asking [`std::thread::panicking`] separates the two cases: a
/// passing test's files are worth nothing and go, a failing test's are the only
/// record of what it saw and stay, with the path printed so that whoever reads
/// the failure knows where to look.
///
/// A directory only survives this far if the test process unwound. A test that
/// is killed — a timeout, a `--nocapture` interrupt, an abort — leaves its
/// directory behind, and nothing here can change that.
///
/// # Holding one
///
/// Bind it to something that outlives the test body. In particular, a database
/// opened inside the directory has to be dropped first, or Windows refuses to
/// remove a file that is still open; bindings in one `let` are dropped in
/// reverse order, so `let (directory, database) = ...` is the right way round
/// and `let (database, directory) = ...` is not.
pub(crate) struct Scratch(PathBuf);

impl Scratch {
    /// An empty `%TEMP%\clipped-{label}-{process}-{thread}`.
    pub(crate) fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "clipped-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
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

impl Drop for Scratch {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("scratch directory kept for diagnosis: {}", self.0.display());
            return;
        }
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            // Said rather than swallowed: a scratch directory that cannot be
            // removed is the defect this type exists for, and the usual cause
            // is a database still open in the test that made it.
            eprintln!(
                "scratch directory could not be removed: {} ({error})",
                self.0.display()
            );
        }
    }
}

/// A scratch directory for one of the index's tests. See [`Scratch`].
pub(crate) fn scratch_directory(name: &str) -> Scratch {
    Scratch::new(&format!("library-{name}"))
}
