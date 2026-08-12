//! The one file this plugin writes into somebody else's game, and how it is
//! taken away again.
//!
//! Counter-Strike 2 only posts its state to something that asked, and the way
//! to ask is a `gamestate_integration_*.cfg` in the game's own configuration
//! directory. That is an official, documented mechanism — AGENTS.md section 34
//! is why this plugin exists at all — and it is still a file written into a
//! directory that belongs to the user and to Valve, so three rules hold it.
//!
//! # It is never a side effect
//!
//! Nothing here runs when the plugin is attached to a session. Installing the
//! configuration is a command the user types, or a button somebody presses on
//! their behalf; a plugin that wrote into a game directory the first time the
//! game launched would be doing something the user never asked for, which
//! `docs/privacy.md` does not allow. Attaching without it produces a
//! [`PluginReport::Problem`](clipped_plugins::PluginReport::Problem) naming the
//! command to run, not a silent failure and not an install.
//!
//! # It never touches anything it did not write
//!
//! Counter-Strike loads **every** `gamestate_integration_*.cfg` in that
//! directory, which is how several tools coexist. So:
//!
//! - The file has a name of this plugin's own,
//!   [`CONFIGURATION_FILE`], and nothing else is ever written or removed.
//! - A file of that name that this plugin did not write is left alone and
//!   reported ([`SetupError::NotOurs`]). The test is structural — the service
//!   name at the top of the document — rather than a comment somebody could
//!   copy.
//! - A neighbouring file that already posts to the port this plugin wants is a
//!   refusal, not an overwrite ([`SetupError::PortTaken`]). Two tools posting
//!   to one port is one of them getting the payloads.
//!
//! # What it writes is exactly what is documented
//!
//! [`render`] is the whole file, and `plugins/cs2/README.md` reproduces it. It
//! subscribes to four blocks and no more, because the payload is the thing this
//! plugin reads: a subscription it does not need is data it does not need to be
//! handling.

use core::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::keyvalues::{KeyValues, KeyValuesError};

/// The file this plugin writes, and the only one it ever writes.
pub const CONFIGURATION_FILE: &str = "gamestate_integration_clipped.cfg";

/// The prefix Counter-Strike 2 looks for when it loads integrations.
pub const CONFIGURATION_PREFIX: &str = "gamestate_integration_";

/// The service name at the top of the file, which is how this plugin
/// recognises its own work.
pub const SERVICE_NAME: &str = "Clipped Game State Integration v1";

/// Where Counter-Strike 2 keeps its configuration, under the install root.
pub const CONFIGURATION_DIRECTORY: [&str; 3] = ["game", "csgo", "cfg"];

/// The loopback port this plugin listens on unless told otherwise.
///
/// It matches the endpoint `plugin.json` declares, and
/// `the_manifest_declares_the_port_this_plugin_actually_listens_on` asserts
/// that rather than leaving the two to drift: the declaration a user consents
/// to has to be the socket that gets opened.
pub const DEFAULT_PORT: u16 = 3212;

/// How often Counter-Strike posts, at most, in seconds.
///
/// This is the plugin's timing precision: an event is placed in the middle of
/// the window between two payloads, so a tenth of a second here is a precision
/// of about fifty milliseconds on a live round (`crate::derive`).
const THROTTLE_SECONDS: &str = "0.1";
/// How long the game buffers changes before posting them, in seconds.
const BUFFER_SECONDS: &str = "0.1";
/// How long the game waits for this plugin to answer, in seconds.
const TIMEOUT_SECONDS: &str = "5.0";
/// How often the game posts when nothing at all has changed, in seconds.
///
/// It bounds the window an event can be placed in when a game is idle, and it
/// is what tells a running plugin that Counter-Strike is still there.
const HEARTBEAT_SECONDS: &str = "10.0";

