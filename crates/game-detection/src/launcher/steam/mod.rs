//! What Steam knows about the games it has installed, read from its own files.
//!
//! # The question this answers
//!
//! [`crate::catalogue`] can match a process by the launcher's own identifier —
//! [`ProcessCandidate::from_launcher`], the strongest rung of its precedence
//! order — and nothing produced that identifier. This module is what produces
//! it for Steam: given the image path of a running process, which Steam
//! application is it, and what does Steam call it?
//!
//! ```text
//! HKCU\Software\Valve\Steam  SteamPath
//!            │
//!            ▼
//! <steam>\steamapps\libraryfolders.vdf ──▶ every library, not just this one
//!            │
//!            ▼
//! <library>\steamapps\appmanifest_<appid>.acf ──▶ app id, name, install dir
//!            │
//!            ▼
//! Steam::candidate_for(name, path) ──▶ ProcessCandidate ──▶ Catalogue::match_process
//! ```
//!
//! # Everything here is local
//!
//! No network calls, by the ticket and by SPEC.md section 6. Every fact comes
//! from a file Steam wrote on this machine, which also means detection works
//! offline, works while Steam is closed, and cannot be slowed down by Valve
//! having a bad afternoon.
//!
//! # Two libraries is the normal case
//!
//! A machine with games on more than one drive is ordinary — the machine this
//! was developed against keeps three applications under `C:\Program Files
//! (x86)\Steam` and eighty-five under `B:\SteamLibrary` — and a detector that
//! read only the default library would miss almost everything on it. So the
//! library index is the first thing read, and the default library is one entry
//! in it rather than a special case. Steam lists its own directory in that file,
//! so libraries are de-duplicated by normalised path.
//!
//! # One bad file does not blind the detector
//!
//! The library index is fatal if it will not parse: without it there is no
//! coherent view of anything, and guessing at a directory layout would be
//! inventing data. A single application manifest is not fatal. Steam wrote
//! eighty-eight of them on the machine this was developed against, they are
//! rewritten while games install, and refusing to detect *any* game because one
//! file is half-written would be the wrong trade for a recorder. They are
//! collected into [`Steam::problems`] instead — named, logged at `warn`, and
//! never silently dropped (AGENTS.md section 15).
//!
//! # A manifest is somebody else's file
//!
//! Nothing in a manifest is trusted to be sensible merely because Steam usually
//! writes it. The value that matters is `installdir`, because
//! [`Steam::app_for_path`] claims every executable beneath an installation
//! directory *at the catalogue's strongest rung*: a manifest naming
//! `..\..\..\Windows` would make Clipped record Notepad as a game and say it was
//! certain. So an `installdir` that does not stay inside its own library is a
//! reported problem and not an application — see [`install_path`].
//!
//! # What Steam is not asked
//!
//! - **Whether a game is running.** That is [`crate::process_watcher`]'s job,
//!   and asking Steam would mean reading `ActiveProcess` out of the registry,
//!   which reports the *last* application launched rather than what is running
//!   now.
//! - **What a game is.** Steam's manifests include redistributables, tools,
//!   soundtracks and the Steam Linux Runtime. Deciding which of those is worth
//!   recording is the catalogue's business; this module reports what Steam says
//!   is installed, without editorialising.
//! - **Anything from `appinfo.vdf`.** Steam's binary caches hold the launch
//!   configuration in an undocumented format that changes. The icon does *not*
//!   need it, which an earlier version of this module had wrong; [`icon`] says
//!   where it actually is.

mod error;
mod icon;
#[cfg(windows)]
mod registry;

use std::fs;
use std::path::{Path, PathBuf};

use clipped_logging::RedactedPath;
use tracing::{debug, warn};

use super::keyvalues::{self, Value};
use crate::catalogue::{normalise_path, path_segments, LauncherKind, ProcessCandidate};

pub use error::SteamError;

/// The library index, in the order Steam has kept it in.
///
/// `steamapps\libraryfolders.vdf` is where a current client writes it;
/// `config\libraryfolders.vdf` is where it used to live, and current clients
/// still write a copy there — the two files were byte-identical on the machine
/// this was developed against. The first that exists is read, so a client that
/// keeps only one of them is read correctly either way.
const LIBRARY_INDEX_FILES: [&str; 2] =
    ["steamapps/libraryfolders.vdf", "config/libraryfolders.vdf"];

