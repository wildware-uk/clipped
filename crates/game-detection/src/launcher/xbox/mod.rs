//! Xbox on Windows: reading the gaming services package repository.
//!
//! The fourth provider, after Steam, Epic and Ubisoft
//! ([issue #44](https://github.com/wildware-uk/clipped/issues/44)), and the one
//! that keeps its metadata furthest from a file.
//!
//! ```text
//! Xbox::discover() ──▶ HKLM\SOFTWARE\Microsoft\GamingServices\PackageRepository\Root
//!                                 │        \<container>\<mangled path>
//!                                 │            Package = <full name>
//!                                 │            Root    = \\?\B:\WindowsApps\<full name>\
//!                                 ▼
//! Xbox::candidate_for(name, path) ──▶ ProcessCandidate ──▶ Catalogue::match_process
//! ```
//!
//! # Why the package repository rather than the package manager
//!
//! `Get-AppxPackage` and the `PackageManager` WinRT API list every MSIX package
//! on the machine — Sticky Notes, the Ubuntu subsystem, the Xbox overlay — and
//! say nothing about which are games. The gaming services repository lists
//! **only** what the Xbox app installed, which is exactly the question this
//! provider answers, and it is a registry key rather than a WinRT call that
//! needs an apartment.
//!
//! Read from a machine with six registered packages across **two drives**:
//!
//! ```text
//! BethesdaSoftworks.ProjectAltar_1.0.12.0_x64__3275kfvn8vcwc   \\?\B:\WindowsApps\…
//! Microsoft.Limitless_1.8.14.0_x64__8wekyb3d8bbwe              \\?\B:\WindowsApps\…
//! 38985CA0.COREBase_1.0.203.0_x64_ww_5bkah9njm3e9g             \\?\B:\WindowsApps\…
//! Microsoft.4297127D64EC6_2.6.2.0_x64__8wekyb3d8bbwe           \\?\C:\Program Files\WindowsApps\…
//! ```
//!
//! # The identifier is the package family name, and deriving it has a trap
//!
//! A package *full* name carries the version, so it changes with every update
//! and is useless as a launcher identity. The *family* name — publisher and
//! name only — is what stays put, and it is what the catalogue matches on.
//!
//! The obvious derivation is "split on `__`", and it is wrong. Look at the
//! third package above: `38985CA0.COREBase_1.0.203.0_x64_ww_5bkah9njm3e9g`
//! carries a resource qualifier and has a **single** underscore before the
//! publisher where every other package has two. A `__` split drops it silently,
//! which on that machine is one game in six.
//!
//! [`family_name`] takes the name before the first underscore and the publisher
//! after the last one, which is right for both shapes. That the result is
//! correct is checkable rather than assumed: the same machine's repository has
//! a `GameSave` entry keyed by `38985CA0.COREBase_5bkah9njm3e9g`, which is what
//! this derives.
//!
//! # `\\?\` and two drives
//!
//! `Root` is an extended-length path. The prefix is stripped, because a running
//! process's path does not carry one and the two would never meet.
//!
//! Games are not all on one drive and not all under `WindowsApps`: the
//! repository is the only thing that knows where each went, which is the other
//! reason for reading it rather than scanning anywhere.

mod error;

use std::path::{Path, PathBuf};

use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;

use crate::catalogue::{LauncherKind, ProcessCandidate};
use crate::launcher::claim::deepest_claimants;
use crate::launcher::registry::{hive, read_string, subkeys};

pub use error::XboxError;

/// Where gaming services records what the Xbox app installed.
const REPOSITORY: &str = r"SOFTWARE\Microsoft\GamingServices\PackageRepository\Root";

/// The value naming a package in full, below a repository entry.
const PACKAGE: &str = "Package";

/// The value naming where it was installed.
const ROOT: &str = "Root";

/// The prefix Windows puts on an extended-length path.
const EXTENDED_LENGTH: &str = r"\\?\";

/// An Xbox installation, as gaming services describes it.
#[derive(Debug)]
pub struct Xbox {
    apps: Vec<XboxApp>,
    problems: Vec<XboxError>,
}

/// One package the Xbox app says is installed.
#[derive(Debug, Clone)]
pub struct XboxApp {
    family: String,
    package: String,
    installation: PathBuf,
}

impl XboxApp {
    /// The package family name, such as `Microsoft.Limitless_8wekyb3d8bbwe`.
    ///
    /// What reaches the catalogue as the launcher identity, and what stays the
    /// same when the game updates.
    #[must_use]
    pub fn family_name(&self) -> &str {
        &self.family
    }

    /// The package full name, which carries the version and architecture.
    #[must_use]
    pub fn package_full_name(&self) -> &str {
        &self.package
    }

    /// What a person calls it, as far as this can tell without opening the
    /// package's manifest.
    ///
    /// The name half of the family name, so `BethesdaSoftworks.ProjectAltar`
    /// reads as `ProjectAltar`. That is a developer's name for it rather than
    /// the one on the store page — the real display name is in `AppxManifest.xml`
    /// inside a directory an ordinary process cannot read — and it is better
    /// than showing somebody a publisher hash.
    #[must_use]
    pub fn name(&self) -> &str {
        let name = self
            .family
            .split('_')
            .next()
            .unwrap_or(self.family.as_str());
        name.rsplit('.').next().unwrap_or(name)
    }

    /// The directory it is installed in.
    #[must_use]
    pub fn installation_directory(&self) -> &Path {
        &self.installation
    }
}

/// The family name of a package full name, or [`None`] if it is not one.
///
/// `Name_Version_Architecture[_Resource]__Publisher` becomes `Name_Publisher`.
/// See the module documentation for why this is not a `__` split: one package in
/// six on the machine this was written against has a single underscore there.
#[must_use]
pub fn family_name(full_name: &str) -> Option<String> {
    let (name, rest) = full_name.split_once('_')?;
    let publisher = rest.rsplit('_').next().filter(|part| !part.is_empty())?;
    if name.is_empty() {
        return None;
    }
    Some(format!("{name}_{publisher}"))
}

