//! `clipped-recorder plugins`: see what a plugin declares, and decide about it.
//!
//! # Why a terminal takes consent
//!
//! [Issue #281](https://github.com/wildware-uk/clipped/issues/281) is the screen
//! that will do this properly, and it needs the plugins IPC as well as a window.
//! Until it lands, the record
//! [issue #282](https://github.com/wildware-uk/clipped/issues/282) added can be
//! read by every recording and written by nothing, so enabling a plugin means
//! hand-editing `settings.json` and typing a consent token exactly right — and
//! getting it slightly wrong is silent, because a token matching no declaration
//! simply lapses.
//!
//! A feature that cannot be reached is the shape AGENTS.md section 27 is about,
//! and `clipped-recorder` already owns the surfaces that have no window yet
//! (`docs/recorder-cli.md`).
//!
//! # The rule this command exists to keep
//!
//! **The declaration is printed before consent is taken, never after.** Consent
//! to something a person has not been shown is not consent, and
//! `docs/privacy.md`'s register is only true if a deliberate, informed action is
//! what starts a plugin. [`NetworkAccess::summary`] is the plain sentences and
//! [`NetworkAccess::ENFORCEMENT`] is the statement of what Clipped can and
//! cannot promise about them; both are printed, and the second is not optional
//! because a declaration without it overstates what enabling a plugin buys.
//!
//! # What it does not do
//!
//! It does not talk to a running recorder, so it does not report health: whether
//! a plugin is running, restarting or was stopped for flooding is a live
//! session's business and belongs with the screen. This command answers "what is
//! installed, what does it ask for, and what have I agreed to", which is the
//! part that has to exist before any of that matters.

use core::fmt;
use std::error::Error;
use std::path::PathBuf;

use clipped_plugins::{discover, InstalledPlugin, NetworkAccess, RejectedPlugin};
use clipped_session::config::{
    ConfigurationError, ConfigurationStore, NotStarted, PluginConsent, PluginConsents,
};

use crate::cli::{PluginsAction, PluginsArgs};

/// Why `plugins` could not do what it was asked.
#[derive(Debug)]
pub enum PluginsError {
    /// There is nowhere to look for plugins.
    NoDirectory,

    /// No plugin has that identifier.
    NoSuchPlugin {
        /// What was asked for.
        wanted: String,
        /// What is installed, as a sentence, or a statement that nothing is.
        available: String,
    },

    /// The settings file could not be read or written.
    Settings(ConfigurationError),
}

impl fmt::Display for PluginsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDirectory => formatter.write_str(
                "there is no plugins directory: this build could not work out where your program                  data lives, so there is nothing to list",
            ),
            Self::NoSuchPlugin { wanted, available } => {
                write!(formatter, "no installed plugin is called {wanted}.{available}")
            }
            Self::Settings(error) => write!(formatter, "the settings could not be saved: {error}"),
        }
    }
}

impl Error for PluginsError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Settings(error) => Some(error),
            Self::NoDirectory | Self::NoSuchPlugin { .. } => None,
        }
    }
}

impl From<ConfigurationError> for PluginsError {
    fn from(error: ConfigurationError) -> Self {
        Self::Settings(error)
    }
}

/// Runs `clipped-recorder plugins`.
///
/// # Errors
///
/// [`PluginsError`] as its variants describe. Nothing here can fail in a way
/// that costs a recording: this command does not record.
pub fn run(args: &PluginsArgs) -> Result<(), PluginsError> {
    let directory = crate::config::plugins_directory().ok_or(PluginsError::NoDirectory)?;
    let discovery = discover(&directory);

    let mut store = ConfigurationStore::at(settings_path());
    // A file that is not there is not an error: a user who has never changed a
    // setting has none, and this command is how they get their first one.
    store.load()?;
    let mut configuration = store.current().clone();

    match &args.action {
        PluginsAction::List => {
            list(
                &discovery.installed,
                &discovery.rejected,
                configuration.plugins(),
            );
            Ok(())
        }
        PluginsAction::Enable { plugin } => {
            let installed = find(&discovery.installed, plugin)?;
            show(installed);

            // The token is taken from what the plugin declares *now*, at the
            // moment it is shown. That is what makes the printout above and the
            // record below the same statement rather than two that could
            // disagree.
            let consent = PluginConsent::enabled(installed.consent_token());
            configuration.set_plugin(plugin.clone(), consent);
            store.store(configuration)?;

            println!("\n{plugin} is enabled. It will start with the next game it supports.");
            Ok(())
        }
        PluginsAction::Disable { plugin } => {
            let installed = find(&discovery.installed, plugin)?;
            // Its own token rather than whatever was stored, so that a plugin
            // turned off while its consent had lapsed does not keep a token
            // that would silently start matching again.
            let consent = configuration.plugins().get(plugin).map_or_else(
                || installed.consent_token(),
                |had| had.consented_to().clone(),
            );
            configuration.set_plugin(plugin.clone(), PluginConsent::disabled(consent));
            store.store(configuration)?;

            println!("{plugin} is turned off. What you agreed to is kept, so turning it back on will not ask again unless it has changed.");
            Ok(())
        }
    }
}

