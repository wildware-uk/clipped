//! Counter-Strike 2's highlight integration: the reference plugin.
//!
//! This is a plugin under the contract `crates/plugins` defines and
//! `docs/plugin-api.md` describes — a directory holding a `plugin.json` and an
//! executable that prints events on its standard output. It is the first one,
//! and the two that follow ([#72](https://github.com/wildware-uk/clipped/issues/72),
//! [#73](https://github.com/wildware-uk/clipped/issues/73)) copy its shape, so
//! the shape is stated here rather than left to be inferred from the code.
//!
//! # Why Counter-Strike 2 is the reference
//!
//! Because it has an official answer. **Game State Integration** is Valve's own
//! documented mechanism: a `.cfg` file in the game's configuration directory
//! asks the game to POST a JSON snapshot of its state to a port on this
//! machine, and it posts. Nothing is injected, nothing is read out of another
//! process, no anti-cheat is anywhere near it. AGENTS.md section 34 is not a
//! constraint this plugin works around; it is the reason this plugin exists in
//! this form.
//!
//! # The four pieces, and the order they matter in
//!
//! | | What it does | Why it is the interesting part |
//! | --- | --- | --- |
//! | [`integration`] | Writes and removes the `.cfg` | It writes into the user's game directory, so it does so only when asked, only under its own name, and never over anything it did not write |
//! | [`listener`] | The loopback socket | `docs/privacy.md`: a loopback listener is reachable by everything on the machine, so it authenticates every payload |
//! | [`payload`] | Reads what arrives | Another program's output, read leniently, so a game update does not stop it working |
//! | [`derive`] | Snapshots into events | Game State Integration reports **state**; a kill is a *difference*, and getting the difference wrong invents events that never happened |
//!
//! The last of those is the substance. Everything a consumer of this plugin
//! ever sees is decided there, and the whole of its discipline is one sentence:
//! an event is reported only for a transition this plugin observed directly,
//! between two payloads it accepted.
//!
//! # What the host sees
//!
//! `clipped_events::GameEvent`s, through the wire in `crates/plugins`: `kill`,
//! `death`, `assist`, `round_started`, `round_ended`, `match_started`,
//! `match_ended`, `win` and `loss`. Nothing above the plugin can tell which
//! game produced one, which is AGENTS.md section 33 and the point of the whole
//! arrangement.
//!
//! # Threading, and what owns what
//!
//! Three threads, in the executable ([`main`](../main/index.html)):
//!
//! - the **listener thread** owns the socket and sends payloads down a channel;
//! - the **standard input thread** waits for `detach`, or for the host to go;
//! - the **main thread** owns the [`derive::MatchTracker`], drains the channel
//!   and writes reports.
//!
//! The tracker is therefore never shared, and every clock reading it uses is
//! supplied by its caller — which is why every case in `derive`'s tests is a
//! sequence of payloads and instants rather than something that has to wait.

pub mod derive;
pub mod integration;
pub mod keyvalues;
pub mod listener;
pub mod location;
pub mod payload;
pub mod token;

#[cfg(test)]
mod manifest_tests {
    use clipped_plugins::{
        NetworkClass, NetworkDirection, ObservedProcess, PluginManifest, CONTRACT,
    };

    use crate::integration::DEFAULT_PORT;

    /// The manifest as it is installed beside the executable.
    const MANIFEST: &str = include_str!("../plugin.json");

    #[test]
    fn the_manifest_is_one_the_host_accepts() {
        let manifest = PluginManifest::parse(MANIFEST).expect("plugin.json is a valid manifest");

        assert_eq!(manifest.contract(), CONTRACT);
        assert_eq!(manifest.id().as_str(), "counter-strike-2");
        assert_eq!(manifest.executable(), "clipped-cs2-plugin.exe");
        assert!(manifest.supports().matches(&ObservedProcess::new(
            r"C:\Games\cs2\game\bin\win64\cs2.exe",
            4242
        )));
        assert!(
            !manifest
                .supports()
                .matches(&ObservedProcess::new("notepad.exe", 1)),
            "a plugin that claims every process starts on every launch"
        );
    }

    #[test]
    fn the_manifest_declares_the_port_this_plugin_actually_listens_on() {
        // The declaration is what the user consents to before the plugin may
        // run (`docs/privacy.md`), so it has to be the socket that gets opened.
        // Without this, changing DEFAULT_PORT would leave a consent for one
        // port and a listener on another, and nothing would say so.
        let manifest = PluginManifest::parse(MANIFEST).expect("plugin.json is a valid manifest");
        let grants = manifest.network().grants();

        assert_eq!(grants.len(), 1, "one socket, and nothing else");
        assert_eq!(grants[0].class, NetworkClass::Loopback);
        assert_eq!(grants[0].direction, NetworkDirection::Listen);
        assert_eq!(grants[0].endpoint, format!("127.0.0.1:{DEFAULT_PORT}"));
        assert!(
            !manifest.network().leaves_the_machine(),
            "nothing this plugin does may leave the machine"
        );
    }

    #[test]
    fn the_sentence_the_user_is_shown_before_enabling_it_says_what_it_does() {
        let manifest = PluginManifest::parse(MANIFEST).expect("plugin.json is a valid manifest");
        let summary = manifest.network().summary();

        assert_eq!(
            summary,
            vec![format!(
                "Listens on 127.0.0.1:{DEFAULT_PORT} (this machine only) — receives \
                 Counter-Strike 2 game state"
            )]
        );
    }

    #[test]
    fn the_manifest_names_the_binary_cargo_builds() {
        // A manifest naming an executable that is not there is a plugin the
        // host refuses at discovery, and the way that happens is somebody
        // renaming the `[[bin]]` target.
        let manifest = PluginManifest::parse(MANIFEST).expect("plugin.json is a valid manifest");
        assert_eq!(
            manifest.executable(),
            format!("{}.exe", env!("CARGO_PKG_NAME")),
            "plugin.json and Cargo.toml disagree about the executable's name"
        );
    }
}
