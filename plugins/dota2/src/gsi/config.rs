//! The file a Valve game reads to learn where to post its state.
//!
//! A Game State Integration configuration is a KeyValues file in the game's own
//! `cfg\gamestate_integration` directory. It names an address, the components
//! the game should describe, how often it may post, and a token it must include
//! (`super::secret`).
//!
//! ```text
//! "Clipped"
//! {
//!     "uri"       "http://127.0.0.1:3213/"
//!     "timeout"   "5.0"
//!     ...
//! }
//! ```
//!
//! # The safety rules, which are the whole of this module
//!
//! Writing into a game's installation is the one thing this plugin does that
//! could damage something a user cares about. So:
//!
//! 1. **One file, named for Clipped, and never any other.** Nothing here lists
//!    the directory, reads a file it did not write, or touches a file whose
//!    name is not the one constant below. Other tools put their own
//!    integrations in the same directory and they are none of this plugin's
//!    business.
//! 2. **A file name, never a path.** [`Installation::new`] refuses a name with
//!    a separator or a `..` in it, so no combination of arguments turns a
//!    configuration writer into a way to write anywhere on the disk.
//! 3. **Written only when it would change.** An installation that already says
//!    what this plugin would say is left exactly as it is — same bytes, same
//!    timestamp — so an unchanged plugin does not rewrite a file inside the
//!    game directory on every launch.
//! 4. **Written atomically.** A temporary file beside it, then a rename. A
//!    half-written configuration file is one the game refuses at start-up, and
//!    the failure would appear a launch later as "Clipped sees no events"
//!    (AGENTS.md section 16).
//! 5. **Never removed.** Detaching does not delete it, because the game reads
//!    it at start-up: deleting on the way out would mean the *next* launch had
//!    no integration at all, which is the launch the user is expecting to work.
//!
//! # When it takes effect
//!
//! **The next time the game starts.** Valve's client reads this directory once,
//! during start-up, so a file written while Dota is running is a file this
//! session will never read. That is a property of the mechanism rather than of
//! this plugin, and it is why [`Installed::Written`] exists as a distinct
//! answer: the user is told to restart the game rather than left watching a
//! timeline that never gains a mark.

use core::fmt;
use std::fs;
use std::io;
use std::path::PathBuf;

use super::secret::AuthToken;

/// The suffix a temporary file is written under before it is renamed into
/// place.
const PENDING_SUFFIX: &str = ".clipped-pending";

/// A game's Game State Integration configuration, as this plugin would write
/// it.
///
/// Game-agnostic on purpose: every field here is something Valve's mechanism
/// defines rather than something Dota defines, so this is the half of a second
/// integration that would not have to be written again (`crate::gsi`).
#[derive(Debug, Clone)]
pub struct Integration {
    title: String,
    uri: String,
    components: Vec<String>,
    timings: Timings,
}

/// How often a game may post, and how long it waits.
///
/// Seconds, as Valve's file spells them. The defaults are
/// [`Timings::responsive`], which is what an integration that wants to place a
/// kill on a video timeline asks for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Timings {
    /// How long the game waits for a reply before it considers the endpoint
    /// gone.
    pub timeout: f32,
    /// How long the game may gather changes into one post.
    pub buffer: f32,
    /// The shortest interval between two posts.
    pub throttle: f32,
    /// How often the game posts even when nothing has changed.
    pub heartbeat: f32,
}

impl Timings {
    /// Timings for an integration that has to know *when* something happened.
    ///
    /// The buffer and the throttle are what bound an event's precision: a
    /// change is reported at most `buffer + throttle` after it happened, and
    /// `super::Cadence` turns the interval that actually elapsed into the
    /// `precision` an event carries. They are not set lower than this because
    /// the cost is paid by the game — every post is work the game does while it
    /// is drawing frames, and an interval below a video frame buys nothing a
    /// video timeline can show.
    ///
    /// The heartbeat is what tells this plugin the game is still there when
    /// nothing is happening, which in Dota is most of a laning phase.
    #[must_use]
    pub const fn responsive() -> Self {
        Self {
            timeout: 5.0,
            buffer: 0.1,
            throttle: 0.1,
            heartbeat: 10.0,
        }
    }
}

