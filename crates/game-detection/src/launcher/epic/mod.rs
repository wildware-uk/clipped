//! The Epic Games launcher: reading its manifests to say which game a path is.
//!
//! The second provider, after [`steam`](crate::launcher::steam), and
//! deliberately shaped like it: discover an installation, read what it says is
//! installed, and answer "which application is this path?" as a
//! [`ProcessCandidate`]. Whether that application is worth recording stays the
//! catalogue's decision
//! ([issue #44](https://github.com/wildware-uk/clipped/issues/44)).
//!
//! ```text
//! Epic::discover() ──▶ %ProgramData%\Epic\EpicGamesLauncher\Data\Manifests\*.item
//!                              │
//!                              ▼
//! Epic::candidate_for(name, path) ──▶ ProcessCandidate ──▶ Catalogue::match_process
//! ```
//!
//! # Why this is simpler than Steam
//!
//! Steam has one registry key, a library index in its own key-value format, and
//! a manifest per application spread across however many drives the user has
//! added. Epic writes **one JSON file per installed application**, all in one
//! directory, each naming its own install location in full. There is no index to
//! follow and no second location to find, so there is no `libraries` step here
//! and no equivalent of `keyvalues`.
//!
//! The directory is machine-wide (`%ProgramData%`) rather than per-user, which
//! is why discovery reads an environment variable rather than the registry.
//!
//! # What a manifest carries, and what is used
//!
//! Epic writes a good deal more than this. Four fields are read:
//!
//! | Field | Used for |
//! | --- | --- |
//! | `AppName` | the launcher identity handed to the catalogue |
//! | `DisplayName` | what a person calls the game |
//! | `InstallLocation` | claiming a running executable's path |
//! | `LaunchExecutable` | the program Epic itself would start |
//!
//! `CatalogNamespace` and `CatalogItemId` are deliberately not read: they
//! identify the *store listing* rather than the installed application, and the
//! catalogue's launcher rung is about what is running.
//!
//! # An entitlement is not an installation
//!
//! A manifest with no `InstallLocation` is one Epic wrote for something the user
//! owns and has not installed. That is not a fault and is not reported as one:
//! it is skipped, and nothing about it reaches [`Epic::problems`]. Reporting it
//! would fill a diagnostics screen with every game somebody ever claimed from
//! the weekly giveaway.

mod error;

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::catalogue::{normalise_path, path_segments, LauncherKind, ProcessCandidate};

pub use error::EpicError;

/// The environment variable naming the machine-wide data directory.
///
/// `%ProgramData%`, which is where Epic puts the manifests. Read rather than
/// hard-coded as `C:\ProgramData` because a machine need not have a `C:` drive,
/// and because a test needs somewhere else to point.
const PROGRAM_DATA: &str = "ProgramData";

/// Where Epic keeps its manifests, below the data directory.
const MANIFESTS: &str = r"Epic\EpicGamesLauncher\Data\Manifests";

/// The extension every manifest has.
const MANIFEST_EXTENSION: &str = "item";

/// An Epic installation, as its manifests describe it.
#[derive(Debug)]
pub struct Epic {
    manifests: PathBuf,
    apps: Vec<EpicApp>,
    problems: Vec<EpicError>,
}

/// One application Epic says is installed.
#[derive(Debug, Clone)]
pub struct EpicApp {
    app_name: String,
    display_name: String,
    installation: PathBuf,
    executable: String,
    manifest: PathBuf,
}

/// The fields of a manifest this reads.
///
/// `deny_unknown_fields` is deliberately **not** set: Epic adds fields between
/// launcher versions, and an update must not stop detection working.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Manifest {
    #[serde(default)]
    app_name: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    install_location: String,
    #[serde(default)]
    launch_executable: String,
}

impl EpicApp {
    /// Epic's own identifier for the application, such as `Fortnite`.
    ///
    /// This is what reaches the catalogue as the launcher identity, so an entry
    /// naming it matches whatever the executable happens to be called.
    #[must_use]
    pub fn app_name(&self) -> &str {
        &self.app_name
    }

    /// What a person calls it.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.display_name
    }

    /// The directory it is installed in.
    #[must_use]
    pub fn installation_directory(&self) -> &Path {
        &self.installation
    }

    /// The program Epic would start, relative to the installation directory.
    #[must_use]
    pub fn executable(&self) -> &str {
        &self.executable
    }

    /// The manifest this was read from.
    #[must_use]
    pub fn manifest(&self) -> &Path {
        &self.manifest
    }
}

