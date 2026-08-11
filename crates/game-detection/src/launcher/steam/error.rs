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

use std::error::Error;
use std::fmt;
use std::io;
use std::path::PathBuf;

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
        /// The file, or the directory that could not be listed.
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

impl fmt::Display for SteamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Registry { doing, source } => {
                write!(formatter, "{doing} could not be read: {source}")
            }
            Self::MissingRoot { path } => write!(
                formatter,
                "{} is not a directory, so there is no Steam installation there",
                path.display()
            ),
            Self::Read { path, source } => {
                write!(formatter, "{} could not be read: {source}", path.display())
            }
            Self::Syntax { path, message } => write!(
                formatter,
                "{} is not valid KeyValues: {message}",
                path.display()
            ),
            Self::Shape { path, missing } => {
                write!(formatter, "{} has no {missing}", path.display())
            }
        }
    }
}

impl Error for SteamError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Registry { source, .. } | Self::Read { source, .. } => Some(source),
            Self::MissingRoot { .. } | Self::Syntax { .. } | Self::Shape { .. } => None,
        }
    }
}