impl Integration {
    /// A configuration named `title`, posting `components` to `uri`.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Unquotable`] if any of the values could not be written
    /// into a KeyValues file as a quoted string. Every value here comes from
    /// this plugin rather than from a user, so this is a guard against a future
    /// change rather than against today's caller — and it is a refusal rather
    /// than an escape, because a game's parser is not this crate's to guess at.
    pub fn new<I, S>(title: &str, uri: &str, components: I) -> Result<Self, ConfigError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let components: Vec<String> = components.into_iter().map(Into::into).collect();
        let integration = Self {
            title: title.to_owned(),
            uri: uri.to_owned(),
            components,
            timings: Timings::responsive(),
        };
        for value in integration
            .components
            .iter()
            .map(String::as_str)
            .chain([integration.title.as_str(), integration.uri.as_str()])
        {
            if !is_quotable(value) {
                return Err(ConfigError::Unquotable {
                    value: value.to_owned(),
                });
            }
        }
        Ok(integration)
    }

    /// The same configuration with different timings.
    #[must_use]
    pub fn with_timings(mut self, timings: Timings) -> Self {
        self.timings = timings;
        self
    }

    /// The file's whole contents, with `token` in it.
    ///
    /// The token is the only part that is not the same on every machine, which
    /// is what makes [`Installation::apply`]'s "is this already what we would
    /// write?" comparison a comparison of the whole file.
    ///
    /// # What the header may say
    ///
    /// It names no game, because this module does not know which one it is
    /// writing for (`super`), and it does not tell the reader to delete the
    /// file. Deleting it works exactly until the next time Clipped attaches to
    /// this game, which writes it again — so "delete this file to stop the game
    /// reporting" would be an instruction that undoes itself, which AGENTS.md
    /// section 27 is about. The thing that actually stops it is disabling the
    /// plugin, and that is what it says.
    #[must_use]
    pub fn render(&self, token: &AuthToken) -> String {
        let mut file = String::new();
        file.push_str(
            "// Written by Clipped, so that this game can report what happens in it to\n",
        );
        file.push_str("// Clipped's highlight plugin for it.\n");
        file.push_str("// It names a port on this machine that Clipped listens on while it is\n");
        file.push_str("// recording, and a token the game includes so that Clipped can tell the\n");
        file.push_str("// game's payloads from anything else that finds the port.\n");
        file.push_str("// Deleting this file stops the game reporting only until the next time\n");
        file.push_str(
            "// Clipped records this game, which writes it again. To stop it for good,\n",
        );
        file.push_str("// turn the plugin off in Clipped.\n");
        file.push_str(&format!("\"{}\"\n{{\n", self.title));
        file.push_str(&format!("    \"uri\"       \"{}\"\n", self.uri));
        file.push_str(&format!(
            "    \"timeout\"   \"{:.1}\"\n",
            self.timings.timeout
        ));
        file.push_str(&format!(
            "    \"buffer\"    \"{:.1}\"\n",
            self.timings.buffer
        ));
        file.push_str(&format!(
            "    \"throttle\"  \"{:.1}\"\n",
            self.timings.throttle
        ));
        file.push_str(&format!(
            "    \"heartbeat\" \"{:.1}\"\n",
            self.timings.heartbeat
        ));
        file.push_str("    \"data\"\n    {\n");
        for component in &self.components {
            file.push_str(&format!("        \"{component}\" \"1\"\n"));
        }
        file.push_str("    }\n");
        file.push_str("    \"auth\"\n    {\n");
        file.push_str(&format!("        \"token\" \"{}\"\n", token.as_str()));
        file.push_str("    }\n}\n");
        file
    }
}

/// Where a configuration file goes, and the writing of it.
#[derive(Debug, Clone)]
pub struct Installation {
    directory: PathBuf,
    file_name: String,
}

impl Installation {
    /// A configuration file called `file_name` in `directory`.
    ///
    /// # Errors
    ///
    /// [`ConfigError::NotAFileName`] for anything that is not a plain file
    /// name. This is rule 2 of the module's list, and it is checked here rather
    /// than at the call site so that there is one place a reviewer has to look.
    pub fn new(directory: impl Into<PathBuf>, file_name: &str) -> Result<Self, ConfigError> {
        let plain = !file_name.is_empty()
            && !file_name.contains(['/', '\\', ':'])
            && file_name != "."
            && file_name != ".."
            && !file_name.chars().any(char::is_control);
        if !plain {
            return Err(ConfigError::NotAFileName {
                file_name: file_name.to_owned(),
            });
        }
        Ok(Self {
            directory: directory.into(),
            file_name: file_name.to_owned(),
        })
    }

