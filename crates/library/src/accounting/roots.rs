//! Where each kind of file lives, as the caller declares it.
//!
//! Accounting does not know the on-disk layout and deliberately does not guess
//! it: the layout belongs to `clipped-storage`, the recording location is a
//! setting the user can change, and a module that hardcoded either would be
//! wrong the first time somebody moved their library to another drive. So the
//! caller declares a root per category and accounting measures what it is given.
//!
//! One rule is enforced here rather than trusted, because getting it wrong
//! silently doubles a user's reported usage: **no root may contain another**. A
//! trash directory inside the recordings directory would be walked twice — once
//! as trash and once as recordings — and the total would be wrong in the
//! direction that makes a quota delete footage it did not need to.

use std::path::{Path, PathBuf};

use crate::accounting::error::RootsError;
use crate::accounting::StorageCategory;

/// One directory, and what kind of file it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageRoot {
    category: StorageCategory,
    path: PathBuf,
}

impl StorageRoot {
    /// What kind of file this directory holds.
    #[must_use]
    pub const fn category(&self) -> StorageCategory {
        self.category
    }

    /// The directory itself.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// The complete set of directories a library occupies.
///
/// "Complete" is the load-bearing word. A set that omits a category is a total
/// that omits it too, and the module documentation says why that is worse than
/// reporting nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StorageRoots {
    roots: Vec<StorageRoot>,
}

impl StorageRoots {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a directory holding `category`.
    ///
    /// More than one root may share a category — recordings spread over two
    /// drives are two roots — so this adds rather than replaces.
    ///
    /// # Errors
    ///
    /// [`RootsError::NotAbsolute`] if the path is relative. A relative path is
    /// resolved against the process's working directory, which the recorder and
    /// the desktop application do not share, so the same setting would measure
    /// two different directories.
    ///
    /// [`RootsError::Overlapping`] if it contains, or is contained by, a root
    /// already added — including being the same directory twice.
    pub fn with(
        mut self,
        category: StorageCategory,
        path: impl Into<PathBuf>,
    ) -> Result<Self, RootsError> {
        let path = path.into();

        if !path.is_absolute() {
            return Err(RootsError::NotAbsolute { path });
        }

        if let Some(existing) = self
            .roots
            .iter()
            .find(|root| overlaps(&root.path, path.as_path()))
        {
            return Err(RootsError::Overlapping {
                existing: existing.path.clone(),
                added: path,
            });
        }

        self.roots.push(StorageRoot { category, path });
        Ok(self)
    }

    /// The roots, in the order they were added.
    #[must_use]
    pub fn roots(&self) -> &[StorageRoot] {
        &self.roots
    }

    /// Whether no root has been declared.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// How many roots have been declared.
    #[must_use]
    pub fn len(&self) -> usize {
        self.roots.len()
    }
}

/// Whether either path contains the other, or they are the same directory.
fn overlaps(left: &Path, right: &Path) -> bool {
    contains(left, right) || contains(right, left)
}

/// Whether `outer` is `inner` or an ancestor of it.
///
/// Compared component by component rather than by string prefix, so that
/// `C:\Videos2` is not treated as living inside `C:\Videos`. Neither path is
/// touched on disk: this has to work for a drive that is not connected, which is
/// exactly when a user is configuring one.
fn contains(outer: &Path, inner: &Path) -> bool {
    let mut outer = outer.components();
    let mut inner = inner.components();

    loop {
        match (outer.next(), inner.next()) {
            (None, _) => return true,
            (Some(_), None) => return false,
            (Some(left), Some(right)) => {
                if !components_equal(left.as_os_str(), right.as_os_str()) {
                    return false;
                }
            }
        }
    }
}

/// Whether two path components name the same thing.
///
/// Windows filenames are case-insensitive, so `D:\Clips` and `d:\clips` are one
/// directory and must not be accepted as two roots. `eq_ignore_ascii_case` is
/// deliberately not full Unicode case folding: NTFS folds using the case table
/// that was current when the volume was formatted, which no user-space
/// comparison can reproduce exactly. ASCII covers drive letters and every
/// directory name Clipped itself creates, and the cost of the residue is one
/// duplicated root that is refused rather than a wrong total.
#[cfg(windows)]
fn components_equal(left: &std::ffi::OsStr, right: &std::ffi::OsStr) -> bool {
    left.eq_ignore_ascii_case(right)
}

