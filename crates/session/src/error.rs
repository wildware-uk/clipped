//! Why a recording could not be made, or could not be finished.
//!
//! Every variant says which stage refused and what it was asked for, because
//! "recording failed" is not something a user can act on (AGENTS.md section
//! 15). Where a failure happened *after* the file existed, the message says the
//! file was kept: the recording up to that point is the thing that cannot be
//! made again.

use std::error::Error;
use std::fmt;

use clipped_capture::{CaptureError, SelectionError};
use clipped_encoder::{EncodeError, EncoderKind, ProbeError};
use clipped_muxer::MuxError;

/// Why a recording did not happen, or did not finish.
#[derive(Debug)]
#[non_exhaustive]
pub enum SessionError {
    /// This build has no capture backend, because it is not for Windows.
    UnsupportedPlatform,
    /// The target has no pixels to capture — a minimised window, most often.
    TargetHasNoPixels,
    /// A frame rate of zero was asked for.
    ZeroFramerate,
    /// A size was asked for that is not the size capture is producing, and
    /// nothing in this build can scale a frame.
    ScalingNotSupported {
        /// The size that was asked for.
        requested: (u32, u32),
        /// The size the capture is actually producing.
        captured: (u32, u32),
    },
    /// No capture backend was willing to capture this target.
    NoCaptureBackend(SelectionError),
    /// Capture failed.
    Capture(CaptureError),
    /// Selection chose a method this build registered no backend for.
    ///
    /// Unreachable by construction — `select` only ever chooses from the
    /// candidates it was handed — and a variant rather than a panic because a
    /// recorder must not end somebody's session by unwinding (AGENTS.md
    /// section 15).
    BackendNotRegistered {
        /// The method that was chosen.
        method: String,
    },
    /// The graphics device behind the capture could not be identified, so there
    /// was nothing to open an encoder against.
    NoGraphicsDevice,
    /// The recording's directory could not be created, or an existing
    /// recording could not be replaced.
    OutputDirectory {
        /// What the filesystem said.
        source: std::io::Error,
    },
    /// The drive the recording would go to is too nearly full to start one.
    ///
    /// Refused before anything is created rather than discovered four minutes
    /// in. A recording is stopped, deliberately, when the volume reaches the
    /// same floor (`crate::disk`), so starting one below it would produce a
    /// file that ends almost immediately.
    NotEnoughDiskSpace {
        /// How many bytes are free on the volume.
        free: u64,
        /// The floor the recording holds itself to.
        minimum: u64,
    },
    /// The machine could not be asked what it can encode with.
    Detection(ProbeError),
    /// Capture is producing frames in a layout no encoder here accepts.
    UnsupportedPixelFormat {
        /// The layout capture produced, as it prints itself.
        format: String,
    },
    /// No encoder could be opened, with what each candidate said.
    NoEncoder {
        /// Each candidate that was tried and why it refused, most preferred
        /// first.
        attempts: Vec<(EncoderKind, String)>,
    },
    /// Encoding failed part way through the recording.
    Encode(EncodeError),
    /// An audio source the caller asked for could not be opened.
    ///
    /// Refused before the recording starts rather than skipped, because a
    /// recording made without the microphone somebody asked for is one they
    /// find out about afterwards, when it cannot be made again (AGENTS.md
    /// section 54). Once a capture is open the same situations are survivable
    /// and are not errors: the track becomes silence and the capture waits for
    /// the device (`docs/audio-routing.md`).
    Audio {
        /// The track the source would have fed, as the container names it.
        track: &'static str,
        /// What the audio stack said.
        source: clipped_audio::AudioError,
    },
    /// A particular output device was asked for and this build can only record
    /// the one Windows is playing through.
    AudioDeviceNotSelectable,
    /// The microphone that was asked for named no device, or several.
    MicrophoneNotFound {
        /// How many of the machine's microphones matched the name.
        matched: usize,
        /// How many microphones the machine has.
        available: usize,
    },
    /// The container could not be written.
    Mux(MuxError),
    /// Capture produced no frame at all before the recording was stopped.
    NoFrames,
    /// The thread writing the file stopped without saying why.
    ///
    /// Only reachable if that thread panicked, which is a bug rather than an
    /// operating condition; it is a variant rather than a panic here so that a
    /// recorder never ends by unwinding through a user's session.
    WriterLost,
}