impl Epic {
    /// Finds the Epic launcher's manifests, if this machine has them.
    ///
    /// `Ok(None)` means Epic is not installed, which is not a failure — most
    /// machines do not have it, and a detector that reported its absence as a
    /// problem would put a warning on every one of them.
    ///
    /// # Errors
    ///
    /// [`EpicError::List`] when the directory is there and cannot be read.
    pub fn discover() -> Result<Option<Self>, EpicError> {
        let Some(data) = std::env::var_os(PROGRAM_DATA) else {
            return Ok(None);
        };
        let manifests = PathBuf::from(data).join(MANIFESTS);
        if !manifests.is_dir() {
            return Ok(None);
        }
        Self::read_at(manifests).map(Some)
    }

    /// Reads the manifests in a directory named by the caller.
    ///
    /// What [`discover`](Self::discover) does once it has found the directory,
    /// and what a test points at a fixture.
    ///
    /// **A manifest that cannot be read does not fail the whole read.** One
    /// half-written file — an update interrupted, a drive removed — would
    /// otherwise cost the user every other game Epic knows about. Each is
    /// collected into [`problems`](Self::problems) and the rest are returned.
    ///
    /// # Errors
    ///
    /// [`EpicError::MissingRoot`] if the directory is not there and
    /// [`EpicError::List`] if it cannot be listed: the two that leave nothing
    /// to return.
    pub fn read_at(manifests: impl AsRef<Path>) -> Result<Self, EpicError> {
        let manifests = manifests.as_ref().to_path_buf();
        if !manifests.is_dir() {
            return Err(EpicError::MissingRoot { path: manifests });
        }

        let entries = fs::read_dir(&manifests).map_err(|source| EpicError::List {
            path: manifests.clone(),
            source,
        })?;

        let mut apps = Vec::new();
        let mut problems = Vec::new();

        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(source) => {
                    problems.push(EpicError::List {
                        path: manifests.clone(),
                        source,
                    });
                    continue;
                }
            };

            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some(MANIFEST_EXTENSION)
            {
                continue;
            }

