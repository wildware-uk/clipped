//! The `start-at-login` subcommand: asking Windows to run the recorder when
//! this user signs in.
//!
//! SPEC.md section 5 and [ADR 0002](../../../docs/adr/0002-separate-recorder-process.md)
//! both describe a recorder that "starts at login and stays up for days".
//! [ADR 0006](../../../docs/adr/0006-recorder-lifetime-and-supervision.md)
//! chose the mechanism; this module is it.
//!
//! # The one rule
//!
//! **Nothing here runs unless somebody asked for it.** There is no automatic
//! enabling, no "on by default", and no code path from starting a recording or
//! opening a window to writing a registry value. `docs/privacy.md`'s position is
//! nothing surprising and nothing hidden, and a program that quietly arranges to
//! start itself for ever is the definition of both. The only way this key is
//! written is somebody running `clipped-recorder start-at-login enable`, and the
//! only way it stays written is nobody running `disable`.
//!
//! # Where it writes, and why there
//!
//! ```text
//! HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run
//!     "Clipped Recorder" = "C:\…\clipped-recorder.exe" serve
//! ```
//!
//! `HKEY_CURRENT_USER` rather than `HKEY_LOCAL_MACHINE`: this is one person's
//! choice on a machine they may share, it needs no elevation, and the recorder
//! runs in that person's own sign-in session — which is the session whose
//! windows it has to capture and whose audio endpoints it has to open
//! ([ADR 0002](../../../docs/adr/0002-separate-recorder-process.md) rejected a
//! Windows service for precisely that reason).
//!
//! A `Run` value rather than a shortcut in the Startup folder, although both
//! work and Windows treats them almost identically. The deciding difference is
//! that a `Run` value appears in **Settings → Apps → Startup** and in Task
//! Manager's Startup tab with a switch beside it, so the user can turn it off
//! without Clipped's help and without finding this subcommand. A `.lnk` also
//! needs COM and `IShellLink` to write, which is a great deal of machinery for
//! the worse of the two outcomes.
//!
//! # What it does not do
//!
//! It does not keep the value in step with a moved or updated installation. The
//! value holds the full path of the executable that wrote it, so an installation
//! moved to another directory leaves a value pointing at nothing — `status`
//! reports exactly that, and `enable` run again fixes it. Doing better means
//! knowing when the application was updated, which is the installer's business
//! and not this subcommand's.

use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::cli::{StartAtLoginAction, StartAtLoginArgs};

/// The registry key Windows reads at sign-in.
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// The value name Clipped's entry is written under.
///
/// It is what appears in Settings → Apps → Startup, so it is written the way a
/// person would read it rather than as an executable name.
const VALUE_NAME: &str = "Clipped Recorder";

/// Why `start-at-login` failed.
#[derive(Debug)]
pub enum StartAtLoginError {
    /// This executable's own path could not be determined, so there is nothing
    /// to write.
    UnknownExecutable(std::io::Error),
    /// The registry refused.
    Registry {
        /// What was being done, as a phrase that fits after "while".
        doing: String,
        /// What Windows said.
        source: std::io::Error,
    },
    /// This build has no registry, because it is not a Windows build.
    Unsupported,
}

impl fmt::Display for StartAtLoginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownExecutable(error) => write!(
                formatter,
                "the recorder could not find its own location, so there is no command to run at \
                 login: {error}"
            ),
            Self::Registry { doing, source } => {
                write!(formatter, "the registry refused while {doing}: {source}")
            }
            Self::Unsupported => formatter.write_str(
                "starting at login is a Windows arrangement, and this is not a Windows build",
            ),
        }
    }
}

impl Error for StartAtLoginError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::UnknownExecutable(error) | Self::Registry { source: error, .. } => Some(error),
            Self::Unsupported => None,
        }
    }
}

/// Reads, writes or removes one value under `HKEY_CURRENT_USER`.
///
/// The key and the value name are fields rather than constants so that the
/// tests can exercise the real registry calls against a scratch key of their
/// own. A test that wrote the real `Run` value would arrange for the machine it
/// ran on to start a recorder at every sign-in afterwards, which is not a thing
/// a test may do (AGENTS.md section 25).
#[derive(Debug, Clone)]
pub struct LoginEntry {
    subkey: String,
    value_name: String,
}

impl LoginEntry {
    /// The entry Clipped uses.
    #[must_use]
    pub fn for_the_recorder() -> Self {
        Self {
            subkey: RUN_KEY.to_owned(),
            value_name: VALUE_NAME.to_owned(),
        }
    }

    /// An entry somewhere else, for a test.
    #[must_use]
    pub fn at(subkey: &str, value_name: &str) -> Self {
        Self {
            subkey: subkey.to_owned(),
            value_name: value_name.to_owned(),
        }
    }