/// A path as a running process would report it.
fn without_extended_length(path: &str) -> &str {
    path.strip_prefix(EXTENDED_LENGTH).unwrap_or(path)
}

impl Xbox {
    /// Reads the gaming services repository, if this machine has one.
    ///
    /// `Ok(None)` means no Xbox games are installed, which is not a failure.
    ///
    /// **An entry that cannot be read does not fail the whole read.** Gaming
    /// services leaves entries behind between an uninstall and its next tidy-up.
    /// Each is collected into [`problems`](Self::problems) and the rest are
    /// returned.
    ///
    /// # Errors
    ///
    /// [`XboxError::Registry`] when the key is there and the registry refuses to
    /// enumerate it, which leaves nothing to return.
    pub fn discover() -> Result<Option<Self>, XboxError> {
        let containers =
            subkeys(HKEY_LOCAL_MACHINE, REPOSITORY).map_err(|status| XboxError::Registry {
                doing: format!("{}\\{REPOSITORY}", hive(HKEY_LOCAL_MACHINE)),
                source: os_error(status),
            })?;
        let Some(containers) = containers else {
            return Ok(None);
        };

        let mut apps = Vec::new();
        let mut problems = Vec::new();
        for container in containers {
            let path = format!(r"{REPOSITORY}\{container}");
            // A container that will not enumerate is one game's worth of
            // information, not the whole repository's.
            let Ok(Some(entries)) = subkeys(HKEY_LOCAL_MACHINE, &path) else {
                continue;
            };
            for entry in entries {
                match read_entry(&format!(r"{path}\{entry}")) {
                    Ok(Some(app)) => apps.push(app),
                    Ok(None) => {}
                    Err(error) => problems.push(error),
                }
            }
        }

        // The same package can be registered under more than one container.
        apps.sort_by(|left, right| left.family.cmp(&right.family));
        apps.dedup_by(|left, right| left.package == right.package);

        Ok(Some(Self { apps, problems }))
    }

    /// Builds a provider from entries a caller supplies.
    ///
    /// What a test uses in place of the registry. Each entry is a package full
    /// name and the `Root` recorded for it, spelled as the registry spells it.
    #[must_use]
    pub fn from_packages<I, S>(packages: I) -> Self
    where
        I: IntoIterator<Item = (S, S)>,
        S: Into<String>,
    {
        let mut apps = Vec::new();
        let mut problems = Vec::new();
        for (package, root) in packages {
            let package: String = package.into();
            let root: String = root.into();
            let Some(family) = family_name(&package) else {
                problems.push(XboxError::Incomplete {
                    package,
                    missing: "recognisable package family name",
                });
                continue;
            };
            if root.trim().is_empty() {
                problems.push(XboxError::Incomplete {
                    package,
                    missing: "Root",
                });
                continue;
            }
            apps.push(XboxApp {
                family,
                package,
                installation: PathBuf::from(without_extended_length(&root)),
            });
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
            .find(|app| app.family_name() == app_id)
            .map(|app| app.name())
    }

    /// Every package the Xbox app says is installed.
    #[must_use]
    pub fn apps(&self) -> &[XboxApp] {
        &self.apps
    }

    /// The application a running executable belongs to.
    ///
    /// **Deepest installation directory first**, the same rule every other
    /// provider uses, and [`None`] when two packages claim the same directory:
    /// nothing in the repository could break that tie, and a confident wrong
    /// answer is worse than none
    /// ([issue #459](https://github.com/wildware-uk/clipped/issues/459)).
    #[must_use]
    pub fn app_for(&self, executable_path: &str) -> Option<&XboxApp> {
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
            Some(app) => candidate.from_launcher(LauncherKind::Xbox, &app.family),
            None => candidate,
        }
    }

    /// Entries that could not be read, each naming itself.
    #[must_use]
    pub fn problems(&self) -> &[XboxError] {
        &self.problems
    }
}

/// Reads one repository entry, or `Ok(None)` if it is not a package at all.
///
/// The repository holds entries that are not installations — a `GameSave` entry
/// keyed by family name, for one — and those carry no `Package` value. They are
/// skipped rather than reported: they are not faults, and a diagnostics screen
/// full of them would bury the entries that are.
fn read_entry(key: &str) -> Result<Option<XboxApp>, XboxError> {
    let package =
        read_string(HKEY_LOCAL_MACHINE, key, PACKAGE).map_err(|status| XboxError::Registry {
            doing: format!("{}\\{key} {PACKAGE}", hive(HKEY_LOCAL_MACHINE)),
            source: os_error(status),
        })?;
    let Some(package) = package.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };

    let root = read_string(HKEY_LOCAL_MACHINE, key, ROOT)
        .ok()
        .flatten()
        .filter(|value| !value.trim().is_empty());
    let Some(root) = root else {
        return Err(XboxError::Incomplete {
            package,
            missing: ROOT,
        });
    };

    let Some(family) = family_name(&package) else {
        return Err(XboxError::Incomplete {
            package,
            missing: "recognisable package family name",
        });
    };

    Ok(Some(XboxApp {
        family,
        package,
        installation: PathBuf::from(without_extended_length(&root)),
    }))
}

/// A registry status as the [`io::Error`](std::io::Error) an error carries.
fn os_error(status: windows::Win32::Foundation::WIN32_ERROR) -> std::io::Error {
    std::io::Error::from_raw_os_error(i32::try_from(status.0).unwrap_or(i32::MAX))
}

#[cfg(test)]
mod tests;
