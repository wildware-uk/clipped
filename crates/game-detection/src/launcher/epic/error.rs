//! Why an Epic installation could not be read, or could only be read in part.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// Something that went wrong reading Epic's manifests.
///
/// Every variant names the file it is about, for the reason
/// [`SteamError`](crate::launcher::steam::SteamError) does: these are files
/// somebody else's installer wrote, so "a manifest was bad" without saying
/// which one leaves a user with nothing to look at (AGENTS.md section 15).
#[derive(Debug)]
pub enum EpicError {
    /// The manifests directory a caller named is not there.
    ///
    /// Only [`Epic::read_at`](super::Epic::read_at) produces this. Discovery
    /// treats an absent directory as Epic not being installed, because that is
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

    /// A manifest exists and could not be read.
    Read {
        /// The file.
        path: PathBuf,
        /// What the filesystem said.
        source: io::Error,
    },

    /// A manifest was read and is not the JSON Epic writes.
    Parse {
        /// The file.
        path: PathBuf,
        /// What the parser said, which carries a line and column.
        detail: String,
    },

    /// A manifest parsed and does not describe an installed application.
    ///
    /// Separate from [`Self::Parse`] because the file is well-formed: it is a
    /// manifest for something with no install location, or no executable, which
    /// Epic writes for an entitlement that has never been installed. That is
    /// not a fault and is not reported as one — see
    /// [`Epic::read_at`](super::Epic::read_at) — but the shape is here so a
    /// manifest missing a field it should have had can say which field.
    Incomplete {
        /// The file.
        path: PathBuf,
        /// The field that was absent or empty.
        field: &'static str,
    },
}

impl EpicError {
    /// The file this is about, for a screen that shows somebody their own disk.
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            Self::MissingRoot { path }
            | Self::List { path, .. }
            | Self::Read { path, .. }
            | Self::Parse { path, .. }
            | Self::Incomplete { path, .. } => path,
        }
    }
}

impl fmt::Display for EpicError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingRoot { path } => write!(
                formatter,
                "{} is not there, so there are no Epic manifests to read",
                path.display()
            ),
            Self::List { path, source } => write!(
                formatter,
                "{} could not be listed: {source}",
                path.display()
            ),
            Self::Read { path, source } => {
                write!(formatter, "{} could not be read: {source}", path.display())
            }
            Self::Parse { path, detail } => write!(
                formatter,
                "{} is not the JSON the Epic launcher writes: {detail}",
                path.display()
            ),
            Self::Incomplete { path, field } => write!(
                formatter,
                "{} describes no {field}, so nothing in it can be located on this disk",
                path.display()
            ),
        }
    }
}

impl std::error::Error for EpicError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::List { source, .. } | Self::Read { source, .. } => Some(source),
            Self::MissingRoot { .. } | Self::Parse { .. } | Self::Incomplete { .. } => None,
        }
    }
}
