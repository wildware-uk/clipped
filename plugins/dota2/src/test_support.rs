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
/// of this repository — never share one.
///
/// It used to be emptied only on the way in, on the argument that what a
/// failing test left behind is worth having. That argument is right and the
/// implementation was not: it kept what a *passing* test left behind too, which
/// is where 766 `clipped-dota2-*` directories came from
/// ([issue #598](https://github.com/wildware-uk/clipped/issues/598)). Removing
/// on the way out *unless the test is panicking* keeps the evidence and stops
/// the accumulation, which is the pattern PR #597 settled.
pub(crate) fn scratch_directory(name: &str) -> Scratch {
    Scratch::new(&format!("dota2-{name}"))
}

/// A scratch directory that removes itself when the test that made it passes.
///
/// Two halves, and both matter ([issue #598](https://github.com/wildware-uk/clipped/issues/598)):
///
/// - **A failing test keeps its directory**, with the path printed, because the
///   files in it are the evidence.
/// - **A removal that fails is said aloud.** Windows refuses to remove a
///   directory holding an open file, and a discarded `Err` turns that into a
///   test reporting success having leaked.
///
/// Never a sweep by prefix: several suites run at once, and a sweep deletes
/// another run's directories out from under it.
pub(crate) struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("clipped-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a scratch directory can be created");
        Self(path)
    }
}

impl std::ops::Deref for Scratch {
    type Target = std::path::Path;

    fn deref(&self) -> &std::path::Path {
        &self.0
    }
}

impl AsRef<std::path::Path> for Scratch {
    fn as_ref(&self) -> &std::path::Path {
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
            eprintln!(
                "scratch directory could not be removed: {} ({error})",
                self.0.display()
            );
        }
    }
}
