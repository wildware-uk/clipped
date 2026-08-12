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
//! Record a window to a Matroska file, list what can be captured, report what
//! this machine can encode, and serve the desktop application over the IPC
//! protocol (`docs/ipc.md`). `record` resolves its target, captures it,
//! encodes the frames and writes them, and stops cleanly on Ctrl+C leaving a
//! finished file
//! ([issue #126](https://github.com/wildware-uk/clipped/issues/126)).
//!
//! The recording itself belongs to `clipped-session`; this package is the
//! command line over it. There is no audio track yet — see [`record`] and
//! `docs/recorder-cli.md`.
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
//! | [`serve`] | The `serve` subcommand, and the recorder behind the IPC protocol |
//! | [`start_at_login`] | The `start-at-login` subcommand: the opt-in registry value |
//! | [`watch`] | The `watch` subcommand: recording games automatically |
//! | [`recover`] | The `recover` subcommand: the footage a killed recorder left |
//! | [`shutdown`] | Ctrl+C, and the finalisation seam a recording ends through |

pub mod capabilities;
pub mod cli;
pub mod config;
pub mod library;
pub mod list_windows;
pub mod options;
pub mod record;
pub mod recover;
pub mod serve;
pub mod shutdown;
pub mod start_at_login;
pub mod watch;

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

/// Exit code for something this build cannot do yet.
///
/// Distinct from [`EXIT_FAILURE`] so that a script, or the test suite, can tell
/// "this does not exist yet" from "this went wrong". `record` no longer exits
/// with it for the absence of a capture engine — there is one — but it is still
/// what a recording asks for that this build genuinely cannot produce ends
/// with: a resolution that would need a scaler, or a high dynamic range capture
/// no encoder here accepts.
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
    /// `serve` failed.
    Serve(serve::ServeError),
    /// `start-at-login` failed.
    StartAtLogin(start_at_login::StartAtLoginError),
    /// `watch` failed.
    Watch(watch::WatchCommandError),
    /// `recover` failed.
    Recover(recover::RecoverError),
}

