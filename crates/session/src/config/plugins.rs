//! Which plugins the user enabled, and what they agreed to when they did.
//!
//! ```json
//! "plugins": {
//!   "acme.counter-strike-2": {
//!     "enabled": true,
//!     "consented_to": "outbound tcp 127.0.0.1:3000"
//!   }
//! }
//! ```
//!
//! # Why the consent is stored as text
//!
//! `clipped_plugins::ConsentToken` is the canonical rendering of a plugin's
//! network declaration, sorted so that reordering a manifest does not lapse
//! consent and any real change does. It is legible on purpose: a person reading
//! their own settings file can see what they agreed to without running
//! anything, which a hash of the declaration would not give them.
//!
//! Storing it also means the *comparison* is what matters rather than the
//! moment. A plugin that updates and asks for a new endpoint no longer matches
//! the token beside it, so it is not started and the reason can be put in front
//! of the user (`clipped_plugins::ConsentLapsed`) — rather than running with
//! access nobody agreed to, which is the failure this whole mechanism exists to
//! prevent.
//!
//! # Why it lives here and not in `crates/plugins`
//!
//! The configuration API is this crate's (`docs/configuration.md`). A plugin
//! crate that read and wrote its own settings file would be the second
//! configuration store AGENTS.md section 30 warns about, which is the defect
//! [issue #252](https://github.com/wildware-uk/clipped/issues/252) records for
//! notification settings
//! ([issue #282](https://github.com/wildware-uk/clipped/issues/282)).
//!
//! # A plugin the configuration does not mention
//!
//! Is disabled. Absence is the safe answer and the only honest one: a plugin is
//! a program somebody else wrote, and "we have no record of you enabling this"
//! must never resolve to "so run it". That is also what makes a settings file
//! written before this existed read correctly rather than fail — every plugin in
//! it is simply disabled (AGENTS.md sections 30 and 43).

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use clipped_plugins::{ConsentToken, InstalledPlugin};

/// What the configuration records for one plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginConsent {
    enabled: bool,
    consented_to: ConsentToken,
}

impl PluginConsent {
    /// A plugin the user enabled, having agreed to `consented_to`.
    #[must_use]
    pub const fn enabled(consented_to: ConsentToken) -> Self {
        Self {
            enabled: true,
            consented_to,
        }
    }

    /// A plugin the user has turned off, keeping what they had agreed to.
    ///
    /// The token is kept rather than cleared so that turning a plugin back on
    /// does not ask again for access that has not changed.
    #[must_use]
    pub const fn disabled(consented_to: ConsentToken) -> Self {
        Self {
            enabled: false,
            consented_to,
        }
    }

    /// Whether the user has this plugin on.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// What they agreed to.
    #[must_use]
    pub const fn consented_to(&self) -> &ConsentToken {
        &self.consented_to
    }
}

/// Every plugin the settings file mentions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginConsents {
    entries: BTreeMap<String, PluginConsent>,
    /// Keys this build could not read, kept so that writing the file back does
    /// not delete what a newer build stored (AGENTS.md section 56).
    unknown: BTreeMap<String, Value>,
}

impl PluginConsents {
    /// No plugin enabled, which is what a settings file with no `plugins`
    /// section means.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// What is recorded for `plugin`, if anything.
    #[must_use]
    pub fn get(&self, plugin: &str) -> Option<&PluginConsent> {
        self.entries.get(plugin)
    }

    /// Records `consent` for `plugin`, replacing whatever was there.
    pub fn set(&mut self, plugin: String, consent: PluginConsent) {
        self.entries.insert(plugin, consent);
    }

