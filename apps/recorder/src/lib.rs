//! The Clipped recording process, minus its entry point.
//!
//! `src/main.rs` is a few lines that parse arguments, start diagnostics and
//! call [`run`]. Everything else is here, in a library target, for two
//! reasons: the argument validation can then be tested against its own types
//! rather than only by spawning a process and reading text, and
//! `examples/shutdown_fixture.rs` can install the same Ctrl+C handling the
//! recorder installs, so the signal path is tested for real.
//!
//! **This is not a public API.** It is `pub` because a library target has to
//! be, not because anything outside this package should depend on it; the
//! desktop application talks to the recorder over IPC and never links to it
//! (ADR 0002). Items here may change without notice.
//!
//! # What the recorder can do today
//!
//! Parse and validate a `record` invocation, stop cleanly when asked to, and
//! report what this machine can encode. It cannot record: the capture, audio
//! and muxer crates are documentation-only stubs and no encoder backend is
//! implemented, so `record` reports that and exits. See [`record`] and
//! [`shutdown`] for exactly where the pipeline plugs in.
//!
//! # Modules
//!
//! | Module | Responsibility |
//! | --- | --- |
//! | [`cli`] | The shape of the command line, and where a new subcommand goes |
//! | [`options`] | The typed values options parse into, and their bounds |
//! | [`config`] | Validating arguments into a [`config::RecordingConfig`] |
//! | [`record`] | The `record` subcommand |
//! | [`list_windows`] | The `list-windows` subcommand |
//! | [`capabilities`] | The `capabilities` subcommand |
//! | [`shutdown`] | Ctrl+C, and the finalisation seam a recording ends through |

pub mod capabilities;
pub mod cli;
pub mod config;
pub mod list_windows;
pub mod options;
pub mod record;
pub mod shutdown;

use std::error::Error;
use std::fmt;

use cli::{Cli, Command};

/// Exit code for a run that did what it was asked.
pub const EXIT_SUCCESS: u8 = 0;

/// Exit code for a run that failed while running.
pub const EXIT_FAILURE: u8 = 1;

/// Exit code for arguments that were rejected.
///
/// The same code clap uses for a usage error, so a script cannot tell a
/// rejected value from a misspelled flag — and does not need to, because both
/// mean "fix the command line".
pub const EXIT_USAGE: u8 = 2;

/// Exit code for a command that is not implemented in this build.
///
/// Distinct from [`EXIT_FAILURE`] so that a script, or the test suite, can tell
/// "this does not exist yet" from "this went wrong".
pub const EXIT_NOT_IMPLEMENTED: u8 = 3;

/// Anything a subcommand can fail with.
#[derive(Debug)]
pub enum RunError {
    /// `record` failed.
    Record(record::RecordError),
    /// `list-windows` failed.
    ListWindows(list_windows::ListWindowsError),
    /// `capabilities` failed.
    Capabilities(capabilities::CapabilitiesError),
}

impl RunError {
    /// The process exit code this failure should produce.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Record(record::RecordError::Configuration(_)) => EXIT_USAGE,
            Self::Record(record::RecordError::CaptureNotImplemented) => EXIT_NOT_IMPLEMENTED,
            Self::Record(record::RecordError::Shutdown(_)) => EXIT_FAILURE,
            // A selector that named no window, or more than one, is a command
            // line to fix; a desktop that could not be enumerated is not.
            Self::ListWindows(list_windows::ListWindowsError::Resolution(_)) => EXIT_USAGE,
            Self::ListWindows(list_windows::ListWindowsError::Enumeration(_)) => EXIT_FAILURE,
            // Not `EXIT_NOT_IMPLEMENTED`, even though no encoder is
            // implemented: detection itself is implemented, and this code means
            // the machine could not be asked. A machine with no encoder at all
            // is a successful run that says so.
            Self::Capabilities(_) => EXIT_FAILURE,
        }
    }

    /// Whether the user should be pointed at `--help`.
    ///
    /// Only for the failures they can fix by reading it. Telling someone to
    /// read the help when the capture engine does not exist would be worse than
    /// saying nothing, and so would telling them to read it when the message
    /// they were just given lists the windows they could have meant — the help
    /// has nothing further to add about a real desktop.
    #[must_use]
    pub fn is_usage_error(&self) -> bool {
        matches!(self, Self::Record(record::RecordError::Configuration(_)))
    }
}

impl fmt::Display for RunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Record(error) => write!(formatter, "{error}"),
            Self::ListWindows(error) => write!(formatter, "{error}"),
            Self::Capabilities(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for RunError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Record(error) => Some(error),
            Self::ListWindows(error) => Some(error),
            Self::Capabilities(error) => Some(error),
        }
    }
}

impl From<record::RecordError> for RunError {
    fn from(error: record::RecordError) -> Self {
        Self::Record(error)
    }
}

impl From<list_windows::ListWindowsError> for RunError {
    fn from(error: list_windows::ListWindowsError) -> Self {
        Self::ListWindows(error)
    }
}

impl From<capabilities::CapabilitiesError> for RunError {
    fn from(error: capabilities::CapabilitiesError) -> Self {
        Self::Capabilities(error)
    }
}

/// Runs the subcommand the command line selected.
///
/// A new subcommand is a variant on [`Command`] and an arm here.
///
/// # Errors
///
/// Whatever the subcommand failed with. See [`RunError::exit_code`] for how
/// each becomes a process exit code.
pub fn run(cli: &Cli) -> Result<(), RunError> {
    match &cli.command {
        Command::Record(args) => record::run(args).map_err(RunError::from),
        Command::ListWindows(args) => list_windows::run(args).map_err(RunError::from),
        Command::Capabilities(args) => capabilities::run(args).map_err(RunError::from),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigError;
    use crate::record::RecordError;

    #[test]
    fn a_rejected_argument_exits_with_the_usage_code_and_offers_help() {
        let error = RunError::from(RecordError::Configuration(ConfigError::NoTarget));
        assert_eq!(error.exit_code(), EXIT_USAGE);
        assert!(error.is_usage_error());
    }

    #[test]
    fn a_missing_capture_engine_has_an_exit_code_of_its_own() {
        let error = RunError::from(RecordError::CaptureNotImplemented);
        assert_eq!(error.exit_code(), EXIT_NOT_IMPLEMENTED);
        assert!(
            !error.is_usage_error(),
            "there is nothing the user can change to fix this"
        );
    }
}
