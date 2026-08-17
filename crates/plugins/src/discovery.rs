//! Finding plugins on disk, and what has to be true before one may run.
//!
//! A plugin is a directory: a `plugin.json` and the executable it names.
//!
//! ```text
//! plugins/
//!     counter-strike-2/
//!         plugin.json
//!         clipped-cs2-plugin.exe
//!     acme-dota/
//!         plugin.json
//!         acme-dota.exe
//! ```
//!
//! # Nothing is skipped silently
//!
//! A directory that is not a plugin is reported as [`RejectedPlugin`] with the
//! reason, not passed over. A user who has dropped a plugin into that folder
//! and cannot see it needs to be told that its manifest names an executable
//! that is not there, and a maintainer reading a bug report needs the same
//! sentence (AGENTS.md section 15). Discovery therefore always returns both
//! lists and never an error.
//!
//! # Consent is a type, not a check
//!
//! [`InstalledPlugin`] cannot be started. [`EnabledPlugin`] can, and the only
//! way to get one is [`InstalledPlugin::enable`], which takes the
//! [`ConsentToken`] the user's consent was recorded as and refuses it if the
//! plugin's declaration has changed since (`crate::network`). `docs/privacy.md`
//! requires that "if an update changes the declaration … the consent lapses and
//! the user is asked again before the plugin runs"; making the enabled plugin a
//! separate type is how that stops being a rule somebody has to remember to
//! check.

use core::fmt;
use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::manifest::{ManifestError, ObservedProcess, PluginId, PluginManifest};
use crate::network::ConsentToken;

/// The file a plugin describes itself in.
pub const MANIFEST_FILE: &str = "plugin.json";

/// The most a manifest may weigh.
///
/// A manifest is a dozen short fields. Anything larger is a mistake or an
/// attempt at one, and reading it into memory to find out is the mistake this
/// avoids.
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;

/// What was found in a plugins directory.
#[derive(Debug, Default)]
pub struct Discovery {
    /// The plugins that could be read, in directory order.
    pub installed: Vec<InstalledPlugin>,
    /// The directories that could not be, and why.
    pub rejected: Vec<RejectedPlugin>,
}

/// Reads every plugin under `root`.
///
/// A `root` that does not exist is not an error: it is a machine with no
/// plugins installed, which is every machine until somebody installs one.
///
/// Directories are read in sorted order so that two runs of the same machine
/// produce the same list, and so that a duplicate identifier is resolved the
/// same way twice.
#[must_use]
pub fn discover(root: &Path) -> Discovery {
    let mut discovery = Discovery::default();

    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            tracing::debug!(root = %root.display(), "no plugins directory, so no plugins");
            return discovery;
        }
        Err(error) => {
            discovery.rejected.push(RejectedPlugin {
                directory: root.to_path_buf(),
                reason: Rejection::Unreadable {
                    because: error.to_string(),
                },
            });
            return discovery;
        }
    };

    let mut directories: Vec<PathBuf> = entries
        .filter_map(|entry| match entry {
            Ok(entry) => entry.path().is_dir().then(|| entry.path()),
            Err(error) => {
                discovery.rejected.push(RejectedPlugin {
                    directory: root.to_path_buf(),
                    reason: Rejection::Unreadable {
                        because: error.to_string(),
                    },
                });
                None
            }
        })
        .collect();
    directories.sort();

    let mut claimed: HashSet<String> = HashSet::new();
    for directory in directories {
        match read_plugin(&directory) {
            Ok(plugin) => {
                if claimed.insert(plugin.id().as_str().to_owned()) {
                    discovery.installed.push(plugin);
                } else {
                    // Two plugins under one identifier would produce two sets
                    // of events attributed to the same name, and a user
                    // disabling one would have no way to tell which.
                    discovery.rejected.push(RejectedPlugin {
                        directory,
                        reason: Rejection::DuplicateId {
                            id: plugin.id().clone(),
                        },
                    });
                }
            }
            Err(reason) => discovery
                .rejected
                .push(RejectedPlugin { directory, reason }),
        }
    }

    discovery
}

