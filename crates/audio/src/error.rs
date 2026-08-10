//! What audio capture reports when it cannot carry on.
//!
//! The list is short on purpose. Almost everything that goes wrong with an
//! audio endpoint during a recording is something this crate is expected to
//! survive rather than report: the default endpoint changing, the endpoint
//! being unplugged, there being no endpoint at all for a while, the audio
//! engine dropping data because the consumer stalled. All of those continue the
//! recording with synthesised silence and a log line (AGENTS.md sections 16 and
//! 17), so none of them is an error here.
//!
//! What remains is the cases where there is nothing to capture and no prospect
//! of one.

use core::fmt;
use std::error::Error;

use crate::format::AudioFormat;

/// System audio capture could not start, or could not continue.
#[derive(Debug)]
#[non_exhaustive]
pub enum AudioError {
    /// The machine has no default render endpoint, so there is no system audio
    /// to record and no format to give a track.
    ///
    /// This is only ever reported when a capture is *opened*. An endpoint
    /// disappearing during a recording is survivable — the track continues as
    /// silence until one comes back — because a recording in progress is worth
    /// more than the audio it is missing. A recording that has not started yet
    /// has nothing to protect, so the caller is told plainly instead.
    NoEndpoint,
    /// The machine has no input device, so there is no microphone to record.
    ///
    /// Reported when a microphone capture is *opened*, for the same reason
    /// [`NoEndpoint`](Self::NoEndpoint) is: there is no format to give a track
    /// and no recording in progress to protect. A microphone disappearing
    /// during a recording is survivable and is not this — the track becomes
    /// silence until one comes back.
    NoMicrophone,
    /// The microphone the user chose is not connected.
    ///
    /// Distinct from [`NoMicrophone`](Self::NoMicrophone) because the answer is
    /// different: the machine may well have other microphones, and the user can
    /// plug this one in or pick another (AGENTS.md section 45). A capture on a
    /// chosen device never silently moves to a different one.
    MicrophoneUnavailable {
        /// The name the device was last known by, which is what the user
        /// recognises. It is remembered with the choice, because a device that
        /// is not there cannot be asked its name.
        name: String,
    },
    /// The endpoint's mix format is one this crate cannot convert.
    ///
    /// Shared-mode WASAPI presents 16-, 24- or 32-bit integer or 32-bit float,
    /// and all four are handled. Anything else is refused rather than
    /// reinterpreted, because reinterpreting sample data produces full-scale
    /// noise rather than a quiet mistake.
    UnsupportedFormat {
        /// What the endpoint said, in the words `IAudioClient::GetMixFormat`
        /// used: the format tag, the bit depth and the subformat GUID.
        described: String,
    },
    /// A read was attempted on a capture that has been closed.
    NotOpen,
    /// A Windows API failed in a way this crate could not classify.
    Platform {
        /// What was being attempted, phrased so the message reads as a
        /// sentence: `"activating the audio client for the default endpoint"`.
        operation: &'static str,
        /// The platform error underneath.
        source: Box<dyn Error + Send + Sync>,
    },
}

impl AudioError {
    /// Describes a format this crate will not convert.
    #[must_use]
    pub fn unsupported_format(described: impl Into<String>) -> Self {
        Self::UnsupportedFormat {
            described: described.into(),
        }
    }
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoEndpoint => f.write_str(
                "this machine has no default audio output device, so there is no system \
                 audio to record",
            ),
            Self::NoMicrophone => f.write_str(
                "no microphone is connected, so there is nothing to record on the \
                 microphone track. Connect one, or turn the microphone track off",
            ),
            Self::MicrophoneUnavailable { name } => write!(
                f,
                "the microphone Clipped is set to record ({name}) is not connected. \
                 Connect it, or choose a different microphone"
            ),
            Self::UnsupportedFormat { described } => write!(
                f,
                "the audio output device presents samples in a format Clipped cannot \
                 convert ({described})"
            ),
            Self::NotOpen => f.write_str("this system audio capture has been closed"),
            Self::Platform { operation, source } => {
                write!(f, "system audio capture failed while {operation}: {source}")
            }
        }
    }
}

impl Error for AudioError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Platform { source, .. } => Some(source.as_ref()),
            Self::NoEndpoint
            | Self::NoMicrophone
            | Self::MicrophoneUnavailable { .. }
            | Self::UnsupportedFormat { .. }
            | Self::NotOpen => None,
        }
    }
}

/// What one read produced.
///
/// Reading never fails because the endpoint misbehaved; it reports what
/// happened and carries on. That is the shape AGENTS.md section 16 asks for,
/// and it is why the interesting outcomes are variants here rather than
/// variants of [`AudioError`].
///
/// Deliberately not `#[non_exhaustive]`, unlike [`AudioError`]. A caller has to
/// decide what to do about every outcome a read can have, and a new one
/// appearing should break the callers that have not thought about it rather
/// than fall into a wildcard arm that quietly does nothing with it. The same
/// reasoning as `clipped_capture::Acquisition`.
#[derive(Debug)]
pub enum Capture<'a> {
    /// Samples, exactly contiguous with the previous ones, whether the
    /// endpoint produced them or this crate synthesised the silence.
    Samples(crate::CapturedAudio<'a>),
    /// The timeout passed with nothing to report.
    ///
    /// Only reachable with a timeout shorter than the endpoint's packet
    /// period; a capture read with a timeout of tens of milliseconds or more
    /// returns [`Samples`](Self::Samples) even when the endpoint is silent,
    /// because silence is something to report.
    Idle,
    /// The endpoint being captured was replaced by one this capture cannot
    /// continue on, and the track has become synthesised silence.
    ///
    /// The user switched from speakers to a headset whose mix format differs —
    /// a different sample rate or a different channel count — and this crate
    /// does not resample or remix
    /// ([issue #30](https://github.com/wildware-uk/clipped/issues/30)). Rather
    /// than change shape underneath a muxer that has already written a stream
    /// header, or end a recording over a headset, the capture keeps the
    /// timeline running as silence and says so once. A caller that wants the
    /// new endpoint's audio opens a new capture.
    FormatChanged(AudioFormat),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_say_what_failed_and_keep_the_cause() {
        let error = AudioError::Platform {
            operation: "activating the audio client for the default endpoint",
            source: Box::new(std::io::Error::other("AUDCLNT_E_DEVICE_IN_USE")),
        };
        assert_eq!(
            error.to_string(),
            "system audio capture failed while activating the audio client for the \
             default endpoint: AUDCLNT_E_DEVICE_IN_USE"
        );
        assert!(
            error.source().is_some(),
            "the platform error must stay reachable through Error::source"
        );
    }

    #[test]
    fn an_unsupported_format_says_what_the_endpoint_offered() {
        // A user-facing message with no numbers in it is a support ticket
        // nobody can answer (AGENTS.md section 15).
        let error = AudioError::unsupported_format(
            "tag 0xfffe, 8 bits per sample, subformat 00000001-0000-0010-8000-00aa00389b71",
        );
        assert!(error.to_string().contains("8 bits per sample"));
        assert!(error.source().is_none());
    }
}