/// Whether two path components name the same thing.
///
/// Case-sensitive off Windows, which is what those filesystems are.
#[cfg(not(windows))]
fn components_equal(left: &std::ffi::OsStr, right: &std::ffi::OsStr) -> bool {
    left == right
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An absolute path, spelled the way the platform running the test spells
    /// one.
    fn absolute(tail: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!(r"D:\Clipped\{tail}"))
        } else {
            PathBuf::from(format!("/clipped/{tail}"))
        }
    }

    #[test]
    fn a_root_keeps_the_category_it_was_declared_with() {
        let roots = StorageRoots::new()
            .with(StorageCategory::Recordings, absolute("Recordings"))
            .expect("an absolute path");

        assert_eq!(roots.len(), 1);
        assert_eq!(roots.roots()[0].category(), StorageCategory::Recordings);
        assert_eq!(roots.roots()[0].path(), absolute("Recordings"));
    }

    #[test]
    fn two_roots_may_share_a_category() {
        let roots = StorageRoots::new()
            .with(StorageCategory::Recordings, absolute("Recordings"))
            .expect("an absolute path")
            .with(StorageCategory::Recordings, absolute("More Recordings"))
            .expect("a second drive is a second root");

        assert_eq!(roots.len(), 2);
    }

    #[test]
    fn a_relative_path_is_refused() {
        let error = StorageRoots::new()
            .with(StorageCategory::Recordings, "Recordings")
            .expect_err("a relative path means two processes measure two directories");

        assert!(matches!(error, RootsError::NotAbsolute { .. }), "{error}");
    }

    #[test]
    fn a_root_inside_another_root_is_refused_in_both_orders() {
        // The double-counting bug this rule exists for: trash inside the
        // recordings directory would be walked twice and the total would be
        // wrong in the direction that makes a cleanup delete more than it needs.
        let outer = absolute("Recordings");
        let inner = absolute("Recordings/Trash");

        let error = StorageRoots::new()
            .with(StorageCategory::Recordings, &outer)
            .expect("an absolute path")
            .with(StorageCategory::Trash, &inner)
            .expect_err("trash inside recordings would be counted twice");
        assert!(matches!(error, RootsError::Overlapping { .. }), "{error}");

        let error = StorageRoots::new()
            .with(StorageCategory::Trash, &inner)
            .expect("an absolute path")
            .with(StorageCategory::Recordings, &outer)
            .expect_err("the same overlap, declared the other way round");
        assert!(matches!(error, RootsError::Overlapping { .. }), "{error}");
    }

    #[test]
    fn the_same_directory_twice_is_refused() {
        let error = StorageRoots::new()
            .with(StorageCategory::Recordings, absolute("Recordings"))
            .expect("an absolute path")
            .with(StorageCategory::Clips, absolute("Recordings"))
            .expect_err("one directory cannot be two roots");

        assert!(matches!(error, RootsError::Overlapping { .. }), "{error}");
    }

    #[test]
    fn a_sibling_whose_name_merely_starts_the_same_is_accepted() {
        // `C:\Videos2` is not inside `C:\Videos`, which a string prefix check
        // would have got wrong.
        let roots = StorageRoots::new()
            .with(StorageCategory::Recordings, absolute("Videos"))
            .expect("an absolute path")
            .with(StorageCategory::Clips, absolute("Videos2"))
            .expect("a sibling directory, not a child");

        assert_eq!(roots.len(), 2);
    }

    #[cfg(windows)]
    #[test]
    fn a_root_differing_only_in_case_is_refused_on_windows() {
        let error = StorageRoots::new()
            .with(StorageCategory::Recordings, r"D:\Clipped\Recordings")
            .expect("an absolute path")
            .with(StorageCategory::Clips, r"d:\clipped\RECORDINGS")
            .expect_err("Windows filenames are case-insensitive: this is one directory");

        assert!(matches!(error, RootsError::Overlapping { .. }), "{error}");
    }

    #[test]
    fn an_empty_set_is_empty() {
        assert!(StorageRoots::new().is_empty());
        assert_eq!(StorageRoots::new().len(), 0);
    }
}
