//! Scaffolding the unit tests in this module share.
//!
//! Indexing is a conversation between a database, a directory and a clock, so
//! most of what is worth testing here needs a real directory on a real disk.
//! The workspace has no `tempfile` dependency and this is not enough reason to
//! add one; `crates/storage` and `crates/session` build the same thing from
//! `std::env::temp_dir` and the process id (AGENTS.md sections 10 and 55).

use std::path::PathBuf;

/// An empty directory of this test's own.
///
/// Named after the test and the process, so two tests running in parallel — and
/// two `cargo test` runs on the same machine — never share one. It is emptied
/// on the way in rather than on the way out, because a failing test's files are
/// worth having afterwards.
pub(crate) fn scratch_directory(name: &str) -> PathBuf {
    let directory =
        std::env::temp_dir().join(format!("clipped-library-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("a scratch directory can be created");
    directory
}
