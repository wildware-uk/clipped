//! Where the recorder listens, and how a client reaches it.
//!
//! The transport is a **Windows named pipe**, restricted by its security
//! descriptor to the account that created it and created with
//! `PIPE_REJECT_REMOTE_CLIENTS`. `docs/ipc.md` sets out the comparison against
//! a loopback socket in full; the short version is that a loopback listener is
//! reachable by every process on the machine including other signed-in users
//! and any web page in a browser, needs a port that can be taken or firewalled,
//! and — by `docs/privacy.md`'s definition — is network communication that has
//! to be declared and authenticated. A named pipe is none of those things: it
//! carries an access-control list, it has a name rather than a port, and
//! `docs/privacy.md` names it explicitly as *not* network communication.
//!
//! # What this module is, and what it is not
//!
//! [`Endpoint`] and [`TransportError`] are platform-independent, because the
//! naming rules and the failure vocabulary are part of the protocol rather than
//! part of Windows. Everything that touches a pipe is in `transport/windows.rs`
//! and nothing above this module names a Windows type (AGENTS.md section 5).
//!
//! A [`Connection`] is a `Read + Write`, and that is the whole of the contract
//! the rest of the crate depends on. It is what lets the framing, the
//! handshake and the dispatch loop be tested against a byte buffer rather than
//! against a pipe.

use std::fmt;
use std::io;

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::{connect, Connection, Listener, ListenerStopper};

#[cfg(not(windows))]
mod unsupported;

#[cfg(not(windows))]
pub use unsupported::{connect, Connection, Listener, ListenerStopper};

/// The prefix every Clipped pipe name is given.
///
/// A client is never given a path, only a name, and the path is built here. A
/// path taken from configuration could name `\\some-server\pipe\…`, which is a
/// named pipe over SMB on another machine — the one shape of this transport
/// that would genuinely be network communication (`docs/privacy.md`).
pub const PIPE_PATH_PREFIX: &str = r"\\.\pipe\";

/// The stem of the default endpoint name, before the session discriminator.
pub const DEFAULT_ENDPOINT_STEM: &str = "clipped-recorder";

/// The longest endpoint name accepted.
///
/// Windows allows 256 characters for the whole path. This leaves room for the
/// prefix with a margin, and anything approaching it is not a name anybody
/// typed.
pub const MAX_ENDPOINT_NAME_LENGTH: usize = 200;

/// Where a recorder listens.
///
/// Constructed rather than passed around as a string, so that the validation
/// happens once, at the edge, and every later use is known to be a local pipe
/// name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    name: String,
}

impl Endpoint {
    /// The endpoint a recorder uses when nobody named one.
    ///
    /// `clipped-recorder.<session>`, where the session is the Windows terminal
    /// session this process belongs to. The pipe namespace is machine-wide, so
    /// without the discriminator two people signed in at once — one at the
    /// keyboard, one over Remote Desktop, or the same machine after a fast user
    /// switch — would be racing for one name, and the second recorder to start
    /// would find the first one holding it. The access-control list would keep
    /// them from reading each other's messages; it would not stop them
    /// colliding.
    ///
    /// # Errors
    ///
    /// [`TransportError::Platform`] if Windows would not say which session this
    /// process is in, or [`TransportError::Unsupported`] off Windows.
    pub fn for_this_session() -> Result<Self, TransportError> {
        let session = current_session_id()?;
        Self::named(&format!("{DEFAULT_ENDPOINT_STEM}.{session}"))
    }

    /// An endpoint with a name of your own.
    ///
    /// Useful for running a second recorder beside the one that starts at
    /// login — a development build, or a test that must not touch the
    /// recorder the person at the keyboard is using.
    ///
    /// # Errors
    ///
    /// [`TransportError::InvalidEndpointName`] if the name is empty, longer
    /// than [`MAX_ENDPOINT_NAME_LENGTH`], or contains anything but ASCII
    /// letters, digits, `-`, `_` and `.`. The character set is narrow on
    /// purpose: a backslash would let a name reach into another machine's pipe
    /// namespace, and the rest are excluded because a name nobody can type is a
    /// name nobody can report in a bug.
    pub fn named(name: &str) -> Result<Self, TransportError> {
        if name.is_empty() || name.len() > MAX_ENDPOINT_NAME_LENGTH {
            return Err(TransportError::InvalidEndpointName {
                name: name.to_owned(),
                reason: format!("a name must be 1 to {MAX_ENDPOINT_NAME_LENGTH} characters"),
            });
        }

        if let Some(bad) = name.chars().find(
            |character| !matches!(character, 'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.'),
        ) {
            return Err(TransportError::InvalidEndpointName {
                name: name.to_owned(),
                reason: format!(
                    "`{bad}` is not allowed; a name may contain letters, digits, `-`, `_` and `.`"
                ),
            });
        }

        Ok(Self {
            name: name.to_owned(),
        })
    }

