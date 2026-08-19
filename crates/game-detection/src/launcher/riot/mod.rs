//! The Riot client: reading its product metadata to say which game a path is.
//!
//! The sixth provider, and shaped like the five before it: discover an
//! installation, read what it says is installed, and answer "which application
//! is this path?" as a [`ProcessCandidate`]
//! ([issue #44](https://github.com/wildware-uk/clipped/issues/44)).
//!
//! ```text
//! Riot::discover() ──▶ %ProgramData%\Riot Games\Metadata\<product>.<patchline>\
//!                              │        <product>.<patchline>.product_settings.yaml
//!                              ▼             product_install_full_path
//! Riot::candidate_for(name, path) ──▶ ProcessCandidate ──▶ Catalogue::match_process
//! ```
//!
//! # Where this comes from
//!
//! A real installation, read before any of this was written. The directory
//! holds one entry per product *and patchline*:
//!
//! ```text
//! bacon.live                       league_of_legends.live.game_patch
//! league_of_legends.live           lion.live
//! teamfighttactics.live            teamfighttactics.pbe
//! valorant.live                    Riot Client
//! ```
//!
//! and only three of those eight held a `product_settings.yaml` — the one
//! product actually installed, and the two patchlines of the product that
//! installs inside it. The rest held a lockfile and a preview manifest: games
//! the client offers rather than games that are there. **A directory here is
//! not an installation**, which is the single most misleading thing about this
//! metadata and the reason nothing is claimed without settings to claim it
//! from.
//!
//! League's settings say where it is:
//!
//! ```text
//! product_install_full_path: "C:/Riot Games/League of Legends"
//! product_install_root: "C:/Riot Games"
//! shortcut_name: "League of Legends.lnk"
//! ```
//!
//! Note the forward slashes, and that the full path has no trailing one while
//! the root does. Nothing here cares, because the claim comparison normalises
//! both, but a fixture written from memory would have got one of the two wrong.
//!
//! # Why a product with no install path is skipped rather than reported
//!
//! Teamfight Tactics has no `product_install_full_path` at all — only a
//! `product_install_root` of `C:/Riot Games/`, because it is not separately
//! installed: it is played from the League client, in League's directory. Both
//! its patchlines are like this.
//!
//! So the *healthy* state of this machine is three products with settings, one
//! of which says where it is. Calling the other two a fault would put two
//! warnings on every machine with League on it, which is how a diagnostics
//! screen teaches people to ignore it.
//!
//! The root is deliberately not used as a fallback. `C:/Riot Games/` contains
//! every Riot game, so a product claiming it would claim anything Riot installs
//! that is not claimed more deeply — including, on a machine with only Teamfight
//! Tactics' directory present, a League process. Saying "Riot, and I cannot say
//! which game" is worth less than saying nothing, because the catalogue can
//! still match nothing by name.
//!
//! # Why the product, not the patchline, is the identity
//!
//! `league_of_legends.live` and `teamfighttactics.pbe` are the same shape: a
//! product identifier, a dot, and which branch of it is installed. **The
//! identity is the part before the first dot**, because `live` and `pbe` are
//! the same game to anybody watching a recording of it, and a catalogue entry
//! naming `league_of_legends` should match a player on either.
//!
//! `league_of_legends.live.game_patch` is why the split is on the *first* dot
//! rather than the last: it is a component of League rather than a product
//! called `league_of_legends.live`.
//!
//! Two patchlines of one product therefore claim the same identifier, which is
//! correct and is also why [`apps`](Riot::apps) may contain it twice. They
//! install to different directories, so nothing is ambiguous about which one a
//! process is in.
//!
//! # Why the settings file is read rather than a manifest
//!
//! Each product directory also holds a `.manifest` (two megabytes of chunk
//! hashes) and a `.db` (three megabytes). Neither says anything this needs, and
//! reading either to learn one path would be work a detection pass repeats.
//! `product_settings.yaml` is a kilobyte.
//!
//! # Known limitations
//!
//! - **Teamfight Tactics is detected as League of Legends.** It is played from
//!   League's client, from League's directory, and there is nothing in a path
//!   or a process name to tell the two apart. Anything that wants to know which
//!   of them is on screen has to ask the game, not the launcher.
//! - **The Riot Client itself is not a game.** `Riot Client` is a directory in
//!   the same place. It is skipped by the rule that a product directory is
//!   named `<product>.<patchline>`, rather than by naming it — it has settings,
//!   but they are `Riot Client.settings.yaml` and say nothing about a game.
//! - **Vanguard.** Riot's anti-cheat installs to its own directory and is not a
//!   product here, so it is never claimed. That is right — it is not something
//!   anybody records.
//! - **A game moved after installation** is claimed at its old path until Riot
//!   rewrites the settings. Every provider here has that limitation; it is
//!   named because a user who moved a library and lost detection deserves to
//!   find out why from the documentation rather than from a bug report.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::catalogue::{LauncherKind, ProcessCandidate};
use crate::launcher::claim::deepest_claimants;