/// Where the settings file is, or the default location.
fn settings_path() -> PathBuf {
    ConfigurationStore::default_path().unwrap_or_else(|| PathBuf::from("settings.json"))
}

/// The plugin called `wanted`, or a refusal naming what is installed.
fn find<'a>(
    installed: &'a [InstalledPlugin],
    wanted: &str,
) -> Result<&'a InstalledPlugin, PluginsError> {
    installed
        .iter()
        .find(|plugin| plugin.id().as_str() == wanted)
        .ok_or_else(|| PluginsError::NoSuchPlugin {
            wanted: wanted.to_owned(),
            available: if installed.is_empty() {
                " Nothing is installed.".to_owned()
            } else {
                let names: Vec<&str> = installed
                    .iter()
                    .map(|plugin| plugin.id().as_str())
                    .collect();
                format!(" Installed: {}.", names.join(", "))
            },
        })
}

/// What one plugin is and what it asks for, as the lines to print.
///
/// Built rather than printed so that a test can assert what a person is shown
/// before they agree to it. The order is load-bearing: what it is, what it
/// wants, then what Clipped can actually promise about that -- a declaration
/// shown without [`NetworkAccess::ENFORCEMENT`] overstates what enabling a
/// plugin buys, so the statement is last and is not optional.
fn declaration_of(plugin: &InstalledPlugin) -> Vec<String> {
    let manifest = plugin.manifest();
    let mut lines = vec![
        format!(
            "{}  {}  {}",
            plugin.id(),
            manifest.name(),
            manifest.version()
        ),
        format!("  {}", manifest.description()),
    ];

    if manifest.network().is_empty() {
        lines.push("  It declares no network access.".to_owned());
    } else {
        lines.extend(
            manifest
                .network()
                .summary()
                .into_iter()
                .map(|sentence| format!("  {sentence}")),
        );
    }

    lines.push(format!("  {}", NetworkAccess::ENFORCEMENT));
    lines
}

/// Prints what one plugin is and what it asks for.
fn show(plugin: &InstalledPlugin) {
    for line in declaration_of(plugin) {
        println!("{line}");
    }
}

/// Prints everything installed, everything refused, and what is agreed to.
fn list(installed: &[InstalledPlugin], rejected: &[RejectedPlugin], consents: &PluginConsents) {
    if installed.is_empty() && rejected.is_empty() {
        println!("No plugins are installed.");
        return;
    }

    for plugin in installed {
        show(plugin);
        println!("  status: {}", state_of(plugin, consents));
        println!();
    }

    // Said rather than skipped: something the user put on disk expecting it to
    // work, which does not, is exactly what they need told (AGENTS.md section
    // 45).
    for refused in rejected {
        println!("Could not be read: {refused}");
    }
}

