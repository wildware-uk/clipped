//! The one Windows call storage accounting needs.
//!
//! Everything else in `crate::accounting` is standard-library filesystem work
//! and arithmetic, and compiles and runs its tests on any platform. Free disk
//! space is the exception: there is no portable way to ask, so the question is
//! answered here, behind `#[cfg(windows)]`, where a port has a marked surface to
//! reimplement rather than a search problem (AGENTS.md section 5).

use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

use crate::accounting::error::VolumeError;
use crate::accounting::volume::VolumeCapacity;

/// Reads the size and free space of the volume holding `directory`.
///
/// `directory` must exist and must be a directory; `GetDiskFreeSpaceExW` is
/// documented to take a directory name. Walking up to one that exists is
/// [`crate::accounting::capacity_of`]'s job, so that the rule lives in one place
/// rather than in every platform implementation of it.
pub(crate) fn capacity_of(directory: &Path) -> Result<VolumeCapacity, VolumeError> {
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

    result.map_err(|error| VolumeError::Unreadable {
        path: directory.to_path_buf(),
        reason: error.message(),
    })?;

    Ok(VolumeCapacity::new(total, free_to_caller))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_directory_is_answered_with_a_volume_that_holds_it() {
        let capacity = capacity_of(&std::env::temp_dir()).expect("the temporary directory exists");

        assert!(capacity.total_bytes() > 0, "no volume is of size zero");
        assert!(capacity.free_bytes() <= capacity.total_bytes());
    }

    #[test]
    fn a_directory_that_is_not_there_is_refused_with_what_windows_said() {
        let missing = std::env::temp_dir().join("clipped-accounting-windows-no-such-directory-93");
        assert!(
            !missing.exists(),
            "this test needs a path that is not there"
        );

        let error = capacity_of(&missing).expect_err("the directory does not exist");

        match error {
            VolumeError::Unreadable { path, reason } => {
                assert_eq!(path, missing);
                assert!(!reason.is_empty(), "Windows always says something");
            }
            VolumeError::Unsupported => panic!("this is a Windows build"),
        }
    }
}