    /// Where this entry is, as a person would write it.
    #[must_use]
    pub fn location(&self) -> String {
        format!(r"HKEY_CURRENT_USER\{}\{}", self.subkey, self.value_name)
    }
}

/// The command line the recorder is started with at login.
///
/// Quoted because an installation path contains spaces far more often than not,
/// and Windows parses this value as a command line.
#[must_use]
pub fn login_command(executable: &Path) -> String {
    format!("\"{}\" serve", executable.display())
}

/// Runs `clipped-recorder start-at-login`.
///
/// # Errors
///
/// [`StartAtLoginError`] if the registry refused, or if this executable's own
/// path could not be read.
pub fn run(args: &StartAtLoginArgs) -> Result<(), StartAtLoginError> {
    let entry = LoginEntry::for_the_recorder();

    match args.action {
        StartAtLoginAction::Enable => {
            let executable = this_executable()?;
            let command = login_command(&executable);
            platform::write(&entry, &command)?;
            println!("Clipped will start at login for this account.");
            println!("  {} = {command}", entry.location());
            println!("Run `clipped-recorder start-at-login disable` to undo it, or turn it off in");
            println!("Settings > Apps > Startup.");
        }
        StartAtLoginAction::Disable => {
            if platform::remove(&entry)? {
                println!("Clipped will no longer start at login for this account.");
                println!("  removed {}", entry.location());
            } else {
                println!("Clipped was not set to start at login for this account.");
            }
        }
        StartAtLoginAction::Status => report(&entry)?,
    }

    Ok(())
}

/// Prints what is configured, and whether it still points at anything.
fn report(entry: &LoginEntry) -> Result<(), StartAtLoginError> {
    let Some(command) = platform::read(entry)? else {
        println!("Clipped is not set to start at login for this account.");
        println!("  {} is not set", entry.location());
        return Ok(());
    };

    println!("Clipped is set to start at login for this account.");
    println!("  {} = {command}", entry.location());

    // Reported rather than repaired. An installation that moved leaves a value
    // pointing at nothing, and silently rewriting somebody's startup entry
    // because a status command was run is exactly the surprising behaviour this
    // subcommand exists to avoid.
    match executable_in(&command) {
        Some(path) if !path.is_file() => {
            println!();
            println!("That path does not exist, so nothing will start. Run");
            println!("`clipped-recorder start-at-login enable` from the installation you want.");
        }
        Some(_) | None => {}
    }

    Ok(())
}

/// The executable a login command names, if it is quoted the way
/// [`login_command`] writes it.
fn executable_in(command: &str) -> Option<PathBuf> {
    let rest = command.strip_prefix('"')?;
    let (path, _) = rest.split_once('"')?;
    Some(PathBuf::from(path))
}

/// The full path of this running executable.
fn this_executable() -> Result<PathBuf, StartAtLoginError> {
    std::env::current_exe().map_err(StartAtLoginError::UnknownExecutable)
}

#[cfg(windows)]
mod platform {
    //! Every registry call this crate makes.

