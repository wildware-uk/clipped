//! The settings file, and the promise that a bad one costs nothing.
//!
//! # Why a store rather than a `load` function
//!
//! Because of the second acceptance criterion: *the previous valid
//! configuration is retained*. That is a statement about something that holds a
//! configuration across a failed read, which a free function cannot be. The
//! store owns the last configuration it knows to be good, and every failure
//! leaves it exactly as it was — so a user who hand-edits their settings into
//! nonsense while Clipped is running keeps recording with the settings they had
//! (AGENTS.md sections 16 and 56).
//!
//! The file is equally protected: nothing here rewrites a file it could not
//! understand. A file this build cannot read is far more likely to have been
//! written by a newer one than to be worthless.

use std::path::{Path, PathBuf};

use crate::config::document::{self, Loaded, FILE_NAME};
use crate::config::error::ConfigurationError;
use crate::config::Configuration;

/// The settings file and the last configuration read from it.
#[derive(Debug, Clone)]
pub struct ConfigurationStore {
    path: PathBuf,
    current: Configuration,
}

impl ConfigurationStore {
    /// A store over `path`, holding the defaults until something is loaded.
    ///
    /// Nothing is read here. Construction that touches the filesystem is
    /// construction that can fail, and a caller should choose when that
    /// happens.
    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            current: Configuration::defaults(),
        }
    }

    /// The settings file under Clipped's per-user directory —
    /// `%LOCALAPPDATA%\Clipped\settings.json` on Windows.
    ///
    /// `None` when the environment describes no per-user directory at all,
    /// which is the same supported state `clipped_logging::application_directory`
    /// documents: settings are then the defaults for the run, and nothing is
    /// saved.
    #[must_use]
    pub fn default_path() -> Option<PathBuf> {
        clipped_logging::application_directory().map(|directory| directory.join(FILE_NAME))
    }

    /// Where the settings are kept.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The configuration in force.
    #[must_use]
    pub const fn current(&self) -> &Configuration {
        &self.current
    }

    /// Reads the file, replacing the configuration in force only if it can be
    /// read in full.
    ///
    /// A missing file is [`Loaded::Absent`] and not an error: a user who has
    /// never changed a setting has no settings file, and inventing one on first
    /// run would write to their disk for nothing.
    ///
    /// # Errors
    ///
    /// Any [`ConfigurationError`]. On every one of them the configuration in
    /// force is unchanged and the file is untouched.
    pub fn load(&mut self) -> Result<Loaded, ConfigurationError> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Loaded::Absent)
            }
            Err(source) => {
                return Err(ConfigurationError::Read {
                    path: self.path.clone(),
                    source,
                })
            }
        };

        // Parsed into a separate value first. Assigning field by field is how a
        // half-applied configuration happens.
        let (configuration, loaded) = document::parse(&self.path, &text)?;
        self.current = configuration;
        Ok(loaded)
    }

    /// Writes `configuration` and makes it the one in force.
    ///
    /// The write is a temporary file and a rename, so that a crash or a full
    /// disk leaves either the previous settings or the new ones, never half of
    /// each (AGENTS.md section 17).
    ///
    /// # Errors
    ///
    /// [`ConfigurationError::Write`] if the directory cannot be created, the
    /// temporary file cannot be written, or the rename fails. The configuration
    /// in force is unchanged in each case.
    pub fn store(&mut self, configuration: Configuration) -> Result<(), ConfigurationError> {
        let text = document::render(&configuration);
        self.write_atomically(&text)?;
        self.current = configuration;
        Ok(())
    }

    fn write_atomically(&self, text: &str) -> Result<(), ConfigurationError> {
        let failed = |source: std::io::Error| ConfigurationError::Write {
            path: self.path.clone(),
            source,
        };

        if let Some(directory) = self.path.parent() {
            if !directory.as_os_str().is_empty() {
                std::fs::create_dir_all(directory).map_err(failed)?;
            }
        }

        let temporary = self.path.with_extension("json.tmp");
        std::fs::write(&temporary, text).map_err(failed)?;
        // `rename` replaces the destination on Windows and on Unix alike, which
        // is what makes this atomic rather than a delete and a create.
        if let Err(error) = std::fs::rename(&temporary, &self.path) {
            // Leaving the temporary file behind would have the next save fail
            // for a reason unrelated to the one that actually happened.
            let _ = std::fs::remove_file(&temporary);
            return Err(failed(error));
        }
        Ok(())
    }
}
