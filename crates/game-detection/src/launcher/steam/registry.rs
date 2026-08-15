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
//! Reading a value is [`crate::launcher::registry`], which Ubisoft Connect's
//! provider shares.
//!
//! # Guessing is deliberately not a fallback
//!
//! No `C:\Program Files (x86)\Steam` if the registry says nothing. A machine
//! with no registry entry has no Steam on it, and a hard-coded path would find
//! the leftovers of an uninstall or, worse, quietly fail to find an
//! installation somewhere else and report *that* as "not installed" rather than
//! as the fault it is.

use std::io;
use std::path::PathBuf;

use windows::Win32::System::Registry::{HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

use crate::launcher::registry::{hive, read_string};

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