impl fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedPlatform => formatter.write_str(
                "this build cannot record: Clipped captures through Windows APIs and this is \
                 not a Windows build",
            ),
            Self::TargetHasNoPixels => formatter.write_str(
                "the capture target has no pixels, which is what Windows reports for a \
                 minimised window; restore it and record again",
            ),
            Self::ZeroFramerate => {
                formatter.write_str("a frame rate of zero frames per second is not a recording")
            }
            Self::ScalingNotSupported {
                requested: (width, height),
                captured: (source_width, source_height),
            } => write!(
                formatter,
                "recording at {width}x{height} would mean scaling a {source_width}x\
                 {source_height} capture, and this build has no scaler: frames go from the \
                 capture to the encoder without being copied, which is what keeps the cost \
                 off the game. Record at `source` and scale afterwards"
            ),
            Self::NoCaptureBackend(error) => write!(formatter, "{error}"),
            Self::Capture(error) => write!(formatter, "capture failed: {error}"),
            Self::BackendNotRegistered { method } => write!(
                formatter,
                "{method} was chosen to capture this target and this build has no backend for \
                 it, which should not be possible; please report it"
            ),
            Self::NoGraphicsDevice => formatter.write_str(
                "the graphics device behind the capture could not be identified, so no encoder \
                 could be opened against it",
            ),
            Self::OutputDirectory { source } => write!(
                formatter,
                "the recording could not be created where it was asked for: {source}"
            ),
            Self::NotEnoughDiskSpace { free, minimum } => write!(
                formatter,
                "there is not enough room where the recording would go: {} is free, and a \
                 recording keeps {} in reserve so that it can always be finished properly. \
                 Free up space or record to another drive",
                crate::disk::describe_bytes(*free),
                crate::disk::describe_bytes(*minimum)
            ),
            Self::Detection(error) => write!(
                formatter,
                "the graphics adapters could not be enumerated, so no encoder could be \
                 chosen: {error}"
            ),
            Self::UnsupportedPixelFormat { format } => write!(
                formatter,
                "the capture is producing {format} frames, and no encoder in this build can \
                 take them. That is high dynamic range content; recording it is issue #99"
            ),
            Self::NoEncoder { attempts } if attempts.is_empty() => formatter.write_str(
                "no encoder is available on this machine, not even the software fallback",
            ),
            Self::NoEncoder { attempts } => {
                formatter.write_str("no encoder could be opened for this recording:")?;
                for (encoder, reason) in attempts {
                    write!(formatter, "\n  {encoder}: {reason}")?;
                }
                Ok(())
            }
            Self::Encode(error) => write!(
                formatter,
                "encoding stopped part way through the recording: {error}. \
                 The file was closed, so everything encoded before this point still plays"
            ),
            Self::Audio { track, source } => write!(
                formatter,
                "the {track} track could not be recorded: {source}. Turn that source off to \
                 record without it"
            ),
            Self::AudioDeviceNotSelectable => formatter.write_str(
                "system audio is recorded from the output device Windows is playing through, \
                 and this build cannot record a different one. Use `default` to record what \
                 you hear, or `none` to record no system audio",
            ),
            Self::MicrophoneNotFound {
                matched,
                available: 0,
            } => {
                let _ = matched;
                formatter.write_str(
                    "no microphone matches that name, because this machine has none connected. \
                     Connect one, or turn the microphone track off",
                )
            }
            Self::MicrophoneNotFound {
                matched: 0,
                available,
            } => write!(
                formatter,
                "no microphone matches that name; this machine has {available} connected. Run \
                 `clipped-recorder capabilities` to see them"
            ),
            Self::MicrophoneNotFound { matched, .. } => write!(
                formatter,
                "{matched} microphones match that name, and recording one of them at random \
                 would be recording the wrong one. Use more of the device's name"
            ),
            Self::Mux(error) => write!(formatter, "the recording could not be written: {error}"),
            Self::NoFrames => formatter.write_str(
                "capture never produced a frame, so there was nothing to record. A window \
                 that is not drawing — minimised, or on a virtual desktop that is not \
                 showing — produces none",
            ),
            Self::WriterLost => formatter.write_str(
                "the thread writing the recording stopped unexpectedly; what reached the \
                 file before that remains",
            ),
        }
    }
}

