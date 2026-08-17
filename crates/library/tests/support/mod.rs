//! The scratch directory every test binary in this directory builds a library
//! in.
//!
//! Shared here rather than copied into each of them for the reason
//! `apps/recorder/tests/support/mod.rs` exists: the same six lines were in four
//! files, and when they were wrong they were wrong in four places
//! ([issue #595](https://github.com/wildware-uk/clipped/issues/595)).
//!
//! It cannot be shared with the crate's unit tests in `src/test_support.rs`,
//! which is the same type again. A `#[cfg(test)]` module is not visible to an
//! integration test — they are separate crates — and the only ways round that
//! are a public API that exists for tests or a dependency on the crate from
//! itself. Neither is worth it for thirty lines (AGENTS.md section 44); what
//! matters is that both copies remove what they make, and `%TEMP%` says
//! whether they do.

// Each test binary compiles this module separately and uses the part of it that
// it needs, so anything used by only one of them is "unused" in the others.
#![allow(dead_code)]

use std::ops::Deref;
use std::path::{Path, PathBuf};

/// A directory of this test's own, removed when the test that made it ends.
///
/// # Why it removes itself
///
/// Naming it after the test and the process stops two tests — here, in another
/// crate, or in another `cargo test` run — sharing one, and emptying it on the
/// way in makes each run start clean. Neither of those cleans up. The process
/// identifier is part of the name, so the next run picks a different name and
/// the last run's copy stays where it is, for good. Counted on one developer's
/// machine, the trash suite alone had left just under four thousand scratch
/// libraries under `%TEMP%`, one per case per run, going back to the day the
/// suite was written.
///
/// # Why a failing test keeps its directory
///
/// [`Drop`] runs while a panicking thread unwinds, and a failed assertion in a
/// `#[test]` is a panic, so removing unconditionally would take the evidence
/// with it. Asking [`std::thread::panicking`] separates the two cases: a
/// passing test's library is worth nothing and goes, a failing test's is the
/// only record of what it saw and stays, with the path printed so that whoever
/// reads the failure knows where to look.
///
/// A directory only survives this far if the test process unwound. A test that
/// is killed — a timeout, an interrupt, an abort — leaves its directory behind,
/// and nothing here can change that.
///
/// # Holding one
///
/// Bind it to something that outlives the test body, and open no database
/// inside it that outlives *this*: Windows will not remove a file that is still
/// open, so a `Database` has to be dropped first. Bindings in one `let` are
/// dropped in reverse order, so `let (directory, database) = …` is the right
/// way round.
pub(crate) struct Scratch(PathBuf);

impl Scratch {
    /// An empty `%TEMP%\clipped-{label}-{process}`.
    pub(crate) fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("clipped-{label}-{}", std::process::id()));
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
