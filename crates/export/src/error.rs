//! Why an export did not produce a file.
//!
//! The variants are the ones a caller has to be able to tell apart, because
//! they need different things said to the person waiting and they leave the
//! disk in different states (AGENTS.md section 45):
//!
//! | Failure | The recording | The destination |
//! | --- | --- | --- |
//! | [`Plan`](ExportError::Plan) | untouched | never created |
//! | [`ReencodeRequired`](ExportError::ReencodeRequired) | untouched | never created |
//! | [`SourceUnreadable`](ExportError::SourceUnreadable) | untouched | never created |
//! | [`SourceRead`](ExportError::SourceRead) | untouched | created, then removed |
//! | [`Output`](ExportError::Output) | untouched | created, then removed |
//! | [`Cancelled`](ExportError::Cancelled) | untouched | created, then removed |
//!
//! The recording is untouched in every row, and that is not a claim about
//! intent: the source is opened with `avformat_open_input`, which opens for
//! reading, and nothing here opens it any other way (AGENTS.md sections 56 and
//! 57). `tests/a_source_recording_is_never_touched.rs` hashes it before and
//! after each of these paths rather than trusting this table.

use core::fmt;
use std::error::Error;
use std::path::PathBuf;

use clipped_logging::RedactedPath;
use clipped_muxer::{AvError, MuxError};

use crate::plan::{CopyBlocker, PlanError};

/// Exporting a clip failed.
#[derive(Debug)]
#[non_exhaustive]
pub enum ExportError {
    /// The clip could not be planned at all.
    Plan(PlanError),

    /// The clip cannot be exported by copying, and this build cannot re-encode.
    ///
    /// Every reason is named. This is the honest form of a gap rather than an
    /// export that quietly produced something else (AGENTS.md section 54):
    /// re-encoding is not implemented, so an edit that needs it is refused and
    /// told why.
    ReencodeRequired {
        /// Everything that stopped the copy.
        blockers: Vec<CopyBlocker>,
    },

    /// A recording could not be opened or described.
    SourceUnreadable {
        /// The recording that was being read.
        source: PathBuf,
        /// What FFmpeg said.
        error: AvError,
    },

    /// A recording's path cannot be expressed as UTF-8.
    ///
    /// FFmpeg's file protocol takes a UTF-8 path, so a path that is not valid
    /// Unicode — an unpaired surrogate in a file name, which Windows permits —
    /// has no representation to pass on.
    SourceNotRepresentable {
        /// The path that could not be converted.
        source: PathBuf,
    },

    /// A recording could not be read to the end of what the clip needs.
    SourceRead {
        /// The recording that was being read.
        source: PathBuf,
        /// What FFmpeg said.
        error: AvError,
    },

    /// The clip could not be written.
    Output {
        /// Where the clip was going.
        destination: PathBuf,
        /// What the container writer said.
        source: MuxError,
    },

    /// The export was cancelled, and the part-written file has been removed.
    Cancelled {
        /// Where the clip was going, and what is no longer there.
        destination: PathBuf,
    },
}

impl From<PlanError> for ExportError {
    fn from(error: PlanError) -> Self {
        Self::Plan(error)
    }
}

impl fmt::Display for ExportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Plan(error) => write!(formatter, "the clip could not be exported: {error}"),
            Self::ReencodeRequired { blockers } => {
                formatter.write_str(
                    "this clip has to be re-encoded to be exported, which this build cannot do \
                     yet",
                )?;
                for blocker in blockers {
                    write!(formatter, "; {blocker}")?;
                }
                Ok(())
            }
            // Redacted rather than printed whole, for the reason `MuxError`
            // gives: an error message reaches the log files at least as
            // reliably as a `Debug` string does, and a recording's path
            // contains the account name (docs/logging.md).
            Self::SourceUnreadable { source, error } => write!(
                formatter,
                "the recording {} could not be read: {error}",
                RedactedPath::new(source)
            ),
            Self::SourceNotRepresentable { source } => write!(
                formatter,
                "the recording's path {} is not valid Unicode, so it cannot be passed to FFmpeg",
                RedactedPath::new(source)
            ),
            Self::SourceRead { source, error } => write!(
                formatter,
                "the recording {} stopped being readable part-way through the export: {error}. \
                 The recording is unchanged and nothing was left behind",
                RedactedPath::new(source)
            ),
            Self::Output {
                destination,
                source,
            } => write!(
                formatter,
                "the clip {} could not be written: {source}",
                RedactedPath::new(destination)
            ),
            Self::Cancelled { destination } => write!(
                formatter,
                "the export of {} was cancelled and the part-written file was removed",
                RedactedPath::new(destination)
            ),
        }
    }
}

impl Error for ExportError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Plan(error) => Some(error),
            Self::SourceUnreadable { error, .. } | Self::SourceRead { error, .. } => Some(error),
            Self::Output { source, .. } => Some(source),
            Self::ReencodeRequired { .. }
            | Self::SourceNotRepresentable { .. }
            | Self::Cancelled { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_says_why_and_does_not_pretend_the_export_happened() {
        let error = ExportError::ReencodeRequired {
            blockers: vec![
                CopyBlocker::Overlays { overlays: 2 },
                CopyBlocker::SegmentTransformed { segment: 1 },
            ],
        };

        let message = error.to_string();
        assert!(message.contains("re-encoded"), "{message}");
        assert!(message.contains("2 pieces of text"), "{message}");
        assert!(message.contains("segment 1"), "{message}");
    }

    #[test]
    fn a_failure_names_the_file_without_naming_whose_recording_it_is() {
        // An error message reaches the log files, so the directories above the
        // file — which carry the account name — must not (docs/logging.md).
        let error = ExportError::Cancelled {
            destination: PathBuf::from(r"C:\Users\some-person\Videos\Clipped\ace.mkv"),
        };

        let message = error.to_string();
        assert!(message.contains("ace.mkv"), "{message}");
        assert!(!message.contains("some-person"), "{message}");
        assert!(
            message.contains("removed"),
            "the next question after a cancel is whether there is half a file there: {message}"
        );
    }
}
