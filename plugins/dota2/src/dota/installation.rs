//! Where Dota 2 is, and where its Game State Integration configuration goes.
//!
//! # Why this is not answered by asking the game
//!
//! The plugin is told the executable's **file name** and its process
//! identifier, and nothing else (`docs/plugin-api.md`: *"a plugin needs enough
//! to find the game's own interface; the rest is somebody's private machine"*).
//! It would be technically possible to turn that identifier into a path by
//! opening a handle to the running game and asking Windows where its image came
//! from. This plugin does not, and will not.
//!
//! AGENTS.md section 34 rules out anything that resembles inspecting another
//! process, and it is a rule about a user's game account rather than about code
//! quality: what an anti-cheat sees is a process opening a handle to the game,
//! not the benign intention behind it. A user's Dota account is worth more than
//! a highlight.
//!
//! So the question is answered the way `clipped-game-detection` already answers
//! it for the launcher providers: from **Steam's own files on this disk** — the
//! library index and the application manifests, which are ordinary text files
//! Steam maintains. Nothing here touches the game process, and the plugin works
//! identically whether or not Dota is running.

use core::fmt;
use std::path::{Path, PathBuf};

/// Steam's identifier for Dota 2.
pub const APP_ID: &str = "570";

/// Where the game reads Game State Integration configurations from, under its
/// own installation directory.
///
/// Dota's content lives under `game\dota`, which is where its `cfg` directory
/// is — unlike Counter-Strike 2, whose path is its own. That difference is
/// exactly the sort of thing the shared `crate::gsi` half is careful not to
/// know.
const CONFIG_DIRECTORY: [&str; 4] = ["game", "dota", "cfg", "gamestate_integration"];

/// The file this plugin writes, and the only file it ever writes there.
///
/// Named for Clipped so that it cannot collide with another tool's integration,
/// and so that a user looking at the directory can see which of the files in it
/// is ours and delete it if they want the integration gone.
pub const CONFIG_FILE: &str = "gamestate_integration_clipped.cfg";

/// The configuration directory inside a Dota 2 installation.
///
/// Pure, so that the path this plugin would write to is testable on a machine
/// with no Dota, no Steam and no Windows.
#[must_use]
pub fn configuration_directory_under(installation: &Path) -> PathBuf {
    CONFIG_DIRECTORY
        .iter()
        .fold(installation.to_path_buf(), |path, segment| {
            path.join(segment)
        })
}

/// Finds Dota 2 through Steam and returns its configuration directory.
///
/// # Errors
///
/// [`InstallationError`], which is reported to the user as a `problem` rather
/// than logged: every one of them has an action attached to it, and a plugin
/// that quietly did nothing would be indistinguishable from a game with no
/// events (AGENTS.md sections 15 and 45).
#[cfg(windows)]
pub fn configuration_directory() -> Result<PathBuf, InstallationError> {
    use clipped_game_detection::launcher::steam::Steam;

    let steam = Steam::discover()
        .map_err(|error| InstallationError::SteamUnreadable {
            because: error.to_string(),
        })?
        .ok_or(InstallationError::NoSteam)?;
    let dota = steam
        .app_by_id(APP_ID)
        .ok_or(InstallationError::NotInstalled)?;
    Ok(configuration_directory_under(dota.installation_directory()))
}

/// See the Windows implementation above. Clipped is a Windows application
/// (SPEC.md section 3), and Steam's install location is read from the Windows
/// registry.
#[cfg(not(windows))]
pub fn configuration_directory() -> Result<PathBuf, InstallationError> {
    Err(InstallationError::NoSteam)
}

/// Why the game's configuration directory could not be found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallationError {
    /// Steam is not installed on this machine, or the registry no longer names
    /// a directory that is there.
    NoSteam,
    /// Steam is installed, and something about reading it failed.
    SteamUnreadable {
        /// What `clipped-game-detection` said, as a sentence.
        because: String,
    },
    /// Steam is installed and has no manifest for Dota 2.
    NotInstalled,
}

impl fmt::Display for InstallationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSteam => formatter.write_str(
                "Clipped could not find Steam on this computer, so it could not set Dota 2 up to \
                 report its events.",
            ),
            Self::SteamUnreadable { because } => write!(
                formatter,
                "Clipped could not read Steam's library to find Dota 2: {because}"
            ),
            Self::NotInstalled => formatter.write_str(
                "Steam has no record of Dota 2 being installed, so Clipped could not set it up to \
                 report its events.",
            ),
        }
    }
}

impl core::error::Error for InstallationError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_configuration_goes_where_dota_reads_it_from() {
        let path = configuration_directory_under(Path::new(
            r"D:\SteamLibrary\steamapps\common\dota 2 beta",
        ));
        assert_eq!(
            path,
            Path::new(
                r"D:\SteamLibrary\steamapps\common\dota 2 beta\game\dota\cfg\gamestate_integration"
            )
        );
    }

    #[test]
    fn the_file_is_named_for_clipped_so_it_can_never_be_somebody_elses() {
        // Other tools install their own integrations in the same directory.
        // This name is what makes "write only our own file" (`crate::gsi::config`)
        // a rule that can be followed.
        assert!(CONFIG_FILE.contains("clipped"));
        assert!(CONFIG_FILE.starts_with("gamestate_integration_"));
        assert!(CONFIG_FILE.ends_with(".cfg"));
    }
}
