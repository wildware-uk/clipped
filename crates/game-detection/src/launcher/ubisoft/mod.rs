//! Ubisoft Connect: reading its registry entries to say which game a path is.
//!
//! The third provider, after [`steam`](crate::launcher::steam) and
//! [`epic`](crate::launcher::epic), and shaped like both: discover an
//! installation, read what it says is installed, and answer "which application
//! is this path?" as a [`ProcessCandidate`]
//! ([issue #44](https://github.com/wildware-uk/clipped/issues/44)).
//!
//! ```text
//! Ubisoft::discover() ──▶ HKLM\SOFTWARE\WOW6432Node\Ubisoft\Launcher\Installs\<id>
//!                                 │                              InstallDir
//!                                 ▼
//! Ubisoft::candidate_for(name, path) ──▶ ProcessCandidate ──▶ Catalogue::match_process
//! ```
//!
//! # Where the two halves come from
//!
//! Ubisoft splits what this needs across two keys, and both were read from a
//! real installation before any of this was written:
//!
//! | Key | Carries |
//! | --- | --- |
//! | `…\WOW6432Node\Ubisoft\Launcher\Installs\<id>` | `InstallDir` |
//! | `…\CurrentVersion\Uninstall\Uplay Install <id>` | `DisplayName` |
//!
//! ```text
//! 15657  C:/Program Files (x86)/Ubisoft/Ubisoft Game Launcher/games/XDefiant/    XDefiant
//! 5595   C:/Program Files (x86)/Ubisoft/Ubisoft Game Launcher/games/Trackmania/  Trackmania
//! ```
//!
//! The identifier is the subkey's own name, so **enumerating the subkeys is the
//! list of installed games**. That is why this provider needs
//! [`registry::subkeys`](crate::launcher::registry::subkeys) where Steam's only
//! needed a value.
//!
//! Note the spelling of `InstallDir`: forward slashes, and a trailing one.
//! Nothing here cares, because [`normalise_path`] does not, but a fixture
//! written from memory would have got both wrong.
//!
//! # The name is optional and the identifier is not
//!
//! `DisplayName` lives under the *uninstall* key, which is somebody else's
//! namespace and may be absent — a game installed while the launcher was mid
//! update, an entry a cleaner removed. A game with no readable name is still
//! detected, named after its identifier, because the identifier is what the
//! catalogue matches on and the name is only for a person to read.
//!
//! # What this deliberately does not do
//!
//! **No executable tie-break.** Epic's manifests name the program the launcher
//! would start, so when two applications share a directory the executable
//! decides between them
//! ([issue #459](https://github.com/wildware-uk/clipped/issues/459)). Ubisoft's
//! registry records no executable at all. So when two identifiers claim one
//! directory this answers [`None`] rather than choosing, for the same reason
//! Epic does when its tie-break also fails: the catalogue's own path and name
//! rungs are a better answer than a confident wrong one.
//!
//! **No scan of the launcher's `games` directory.** Every installation seen sits
//! under `Ubisoft Game Launcher\games\<Name>`, and a directory listing would
//! find games the registry has forgotten. It would also invent an identifier out
//! of a path segment, which is the thing this provider exists to avoid — and
//! `<Name>` is not what Ubisoft calls the application anyway.

mod error;

use std::path::{Path, PathBuf};

use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;

use crate::catalogue::{LauncherKind, ProcessCandidate};
use crate::launcher::claim::deepest_claimants;
use crate::launcher::registry::{hive, read_string, subkeys};

pub use error::UbisoftError;

/// Where Ubisoft Connect records one subkey per installed game.
///
/// `WOW6432Node` for the reason Steam's key needs it: Ubisoft Connect is a
/// 32-bit application, so its installer writes under the redirected key and a
/// 64-bit process has to name the redirected key to see it.
const INSTALLS: &str = r"SOFTWARE\WOW6432Node\Ubisoft\Launcher\Installs";

/// The value naming a game's directory, below an install key.
const INSTALL_DIR: &str = "InstallDir";

