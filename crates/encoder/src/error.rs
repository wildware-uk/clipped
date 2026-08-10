//! What goes wrong while encoding, and how it says so.
//!
//! Every failure in this module names three things before it says anything
//! else: which encoder was asked, which codec it was asked for, and at what
//! resolution. That is not decoration. "Encoder failed" arrives in a bug report
//! with no way to reproduce it; "NVIDIA NVENC could not encode 3840x2160 AV1:
//! the encoder reported that the resolution exceeds its maximum of 4096x4096"
//! is a sentence a user can act on and a maintainer can diagnose (AGENTS.md
//! section 15).
//!
//! The context is carried by [`EncodeError`] itself rather than repeated in
//! every variant, so a new failure mode cannot forget to mention it.

use core::fmt;

use crate::codec::{Codec, EncoderKind, Resolution};
use crate::config::SurfaceFormat;
use crate::frame::SurfaceKind;

/// Which encoder was doing what, for a failure to describe itself with.
///
/// Constructed once when a session is opened and copied into every error that
/// session produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EncodeContext {
    encoder: EncoderKind,
    codec: Codec,
    resolution: Resolution,
}

impl EncodeContext {
    /// The encoder, codec and resolution a failure should name.
    #[must_use]
    pub const fn new(encoder: EncoderKind, codec: Codec, resolution: Resolution) -> Self {
        Self {
            encoder,
            codec,
            resolution,
        }
    }

    /// Which encoder family was asked.
    #[must_use]
    pub const fn encoder(&self) -> EncoderKind {
        self.encoder
    }

    /// Which codec it was asked for.
    #[must_use]
    pub const fn codec(&self) -> Codec {
        self.codec
    }

    /// The picture size it was asked for.
    #[must_use]
    pub const fn resolution(&self) -> Resolution {
        self.resolution
    }
}

impl fmt::Display for EncodeContext {
    /// `NVIDIA NVENC could not encode 2560x1440 HEVC`, the opening clause of
    /// every message in this module.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} could not encode {} {}",
            self.encoder, self.resolution, self.codec
        )
    }
}

/// An encoder failure, with the encoder, codec and resolution attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodeError {
    context: EncodeContext,
    kind: EncodeErrorKind,
}

impl EncodeError {
    /// Attaches `context` to `kind`.
    #[must_use]
    pub const fn new(context: EncodeContext, kind: EncodeErrorKind) -> Self {
        Self { context, kind }
    }

    /// Which encoder, codec and resolution this failure is about.
    #[must_use]
    pub const fn context(&self) -> EncodeContext {
        self.context
    }

    /// What went wrong.
    #[must_use]
    pub const fn kind(&self) -> &EncodeErrorKind {
        &self.kind
    }

    /// Whether retrying with the same configuration could plausibly work.
    ///
    /// This is the question a session asks when a recording fails to start: a
    /// [session limit](EncodeErrorKind::SessionLimitReached) or a
    /// [lost device](EncodeErrorKind::DeviceLost) may clear on its own, while
    /// an unsupported codec never will, and offering "try again" for the second
    /// kind wastes the user's time (AGENTS.md section 45).
    #[must_use]
    pub const fn is_transient(&self) -> bool {
        matches!(
            self.kind,
            EncodeErrorKind::SessionLimitReached
                | EncodeErrorKind::DeviceLost
                | EncodeErrorKind::OutOfMemory
        )
    }
}

impl fmt::Display for EncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.context, self.kind)
    }
}

impl std::error::Error for EncodeError {}