/// Everything the file says, rendered.
///
/// The token is a secret shared between this file and the listening socket:
/// `docs/privacy.md` requires a loopback listener to authenticate what it
/// accepts, because every process on the machine — including a web page in a
/// browser — can reach a loopback port.
#[must_use]
pub fn render(port: u16, token: &str) -> String {
    format!(
        "\
// Written by Clipped's Counter-Strike 2 plugin (clipped-cs2-plugin).
//
// It asks Counter-Strike 2 to post the state below to a port on this machine,
// which is how Clipped knows when you got a kill. It is the only file Clipped
// writes into your game, nothing in it leaves this computer, and deleting it
// stops the plugin working and breaks nothing else. `clipped-cs2-plugin
// uninstall` removes it for you.
//
// The token is how the listener knows a payload came from your game rather
// than from something else on this machine. It is not a Steam credential and
// it is not shared with anybody.
\"{SERVICE_NAME}\"
{{
    \"uri\"       \"http://127.0.0.1:{port}/\"
    \"timeout\"   \"{TIMEOUT_SECONDS}\"
    \"buffer\"    \"{BUFFER_SECONDS}\"
    \"throttle\"  \"{THROTTLE_SECONDS}\"
    \"heartbeat\" \"{HEARTBEAT_SECONDS}\"
    \"auth\"
    {{
        \"token\" \"{token}\"
    }}
    \"data\"
    {{
        \"provider\"           \"1\"
        \"map\"                \"1\"
        \"round\"              \"1\"
        \"player_id\"          \"1\"
        \"player_state\"       \"1\"
        \"player_match_stats\" \"1\"
    }}
}}
"
    )
}

/// What a configuration file says, once it has been read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// Where the file is.
    pub path: PathBuf,
    /// The port Counter-Strike will post to.
    pub port: u16,
    /// The token it will present.
    pub token: String,
}

/// Where Counter-Strike 2's configuration directory is, under an install root.
///
/// Accepts either the install root — the folder Steam's "Browse local files"
/// opens, which contains `game/csgo` — or the configuration directory itself,
/// because both are things a person reasonably has in their clipboard.
///
/// # Errors
///
/// [`SetupError::NotAGameDirectory`], naming both places it looked. A plugin
/// that guessed wrong and created the directory would leave a `cfg` folder in
/// a place Counter-Strike never reads.
pub fn configuration_directory(game_directory: &Path) -> Result<PathBuf, SetupError> {
    let under_root = CONFIGURATION_DIRECTORY
        .iter()
        .fold(game_directory.to_path_buf(), |path, part| path.join(part));
    if under_root.is_dir() {
        return Ok(under_root);
    }
    if game_directory.is_dir() && game_directory.ends_with("cfg") {
        return Ok(game_directory.to_path_buf());
    }
    Err(SetupError::NotAGameDirectory {
        given: game_directory.to_path_buf(),
        looked_for: under_root,
    })
}

/// Writes the configuration file, generating a fresh token.
///
/// `replace` allows an existing file of this plugin's own to be rewritten,
/// which is how a port is changed. It does **not** allow a file this plugin did
/// not write to be replaced; nothing does.
///
/// # Errors
///
/// [`SetupError`], naming the file it would have touched.
pub fn install(
    game_directory: &Path,
    port: u16,
    token: &str,
    replace: bool,
) -> Result<Installed, SetupError> {
    let directory = configuration_directory(game_directory)?;
    let path = directory.join(CONFIGURATION_FILE);

    if path.exists() {
        // Ours to replace, or somebody else's to leave alone.
        let existing = read(&path)?;
        if existing.is_none() {
            return Err(SetupError::NotOurs { path });
        }
        if !replace {
            return Err(SetupError::AlreadyInstalled { path });
        }
    }

    if let Some(neighbour) = neighbour_posting_to(&directory, port)? {
        return Err(SetupError::PortTaken { port, neighbour });
    }

    fs::write(&path, render(port, token)).map_err(|source| SetupError::Write {
        path: path.clone(),
        source,
    })?;

    Ok(Installed {
        path,
        port,
        token: token.to_owned(),
    })
}

/// Removes the configuration file, if it is this plugin's.
///
/// Answers whether there was one. Removing something that is not there is not a
/// failure: a user running `uninstall` twice has got what they asked for.
///
/// # Errors
///
/// [`SetupError::NotOurs`] when a file of that name was written by something
/// else, which is the case this whole module exists to get right.
pub fn uninstall(game_directory: &Path) -> Result<Option<PathBuf>, SetupError> {
    let path = configuration_directory(game_directory)?.join(CONFIGURATION_FILE);
    if !path.exists() {
        return Ok(None);
    }
    if read(&path)?.is_none() {
        return Err(SetupError::NotOurs { path });
    }
    fs::remove_file(&path).map_err(|source| SetupError::Write {
        path: path.clone(),
        source,
    })?;
    Ok(Some(path))
}

