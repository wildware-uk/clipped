//! The Dota 2 highlight plugin: everything the plugin does, without the process
//! around it.
//!
//! Dota 2 has **Game State Integration**. A configuration file in the game's
//! own directory names a local address, and the game POSTs a JSON description
//! of what it can see to that address whenever it changes. It is Valve's own
//! documented mechanism, it is the same one Counter-Strike 2 uses, and it is
//! exactly what AGENTS.md section 34 asks an integration to be built on: no DLL
//! injection, no reading the game's memory, nothing that resembles an
//! anti-cheat bypass. A user's Dota account is worth more than a highlight, and
//! this plugin never touches anything but a file the game reads and a socket
//! the game posts to.
//!
//! `docs/plugin-api.md` is the contract this is written against.
//!
//! # Two halves, deliberately
//!
//! | Module | Knows about |
//! | --- | --- |
//! | [`gsi`] | Sockets, a configuration file, an auth token, and how often a state arrives. **Nothing about Dota.** |
//! | [`dota`] | What `DOTA_GAMERULES_STATE_POST_GAME` means. **Nothing about sockets or files.** |
//!
//! That split is the whole of this crate's structure, and it is there because
//! Counter-Strike 2's integration ([issue #70](https://github.com/wildware-uk/clipped/issues/70))
//! is the same mechanism with a different vocabulary. Writing the transport,
//! the configuration installation and the payload cadence a second time when
//! #70 lands is precisely what AGENTS.md section 55 forbids, so [`gsi`] is
//! written to be *moved* rather than copied: it names no Dota type, reads no
//! Dota field and has its own tests.
//!
//! **Where it should end up.** A crate — `crates/gsi` — that plugin binaries
//! link and the recorder does not. It cannot go into `clipped-plugins`: that is
//! the *host* side, linked into the recorder, and putting a listening socket
//! and a configuration-file writer in it would put both inside the process that
//! is recording (ADR 0002). It is not a crate yet because there is exactly one
//! caller, and an abstraction with one implementation is the thing AGENTS.md
//! section 1 warns about — the second caller is what makes the extraction a
//! move rather than a guess. #70 is asked to do it in
//! [its own issue](https://github.com/wildware-uk/clipped/issues/70).
//!
//! What could **not** be shared, and what #70 will have to write for itself:
//! the shape of the state blob and every rule about what a change in it means.
//! Dota's `map.game_state` is a nine-valued enumeration of game rules states;
//! Counter-Strike's is rounds and phases. That is not a difference an
//! abstraction can absorb, and pretending otherwise would produce a
//! configurable diff engine nobody can read.
//!
//! # What it reports
//!
//! [`dota::Watcher`] is the table of what maps to a standard event kind, what
//! is reported under this plugin's own namespace, and — as importantly — what
//! this plugin deliberately does not claim to see.
//!
//! # The network
//!
//! One loopback listener, on the address the manifest declares, authenticated
//! with a token the plugin generated and wrote into the game's configuration
//! file. `docs/privacy.md` requires all three of those: loopback rather than a
//! wildcard bind, declared before the user enables the plugin, and
//! authenticated because a socket on `127.0.0.1` is reachable by every other
//! process on the machine — including a web page in a browser.

pub mod dota;
pub mod gsi;

#[cfg(test)]
mod test_support;

/// Where this plugin listens for Dota 2's Game State Integration payloads.
///
/// Fixed rather than chosen at start-up, and that is forced rather than lazy:
/// the address is written into a configuration file the game reads **when it
/// starts**, and it is declared in `plugin.json`, which is what the user
/// consents to. A port picked at run time would be a port the running game was
/// never told about and a declaration that could not name an endpoint.
///
/// `3213` is one above the port `docs/plugin-api.md` uses in its
/// Counter-Strike 2 example, so that the two plugins can run at once — which
/// they will, because two games can be installed and only one of them has to be
/// running for a plugin to be startable.
pub const LISTEN_ADDRESS: &str = "127.0.0.1:3213";

/// The identifier this plugin is known by, and the namespace its own event
/// names carry.
///
/// One string for both, by the convention `docs/plugin-api.md` sets out: an
/// unexplained mark on a timeline should be traceable to the plugin that made
/// it without a registry to consult. It is checked against `plugin.json` in
/// this crate's tests, because two places saying the same thing is two places
/// that can disagree.
pub const PLUGIN_ID: &str = "dota-2";