/// The ways encoding fails, independent of which encoder is failing.
///
/// Platform-neutral on purpose: NVENC, AMF, Quick Sync and the software encoder
/// all fail in these ways, and a caller that wants to react to "there is no
/// session slot left" should not have to know whose error code said so. Vendor
/// detail survives in [`Api`](Self::Api) and in the logs.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EncodeErrorKind {
    /// The vendor's encoder runtime is not installed, or will not load.
    ///
    /// On a machine with the right GPU this usually means a driver that was
    /// half installed or half removed.
    RuntimeUnavailable {
        /// The library that was looked for, as in `nvEncodeAPI64.dll`.
        library: &'static str,
        /// What the loader said.
        detail: String,
    },
    /// The runtime loaded but is older than the interface this build compiles
    /// against, which means the graphics driver needs updating.
    DriverTooOld {
        /// The interface version this build needs, as `major.minor`.
        required: (u32, u32),
        /// The newest version the installed driver offers.
        installed: (u32, u32),
        /// The oldest driver release that carries `required`, so the message
        /// can say what to install rather than only what is missing.
        minimum_driver: &'static str,
    },
    /// This encoder cannot produce this codec on this hardware.
    ///
    /// Distinct from "the codec is unknown": the encoder answered, and the
    /// answer was no.
    CodecUnsupported,
    /// The picture is larger than the encoder can handle.
    ResolutionUnsupported {
        /// The largest picture the encoder reported it can encode.
        maximum: Resolution,
    },
    /// The frames on offer are in a layout this encoder cannot take.
    SurfaceFormatUnsupported {
        /// What was offered.
        offered: SurfaceFormat,
        /// What this encoder accepts.
        supported: &'static [SurfaceFormat],
    },
    /// The frames are in a kind of graphics resource this encoder cannot bind.
    SurfaceKindUnsupported {
        /// What was offered.
        offered: SurfaceKind,
        /// What this encoder accepts.
        supported: &'static [SurfaceKind],
    },
    /// Every concurrent encoding session the hardware allows is already in use.
    ///
    /// Consumer NVIDIA cards cap concurrent NVENC sessions, and the other
    /// sessions may belong to another application entirely, so this is a
    /// transient condition rather than a misconfiguration.
    SessionLimitReached,
    /// The graphics device was reset or removed underneath the session — a
    /// driver reset, a GPU hot-unplug, or the driver's timeout detection firing
    /// (AGENTS.md section 16).
    ///
    /// Everything the session held is gone; a new session on a new device is
    /// the only recovery.
    DeviceLost,
    /// The encoder ran out of memory, or out of some other resource it does not
    /// name more precisely.
    OutOfMemory,
    /// The configuration is not one this encoder can be asked for.
    Configuration {
        /// What is wrong with it, in the user's terms.
        detail: String,
    },
    /// A frame arrived with a presentation time earlier than one already
    /// submitted.
    ///
    /// Refused rather than passed on: timestamps that go backwards produce a
    /// file whose playback stalls or jumps, and the encoder is the last place
    /// that can still tell the difference between a bug and a recording
    /// (AGENTS.md section 22).
    TimestampWentBackwards {
        /// The presentation time of the previous frame, in nanoseconds.
        previous_nanos: u128,
        /// The presentation time that was submitted, in nanoseconds.
        submitted_nanos: u128,
    },
    /// The caller submitted more frames than it drained packets, and the
    /// encoder has no output buffer left.
    ///
    /// A contract violation rather than a hardware condition: see
    /// [`VideoEncoder`](crate::VideoEncoder) for the loop that cannot hit it.
    OutputBuffersExhausted {
        /// How many output buffers the session holds.
        capacity: usize,
    },
    /// The session has been shut down and cannot be used again.
    NotRunning,
    /// The encoder API refused a call, and this is what it said.
    ///
    /// The escape hatch for everything the variants above do not model. It
    /// keeps the vendor's own words rather than flattening them, because an
    /// unrecognised status code in a bug report is still worth more than
    /// "unknown error".
    Api {
        /// The call that failed, as in `nvEncEncodePicture`.
        operation: &'static str,
        /// The vendor's status code.
        status: i32,
        /// The vendor's name for that status, where there is one.
        status_name: &'static str,
        /// Anything more the runtime had to say.
        detail: String,
    },
}

impl fmt::Display for EncodeErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RuntimeUnavailable { library, detail } => write!(
                formatter,
                "its runtime {library} could not be loaded ({detail}); the graphics driver is \
                 missing or damaged"
            ),
            Self::DriverTooOld {
                required,
                installed,
                minimum_driver,
            } => write!(
                formatter,
                "the graphics driver supports encoder interface {}.{} and this build needs {}.{}; \
                 update the driver to {minimum_driver} or newer",
                installed.0, installed.1, required.0, required.1
            ),
            Self::CodecUnsupported => {
                formatter.write_str("this hardware does not encode that codec")
            }
            Self::ResolutionUnsupported { maximum } => write!(
                formatter,
                "the largest picture this encoder accepts is {maximum}"
            ),
            Self::SurfaceFormatUnsupported { offered, supported } => write!(
                formatter,
                "it cannot take {offered} frames; it accepts {}",
                Listed(supported)
            ),
            Self::SurfaceKindUnsupported { offered, supported } => write!(
                formatter,
                "it cannot bind a {offered}; it accepts {}",
                Listed(supported)
            ),
            Self::SessionLimitReached => formatter.write_str(
                "every encoding session this hardware allows is already in use, possibly by \
                 another application",
            ),
            Self::DeviceLost => {
                formatter.write_str("the graphics device was reset or removed while it was running")
            }
            Self::OutOfMemory => formatter.write_str("the encoder ran out of memory"),
            Self::Configuration { detail } => formatter.write_str(detail),
            Self::TimestampWentBackwards {
                previous_nanos,
                submitted_nanos,
            } => write!(
                formatter,
                "a frame arrived {} ns before the previous one ({submitted_nanos} ns after \
                 {previous_nanos} ns); capture timestamps must not go backwards",
                previous_nanos.saturating_sub(*submitted_nanos)
            ),
            Self::OutputBuffersExhausted { capacity } => write!(
                formatter,
                "all {capacity} output buffers are still holding encoded frames; drain the \
                 packets from one submission before making the next"
            ),
            Self::NotRunning => formatter.write_str("the encoding session has been shut down"),
            Self::Api {
                operation,
                status,
                status_name,
                detail,
            } => {
                write!(
                    formatter,
                    "{operation} failed with {status_name} ({status})"
                )?;
                if !detail.is_empty() {
                    write!(formatter, ": {detail}")?;
                }
                Ok(())
            }
        }
    }
}

