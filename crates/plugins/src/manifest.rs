//! What a plugin says about itself, before any of it runs.
//!
//! A manifest is `plugin.json` beside a plugin's executable. It is read by the
//! host, shown to the user and never interpreted by the plugin, and it answers
//! three questions that all have to be answerable *without* starting anything:
//!
//! - **Who is this?** [`PluginId`], which is also the `source` every event it
//!   reports is stamped with, so a mark on a timeline is traceable to the
//!   plugin that made it and to nothing else.
//! - **Does it care about this game?** [`Supports`], matched against a launched
//!   process. Answering from the manifest rather than from running code is the
//!   difference between reading a file and starting a process every time
//!   anything at all is launched.
//! - **What will it do with the network?** [`NetworkAccess`], which the user
//!   sees and consents to before the plugin is ever started (`crate::network`).
//!
//! # Why a manifest this build does not fully understand is refused
//!
//! `crates/events` never refuses to read a stored event, because a refusal
//! there destroys something a user cannot get back. A manifest is the opposite
//! case, so it takes the opposite rule: **an unknown field, or a contract
//! version this build does not speak, refuses the whole manifest**
//! ([`ManifestError`]).
//!
//! The asymmetry is the point. A manifest is a permission document. A build
//! that ignored a field it had not learned would run a plugin under a narrower
//! declaration than the plugin was written to — the user consenting to the part
//! of it this build happened to understand — and that is precisely the failure
//! `docs/privacy.md` exists to prevent. Refusing costs a plugin that does not
//! run and a message naming the version needed; ignoring costs a permission
//! nobody granted.

use core::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use clipped_events::{EventSource, InvalidSource};

use crate::network::{NetworkAccess, NetworkDeclarationError};

/// The contract version this build speaks.
///
/// This is the version of the *plugin contract* — the manifest's shape, the
/// wire a plugin reports over, and the lifecycle around it — and it is
/// deliberately separate from `clipped_events::SchemaVersion`, which versions
/// the events themselves. A stored event outlives every build that reads it; a
/// running plugin is negotiated with once, at start-up. Tying the two together
/// would mean a plugin that added a wire message forcing a migration of every
/// event in a user's library.
pub const CONTRACT: ContractVersion = ContractVersion(1);

/// The most a manifest's human-readable fields may carry.
const MAX_NAME_BYTES: usize = 64;
const MAX_DESCRIPTION_BYTES: usize = 240;
const MAX_VERSION_BYTES: usize = 32;
/// The most executables one plugin may claim to support.
const MAX_SUPPORTED_EXECUTABLES: usize = 32;

/// Which version of the plugin contract a plugin was written against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContractVersion(u32);

impl ContractVersion {
    /// A version number as it appears in a manifest.
    #[must_use]
    pub const fn new(number: u32) -> Self {
        Self(number)
    }

    /// The number.
    #[must_use]
    pub const fn number(self) -> u32 {
        self.0
    }

    /// Whether this build speaks it.
    ///
    /// One version today, so this is an equality test. It is a method rather
    /// than a comparison at every call site because the day there are two, the
    /// rule about which of them a host still accepts is written once.
    #[must_use]
    pub const fn is_supported(self) -> bool {
        self.0 == CONTRACT.0
    }
}

impl fmt::Display for ContractVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// A plugin's identifier: who it is, and what its events are attributed to.
///
/// It is an [`EventSource`] under the syntax `crates/events` already defines —
/// lowercase ASCII letters, digits, `-`, `_` and `.`, each dot-separated
/// segment starting with a letter — and `clipped` is refused, because that is
/// the source the application itself reports under and a plugin must not be
/// able to speak for the project.
///
/// There is one identifier rather than one for the manifest and one for the
/// event stream so that a mark on a timeline and the plugin a user can disable
/// are the same name. Nothing translates between them, so nothing can disagree.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PluginId(EventSource);

