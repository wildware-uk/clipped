//! Why an Xbox installation could not be read, or could only be read in part.

use std::fmt;
use std::io;

/// Something that went wrong reading the gaming services package repository.
///
/// Every variant names what it was reading, for the reason
/// [`SteamError`](crate::launcher::steam::SteamError) does: a bare "the registry
/// failed" leaves somebody with nothing to look at (AGENTS.md section 15).
#[derive(Debug)]
pub enum XboxError {
    /// The registry refused, for a reason other than the key not being there.
    ///
    /// A key that is absent is the answer "no Xbox games are installed", which
    /// is not a fault and never reaches here.
    Registry {
        /// What was being read, in words.
        doing: String,
        /// What the registry said.
        source: io::Error,
    },

    /// A registered package does not say where it is, or is named in a way this
    /// cannot make a family name out of.
    ///
    /// Gaming services leaves entries behind between an uninstall and its next
    /// tidy-up, so this is a real state on a working machine rather than a
    /// corrupted one. It is collected into
    /// [`Xbox::problems`](super::Xbox::problems) and the other games are still
    /// returned.
    Incomplete {
        /// The package full name, or the key it was found under.
        package: String,
        /// What was missing.
        missing: &'static str,
    },
}

impl XboxError {
    /// What this is about, for a screen that shows somebody their own machine.
    #[must_use]
    pub fn doing(&self) -> &str {
        match self {
            Self::Registry { doing, .. } => doing,
            Self::Incomplete { package, .. } => package,
        }
    }
}

impl fmt::Display for XboxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Registry { doing, source } => {
                write!(formatter, "{doing} could not be read: {source}")
            }
            Self::Incomplete { package, missing } => write!(
                formatter,
                "the gaming services entry for {package} has no {missing}, so it cannot be \
                 matched against a running program"
            ),
        }
    }
}

impl std::error::Error for XboxError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Registry { source, .. } => Some(source),
            Self::Incomplete { .. } => None,
        }
    }
}
