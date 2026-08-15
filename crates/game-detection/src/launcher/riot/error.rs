//! Why a Riot installation could not be read, or could only be read in part.

use std::fmt;
use std::io;
use std::path::PathBuf;

/// Something that went wrong reading Riot's product metadata.
///
/// Every variant names the product directory or file it is about, for the
/// reason [`EpicError`](crate::launcher::epic::EpicError) does: these are files
/// somebody else's installer wrote, so "a product was bad" without saying which
/// one leaves a user with nothing to look at (AGENTS.md section 15).
#[derive(Debug)]
pub enum RiotError {
    /// The metadata directory a caller named is not there.
    ///
    /// Only [`Riot::read_at`](super::Riot::read_at) produces this. Discovery
    /// treats an absent directory as Riot not being installed, because that is
    /// what it is.
    MissingRoot {
        /// The directory that is not there.
        path: PathBuf,
    },

    /// The directory exists and could not be listed.
    List {
        /// The directory.
        path: PathBuf,
        /// What the filesystem said.
        source: io::Error,
    },

    /// One product's settings file could not be read.
    ///
    /// Collected rather than returned: one unreadable product must not cost the
    /// user every other game Riot knows about.
    Unreadable {
        /// The file.
        path: PathBuf,
        /// What the filesystem said.
        source: io::Error,
    },
}

impl fmt::Display for RiotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingRoot { path } => {
                write!(formatter, "{} is not there", path.display())
            }
            Self::List { path, source } => {
                write!(
                    formatter,
                    "{} could not be listed: {source}",
                    path.display()
                )
            }
            Self::Unreadable { path, source } => {
                write!(formatter, "{} could not be read: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for RiotError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::List { source, .. } | Self::Unreadable { source, .. } => Some(source),
            Self::MissingRoot { .. } => None,
        }
    }
}
