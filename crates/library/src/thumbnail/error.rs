//! What can go wrong, and what a screen can say about it.
//!
//! None of these are fatal to anything. A thumbnail that could not be made is a
//! library tile drawn without a picture ([`ThumbnailState`](super::ThumbnailState)),
//! never a recording that cannot be played and never a screen that cannot be
//! opened (AGENTS.md section 27, issue #57's third acceptance criterion).
//!
//! # Every path here is a [`RedactedPath`]
//!
//! These messages reach the log, and every path a library touches is inside the
//! user's own profile — `C:\Users\<account>\Videos\Clipped\...` — so a raw path
//! in one of them puts the account name and the folders somebody chose into a
//! support file (AGENTS.md section 13, `docs/logging.md`, "Privacy"). No variant
//! holds a [`std::path::Path`], and the one free-text field left,
//! [`ThumbnailError::Cache::detail`], is a `&'static str` so that a path cannot
//! be formatted into it. That is a compile-time constraint rather than a
//! convention, and it is the same one `clipped-waveform` arrived at.

use core::fmt;
use std::io;

use clipped_logging::RedactedPath;

/// Why a recording has no thumbnail.
#[derive(Debug)]
#[non_exhaustive]
pub enum ThumbnailError {
    /// The recording could not be opened or stat-ed.
    Unreadable {
        /// The recording, reduced for logs.
        path: RedactedPath,
        /// What the operating system said.
        cause: io::Error,
    },
    /// The container opened but FFmpeg could not make sense of it, or the
    /// decode, scale or encode failed part way.
    Undecodable {
        /// The recording, reduced for logs.
        path: RedactedPath,
        /// What was being attempted, and what FFmpeg returned.
        detail: String,
    },
    /// The container holds no video stream at all.
    ///
    /// Named rather than reported as a decode failure: an audio-only file is a
    /// perfectly good file, and the honest answer is that there is no picture in
    /// it to show.
    NoVideo {
        /// The recording, reduced for logs.
        path: RedactedPath,
    },
    /// Generation was suspended for a recording and then stopped, or the
    /// service was shutting down.
    Cancelled,
    /// The cache directory could not be read or written.
    ///
    /// Never fatal: a thumbnail is regenerable
    /// ([`StorageCategory::is_regenerable`](crate::accounting::StorageCategory::is_regenerable)),
    /// so a cache that cannot be written costs the time to make it again.
    Cache {
        /// What was being attempted.
        ///
        /// A `&'static str` rather than a `String` so that the file being acted
        /// on cannot be formatted into it; the file goes in `entry`, reduced.
        detail: &'static str,
        /// What was being read or written, reduced for logs.
        entry: RedactedPath,
        /// What the operating system said.
        cause: io::Error,
    },
    /// An earlier attempt failed, and the cache remembers that rather than
    /// decoding the same broken file again on every lookup.
    ///
    /// The memory belongs to one version of the recording: a file repaired,
    /// replaced or re-encoded no longer matches the entry and is attempted
    /// again (`docs/thumbnails.md`, "Invalidation").
    Remembered {
        /// The recording, reduced for logs.
        path: RedactedPath,
        /// What the failed attempt said, as it was written into the entry.
        reason: String,
    },
}

impl fmt::Display for ThumbnailError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable { path, cause } => {
                write!(
                    formatter,
                    "could not read {path} to make a thumbnail: {cause}"
                )
            }
            Self::Undecodable { path, detail } => write!(
                formatter,
                "could not decode a frame of {path} for a thumbnail: {detail}"
            ),
            Self::NoVideo { path } => write!(
                formatter,
                "{path} has no video stream, so there is no frame to show"
            ),
            Self::Cancelled => {
                formatter.write_str("thumbnail generation was stopped before it finished")
            }
            Self::Cache {
                detail,
                entry,
                cause,
            } => write!(
                formatter,
                "the thumbnail cache could not {detail} ({entry}): {cause}"
            ),
            Self::Remembered { path, reason } => write!(
                formatter,
                "{path} produced no thumbnail when it was last attempted: {reason}"
            ),
        }
    }
}

impl std::error::Error for ThumbnailError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Unreadable { cause, .. } | Self::Cache { cause, .. } => Some(cause),
            _ => None,
        }
    }
}
