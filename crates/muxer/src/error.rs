//! What the container writer reports when writing goes wrong.
//!
//! A recording is irreplaceable, so the failures here are the ones a caller has
//! to be able to tell apart: refusing to touch a file that already exists,
//! being handed a track that was never declared, and FFmpeg saying no. Every
//! variant names the operation that failed rather than leaving the caller to
//! infer it from a numeric code (AGENTS.md section 15).

use core::fmt;
use std::error::Error;
use std::ffi::{c_char, CStr};
use std::path::PathBuf;

use clipped_logging::RedactedPath;
use rusty_ffmpeg::ffi;

use crate::track::TrackId;

/// Writing a recording into a container failed.
#[derive(Debug)]
#[non_exhaustive]
pub enum MuxError {
    /// The loaded FFmpeg build has no Matroska muxer in it.
    ///
    /// Checked before anything is created, because the alternative is a
    /// half-open file and an error from four calls further on. The pinned
    /// build has one and `crates/muxer/tests/ffmpeg_linkage.rs` asserts it, so
    /// this means the DLLs beside the executable were replaced with a build
    /// that is not the pinned one (`docs/ffmpeg.md`).
    ContainerUnsupported,

    /// Something already exists where the recording was going to be written.
    ///
    /// The writer never opens an existing file, because the mode that would
    /// let it truncates, and truncating is how a recorder destroys footage
    /// somebody cannot get back (AGENTS.md section 56). Choosing another name
    /// belongs to the caller, which is the layer that knows what the recording
    /// is of.
    OutputExists {
        /// The path that was already taken.
        path: PathBuf,
    },

    /// The output path cannot be expressed as UTF-8.
    ///
    /// FFmpeg's file protocol takes a UTF-8 path and converts it to the wide
    /// form Windows wants, so a path that is not valid Unicode — an unpaired
    /// surrogate in a file name, which Windows permits — has no representation
    /// to pass on.
    PathNotRepresentable {
        /// The path that could not be converted.
        path: PathBuf,
    },

    /// A track was described in a way the container cannot carry.
    InvalidTrack {
        /// Which track.
        track: TrackId,
        /// What is wrong with it, phrased for a diagnostics screen.
        reason: &'static str,
    },

    /// A packet was addressed to a track this recording does not have.
    ///
    /// A programming error in the caller: the track layout is fixed when the
    /// file is created and cannot grow afterwards, because Matroska's track
    /// entries are written in the header.
    UnknownTrack {
        /// The track the packet named.
        track: TrackId,
        /// How many audio tracks the recording actually has.
        audio_tracks: usize,
    },

    /// A packet carried no data.
    ///
    /// Passing it on would write a zero-length block, which is legal Matroska
    /// and meaningless media, so it is refused where the caller can still see
    /// which track produced it.
    EmptyPacket {
        /// The track the packet named.
        track: TrackId,
    },

    /// A packet was larger than a container block can be.
    ///
    /// FFmpeg counts a packet's bytes in a signed 32-bit integer, so two
    /// gigabytes is the ceiling. Nothing an encoder produces comes close, and
    /// a value that does is a length that has been computed wrongly somewhere;
    /// truncating it into the field would write a fraction of a frame and
    /// report success.
    PacketTooLarge {
        /// The track the packet named.
        track: TrackId,
        /// How many bytes it carried.
        bytes: usize,
    },

    /// FFmpeg refused, or the operating system did.
    Ffmpeg {
        /// What was being attempted, phrased so the message reads as a
        /// sentence: `"opening the output file"`.
        operation: &'static str,
        /// FFmpeg's own error.
        source: AvError,
    },
}

impl fmt::Display for MuxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ContainerUnsupported => formatter.write_str(
                "the loaded FFmpeg build cannot write Matroska, which is the recording \
                 container",
            ),
            // Redacted rather than printed whole: an error message reaches the
            // log files at least as reliably as a `Debug` string does, and a
            // recording path contains the account name (docs/logging.md). The
            // file name is what a person needs to recognise which recording is
            // meant, and the digest is what correlates two lines about the same
            // one; the directories above it identify the machine's owner and
            // say nothing about the failure. `path` stays on the variant for a
            // caller that has a legitimate need for the whole thing.
            Self::OutputExists { path } => write!(
                formatter,
                "{} already exists, and a recording is never written over one",
                RedactedPath::new(path)
            ),
            Self::PathNotRepresentable { path } => write!(
                formatter,
                "the output path {} is not valid Unicode, so it cannot be passed to FFmpeg",
                RedactedPath::new(path)
            ),
            Self::InvalidTrack { track, reason } => {
                write!(formatter, "the {track} cannot be written: {reason}")
            }
            Self::UnknownTrack {
                track,
                audio_tracks,
            } => write!(
                formatter,
                "a packet was addressed to the {track}, but this recording has one video \
                 track and {audio_tracks} audio tracks"
            ),
            Self::EmptyPacket { track } => {
                write!(formatter, "the {track} produced a packet with no data")
            }
            Self::PacketTooLarge { track, bytes } => write!(
                formatter,
                "the {track} produced a packet of {bytes} bytes, which is more than a \
                 container block can hold"
            ),
            Self::Ffmpeg { operation, source } => {
                write!(formatter, "FFmpeg failed while {operation}: {source}")
            }
        }
    }
}