/// Where a library keeps its manifests, relative to the library's own directory.
const MANIFEST_DIRECTORY: &str = "steamapps";

/// Where a library keeps installed applications, relative to itself.
const INSTALL_DIRECTORY: &str = "steamapps/common";

/// The prefix and suffix of an application manifest's file name.
const MANIFEST_PREFIX: &str = "appmanifest_";
/// See [`MANIFEST_PREFIX`].
const MANIFEST_SUFFIX: &str = ".acf";

/// Steam's local installation, and what it says is installed.
///
/// Read once and then asked questions; nothing here watches for changes, so a
/// caller that wants to notice a game installed since start-up reads it again.
/// Reading is a few dozen small files and takes milliseconds.
#[derive(Debug)]
pub struct Steam {
    root: PathBuf,
    libraries: Vec<PathBuf>,
    apps: Vec<SteamApp>,
    problems: Vec<SteamError>,
}

/// One application Steam has a manifest for.
///
/// "Application" rather than "game" on purpose: Steam manages redistributables,
/// tools and soundtracks with the same file, and this type reports what the file
/// says rather than deciding which of them somebody would want recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteamApp {
    app_id: String,
    name: String,
    installation: PathBuf,
    library: PathBuf,
    manifest: PathBuf,
    icon: Option<PathBuf>,
}

impl SteamApp {
    /// Steam's identifier for the application, as it is written in the manifest.
    ///
    /// A string rather than a number because that is what it is everywhere it is
    /// used: in the file, in the catalogue's `app_id`, in a `steam://` URL. It
    /// is [`crate::catalogue`]'s launcher identifier, and the two are compared
    /// as text.
    #[must_use]
    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    /// What Steam calls the application.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Where the application is installed.
    ///
    /// `<library>\steamapps\common\<installdir>`, which is where Steam puts it.
    /// The directory may not exist yet: a manifest appears when a download
    /// starts, not when it finishes.
    #[must_use]
    pub fn installation_directory(&self) -> &Path {
        &self.installation
    }

    /// The library this application belongs to.
    #[must_use]
    pub fn library(&self) -> &Path {
        &self.library
    }

    /// The manifest this was read from.
    #[must_use]
    pub fn manifest(&self) -> &Path {
        &self.manifest
    }

    /// A picture of the game Steam has already downloaded, if there is one.
    ///
    /// This is a **file on this machine** and never a network fetch: everything
    /// here is something Steam put in `appcache\librarycache`.
    ///
    /// The application icon when Steam has cached one, which it usually has —
    /// 511 of the 660 cached applications on the machine this was developed
    /// against. It is a 32x32 JPEG in the application's own cache directory,
    /// named for its SHA-1, so it is found by being that shape rather than by
    /// its name; [`icon`] is the module that explains why, and why no
    /// `appinfo.vdf` is needed to do it.
    ///
    /// Otherwise the artwork Steam shows in the library — the portrait capsule,
    /// the header, the logo — which is not an icon and is visibly not one from
    /// its file name, but is better than nothing.
    ///
    /// `None` means Steam has cached nothing at all for this application, which
    /// is ordinary for a game installed but never shown in the library.
    #[must_use]
    pub fn icon(&self) -> Option<&Path> {
        self.icon.as_deref()
    }
}

impl Steam {
    /// Finds Steam on this machine and reads it.
    ///
    /// `Ok(None)` when Steam is not installed — no registry entry, or one
    /// pointing at a directory that is no longer there. That is not an error: a
    /// machine with no Steam on it is a machine Clipped runs perfectly well on,
    /// and the caller's response is to detect nothing rather than to tell the
    /// user something is wrong (issue #43).
    ///
    /// # Errors
    ///
    /// [`SteamError`] when Steam is installed and something about reading it
    /// failed: the registry refused, or the library index is unreadable or is
    /// not KeyValues. A manifest that cannot be read is a
    /// [`problem`](Self::problems) rather than a failure.
    #[cfg(windows)]
    pub fn discover() -> Result<Option<Self>, SteamError> {
        Self::at_registry_path(registry::steam_path()?)
    }

