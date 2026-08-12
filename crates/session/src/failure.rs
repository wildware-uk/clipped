//! What to tell somebody whose recording just failed, and what they can do.
//!
//! [`SessionError`] says what went wrong in the pipeline's own terms, which is
//! the right vocabulary for a log and the wrong one for a person: "FFmpeg
//! failed while writing a packet: No space left on device (-28)" is accurate
//! and useless. AGENTS.md section 45 asks for the other thing — a headline, the
//! technical detail kept but demoted, and an action worth taking — and this
//! module is where a `SessionError` is turned into it.
//!
//! # The three questions
//!
//! Every failure a recording can have is answered in the same shape, because
//! these are the three things somebody actually wants to know:
//!
//! ```text
//! what is kept       FootageKept        the recording is the thing that cannot be made again
//! what happened      headline + detail  one sentence, then the technical words
//! what can I do      actions            at least one, always
//! ```
//!
//! # Where the classification comes from
//!
//! Mostly from the variant, and in two places from a code inside it. A full
//! disk and a disconnected drive both surface as
//! [`SessionError::Mux`] wrapping FFmpeg's own error number, so `ENOSPC` and
//! `ENODEV` are read out of it rather than the user being told "the recording
//! could not be written" for two conditions with completely different answers.
//! A driver reset surfaces as [`clipped_encoder::EncodeErrorKind::DeviceLost`],
//! which is the same idea one layer along.
//!
//! # What this does not do
//!
//! Recover. Reopening the device and continuing a recording through a driver
//! reset is [issue #148](https://github.com/wildware-uk/clipped/issues/148),
//! and it needs a capture backend that can be rebuilt underneath the session.
//! What this build does is finish the file properly and say what happened,
//! which is the half of AGENTS.md section 17 that can be kept today.

use core::fmt;
use std::path::Path;

use clipped_encoder::EncodeErrorKind;
use clipped_muxer::MuxError;

use crate::error::SessionError;

/// FFmpeg reports the C library's own error numbers, negated. `-28` is
/// `ENOSPC`.
const ENOSPC: i32 = -28;

/// `-19` is `ENODEV`: there is no such device. What an unplugged drive gives.
const ENODEV: i32 = -19;

/// `-2` is `ENOENT`: the path stopped resolving, which is the other shape a
/// removed drive takes.
const ENOENT: i32 = -2;

/// `-5` is `EIO`: the device is there and would not answer.
const EIO: i32 = -5;

/// `-13` is `EACCES`: something else took the file, or the account lost the
/// right to write it.
const EACCES: i32 = -13;

/// Whether the recording that was being made survived.
///
/// The distinction the user cares about most, and the one a bare error message
/// never answers. It is stated in three states rather than two because guessing
/// is worse than admitting: a capture failure can happen before the first
/// packet or an hour in, and this crate does not always know which.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FootageKept {
    /// Nothing was written. The failure happened before the file existed.
    Nothing,
    /// Everything up to the failure is in a finalised, playable file.
    UpToTheFailure,
    /// A file may exist. If the recording had started, what it contained was
    /// finalised on the way out.
    Unknown,
}

/// The word a failure is filed under, in a log line and in a session's record.
///
/// Deliberately coarse. It is what a support request is grouped by and what a
/// screen chooses an icon from, not a second copy of [`SessionError`] — the
/// error itself is still there, and is what carries the detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FailureKind {
    /// The drive being recorded to ran out of room.
    DiskFull,
    /// The drive being recorded to stopped answering — unplugged, or offline.
    OutputUnavailable,
    /// Something else holds the file, or the account may not write it.
    OutputRefused,
    /// The graphics device was reset or removed underneath the recording: a
    /// driver reset, a GPU hot-unplug, or the driver's timeout detection.
    GraphicsDeviceLost,
    /// No encoder could be opened, or the one in use became unavailable.
    EncoderUnavailable,
    /// The thing being recorded went away, or never appeared.
    CaptureLost,
    /// The recording was never possible as asked for: a size this build cannot
    /// produce, a frame rate of zero, a platform with no capture.
    NotPossible,
    /// Something else. The error's own words are the whole of what is known.
    Unclassified,
}