            match read_manifest(&path) {
                Ok(Some(app)) => apps.push(app),
                // An entitlement rather than an installation. Not a problem.
                Ok(None) => {}
                Err(problem) => {
                    tracing::warn!(
                        manifest = %clipped_logging::RedactedPath::new(&path),
                        error = %problem,
                        "an Epic manifest could not be read, so whatever it describes will not be \
                         identified as an Epic game"
                    );
                    problems.push(problem);
                }
            }
        }

        // Sorted so that two reads of one directory answer in the same order:
        // `read_dir` promises none, and a diagnostics screen that reordered
        // itself between refreshes reads as a fault.
        apps.sort_by(|left, right| left.app_name.cmp(&right.app_name));

        Ok(Self {
            manifests,
            apps,
            problems,
        })
    }

    /// The directory the manifests were read from.
    #[must_use]
    pub fn manifests(&self) -> &Path {
        &self.manifests
    }

    /// Every application Epic says is installed.
    #[must_use]
    pub fn apps(&self) -> &[EpicApp] {
        &self.apps
    }

    /// The application a running executable belongs to.
    ///
    /// Two questions, in order, and the second exists because the first is not
    /// enough on a real machine.
    ///
    /// **Deepest installation directory first.** One directory can sit inside
    /// another, and the more specific answer is the right one — the same rule
    /// Steam's provider uses.
    ///
    /// **Then the executable, because a directory does not identify an
    /// application.** Epic installs plugins *into* the thing they extend and
    /// gives each its own manifest, so several applications share one
    /// `InstallLocation`. On the machine this was checked against, three of ten
    /// directories were shared:
    ///
    /// ```text
    /// B:\Epic Games\UE_5.8  <-  QuixelBridge_5.8, FabPlugin_5.8, UE_5.8
    /// ```
    ///
    /// Depth cannot break that tie, so `LaunchExecutable` does: the manifest
    /// Epic itself would start this program from is the one that owns it.
    ///
    /// **And when that still ties, the answer is [`None`].** An arbitrary
    /// choice would hand the catalogue a launcher identity for the wrong
    /// application, and the catalogue's own path and name rungs are a better
    /// answer than a confident wrong one
    /// ([issue #459](https://github.com/wildware-uk/clipped/issues/459)).
    #[must_use]
    pub fn app_for(&self, executable_name: &str, executable_path: &str) -> Option<&EpicApp> {
        let normalised = normalise_path(executable_path);
        let path: Vec<&str> = path_segments(&normalised).collect();

        let mut deepest = 0;
        let mut claimants: Vec<&EpicApp> = Vec::new();
        for app in &self.apps {
            let normalised = normalise_path(&app.installation.to_string_lossy());
            let directory: Vec<&str> = path_segments(&normalised).collect();
            // Longer than, not at least as long as: the last segment is the
            // executable's file name, so a path equal to the installation
            // directory is a directory and not a program in it.
            if directory.is_empty() || path.len() <= directory.len() {
                continue;
            }
            if !path.starts_with(&directory) {
                continue;
            }
            if directory.len() > deepest {
                deepest = directory.len();
                claimants.clear();
            }
            if directory.len() == deepest {
                claimants.push(app);
            }
        }

        match claimants.as_slice() {
            [] => None,
            [only] => Some(only),
            several => {
                // The executable decides. `LaunchExecutable` is relative to the
                // installation directory, so only its file name is comparable
                // with the name of a running process.
                let mut named = several.iter().filter(|app| {
                    let normalised = normalise_path(&app.executable);
                    path_segments(&normalised)
                        .last()
                        .is_some_and(|file| file.eq_ignore_ascii_case(executable_name))
                });
                match (named.next(), named.next()) {
                    (Some(app), None) => Some(app),
                    // Nothing matched, or more than one did. Either way this
                    // cannot say which application it is, and saying so is the
                    // point.
                    _ => None,
                }
            }
        }
    }

    /// A running process as the catalogue wants to be asked about it.
    ///
    /// The launcher identity is attached when Epic claims the path and left off
    /// when it does not, so a game Epic has never heard of is matched by the
    /// catalogue's path and name rungs exactly as well as it was before.
    #[must_use]
    pub fn candidate_for<'a>(
        &'a self,
        executable_name: &'a str,
        executable_path: &'a str,
    ) -> ProcessCandidate<'a> {
        let candidate = ProcessCandidate::new(executable_name).with_path(executable_path);
        match self.app_for(executable_name, executable_path) {
            Some(app) => candidate.from_launcher(LauncherKind::Epic, &app.app_name),
            None => candidate,
        }
    }

    /// Manifests that could not be read, each naming itself.
    ///
    /// Empty on a healthy installation. A non-empty list means detection is
    /// working with less than everything Epic has, which a diagnostics screen
    /// should say rather than leaving somebody wondering why one game is never
    /// detected.
    #[must_use]
    pub fn problems(&self) -> &[EpicError] {
        &self.problems
    }
}

/// Reads one `.item` manifest.
///
/// `Ok(None)` is an entitlement rather than an installation — owned, never
/// installed — which the module documentation explains is not a fault.
fn read_manifest(path: &Path) -> Result<Option<EpicApp>, EpicError> {
    let text = fs::read_to_string(path).map_err(|source| EpicError::Read {
        path: path.to_path_buf(),
        source,
    })?;

    let manifest: Manifest = serde_json::from_str(&text).map_err(|error| EpicError::Parse {
        path: path.to_path_buf(),
        detail: error.to_string(),
    })?;

    if manifest.install_location.trim().is_empty() {
        return Ok(None);
    }
    if manifest.app_name.trim().is_empty() {
        return Err(EpicError::Incomplete {
            path: path.to_path_buf(),
            field: "AppName",
        });
    }

    // A manifest with no display name is odd and not fatal: the identifier is
    // what the catalogue matches on, and the name is for a person to read. It
    // falls back to the identifier rather than to an empty string, so nothing
    // downstream has to draw a nameless row.
    let display_name = if manifest.display_name.trim().is_empty() {
        manifest.app_name.clone()
    } else {
        manifest.display_name
    };

    Ok(Some(EpicApp {
        app_name: manifest.app_name,
        display_name,
        installation: PathBuf::from(manifest.install_location),
        executable: manifest.launch_executable,
        manifest: path.to_path_buf(),
    }))
}

#[cfg(test)]
mod tests;