    /// [`Self::discover`] with the registry already read.
    ///
    /// The split is what makes "Steam is not installed" testable on a machine
    /// that has Steam installed: both shapes of absence are decided here, and
    /// neither of them needs a registry.
    #[cfg(windows)]
    fn at_registry_path(path: Option<PathBuf>) -> Result<Option<Self>, SteamError> {
        let Some(root) = path else {
            debug!("Steam is not installed: no path in the registry");
            return Ok(None);
        };
        if !root.is_dir() {
            // Uninstalling Steam leaves the registry entry behind, so this is
            // the ordinary shape of "it was here once" rather than a fault.
            debug!(
                path = %RedactedPath::new(&root),
                "the registry names a Steam directory that is not there"
            );
            return Ok(None);
        }
        Self::read_at(&root).map(Some)
    }

    /// Reads the Steam installation in a named directory.
    ///
    /// What tests use, and what a caller that already knows where Steam is —
    /// because the user said so, or because [`Self::discover`] found it — uses
    /// to read it again.
    ///
    /// # Errors
    ///
    /// [`SteamError::MissingRoot`] if `root` is not a directory, and the errors
    /// [`Self::discover`] documents.
    pub fn read_at(root: impl AsRef<Path>) -> Result<Self, SteamError> {
        let root = root.as_ref();
        if !root.is_dir() {
            return Err(SteamError::MissingRoot {
                path: root.to_path_buf(),
            });
        }

        let mut problems = Vec::new();
        let libraries = libraries(root, &mut problems)?;

        let mut apps = Vec::new();
        for library in &libraries {
            apps.extend(read_library(root, library, &mut problems));
        }

        debug!(
            libraries = libraries.len(),
            apps = apps.len(),
            problems = problems.len(),
            root = %RedactedPath::new(root),
            "read the local Steam installation"
        );

        Ok(Self {
            root: root.to_path_buf(),
            libraries,
            apps,
            problems,
        })
    }

    /// Where Steam itself is installed.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Every library folder, the default one first.
    #[must_use]
    pub fn libraries(&self) -> &[PathBuf] {
        &self.libraries
    }

    /// Every application Steam has a manifest for, by library and then by
    /// identifier.
    ///
    /// Sorted rather than left in directory order so that two reads of an
    /// unchanged installation agree, which is what lets a caller diff them.
    #[must_use]
    pub fn apps(&self) -> &[SteamApp] {
        &self.apps
    }

    /// The application with this identifier, if Steam has one installed.
    #[must_use]
    pub fn app_by_id(&self, app_id: &str) -> Option<&SteamApp> {
        self.apps
            .iter()
            .find(|app| app.app_id.eq_ignore_ascii_case(app_id))
    }

    /// Which application, if any, an executable belongs to.
    ///
    /// The image path has to be *inside* an application's installation
    /// directory, compared as whole directory names in the same way
    /// [`crate::catalogue`] compares its path qualifiers — so a game in
    /// `common\Portal` is not found in `common\Portal 2`, and a path that is the
    /// installation directory itself is not a file in it.
    ///
    /// Where two applications' directories nest, the innermost answers. That is
    /// not hypothetical: a mod or a tool installed inside another game's
    /// directory would otherwise be reported as that game.
    #[must_use]
    pub fn app_for_path(&self, executable_path: &str) -> Option<&SteamApp> {
        let normalised = normalise_path(executable_path);
        let path: Vec<&str> = path_segments(&normalised).collect();

        let mut best: Option<(&SteamApp, usize)> = None;
        for app in &self.apps {
            let normalised = normalise_path(&app.installation.to_string_lossy());
            let directory: Vec<&str> = path_segments(&normalised).collect();
            // Longer than, not at least as long as: the last segment of the
            // path is the executable's file name, so a path equal to the
            // installation directory is a directory and not a program in it.
            if directory.is_empty() || path.len() <= directory.len() {
                continue;
            }
            if path.starts_with(&directory) && best.is_none_or(|(_, depth)| depth < directory.len())
            {
                best = Some((app, directory.len()));
            }
        }
        best.map(|(app, _)| app)
    }