impl PluginId {
    /// An identifier, checked.
    ///
    /// # Errors
    ///
    /// [`InvalidSource`] naming the rule it broke.
    pub fn new(identifier: &str) -> Result<Self, InvalidSource> {
        EventSource::plugin(identifier).map(Self)
    }

    /// The identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// The source events from this plugin are stamped with.
    ///
    /// The host stamps it; a plugin never sends one. See
    /// [`crate::report`](crate::report).
    #[must_use]
    pub fn as_source(&self) -> &EventSource {
        &self.0
    }
}

impl TryFrom<String> for PluginId {
    type Error = InvalidSource;

    fn try_from(identifier: String) -> Result<Self, Self::Error> {
        Self::new(&identifier)
    }
}

/// Checked when read, unlike `EventSource`.
///
/// `crates/events` reads a stored source verbatim, because refusing a document
/// already in a user's library would delete an event to enforce a rule it has
/// already broken. A manifest is not stored data: it is a declaration being
/// offered, and the plugin it describes has not run yet. Refusing it costs a
/// message; accepting it would mean a plugin whose events are attributed to
/// something that is not an identifier.
impl<'de> Deserialize<'de> for PluginId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let identifier = String::deserialize(deserializer)?;
        Self::new(&identifier).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for PluginId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A process that has started, as much of it as a plugin is told.
///
/// The executable's file name and its process identifier, and nothing else. A
/// plugin is another program: it is told what it needs in order to find the
/// game's own interface — a log directory under the executable, a port the game
/// opens — and not the window title, the command line or where recordings are
/// being written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedProcess {
    /// The executable's file name, such as `cs2.exe`.
    executable: String,
    /// The operating system's identifier for the running process.
    process_id: u32,
}

impl ObservedProcess {
    /// A process running `executable`.
    ///
    /// `executable` may be a full path; only the file name is kept. Taking the
    /// file name here rather than trusting the caller is what stops
    /// `C:\Games\cs2.exe` silently matching nothing when a manifest says
    /// `cs2.exe`.
    #[must_use]
    pub fn new(executable: &str, process_id: u32) -> Self {
        let file_name = executable
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or(executable)
            .to_owned();
        Self {
            executable: file_name,
            process_id,
        }
    }

    /// The executable's file name.
    #[must_use]
    pub fn executable(&self) -> &str {
        &self.executable
    }

    /// The operating system's identifier for the process.
    #[must_use]
    pub const fn process_id(&self) -> u32 {
        self.process_id
    }
}

impl fmt::Display for ObservedProcess {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({})", self.executable, self.process_id)
    }
}

/// Which games a plugin handles.
///
/// Executable file names, compared without regard to case because Windows
/// paths are. Deliberately narrower than `clipped-game-detection`'s catalogue,
/// which matches launchers, process trees and window titles: a plugin does not
/// decide what a game *is*, it says which processes it has an integration for,
/// and the two questions are answered by different code because they have
/// different answers — the catalogue knows `cs2.exe` is Counter-Strike 2
/// whether or not any plugin is installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Supports {
    /// The executables this plugin has an integration for.
    executables: Vec<String>,
}

impl Supports {
    /// A plugin that supports `executables`.
    #[must_use]
    pub fn executables<I, S>(executables: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            executables: executables.into_iter().map(Into::into).collect(),
        }
    }

    /// The executables, as the manifest wrote them.
    #[must_use]
    pub fn executable_names(&self) -> &[String] {
        &self.executables
    }

    /// Whether this plugin handles `process`.
    #[must_use]
    pub fn matches(&self, process: &ObservedProcess) -> bool {
        self.executables
            .iter()
            .any(|name| name.eq_ignore_ascii_case(process.executable()))
    }

    fn validate(&self) -> Result<(), ManifestError> {
        if self.executables.is_empty() {
            return Err(ManifestError::SupportsNothing);
        }
        if self.executables.len() > MAX_SUPPORTED_EXECUTABLES {
            return Err(ManifestError::SupportsTooMany {
                declared: self.executables.len(),
            });
        }
        for executable in &self.executables {
            if executable.is_empty()
                || executable.len() > MAX_NAME_BYTES
                || executable.contains(['\\', '/'])
                || executable.chars().any(char::is_control)
            {
                return Err(ManifestError::SupportedExecutable {
                    executable: executable.clone(),
                });
            }
        }
        Ok(())
    }
}