/// Reads a configuration file back, if it is this plugin's.
///
/// `Ok(None)` means the file is a valid KeyValues document that some other tool
/// wrote — a perfectly normal thing to find, and never something to overwrite.
///
/// # Errors
///
/// [`SetupError`] when the file cannot be read or is not KeyValues at all.
pub fn read(path: &Path) -> Result<Option<Installed>, SetupError> {
    let text = fs::read_to_string(path).map_err(|source| SetupError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let document = KeyValues::parse(&text).map_err(|source| SetupError::Malformed {
        path: path.to_path_buf(),
        source,
    })?;

    let Some(service) = document.block(SERVICE_NAME) else {
        return Ok(None);
    };
    let uri = service.text("uri").ok_or_else(|| SetupError::Incomplete {
        path: path.to_path_buf(),
        field: "uri",
    })?;
    let port = port_of(uri).ok_or_else(|| SetupError::Incomplete {
        path: path.to_path_buf(),
        field: "uri",
    })?;
    let token = service
        .block("auth")
        .and_then(|auth| auth.text("token"))
        .ok_or_else(|| SetupError::Incomplete {
            path: path.to_path_buf(),
            field: "auth/token",
        })?;

    Ok(Some(Installed {
        path: path.to_path_buf(),
        port,
        token: token.to_owned(),
    }))
}

/// The first neighbouring integration file already posting to `port`.
///
/// Counter-Strike loads every one of them, so two files naming one port means
/// one of the two tools silently gets nothing. A file that cannot be read is
/// skipped rather than refused: it is somebody else's, this is only a courtesy
/// check, and failing an install over a third party's malformed file would be
/// an odd thing to do.
fn neighbour_posting_to(directory: &Path, port: u16) -> Result<Option<PathBuf>, SetupError> {
    let entries = fs::read_dir(directory).map_err(|source| SetupError::Read {
        path: directory.to_path_buf(),
        source,
    })?;

    let mut clashes: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(CONFIGURATION_PREFIX) || name == CONFIGURATION_FILE {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(document) = KeyValues::parse(&text) else {
            continue;
        };
        let posts_here = document.entries().iter().any(|(_, value)| match value {
            crate::keyvalues::KeyValue::Block(block) => {
                block.text("uri").and_then(port_of) == Some(port)
            }
            crate::keyvalues::KeyValue::Text(_) => false,
        });
        if posts_here {
            clashes.push(path);
        }
    }
    // Sorted so that a directory with two clashing files names the same one on
    // every machine and in every run.
    clashes.sort();
    Ok(clashes.into_iter().next())
}

/// The port in `http://127.0.0.1:3212/`.
fn port_of(uri: &str) -> Option<u16> {
    let after_scheme = uri.split_once("//").map_or(uri, |(_, rest)| rest);
    let authority = after_scheme
        .split_once('/')
        .map_or(after_scheme, |(authority, _)| authority);
    authority.rsplit_once(':')?.1.parse().ok()
}

/// What went wrong setting the integration up.
#[derive(Debug)]
pub enum SetupError {
    /// The path given is not Counter-Strike 2's directory.
    NotAGameDirectory {
        /// What the user gave.
        given: PathBuf,
        /// Where the configuration directory would have been.
        looked_for: PathBuf,
    },
    /// A file of this plugin's name exists and something else wrote it.
    NotOurs {
        /// The file, left exactly as it was.
        path: PathBuf,
    },
    /// This plugin's own file is already there.
    AlreadyInstalled {
        /// The file.
        path: PathBuf,
    },
    /// Another tool's integration already posts to this port.
    PortTaken {
        /// The port asked for.
        port: u16,
        /// The file that has it.
        neighbour: PathBuf,
    },
    /// A file could not be read.
    Read {
        /// The file.
        path: PathBuf,
        /// Why.
        source: io::Error,
    },
    /// A file could not be written.
    Write {
        /// The file.
        path: PathBuf,
        /// Why.
        source: io::Error,
    },
    /// A file is not a KeyValues document.
    Malformed {
        /// The file.
        path: PathBuf,
        /// Why.
        source: KeyValuesError,
    },
    /// This plugin's own file is missing something it wrote.
    Incomplete {
        /// The file.
        path: PathBuf,
        /// The key that is not there.
        field: &'static str,
    },
}

impl fmt::Display for SetupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAGameDirectory { given, looked_for } => write!(
                formatter,
                "{} is not Counter-Strike 2's directory: there is no {}. Steam shows it under \
                 Counter-Strike 2 → Manage → Browse local files",
                given.display(),
                looked_for.display()
            ),
            Self::NotOurs { path } => write!(
                formatter,
                "{} was written by something other than Clipped and has been left alone. Move it \
                 aside yourself if you want Clipped to use that name",
                path.display()
            ),
            Self::AlreadyInstalled { path } => write!(
                formatter,
                "{} is already installed. Run `status` to see it, or `install --replace` to write \
                 it again with a new token",
                path.display()
            ),
            Self::PortTaken { port, neighbour } => write!(
                formatter,
                "{} already asks Counter-Strike 2 to post to port {port}, and two integrations on \
                 one port means one of them gets nothing. Choose another with `--port`",
                neighbour.display()
            ),
            Self::Read { path, source } => {
                write!(formatter, "{} could not be read: {source}", path.display())
            }
            Self::Write { path, source } => {
                write!(
                    formatter,
                    "{} could not be written: {source}",
                    path.display()
                )
            }
            Self::Malformed { path, source } => write!(
                formatter,
                "{} is not a Counter-Strike configuration file: {source}",
                path.display()
            ),
            Self::Incomplete { path, field } => write!(
                formatter,
                "{} has no `{field}`. Run `install --replace` to write it again",
                path.display()
            ),
        }
    }
}

