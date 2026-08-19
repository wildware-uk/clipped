//! Battle.net: reading the entries its uninstaller owns.
//!
//! The fifth provider ([issue #44](https://github.com/wildware-uk/clipped/issues/44)),
//! and the one whose identifier is hiding in a command line.
//!
//! ```text
//! BattleNet::discover() ──▶ …\CurrentVersion\Uninstall\<key>
//!                               UninstallString = "…\Blizzard Uninstaller.exe"
//!                                                 --lang=enUS --uid=prometheus
//!                                                 --displayname="Overwatch"
//!                               InstallLocation = B:\BattleNet\Overwatch
//!                                  │
//!                                  ▼
//! BattleNet::candidate_for(name, path) ──▶ ProcessCandidate ──▶ Catalogue
//! ```
//!
//! # Why the uninstall entries and not Battle.net's own files
//!
//! Battle.net keeps its product list in two places and **neither of them joins
//! a product to a directory**, which is the join a provider needs:
//!
//! - `%APPDATA%\Battle.net\Battle.net.config` has a `Games` section keyed by
//!   product — `battle_net`, `prometheus` — and records no install path for any
//!   of them, only a `DefaultInstallPath` for the next one.
//! - `%PROGRAMDATA%\Battle.net\Agent\product.db` is protocol buffers, which
//!   would mean carrying a parser and a schema for somebody else's private
//!   format.
//!
//! The uninstall entry has both halves, and the product identifier in it is the
//! same one the config uses, so the two agree without this having to make them.
//!
//! # The identifier is in the command line
//!
//! `--uid=prometheus` — Overwatch's product code, which is what stays the same
//! when the game is renamed or reinstalled, and what the catalogue matches on.
//! [`product_uid`] pulls it out.
//!
//! The alternative was the `Product` column of `.build.info` in the game's own
//! directory, which says `pro` for the same game. That is a *different*
//! identifier for the same thing, and reading it would mean opening a file in a
//! directory that may be on a drive that has gone. The command line is already
//! in the registry beside the path.
//!
//! # The launcher is not a game
//!
//! Battle.net's own uninstall entry is written by the same uninstaller and looks
//! identical, down to the flags: `--uid=battle.net --displayname="Battle.net"`.
//! Left in, it would claim every process under
//! `C:\Program Files (x86)\Battle.net` as a game called Battle.net. It is
//! excluded by that identifier, which is the one thing that distinguishes it.

mod error;

use std::path::{Path, PathBuf};

use windows::Win32::System::Registry::{HKEY, HKEY_LOCAL_MACHINE};

use crate::catalogue::{LauncherKind, ProcessCandidate};
use crate::launcher::claim::deepest_claimants;
use crate::launcher::registry::{hive, read_string, subkeys};

pub use error::BattleNetError;

/// Where Windows records what an installed program is.
///
/// Both are read: a 32-bit installer writes under the redirected key and a
/// 64-bit one does not, and Battle.net has shipped both.
const UNINSTALL_KEYS: [&str; 2] = [
    r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
    r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
];

/// What every Battle.net entry's uninstall command names.
///
/// The marker that says an entry is Battle.net's rather than somebody else's.
const UNINSTALLER: &str = "Blizzard Uninstaller.exe";

/// The flag carrying the product identifier.
const UID_FLAG: &str = "--uid=";

/// Battle.net's own product identifier, which is not a game.
const CLIENT_UID: &str = "battle.net";

/// The value naming where a game was installed.
const INSTALL_LOCATION: &str = "InstallLocation";

/// The value naming the game.
const DISPLAY_NAME: &str = "DisplayName";

/// A Battle.net installation, as its uninstall entries describe it.
#[derive(Debug)]
pub struct BattleNet {
    apps: Vec<BattleNetApp>,
    problems: Vec<BattleNetError>,
}

/// One game Battle.net says is installed.
#[derive(Debug, Clone)]
pub struct BattleNetApp {
    uid: String,
    display_name: String,
    installation: PathBuf,
}

impl BattleNetApp {
    /// Battle.net's own identifier, such as `prometheus` for Overwatch.
    #[must_use]
    pub fn uid(&self) -> &str {
        &self.uid
    }

    /// What a person calls it, falling back to [`uid`](Self::uid).
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

/// The product identifier in an uninstall command line, if it carries one.
///
/// `--uid=prometheus --displayname="Overwatch"` yields `prometheus`. The value
/// ends at the next space, because these are flags rather than a path and none
/// of the identifiers seen has ever contained one.
#[must_use]
pub fn product_uid(uninstall_string: &str) -> Option<&str> {
    let after = uninstall_string.split_once(UID_FLAG)?.1;
    let uid = after.split_whitespace().next()?.trim_matches('"');
    (!uid.is_empty()).then_some(uid)
}

impl BattleNet {
    /// Reads the uninstall entries the Blizzard uninstaller owns.
    ///
    /// `Ok(None)` means Battle.net has installed nothing here, which is not a
    /// failure.
    ///
    /// # Errors
    ///
    /// [`BattleNetError::Registry`] when the uninstall key is there and the
    /// registry refuses to enumerate it.
    pub fn discover() -> Result<Option<Self>, BattleNetError> {
        let mut apps = Vec::new();
        let mut problems = Vec::new();
        let mut looked = false;

        for root in UNINSTALL_KEYS {
            let entries =
                subkeys(HKEY_LOCAL_MACHINE, root).map_err(|status| BattleNetError::Registry {
                    doing: format!("{}\\{root}", hive(HKEY_LOCAL_MACHINE)),
                    source: os_error(status),
                })?;
            let Some(entries) = entries else {
                continue;
            };
            looked = true;

            for entry in entries {
                let key = format!(r"{root}\{entry}");
                match read_entry(HKEY_LOCAL_MACHINE, &key) {
                    Ok(Some(app)) => apps.push(app),
                    Ok(None) => {}
                    Err(error) => problems.push(error),
                }
            }
        }

        if !looked {
            return Ok(None);
        }
        if apps.is_empty() && problems.is_empty() {
            // The uninstall key exists on every Windows machine; finding nothing
            // of Battle.net's in it means Battle.net is not here.
            return Ok(None);
        }

        apps.sort_by(|left, right| left.uid.cmp(&right.uid));
        apps.dedup_by(|left, right| left.uid == right.uid);
        Ok(Some(Self { apps, problems }))
    }

