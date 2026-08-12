//! Why Steam's own files could not be read.
//!
//! One type for two jobs, deliberately. A [`SteamError`] is returned when
//! nothing could be read at all, and the *same* type is collected into
//! [`super::Steam::problems`] when one file out of a hundred could not be. The
//! two cases differ in what the caller can still do, not in what went wrong, and
//! a second parallel enum would have to be kept in step with this one for no
//! benefit.
//!
//! Every variant names the file. That is the requirement issue #43 sets — a
//! malformed manifest must fail with something naming the file rather than
//! panicking or silently yielding nothing — and it is the same rule the
//! catalogue's errors follow (`crate::catalogue::error`), for the same reason:
//! whoever has to fix it should not have to read any Rust to find out where.
//!
//! # Naming a file without describing somebody's disk
//!
//! Every one of these paths is somewhere a person chose to put Steam. A Windows
//! user path starts with the account name and a library path names the folders
//! they picked, and these messages reach the log file: [`super::report`] logs
//! each collected problem at `warn`, so an unredacted `Display` would put the
//! account name and the drive layout into a file users hand to strangers
//! (AGENTS.md section 13, `docs/logging.md`).
//!
//! So `Display` prints [`RedactedPath`] — the final component, which is the part
//! that says *which* file, and a digest of the whole path, which is what
//! correlates two lines about the same one. The whole path stays on the variant
//! and is reachable through [`SteamError::path`] for a caller with a legitimate
//! need for it, such as a diagnostics screen showing the user their own disk.
//! This is the same trade `clipped_muxer::MuxError` makes, for the same reason.

use std::error::Error;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use clipped_logging::RedactedPath;

/// Why a Steam file could not be read, or could not be believed.
#[derive(Debug)]
#[non_exhaustive]
pub enum SteamError {
    /// Steam's location could not be read out of the registry.
    ///
    /// Not the same as Steam being absent, which is
    /// [`Steam::discover`](super::Steam::discover) answering `Ok(None)`: this
    /// is the registry itself refusing.
    Registry {
        /// What was being read, in words, e.g. ``HKEY_CURRENT_USER\Software\Valve\Steam SteamPath``.
        doing: String,
        /// What Windows said.
        source: io::Error,
    },

    /// The directory a caller named is not there.
    ///
    /// Only [`Steam::read_at`](super::Steam::read_at) produces this. Discovery
    /// treats a registry entry pointing at a directory that has gone as Steam
    /// not being installed, because that is what it is.
    MissingRoot {
        /// The directory that is not there.
        path: PathBuf,
    },

    /// A file exists and could not be read.
    Read {
        /// The file.
        path: PathBuf,
        /// What the filesystem said.
        source: io::Error,
    },

    /// A library folder's manifests could not be listed.
    ///
    /// The ordinary cause is a library on a drive that is not plugged in.
    /// Detection carries on with the libraries that are.
    Library {
        /// The library folder, and not the `steamapps` directory inside it that
        /// the listing actually failed on: the library is the thing a person
        /// recognises, and it is the component that survives redaction as
        /// something they can act on. Every library holds its manifests in the
        /// same place, so nothing is lost by naming the library instead.
        path: PathBuf,
        /// What the filesystem said.
        source: io::Error,
    },

    /// A file is not KeyValues. The message carries the line.
    Syntax {
        /// The file.
        path: PathBuf,
        /// The reader's account of it.
        message: String,
    },

    /// A file parsed but does not hold what its kind of file holds.
    Shape {
        /// The file.
        path: PathBuf,
        /// What was looked for and not found.
        missing: String,
    },
}

impl SteamError {
    /// The file or directory this is about, whole and unredacted.
    ///
    /// `None` only for [`Self::Registry`], which is about a registry key rather
    /// than a path. This is the accessor for a caller that has a reason to show
    /// somebody their own disk — a diagnostics screen listing
    /// [`Steam::problems`](super::Steam::problems), for instance. It is not for
    /// logging: the module documentation says why, and [`Self::to_string`]
    /// already gives the redacted form.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Registry { .. } => None,
            Self::MissingRoot { path }
            | Self::Read { path, .. }
            | Self::Library { path, .. }
            | Self::Syntax { path, .. }
            | Self::Shape { path, .. } => Some(path),
        }
    }
}