impl Error for MuxError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Ffmpeg { source, .. } => Some(source),
            Self::ContainerUnsupported
            | Self::OutputExists { .. }
            | Self::PathNotRepresentable { .. }
            | Self::InvalidTrack { .. }
            | Self::UnknownTrack { .. }
            | Self::EmptyPacket { .. }
            | Self::PacketTooLarge { .. } => None,
        }
    }
}

/// An error code from one of the FFmpeg libraries, with the text FFmpeg gives
/// for it.
///
/// The code is kept as well as the message because the message is localised in
/// some builds and the code is not, so a log line carrying both can still be
/// matched against FFmpeg's own headers months later. Common ones are worth
/// recognising on sight: `-2` is `ENOENT`, `-13` is `EACCES`, `-28` is
/// `ENOSPC`, and `-1094995529` is `AVERROR_INVALIDDATA`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvError {
    code: i32,
    message: String,
}

impl AvError {
    /// Reads the message FFmpeg has for `code`.
    ///
    /// Every FFmpeg entry point that can fail returns a negative code, so this
    /// takes the code as returned rather than negated.
    #[must_use]
    pub fn new(code: i32) -> Self {
        Self {
            code,
            message: describe(code),
        }
    }

    /// FFmpeg's error code, as it was returned.
    #[must_use]
    pub const fn code(&self) -> i32 {
        self.code
    }

    /// The text FFmpeg has for the code.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for AvError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({})", self.message, self.code)
    }
}

impl Error for AvError {}

/// Asks FFmpeg what an error code means.
///
/// `av_strerror` fills the buffer with a description, or leaves an
/// implementation-defined string and returns negative when it does not
/// recognise the code; either way the buffer is NUL-terminated, so the result
/// is worth reporting rather than discarding.
fn describe(code: i32) -> String {
    // FFmpeg's own AV_ERROR_MAX_STRING_SIZE, which `av_make_error_string` uses.
    let mut buffer = [0 as c_char; ffi::AV_ERROR_MAX_STRING_SIZE as usize];

    // SAFETY: `av_strerror` writes at most `buffer.len()` bytes into the
    // pointer it is given and always NUL-terminates within them. The buffer is
    // a live, exclusively owned local of exactly that length, and it outlives
    // the call.
    unsafe {
        ffi::av_strerror(code, buffer.as_mut_ptr(), buffer.len());
    }

    // SAFETY: the buffer was zeroed before the call and `av_strerror` never
    // writes past its end, so a NUL is present whether or not FFmpeg wrote one.
    let text = unsafe { CStr::from_ptr(buffer.as_ptr()) };
    text.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ffmpeg_failure_names_what_was_being_done_and_keeps_the_code() {
        // -2 is ENOENT, which FFmpeg describes with the C library's text.
        let error = MuxError::Ffmpeg {
            operation: "opening the output file",
            source: AvError::new(-2),
        };

        let message = error.to_string();
        assert!(
            message.starts_with("FFmpeg failed while opening the output file: "),
            "{message}"
        );
        assert!(
            message.contains("(-2)"),
            "the numeric code has to survive into the message, because it is the part \
             that can be looked up: {message}"
        );
        assert!(error.source().is_some());
    }

    #[test]
    fn error_codes_are_described_by_ffmpeg_rather_than_by_us() {
        // A code FFmpeg does not recognise still produces a message rather than
        // an empty string, which is what makes it safe to report unconditionally.
        let unknown = AvError::new(i32::MIN + 1);
        assert!(!unknown.message().is_empty());

        let no_such_file = AvError::new(-2);
        assert_eq!(no_such_file.code(), -2);
        assert!(
            !no_such_file.message().is_empty(),
            "av_strerror produced nothing for ENOENT"
        );
    }

    #[test]
    fn refusing_to_overwrite_says_which_file_without_saying_whose_it_is() {
        let error = MuxError::OutputExists {
            path: PathBuf::from(r"C:\Users\some-person\Videos\Clipped\match.mkv"),
        };

        let message = error.to_string();
        assert!(
            message.contains("match.mkv"),
            "the file name is what identifies the recording: {message}"
        );
        // An error message reaches the log files, so the directories above the
        // file — which carry the account name — must not (docs/logging.md).
        assert!(
            !message.contains("some-person"),
            "the message carries the account name into the logs: {message}"
        );
        assert!(
            !message.contains("Videos"),
            "the message carries the directories into the logs: {message}"
        );
    }

    #[test]
    fn a_path_that_is_not_unicode_is_reported_without_the_directories_above_it() {
        let error = MuxError::PathNotRepresentable {
            path: PathBuf::from(r"C:\Users\some-person\Videos\Clipped\match.mkv"),
        };

        let message = error.to_string();
        assert!(message.contains("match.mkv"), "{message}");
        assert!(
            !message.contains("some-person"),
            "the message carries the account name into the logs: {message}"
        );
    }
}