/// What this build will do about `plugin`, in the words a person needs.
fn state_of(plugin: &InstalledPlugin, consents: &PluginConsents) -> String {
    // Asked through the same resolution a recording uses, rather than
    // reimplemented here — two answers to "will this start?" that could
    // disagree is the defect this whole command exists to avoid.
    let (enabled, refused) = consents.enable_all([plugin.clone()]);
    if !enabled.is_empty() {
        return "enabled".to_owned();
    }
    match refused.first() {
        Some(NotStarted::NeverEnabled { .. }) => {
            "not enabled — run `plugins enable` to allow what it declares above".to_owned()
        }
        Some(NotStarted::TurnedOff { .. }) => "turned off".to_owned(),
        Some(NotStarted::ConsentLapsed {
            agreed_to,
            now_declares,
            ..
        }) => format!(
            "needs consent again — it asks for something other than what you agreed to\n    \
             you agreed to: {agreed_to}\n    it now asks for: {now_declares}"
        ),
        None => "unknown".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use clipped_media_validation::TemporaryDirectory;
    use clipped_plugins::ConsentToken;

    use super::*;

    /// Installs a plugin declaring `network` and answers it as discovered.
    ///
    /// The executable is an empty file: discovery asks only whether one is
    /// there, and nothing here starts anything.
    fn installed(root: &TemporaryDirectory, id: &str, network: &str) -> InstalledPlugin {
        let executable = format!("stub.{}", std::env::consts::EXE_EXTENSION);
        let executable = executable.trim_end_matches('.').to_owned();
        let manifest = format!(
            r#"{{
                "contract": 1,
                "id": "{id}",
                "name": "Test plugin",
                "version": "0.1.0",
                "description": "Installed by a test of the plugins command.",
                "executable": "{executable}",
                "supports": {{ "executables": ["cs2.exe"] }},
                "network": {network}
            }}"#
        );

        let directory = root.path().join(id);
        std::fs::create_dir_all(&directory).expect("a plugin directory can be created");
        std::fs::write(directory.join("plugin.json"), manifest).expect("a manifest is written");
        std::fs::write(directory.join(&executable), []).expect("an executable is there");

        discover(root.path())
            .installed
            .into_iter()
            .find(|plugin| plugin.id().as_str() == id)
            .expect("the plugin that was just installed is discovered")
    }

    const LISTENS: &str = r#"[{ "class": "loopback", "direction": "listen",
        "endpoint": "127.0.0.1:3212", "purpose": "receives game state" }]"#;

    #[test]
    fn what_a_plugin_asks_for_is_shown_with_what_clipped_can_promise_about_it() {
        // The first acceptance criterion, and the rule the command exists to
        // keep. The declaration is only half of what somebody needs: without
        // the enforcement statement it reads as a guarantee Clipped cannot
        // make.
        let root = TemporaryDirectory::new("plugins-declaration");
        let lines = declaration_of(&installed(&root, "acme.cs2", LISTENS));

        let endpoint = lines
            .iter()
            .position(|line| line.contains("127.0.0.1:3212"))
            .expect("the endpoint it listens on is shown");
        let enforcement = lines
            .iter()
            .position(|line| line.contains(NetworkAccess::ENFORCEMENT))
            .expect("what Clipped can promise about that is shown");

        assert!(
            endpoint < enforcement,
            "the enforcement statement came before the declaration it qualifies"
        );
        assert!(
            lines[0].contains("acme.cs2") && lines[0].contains("0.1.0"),
            "the plugin is not named and versioned first: {:?}",
            lines[0]
        );
    }

    #[test]
    fn a_plugin_that_asks_for_no_network_says_so_rather_than_saying_nothing() {
        // An empty list and "no declaration" look identical if nothing is
        // printed, and they are different things to be told.
        let root = TemporaryDirectory::new("plugins-no-network");
        let lines = declaration_of(&installed(&root, "acme.quiet", "[]"));

        assert!(
            lines.iter().any(|line| line.contains("no network access")),
            "a plugin declaring nothing produced no sentence about it: {lines:?}"
        );
    }

    #[test]
    fn the_four_states_a_plugin_can_be_in_are_told_apart() {
        let root = TemporaryDirectory::new("plugins-states");
        let plugin = installed(&root, "acme.cs2", LISTENS);
        let declared = plugin.consent_token();

        let never = PluginConsents::none();
        assert!(state_of(&plugin, &never).starts_with("not enabled"));

        let mut off = PluginConsents::none();
        off.set(
            "acme.cs2".to_owned(),
            PluginConsent::disabled(declared.clone()),
        );
        assert_eq!(state_of(&plugin, &off), "turned off");

        let mut on = PluginConsents::none();
        on.set("acme.cs2".to_owned(), PluginConsent::enabled(declared));
        assert_eq!(state_of(&plugin, &on), "enabled");

        // The third acceptance criterion: what changed is shown, both halves,
        // so somebody can see what they are being asked to agree to.
        let mut lapsed = PluginConsents::none();
        lapsed.set(
            "acme.cs2".to_owned(),
            PluginConsent::enabled(ConsentToken::from_stored("loopback listen 127.0.0.1:9999")),
        );
        let said = state_of(&plugin, &lapsed);
        assert!(said.starts_with("needs consent again"), "{said}");
        assert!(
            said.contains("127.0.0.1:9999"),
            "what was agreed to: {said}"
        );
        assert!(said.contains("127.0.0.1:3212"), "what it asks now: {said}");
    }

    #[test]
    fn an_unknown_identifier_is_refused_by_naming_what_is_installed() {
        // The fourth criterion. A typo must not write a record for a plugin
        // that is not there, and "no such plugin" on its own leaves somebody
        // guessing at an identifier they have never seen written down.
        let root = TemporaryDirectory::new("plugins-unknown");
        let installed_plugins = vec![installed(&root, "acme.cs2", LISTENS)];

        let error = find(&installed_plugins, "acme.typo").expect_err("it is not installed");

        let said = error.to_string();
        assert!(said.contains("acme.typo"), "{said}");
        assert!(
            said.contains("acme.cs2"),
            "what is installed is not named: {said}"
        );
    }

    #[test]
    fn nothing_installed_is_said_plainly_rather_than_as_an_empty_list() {
        let error = find(&[], "acme.cs2").expect_err("nothing is installed");

        assert!(
            error.to_string().contains("Nothing is installed"),
            "{error}"
        );
    }
}