    /// Every plugin mentioned, by identifier.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &PluginConsent)> {
        self.entries
            .iter()
            .map(|(plugin, consent)| (plugin.as_str(), consent))
    }

    /// Whether nothing is recorded at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.unknown.is_empty()
    }

    /// Turns `installed` into the plugins that may actually be started.
    ///
    /// Everything that is not started is returned beside them as a
    /// [`NotStarted`], because a plugin the user switched on and which then
    /// silently did not run is the failure AGENTS.md section 27 is about: a
    /// caller can say which plugin, and why, without asking again.
    ///
    /// A plugin whose declaration no longer matches the stored token is
    /// [`NotStarted::ConsentLapsed`] rather than an error — the user is asked
    /// again, and nothing runs in the meantime.
    #[must_use]
    pub fn enable_all(
        &self,
        installed: impl IntoIterator<Item = InstalledPlugin>,
    ) -> (Vec<clipped_plugins::EnabledPlugin>, Vec<NotStarted>) {
        let mut enabled = Vec::new();
        let mut refused = Vec::new();
        for plugin in installed {
            let id = plugin.id().as_str().to_owned();
            let Some(consent) = self.get(&id) else {
                refused.push(NotStarted::NeverEnabled { plugin: id });
                continue;
            };
            if !consent.is_enabled() {
                refused.push(NotStarted::TurnedOff { plugin: id });
                continue;
            }
            match plugin.enable(consent.consented_to()) {
                Ok(plugin) => enabled.push(plugin),
                Err(lapsed) => refused.push(NotStarted::ConsentLapsed {
                    plugin: id,
                    agreed_to: lapsed.consented_to.to_string(),
                    now_declares: lapsed.now_declares.to_string(),
                }),
            }
        }
        (enabled, refused)
    }

    /// Keys this build did not understand.
    pub fn unrecognised(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.unknown.iter()
    }

    /// Keeps a key this build could not read.
    pub(crate) fn keep_unrecognised(&mut self, key: String, value: Value) {
        self.unknown.insert(key, value);
    }
}

/// Why an installed plugin was not started.
///
/// Three different things to tell somebody, which is why they are not one
/// boolean: a plugin nobody has ever enabled needs an invitation, one that was
/// turned off needs nothing, and one whose consent lapsed needs the user to
/// look at what changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotStarted {
    /// The configuration has never mentioned this plugin.
    NeverEnabled {
        /// Which plugin.
        plugin: String,
    },
    /// The user turned it off.
    TurnedOff {
        /// Which plugin.
        plugin: String,
    },
    /// It asks for something other than what was agreed to.
    ConsentLapsed {
        /// Which plugin.
        plugin: String,
        /// What the user agreed to.
        agreed_to: String,
        /// What it declares now.
        now_declares: String,
    },
}

impl NotStarted {
    /// Which plugin this is about.
    #[must_use]
    pub fn plugin(&self) -> &str {
        match self {
            Self::NeverEnabled { plugin }
            | Self::TurnedOff { plugin }
            | Self::ConsentLapsed { plugin, .. } => plugin,
        }
    }
}

/// Reads the `plugins` section.
///
/// An entry this build cannot make sense of is kept rather than refused, for
/// the reason the module documentation gives: the alternative is deleting a
/// newer build's record of what somebody consented to.
pub(crate) fn read(object: Option<Map<String, Value>>) -> PluginConsents {
    let mut consents = PluginConsents::none();
    let Some(object) = object else {
        return consents;
    };

    for (key, value) in object {
        let Some(entry) = value.as_object() else {
            consents.keep_unrecognised(key, value);
            continue;
        };
        let Some(consented_to) = entry.get("consented_to").and_then(Value::as_str) else {
            // Enabled without a record of what was agreed to is exactly the
            // state this design refuses to have: it would either run with
            // unexamined access or invent a consent nobody gave.
            consents.keep_unrecognised(key, value.clone());
            continue;
        };
        let token = ConsentToken::from_stored(consented_to);
        let consent = if entry.get("enabled").and_then(Value::as_bool) == Some(true) {
            PluginConsent::enabled(token)
        } else {
            PluginConsent::disabled(token)
        };
        consents.set(key, consent);
    }

    consents
}