/// Reads one plugin directory.
fn read_plugin(directory: &Path) -> Result<InstalledPlugin, Rejection> {
    let path = directory.join(MANIFEST_FILE);
    let metadata = fs::metadata(&path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            Rejection::NoManifest
        } else {
            Rejection::Unreadable {
                because: error.to_string(),
            }
        }
    })?;
    if metadata.len() > MAX_MANIFEST_BYTES {
        return Err(Rejection::oversize(metadata.len()));
    }

    let json = fs::read_to_string(&path).map_err(|error| Rejection::Unreadable {
        because: error.to_string(),
    })?;
    let manifest = PluginManifest::parse(&json).map_err(|source| Rejection::Manifest { source })?;

    let executable = directory.join(manifest.executable());
    if !executable.is_file() {
        return Err(Rejection::ExecutableMissing { path: executable });
    }

    Ok(InstalledPlugin {
        manifest,
        directory: directory.to_path_buf(),
        executable,
    })
}

/// A plugin that was read, and that the user has not enabled.
///
/// It cannot be started. Everything about it can be shown: its name, what it
/// says it does, and every sentence of its network declaration.
#[derive(Debug, Clone)]
pub struct InstalledPlugin {
    manifest: PluginManifest,
    directory: PathBuf,
    executable: PathBuf,
}

impl InstalledPlugin {
    /// What it declares.
    #[must_use]
    pub const fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    /// Who it is.
    #[must_use]
    pub const fn id(&self) -> &PluginId {
        self.manifest.id()
    }

    /// Where it lives. Also the working directory it is run in, so that a
    /// plugin can keep its own files beside itself.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// The executable, resolved inside [`directory`](Self::directory).
    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Whether it handles `process`.
    ///
    /// This is SPEC.md section 22's `supports(process)`, and it is answered
    /// from the manifest rather than by asking the plugin: starting a process
    /// to ask whether it cares about Notepad would mean every launch on the
    /// machine starting every installed plugin, and a question that can be
    /// answered before anything runs is one the user can also see the answer
    /// to.
    #[must_use]
    pub fn supports(&self, process: &ObservedProcess) -> bool {
        self.manifest.supports().matches(process)
    }

    /// The consent token for what it declares **now** — network and
    /// filesystem together.
    ///
    /// Show the declaration
    /// ([`NetworkAccess::summary`](crate::NetworkAccess::summary),
    /// [`FilesystemAccess::summary`](crate::FilesystemAccess::summary)), and
    /// store this alongside the fact that the user enabled it.
    #[must_use]
    pub fn consent_token(&self) -> ConsentToken {
        self.manifest.consent_token()
    }

    /// Turns a plugin the user has allowed into one that may be started.
    ///
    /// # Errors
    ///
    /// [`ConsentLapsed`] when `consented_to` is not what the plugin declares
    /// now — an updated plugin that has added an endpoint, changed a loopback
    /// listener into an outbound connection, or asked for the network where it
    /// previously asked for nothing. The user is asked again; the plugin does
    /// not run in the meantime.
    pub fn enable(self, consented_to: &ConsentToken) -> Result<EnabledPlugin, ConsentLapsed> {
        let declared = self.consent_token();
        if &declared != consented_to {
            return Err(ConsentLapsed {
                plugin: self.id().clone(),
                consented_to: consented_to.clone(),
                now_declares: declared,
            });
        }
        Ok(EnabledPlugin { installed: self })
    }
}

/// A plugin the user has enabled, which is the only kind that can be started.
#[derive(Debug, Clone)]
pub struct EnabledPlugin {
    installed: InstalledPlugin,
}

impl EnabledPlugin {
    /// What was enabled.
    #[must_use]
    pub const fn installed(&self) -> &InstalledPlugin {
        &self.installed
    }

    /// Who it is.
    #[must_use]
    pub const fn id(&self) -> &PluginId {
        self.installed.id()
    }

    /// Whether it handles `process`.
    #[must_use]
    pub fn supports(&self, process: &ObservedProcess) -> bool {
        self.installed.supports(process)
    }
}

/// A directory under the plugins folder that is not a usable plugin.
#[derive(Debug)]
pub struct RejectedPlugin {
    /// Where it is, so the message can name it.
    pub directory: PathBuf,
    /// Why it was refused.
    pub reason: Rejection,
}

impl fmt::Display for RejectedPlugin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.directory.display(), self.reason)
    }
}

