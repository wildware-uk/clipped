//! Where Steam says it is, which is the registry and nowhere else.
//!
//! Two values are read, in this order:
//!
//! | Key | Value | Written by |
//! | --- | --- | --- |
//! | `HKEY_CURRENT_USER\Software\Valve\Steam` | `SteamPath` | the client, for this user |
//! | `HKEY_LOCAL_MACHINE\SOFTWARE\WOW6432Node\Valve\Steam` | `InstallPath` | the installer, for the machine |
//!
//! The per-user value first because it is the one the client keeps current, and
//! the machine-wide one after it because a user account that has never launched
//! Steam has no per-user key at all while the installation is plainly there.
//! Both were present on the machine this was developed against and both named
//! the same directory, spelled differently: `c:/program files (x86)/steam` in
//! the first and `C:\Program Files (x86)\Steam` in the second. Nothing here
//! cares, because a [`PathBuf`] does not.
//!
//! `WOW6432Node` is not a guess either. Steam is a 32-bit application, so its
//! installer writes under the redirected key, and Clipped is a 64-bit process
//! that therefore has to name the redirected key to see it.
//!
//! # Guessing is deliberately not a fallback
//!
//! No `C:\Program Files (x86)\Steam` if the registry says nothing. A machine
//! with no registry entry has no Steam on it, and a hard-coded path would find
//! the leftovers of an uninstall or, worse, quietly fail to find an
//! installation somewhere else and report *that* as "not installed" rather than
//! as the fault it is.

use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS, WIN32_ERROR,
};
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ,
};

use super::SteamError;

/// The places Steam records where it is, best first.
const LOCATIONS: [(HKEY, &str, &str); 2] = [
    (HKEY_CURRENT_USER, r"Software\Valve\Steam", "SteamPath"),
    (
        HKEY_LOCAL_MACHINE,
        r"SOFTWARE\WOW6432Node\Valve\Steam",
        "InstallPath",
    ),
];

/// Where Steam is installed, or `None` if it is not.
///
/// # Errors
///
/// [`SteamError::Registry`] if the registry refused for any reason other than
/// the key or value not being there. A value that is absent is the answer "no
/// Steam", and a registry that will not answer is not.
pub(super) fn steam_path() -> Result<Option<PathBuf>, SteamError> {
    for (key, subkey, value) in LOCATIONS {
        match read_string(key, subkey, value) {
            Ok(Some(path)) if !path.trim().is_empty() => return Ok(Some(PathBuf::from(path))),
            Ok(_) => {}
            Err(status) => {
                return Err(SteamError::Registry {
                    doing: format!("{}\\{subkey} {value}", hive(key)),
                    source: io::Error::from_raw_os_error(
                        i32::try_from(status.0).unwrap_or(i32::MAX),
                    ),
                })
            }
        }
    }
    Ok(None)
}

/// A hive's name, for an error message a person can act on.
fn hive(key: HKEY) -> &'static str {
    if key == HKEY_CURRENT_USER {
        "HKEY_CURRENT_USER"
    } else {
        "HKEY_LOCAL_MACHINE"
    }
}

/// Reads one `REG_SZ`, or `None` if the key or the value is not there.
fn read_string(key: HKEY, subkey: &str, value: &str) -> Result<Option<String>, WIN32_ERROR> {
    let subkey = wide(subkey);
    let value = wide(value);
    let mut bytes: u32 = 0;

    // SAFETY: both strings are NUL-terminated wide strings that outlive the
    // call. Passing no buffer with a live size out-parameter is the documented
    // way to ask `RegGetValueW` how large the value is.
    let status = unsafe {
        RegGetValueW(
            key,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&raw mut bytes),
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if status != ERROR_SUCCESS {
        return Err(status);
    }

    let mut buffer = vec![0_u16; (bytes as usize).div_ceil(2)];
    let mut length = u32::try_from(std::mem::size_of_val(buffer.as_slice())).unwrap_or(u32::MAX);

    // SAFETY: as above, and `buffer` is at least the size Windows just asked
    // for. `length` is read back below to find how much of it was written.
    let status = unsafe {
        RegGetValueW(
            key,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&raw mut length),
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if status == ERROR_MORE_DATA {
        // The value grew between the two calls. One retry is not worth the
        // machinery: the caller reads this once at start-up, and reporting it
        // is better than looping against something that is changing.
        return Err(status);
    }
    if status != ERROR_SUCCESS {
        return Err(status);
    }

    // `RegGetValueW` guarantees a terminator, unlike `RegQueryValueExW`, and
    // counts it in the length it reports.
    let characters = (length as usize) / 2;
    let text = &buffer[..characters.min(buffer.len())];
    let text = text.strip_suffix(&[0]).unwrap_or(text);
    Ok(Some(String::from_utf16_lossy(text)))
}

/// A NUL-terminated wide string, as every `…W` entry point wants.
fn wide(value: &str) -> Vec<u16> {
    std::ffi::OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_that_is_not_there_is_not_an_error() {
        // The "Steam is not installed" path, taken against a key that certainly
        // does not exist rather than by uninstalling Steam.
        let answer = read_string(
            HKEY_CURRENT_USER,
            r"Software\Clipped\NoSuchKeyForATest",
            "NoSuchValue",
        )
        .expect("a missing key is not a failure");
        assert_eq!(answer, None);
    }

    #[test]
    fn a_missing_value_in_a_key_that_exists_is_also_not_an_error() {
        let answer = read_string(HKEY_CURRENT_USER, r"Software", "NoSuchValueForATest")
            .expect("a missing value is not a failure");
        assert_eq!(answer, None);
    }

    #[test]
    fn a_real_string_value_comes_back_whole() {
        // `HKCU\Environment\TEMP` is set on every Windows user profile and is
        // long enough that the two-call sizing has to be right. Reading
        // Steam's own value here would make the test depend on Steam being
        // installed (AGENTS.md section 25).
        let answer = read_string(HKEY_CURRENT_USER, r"Environment", "TEMP")
            .expect("the environment key is readable");
        let Some(value) = answer else {
            // Not every automation account has one; nothing is claimed if so.
            return;
        };
        assert!(!value.is_empty());
        assert!(
            !value.contains('\0'),
            "the terminator should have been trimmed: {value:?}"
        );
    }
}