    /// A running process as the catalogue wants to be asked about it.
    ///
    /// The launcher identity is attached when Steam claims the path, and left
    /// off when it does not — an absent identity is what makes the catalogue
    /// fall back to its path and name rungs, so a game Steam does not know about
    /// is matched exactly as well as it was before.
    ///
    /// This is the join issue #43 exists to produce. Deciding what to *do* about
    /// the answer belongs to `clipped_session`
    /// ([#46](https://github.com/wildware-uk/clipped/issues/46)), which is why
    /// this returns a candidate rather than starting anything.
    #[must_use]
    pub fn candidate_for<'a>(
        &'a self,
        executable_name: &'a str,
        executable_path: &'a str,
    ) -> ProcessCandidate<'a> {
        let candidate = ProcessCandidate::new(executable_name).with_path(executable_path);
        match self.app_for_path(executable_path) {
            Some(app) => candidate.from_launcher(LauncherKind::Steam, &app.app_id),
            None => candidate,
        }
    }

    /// Files that could not be read, each naming itself.
    ///
    /// Empty on a healthy installation. A non-empty list means detection is
    /// working with less than everything Steam has — a library on a drive that
    /// is not plugged in, a manifest half-written by an interrupted update — and
    /// a diagnostics screen should say so rather than leave somebody wondering
    /// why one game is never detected. Every one of these is also logged at
    /// `warn` when it is found, with the path redacted; a screen showing the
    /// user their own disk reads [`SteamError::path`] for the whole thing.
    #[must_use]
    pub fn problems(&self) -> &[SteamError] {
        &self.problems
    }
}

/// Every library folder of an installation, the default one first.
fn libraries(root: &Path, problems: &mut Vec<SteamError>) -> Result<Vec<PathBuf>, SteamError> {
    let mut libraries = vec![root.to_path_buf()];
    let mut seen = vec![normalise_path(&root.to_string_lossy())];

    let Some((path, text)) = library_index(root)? else {
        // A client installed and never run has no index, and its own directory
        // is the only library there is. Not a problem: nothing is missing.
        debug!(
            root = %RedactedPath::new(root),
            "Steam has no library index; reading the default library only"
        );
        return Ok(libraries);
    };

    let document = keyvalues::parse(&text).map_err(|error| SteamError::Syntax {
        path: path.clone(),
        message: error.to_string(),
    })?;
    let folders = document
        .table("libraryfolders")
        .ok_or_else(|| SteamError::Shape {
            path: path.clone(),
            missing: "`libraryfolders` table".to_owned(),
        })?;

    for (key, value) in folders.entries() {
        let Value::Table(folder) = value else {
            // Steam wrote the path as a bare value in clients old enough that
            // nobody is running one. Say so rather than skip it, because a
            // library silently missing is a game that silently never records.
            problems.push(report(SteamError::Shape {
                path: path.clone(),
                missing: format!("a table for library `{key}`"),
            }));
            continue;
        };
        let Some(folder_path) = folder.string("path") else {
            problems.push(report(SteamError::Shape {
                path: path.clone(),
                missing: format!("a `path` for library `{key}`"),
            }));
            continue;
        };

        let normalised = normalise_path(folder_path);
        if seen.iter().any(|existing| existing == &normalised) {
            // Steam lists its own directory in the index, so this is the usual
            // case rather than a corrupt file.
            continue;
        }
        seen.push(normalised);
        libraries.push(PathBuf::from(folder_path));
    }

    Ok(libraries)
}

/// The library index and where it was found, or `None` if there is not one.
fn library_index(root: &Path) -> Result<Option<(PathBuf, String)>, SteamError> {
    for candidate in LIBRARY_INDEX_FILES {
        let path = join(root, candidate);
        match fs::read_to_string(&path) {
            Ok(text) => return Ok(Some((path, text))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(SteamError::Read { path, source }),
        }
    }
    Ok(None)
}

/// Every application manifest in one library, in identifier order.
fn read_library(root: &Path, library: &Path, problems: &mut Vec<SteamError>) -> Vec<SteamApp> {
    let directory = library.join(MANIFEST_DIRECTORY);
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(source) => {
            // The ordinary cause is a library on a drive that is not plugged in.
            // Detection carries on with the libraries that are.
            problems.push(report(SteamError::Library {
                path: library.to_path_buf(),
                source,
            }));
            return Vec::new();
        }
    };

    let mut apps = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(source) => {
                problems.push(report(SteamError::Library {
                    path: library.to_path_buf(),
                    source,
                }));
                continue;
            }
        };

        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let name = name.to_ascii_lowercase();
        if !name.starts_with(MANIFEST_PREFIX) || !name.ends_with(MANIFEST_SUFFIX) {
            continue;
        }

        match read_manifest(root, library, &entry.path()) {
            Ok(app) => apps.push(app),
            Err(error) => problems.push(report(error)),
        }
    }

    // Directory order is whatever the filesystem happened to give, and two
    // reads of the same installation should agree.
    apps.sort_by(|left, right| {
        numeric(&left.app_id)
            .cmp(&numeric(&right.app_id))
            .then_with(|| left.app_id.cmp(&right.app_id))
    });
    apps
}