mod error;

pub use error::RiotError;

/// The environment variable naming the machine-wide data directory.
///
/// Read rather than hard-coded as `C:\ProgramData` for the reason
/// [`epic`](crate::launcher::epic) reads it: a machine need not have a `C:`
/// drive, and a test needs somewhere else to point.
const PROGRAM_DATA: &str = "ProgramData";

/// Where the Riot client records one directory per installed product.
const METADATA: &str = r"Riot Games\Metadata";

/// The key in a product's settings that says where it is installed.
const INSTALL_PATH: &str = "product_install_full_path";

/// A Riot installation, and the products it says are installed.
#[derive(Debug)]
pub struct Riot {
    apps: Vec<RiotApp>,
    problems: Vec<RiotError>,
}

/// One product the Riot client has installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiotApp {
    id: String,
    patchline: String,
    installation_directory: PathBuf,
}

impl RiotApp {
    /// The product identifier: the part before the first dot.
    ///
    /// `league_of_legends`, not `league_of_legends.live`. See the module
    /// documentation for why the patchline is not part of it.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Which branch of the product is installed: `live`, `pbe`, and whatever
    /// Riot adds next.
    ///
    /// Kept because a diagnostics screen showing two entries for one game
    /// should be able to say why, and dropping it would make them look like a
    /// duplicate.
    #[must_use]
    pub fn patchline(&self) -> &str {
        &self.patchline
    }

    /// Where it is installed.
    #[must_use]
    pub fn installation_directory(&self) -> &Path {
        &self.installation_directory
    }
}

impl Riot {
    /// Finds the Riot client's metadata and reads it.
    ///
    /// [`None`] when Riot is not installed, which is an absent directory rather
    /// than a failure — #44's second acceptance criterion is that a launcher
    /// that is not installed is skipped silently.
    ///
    /// # Errors
    ///
    /// [`RiotError::List`] if the directory is there and cannot be listed.
    pub fn discover() -> Result<Option<Self>, RiotError> {
        let Some(data) = std::env::var_os(PROGRAM_DATA) else {
            return Ok(None);
        };
        let metadata = PathBuf::from(data).join(METADATA);
        if !metadata.is_dir() {
            return Ok(None);
        }
        Self::read_at(metadata).map(Some)
    }

    /// Reads the products in a directory named by the caller.
    ///
    /// What [`discover`](Self::discover) does once it has found the directory,
    /// and what a test points at a fixture.
    ///
    /// **A product that cannot be read does not fail the whole read.** One
    /// half-written file — an update interrupted, a drive removed — would
    /// otherwise cost the user every other game Riot knows about. Each is
    /// collected into [`problems`](Self::problems) and the rest are returned.
    ///
    /// # Errors
    ///
    /// [`RiotError::MissingRoot`] if the directory is not there and
    /// [`RiotError::List`] if it cannot be listed: the two that leave nothing
    /// to return.
    pub fn read_at(metadata: impl AsRef<Path>) -> Result<Self, RiotError> {
        let metadata = metadata.as_ref().to_path_buf();
        if !metadata.is_dir() {
            return Err(RiotError::MissingRoot { path: metadata });
        }

        let entries = fs::read_dir(&metadata).map_err(|source| RiotError::List {
            path: metadata.clone(),
            source,
        })?;

        let mut apps = Vec::new();
        let mut problems = Vec::new();

        for entry in entries.flatten() {
            let directory = entry.path();
            if !directory.is_dir() {
                continue;
            }
            let Some(name) = directory.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            // `<product>.<patchline>`. Anything without a dot is not a product
            // directory — `Riot Client` is the one this really skips — and is
            // passed over rather than reported, because it is not a fault.
            let Some((id, patchline)) = name.split_once('.') else {
                continue;
            };

            let settings = directory.join(format!("{name}.product_settings.yaml"));

            match fs::read_to_string(&settings) {
                // No `product_install_full_path` means the product is not
                // separately installed — it lives inside the one it depends on,
                // which is how Teamfight Tactics is installed. Skipped rather
                // than reported: see the module documentation for why treating
                // it as a fault would put two warnings on a healthy machine.
                Ok(text) => {
                    if let Some(path) = install_path(&text) {
                        apps.push(RiotApp {
                            id: id.to_owned(),
                            patchline: patchline.to_owned(),
                            installation_directory: PathBuf::from(path),
                        });
                    }
                }
                // No settings file at all is a product the client offers rather
                // than one that is installed — most of the directories on the
                // machine this was read from. Not being there is the ordinary
                // case and is passed over; anything else that stops the file
                // being read is a fault, and is reported by name.
                Err(source) if source.kind() == io::ErrorKind::NotFound => {}
                Err(source) => problems.push(RiotError::Unreadable {
                    path: settings,
                    source,
                }),
            }
        }

        apps.sort_by(|left, right| (&left.id, &left.patchline).cmp(&(&right.id, &right.patchline)));

        Ok(Self { apps, problems })
    }

