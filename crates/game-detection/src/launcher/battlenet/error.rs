//! Why a Battle.net installation could not be read, or could only be read in
//! part.

use std::fmt;
use std::io;

/// Something that went wrong reading Battle.net's uninstall entries.
///
/// Every variant names what it was reading, for the reason
/// [`SteamError`](crate::launcher::steam::SteamError) does: a bare "the registry
/// failed" leaves somebody with nothing to look at (AGENTS.md section 15).
#[derive(Debug)]
pub enum BattleNetError {
    /// The registry refused, for a reason other than the key not being there.
    ///
    /// A key that is absent is the answer "Battle.net is not installed", which
    /// is not a fault and never reaches here.
    Registry {
        /// What was being read, in words.
        doing: String,
        /// What the registry said.
        source: io::Error,
    },

    /// An entry the Blizzard uninstaller owns does not say where the game is.
    ///
    /// Collected into [`BattleNet::problems`](super::BattleNet::problems) so
    /// that the other games are still returned.
    Incomplete {
        /// The product identifier, or the key it was found under.
        product: String,
        /// What was missing.
        missing: &'static str,
    },
}

impl BattleNetError {
    /// What this is about, for a screen that shows somebody their own machine.
    #[must_use]
    pub fn doing(&self) -> &str {
        match self {
            Self::Registry { doing, .. } => doing,
            Self::Incomplete { product, .. } => product,
        }
    }
}

impl fmt::Display for BattleNetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Registry { doing, source } => {
                write!(formatter, "{doing} could not be read: {source}")
            }
            Self::Incomplete { product, missing } => write!(
                formatter,
                "the Battle.net entry for {product} has no {missing}, so it cannot be matched \
                 against a running program"
            ),
        }
    }
}

impl std::error::Error for BattleNetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Registry { source, .. } => Some(source),
            Self::Incomplete { .. } => None,
        }
    }
}
