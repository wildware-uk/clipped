//! What can go wrong, and what a caller can say about it.
//!
//! Every variant names the file and the thing that was being attempted, because
//! the person reading it is looking at a support log rather than at this code
//! (AGENTS.md section 15). None of them are fatal to anything: a waveform that
//! could not be produced is a timeline drawn without one
//! ([`WaveformState`](crate::WaveformState)), not a failed recording.

use core::fmt;
use core::time::Duration;
use std::io;

use clipped_logging::RedactedPath;

/// Why a waveform could not be produced or read back.
#[derive(Debug)]
#[non_exhaustive]
pub enum WaveformError {
    /// The recording could not be opened or read.
    Unreadable {
        /// The recording, reduced for logs (docs/logging.md, "Privacy").
        path: RedactedPath,
        /// What the operating system said.
        cause: io::Error,
    },
    /// The container opened but FFmpeg could not make sense of it.
    Undecodable {
        /// The recording, reduced for logs.
        path: RedactedPath,
        /// What was being attempted, and what FFmpeg returned.
        detail: String,
    },
    /// One audio track is in a sample format this build cannot read.
    ///
    /// Named rather than skipped silently: a track that quietly produces no
    /// waveform looks exactly like a track that is silent.
    UnsupportedSampleFormat {
        /// Which stream of the container.
        stream: u32,
        /// What libavcodec called the format.
        format: String,
    },
    /// The audio is longer than the analyser will summarise in one pass.
    TooLong {
        /// The most it will summarise.
        limit: Duration,
    },
    /// The service was shutting down, or was asked to stop, before the analysis
    /// finished.
    Cancelled,
    /// The cache directory could not be read or written.
    ///
    /// Never fatal: peaks are derived data, so a cache that cannot be written
    /// costs the time to compute them again and nothing else.
    Cache {
        /// What was being attempted.
        detail: String,
        /// What the operating system said.
        cause: io::Error,
    },
}

impl fmt::Display for WaveformError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable { path, cause } => write!(
                formatter,
                "could not read {path} to generate its audio waveform: {cause}"
            ),
            Self::Undecodable { path, detail } => write!(
                formatter,
                "could not decode the audio of {path} to generate its waveform: {detail}"
            ),
            Self::UnsupportedSampleFormat { stream, format } => write!(
                formatter,
                "audio stream {stream} decodes to the sample format {format}, which this build \
                 cannot summarise; the track has no waveform"
            ),
            Self::TooLong { limit } => write!(
                formatter,
                "the audio is longer than the {} hours this analyser summarises in one pass",
                limit.as_secs() / 3_600
            ),
            Self::Cancelled => {
                formatter.write_str("waveform generation was stopped before it finished")
            }
            Self::Cache { detail, cause } => {
                write!(formatter, "the waveform cache could not {detail}: {cause}")
            }
        }
    }
}

impl std::error::Error for WaveformError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Unreadable { cause, .. } | Self::Cache { cause, .. } => Some(cause),
            _ => None,
        }
    }
}