    /// The name, without the `\\.\pipe\` prefix.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The full pipe path, which is what Windows is given.
    #[must_use]
    pub fn path(&self) -> String {
        format!("{PIPE_PATH_PREFIX}{}", self.name)
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.path())
    }
}

/// Why a connection could not be made, accepted or listened for.
#[derive(Debug)]
pub enum TransportError {
    /// The name would not be a usable local pipe name.
    InvalidEndpointName {
        /// What was asked for.
        name: String,
        /// Why it was refused, in a form that can be shown to whoever typed it.
        reason: String,
    },
    /// Something is already listening on that endpoint.
    ///
    /// Normally the recorder that started at login. The second one should say
    /// so and stop, rather than competing for the name.
    EndpointInUse {
        /// Where.
        endpoint: String,
    },
    /// Nothing is listening on that endpoint.
    ///
    /// The recorder is not running, which is a state the desktop application
    /// has to render honestly rather than papering over ([ADR
    /// 0002](../../../docs/adr/0002-separate-recorder-process.md)).
    NotListening {
        /// Where nothing was found.
        endpoint: String,
    },
    /// The endpoint exists and this process is not allowed to open it.
    ///
    /// The security descriptor doing its job: the pipe belongs to another
    /// account.
    AccessDenied {
        /// Where.
        endpoint: String,
    },
    /// The platform refused an operation for some other reason.
    Platform(io::Error),
    /// This build has no local transport, because it was not built for
    /// Windows.
    Unsupported,
}

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEndpointName { name, reason } => {
                write!(
                    formatter,
                    "`{name}` is not a usable endpoint name: {reason}"
                )
            }
            Self::EndpointInUse { endpoint } => write!(
                formatter,
                "another recorder is already listening on {endpoint}"
            ),
            Self::NotListening { endpoint } => {
                write!(formatter, "no recorder is listening on {endpoint}")
            }
            Self::AccessDenied { endpoint } => write!(
                formatter,
                "{endpoint} belongs to another account and cannot be opened from here"
            ),
            Self::Platform(error) => write!(formatter, "the local transport failed: {error}"),
            Self::Unsupported => {
                formatter.write_str("this build has no local transport; it is not a Windows build")
            }
        }
    }
}

impl std::error::Error for TransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Platform(error) => Some(error),
            _ => None,
        }
    }
}

/// The terminal session this process belongs to.
#[cfg(windows)]
fn current_session_id() -> Result<u32, TransportError> {
    windows::current_session_id()
}

/// Always an error: there is no session, because there is no transport.
#[cfg(not(windows))]
fn current_session_id() -> Result<u32, TransportError> {
    Err(TransportError::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_becomes_a_local_pipe_path_and_nothing_else() {
        let endpoint = Endpoint::named("clipped-recorder.1").expect("that is a valid name");
        assert_eq!(endpoint.name(), "clipped-recorder.1");
        assert_eq!(endpoint.path(), r"\\.\pipe\clipped-recorder.1");
    }

    #[test]
    fn a_name_that_could_reach_another_machine_is_refused() {
        // The one shape of this transport that would genuinely be network
        // communication: `\\server\pipe\x` is a named pipe over SMB
        // (docs/privacy.md). A name cannot contain the separator that would
        // make one.
        for name in [
            r"..\..\server\pipe\clipped",
            r"\\server\pipe\clipped",
            "clipped/../other",
            "clipped recorder",
        ] {
            let error = Endpoint::named(name).expect_err("that should be refused");
            assert!(
                matches!(error, TransportError::InvalidEndpointName { .. }),
                "unexpected error for {name}: {error}"
            );
        }
    }

    #[test]
    fn an_empty_or_enormous_name_is_refused() {
        assert!(Endpoint::named("").is_err());
        assert!(Endpoint::named(&"a".repeat(MAX_ENDPOINT_NAME_LENGTH + 1)).is_err());
        assert!(Endpoint::named(&"a".repeat(MAX_ENDPOINT_NAME_LENGTH)).is_ok());
    }

    #[test]
    fn the_refusal_says_which_character_was_the_problem() {
        let error = Endpoint::named("clipped:recorder").expect_err("a colon is not allowed");
        assert!(
            error.to_string().contains(':'),
            "the message should name the character: {error}"
        );
    }
}