/// Everything a plugin declares about itself.
///
/// The file is `plugin.json`, in the plugin's own directory:
///
/// ```json
/// {
///   "contract": 1,
///   "id": "counter-strike-2",
///   "name": "Counter-Strike 2",
///   "version": "0.1.0",
///   "description": "Reports kills, deaths and rounds from Game State Integration.",
///   "executable": "clipped-cs2-plugin.exe",
///   "supports": { "executables": ["cs2.exe"] },
///   "network": [
///     {
///       "class": "loopback",
///       "direction": "listen",
///       "endpoint": "127.0.0.1:3212",
///       "purpose": "receives Counter-Strike 2 game state"
///     }
///   ]
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    /// The contract version the plugin was written against.
    contract: ContractVersion,
    /// Who it is.
    id: PluginId,
    /// What to call it in the interface.
    name: String,
    /// The plugin's own version, for the user and for a bug report. Free text:
    /// the host never compares two of them, because a plugin is not updated by
    /// Clipped.
    version: String,
    /// One or two lines saying what it reports.
    #[serde(default)]
    description: String,
    /// The executable to run, as a file name inside the plugin's own directory.
    executable: String,
    /// Which games it handles.
    supports: Supports,
    /// What it will do with the network. Absent means none, which means none is
    /// permitted (`docs/privacy.md`).
    #[serde(default)]
    network: NetworkAccess,
}