impl FailureKind {
    /// The token this kind is written as, in logs and in a session's record.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::DiskFull => "disk-full",
            Self::OutputUnavailable => "output-unavailable",
            Self::OutputRefused => "output-refused",
            Self::GraphicsDeviceLost => "graphics-device-lost",
            Self::EncoderUnavailable => "encoder-unavailable",
            Self::CaptureLost => "capture-lost",
            Self::NotPossible => "not-possible",
            Self::Unclassified => "unclassified",
        }
    }
}

impl fmt::Display for FailureKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.token())
    }
}

/// A failure as it should be put to the person it happened to.
///
/// Built from a [`SessionError`] and the path the recording was going to. The
/// path is needed because half of the useful actions name a drive — "free space
/// on D:" is an action and "free some space" is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingFailure {
    kind: FailureKind,
    headline: String,
    detail: String,
    actions: Vec<String>,
    footage: FootageKept,
}

impl RecordingFailure {
    /// How `error` should be described to somebody who was recording to
    /// `output`.
    #[must_use]
    pub fn of(error: &SessionError, output: &Path) -> Self {
        let drive = drive_of(output);

        match error {
            SessionError::NotEnoughDiskSpace { free, minimum } => Self {
                kind: FailureKind::DiskFull,
                headline: format!("There is not enough space on {drive} to record"),
                detail: format!(
                    "{} is free and a recording keeps {} in reserve so that it can always be \
                     finished properly.",
                    crate::disk::describe_bytes(*free),
                    crate::disk::describe_bytes(*minimum)
                ),
                actions: vec![
                    format!("Free up space on {drive}"),
                    "Choose another drive to record to".to_owned(),
                ],
                footage: FootageKept::Nothing,
            },

            SessionError::Mux(MuxError::Ffmpeg { source, .. }) => {
                Self::of_write_failure(source.code(), &source.to_string(), &drive)
            }

            SessionError::Mux(MuxError::OutputExists { .. }) => Self {
                kind: FailureKind::OutputRefused,
                headline: "Something is already at that path".to_owned(),
                detail: error.to_string(),
                actions: vec![
                    "Record to another file".to_owned(),
                    "Move the existing recording out of the way".to_owned(),
                ],
                footage: FootageKept::Nothing,
            },

            SessionError::Mux(_) => Self {
                kind: FailureKind::Unclassified,
                headline: "The recording could not be written".to_owned(),
                detail: error.to_string(),
                actions: vec![report_it()],
                footage: FootageKept::Unknown,
            },

            SessionError::OutputDirectory { source } => Self {
                kind: FailureKind::OutputRefused,
                headline: format!("The recording could not be created on {drive}"),
                detail: source.to_string(),
                actions: vec![
                    format!("Check that {drive} is connected and writable"),
                    "Choose another drive to record to".to_owned(),
                ],
                footage: FootageKept::Nothing,
            },

            SessionError::Encode(encode) => Self::of_encode_failure(encode, error),

            SessionError::NoEncoder { .. } => Self {
                kind: FailureKind::EncoderUnavailable,
                headline: "No encoder could be opened for this recording".to_owned(),
                detail: error.to_string(),
                actions: vec![
                    "Update your graphics driver".to_owned(),
                    "Close other applications that record or stream".to_owned(),
                    "Run `clipped-recorder capabilities` to see what this machine offers"
                        .to_owned(),
                ],
                footage: FootageKept::Nothing,
            },

            SessionError::Capture(_) | SessionError::NoCaptureBackend(_) => Self {
                kind: FailureKind::CaptureLost,
                headline: "Capture stopped".to_owned(),
                detail: error.to_string(),
                actions: vec![
                    "Check the window is still open and on a connected display".to_owned(),
                    "Start recording again".to_owned(),
                ],
                footage: FootageKept::Unknown,
            },

            SessionError::NoFrames => Self {
                kind: FailureKind::CaptureLost,
                headline: "Nothing was recorded".to_owned(),
                detail: error.to_string(),
                actions: vec![
                    "Restore the window if it is minimised".to_owned(),
                    "Make sure it is on a desktop that is showing".to_owned(),
                ],
                footage: FootageKept::Nothing,
            },

            SessionError::TargetHasNoPixels => Self {
                kind: FailureKind::CaptureLost,
                headline: "That window has nothing to capture".to_owned(),
                detail: error.to_string(),
                actions: vec!["Restore the window and record again".to_owned()],
                footage: FootageKept::Nothing,
            },

            SessionError::NoGraphicsDevice => Self {
                kind: FailureKind::GraphicsDeviceLost,
                headline: "The graphics device behind the capture could not be identified"
                    .to_owned(),
                detail: error.to_string(),
                actions: vec!["Start recording again".to_owned(), report_it()],
                footage: FootageKept::Nothing,
            },

            SessionError::UnsupportedPlatform
            | SessionError::ZeroFramerate
            | SessionError::ScalingNotSupported { .. }
            | SessionError::UnsupportedPixelFormat { .. } => Self {
                kind: FailureKind::NotPossible,
                headline: "This recording cannot be made as asked for".to_owned(),
                detail: error.to_string(),
                actions: vec!["Change the settings the message names".to_owned()],
                footage: FootageKept::Nothing,
            },

            SessionError::Detection(_) => Self {
                kind: FailureKind::EncoderUnavailable,
                headline: "This machine could not be asked what it can encode with".to_owned(),
                detail: error.to_string(),
                actions: vec!["Update your graphics driver".to_owned(), report_it()],
                footage: FootageKept::Nothing,
            },

            // Deliberately no catch-all arm. `SessionError` is
            // `#[non_exhaustive]` to the rest of the workspace but not to the
            // crate that declares it, so a failure added later stops this
            // compiling — which is the point. A wildcard here would let a new
            // way for a recording to fail ship with no advice attached, and
            // nothing would notice (AGENTS.md section 45).
            SessionError::BackendNotRegistered { .. } | SessionError::WriterLost => Self {
                kind: FailureKind::Unclassified,
                headline: "The recording stopped for a reason that should not happen".to_owned(),
                detail: error.to_string(),
                actions: vec![report_it()],
                footage: FootageKept::Unknown,
            },
        }
    }

