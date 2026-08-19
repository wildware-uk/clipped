//! How much room is left on the volume holding a path.
//!
//! One call, `GetDiskFreeSpaceExW`, and the rule about which path to make it
//! with. Everything that *decides* anything from the answer is somewhere else:
//! `clipped_session::disk` judges whether a recording can still be finished
//! properly, and `clipped_library::accounting` judges the storage limits
//! SPEC.md section 27 configures. This module tells them both the same two
//! numbers and holds no opinion about either question.
//!
//! # Why it is here rather than in one of them
//!
//! It was in both, twenty lines each, until
//! [issue #277](https://github.com/wildware-uk/clipped/issues/277). The obvious
//! alternative — have the recording engine call
//! `clipped_library::accounting::capacity_of` — is legal by the layer table
//! (library is layer 1, session is layer 4) and is the wrong shape: it would
//! give the recorder a dependency on the media library, and through it on
//! SQLite, to ask the operating system how full a disk is. The recorder is the
//! process that must keep running while everything else fails (ADR 0002).
//!
//! This crate is layer 0, both of them may name it, and AGENTS.md section 5
//! puts platform queries here. `tests/integration/tests/disk_space_reads.rs`
//! is what keeps the call from being written a third time.
//!
//! # What the two callers share, and what they do not
//!
//! They share all of the mechanism and none of the policy. Both want the size
//! of the volume and what is free *to this user*; both want a path that does
//! not exist yet to be answered by the drive above it, because a recordings
//! directory nobody has recorded into is an ordinary first-run state; and both
//! treat a path that resolves to nothing as a drive that is not there rather
//! than as a fault. Where they differ is entirely in what they do with the
//! answer, and in the error vocabulary they say it in — so this module returns
//! its own [`VolumeUnavailable`], which each of them translates into theirs.

use core::fmt;
use std::error::Error;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

/// How large a volume is, and how much of it is free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeSpace {
    total_bytes: u64,
    free_bytes: u64,
}

impl VolumeSpace {
    /// A volume of `total_bytes` with `free_bytes` free.
    ///
    /// Free space above the total is clamped to the total rather than refused.
    /// It should not happen, and on a volume with a per-user quota it can,
    /// because Windows reports what is available *to the calling account*,
    /// which a quota can make larger than what is free on the volume itself.
    /// Refusing to answer at all would take a working free-space check away
    /// over an arithmetic curiosity, and a caller that subtracts the two would
    /// otherwise underflow.
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

    /// How many bytes are free, as available to the account Clipped runs as.
    #[must_use]
    pub const fn free_bytes(&self) -> u64 {
        self.free_bytes
    }
}

/// The volume holding a path could not be asked how much room it has.
///
/// Mid-recording this is what an unplugged drive looks like: the path stops
/// resolving to anything, and every ancestor of it fails too.
///
/// This is the platform layer's vocabulary, and it is not the one anybody
/// reads. Both callers translate it the moment they receive it —
/// `clipped_session::disk::VolumeUnreadable`, whose message redacts the path
/// for the logs, and `clipped_library::accounting::VolumeError::Unreadable`,
/// which has a second variant for a build that cannot ask at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeUnavailable {
    /// The path that was asked about, as the caller gave it.
    pub path: PathBuf,
    /// What Windows said, as it said it.
    pub reason: String,
}

impl fmt::Display for VolumeUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "the volume holding '{}' could not be read: {}",
            self.path.display(),
            self.reason
        )
    }
}

impl Error for VolumeUnavailable {}

/// Reads the size and free space of the volume holding `path`.
///
/// `path` does not have to exist. The nearest ancestor of it that does is asked
/// instead, because the drive is what the question is really about — and the
/// failure reported if none of them can be asked is the one from `path` itself,
/// which is what the caller asked about.
///
/// # Errors
///
/// [`VolumeUnavailable`] when nothing along the path could be read, which is
/// what a disconnected drive looks like.
pub fn volume_free_space(path: &Path) -> Result<VolumeSpace, VolumeUnavailable> {
    // `Path::ancestors` yields the path itself first, so this is both the first
    // candidate and — if every ancestor fails as well — the failure to report.
    let asked = measure(path);
    if asked.is_ok() {
        return asked;
    }

    for ancestor in path.ancestors().skip(1) {
        if let Ok(space) = measure(ancestor) {
            return Ok(space);
        }
    }

    asked
}

