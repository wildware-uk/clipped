//! How large the drive is and how much of it is free.
//!
//! Two of the three limits SPEC.md section 27 configures cannot be judged from
//! the library alone. A quota is a fact about Clipped's own files, but a minimum
//! free space is a fact about the *volume* — everything else on it counts,
//! including the files of every other application — and validating either
//! against the disk needs the disk's size.
//!
//! The measurement is a single call per volume, so it is cheap enough to take
//! whenever a status is evaluated, and it is the one part of accounting that
//! needs the platform ([`crate::accounting::windows`], AGENTS.md section 5).

use std::path::Path;

use crate::accounting::error::VolumeError;

/// How large a volume is, and how much of it is free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeCapacity {
    total_bytes: u64,
    free_bytes: u64,
}

impl VolumeCapacity {
    /// A volume of `total_bytes` with `free_bytes` free.
    ///
    /// Free space above the total is clamped to the total rather than refused.
    /// It should not happen, and on a volume with a per-user quota it can: the
    /// operating system reports what is available *to this user*, which a quota
    /// can make larger than the free space on the physical volume. Refusing to
    /// answer at all would take a working free-space limit away over an
    /// arithmetic curiosity.
    #[must_use]
    pub fn new(total_bytes: u64, free_bytes: u64) -> Self {
        Self {
            total_bytes,
            free_bytes: free_bytes.min(total_bytes),
        }
    }

    /// The size of the volume in bytes.
    #[must_use]
    pub const fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// How many bytes are free, as available to the user Clipped runs as.
    #[must_use]
    pub const fn free_bytes(&self) -> u64 {
        self.free_bytes
    }

    /// How many bytes are in use, by everything on the volume.
    #[must_use]
    pub const fn used_bytes(&self) -> u64 {
        self.total_bytes - self.free_bytes
    }
}

/// Reads the size and free space of the volume holding `path`.
///
/// `path` does not have to exist. A recording directory that has not been
/// created yet is an ordinary first-run state, so the nearest ancestor that does
/// exist is asked instead — the drive is what the question is really about.
///
/// # Errors
///
/// [`VolumeError::Unreadable`] if nothing along the path could be read, which is
/// what a disconnected drive looks like, and [`VolumeError::Unsupported`] on a
/// build that is not for Windows.
pub fn capacity_of(path: &Path) -> Result<VolumeCapacity, VolumeError> {
    #[cfg(windows)]
    {
        let mut first_failure = None;

        for candidate in path.ancestors() {
            match crate::accounting::windows::capacity_of(candidate) {
                Ok(capacity) => return Ok(capacity),
                Err(error) => {
                    first_failure.get_or_insert(error);
                }
            }
        }

        Err(first_failure.unwrap_or(VolumeError::Unreadable {
            path: path.to_path_buf(),
            reason: "the path names no directory at all".to_owned(),
        }))
    }

    #[cfg(not(windows))]
    {
        let _ = path;
        Err(VolumeError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn used_space_is_what_is_left_of_the_total() {
        let capacity = VolumeCapacity::new(500_000_000_000, 120_000_000_000);

        assert_eq!(capacity.total_bytes(), 500_000_000_000);
        assert_eq!(capacity.free_bytes(), 120_000_000_000);
        assert_eq!(capacity.used_bytes(), 380_000_000_000);
    }

    #[test]
    fn free_space_above_the_total_is_clamped_rather_than_producing_negative_use() {
        // A per-user quota can report more available than the volume holds.
        // `used_bytes` subtracts, so this is the difference between a clamp and
        // a panic in a debug build.
        let capacity = VolumeCapacity::new(100, 400);

        assert_eq!(capacity.free_bytes(), 100);
        assert_eq!(capacity.used_bytes(), 0);
    }

    #[cfg(windows)]
    #[test]
    fn the_volume_holding_the_temporary_directory_reports_a_plausible_size() {
        // Not an assertion about this machine's disk, which no test may depend
        // on (AGENTS.md section 25). It asserts the three properties the call
        // has to have to be usable at all: it answers, the volume is not of
        // size zero, and free space does not exceed it.
        let capacity = capacity_of(&std::env::temp_dir()).expect("the temporary directory exists");

        assert!(capacity.total_bytes() > 0);
        assert!(capacity.free_bytes() <= capacity.total_bytes());
    }

    #[cfg(windows)]
    #[test]
    fn a_directory_that_does_not_exist_yet_is_answered_by_its_drive() {
        // The first-run case: the recording directory has not been created.
        let missing = std::env::temp_dir().join("clipped-accounting-no-such-directory-93");
        assert!(
            !missing.exists(),
            "this test needs a path that is not there"
        );

        let capacity = capacity_of(&missing).expect("the drive above it exists");

        assert!(capacity.total_bytes() > 0);
    }

    #[cfg(windows)]
    #[test]
    fn a_drive_that_is_not_there_is_an_error_naming_the_path() {
        // The disconnected-recording-drive case. A drive letter no volume is
        // mounted on is found rather than assumed, because a machine may well
        // have a Z: drive.
        let Some(letter) = (b'D'..=b'Z')
            .rev()
            .map(|letter| format!(r"{}:\", letter as char))
            .find(|root| !Path::new(root).exists())
        else {
            // Every drive letter is in use, which is a machine this test cannot
            // say anything on.
            return;
        };

        let error = capacity_of(&Path::new(&letter).join("Clipped"))
            .expect_err("no volume is mounted there");

        match error {
            VolumeError::Unreadable { path, reason } => {
                assert!(path.starts_with(&letter), "{}", path.display());
                assert!(!reason.is_empty(), "the reason has to reach the user");
            }
            VolumeError::Unsupported => panic!("this is a Windows build"),
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn a_build_that_cannot_ask_says_so_rather_than_inventing_a_figure() {
        assert_eq!(capacity_of(Path::new("/")), Err(VolumeError::Unsupported));
    }
}