/// Where Windows records what an installed program is called.
const UNINSTALL: &str = r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall";

/// What Ubisoft prefixes its uninstall keys with, before the identifier.
const UNINSTALL_PREFIX: &str = "Uplay Install ";

/// The value naming a game, below an uninstall key.
const DISPLAY_NAME: &str = "DisplayName";

/// A Ubisoft Connect installation, as its registry entries describe it.
#[derive(Debug)]
pub struct Ubisoft {
    apps: Vec<UbisoftApp>,
    problems: Vec<UbisoftError>,
}

/// One game Ubisoft Connect says is installed.
#[derive(Debug, Clone)]
pub struct UbisoftApp {
    id: String,
    display_name: String,
    installation: PathBuf,
}

impl UbisoftApp {
    /// Ubisoft's own identifier for the application, such as `15657`.
    ///
    /// This is what reaches the catalogue as the launcher identity, so an entry
    /// naming it matches whatever the executable happens to be called.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// What a person calls it, falling back to [`id`](Self::id).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.display_name
    }

    /// The directory it is installed in.
    #[must_use]
    pub fn installation_directory(&self) -> &Path {
        &self.installation
    }
}

impl Ubisoft {
    /// Reads Ubisoft Connect's install keys, if this machine has them.
    ///
    /// `Ok(None)` means Ubisoft Connect is not installed, which is not a
    /// failure — most machines do not have it, and a detector that reported its
    /// absence as a problem would put a warning on every one of them.
    ///
    /// **An install key that cannot be read does not fail the whole read.** One
    /// leftover key — an uninstall the launcher has not tidied up after — would
    /// otherwise cost the user every other game Ubisoft knows about. Each is
    /// collected into [`problems`](Self::problems) and the rest are returned.
    ///
    /// # Errors
    ///
    /// [`UbisoftError::Registry`] when the key is there and the registry
    /// refuses to enumerate it, which leaves nothing to return.
    pub fn discover() -> Result<Option<Self>, UbisoftError> {
        let ids =
            subkeys(HKEY_LOCAL_MACHINE, INSTALLS).map_err(|status| UbisoftError::Registry {
                doing: format!("{}\\{INSTALLS}", hive(HKEY_LOCAL_MACHINE)),
                source: os_error(status),
            })?;
        let Some(ids) = ids else {
            return Ok(None);
        };

        let mut apps = Vec::new();
        let mut problems = Vec::new();
        for id in ids {
            match Self::read_install(&id) {
                Ok(Some(app)) => apps.push(app),
                Ok(None) => problems.push(UbisoftError::Incomplete { id }),
                Err(error) => problems.push(error),
            }
        }
        Ok(Some(Self { apps, problems }))
    }

    /// Builds a provider from entries a caller supplies.
    ///
    /// What a test uses in place of the registry, and the only way this type is
    /// constructed without one. Each entry is an identifier, an install
    /// directory, and the name to show for it — `None` falls back to the
    /// identifier, which is what a missing `DisplayName` produces.
    #[must_use]
    pub fn from_installs<I, S>(installs: I) -> Self
    where
        I: IntoIterator<Item = (S, S, Option<S>)>,
        S: Into<String>,
    {
        let mut apps = Vec::new();
        let mut problems = Vec::new();
        for (id, directory, name) in installs {
            let id: String = id.into();
            let directory: String = directory.into();
            if directory.trim().is_empty() {
                problems.push(UbisoftError::Incomplete { id });
                continue;
            }
            let display_name = name
                .map(Into::into)
                .filter(|name: &String| !name.trim().is_empty())
                .unwrap_or_else(|| id.clone());
            apps.push(UbisoftApp {
                id,
                display_name,
                installation: PathBuf::from(directory),
            });
        }
        Self { apps, problems }
    }

