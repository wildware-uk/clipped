//! Scaffolding this crate's tests share.
//!
//! One copy of "a directory of this test's own", for the same reason
//! `clipped-storage` has one: two tests that each write their own version of it
//! are two chances to write one that collides with a test running beside it
//! (AGENTS.md sections 25 and 26).

use std::path::PathBuf;

/// An empty directory of this test's own, under the platform's temporary
/// directory.
///
/// Named after the test and the process, so that two tests running in parallel
/// — and two `cargo test` runs on the same machine, which is the ordinary state
/// of this repository — never share one. It is emptied on the way in rather
/// than on the way out: what a failing test left behind is worth having, and
/// the next run starts clean regardless.
pub(crate) fn scratch_directory(name: &str) -> PathBuf {
    let directory =
        std::env::temp_dir().join(format!("clipped-dota2-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("a scratch directory can be created");
    directory
}
