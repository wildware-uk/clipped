//! Reading the registry, for the launchers that record themselves in it.
//!
//! Steam keeps one value naming its own directory; Ubisoft Connect keeps a
//! subkey per installed game. Both need the same two-call sizing dance around
//! `RegGetValueW` and the same NUL handling, so it lives here once rather than
//! twice (AGENTS.md section 55).
//!
//! Every function reports a missing key or value as `Ok(None)` and a registry
//! that *refuses* as `Err`. That distinction is the whole point: "this launcher
//! is not installed" is an ordinary answer on most machines, and "the registry
//! would not answer" is a fault somebody should see.
//!
//! The error type is [`WIN32_ERROR`] rather than any provider's error, because
//! what a failed read should be called depends on which launcher was being
//! looked for. Each caller maps it to its own.

use std::os::windows::ffi::OsStrExt;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, WIN32_ERROR,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
    RRF_RT_REG_SZ,
};

/// The longest a registry key name may be, per `RegEnumKeyExW`, plus its
/// terminator.
const MAX_KEY_NAME: usize = 256;

/// A hive's name, for an error message a person can act on.
pub(super) fn hive(key: HKEY) -> &'static str {
    if key == HKEY_CURRENT_USER {
        "HKEY_CURRENT_USER"
    } else {
        "HKEY_LOCAL_MACHINE"
    }
}

/// Reads one `REG_SZ`, or `None` if the key or the value is not there.
pub(super) fn read_string(
    key: HKEY,
    subkey: &str,
    value: &str,
) -> Result<Option<String>, WIN32_ERROR> {
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

/// The names of a key's immediate subkeys, or `None` if the key is not there.
///
/// Ubisoft Connect records one subkey per installed game, named by the
/// application identifier, so enumerating them *is* the list of installed
/// games.
///
/// A name that cannot be read stops the enumeration rather than being skipped:
/// a partial list of installed games looks exactly like a shorter one, and
/// silently detecting fewer games than are installed is worse than saying the
/// registry refused.
pub(super) fn subkeys(key: HKEY, subkey: &str) -> Result<Option<Vec<String>>, WIN32_ERROR> {
    let wide_subkey = wide(subkey);
    let mut opened = HKEY::default();

    // SAFETY: the name is a NUL-terminated wide string that outlives the call,
    // and `opened` is a live local. The key is closed below on every path.
    let status = unsafe {
        RegOpenKeyExW(
            key,
            PCWSTR(wide_subkey.as_ptr()),
            None,
            KEY_READ,
            &raw mut opened,
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if status != ERROR_SUCCESS {
        return Err(status);
    }

    let mut names = Vec::new();
    let mut index = 0_u32;
    let outcome = loop {
        let mut buffer = [0_u16; MAX_KEY_NAME];
        let mut length = u32::try_from(buffer.len()).unwrap_or(u32::MAX);

        // SAFETY: the key is open, the buffer and its length are live locals,
        // and `length` is in characters as this entry point documents. Every
        // optional out-parameter this call does not need is `None`.
        let status = unsafe {
            RegEnumKeyExW(
                opened,
                index,
                Some(PWSTR(buffer.as_mut_ptr())),
                &raw mut length,
                None,
                None,
                None,
                None,
            )
        };
        if status == ERROR_NO_MORE_ITEMS {
            break Ok(Some(names));
        }
        if status != ERROR_SUCCESS {
            break Err(status);
        }
        let characters = (length as usize).min(buffer.len());
        names.push(String::from_utf16_lossy(&buffer[..characters]));
        index += 1;
    };

    // SAFETY: `opened` came from a successful `RegOpenKeyExW` and is not used
    // again.
    let _ = unsafe { RegCloseKey(opened) };
    outcome
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
    use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;

    #[test]
    fn a_value_that_is_not_there_is_not_an_error() {
        // The "this launcher is not installed" path, taken against a key that
        // certainly does not exist rather than by uninstalling anything.
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
        // long enough that the two-call sizing has to be right. Reading a
        // launcher's own value here would make the test depend on that launcher
        // being installed (AGENTS.md section 25).
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

    #[test]
    fn a_key_that_is_not_there_has_no_subkeys_and_is_not_an_error() {
        let answer = subkeys(HKEY_CURRENT_USER, r"Software\Clipped\NoSuchKeyForATest")
            .expect("a missing key is not a failure");
        assert_eq!(answer, None);
    }

    #[test]
    fn the_subkeys_of_a_real_key_come_back_named() {
        // Every Windows installation has this key and it always has subkeys, so
        // this exercises the enumeration loop without depending on any
        // launcher. `Microsoft` is present on all of them.
        let answer = subkeys(HKEY_LOCAL_MACHINE, r"SOFTWARE")
            .expect("the software key is readable")
            .expect("the software key exists");

        assert!(
            answer.len() > 1,
            "SOFTWARE has many subkeys; got {answer:?}"
        );
        assert!(
            answer.iter().any(|name| name == "Microsoft"),
            "every Windows machine has SOFTWARE\\Microsoft; got {answer:?}"
        );
        assert!(
            answer.iter().all(|name| !name.contains('\0')),
            "a terminator was left in a key name: {answer:?}"
        );
    }
}
