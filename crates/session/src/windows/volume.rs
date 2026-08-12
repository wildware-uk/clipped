//! How much room is left on the volume a recording is being written to.
//!
//! One call, `GetDiskFreeSpaceExW`, behind the same `#[cfg(windows)]` boundary
//! the Direct3D call beside it sits behind (AGENTS.md section 5). Everything
//! that *decides* anything from the answer is platform-neutral and lives in
//! [`crate::disk`].
//!
//! # Why this is not `clipped_library::accounting::capacity_of`
//!
//! It is the same call, and that is not an accident to be tidied away silently.
//! `clipped-library` asks it for storage accounting — how much of a drive the
//! library occupies, and whether a configured limit is breached
//! (`docs/storage-management.md`) — and the crate that owns that question also
//! owns the SQLite library index. `clipped-session` is the recording engine and
//! deliberately links no database: the recorder is the process that must keep
//! running while everything else fails (ADR 0002), and giving it a dependency
//! on the media library to ask the operating system how full a disk is would
//! invert the relationship between the two.
//!
//! The right home for a Windows API call both of them need is
//! `clipped-windows`, the platform layer at the bottom of the stack, and moving
//! it there is
//! [issue #277](https://github.com/wildware-uk/clipped/issues/277). Until then
//! this is twenty lines of foreign function interface stated in one place per
//! crate rather than a second implementation of any *policy* — the policy is in
//! [`crate::disk`] and there is one of it.

use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

use crate::disk::{VolumeSpace, VolumeUnreadable};

/// Reads the size and free space of the volume holding `directory`.
///
/// `directory` must exist and must be a directory; `GetDiskFreeSpaceExW` is
/// documented to take a directory name. Walking up to one that exists is
/// [`crate::disk::free_space`]'s job, so that the rule lives in one place.
pub(crate) fn free_space(directory: &Path) -> Result<VolumeSpace, VolumeUnreadable> {
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

    result.map_err(|error| VolumeUnreadable {
        path: directory.to_path_buf(),
        reason: error.message(),
    })?;

    Ok(VolumeSpace::new(total, free_to_caller))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_directory_is_answered_with_the_volume_that_holds_it() {
        let space = free_space(&std::env::temp_dir()).expect("the temporary directory exists");

        assert!(space.total_bytes() > 0, "no volume is of size zero");
        assert!(space.free_bytes() <= space.total_bytes());
    }

    #[test]
    fn a_directory_that_is_not_there_is_refused_with_what_windows_said() {
        let missing = std::env::temp_dir().join("clipped-session-volume-no-such-directory-103");
        assert!(
            !missing.exists(),
            "this test needs a path that is not there"
        );

        let error = free_space(&missing).expect_err("the directory does not exist");

        assert_eq!(error.path, missing);
        assert!(!error.reason.is_empty(), "Windows always says something");
    }
}