impl fmt::Display for SteamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Redacted, never `path.display()`: see the module documentation. The
        // file name survives, so every message still names the file the issue
        // asks it to.
        match self {
            Self::Registry { doing, source } => {
                write!(formatter, "{doing} could not be read: {source}")
            }
            Self::MissingRoot { path } => write!(
                formatter,
                "{} is not a directory, so there is no Steam installation there",
                RedactedPath::new(path)
            ),
            Self::Read { path, source } => {
                write!(
                    formatter,
                    "{} could not be read: {source}",
                    RedactedPath::new(path)
                )
            }
            Self::Library { path, source } => write!(
                formatter,
                "the Steam library {} could not be listed: {source}",
                RedactedPath::new(path)
            ),
            Self::Syntax { path, message } => write!(
                formatter,
                "{} is not valid KeyValues: {message}",
                RedactedPath::new(path)
            ),
            Self::Shape { path, missing } => {
                write!(formatter, "{} has no {missing}", RedactedPath::new(path))
            }
        }
    }
}

impl Error for SteamError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Registry { source, .. }
            | Self::Read { source, .. }
            | Self::Library { source, .. } => Some(source),
            Self::MissingRoot { .. } | Self::Syntax { .. } | Self::Shape { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path with an account name and chosen folders in it, in the form the
    /// platform these tests are compiled for uses. `Path::file_name` splits on
    /// that platform's separators only, so a backslash-separated literal is one
    /// long file name on Linux and would make the assertions below pass for the
    /// wrong reason.
    #[cfg(windows)]
    const LIBRARY: &str = r"D:\Users\alice\Games\SteamLibrary";
    #[cfg(not(windows))]
    const LIBRARY: &str = "/home/alice/Games/SteamLibrary";

    fn refused() -> io::Error {
        io::Error::from(io::ErrorKind::PermissionDenied)
    }

    /// Every one of these ends up in a log file through
    /// [`super::super::report`], so none of them may carry a directory that
    /// names the machine's owner (AGENTS.md section 13).
    #[test]
    fn no_message_carries_a_directory_above_the_file_it_names() {
        let manifest = Path::new(LIBRARY)
            .join("steamapps")
            .join("appmanifest_1.acf");
        let messages = [
            SteamError::MissingRoot {
                path: manifest.clone(),
            }
            .to_string(),
            SteamError::Read {
                path: manifest.clone(),
                source: refused(),
            }
            .to_string(),
            SteamError::Library {
                path: PathBuf::from(LIBRARY),
                source: refused(),
            }
            .to_string(),
            SteamError::Syntax {
                path: manifest.clone(),
                message: "line 4".to_owned(),
            }
            .to_string(),
            SteamError::Shape {
                path: manifest,
                missing: "`name`".to_owned(),
            }
            .to_string(),
        ];

        for message in &messages {
            for leaked in ["alice", "Users", "Games", "steamapps"] {
                assert!(
                    !message.contains(leaked),
                    "{message} leaked the directory {leaked}"
                );
            }
        }
    }

    /// Redaction must not cost the requirement the issue actually sets: the file
    /// somebody has to go and look at is still named.
    #[test]
    fn the_file_a_person_has_to_look_at_is_still_named() {
        let manifest = Path::new(LIBRARY)
            .join("steamapps")
            .join("appmanifest_1.acf");
        let message = SteamError::Shape {
            path: manifest,
            missing: "`name`".to_owned(),
        }
        .to_string();

        assert!(
            message.contains("appmanifest_1.acf"),
            "the message should still name the manifest: {message}"
        );
        assert!(
            message.contains("`name`"),
            "and the key that is missing: {message}"
        );
    }

    /// The library is named rather than the `steamapps` directory inside it,
    /// because `steamapps` is the same word for every library and would leave a
    /// person with nothing to act on.
    #[test]
    fn an_unreadable_library_names_the_library() {
        let message = SteamError::Library {
            path: PathBuf::from(LIBRARY),
            source: refused(),
        }
        .to_string();

        assert!(
            message.contains("SteamLibrary"),
            "the message should name the library: {message}"
        );
    }

    /// The whole path stays available for the one caller that legitimately shows
    /// it: a diagnostics screen, on the user's own machine.
    #[test]
    fn the_whole_path_is_still_reachable_for_a_caller_that_needs_it() {
        let error = SteamError::Read {
            path: PathBuf::from(LIBRARY),
            source: refused(),
        };
        assert_eq!(error.path(), Some(Path::new(LIBRARY)));
        assert_eq!(
            SteamError::Registry {
                doing: "a key".to_owned(),
                source: refused(),
            }
            .path(),
            None,
            "a registry failure is not about a path"
        );
    }
}