    /// Where the file is, or would be.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.directory.join(&self.file_name)
    }

    /// Puts `contents` in place, if they are not already there.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Unwritable`], naming the path and the reason. A game
    /// installed somewhere the user cannot write to is the ordinary cause, and
    /// it is reported to the user rather than logged, because the action —
    /// write the file by hand, or install the game somewhere else — is theirs.
    pub fn apply(&self, contents: &str) -> Result<Installed, ConfigError> {
        let path = self.path();
        // Reading our own file to compare is the only read this module does,
        // and a failure to read is not a failure: a file that cannot be read is
        // one that has to be written.
        if fs::read_to_string(&path).is_ok_and(|existing| existing == contents) {
            return Ok(Installed::AlreadyCurrent { path });
        }

        fs::create_dir_all(&self.directory).map_err(|source| ConfigError::Unwritable {
            path: self.directory.clone(),
            source,
        })?;

        let pending = self
            .directory
            .join(format!("{}{PENDING_SUFFIX}", self.file_name));
        fs::write(&pending, contents).map_err(|source| ConfigError::Unwritable {
            path: pending.clone(),
            source,
        })?;
        // `rename` replaces an existing file on Windows only through
        // `MoveFileEx`, which is what `fs::rename` uses; on a failure the
        // pending file is removed so that a directory the user browses is not
        // left with a half-named artefact of a failed launch.
        if let Err(source) = fs::rename(&pending, &path) {
            let _ = fs::remove_file(&pending);
            return Err(ConfigError::Unwritable { path, source });
        }
        Ok(Installed::Written { path })
    }
}

/// What applying a configuration did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installed {
    /// The file already said exactly this, and was not touched.
    AlreadyCurrent {
        /// Where it is.
        path: PathBuf,
    },
    /// It was written. **The game will not read it until it is restarted.**
    Written {
        /// Where it is now.
        path: PathBuf,
    },
}

/// Why a configuration could not be written.
#[derive(Debug)]
pub enum ConfigError {
    /// A value could not be written into a KeyValues file as a quoted string.
    Unquotable {
        /// What was offered.
        value: String,
    },
    /// The file name was a path, or was not a name at all.
    NotAFileName {
        /// What was offered.
        file_name: String,
    },
    /// The file could not be written where it has to go.
    Unwritable {
        /// What could not be written.
        path: PathBuf,
        /// Why not.
        source: io::Error,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unquotable { value } => write!(
                formatter,
                "`{value}` cannot be written into a game configuration file as a quoted string"
            ),
            Self::NotAFileName { file_name } => write!(
                formatter,
                "`{file_name}` is not a plain file name, and a configuration writer that accepted \
                 a path could write anywhere on this machine"
            ),
            Self::Unwritable { path, source } => write!(
                formatter,
                "the Game State Integration configuration could not be written to {}: {source}",
                path.display()
            ),
        }
    }
}

impl core::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Unquotable { .. } | Self::NotAFileName { .. } => None,
            Self::Unwritable { source, .. } => Some(source),
        }
    }
}