impl RunError {
    /// The process exit code this failure should produce.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Record(record::RecordError::Configuration(_)) => EXIT_USAGE,
            // A selector that named no window, or several, is the same class of
            // mistake as a rejected argument: the command line is what has to
            // change, and the message already lists the candidates.
            Self::Record(record::RecordError::Resolution(_)) => EXIT_USAGE,
            // And a `--microphone` that named no device, or several, is the
            // same mistake again: the message says how many matched, and the
            // command line is what has to change.
            Self::Record(record::RecordError::Session(
                clipped_session::SessionError::MicrophoneNotFound { .. },
            )) => EXIT_USAGE,
            // What this build genuinely cannot produce, as opposed to what went
            // wrong while producing it.
            Self::Record(record::RecordError::Session(
                clipped_session::SessionError::ScalingNotSupported { .. }
                | clipped_session::SessionError::UnsupportedPixelFormat { .. }
                | clipped_session::SessionError::AudioDeviceNotSelectable,
            )) => EXIT_NOT_IMPLEMENTED,
            Self::Record(
                record::RecordError::Shutdown(_)
                | record::RecordError::Enumeration(_)
                | record::RecordError::Session(_),
            ) => EXIT_FAILURE,
            // A selector that named no window, or more than one, is a command
            // line to fix; a desktop that could not be enumerated is not.
            Self::ListWindows(list_windows::ListWindowsError::Resolution(_)) => EXIT_USAGE,
            Self::ListWindows(list_windows::ListWindowsError::Enumeration(_)) => EXIT_FAILURE,
            // Not `EXIT_NOT_IMPLEMENTED`, even though no encoder is
            // implemented: detection itself is implemented, and this code means
            // the machine could not be asked. A machine with no encoder at all
            // is a successful run that says so.
            Self::Capabilities(_) => EXIT_FAILURE,
            // Including "a recorder is already listening there", which is a
            // second recorder correctly declining to compete with the first
            // rather than a command line to fix: the endpoint is not something
            // the user chose, and `--endpoint` would be the wrong advice.
            Self::Serve(_) => EXIT_FAILURE,
            // Including "this is not a Windows build": starting at login is a
            // Windows arrangement, and a build without one has not met a
            // command line to fix.
            Self::StartAtLogin(_) => EXIT_FAILURE,
            // A directory that cannot be written to, a catalogue that cannot be
            // read, or detection that stopped. None of them is a command line
            // to fix except the first, and that one names a path the user gave
            // us, so the message is the useful part rather than `--help`.
            Self::Watch(_) => EXIT_FAILURE,
            // `--discard` without `--session`, and a `--session` that names
            // nothing, are both command lines to fix — and the message already
            // says what to type. The rest is a directory that could not be read
            // or a record that could not be rewritten, which no argument fixes.
            Self::Recover(
                recover::RecoverError::DiscardNeedsASession
                | recover::RecoverError::NoSuchRecording { .. },
            ) => EXIT_USAGE,
            Self::Recover(_) => EXIT_FAILURE,
        }
    }

    /// Whether the user should be pointed at `--help`.
    ///
    /// Only for the failures they can fix by reading it. Telling someone to
    /// read the help when a driver failed mid-recording would be worse than
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
            Self::Serve(error) => write!(formatter, "{error}"),
            Self::StartAtLogin(error) => write!(formatter, "{error}"),
            Self::Watch(error) => write!(formatter, "{error}"),
            Self::Recover(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for RunError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Record(error) => Some(error),
            Self::ListWindows(error) => Some(error),
            Self::Capabilities(error) => Some(error),
            Self::Serve(error) => Some(error),
            Self::StartAtLogin(error) => Some(error),
            Self::Watch(error) => Some(error),
            Self::Recover(error) => Some(error),
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

impl From<serve::ServeError> for RunError {
    fn from(error: serve::ServeError) -> Self {
        Self::Serve(error)
    }
}

impl From<start_at_login::StartAtLoginError> for RunError {
    fn from(error: start_at_login::StartAtLoginError) -> Self {
        Self::StartAtLogin(error)
    }
}

impl From<watch::WatchCommandError> for RunError {
    fn from(error: watch::WatchCommandError) -> Self {
        Self::Watch(error)
    }
}

impl From<recover::RecoverError> for RunError {
    fn from(error: recover::RecoverError) -> Self {
        Self::Recover(error)
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
        Command::Serve(args) => serve::run(args).map_err(RunError::from),
        Command::StartAtLogin(args) => start_at_login::run(args).map_err(RunError::from),
        Command::Watch(args) => watch::run(args).map_err(RunError::from),
        Command::Recover(args) => recover::run(args).map_err(RunError::from),
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
    fn what_this_build_cannot_produce_has_an_exit_code_of_its_own() {
        // Distinct from a failure, because a script — and the test suite — has
        // to be able to tell "this does not exist yet" from "this went wrong".
        let error = RunError::from(RecordError::Session(
            clipped_session::SessionError::ScalingNotSupported {
                requested: (1920, 1080),
                captured: (2560, 1440),
            },
        ));
        assert_eq!(error.exit_code(), EXIT_NOT_IMPLEMENTED);
        assert!(
            !error.is_usage_error(),
            "the help has nothing to add: no argument turns a scaler on"
        );
    }

    #[test]
    fn an_output_device_this_build_cannot_name_shares_that_code() {
        // `--system-audio name:Speakers` parses, and there is no way to honour
        // it: WASAPI loopback opens the endpoint Windows is playing through
        // (#316). It is the same class of answer as "there is no scaler" — the
        // feature does not exist — rather than a mistyped argument, so a script
        // can tell it from a failure.
        let error = RunError::from(RecordError::Session(
            clipped_session::SessionError::AudioDeviceNotSelectable,
        ));
        assert_eq!(error.exit_code(), EXIT_NOT_IMPLEMENTED);
    }

    #[test]
    fn a_microphone_that_named_no_device_is_a_command_line_to_fix() {
        // The same code a window selector that matched nothing gets, and for
        // the same reason: what has to change is what was typed.
        let error = RunError::from(RecordError::Session(
            clipped_session::SessionError::MicrophoneNotFound {
                matched: 0,
                available: 3,
            },
        ));
        assert_eq!(error.exit_code(), EXIT_USAGE);
    }

    #[test]
    fn a_recording_that_failed_part_way_through_is_a_failure_and_not_a_usage_error() {
        let error = RunError::from(RecordError::Session(
            clipped_session::SessionError::NoFrames,
        ));
        assert_eq!(error.exit_code(), EXIT_FAILURE);
        assert!(!error.is_usage_error());
    }

    #[test]
    fn a_selector_that_named_no_window_is_a_command_line_to_fix() {
        // The same code `list-windows` uses for the same mistake, so a script
        // cannot tell them apart and does not need to.
        let error = RunError::from(RecordError::Resolution(
            clipped_windows::ResolveError::NoMatch {
                selector: clipped_windows::TargetSelector::ProcessId(4242),
            },
        ));
        assert_eq!(error.exit_code(), EXIT_USAGE);
    }
}
