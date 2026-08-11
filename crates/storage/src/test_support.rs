//! Scaffolding the tests in this crate share.
//!
//! A database is a file, so most of what is worth testing here needs a real one
//! on a real disk: write-ahead logging, `VACUUM INTO`, and what a failure leaves
//! behind are all properties of the filesystem and none of them are observable
//! in memory.

use std::path::PathBuf;

use rusqlite::Connection;

/// An empty directory of this test's own, under the platform's temporary
/// directory.
///
/// Named after the test and the process, so two tests running in parallel — and
/// two `cargo test` runs on the same machine — never share one. It is emptied
/// on the way in rather than on the way out: a failing test's database is worth
/// having afterwards, and the next run starts clean regardless.
pub(crate) fn scratch_directory(name: &str) -> PathBuf {
    let directory =
        std::env::temp_dir().join(format!("clipped-storage-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("a scratch directory can be created");
    directory
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