    /// Builds one from products a caller already has, for tests and callers
    /// that read the metadata themselves.
    #[must_use]
    pub fn from_products<I>(products: I) -> Self
    where
        I: IntoIterator<Item = (String, String, PathBuf)>,
    {
        let mut apps: Vec<RiotApp> = products
            .into_iter()
            .map(|(id, patchline, installation_directory)| RiotApp {
                id,
                patchline,
                installation_directory,
            })
            .collect();
        apps.sort_by(|left, right| (&left.id, &left.patchline).cmp(&(&right.id, &right.patchline)));
        Self {
            apps,
            problems: Vec::new(),
        }
    }

    /// What this launcher calls the application it knows as `app_id`.
    ///
    /// The identifier is the one [`Self::candidate_for`] puts into a claim, so a
    /// caller holding a claim can name the application without knowing which
    /// field of which provider that identifier came from
    /// ([issue #664](https://github.com/wildware-uk/clipped/issues/664)).
    /// Riot records no display name of its own, so its identifier is the
    /// best name available and is returned as one. It is readable —
    /// `league_of_legends` — which is why this is acceptable rather than a
    /// placeholder.
    ///
    ///
    /// [`None`] when nothing installed here carries that identifier.
    #[must_use]
    pub fn name_of(&self, app_id: &str) -> Option<&str> {
        self.apps()
            .iter()
            .find(|app| app.id() == app_id)
            .map(|app| app.id())
    }

    /// The products Riot says are installed.
    #[must_use]
    pub fn apps(&self) -> &[RiotApp] {
        &self.apps
    }

    /// The product whose directory contains `executable_path`, if exactly one
    /// does.
    ///
    /// The deepest claimant wins, which is what makes a game installed inside
    /// another product's directory answer as itself
    /// ([`deepest_claimants`](crate::launcher::claim::deepest_claimants)).
    #[must_use]
    pub fn app_for(&self, executable_path: &str) -> Option<&RiotApp> {
        let claimants = deepest_claimants(executable_path, &self.apps, |app| {
            app.installation_directory.to_string_lossy().into_owned()
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
    /// The launcher identity is attached when Riot claims the path and left off
    /// when it does not, so a game Riot has never heard of is matched by the
    /// catalogue's path and name rungs exactly as well as it was before.
    #[must_use]
    pub fn candidate_for<'a>(
        &'a self,
        executable_name: &'a str,
        executable_path: &'a str,
    ) -> ProcessCandidate<'a> {
        let candidate = ProcessCandidate::new(executable_name).with_path(executable_path);
        match self.app_for(executable_path) {
            Some(app) => candidate.from_launcher(LauncherKind::Riot, &app.id),
            None => candidate,
        }
    }

    /// Products that could not be read, each naming itself.
    ///
    /// Empty on a healthy installation. A non-empty list means detection is
    /// working with less than everything Riot has, which a diagnostics screen
    /// should say rather than leaving somebody wondering why one game is never
    /// detected.
    #[must_use]
    pub fn problems(&self) -> &[RiotError] {
        &self.problems
    }
}

/// The install path out of a product's settings, if it says one.
///
/// A three-line reader rather than a YAML dependency, and deliberately: the
/// file is `key: "value"` throughout, one key is needed, and a parser for the
/// rest of YAML is a dependency, a licence to check and an attack surface bought
/// to read one string (AGENTS.md section 55 on not carrying two of a thing —
/// this crate already reads Steam's key-values by hand for the same reason).
///
/// Quotes are stripped because Riot writes them; a value without them is
/// accepted because nothing guarantees it always will.
fn install_path(settings: &str) -> Option<String> {
    settings.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        if key.trim() != INSTALL_PATH {
            return None;
        }
        let value = value.trim().trim_matches('"').trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

#[cfg(test)]
mod tests;
