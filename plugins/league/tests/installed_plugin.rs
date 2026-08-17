//! The committed `plugin.json`, read by the code that will read it on a user's
//! machine.
//!
//! A manifest is a permission document (`docs/plugin-api.md`): it is what the
//! user is shown before they enable a plugin, and the host refuses to start a
//! plugin whose manifest it cannot read. A typo in it is therefore not a
//! cosmetic problem — it is a plugin that never runs — and it is exactly the
//! kind of file that is never opened again after the day it is written. So it
//! is parsed here by `clipped_plugins` itself rather than eyeballed.

use std::path::Path;

use clipped_plugins::{
    NetworkClass, NetworkDirection, ObservedProcess, PluginManifest, CONTRACT, MANIFEST_FILE,
};

/// The manifest as it will be installed.
fn manifest() -> PluginManifest {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(MANIFEST_FILE);
    let json = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} should be readable: {error}", path.display()));
    PluginManifest::parse(&json)
        .unwrap_or_else(|error| panic!("{} should be a valid manifest: {error}", path.display()))
}

#[test]
fn the_manifest_says_who_this_is_and_what_it_runs() {
    let manifest = manifest();

    // Accepted by this build, not necessarily on its newest contract. A
    // manifest declaring an older version is read exactly as it always was
    // (`ContractVersion::is_supported`), which is what lets a plugin using none
    // of contract 2's vocabulary stay as it is rather than being reissued for
    // a field it does not use.
    assert!(
        manifest.contract().is_supported(),
        "this build supports up to contract {CONTRACT}, and plugin.json declares {}",
        manifest.contract()
    );
    assert_eq!(
        manifest.id().as_str(),
        "league-of-legends",
        "the identifier is the `source` every event this plugin reports is stamped with, so \
         changing it renames every mark on every timeline that came from it"
    );
    assert_eq!(
        manifest.executable(),
        format!("{}.exe", env!("CARGO_PKG_NAME")),
        "the manifest names the executable this package builds, and cargo decides that name"
    );
}

#[test]
fn it_is_started_for_the_process_that_serves_the_api() {
    // The Live Client Data API is served by the game process itself, not by the
    // client that launches it, so this is the executable the plugin has to be
    // attached to.
    let manifest = manifest();
    let supports = manifest.supports();
    assert!(supports.matches(&ObservedProcess::new("League of Legends.exe", 42)));
    assert!(
        supports.matches(&ObservedProcess::new(
            r"C:\Riot Games\League of Legends\Game\League of Legends.exe",
            42
        )),
        "a process arrives as a path, and the host compares file names"
    );
    assert!(!supports.matches(&ObservedProcess::new("LeagueClient.exe", 42)));
}

#[test]
fn it_declares_the_one_thing_it_does_with_the_network() {
    // docs/privacy.md: a plugin that declares nothing is permitted nothing, and
    // what is declared is what the user is shown and consents to. This plugin
    // connects to one loopback endpoint and does nothing else, and the
    // declaration should say exactly that and no more.
    let manifest = manifest();
    let network = manifest.network();

    let [grant] = network.grants() else {
        panic!("expected exactly one grant, got {:?}", network.grants());
    };
    assert_eq!(grant.class, NetworkClass::Loopback);
    assert_eq!(grant.direction, NetworkDirection::Connect);
    assert_eq!(grant.endpoint, "127.0.0.1:2999");
    assert!(
        !network.leaves_the_machine(),
        "nothing this plugin does reaches a network adapter"
    );
    assert_eq!(
        network.summary(),
        vec![
            "Connects to 127.0.0.1:2999 (this machine only) — reads the Live Client Data API the \
             running game serves"
        ],
        "this is the sentence the user reads before enabling the plugin"
    );
}