    /// A write that FFmpeg refused, read by the error number underneath it.
    ///
    /// This is the one place a full disk and an unplugged drive are told apart.
    /// They arrive as the same `SessionError` variant and want opposite advice:
    /// one is "make room", the other is "plug it back in", and offering both
    /// for either is how a recovery message becomes noise.
    fn of_write_failure(code: i32, detail: &str, drive: &str) -> Self {
        match code {
            ENOSPC => Self {
                kind: FailureKind::DiskFull,
                headline: format!("{drive} filled up while recording"),
                detail: detail.to_owned(),
                actions: vec![
                    format!("Free up space on {drive}"),
                    "Record to a drive with more room".to_owned(),
                ],
                footage: FootageKept::UpToTheFailure,
            },
            ENODEV | ENOENT | EIO => Self {
                kind: FailureKind::OutputUnavailable,
                headline: format!("{drive} stopped answering while recording"),
                detail: detail.to_owned(),
                actions: vec![
                    format!("Reconnect {drive}"),
                    "Record to an internal drive if this keeps happening".to_owned(),
                ],
                footage: FootageKept::UpToTheFailure,
            },
            EACCES => Self {
                kind: FailureKind::OutputRefused,
                headline: "The recording could no longer be written".to_owned(),
                detail: detail.to_owned(),
                actions: vec![
                    "Close anything that has the file open".to_owned(),
                    "Check you can write to that folder".to_owned(),
                ],
                footage: FootageKept::UpToTheFailure,
            },
            _ => Self {
                kind: FailureKind::Unclassified,
                headline: "The recording could not be written".to_owned(),
                detail: detail.to_owned(),
                actions: vec![report_it()],
                footage: FootageKept::UpToTheFailure,
            },
        }
    }