    use std::io;

    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegQueryValueExW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
    };

    use super::{LoginEntry, StartAtLoginError};

    /// An open registry key, closed exactly once when dropped.
    struct OpenKey(HKEY);

    impl Drop for OpenKey {
        fn drop(&mut self) {
            // SAFETY: the key came from `RegCreateKeyExW`, is owned solely by
            // this value, and is closed exactly once.
            unsafe {
                RegCloseKey(self.0);
            }
        }
    }

    /// Opens the entry's key, creating it if the `Run` key somehow does not
    /// exist.
    ///
    /// `RegCreateKeyExW` rather than `RegOpenKeyExW` because the same call is
    /// wanted for a test's scratch key, which will not exist the first time, and
    /// because it opens an existing key unchanged when there is one.
    fn open(entry: &LoginEntry, access: u32) -> Result<OpenKey, StartAtLoginError> {
        let subkey = wide(&entry.subkey);
        let mut key: HKEY = std::ptr::null_mut();

        // SAFETY: `subkey` is a NUL-terminated wide string that outlives the
        // call; the two null pointers are the documented "no class" and "no
        // security attributes"; `key` and the disposition out-parameter point at
        // live locals. The key written into `key` is owned by the `OpenKey`
        // returned.
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                0,
                std::ptr::null_mut(),
                REG_OPTION_NON_VOLATILE,
                access,
                std::ptr::null(),
                &raw mut key,
                std::ptr::null_mut(),
            )
        };

        if status != ERROR_SUCCESS {
            return Err(registry_error(
                format!("opening HKEY_CURRENT_USER\\{}", entry.subkey),
                status,
            ));
        }

        Ok(OpenKey(key))
    }

    /// Sets the entry's value.
    pub(super) fn write(entry: &LoginEntry, command: &str) -> Result<(), StartAtLoginError> {
        let key = open(entry, KEY_SET_VALUE)?;
        let name = wide(&entry.value_name);
        let value = wide(command);
        let bytes = std::mem::size_of_val(value.as_slice());

        // SAFETY: `name` and `value` are NUL-terminated wide strings that
        // outlive the call, and `bytes` is exactly the length of `value`
        // including its terminator, which is what `RegSetValueExW` documents
        // REG_SZ wants.
        let status = unsafe {
            RegSetValueExW(
                key.0,
                name.as_ptr(),
                0,
                REG_SZ,
                value.as_ptr().cast::<u8>(),
                u32::try_from(bytes).unwrap_or(u32::MAX),
            )
        };

        if status != ERROR_SUCCESS {
            return Err(registry_error(
                format!("writing {}", entry.location()),
                status,
            ));
        }

        Ok(())
    }

    /// Reads the entry's value, or [`None`] if it is not set.
    pub(super) fn read(entry: &LoginEntry) -> Result<Option<String>, StartAtLoginError> {
        let key = open(entry, KEY_QUERY_VALUE)?;
        let name = wide(&entry.value_name);
        let mut bytes: u32 = 0;

        // SAFETY: asking for the size with no buffer is the documented first
        // half of this call. `name` outlives it and the out-parameter points at
        // a live local.
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                name.as_ptr(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &raw mut bytes,
            )
        };

        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if status != ERROR_SUCCESS {
            return Err(registry_error(
                format!("reading {}", entry.location()),
                status,
            ));
        }

        let mut buffer = vec![0_u16; (bytes as usize).div_ceil(2)];
        let mut length =
            u32::try_from(std::mem::size_of_val(buffer.as_slice())).unwrap_or(u32::MAX);

        // SAFETY: the buffer is at least the length Windows just asked for and
        // is alive for the call; `length` is written again and read below.
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                name.as_ptr(),
                std::ptr::null(),
                std::ptr::null_mut(),
                buffer.as_mut_ptr().cast::<u8>(),
                &raw mut length,
            )
        };

        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if status != ERROR_SUCCESS {
            return Err(registry_error(
                format!("reading {}", entry.location()),
                status,
            ));
        }

        let characters = (length as usize) / 2;
        let text = &buffer[..characters.min(buffer.len())];
        // A REG_SZ is not required to be NUL-terminated in the registry, and the
        // length includes the terminator when it is, so it is trimmed rather
        // than assumed either way.
        let text = text.strip_suffix(&[0]).unwrap_or(text);

        Ok(Some(String::from_utf16_lossy(text)))
    }

    /// Removes the entry's value, reporting whether there was one.
    pub(super) fn remove(entry: &LoginEntry) -> Result<bool, StartAtLoginError> {
        let key = open(entry, KEY_SET_VALUE)?;
        let name = wide(&entry.value_name);

        // SAFETY: `name` is a NUL-terminated wide string that outlives the call.
        let status = unsafe { RegDeleteValueW(key.0, name.as_ptr()) };

        match status {
            ERROR_SUCCESS => Ok(true),
            ERROR_FILE_NOT_FOUND => Ok(false),
            other => Err(registry_error(
                format!("removing {}", entry.location()),
                other,
            )),
        }
    }

    /// Removes the entry's whole subkey, which must have no subkeys of its own.
    ///
    /// Only ever used by this module's tests, to leave the registry as they
    /// found it. The recorder never removes a key it did not create, and the
    /// `Run` key is emphatically not one it created.
    #[cfg(test)]
    pub(super) fn delete_subkey(subkey: &str) -> Result<(), StartAtLoginError> {
        use windows_sys::Win32::System::Registry::RegDeleteKeyW;

        let wide_subkey = wide(subkey);
        // SAFETY: `subkey` is a NUL-terminated wide string that outlives the
        // call.
        let status = unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, wide_subkey.as_ptr()) };

        match status {
            ERROR_SUCCESS | ERROR_FILE_NOT_FOUND => Ok(()),
            other => Err(registry_error(
                format!(r"removing HKEY_CURRENT_USER\{subkey}"),
                other,
            )),
        }
    }

    /// A Win32 status as this module's failure.
    fn registry_error(doing: String, status: WIN32_ERROR) -> StartAtLoginError {
        StartAtLoginError::Registry {
            doing,
            source: io::Error::from_raw_os_error(status as i32),
        }
    }

    /// A NUL-terminated UTF-16 copy of `text`.
    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }
}