/// Writes a slice as `a, b or c`, for the "it accepts ..." clauses above.
struct Listed<'a, T>(&'a [T]);

impl<T: fmt::Display> fmt::Display for Listed<'_, T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return formatter.write_str("nothing");
        }

        let last = self.0.len() - 1;
        for (index, item) in self.0.iter().enumerate() {
            if index > 0 {
                formatter.write_str(if index == last { " or " } else { ", " })?;
            }
            write!(formatter, "{item}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> EncodeContext {
        EncodeContext::new(EncoderKind::Nvenc, Codec::Hevc, Resolution::new(2560, 1440))
    }

    #[test]
    fn every_message_names_the_encoder_the_codec_and_the_resolution() {
        // The acceptance criterion of issue #15, asserted against every failure
        // mode rather than against one, because the context is what makes a bug
        // report reproducible and a variant that quietly dropped it would look
        // fine in isolation.
        //
        // Every variant of `EncodeErrorKind` is in this list, and a new one
        // belongs here too. The compiler cannot ask for that — the enum is
        // `#[non_exhaustive]` and this is a list of values, not a match — so
        // the second assertion below is what earns the list its keep: it fails
        // on a variant that names the context and then says nothing.
        let kinds = [
            EncodeErrorKind::RuntimeUnavailable {
                library: "nvEncodeAPI64.dll",
                detail: "module not found".to_owned(),
            },
            EncodeErrorKind::DriverTooOld {
                required: (12, 0),
                installed: (11, 1),
                minimum_driver: "522.25",
            },
            EncodeErrorKind::CodecUnsupported,
            EncodeErrorKind::ResolutionUnsupported {
                maximum: Resolution::new(4096, 4096),
            },
            EncodeErrorKind::SurfaceFormatUnsupported {
                offered: SurfaceFormat::Rgb10A2Unorm,
                supported: &[SurfaceFormat::Bgra8Unorm],
            },
            EncodeErrorKind::SurfaceKindUnsupported {
                offered: SurfaceKind::D3d11Texture2D,
                supported: &[SurfaceKind::D3d11Texture2D],
            },
            EncodeErrorKind::SessionLimitReached,
            EncodeErrorKind::DeviceLost,
            EncodeErrorKind::OutOfMemory,
            EncodeErrorKind::Configuration {
                detail: "1919x1080 has an odd dimension".to_owned(),
            },
            EncodeErrorKind::TimestampWentBackwards {
                previous_nanos: 2_000_000_000,
                submitted_nanos: 1_000_000_000,
            },
            EncodeErrorKind::OutputBuffersExhausted { capacity: 8 },
            EncodeErrorKind::NotRunning,
            EncodeErrorKind::Api {
                operation: "nvEncEncodePicture",
                status: 8,
                status_name: "NV_ENC_ERR_INVALID_PARAM",
                detail: String::new(),
            },
        ];

        for kind in kinds {
            let message = EncodeError::new(context(), kind.clone()).to_string();
            assert!(
                message.starts_with("NVIDIA NVENC could not encode 2560x1440 HEVC: "),
                "a {kind:?} error does not name the encoder, resolution and codec: {message}"
            );
            assert!(
                message.len() > "NVIDIA NVENC could not encode 2560x1440 HEVC: ".len() + 10,
                "a {kind:?} error names the context and then says nothing useful: {message}"
            );
        }
    }

    #[test]
    fn a_driver_that_is_too_old_says_which_driver_to_install() {
        let message = EncodeError::new(
            context(),
            EncodeErrorKind::DriverTooOld {
                required: (12, 0),
                installed: (11, 1),
                minimum_driver: "522.25",
            },
        )
        .to_string();

        assert!(message.contains("11.1"), "{message}");
        assert!(message.contains("12.0"), "{message}");
        assert!(
            message.contains("522.25"),
            "a version comparison the reader cannot act on: {message}"
        );
    }

    #[test]
    fn only_the_failures_that_can_clear_are_transient() {
        // What decides whether the recorder offers to try again.
        assert!(EncodeError::new(context(), EncodeErrorKind::SessionLimitReached).is_transient());
        assert!(EncodeError::new(context(), EncodeErrorKind::DeviceLost).is_transient());
        assert!(!EncodeError::new(context(), EncodeErrorKind::CodecUnsupported).is_transient());
        assert!(!EncodeError::new(
            context(),
            EncodeErrorKind::ResolutionUnsupported {
                maximum: Resolution::new(4096, 4096)
            }
        )
        .is_transient());
    }

    #[test]
    fn a_list_of_alternatives_reads_as_a_sentence() {
        assert_eq!(Listed::<u8>(&[]).to_string(), "nothing");
        assert_eq!(Listed(&[1]).to_string(), "1");
        assert_eq!(Listed(&[1, 2]).to_string(), "1 or 2");
        assert_eq!(Listed(&[1, 2, 3]).to_string(), "1, 2 or 3");
    }
}