    /// Reads one install key, or `Ok(None)` if it names no directory.
    fn read_install(id: &str) -> Result<Option<UbisoftApp>, UbisoftError> {
        let key = format!(r"{INSTALLS}\{id}");
        let directory = read_string(HKEY_LOCAL_MACHINE, &key, INSTALL_DIR).map_err(|status| {
            UbisoftError::Registry {
                doing: format!("{}\\{key} {INSTALL_DIR}", hive(HKEY_LOCAL_MACHINE)),
                source: os_error(status),
            }
        })?;
        let Some(directory) = directory.filter(|value| !value.trim().is_empty()) else {
            return Ok(None);
        };

        // A name that will not read is not worth failing a game over: it is in
        // somebody else's namespace, and the identifier is what matches.
        let uninstall = format!(r"{UNINSTALL}\{UNINSTALL_PREFIX}{id}");
        let display_name = read_string(HKEY_LOCAL_MACHINE, &uninstall, DISPLAY_NAME)
            .ok()
            .flatten()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| id.to_owned());

        Ok(Some(UbisoftApp {
            id: id.to_owned(),
            display_name,
            installation: PathBuf::from(directory),
        }))
    }

    /// What this launcher calls the application it knows as `app_id`.
    ///
    /// The identifier is the one [`Self::candidate_for`] puts into a claim, so a
    /// caller holding a claim can name the application without knowing which
    /// field of which provider that identifier came from
    /// ([issue #664](https://github.com/wildware-uk/clipped/issues/664)).
    ///
    /// [`None`] when nothing installed here carries that identifier.
    #[must_use]
    pub fn name_of(&self, app_id: &str) -> Option<&str> {
        self.apps()
            .iter()
            .find(|app| app.id() == app_id)
            .map(|app| app.name())
    }

    /// Every game Ubisoft Connect says is installed.
    #[must_use]
    pub fn apps(&self) -> &[UbisoftApp] {
        &self.apps
    }

    /// The application a running executable belongs to.
    ///
    /// **Deepest installation directory first**, the same rule Steam's and
    /// Epic's providers use: one directory can sit inside another and the more
    /// specific answer is the right one.
    ///
    /// **And when two identifiers claim the same directory, the answer is
    /// [`None`].** Ubisoft's registry records no executable, so there is
    /// nothing to break the tie with — see the module documentation.
    #[must_use]
    pub fn app_for(&self, executable_path: &str) -> Option<&UbisoftApp> {
        let claimants = deepest_claimants(executable_path, &self.apps, |app| {
            app.installation.to_string_lossy().into_owned()
        });

        match claimants.as_slice() {
            [only] => Some(only),
            // Nothing claimed it, or more than one did. Either way this cannot
            // say which application it is, and saying so is the point.
            _ => None,
        }
    }

    /// A running process as the catalogue wants to be asked about it.
    ///
    /// The launcher identity is attached when Ubisoft claims the path and left
    /// off when it does not, so a game Ubisoft has never heard of is matched by
    /// the catalogue's path and name rungs exactly as well as it was before.
    #[must_use]
    pub fn candidate_for<'a>(
        &'a self,
        executable_name: &'a str,
        executable_path: &'a str,
    ) -> ProcessCandidate<'a> {
        let candidate = ProcessCandidate::new(executable_name).with_path(executable_path);
        match self.app_for(executable_path) {
            Some(app) => candidate.from_launcher(LauncherKind::Ubisoft, &app.id),
            None => candidate,
        }
    }

    /// Install keys that could not be read, each naming itself.
    ///
    /// Empty on a healthy installation. A non-empty list means detection is
    /// working with less than everything Ubisoft has, which a diagnostics screen
    /// should say rather than leaving somebody wondering why one game is never
    /// detected.
    #[must_use]
    pub fn problems(&self) -> &[UbisoftError] {
        &self.problems
    }
}

/// A registry status as the [`io::Error`](std::io::Error) an error variant
/// carries.
fn os_error(status: windows::Win32::Foundation::WIN32_ERROR) -> std::io::Error {
    std::io::Error::from_raw_os_error(i32::try_from(status.0).unwrap_or(i32::MAX))
}

#[cfg(test)]
mod tests;