impl PluginManifest {
    /// Reads and checks a `plugin.json`.
    ///
    /// # Errors
    ///
    /// [`ManifestError`], naming the rule the manifest broke. A manifest
    /// written against a contract this build does not speak is reported as
    /// exactly that rather than as a parse failure, because the two have
    /// different answers: one needs a newer Clipped, the other needs a fixed
    /// file.
    pub fn parse(json: &str) -> Result<Self, ManifestError> {
        // The contract version is read before anything else is interpreted, and
        // leniently. A manifest written against contract 2 will very likely
        // carry a field this build has never heard of, and `deny_unknown_fields`
        // would report that field rather than the version — sending a user to
        // look for a typo in a file that is simply newer than their Clipped.
        let document: Value =
            serde_json::from_str(json).map_err(|source| ManifestError::Malformed { source })?;
        let contract = document
            .get("contract")
            .and_then(Value::as_u64)
            .and_then(|number| u32::try_from(number).ok())
            .map(ContractVersion::new)
            .ok_or(ManifestError::NoContractVersion)?;
        if !contract.is_supported() {
            return Err(ManifestError::UnsupportedContract { contract });
        }

        let manifest: Self = serde_json::from_value(document)
            .map_err(|source| ManifestError::Malformed { source })?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// The contract version it was written against.
    #[must_use]
    pub const fn contract(&self) -> ContractVersion {
        self.contract
    }

    /// Who it is.
    #[must_use]
    pub const fn id(&self) -> &PluginId {
        &self.id
    }

    /// What to call it in the interface.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The plugin's own version.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// What it says it reports.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// The executable's file name, inside the plugin's own directory.
    #[must_use]
    pub fn executable(&self) -> &str {
        &self.executable
    }

    /// Which games it handles.
    #[must_use]
    pub const fn supports(&self) -> &Supports {
        &self.supports
    }

    /// What it will do with the network.
    #[must_use]
    pub const fn network(&self) -> &NetworkAccess {
        &self.network
    }

    fn validate(&self) -> Result<(), ManifestError> {
        check_line("name", &self.name, MAX_NAME_BYTES, true)?;
        check_line("version", &self.version, MAX_VERSION_BYTES, true)?;
        check_line(
            "description",
            &self.description,
            MAX_DESCRIPTION_BYTES,
            false,
        )?;
        self.check_executable()?;
        self.supports.validate()?;
        self.network
            .validate()
            .map_err(|source| ManifestError::Network { source })
    }

    /// A plugin runs its own file, from its own directory.
    ///
    /// A manifest naming `..\..\Windows\System32\cmd.exe`, or an absolute path,
    /// would make a plugin directory a way to run anything on the machine under
    /// a name the user consented to. The file name is therefore one path
    /// component, and the host joins it to the directory the manifest was found
    /// in (`crate::discovery`).
    fn check_executable(&self) -> Result<(), ManifestError> {
        let refused = self.executable.is_empty()
            || self.executable.len() > MAX_NAME_BYTES
            || self.executable.contains(['\\', '/', ':'])
            || self.executable.chars().any(char::is_control)
            || self.executable == ".."
            || self.executable == ".";
        if refused {
            return Err(ManifestError::Executable {
                executable: self.executable.clone(),
            });
        }
        Ok(())
    }
}

/// Checks one human-readable field.
fn check_line(
    field: &'static str,
    value: &str,
    limit: usize,
    required: bool,
) -> Result<(), ManifestError> {
    let refused = (required && value.is_empty())
        || value.len() > limit
        || value.chars().any(char::is_control);
    if refused {
        return Err(ManifestError::Text {
            field,
            limit,
            required,
        });
    }
    Ok(())
}

/// Why a manifest was refused.
#[derive(Debug)]
pub enum ManifestError {
    /// It is not the JSON this build expects.
    Malformed {
        /// What `serde_json` made of it.
        source: serde_json::Error,
    },
    /// It has no `contract` field, or one that is not a whole number.
    NoContractVersion,
    /// It was written against a contract version this build does not speak.
    UnsupportedContract {
        /// The version it asked for.
        contract: ContractVersion,
    },
    /// A field shown to the user is empty, too long, or not one line.
    Text {
        /// Which field.
        field: &'static str,
        /// The limit it exceeded.
        limit: usize,
        /// Whether the field is required at all.
        required: bool,
    },
    /// The executable is not a single file name inside the plugin's directory.
    Executable {
        /// What was named.
        executable: String,
    },
    /// It supports no executables, so it could never run.
    SupportsNothing,
    /// It claims more executables than a manifest may list.
    SupportsTooMany {
        /// How many.
        declared: usize,
    },
    /// One of the supported executables is not a file name.
    SupportedExecutable {
        /// What was named.
        executable: String,
    },
    /// Its network declaration breaks `docs/privacy.md`.
    Network {
        /// Which rule.
        source: NetworkDeclarationError,
    },
}

impl fmt::Display for ManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed { source } => {
                write!(formatter, "plugin.json could not be read: {source}")
            }
            Self::NoContractVersion => formatter.write_str(
                "plugin.json has no `contract` version, so there is no way to tell which plugin \
                 contract it was written against",
            ),
            Self::UnsupportedContract { contract } => write!(
                formatter,
                "this plugin was written against plugin contract {contract}, and this build of \
                 Clipped speaks contract {CONTRACT}; a newer Clipped is needed to run it"
            ),
            Self::Text {
                field,
                limit,
                required: true,
            } => write!(
                formatter,
                "plugin.json's `{field}` is shown to the user, so it is required and is one line \
                 of at most {limit} bytes"
            ),
            Self::Text {
                field,
                limit,
                required: false,
            } => write!(
                formatter,
                "plugin.json's `{field}` is shown to the user, so it is one line of at most \
                 {limit} bytes"
            ),
            Self::Executable { executable } => write!(
                formatter,
                "`{executable}` is not a usable executable: a plugin runs one file from its own \
                 directory, named without a path"
            ),
            Self::SupportsNothing => formatter
                .write_str("plugin.json supports no executables, so nothing would ever start it"),
            Self::SupportsTooMany { declared } => write!(
                formatter,
                "a plugin may support at most {MAX_SUPPORTED_EXECUTABLES} executables, and this \
                 one names {declared}"
            ),
            Self::SupportedExecutable { executable } => {
                write!(formatter, "`{executable}` is not an executable file name")
            }
            Self::Network { source } => write!(
                formatter,
                "plugin.json's network declaration was refused: {source}"
            ),
        }
    }
}