impl core::error::Error for SetupError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } | Self::Write { source, .. } => Some(source),
            Self::Malformed { source, .. } => Some(source),
            Self::NotAGameDirectory { .. }
            | Self::NotOurs { .. }
            | Self::AlreadyInstalled { .. }
            | Self::PortTaken { .. }
            | Self::Incomplete { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory that looks like a Counter-Strike 2 install, in a temporary
    /// place of this test's own.
    struct FakeGame {
        root: PathBuf,
    }

    impl FakeGame {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "clipped-cs2-{name}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(root.join("game").join("csgo").join("cfg"))
                .expect("a temporary directory");
            Self { root }
        }

        fn cfg(&self) -> PathBuf {
            self.root.join("game").join("csgo").join("cfg")
        }

        fn write_neighbour(&self, name: &str, contents: &str) -> PathBuf {
            let path = self.cfg().join(name);
            fs::write(&path, contents).expect("a neighbouring file");
            path
        }
    }

    impl Drop for FakeGame {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn what_is_written_is_readable_by_the_plugin_that_wrote_it() {
        let game = FakeGame::new("roundtrip");
        let installed =
            install(&game.root, 3212, "a-token", false).expect("an empty directory takes it");

        assert_eq!(installed.path, game.cfg().join(CONFIGURATION_FILE));
        assert_eq!(
            read(&installed.path).expect("it reads back"),
            Some(installed.clone())
        );

        let text = fs::read_to_string(&installed.path).expect("the file is there");
        assert!(
            text.contains("http://127.0.0.1:3212/"),
            "the game is told to post to loopback and to nothing else:\n{text}"
        );
        assert!(
            text.contains("\"player_match_stats\" \"1\""),
            "the counters a kill is derived from have to be subscribed to:\n{text}"
        );
    }

    #[test]
    fn the_configuration_directory_is_found_from_the_install_root_or_from_itself() {
        let game = FakeGame::new("locate");
        assert_eq!(
            configuration_directory(&game.root).expect("the install root"),
            game.cfg()
        );
        assert_eq!(
            configuration_directory(&game.cfg()).expect("the directory itself"),
            game.cfg()
        );

        let elsewhere = game.root.join("game");
        let refusal = configuration_directory(&elsewhere).expect_err("not a game directory");
        assert!(
            refusal.to_string().contains("Browse local files"),
            "the message should say where to find it: {refusal}"
        );
    }

    #[test]
    fn a_file_of_this_name_that_clipped_did_not_write_is_left_exactly_as_it_was() {
        // The rule this module exists for. Somebody else's configuration under
        // a name Clipped happens to want is not Clipped's to replace.
        let game = FakeGame::new("not-ours");
        let theirs = "\"Some Other Tool\"\n{\n\"uri\" \"http://127.0.0.1:9999/\"\n}\n";
        let path = game.write_neighbour(CONFIGURATION_FILE, theirs);

        let refusal = install(&game.root, 3212, "a-token", true).expect_err("somebody else's file");
        assert!(matches!(refusal, SetupError::NotOurs { .. }), "{refusal}");
        assert_eq!(
            fs::read_to_string(&path).expect("still there"),
            theirs,
            "the file was modified"
        );

        let refusal = uninstall(&game.root).expect_err("somebody else's file");
        assert!(matches!(refusal, SetupError::NotOurs { .. }), "{refusal}");
        assert!(path.exists(), "the file was removed");
    }

    #[test]
    fn another_tools_integration_on_the_same_port_is_a_refusal_not_an_overwrite() {
        let game = FakeGame::new("port-clash");
        let theirs = game.write_neighbour(
            "gamestate_integration_othertool.cfg",
            "\"Other Tool\"\n{\n    \"uri\" \"http://127.0.0.1:3212/\"\n}\n",
        );

        let refusal = install(&game.root, 3212, "a-token", false).expect_err("the port is taken");
        let message = refusal.to_string();
        assert!(matches!(refusal, SetupError::PortTaken { .. }), "{message}");
        assert!(
            message.contains("gamestate_integration_othertool.cfg") && message.contains("--port"),
            "the message should name the file and the way out: {message}"
        );
        assert!(theirs.exists());
        assert!(
            !game.cfg().join(CONFIGURATION_FILE).exists(),
            "nothing should have been written"
        );

        // A different port is fine: several integrations is the normal case.
        install(&game.root, 3213, "a-token", false).expect("a free port");
    }

    #[test]
    fn a_neighbour_that_spells_uri_differently_is_still_on_the_port() {
        // KeyValues keys are case-insensitive to the game that reads them, so a
        // tool writing `URI` posts to that port exactly as one writing `uri`
        // does. Missing it here would be installing a second integration on an
        // occupied port and telling the user it was free.
        let game = FakeGame::new("port-clash-case");
        game.write_neighbour(
            "gamestate_integration_shouty.cfg",
            "\"Other Tool\"\n{\n    \"URI\" \"http://127.0.0.1:3212/\"\n}\n",
        );

        let refusal = install(&game.root, 3212, "a-token", false).expect_err("the port is taken");
        assert!(matches!(refusal, SetupError::PortTaken { .. }), "{refusal}");
        assert!(
            !game.cfg().join(CONFIGURATION_FILE).exists(),
            "nothing should have been written"
        );
    }

    #[test]
    fn a_neighbour_on_a_different_port_is_left_completely_alone() {
        let game = FakeGame::new("neighbour");
        let theirs = game.write_neighbour(
            "gamestate_integration_othertool.cfg",
            "\"Other Tool\"\n{\n    \"uri\" \"http://127.0.0.1:4000/\"\n}\n",
        );
        let before = fs::read_to_string(&theirs).expect("it is there");

        install(&game.root, 3212, "a-token", false).expect("a free port");
        uninstall(&game.root).expect("our own file");

        assert!(theirs.exists(), "a neighbour was removed");
        assert_eq!(fs::read_to_string(&theirs).expect("it is there"), before);
    }

    #[test]
    fn installing_twice_needs_asking_for_and_uninstalling_twice_does_not() {
        let game = FakeGame::new("twice");
        install(&game.root, 3212, "first-token", false).expect("the first install");

        let refusal =
            install(&game.root, 3212, "second-token", false).expect_err("already installed");
        assert!(
            matches!(refusal, SetupError::AlreadyInstalled { .. }),
            "{refusal}"
        );

        let replaced =
            install(&game.root, 3299, "second-token", true).expect("replacing our own file");
        assert_eq!(replaced.port, 3299);
        assert_eq!(replaced.token, "second-token");

        assert!(uninstall(&game.root).expect("our own file").is_some());
        assert!(uninstall(&game.root)
            .expect("nothing to remove is not a failure")
            .is_none());
    }

    #[test]
    fn a_port_is_read_out_of_a_uri_and_nothing_else_is() {
        assert_eq!(port_of("http://127.0.0.1:3212/"), Some(3212));
        assert_eq!(port_of("http://localhost:3212"), Some(3212));
        assert_eq!(port_of("127.0.0.1:3212"), Some(3212));
        assert_eq!(port_of("http://127.0.0.1/"), None);
        assert_eq!(port_of(""), None);
    }
}
