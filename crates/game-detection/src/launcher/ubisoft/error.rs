//! Why a Ubisoft Connect installation could not be read, or could only be read
//! in part.

use std::fmt;
use std::io;

/// Something that went wrong reading Ubisoft Connect's registry entries.
///
/// Every variant names what it was reading, for the reason
/// [`SteamError`](crate::launcher::steam::SteamError) does: a bare "the
/// registry failed" leaves somebody with nothing to look at
/// (AGENTS.md section 15). These name a key rather than a file because that is
/// where Ubisoft records itself.
#[derive(Debug)]
pub enum UbisoftError {
    /// The registry refused, for a reason other than the key not being there.
    ///
    /// A key that is absent is the answer "Ubisoft Connect is not installed",
    /// which is not a fault and never reaches here.
    Registry {
        /// What was being read, in words, e.g.
        /// ``HKEY_LOCAL_MACHINE\SOFTWARE\WOW6432Node\Ubisoft\Launcher\Installs``.
        doing: String,
        /// What the registry said.
        source: io::Error,
    },

    /// An install key exists and does not say where the game is.
    ///
    /// Ubisoft leaves a key behind between an uninstall and the launcher next
    /// tidying up, so this is a real state on a working machine rather than a
    /// corrupted one. It is collected into
    /// [`Ubisoft::problems`](super::Ubisoft::problems) and the other games are
    /// still returned.
    Incomplete {
        /// The application identifier the key is named after.
        id: String,
    },
}

impl UbisoftError {
    /// What this is about, for a screen that shows somebody their own machine.
    #[must_use]
    pub fn doing(&self) -> &str {
        match self {
            Self::Registry { doing, .. } => doing,
            Self::Incomplete { id } => id,
        }
    }
}

impl fmt::Display for UbisoftError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Registry { doing, source } => {
                write!(formatter, "{doing} could not be read: {source}")
            }
            Self::Incomplete { id } => write!(
                formatter,
                "the Ubisoft install key for application {id} has no InstallDir, so nothing in \
                 it can be located on this disk"
            ),
        }
    }
}

impl std::error::Error for UbisoftError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Registry { source, .. } => Some(source),
            Self::Incomplete { .. } => None,
        }
    }
}
