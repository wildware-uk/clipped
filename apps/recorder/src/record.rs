//! The `record` subcommand.
//!
//! What this does today, stated plainly, because the gap matters: it validates
//! the arguments into a [`RecordingConfig`], logs what it resolved them to,
//! installs the Ctrl+C handler, runs the shutdown seam with a finalisation hook
//! — and then reports that there is no capture engine to put between them.
//!
//! There genuinely is not one. `crates/audio` and `crates/muxer` contain module
//! documentation and no code, `crates/capture` has an interface with no backend
//! behind it, and `crates/encoder` can detect an encoder without being able to
//! drive one — see [`crate::capabilities`], which is what that detection is
//! good for today. Printing
//! "recording…" and producing nothing, or writing an empty file, would be worse
//! than saying so (AGENTS.md sections 27 and 54).

use std::error::Error;
use std::fmt;

use clipped_logging::RedactedPath;

use crate::cli::RecordArgs;
use crate::config::{ConfigError, RecordingConfig};
use crate::shutdown::{install_ctrl_c_handler, run_until_shutdown, CtrlCError, ShutdownSignal};

/// The milestone that delivers capture, named in the message the user sees so
/// that it can be looked up.
pub const CAPTURE_MILESTONE: &str = "M1 - Recording Engine";

/// Why `record` did not record.
#[derive(Debug)]
pub enum RecordError {
    /// The arguments did not describe a recording that could be made.
    Configuration(ConfigError),
    /// The Ctrl+C handler could not be installed, so a recording could not be
    /// stopped cleanly.
    Shutdown(CtrlCError),
    /// The arguments were fine and there is no capture engine to run them.
    CaptureNotImplemented,
}

impl fmt::Display for RecordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(error) => write!(formatter, "{error}"),
            Self::Shutdown(error) => write!(formatter, "{error}"),
            Self::CaptureNotImplemented => write!(
                formatter,
                "the arguments are valid, but this build cannot record: the capture engine \
                 is not implemented yet. It is milestone {CAPTURE_MILESTONE} — capture \
                 backend issue #11, encoder issue #15, at \
                 https://github.com/wildware-uk/clipped/issues. No recording was written."
            ),
        }
    }
}

impl Error for RecordError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Configuration(error) => Some(error),
            Self::Shutdown(error) => Some(error),
            Self::CaptureNotImplemented => None,
        }
    }
}

impl From<ConfigError> for RecordError {
    fn from(error: ConfigError) -> Self {
        Self::Configuration(error)
    }
}

impl From<CtrlCError> for RecordError {
    fn from(error: CtrlCError) -> Self {
        Self::Shutdown(error)
    }
}

/// Validates `args` and runs a recording.
///
/// # Errors
///
/// [`RecordError::Configuration`] if the arguments cannot describe a recording,
/// [`RecordError::Shutdown`] if the process could not be made interruptible,
/// and [`RecordError::CaptureNotImplemented`] every time otherwise, until the
/// capture pipeline exists.
pub fn run(args: &RecordArgs) -> Result<(), RecordError> {
    let config = RecordingConfig::resolve(args)?;
    log_configuration(&config);

    let signal = ShutdownSignal::new();
    install_ctrl_c_handler(&signal)?;
    tracing::debug!("Ctrl+C will stop the recording at the next frame boundary");

    // The seam. When the pipeline exists, the body below creates the output
    // file's directory — validation deliberately does not, so that a run which
    // records nothing leaves nothing behind (`config::resolve_output`) — opens
    // the capture, encoder, audio and muxer sessions, and runs the frame loop
    // until the signal is raised; the hook flushes the encoder and closes the
    // container. Both halves are real today — what is missing is the pipeline
    // between them, which is why the body returns immediately.
    run_until_shutdown(
        &signal,
        |_signal| Err(RecordError::CaptureNotImplemented),
        |reason| {
            tracing::info!(
                reason = %reason,
                "finalisation hook ran; there was no capture session to close"
            );
        },
    )
}

/// Records what the arguments resolved to, once, before anything uses them.
///
/// This is the line that answers "what was it actually asked to do?" in a bug
/// report, so it carries every resolved value. The output path goes through
/// [`RedactedPath`] because it normally contains the account name, which
/// AGENTS.md section 14 keeps out of logs; the digest still makes two lines
/// about the same file correlatable (docs/logging.md).
fn log_configuration(config: &RecordingConfig) {
    tracing::info!(
        target_selector = %config.target,
        output = %RedactedPath::new(&config.output),
        overwrite = config.overwrite,
        resolution = %config.resolution,
        framerate = config.framerate.frames_per_second(),
        codec = %config.codec,
        encoder = %config.encoder,
        microphone = %config.microphone,
        system_audio = %config.system_audio,
        "resolved recording configuration"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_configuration_error_is_reported_as_itself_rather_than_wrapped_in_jargon() {
        let error = RecordError::from(ConfigError::ZeroProcessId);
        assert_eq!(error.to_string(), ConfigError::ZeroProcessId.to_string());
        assert!(error.source().is_some(), "the cause should stay reachable");
    }

    #[test]
    fn the_not_implemented_message_names_the_milestone_and_says_no_recording_was_written() {
        let message = RecordError::CaptureNotImplemented.to_string();
        assert!(
            message.contains(CAPTURE_MILESTONE),
            "the message must name the milestone: {message}"
        );
        assert!(
            message.contains("No recording was written"),
            "the message must say no file was produced: {message}"
        );
        // Not "nothing was written": the run wrote log files, and a claim the
        // program cannot keep is worse than a narrower one it can.
        assert!(
            !message.to_lowercase().contains("nothing was written"),
            "the message overclaims: {message}"
        );
    }
}