    /// An encoder that stopped part way through.
    fn of_encode_failure(encode: &clipped_encoder::EncodeError, error: &SessionError) -> Self {
        let (kind, headline, actions) = match encode.kind() {
            EncodeErrorKind::DeviceLost => (
                FailureKind::GraphicsDeviceLost,
                "Your graphics driver reset while recording".to_owned(),
                vec![
                    "Start recording again — a new encoder opens on the recovered device"
                        .to_owned(),
                    "Update your graphics driver if this keeps happening".to_owned(),
                ],
            ),
            EncodeErrorKind::SessionLimitReached => (
                FailureKind::EncoderUnavailable,
                format!(
                    "{} has no encoding session left",
                    encode.context().encoder()
                ),
                vec![
                    "Close other applications that are recording or streaming".to_owned(),
                    "Record with a different encoder".to_owned(),
                ],
            ),
            EncodeErrorKind::OutOfMemory => (
                FailureKind::EncoderUnavailable,
                "The encoder ran out of memory".to_owned(),
                vec![
                    "Close other applications using the GPU".to_owned(),
                    "Record at a smaller size or frame rate".to_owned(),
                ],
            ),
            _ => (
                FailureKind::EncoderUnavailable,
                "Encoding stopped part way through the recording".to_owned(),
                vec!["Start recording again".to_owned(), report_it()],
            ),
        };

        Self {
            kind,
            headline,
            detail: error.to_string(),
            actions,
            footage: FootageKept::UpToTheFailure,
        }
    }

    /// Which class of failure this is.
    #[must_use]
    pub const fn kind(&self) -> FailureKind {
        self.kind
    }

    /// One sentence, in the user's terms, with no error codes in it.
    #[must_use]
    pub fn headline(&self) -> &str {
        &self.headline
    }

    /// The technical words, kept rather than thrown away.
    ///
    /// Belongs beside the headline in a console and behind "details" on a
    /// screen; it is what a support request needs and what nobody reads
    /// otherwise (AGENTS.md section 45).
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }

    /// What the user can do about it. Never empty.
    #[must_use]
    pub fn actions(&self) -> &[String] {
        &self.actions
    }

    /// Whether the recording that was being made survived.
    #[must_use]
    pub const fn footage(&self) -> FootageKept {
        self.footage
    }

    /// What to say about the file, given where it was going.
    ///
    /// Separate from [`headline`](Self::headline) because it is the sentence
    /// that must be said even when the news is good: somebody whose driver
    /// reset assumes the last hour is gone unless they are told otherwise.
    #[must_use]
    pub fn footage_sentence(&self, output: &Path) -> String {
        match self.footage {
            FootageKept::Nothing => "Nothing had been recorded, so no file was left.".to_owned(),
            FootageKept::UpToTheFailure => format!(
                "Everything recorded before this was finished and plays: {}",
                output.display()
            ),
            FootageKept::Unknown => format!(
                "If the recording had started, what it contained was finished and plays: {}",
                output.display()
            ),
        }
    }
}

impl fmt::Display for RecordingFailure {
    /// The block the recorder prints, and the shape a screen should follow.
    ///
    /// Headline, what happened to the footage, then the actions — and the
    /// technical detail last, because it is the part that is there to be
    /// pasted into a bug report rather than read.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.headline)?;
        for action in &self.actions {
            write!(formatter, "\n  - {action}")?;
        }
        write!(formatter, "\n  {}", self.detail)
    }
}

/// The action of last resort, worded once so it reads the same everywhere.
fn report_it() -> String {
    "Report this with the diagnostics from %LOCALAPPDATA%\\Clipped\\logs".to_owned()
}