/// Turns the section back into JSON.
pub(crate) fn write(consents: &PluginConsents) -> Map<String, Value> {
    let mut object = Map::new();
    for (plugin, consent) in consents.iter() {
        let mut entry = Map::new();
        entry.insert("enabled".to_owned(), Value::from(consent.is_enabled()));
        entry.insert(
            "consented_to".to_owned(),
            Value::from(consent.consented_to().as_str()),
        );
        object.insert(plugin.to_owned(), Value::Object(entry));
    }
    for (key, value) in consents.unrecognised() {
        object.insert(key.clone(), value.clone());
    }
    object
}

#[cfg(test)]
mod tests {
    use clipped_media_validation::TemporaryDirectory;

    use super::*;

    fn token(text: &str) -> ConsentToken {
        ConsentToken::from_stored(text)
    }

    #[test]
    fn a_settings_file_with_no_plugins_section_leaves_every_plugin_disabled() {
        // Acceptance criterion three, and the property that makes this change
        // safe to ship: a file written before the section existed reads, and
        // reads as "nothing enabled" rather than failing or defaulting to on.
        let consents = read(None);

        assert!(consents.is_empty());
        assert_eq!(consents.get("acme.cs2"), None);
    }

    #[test]
    fn what_was_enabled_and_what_was_agreed_to_survive_a_write_and_a_read() {
        let mut consents = PluginConsents::none();
        consents.set(
            "acme.cs2".to_owned(),
            PluginConsent::enabled(token("outbound tcp 127.0.0.1:3000")),
        );
        consents.set(
            "acme.dota".to_owned(),
            PluginConsent::disabled(token("no network access")),
        );

        let read_back = read(Some(write(&consents)));

        assert_eq!(read_back, consents, "a round trip changed something");
        let cs2 = read_back.get("acme.cs2").expect("it is recorded");
        assert!(cs2.is_enabled());
        assert_eq!(cs2.consented_to().as_str(), "outbound tcp 127.0.0.1:3000");
        assert!(
            !read_back
                .get("acme.dota")
                .expect("it is recorded")
                .is_enabled(),
            "a plugin the user turned off came back on"
        );
    }

    #[test]
    fn turning_a_plugin_off_keeps_what_was_agreed_to() {
        // So that turning it back on does not ask again for access that has
        // not changed. The token is the record of a conversation, not of a
        // state.
        let consent = PluginConsent::disabled(token("outbound tcp 127.0.0.1:3000"));

        assert!(!consent.is_enabled());
        assert_eq!(
            consent.consented_to().as_str(),
            "outbound tcp 127.0.0.1:3000"
        );
    }

    #[test]
    fn an_entry_with_no_record_of_consent_is_kept_but_not_obeyed() {
        // The dangerous shape: `enabled` with nothing saying what was agreed
        // to. Running it would mean granting access nobody examined, and
        // deleting it would throw away a newer build's record — so it is kept
        // verbatim and is not a plugin this build will start.
        let mut object = Map::new();
        object.insert(
            "acme.cs2".to_owned(),
            serde_json::json!({ "enabled": true }),
        );

        let consents = read(Some(object));

        assert_eq!(
            consents.get("acme.cs2"),
            None,
            "a plugin was enabled with no record of what was consented to"
        );
        assert_eq!(
            consents.unrecognised().count(),
            1,
            "the entry was discarded rather than kept"
        );
    }