/// Why a directory is not a usable plugin.
#[derive(Debug)]
pub enum Rejection {
    /// There is no `plugin.json` in it.
    NoManifest,
    /// Something on the way to reading it failed.
    Unreadable {
        /// What the operating system said, rendered so that this type stays
        /// comparable and printable.
        because: String,
    },
    /// The manifest is not one this build can use.
    Manifest {
        /// Which rule it broke.
        source: ManifestError,
    },
    /// The manifest names an executable that is not beside it.
    ExecutableMissing {
        /// Where it was looked for.
        path: PathBuf,
    },
    /// Another plugin already claims this identifier.
    DuplicateId {
        /// The identifier claimed twice.
        id: PluginId,
    },
}

impl Rejection {
    fn oversize(bytes: u64) -> Self {
        Self::Unreadable {
            because: format!(
                "plugin.json is {bytes} bytes, and a manifest is at most {MAX_MANIFEST_BYTES}"
            ),
        }
    }
}

impl fmt::Display for Rejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoManifest => write!(
                formatter,
                "there is no {MANIFEST_FILE} here, so this is not a plugin"
            ),
            Self::Unreadable { because } => write!(formatter, "{because}"),
            Self::Manifest { source } => write!(formatter, "{source}"),
            Self::ExecutableMissing { path } => write!(
                formatter,
                "{MANIFEST_FILE} names an executable that is not there: {}",
                path.display()
            ),
            Self::DuplicateId { id } => write!(
                formatter,
                "another plugin is already installed as `{id}`, and two plugins cannot share an \
                 identifier: every event either of them reported would be attributed to the same \
                 name"
            ),
        }
    }
}

impl core::error::Error for Rejection {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Manifest { source } => Some(source),
            _ => None,
        }
    }
}

/// A plugin whose network declaration is not the one the user allowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentLapsed {
    /// Which plugin.
    pub plugin: PluginId,
    /// What the user allowed.
    pub consented_to: ConsentToken,
    /// What it declares now.
    pub now_declares: ConsentToken,
}

impl fmt::Display for ConsentLapsed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "`{}` now declares different network access from the one it was allowed \
             (allowed: {}; declares: {}), so it needs to be allowed again before it runs",
            self.plugin, self.consented_to, self.now_declares
        )
    }
}

