//! What this plugin declares about itself, checked against the host that reads
//! it.
//!
//! `plugin.json` is a permission document: it is what the user is shown before
//! they enable the plugin, and it is what the host refuses to run the plugin
//! without (`docs/plugin-api.md`, "The manifest"). Everything in it is also
//! stated somewhere in the code — the identifier, the address, the executable's
//! name — and two places saying the same thing is two places that can disagree.
//! This is where they are compared.

use std::path::Path;

use clipped_dota2_plugin::{LISTEN_ADDRESS, PLUGIN_ID};
use clipped_plugins::{NetworkClass, NetworkDirection, ObservedProcess, PluginManifest, CONTRACT};

fn manifest() -> PluginManifest {
    PluginManifest::parse(include_str!("../plugin.json"))
        .expect("this plugin's own manifest should be one the host accepts")
}

#[test]
fn the_manifest_is_one_this_build_of_the_host_can_read() {
    let manifest = manifest();
    assert_eq!(manifest.contract(), CONTRACT);
    assert_eq!(manifest.id().as_str(), PLUGIN_ID);
    assert_eq!(manifest.name(), "Dota 2");
    assert!(
        !manifest.description().is_empty(),
        "the description is what the user reads in the plugin manager"
    );
}

#[test]
fn the_user_is_told_it_writes_into_their_game_before_they_enable_it() {
    // This plugin does two things to the machine, and contract 1 has a typed
    // declaration for only one of them: `network` covers the loopback socket,
    // and nothing covers a file written into the user's Dota 2 installation.
    // Until there is a field for it
    // ([#343](https://github.com/wildware-uk/clipped/issues/343)), the
    // description carries it — that is the other thing the plugin manager
    // shows before the user enables anything, and a permission the user is
    // never told about is a permission nobody granted (`docs/privacy.md`).
    //
    // The assertion is on the *manifest*, not on the source string, so a
    // description edited down to one tidy sentence fails here rather than
    // quietly removing the only disclosure there is.
    let description = manifest().description().to_lowercase();
    assert!(
        description.contains("writes one configuration file"),
        "the file this plugin writes into a user's game has to be in what they are shown: \
         {description}"
    );
    assert!(
        description.contains("dota 2 installation"),
        "and it has to say where: {description}"
    );
}

#[test]
fn the_manifest_names_the_executable_this_package_actually_builds() {
    // The failure this catches is a rename: `Cargo.toml` produces
    // `clipped-dota2-plugin.exe`, the manifest names a file, and a plugin whose
    // manifest names an executable that is not there is refused by
    // `clipped_plugins::discover` with nothing but a message in a log to say
    // why.
    let built = Path::new(env!("CARGO_BIN_EXE_clipped-dota2-plugin"))
        .file_name()
        .expect("the built binary has a file name")
        .to_string_lossy()
        .into_owned();

    assert_eq!(manifest().executable(), built);
}

#[test]
fn it_supports_dota_and_nothing_else() {
    let manifest = manifest();
    assert!(manifest
        .supports()
        .matches(&ObservedProcess::new("dota2.exe", 4242)));
    assert!(
        manifest.supports().matches(&ObservedProcess::new(
            r"D:\Steam\dota 2 beta\game\bin\win64\DOTA2.EXE",
            1
        )),
        "a launched process arrives as a path, and Windows paths are compared without case"
    );
    assert!(!manifest
        .supports()
        .matches(&ObservedProcess::new("cs2.exe", 4242)));
}

#[test]
fn the_network_it_declares_is_the_socket_it_actually_binds() {
    // The declaration is what the user consents to (`docs/privacy.md`). If the
    // constant the code binds and the endpoint the manifest declares can drift
    // apart, the consent is for a port nothing listens on and the listener is
    // on a port nobody agreed to.
    let manifest = manifest();
    let grants = manifest.network().grants();

    assert_eq!(grants.len(), 1, "one socket, and only one");
    assert_eq!(grants[0].class, NetworkClass::Loopback);
    assert_eq!(grants[0].direction, NetworkDirection::Listen);
    assert_eq!(grants[0].endpoint, LISTEN_ADDRESS);
    assert!(
        !manifest.network().leaves_the_machine(),
        "nothing this plugin does reaches a network adapter"
    );
}

#[test]
fn what_the_user_is_shown_before_enabling_it_is_a_sentence() {
    // Not a permissions grid, and not a port number on its own: privacy.md
    // asks for one line per grant, in plain terms.
    assert_eq!(
        manifest().network().summary(),
        vec![format!(
            "Listens on {LISTEN_ADDRESS} (this machine only) — receives Dota 2 game state from \
             Dota 2 on this computer"
        )]
    );
}