    /// Installs a plugin declaring `network`, and answers it as discovered.
    ///
    /// The executable is an empty file: discovery only asks whether one is
    /// there, and nothing in this test starts anything.
    fn installed(root: &TemporaryDirectory, id: &str, network: &str) -> InstalledPlugin {
        let executable = format!("stub.{}", std::env::consts::EXE_EXTENSION);
        let executable = executable.trim_end_matches('.').to_owned();
        let manifest = format!(
            r#"{{
                "contract": 1,
                "id": "{id}",
                "name": "Test plugin",
                "version": "0.0.0",
                "description": "Installed by a test of plugin consent.",
                "executable": "{executable}",
                "supports": {{ "executables": ["cs2.exe"] }},
                "network": {network}
            }}"#
        );

        let directory = root.path().join(id);
        std::fs::create_dir_all(&directory).expect("a plugin directory can be created");
        std::fs::write(directory.join("plugin.json"), manifest).expect("a manifest is written");
        std::fs::write(directory.join(&executable), []).expect("an executable is there");

        clipped_plugins::discover(root.path())
            .installed
            .into_iter()
            .find(|plugin| plugin.id().as_str() == id)
            .expect("the plugin that was just installed is discovered")
    }

    #[test]
    fn a_plugin_whose_declaration_changed_is_not_started_and_says_what_changed() {
        // Acceptance criterion two. The user agreed to one endpoint; the plugin
        // now asks for another. It does not run, and both texts are reportable
        // so somebody can be shown what they are being asked to agree to.
        let root = TemporaryDirectory::new("plugin-consent-lapsed");
        let plugin = installed(
            &root,
            "acme.cs2",
            r#"[{ "class": "loopback", "direction": "listen", "endpoint": "127.0.0.1:3000",
                   "purpose": "receives game state" }]"#,
        );

        let mut consents = PluginConsents::none();
        consents.set(
            "acme.cs2".to_owned(),
            PluginConsent::enabled(token("loopback listen 127.0.0.1:9999")),
        );

        let (enabled, refused) = consents.enable_all([plugin]);

        assert!(enabled.is_empty(), "a plugin ran on consent nobody gave");
        match refused.as_slice() {
            [NotStarted::ConsentLapsed {
                plugin,
                agreed_to,
                now_declares,
            }] => {
                assert_eq!(plugin, "acme.cs2");
                assert_eq!(agreed_to, "loopback listen 127.0.0.1:9999");
                assert_eq!(now_declares, "loopback listen 127.0.0.1:3000");
            }
            other => panic!("expected one lapsed consent, got {other:?}"),
        }
    }

    #[test]
    fn a_plugin_enabled_with_consent_to_what_it_declares_now_is_started() {
        let root = TemporaryDirectory::new("plugin-consent-matches");
        let plugin = installed(
            &root,
            "acme.cs2",
            r#"[{ "class": "loopback", "direction": "listen", "endpoint": "127.0.0.1:3000",
                   "purpose": "receives game state" }]"#,
        );
        let declared = plugin.consent_token().as_str().to_owned();

        let mut consents = PluginConsents::none();
        consents.set(
            "acme.cs2".to_owned(),
            PluginConsent::enabled(token(&declared)),
        );

        let (enabled, refused) = consents.enable_all([plugin]);

        assert_eq!(enabled.len(), 1, "a plugin the user enabled did not start");
        assert!(refused.is_empty(), "{refused:?}");
    }

    #[test]
    fn a_plugin_nobody_enabled_and_one_turned_off_are_told_apart() {
        // Two different things to say to somebody: one needs an invitation and
        // the other needs nothing at all (AGENTS.md section 27).
        let root = TemporaryDirectory::new("plugin-consent-off");
        let unmentioned = installed(&root, "acme.one", "[]");
        let turned_off = installed(&root, "acme.two", "[]");

        let mut consents = PluginConsents::none();
        consents.set(
            "acme.two".to_owned(),
            PluginConsent::disabled(token("no network access")),
        );

        let (enabled, refused) = consents.enable_all([unmentioned, turned_off]);

        assert!(enabled.is_empty());
        assert_eq!(
            refused,
            vec![
                NotStarted::NeverEnabled {
                    plugin: "acme.one".to_owned()
                },
                NotStarted::TurnedOff {
                    plugin: "acme.two".to_owned()
                },
            ]
        );
    }

    #[test]
    fn a_key_from_a_newer_build_is_written_back_unchanged() {
        let mut object = Map::new();
        object.insert("acme.cs2".to_owned(), serde_json::json!("not an object"));

        let written = write(&read(Some(object)));

        assert_eq!(
            written.get("acme.cs2"),
            Some(&serde_json::json!("not an object")),
            "a key this build could not read was lost by writing the file back"
        );
    }
}