/// Whether a value can be written between two quotation marks unchanged.
fn is_quotable(value: &str) -> bool {
    !value.is_empty()
        && !value.contains(['"', '\\'])
        && !value.chars().any(char::is_control)
        && value.is_ascii()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token() -> AuthToken {
        AuthToken::parse("abcdefghijklmnopqrstuvwx").expect("a well-formed token")
    }

    fn integration() -> Integration {
        Integration::new(
            "Clipped",
            "http://127.0.0.1:3213/",
            ["provider", "map", "player", "hero"],
        )
        .expect("every value is quotable")
    }

    use crate::test_support::scratch_directory;

    #[test]
    fn the_rendered_file_is_the_keyvalues_a_valve_game_reads() {
        let rendered = integration().render(&token());

        assert!(rendered.contains("\"Clipped\"\n{\n"));
        assert!(rendered.contains("\"uri\"       \"http://127.0.0.1:3213/\""));
        assert!(rendered.contains("\"heartbeat\" \"10.0\""));
        assert!(rendered.contains("        \"player\" \"1\""));
        assert!(rendered.contains("        \"token\" \"abcdefghijklmnopqrstuvwx\""));
        assert_eq!(
            rendered.matches('{').count(),
            rendered.matches('}').count(),
            "the braces have to balance or the game refuses the file: {rendered}"
        );
    }

    #[test]
    fn a_value_that_would_break_the_file_is_refused_rather_than_escaped() {
        // KeyValues escaping is Valve's business, not this crate's. Refusing is
        // the honest answer to a value we cannot write.
        assert!(matches!(
            Integration::new("Clip\"ped", "http://127.0.0.1:3213/", ["map"]),
            Err(ConfigError::Unquotable { .. })
        ));
        assert!(matches!(
            Integration::new("Clipped", "http://127.0.0.1:3213/\\", ["map"]),
            Err(ConfigError::Unquotable { .. })
        ));
        assert!(matches!(
            Integration::new("Clipped", "http://127.0.0.1:3213/", ["ma\np"]),
            Err(ConfigError::Unquotable { .. })
        ));
    }

    #[test]
    fn a_file_name_that_is_a_path_is_refused() {
        // Rule 2. Without this, a configuration directory taken from anywhere
        // and a file name taken from anywhere would compose into a write to
        // any path on the machine.
        for escape in ["../evil.cfg", "sub\\evil.cfg", "sub/evil.cfg", "C:evil", ""] {
            assert!(
                matches!(
                    Installation::new("C:\\games", escape),
                    Err(ConfigError::NotAFileName { .. })
                ),
                "`{escape}` should not be accepted as a file name"
            );
        }
        assert!(Installation::new("C:\\games", "gamestate_integration_clipped.cfg").is_ok());
    }

    #[test]
    fn an_unchanged_configuration_is_not_rewritten_and_a_changed_one_is() {
        let directory = scratch_directory("config");
        let installation =
            Installation::new(directory.join("gamestate_integration"), "clipped.cfg")
                .expect("a plain file name");
        let contents = integration().render(&token());

        let first = installation
            .apply(&contents)
            .expect("the directory can be created and the file written");
        assert_eq!(
            first,
            Installed::Written {
                path: installation.path()
            },
            "the directory did not exist, so this had to create it"
        );
        assert_eq!(
            fs::read_to_string(installation.path()).expect("the file is there"),
            contents
        );

        let again = installation.apply(&contents).expect("nothing to do");
        assert_eq!(
            again,
            Installed::AlreadyCurrent {
                path: installation.path()
            },
            "an unchanged plugin must not rewrite a file inside the game's directory on every \
             launch"
        );

        let changed = integration()
            .with_timings(Timings {
                heartbeat: 30.0,
                ..Timings::responsive()
            })
            .render(&token());
        assert_eq!(
            installation.apply(&changed).expect("it can be replaced"),
            Installed::Written {
                path: installation.path()
            }
        );
        assert_eq!(
            fs::read_to_string(installation.path()).expect("the file is there"),
            changed
        );
    }

    #[test]
    fn the_header_does_not_tell_the_user_to_do_something_that_undoes_itself() {
        // The file said "Delete this file to stop the game reporting to
        // Clipped", and the next attach wrote it straight back. An instruction
        // that does not survive the software saying it is worse than no
        // instruction (AGENTS.md section 27), so this test is both halves: the
        // file does come back, and the header does not claim otherwise.
        let directory = scratch_directory("deleted");
        let installation = Installation::new(&directory, "gamestate_integration_clipped.cfg")
            .expect("a plain file name");
        let contents = integration().render(&token());

        installation.apply(&contents).expect("it is written");
        fs::remove_file(installation.path()).expect("and the user deletes it");
        assert_eq!(
            installation.apply(&contents).expect("the next attach"),
            Installed::Written {
                path: installation.path()
            },
            "the next attach writes it again, which is what the header has to be honest about"
        );

        let header: String = contents
            .lines()
            .take_while(|line| line.starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !header.to_lowercase().contains("delete this file to stop"),
            "the header promises something the line above disproves: {header}"
        );
        assert!(
            header.contains("only until the next time"),
            "it has to say what deleting it actually buys: {header}"
        );
        assert!(
            header.contains("turn the plugin off in Clipped"),
            "and what does stop it: {header}"
        );
    }

    #[test]
    fn the_rendered_file_names_no_particular_game() {
        // `super`'s claim is that nothing in `crate::gsi` knows which game it
        // is serving, which is what makes it a module that can be moved to a
        // crate two plugin binaries link rather than copied. A header with
        // "Dota 2" written into it is that claim being untrue in the one place
        // a user reads.
        let rendered = integration().render(&token());
        for game in ["Dota", "dota", "Counter-Strike", "cs2"] {
            assert!(
                !rendered.contains(game),
                "`{game}` should not appear in a game-agnostic configuration writer's output: \
                 {rendered}"
            );
        }
    }

    #[test]
    fn nothing_else_in_the_games_configuration_directory_is_touched() {
        // Rule 1. Other tools put their own integrations in this directory, and
        // a plugin that tidied, rewrote or removed one would be destroying
        // somebody else's configuration (AGENTS.md section 56).
        let directory = scratch_directory("neighbours");
        let neighbour = directory.join("gamestate_integration_someone_else.cfg");
        fs::write(&neighbour, "\"Someone Else\"\n{\n}\n").expect("a neighbour can be written");

        let installation = Installation::new(&directory, "gamestate_integration_clipped.cfg")
            .expect("a plain file name");
        installation
            .apply(&integration().render(&token()))
            .expect("the file is written");
        installation
            .apply(&integration().render(&token()))
            .expect("and then left alone");

        assert_eq!(
            fs::read_to_string(&neighbour).expect("the neighbour is still there"),
            "\"Someone Else\"\n{\n}\n"
        );
        let mut names: Vec<String> = fs::read_dir(&directory)
            .expect("the directory can be listed")
            .map(|entry| {
                entry
                    .expect("an entry")
                    .file_name()
                    .to_string_lossy()
                    .into()
            })
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "gamestate_integration_clipped.cfg".to_owned(),
                "gamestate_integration_someone_else.cfg".to_owned()
            ],
            "the pending file must not be left behind, and nothing else may appear"
        );
    }
}