/// Asks Windows about one directory, which has to exist.
///
/// `GetDiskFreeSpaceExW` is documented to take a directory name. Walking up to
/// one that exists is [`volume_free_space`]'s job, so that the rule lives in
/// one place rather than in every caller that has a path a user typed.
fn measure(directory: &Path) -> Result<VolumeSpace, VolumeUnavailable> {
    // Wide and null-terminated, which `PCWSTR` requires and `OsStr` does not
    // provide: a Rust string is not terminated, and passing one unterminated
    // would have the API read past the end of the buffer.
    let wide: Vec<u16> = directory
        .as_os_str()
        .encode_wide()
        .chain(core::iter::once(0))
        .collect();

    let mut free_to_caller = 0u64;
    let mut total = 0u64;

    // SAFETY: `wide` is a null-terminated UTF-16 buffer that outlives the call,
    // and both output pointers address `u64` locals that outlive it too. The
    // call writes to them and retains nothing.
    let result = unsafe {
        GetDiskFreeSpaceExW(
            PCWSTR(wide.as_ptr()),
            Some(&mut free_to_caller),
            Some(&mut total),
            None,
        )
    };

    result.map_err(|error| VolumeUnavailable {
        path: directory.to_path_buf(),
        reason: error.message(),
    })?;

    Ok(VolumeSpace::new(total, free_to_caller))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_space_above_the_total_is_clamped_rather_than_reported() {
        let space = VolumeSpace::new(100, 200);

        assert_eq!(space.free_bytes(), 100);
        assert_eq!(space.total_bytes(), 100);
    }

    #[test]
    fn a_real_directory_is_answered_with_the_volume_that_holds_it() {
        // Not an assertion about this machine's disk, which no test may depend
        // on (AGENTS.md section 25). It asserts the three properties the call
        // has to have to be usable at all: it answers, the volume is not of
        // size zero, and free space does not exceed it.
        let space =
            volume_free_space(&std::env::temp_dir()).expect("the temporary directory exists");

        assert!(space.total_bytes() > 0, "no volume is of size zero");
        assert!(space.free_bytes() <= space.total_bytes());
    }

    #[test]
    fn a_directory_that_does_not_exist_yet_is_answered_by_the_drive_above_it() {
        // The first-run state, and the reason the walk exists: the recording
        // directory has not been created. Refusing to answer would make the
        // pre-flight check useless on exactly the run where a full disk is most
        // surprising.
        let missing = std::env::temp_dir()
            .join("clipped-windows-volume-277")
            .join("not-created-yet");
        assert!(
            !missing.exists(),
            "this test needs a path that is not there"
        );

        let space = volume_free_space(&missing).expect("the drive above it exists");

        assert!(space.total_bytes() > 0);
    }

    #[test]
    fn a_drive_that_is_not_there_is_refused_with_the_path_that_was_asked_about() {
        // The disconnected-recording-drive case: no ancestor resolves, so there
        // is nothing to fall back to. A drive letter no volume is mounted on is
        // found rather than assumed, because a machine may well have a Z: drive.
        let Some(letter) = (b'D'..=b'Z')
            .rev()
            .map(|letter| format!(r"{}:\", letter as char))
            .find(|root| !Path::new(root).exists())
        else {
            // Every drive letter is in use, which is a machine this test cannot
            // say anything on.
            return;
        };
        let asked = Path::new(&letter).join("Clipped");

        let error = volume_free_space(&asked).expect_err("no volume is mounted there");

        assert_eq!(
            error.path, asked,
            "the caller is told about the path it named, not an ancestor it did not"
        );
        assert!(!error.reason.is_empty(), "Windows always says something");
    }
}