#[cfg(not(windows))]
mod platform {
    //! There is no registry off Windows, and nothing to pretend about.

    use super::{LoginEntry, StartAtLoginError};

    pub(super) fn write(_entry: &LoginEntry, _command: &str) -> Result<(), StartAtLoginError> {
        Err(StartAtLoginError::Unsupported)
    }

    pub(super) fn read(_entry: &LoginEntry) -> Result<Option<String>, StartAtLoginError> {
        Err(StartAtLoginError::Unsupported)
    }

    pub(super) fn remove(_entry: &LoginEntry) -> Result<bool, StartAtLoginError> {
        Err(StartAtLoginError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch key under this account that no real software reads, removed
    /// when the test that made it finishes.
    ///
    /// Deliberately **not** the `Run` key: a test that wrote there would arrange
    /// for the machine running it to start a recorder at every sign-in
    /// afterwards (AGENTS.md section 25). What is exercised is the same code
    /// making the same registry calls against a different path, which is the
    /// whole of the difference.
    #[cfg(windows)]
    struct Scratch(LoginEntry);

    #[cfg(windows)]
    impl Scratch {
        fn new(label: &str) -> Self {
            Self(LoginEntry::at(
                &format!(r"Software\Clipped\tests\{}-{label}", std::process::id()),
                "start-at-login",
            ))
        }

        fn entry(&self) -> &LoginEntry {
            &self.0
        }
    }

    #[cfg(windows)]
    impl Drop for Scratch {
        /// A test that leaves keys behind litters somebody's registry every time
        /// the suite runs.
        fn drop(&mut self) {
            let _ = platform::remove(&self.0);
            // The parents are removed too, and each attempt fails harmlessly
            // while another test still holds a key under it: `RegDeleteKeyW`
            // refuses a key that has subkeys, so the last test out is the one
            // that takes the tree away.
            for subkey in [
                self.0.subkey.as_str(),
                r"Software\Clipped\tests",
                r"Software\Clipped",
            ] {
                let _ = platform::delete_subkey(subkey);
            }
        }
    }

    #[test]
    fn the_command_written_quotes_the_path_and_asks_for_serve() {
        // Quoted because an installation path contains spaces far more often
        // than not, and Windows parses this value as a command line: without the
        // quotes, `C:\Program Files\Clipped\clipped-recorder.exe` starts
        // `C:\Program.exe`.
        let command = login_command(Path::new(r"C:\Program Files\Clipped\clipped-recorder.exe"));
        assert_eq!(
            command,
            r#""C:\Program Files\Clipped\clipped-recorder.exe" serve"#
        );
        assert_eq!(
            executable_in(&command),
            Some(PathBuf::from(
                r"C:\Program Files\Clipped\clipped-recorder.exe"
            )),
            "the path has to be readable back out, or `status` cannot say whether it still exists"
        );
    }

    #[test]
    fn the_recorders_entry_is_the_run_key_windows_actually_reads() {
        let entry = LoginEntry::for_the_recorder();
        assert_eq!(
            entry.location(),
            r"HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run\Clipped Recorder",
            "this string is the whole of what `enable` changes on somebody's machine, so it is \
             pinned rather than left to be discovered in a registry editor"
        );
    }

    #[cfg(windows)]
    #[test]
    fn nothing_is_configured_until_something_writes_it() {
        // The property that matters most: the default state is off, and reading
        // it does not create it.
        let scratch = Scratch::new("absent");
        assert_eq!(
            platform::read(scratch.entry()).expect("the registry can be read"),
            None
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_written_entry_can_be_read_back_and_removed() {
        let scratch = Scratch::new("round-trip");
        let entry = scratch.entry();
        let command = login_command(Path::new(r"C:\Program Files\Clipped\clipped-recorder.exe"));

        platform::write(entry, &command).expect("the value can be written");
        assert_eq!(
            platform::read(entry).expect("the registry can be read"),
            Some(command),
            "what is read back has to be exactly what was written, or `status` lies"
        );

        assert!(
            platform::remove(entry).expect("the value can be removed"),
            "removing a value that was there should say so"
        );
        assert_eq!(
            platform::read(entry).expect("the registry can be read"),
            None,
            "reversible means reversible: nothing may be left behind"
        );
    }

    #[cfg(windows)]
    #[test]
    fn removing_an_entry_that_was_never_there_is_not_a_failure() {
        // `disable` run twice, or run by somebody who never enabled it. It is
        // the state they asked for either way, and an error would be wrong.
        let scratch = Scratch::new("never-there");
        assert!(!platform::remove(scratch.entry()).expect("the registry can be reached"));
    }
}