impl Error for SessionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::NoCaptureBackend(error) => Some(error),
            Self::Capture(error) => Some(error),
            Self::Detection(error) => Some(error),
            Self::Encode(error) => Some(error),
            Self::Mux(error) => Some(error),
            Self::Audio { source, .. } => Some(source),
            Self::OutputDirectory { source } => Some(source),
            Self::AudioDeviceNotSelectable
            | Self::MicrophoneNotFound { .. }
            | Self::UnsupportedPlatform
            | Self::TargetHasNoPixels
            | Self::ZeroFramerate
            | Self::ScalingNotSupported { .. }
            | Self::UnsupportedPixelFormat { .. }
            | Self::BackendNotRegistered { .. }
            | Self::NoGraphicsDevice
            | Self::NoEncoder { .. }
            | Self::NoFrames
            | Self::NotEnoughDiskSpace { .. }
            | Self::WriterLost => None,
        }
    }
}

impl From<SelectionError> for SessionError {
    fn from(error: SelectionError) -> Self {
        Self::NoCaptureBackend(error)
    }
}

impl From<CaptureError> for SessionError {
    fn from(error: CaptureError) -> Self {
        Self::Capture(error)
    }
}

impl From<EncodeError> for SessionError {
    fn from(error: EncodeError) -> Self {
        Self::Encode(error)
    }
}

impl From<MuxError> for SessionError {
    fn from(error: MuxError) -> Self {
        Self::Mux(error)
    }
}

impl From<ProbeError> for SessionError {
    fn from(error: ProbeError) -> Self {
        Self::Detection(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scaling_request_names_both_sizes_and_says_what_to_do_instead() {
        let error = SessionError::ScalingNotSupported {
            requested: (1920, 1080),
            captured: (2560, 1440),
        };
        let message = error.to_string();
        assert!(message.contains("1920x1080"), "{message}");
        assert!(message.contains("2560x1440"), "{message}");
        assert!(message.contains("Record at `source`"), "{message}");
    }

    #[test]
    fn every_encoder_that_refused_is_named_with_its_reason() {
        // A machine where nothing opened has to say what each one said, or the
        // only way to find out is a debugger.
        let error = SessionError::NoEncoder {
            attempts: vec![
                (
                    EncoderKind::Nvenc,
                    "no NVIDIA adapter is present".to_owned(),
                ),
                (EncoderKind::Software, "libopenh264 is missing".to_owned()),
            ],
        };
        let message = error.to_string();
        assert!(
            message.contains("no NVIDIA adapter is present"),
            "{message}"
        );
        assert!(message.contains("libopenh264 is missing"), "{message}");
    }

    #[test]
    fn an_encoder_failure_says_the_recording_was_kept() {
        // AGENTS.md section 17: the recording up to the failure is the thing
        // that cannot be made again, and a user who is not told it survived
        // will assume it did not.
        let error = SessionError::Encode(EncodeError::new(
            clipped_encoder::EncodeContext::new(
                EncoderKind::Nvenc,
                clipped_encoder::Codec::H264,
                clipped_encoder::Resolution::HD_1080P,
            ),
            clipped_encoder::EncodeErrorKind::Configuration {
                detail: "the device was removed".to_owned(),
            },
        ));
        assert!(error.to_string().contains("still plays"), "{error}");
    }
}