impl core::error::Error for ManifestError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Malformed { source } => Some(source),
            Self::Network { source } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A manifest with everything filled in, as a plugin author would write it.
    pub(crate) const EXAMPLE: &str = r#"{
        "contract": 1,
        "id": "counter-strike-2",
        "name": "Counter-Strike 2",
        "version": "0.1.0",
        "description": "Reports kills, deaths and rounds from Game State Integration.",
        "executable": "clipped-cs2-plugin.exe",
        "supports": { "executables": ["cs2.exe"] },
        "network": [
            {
                "class": "loopback",
                "direction": "listen",
                "endpoint": "127.0.0.1:3212",
                "purpose": "receives Counter-Strike 2 game state"
            }
        ]
    }"#;

    /// `EXAMPLE` with one field replaced, for the cases that differ by one
    /// thing.
    fn example_with(field: &str, value: &str) -> String {
        let mut document: Value = serde_json::from_str(EXAMPLE).expect("the example parses");
        let replacement: Value = serde_json::from_str(value).expect("a JSON value");
        document
            .as_object_mut()
            .expect("an object")
            .insert(field.to_owned(), replacement);
        document.to_string()
    }

    #[test]
    fn a_manifest_says_who_what_and_where() {
        let manifest = PluginManifest::parse(EXAMPLE).expect("the example is well formed");
        assert_eq!(manifest.contract(), CONTRACT);
        assert_eq!(manifest.id().as_str(), "counter-strike-2");
        assert_eq!(manifest.name(), "Counter-Strike 2");
        assert_eq!(manifest.executable(), "clipped-cs2-plugin.exe");
        assert!(manifest
            .supports()
            .matches(&ObservedProcess::new("cs2.exe", 4)));
        assert_eq!(
            manifest.network().summary(),
            vec![
                "Listens on 127.0.0.1:3212 (this machine only) — receives Counter-Strike 2 game \
                 state"
            ]
        );
    }

    #[test]
    fn a_newer_contract_is_reported_as_a_newer_contract() {
        // And not as the unknown field a newer manifest is likely to carry:
        // the two failures have different answers, and only one of them is the
        // user's to fix.
        let newer = r#"{"contract": 2, "id": "acme", "name": "Acme", "version": "1",
                        "executable": "acme.exe", "supports": {"executables": ["a.exe"]},
                        "reads_replays": true}"#;
        let refusal = PluginManifest::parse(newer).expect_err("contract 2 is not spoken here");
        assert!(
            matches!(
                refusal,
                ManifestError::UnsupportedContract { contract } if contract.number() == 2
            ),
            "expected an unsupported contract, got {refusal}"
        );
        assert!(
            refusal.to_string().contains("newer Clipped"),
            "the message should say what to do: {refusal}"
        );
    }

    #[test]
    fn an_unknown_field_on_a_known_contract_refuses_the_manifest() {
        // The opposite rule from `crates/events`, deliberately. A manifest is a
        // permission document: running a plugin under the part of its
        // declaration this build happened to understand is consent nobody gave.
        let refusal = PluginManifest::parse(&example_with("reads_replays", "true"))
            .expect_err("an unknown field is refused");
        assert!(
            matches!(refusal, ManifestError::Malformed { .. }),
            "expected a malformed manifest, got {refusal}"
        );
        assert!(
            refusal.to_string().contains("reads_replays"),
            "the message should name the field: {refusal}"
        );
    }

    #[test]
    fn a_manifest_without_a_contract_version_is_refused_by_name() {
        let refusal = PluginManifest::parse(
            r#"{"id": "acme", "name": "Acme", "version": "1", "executable": "acme.exe",
                "supports": {"executables": ["a.exe"]}}"#,
        )
        .expect_err("a manifest without a contract version cannot be interpreted");
        assert!(matches!(refusal, ManifestError::NoContractVersion));
        assert!(refusal.to_string().contains("contract"));
    }

    #[test]
    fn a_plugin_cannot_call_itself_the_application() {
        // `clipped` is the source the application reports events under. A
        // plugin that could claim it could put a mark on a timeline that the
        // user believes Clipped made.
        let refusal = PluginManifest::parse(&example_with("id", r#""clipped""#))
            .expect_err("`clipped` is reserved");
        assert!(refusal.to_string().contains("reserved"), "{refusal}");

        for malformed in [r#""Counter Strike""#, r#""9lives""#, r#""""#] {
            assert!(
                PluginManifest::parse(&example_with("id", malformed)).is_err(),
                "{malformed} is not an identifier"
            );
        }
    }

    #[test]
    fn a_plugin_runs_one_file_from_its_own_directory() {
        // A manifest is data, and a plugin directory must not be a way to run
        // anything else on the machine under a name the user consented to.
        for escape in [
            r#""..\\..\\Windows\\System32\\cmd.exe""#,
            r#""C:\\Windows\\System32\\cmd.exe""#,
            r#""../evil.exe""#,
            r#""sub/dir.exe""#,
            r#""""#,
        ] {
            let refusal = PluginManifest::parse(&example_with("executable", escape))
                .expect_err("a path is not an executable file name");
            assert!(
                matches!(refusal, ManifestError::Executable { .. }),
                "{escape} should be refused as an executable, and was refused as {refusal}"
            );
        }
    }

    #[test]
    fn a_plugin_that_supports_nothing_is_refused() {
        let refusal = PluginManifest::parse(&example_with("supports", r#"{"executables": []}"#))
            .expect_err("a plugin that supports nothing would never run");
        assert!(matches!(refusal, ManifestError::SupportsNothing));
    }

    #[test]
    fn a_process_matches_by_file_name_whatever_case_or_path_it_arrives_in() {
        let manifest = PluginManifest::parse(EXAMPLE).expect("the example is well formed");
        let supports = manifest.supports();
        assert!(supports.matches(&ObservedProcess::new(r"C:\Games\cs2\CS2.EXE", 9)));
        assert!(supports.matches(&ObservedProcess::new("cs2.exe", 9)));
        assert!(!supports.matches(&ObservedProcess::new("notepad.exe", 9)));
    }

    #[test]
    fn a_declaration_the_privacy_policy_refuses_refuses_the_manifest() {
        let refusal = PluginManifest::parse(&example_with(
            "network",
            r#"[{"class":"loopback","direction":"listen","endpoint":"0.0.0.0:3212",
                 "purpose":"reads game state"}]"#,
        ))
        .expect_err("a wildcard bind is not loopback");
        assert!(
            matches!(refusal, ManifestError::Network { .. }),
            "expected a network refusal, got {refusal}"
        );
    }

    #[test]
    fn a_manifest_that_declares_no_network_access_declares_none() {
        let mut document: Value = serde_json::from_str(EXAMPLE).expect("the example parses");
        document
            .as_object_mut()
            .expect("an object")
            .remove("network");
        let manifest = PluginManifest::parse(&document.to_string()).expect("network is optional");
        assert!(manifest.network().is_empty());
        assert_eq!(
            manifest.network().consent_token().as_str(),
            "no network access"
        );
    }
}
