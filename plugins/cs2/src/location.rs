//! Where this plugin left the configuration file.
//!
//! A plugin is started by the host with no arguments and told the session
//! identifier and the game's executable *file name* — not its path
//! (`clipped_plugins::ObservedProcess`). That is deliberate: a plugin gets what
//! it needs to find the game's own interface and nothing about the machine it
//! is running on. It also means a running plugin cannot work out where
//! Counter-Strike is installed, and it has to know, because the port and the
//! token it must use are in the file it wrote there.
//!
//! So `install` leaves one line of its own beside the plugin's executable
//! saying where it put the file, and the running plugin follows it. Nothing
//! else is remembered: the port and the token are read out of the configuration
//! file itself, so there is exactly one copy of each and no way for two records
//! to disagree about what the game was told (AGENTS.md section 30).
//!
//! It is beside the executable rather than in Clipped's settings for the reason
//! `docs/plugin-api.md` gives for the host: a plugin's own state is not the
//! application's configuration, and putting it there would be a second
//! configuration store. Beside the executable is the plugin's own directory,
//! which is the directory the user installed and the directory they delete.

use core::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The file that records where the configuration went.
pub const RECORD_FILE: &str = "installed-at.json";

/// Where the Game State Integration configuration file was written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallRecord {
    /// The full path of the `gamestate_integration_clipped.cfg` this plugin
    /// wrote.
    pub configuration: PathBuf,
}

impl InstallRecord {
    /// Reads the record from a plugin directory.
    ///
    /// `Ok(None)` means the integration has not been installed, which is the
    /// state of every machine until somebody installs it, and is not an error.
    ///
    /// # Errors
    ///
    /// [`RecordError`] when the file is there and cannot be used.
    pub fn read(plugin_directory: &Path) -> Result<Option<Self>, RecordError> {
        let path = plugin_directory.join(RECORD_FILE);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(RecordError::Read { path, source }),
        };
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|source| RecordError::Malformed { path, source })
    }

    /// Writes the record into a plugin directory.
    ///
    /// # Errors
    ///
    /// [`RecordError::Write`], which on a plugin installed somewhere the user
    /// cannot write is the thing they need to be told.
    pub fn write(&self, plugin_directory: &Path) -> Result<PathBuf, RecordError> {
        let path = plugin_directory.join(RECORD_FILE);
        let json = serde_json::to_string_pretty(self).map_err(|source| RecordError::Malformed {
            path: path.clone(),
            source,
        })?;
        fs::write(&path, format!("{json}\n")).map_err(|source| RecordError::Write {
            path: path.clone(),
            source,
        })?;
        Ok(path)
    }

    /// Removes the record, if there is one.
    ///
    /// # Errors
    ///
    /// [`RecordError::Write`] when it is there and will not go.
    pub fn remove(plugin_directory: &Path) -> Result<(), RecordError> {
        let path = plugin_directory.join(RECORD_FILE);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(RecordError::Write { path, source }),
        }
    }
}

/// The directory this executable is installed in.
///
/// # Errors
///
/// [`RecordError::NoPluginDirectory`] when the operating system will not say
/// where this program is, which is not something a caller can fix but is
/// something they have to be told rather than have guessed at.
pub fn plugin_directory() -> Result<PathBuf, RecordError> {
    std::env::current_exe()
        .ok()
        .and_then(|executable| executable.parent().map(Path::to_path_buf))
        .ok_or(RecordError::NoPluginDirectory)
}

/// What went wrong with the record.
#[derive(Debug)]
pub enum RecordError {
    /// This program cannot tell where it is installed.
    NoPluginDirectory,
    /// The record could not be read.
    Read {
        /// The file.
        path: PathBuf,
        /// Why.
        source: io::Error,
    },
    /// The record could not be written or removed.
    Write {
        /// The file.
        path: PathBuf,
        /// Why.
        source: io::Error,
    },
    /// The record is not the JSON this plugin writes.
    Malformed {
        /// The file.
        path: PathBuf,
        /// Why.
        source: serde_json::Error,
    },
}

impl fmt::Display for RecordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoPluginDirectory => {
                formatter.write_str("this program cannot tell which directory it is installed in")
            }
            Self::Read { path, source } => {
                write!(formatter, "{} could not be read: {source}", path.display())
            }
            Self::Write { path, source } => {
                write!(
                    formatter,
                    "{} could not be written: {source}",
                    path.display()
                )
            }
            Self::Malformed { path, source } => write!(
                formatter,
                "{} is not the record this plugin writes ({source}). Run `install` again",
                path.display()
            ),
        }
    }
}

impl core::error::Error for RecordError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } | Self::Write { source, .. } => Some(source),
            Self::Malformed { source, .. } => Some(source),
            Self::NoPluginDirectory => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("clipped-cs2-record-{name}"));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("a temporary directory");
        path
    }

    #[test]
    fn a_record_survives_a_round_trip_and_its_absence_is_not_a_failure() {
        let directory = scratch("roundtrip");
        assert_eq!(
            InstallRecord::read(&directory).expect("no record is not an error"),
            None
        );

        let record = InstallRecord {
            configuration: PathBuf::from(
                r"C:\Games\cs2\game\csgo\cfg\gamestate_integration_clipped.cfg",
            ),
        };
        record.write(&directory).expect("the record is written");
        assert_eq!(
            InstallRecord::read(&directory).expect("it reads back"),
            Some(record)
        );

        InstallRecord::remove(&directory).expect("it goes");
        assert_eq!(InstallRecord::read(&directory).expect("gone"), None);
        InstallRecord::remove(&directory).expect("removing it twice is not a failure");

        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_record_this_plugin_did_not_write_is_refused_by_name() {
        // A field this build does not know means the file was written by
        // something else, or by a later version. Either way, guessing at what
        // it meant would be worse than saying so.
        let directory = scratch("malformed");
        fs::write(
            directory.join(RECORD_FILE),
            r#"{"configuration": "C:\\cs2", "port": 3212}"#,
        )
        .expect("a file");

        let refusal = InstallRecord::read(&directory).expect_err("an unknown field");
        assert!(
            refusal.to_string().contains("install"),
            "the message should say what to do about it: {refusal}"
        );

        let _ = fs::remove_dir_all(&directory);
    }
}