impl core::error::Error for ConsentLapsed {}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;
    use crate::fixture::TemporaryDirectory;
    use crate::manifest::tests::EXAMPLE;

    fn example_named(id: &str) -> String {
        EXAMPLE.replace("counter-strike-2", id)
    }

    #[test]
    fn a_plugin_is_a_manifest_and_the_executable_it_names() {
        let root = TemporaryDirectory::new("found");
        root.install("cs2", EXAMPLE, Some("clipped-cs2-plugin.exe"));

        let discovery = discover(root.path());
        assert!(discovery.rejected.is_empty(), "{:?}", discovery.rejected);
        assert_eq!(discovery.installed.len(), 1);

        let plugin = &discovery.installed[0];
        assert_eq!(plugin.id().as_str(), "counter-strike-2");
        assert!(plugin.supports(&ObservedProcess::new("cs2.exe", 12)));
        assert!(!plugin.supports(&ObservedProcess::new("notepad.exe", 12)));
        assert_eq!(
            plugin.executable(),
            root.path().join("cs2").join("clipped-cs2-plugin.exe")
        );
    }

    #[test]
    fn a_machine_with_no_plugins_directory_has_no_plugins_and_no_complaints() {
        let discovery = discover(Path::new("Z:/no/such/plugins/directory"));
        assert!(discovery.installed.is_empty());
        assert!(discovery.rejected.is_empty());
    }

    #[test]
    fn nothing_is_skipped_without_a_reason() {
        let root = TemporaryDirectory::new("rejected");
        root.install("no-manifest-here", "", None);
        fs::remove_file(root.path().join("no-manifest-here").join(MANIFEST_FILE))
            .expect("the manifest can be removed again");
        root.install("broken", "{ not json", Some("x.exe"));
        root.install("no-executable", EXAMPLE, None);

        let discovery = discover(root.path());
        assert!(discovery.installed.is_empty());
        assert_eq!(discovery.rejected.len(), 3);
        for rejected in &discovery.rejected {
            let said = rejected.to_string();
            assert!(
                said.len() > 30,
                "a rejection has to say enough to act on: {said}"
            );
        }
        assert!(discovery
            .rejected
            .iter()
            .any(|rejected| matches!(rejected.reason, Rejection::NoManifest)));
        assert!(discovery
            .rejected
            .iter()
            .any(|rejected| matches!(rejected.reason, Rejection::ExecutableMissing { .. })));
        assert!(discovery
            .rejected
            .iter()
            .any(|rejected| matches!(rejected.reason, Rejection::Manifest { .. })));
    }

    #[test]
    fn two_plugins_cannot_share_an_identifier() {
        let root = TemporaryDirectory::new("duplicate");
        root.install("a-first", EXAMPLE, Some("clipped-cs2-plugin.exe"));
        root.install("b-second", EXAMPLE, Some("clipped-cs2-plugin.exe"));

        let discovery = discover(root.path());
        assert_eq!(discovery.installed.len(), 1);
        assert_eq!(
            discovery.installed[0].directory(),
            root.path().join("a-first"),
            "the first in sorted order keeps the identifier, so two runs agree"
        );
        assert!(matches!(
            discovery.rejected[0].reason,
            Rejection::DuplicateId { .. }
        ));
    }

    #[test]
    fn several_plugins_are_read_in_a_stable_order() {
        let root = TemporaryDirectory::new("order");
        root.install(
            "z-last",
            &example_named("zebra"),
            Some("clipped-cs2-plugin.exe"),
        );
        root.install(
            "a-first",
            &example_named("acme"),
            Some("clipped-cs2-plugin.exe"),
        );

        let ids: Vec<String> = discover(root.path())
            .installed
            .iter()
            .map(|plugin| plugin.id().as_str().to_owned())
            .collect();
        assert_eq!(ids, vec!["acme".to_owned(), "zebra".to_owned()]);
    }

    #[test]
    fn a_plugin_can_only_be_started_with_consent_to_what_it_declares_now() {
        let root = TemporaryDirectory::new("consent");
        root.install("cs2", EXAMPLE, Some("clipped-cs2-plugin.exe"));
        let plugin = discover(root.path()).installed.remove(0);

        let allowed = plugin.consent_token();
        assert!(plugin.clone().enable(&allowed).is_ok());

        // The plugin updates, and now wants to talk to the internet as well.
        let updated = EXAMPLE.replace(
            r#""purpose": "receives Counter-Strike 2 game state""#,
            r#""purpose": "receives Counter-Strike 2 game state"},
               {"class":"outbound","direction":"connect","endpoint":"stats.example.com:443",
                "purpose":"uploads match summaries""#,
        );
        root.install("cs2", &updated, Some("clipped-cs2-plugin.exe"));
        let updated = discover(root.path()).installed.remove(0);
        assert!(updated.manifest().network().leaves_the_machine());

        let lapsed = updated
            .enable(&allowed)
            .expect_err("consent to the old declaration is not consent to this one");
        assert_eq!(lapsed.consented_to, allowed);
        assert!(
            lapsed.to_string().contains("allowed again"),
            "the message should say what happens next: {lapsed}"
        );
    }

    #[test]
    fn a_plugin_that_starts_writing_into_a_game_directory_needs_consent_again() {
        // The filesystem half of the same property: a plugin that starts
        // declaring it will write into the game's own directory — the gap
        // #343 exists to close — is not started on consent to the version of
        // it that made no such claim.
        let root = TemporaryDirectory::new("consent-filesystem");
        root.install("cs2fs", EXAMPLE, Some("clipped-cs2-plugin.exe"));
        let plugin = discover(root.path()).installed.remove(0);

        let allowed = plugin.consent_token();
        assert!(plugin.clone().enable(&allowed).is_ok());
        assert!(
            plugin.manifest().filesystem().is_empty(),
            "the baseline declares no filesystem access"
        );

        // The plugin updates, and now writes into Counter-Strike 2's own
        // installation directory as well as talking to the network it already
        // declared.
        let mut document: Value = serde_json::from_str(EXAMPLE).expect("EXAMPLE parses");
        document.as_object_mut().expect("an object").insert(
            "filesystem".to_owned(),
            serde_json::json!([{
                "scope": "game-installation",
                "access": "write",
                "purpose": "writes the Game State Integration configuration"
            }]),
        );
        root.install(
            "cs2fs",
            &document.to_string(),
            Some("clipped-cs2-plugin.exe"),
        );
        let updated = discover(root.path()).installed.remove(0);
        assert!(!updated.manifest().filesystem().is_empty());

        let lapsed = updated
            .enable(&allowed)
            .expect_err("consent to a plugin that touched no files is not consent to one that now writes into a game");
        assert_eq!(lapsed.consented_to, allowed);
    }
}