/// Logs a problem that detection carried on past, and hands it back to be
/// collected.
///
/// Both, always: the log is for the machine that has already failed, and
/// [`Steam::problems`] is for a caller that wants to tell the user why one game
/// is missing (AGENTS.md sections 15 and 45).
///
/// What reaches the log is [`SteamError`]'s `Display`, which names the file and
/// redacts the directories above it — the account name and the folders somebody
/// chose have no business in a file users send to strangers (AGENTS.md section
/// 13). A caller showing the user their own disk reads
/// [`SteamError::path`] instead.
fn report(problem: SteamError) -> SteamError {
    warn!(%problem, "part of the local Steam installation could not be read");
    problem
}

/// An application identifier as a number, for ordering only.
///
/// Anything unparseable sorts last rather than first, so a file with something
/// odd in it does not push itself to the top of a list a person reads.
fn numeric(app_id: &str) -> u64 {
    app_id.parse().unwrap_or(u64::MAX)
}

/// One application manifest.
fn read_manifest(root: &Path, library: &Path, path: &Path) -> Result<SteamApp, SteamError> {
    let text = fs::read_to_string(path).map_err(|source| SteamError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let document = keyvalues::parse(&text).map_err(|error| SteamError::Syntax {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;

    let state = document
        .table("AppState")
        .ok_or_else(|| missing(path, "`AppState` table"))?;
    let app_id = non_empty(state.string("appid")).ok_or_else(|| missing(path, "`appid`"))?;
    let name = non_empty(state.string("name")).ok_or_else(|| missing(path, "`name`"))?;
    let install_dir =
        non_empty(state.string("installdir")).ok_or_else(|| missing(path, "`installdir`"))?;
    let installation = install_path(library, install_dir)
        .ok_or_else(|| missing(path, "`installdir` that stays inside its own library"))?;

    Ok(SteamApp {
        icon: icon::icon(root, app_id),
        app_id: app_id.to_owned(),
        name: name.to_owned(),
        installation,
        library: library.to_path_buf(),
        manifest: path.to_path_buf(),
    })
}

/// Where a manifest's `installdir` puts an application, if it puts it inside the
/// library at all.
///
/// `installdir` is a value out of a file Clipped did not write, and joining it
/// onto the library unchecked hands it more authority than any other field in
/// the manifest. [`Steam::app_for_path`] claims every executable beneath an
/// installation directory, and the catalogue believes a launcher identity above
/// every other rung, so `"installdir" "C:\\Windows\\System32"` — or the same
/// thing spelled `..\..\..\..\Windows\System32` — would make Clipped record
/// Notepad, or the user's browser, as whatever game the manifest named, and
/// record it with more confidence than it has in a game it recognised properly.
/// That is a malformed or hostile manifest choosing what Clipped points a
/// recorder at.
///
/// So the value has to be a relative path built out of ordinary names: no drive
/// or share, no leading separator, no `.` or `..`, and nothing empty. Both
/// separators are treated as separators regardless of platform, because Steam
/// writes `\` and this must not depend on which operating system is reading it.
///
/// More than one component is still allowed. All eighty-eight manifests on the
/// machine this was developed against name a single directory, but a nested
/// `installdir` escapes nothing, and refusing one would be a rule stricter than
/// the reason for having it.
///
/// `None` is reported by [`read_manifest`] as a problem naming the manifest, so
/// a value this refuses is visible rather than silently dropped.
fn install_path(library: &Path, install_dir: &str) -> Option<PathBuf> {
    let mut path = join(library, INSTALL_DIRECTORY);
    let mut components = 0_usize;

    for component in install_dir.split(['/', '\\']) {
        let ordinary = !component.is_empty()
            && component != "."
            && component != ".."
            // A drive letter or an alternate data stream: `C:` and `C:\Windows`
            // both reach here as components carrying a colon.
            && !component.contains(':');
        if !ordinary {
            return None;
        }
        path.push(component);
        components += 1;
    }

    (components > 0).then_some(path)
}

/// A value Steam wrote, unless it wrote an empty one.
fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

/// A [`SteamError::Shape`] about `path`.
fn missing(path: &Path, what: &str) -> SteamError {
    SteamError::Shape {
        path: path.to_path_buf(),
        missing: what.to_owned(),
    }
}

/// Joins a `/`-separated relative path onto a directory.
///
/// The constants above are written with one separator so they read as the paths
/// they are; this is what turns them into real ones.
fn join(root: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .fold(root.to_path_buf(), |path, segment| path.join(segment))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Scratch;

    /// An empty directory of one test's own, under the system temporary
    /// directory, removed again when the test that made it passes.
    ///
    /// This used to return a bare [`PathBuf`] and nothing ever removed it,
    /// which is where 1,578 of the `clipped-steam-*` directories counted in
    /// [issue #598](https://github.com/wildware-uk/clipped/issues/598) came
    /// from. See [`Scratch`] for what the returned value does and how to hold
    /// it.
    pub(super) fn scratch(label: &str) -> Scratch {
        Scratch::new(&format!("steam-{label}"))
    }

    /// The library an `installdir` is resolved against in these tests.
    const LIBRARY: &str = "C:/SteamLibrary";

    /// Where an ordinary `installdir` lands.
    fn installed(install_dir: &str) -> Option<PathBuf> {
        install_path(Path::new(LIBRARY), install_dir)
    }

    #[test]
    fn an_ordinary_install_directory_lands_under_the_library() {
        assert_eq!(
            installed("Counter-Strike Global Offensive"),
            Some(
                Path::new(LIBRARY)
                    .join("steamapps")
                    .join("common")
                    .join("Counter-Strike Global Offensive")
            )
        );
    }

    #[test]
    fn a_nested_install_directory_is_still_inside_the_library() {
        // Steam writes a single name in all eighty-eight manifests on the
        // machine this was developed against, but a nested one escapes nothing
        // and refusing it would be stricter than the reason for the rule.
        assert_eq!(
            installed(r"Some Game\bin"),
            Some(
                Path::new(LIBRARY)
                    .join("steamapps")
                    .join("common")
                    .join("Some Game")
                    .join("bin")
            )
        );
    }

    /// The one that matters: an `installdir` that leaves the library would make
    /// [`Steam::app_for_path`] claim unrelated executables, and the catalogue
    /// believes a launcher identity above every other rung.
    #[test]
    fn an_install_directory_that_leaves_the_library_is_refused() {
        for escape in [
            r"..\..\..\Windows\System32",
            "../../../Windows/System32",
            r"C:\Windows\System32",
            "/Windows/System32",
            r"\\server\share\game",
            "..",
            ".",
            r"Some Game\..\..\..\Windows",
            "",
            r"Some Game\",
            "C:",
        ] {
            assert_eq!(
                installed(escape),
                None,
                "`{escape}` should not be accepted as an install directory"
            );
        }
    }

    #[test]
    fn a_relative_path_written_with_slashes_becomes_a_real_one() {
        let joined = join(Path::new("C:/Steam"), "steamapps/common");
        assert_eq!(
            joined,
            Path::new("C:/Steam").join("steamapps").join("common")
        );
    }

    #[test]
    fn an_identifier_that_is_not_a_number_sorts_last() {
        assert!(numeric("730") < numeric("not a number"));
    }

    #[test]
    fn an_empty_value_is_not_a_value() {
        assert_eq!(non_empty(Some("  ")), None);
        assert_eq!(non_empty(Some("730")), Some("730"));
        assert_eq!(non_empty(None), None);
    }

    /// A machine with no Steam on it is a machine Clipped runs on, so neither
    /// shape of "not installed" is a failure (issue #43). The two shapes are the
    /// registry saying nothing, and the registry naming a directory that an
    /// uninstall took away — the entry survives it.
    #[cfg(windows)]
    #[test]
    fn a_machine_with_no_steam_on_it_is_answered_rather_than_refused() {
        assert!(Steam::at_registry_path(None)
            .expect("no Steam is not an error")
            .is_none());

        // Named inside a directory of this test's own rather than straight
        // under `%TEMP%`: a path that has to *not* exist used to be removed on
        // the way in, and removing something the test did not create is how a
        // suite deletes another run's files (issue #598).
        let directory = scratch("no-steam");
        let uninstalled = directory.join("absent");
        assert!(Steam::at_registry_path(Some(uninstalled))
            .expect("a registry entry left behind by an uninstall is not an error")
            .is_none());
    }
}