    /// Builds a provider from entries a caller supplies.
    ///
    /// What a test uses in place of the registry: an uninstall command line, an
    /// install location, and the name to show.
    #[must_use]
    pub fn from_entries<I, S>(entries: I) -> Self
    where
        I: IntoIterator<Item = (S, S, S)>,
        S: Into<String>,
    {
        let mut apps = Vec::new();
        let mut problems = Vec::new();
        for (uninstall, location, name) in entries {
            let uninstall: String = uninstall.into();
            let location: String = location.into();
            let name: String = name.into();
            match built(&uninstall, &location, &name) {
                Ok(Some(app)) => apps.push(app),
                Ok(None) => {}
                Err(error) => problems.push(error),
            }
        }
        Self { apps, problems }
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
            .find(|app| app.uid() == app_id)
            .map(|app| app.name())
    }

    /// Every game Battle.net says is installed.
    #[must_use]
    pub fn apps(&self) -> &[BattleNetApp] {
        &self.apps
    }

    /// The application a running executable belongs to.
    ///
    /// Deepest installation directory first, and [`None`] when two products
    /// claim the same one — nothing here could break that tie, and a confident
    /// wrong answer is worse than none
    /// ([issue #459](https://github.com/wildware-uk/clipped/issues/459)).
    #[must_use]
    pub fn app_for(&self, executable_path: &str) -> Option<&BattleNetApp> {
        let claimants = deepest_claimants(executable_path, &self.apps, |app| {
            app.installation.to_string_lossy().into_owned()
        });
        match claimants.as_slice() {
            [only] => Some(only),
            _ => None,
        }
    }

    /// A running process as the catalogue wants to be asked about it.
    #[must_use]
    pub fn candidate_for<'a>(
        &'a self,
        executable_name: &'a str,
        executable_path: &'a str,
    ) -> ProcessCandidate<'a> {
        let candidate = ProcessCandidate::new(executable_name).with_path(executable_path);
        match self.app_for(executable_path) {
            Some(app) => candidate.from_launcher(LauncherKind::BattleNet, &app.uid),
            None => candidate,
        }
    }

    /// Entries that could not be read, each naming itself.
    #[must_use]
    pub fn problems(&self) -> &[BattleNetError] {
        &self.problems
    }
}

/// Builds one app from the three values an entry carries.
///
/// `Ok(None)` for an entry that is not Battle.net's, and for Battle.net itself.
fn built(
    uninstall: &str,
    location: &str,
    display_name: &str,
) -> Result<Option<BattleNetApp>, BattleNetError> {
    if !uninstall.contains(UNINSTALLER) {
        return Ok(None);
    }
    let Some(uid) = product_uid(uninstall) else {
        return Err(BattleNetError::Incomplete {
            product: display_name.to_owned(),
            missing: "--uid in its uninstall command",
        });
    };
    if uid.eq_ignore_ascii_case(CLIENT_UID) {
        return Ok(None);
    }
    if location.trim().is_empty() {
        return Err(BattleNetError::Incomplete {
            product: uid.to_owned(),
            missing: INSTALL_LOCATION,
        });
    }

    let name = if display_name.trim().is_empty() {
        uid.to_owned()
    } else {
        display_name.to_owned()
    };
    Ok(Some(BattleNetApp {
        uid: uid.to_owned(),
        display_name: name,
        installation: PathBuf::from(location),
    }))
}

/// Reads one uninstall entry.
fn read_entry(hive_key: HKEY, key: &str) -> Result<Option<BattleNetApp>, BattleNetError> {
    // An entry that will not give up its uninstall command is one of the many
    // hundreds under this key that are nothing to do with Battle.net.
    let Ok(Some(uninstall)) = read_string(hive_key, key, "UninstallString") else {
        return Ok(None);
    };
    if !uninstall.contains(UNINSTALLER) {
        return Ok(None);
    }

    let location = read_string(hive_key, key, INSTALL_LOCATION)
        .ok()
        .flatten()
        .unwrap_or_default();
    let display_name = read_string(hive_key, key, DISPLAY_NAME)
        .ok()
        .flatten()
        .unwrap_or_default();

    built(&uninstall, &location, &display_name)
}

/// A registry status as the [`io::Error`](std::io::Error) an error carries.
fn os_error(status: windows::Win32::Foundation::WIN32_ERROR) -> std::io::Error {
    std::io::Error::from_raw_os_error(i32::try_from(status.0).unwrap_or(i32::MAX))
}

#[cfg(test)]
mod tests;
