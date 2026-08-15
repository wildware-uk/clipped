//! What the window is told about the plugins on this machine.
//!
//! # The rule this shape exists to keep
//!
//! **A declaration is shown before consent is taken, never after.** Enabling a
//! plugin *is* the consent to the network access it declares, and every bundled
//! plugin opens a loopback socket, so `docs/privacy.md`'s register is only true
//! if a deliberate, informed action is what starts one.
//!
//! That is why [`PluginDeclaration`] carries the sentences and the enforcement
//! statement rather than leaving the window to compose them: the words a person
//! agrees to are the recorder's, produced by `clipped_plugins::NetworkAccess`,
//! and two renderings of one declaration are two things to keep in step.
//!
//! # What is not here
//!
//! **Health**, and **enabling**. Whether a plugin is running, restarting or was
//! stopped for flooding belongs to a live session rather than to the list of
//! what is installed, and writing the consent record is a settings write the
//! protocol does not have ([issue
//! #281](https://github.com/wildware-uk/clipped/issues/281) owns both).
//! `clipped-recorder plugins enable` is what writes one today.

use serde::{Deserialize, Serialize};

/// One installed plugin, and what it asks for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginDeclaration {
    /// The plugin's identifier, as its manifest gives it.
    pub id: String,
    /// What to call it on a screen.
    pub name: String,
    /// Its own version. Free text: nothing compares two of them.
    pub version: String,
    /// What it says it does.
    pub description: String,
    /// What it will do with the network, one plain sentence per grant.
    ///
    /// Empty when it declares none, which is a statement rather than an
    /// absence: a screen must say "it declares no network access" rather than
    /// leave the row blank.
    pub network: Vec<String>,
    /// What Clipped can and cannot promise about the sentences above.
    ///
    /// Sent with every declaration rather than being a string the window keeps,
    /// because it is part of what somebody is agreeing to and must not drift
    /// from what the recorder enforces.
    pub enforcement: String,
    /// What this build will do about the plugin.
    pub state: PluginState,
}

/// Whether a plugin will start, and why not when it will not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum PluginState {
    /// It will start with the next game it supports.
    Enabled,
    /// Nothing has ever allowed it. What a newly installed plugin says.
    NotEnabled,
    /// The user allowed it and then turned it off. What they agreed to is kept,
    /// so turning it back on asks again only if the plugin has changed.
    TurnedOff,
    /// It asks for something other than what was agreed to, so it does not run
    /// until somebody agrees to the new declaration.
    ///
    /// Both texts travel, because the question a screen has to ask is "here is
    /// what changed" and it cannot ask it with one of them.
    NeedsConsentAgain {
        /// What the user agreed to.
        agreed_to: String,
        /// What it declares now.
        now_declares: String,
    },
}

/// Something under the plugins directory that is not a usable plugin.
///
/// Reported rather than skipped: somebody put it there expecting it to work,
/// and a list that silently omitted it would leave them with no way to find out
/// why (AGENTS.md section 45).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefusedPlugin {
    /// Where it is.
    pub directory: String,
    /// Why it was refused, in the words the recorder used.
    pub reason: String,
}
