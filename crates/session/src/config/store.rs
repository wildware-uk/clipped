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
//!
//! # Why saving reads the file first
//!
//! Refusing to *read* a newer build's file preserves nothing on its own. The
//! user whose other machine is a version ahead opens the settings here, sees
//! the defaults, changes one thing, and the save is what destroys their file —
//! which is the destruction AGENTS.md section 56 is about, arrived at one step
//! later. So [`ConfigurationStore::store`] looks at what is on disk before it
//! replaces it, and refuses if this build could not read it.
//!
//! It looks at the file rather than at what the last [`ConfigurationStore::load`]
//! found, for two reasons. A store that was never asked to load has no such
//! memory and would otherwise overwrite the file blind; and a file that changed
//! since the load — the other machine's sync client landed it — is exactly the
//! case worth catching. What remains is the window between that read and the
//! rename, which no amount of remembering closes and which cross-process
//! locking ([issue #194](https://github.com/wildware-uk/clipped/issues/194))
//! is what would.

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
    /// A settings file that is already there and that this build cannot read is
    /// never replaced — see the module documentation for why that check lives
    /// here rather than in [`Self::load`].
    ///
    /// # Errors
    ///
    /// [`ConfigurationError::WouldOverwrite`] if the file on disk is one this
    /// build could not read, and [`ConfigurationError::Write`] if the directory
    /// cannot be created, the temporary file cannot be written, or the rename
    /// fails. The configuration in force is unchanged in each case, and so is
    /// the file.
    pub fn store(&mut self, configuration: Configuration) -> Result<(), ConfigurationError> {
        self.check_the_file_may_be_replaced()?;
        let text = document::render(&configuration);
        self.write_atomically(&text)?;
        self.current = configuration;
        Ok(())
    }

    /// Fails when there is a settings file this build cannot read.
    ///
    /// An absent file is fine — there is nothing to destroy — and so is one
    /// that parses, whatever version it is at: a version 0 file is one this
    /// build understands, and saving is how it becomes a version 1 one.
    fn check_the_file_may_be_replaced(&self) -> Result<(), ConfigurationError> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            // Unreadable for any other reason is reported rather than written
            // over. A file that cannot be read is not one whose contents are
            // known to be worthless.
            Err(source) => {
                return Err(ConfigurationError::Read {
                    path: self.path.clone(),
                    source,
                })
            }
        };
        document::parse(&self.path, &text)
            .map(|_| ())
            .map_err(|source| ConfigurationError::WouldOverwrite {
                path: self.path.clone(),
                source: Box::new(source),
            })
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

        // Through `clipped_logging`, which names the temporary after this
        // process and sweeps the ones abandoned processes left
        // ([issue #400](https://github.com/wildware-uk/clipped/issues/400)).
        // The name here used to be a fixed `json.tmp`, so two processes saving
        // settings at once shared one temporary and could rename half of it
        // into place.
        clipped_logging::write_atomically(&self.path, |temporary| {
            std::io::Write::write_all(temporary, text.as_bytes())
        })
        .map_err(failed)
    }
}
