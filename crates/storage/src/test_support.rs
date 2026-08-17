//! Scaffolding the tests in this crate share.
//!
//! A database is a file, so most of what is worth testing here needs a real one
//! on a real disk: write-ahead logging, `VACUUM INTO`, and what a failure leaves
//! behind are all properties of the filesystem and none of them are observable
//! in memory.

use std::ops::Deref;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

/// An empty directory of this test's own, under the platform's temporary
/// directory, removed when the test that made it ends.
///
/// Named after the test and the process, so two tests running in parallel — and
/// two `cargo test` runs on the same machine — never share one. Emptying on the
/// way in is not enough on its own: the process identifier is part of the name,
/// so the next run picks a different one and the last run's copy stays for
/// good. That is how this machine came to hold five thousand of them
/// ([issue #595](https://github.com/wildware-uk/clipped/issues/595)).
///
/// Removal is skipped when the thread is unwinding, which is what a failed
/// assertion in a `#[test]` is doing by the time this runs. A passing test's
/// database is worth nothing; a failing one's is the only evidence there is, so
/// it stays and the path is printed.
pub(crate) struct Scratch(PathBuf);

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

/// See [`Scratch`]. Bind the answer to a variable that outlives the test body:
/// the directory goes when it does.
pub(crate) fn scratch_directory(name: &str) -> Scratch {
    let directory =
        std::env::temp_dir().join(format!("clipped-storage-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("a scratch directory can be created");
    Scratch(directory)
}

/// Every table in a database, sorted.
pub(crate) fn table_names(connection: &Connection) -> Vec<String> {
    let mut statement = connection
        .prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' \
             AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .expect("the table list can be read");
    let names = statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("the table list can be queried");
    names.map(|name| name.expect("a table name")).collect()
}
