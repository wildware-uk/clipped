//! Somewhere to put a recording while a test looks at it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// A directory under the system temporary directory, removed when dropped.
///
/// Recordings are large and media tests are run repeatedly, so nothing is left
/// behind — including when a test fails, since `Drop` runs while the panic
/// unwinds.
#[derive(Debug)]
pub struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    /// Creates a directory named after this process and `purpose`, so that two
    /// test binaries running at once cannot collide.
    ///
    /// # Panics
    ///
    /// When the directory cannot be created, which is a broken machine rather
    /// than a failed expectation.
    pub fn new(purpose: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "clipped-{purpose}-{}-{}-{unique}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("a temporary directory can be created");
        Self(path)
    }

    /// A path inside the directory. Nothing is created.
    pub fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    /// The directory itself.
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