/// The drive a path is on, as a person names it.
///
/// `D:` for `D:\clips\session.mkv`, and the folder itself when there is no
/// drive letter — a UNC path, or a path relative to the working directory. The
/// point is that the action names something the user can find: "free up space
/// on D:" is an instruction and "free up space" is a shrug.
fn drive_of(output: &Path) -> String {
    let mut components = output.components();
    if let Some(std::path::Component::Prefix(prefix)) = components.next() {
        return prefix.as_os_str().to_string_lossy().into_owned();
    }
    output.parent().map_or_else(
        || output.display().to_string(),
        |parent| parent.display().to_string(),
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use clipped_encoder::{Codec, EncodeContext, EncodeError, EncoderKind, Resolution};
    use clipped_muxer::AvError;

    use super::*;

    fn output() -> PathBuf {
        PathBuf::from(r"D:\clips\session.mkv")
    }

    fn write_failure(code: i32) -> SessionError {
        SessionError::Mux(MuxError::Ffmpeg {
            operation: "writing a packet",
            source: AvError::new(code),
        })
    }

    fn encode_failure(kind: EncodeErrorKind) -> SessionError {
        SessionError::Encode(EncodeError::new(
            EncodeContext::new(EncoderKind::Nvenc, Codec::H264, Resolution::HD_1080P),
            kind,
        ))
    }

    #[test]
    fn a_full_disk_and_an_unplugged_drive_are_told_apart_and_given_opposite_advice() {
        // They arrive as the same `SessionError` variant. Reading only the
        // variant would offer "free up space" to somebody who unplugged a drive
        // and "reconnect it" to somebody whose drive is full, which is how
        // recovery advice becomes noise nobody reads.
        let full = RecordingFailure::of(&write_failure(ENOSPC), &output());
        assert_eq!(full.kind(), FailureKind::DiskFull);
        assert!(
            full.actions()
                .iter()
                .any(|action| action.contains("Free up space on D:")),
            "{full:?}"
        );

        let gone = RecordingFailure::of(&write_failure(ENODEV), &output());
        assert_eq!(gone.kind(), FailureKind::OutputUnavailable);
        assert!(
            gone.actions()
                .iter()
                .any(|action| action.contains("Reconnect D:")),
            "{gone:?}"
        );
        assert!(
            !gone
                .actions()
                .iter()
                .any(|action| action.contains("Free up space")),
            "an unplugged drive must not be answered with 'free up space': {gone:?}"
        );
    }

    #[test]
    fn a_write_that_failed_says_what_was_kept_rather_than_leaving_it_to_be_guessed() {
        // The sentence that matters most and that a bare error never says. A
        // user who is not told the file survived will assume it did not.
        let failure = RecordingFailure::of(&write_failure(ENOSPC), &output());
        assert_eq!(failure.footage(), FootageKept::UpToTheFailure);
        let sentence = failure.footage_sentence(&output());
        assert!(sentence.contains("plays"), "{sentence}");
        assert!(sentence.contains(r"D:\clips\session.mkv"), "{sentence}");
    }

    #[test]
    fn a_driver_reset_is_named_as_one_and_says_the_recording_survived() {
        // `DeviceLost` is a driver reset, a GPU hot-unplug or the driver's
        // timeout detection firing. Telling somebody "encoding stopped part way
        // through" gives them nothing to do; telling them their driver reset
        // and that recording again opens a new encoder does.
        let failure = RecordingFailure::of(&encode_failure(EncodeErrorKind::DeviceLost), &output());

        assert_eq!(failure.kind(), FailureKind::GraphicsDeviceLost);
        assert!(
            failure.headline().contains("graphics driver reset"),
            "{failure:?}"
        );
        assert_eq!(failure.footage(), FootageKept::UpToTheFailure);
        assert!(
            failure
                .actions()
                .iter()
                .any(|action| action.contains("again")),
            "{failure:?}"
        );
    }

    #[test]
    fn a_full_encoder_session_table_points_at_the_other_application_holding_it() {
        let failure = RecordingFailure::of(
            &encode_failure(EncodeErrorKind::SessionLimitReached),
            &output(),
        );

        assert_eq!(failure.kind(), FailureKind::EncoderUnavailable);
        assert!(
            failure
                .actions()
                .iter()
                .any(|action| action.contains("recording or streaming")),
            "{failure:?}"
        );
    }

    #[test]
    fn a_pre_flight_refusal_names_both_figures_and_offers_another_drive() {
        let failure = RecordingFailure::of(
            &SessionError::NotEnoughDiskSpace {
                free: 400 * 1024 * 1024,
                minimum: 1 << 30,
            },
            &output(),
        );

        assert_eq!(failure.kind(), FailureKind::DiskFull);
        assert_eq!(failure.footage(), FootageKept::Nothing);
        assert!(failure.detail().contains("400.0 MiB"), "{failure:?}");
        assert!(failure.detail().contains("1.0 GiB"), "{failure:?}");
        assert!(
            failure.footage_sentence(&output()).contains("no file"),
            "nothing was recorded, and saying a file plays would be a lie"
        );
    }

    #[test]
    fn every_failure_offers_at_least_one_action_and_keeps_the_technical_words() {
        // AGENTS.md section 45: an error with nothing to do about it is an
        // error code with a sentence around it. The detail is kept as well,
        // because the log is not always available to whoever is looking.
        let errors = [
            write_failure(ENOSPC),
            write_failure(ENODEV),
            write_failure(EACCES),
            write_failure(-1_094_995_529),
            encode_failure(EncodeErrorKind::DeviceLost),
            encode_failure(EncodeErrorKind::SessionLimitReached),
            encode_failure(EncodeErrorKind::OutOfMemory),
            encode_failure(EncodeErrorKind::NotRunning),
            SessionError::NotEnoughDiskSpace {
                free: 0,
                minimum: 1,
            },
            SessionError::NoFrames,
            SessionError::TargetHasNoPixels,
            SessionError::NoGraphicsDevice,
            SessionError::ZeroFramerate,
            SessionError::WriterLost,
            SessionError::NoEncoder { attempts: vec![] },
            SessionError::OutputDirectory {
                source: std::io::Error::other("the drive is not there"),
            },
            SessionError::Mux(MuxError::OutputExists { path: output() }),
            SessionError::Mux(MuxError::ContainerUnsupported),
            SessionError::UnsupportedPlatform,
        ];

        for error in &errors {
            let failure = RecordingFailure::of(error, &output());
            assert!(
                !failure.actions().is_empty(),
                "`{error}` was given nothing to do about it"
            );
            assert!(
                !failure.headline().is_empty() && !failure.detail().is_empty(),
                "`{error}` lost either its headline or its detail"
            );
            assert!(
                !failure.headline().contains("-28") && !failure.headline().contains("0x"),
                "the headline should be free of error codes: {}",
                failure.headline()
            );
        }
    }

    #[test]
    fn the_drive_named_in_an_action_is_the_one_the_recording_was_going_to() {
        assert_eq!(drive_of(Path::new(r"D:\clips\session.mkv")), "D:");
        assert_eq!(drive_of(Path::new(r"C:\Users\a\Videos\x.mkv")), "C:");
        // A UNC share has no drive letter, so the share itself is what a person
        // can act on.
        assert_eq!(
            drive_of(Path::new(r"\\nas\clips\session.mkv")),
            r"\\nas\clips"
        );
        // Relative paths happen in tests and in a shell. Naming the folder is
        // still better than naming nothing.
        assert_eq!(drive_of(Path::new("out/session.mkv")), "out");
    }

    #[test]
    fn the_printed_block_leads_with_the_headline_and_ends_with_the_technical_detail() {
        let printed = RecordingFailure::of(&write_failure(ENOSPC), &output()).to_string();
        let mut lines = printed.lines();

        assert_eq!(lines.next(), Some("D: filled up while recording"));
        assert!(
            lines.next().is_some_and(|line| line.starts_with("  - ")),
            "the actions should come next: {printed}"
        );
        assert!(
            printed
                .lines()
                .last()
                .is_some_and(|line| line.contains("-28")),
            "the error code belongs last, not in the headline: {printed}"
        );
    }
}
