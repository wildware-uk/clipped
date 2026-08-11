//! What a recording was asked for.
//!
//! [`RecordingSettings`] is the whole of the session's input. It is expressed
//! in this crate's own vocabulary rather than in `clipped-capture`'s or
//! `clipped-encoder`'s, for one reason: `apps/recorder` would otherwise have to
//! depend on both of them directly to name the types it passes down, and a
//! command-line front end reaching two layers past the crate it calls is how a
//! boundary stops meaning anything (AGENTS.md section 5).
//!
//! Everything here is a value the caller has already validated as far as it can
//! be validated without hardware — the recorder's `options` module does that
//! for the command line. What is checked *here* is what only the session knows:
//! that the frame rate is not zero, and that the requested size is one this
//! build can actually produce.

use std::path::{Path, PathBuf};

use clipped_capture::{CaptureTarget, FrameSize, TargetHandle, TargetKind, TargetProperties};
use clipped_encoder::{Codec, EncoderKind};

use crate::error::SessionError;

/// The default frame rate, when a caller does not care.
///
/// The same 60 the command line defaults to (`apps/recorder/src/options.rs`),
/// stated here as well because a library that only works when its caller
/// remembers to fill a field in is a library with a trap in it.
pub const DEFAULT_FRAMERATE: u32 = 60;

/// Which window or display to record, and what is known about it.
///
/// The caller has already resolved this: `clipped-windows` turned a
/// `--window`/`--process`/`--pid` selector into one window, with its handle and
/// its client size. This carries the answer, not the question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureTargetSettings {
    handle: u64,
    kind: TargetKind,
    width: u32,
    height: u32,
    content_protected: bool,
}

impl CaptureTargetSettings {
    /// A window of `width` by `height` physical pixels, by its platform handle.
    #[must_use]
    pub const fn window(handle: u64, width: u32, height: u32) -> Self {
        Self {
            handle,
            kind: TargetKind::Window,
            width,
            height,
            content_protected: false,
        }
    }

    /// A whole display, by its platform handle.
    #[must_use]
    pub const fn monitor(handle: u64, width: u32, height: u32) -> Self {
        Self {
            handle,
            kind: TargetKind::Monitor,
            width,
            height,
            content_protected: false,
        }
    }

    /// Marks a target the system refuses to let anything capture.
    ///
    /// Passed through so that backend selection can decline it with a reason
    /// rather than the recording being a black rectangle
    /// ([`TargetProperties::with_content_protected`]).
    #[must_use]
    pub const fn content_protected(mut self, protected: bool) -> Self {
        self.content_protected = protected;
        self
    }

    /// Whether this is a window or a whole display.
    #[must_use]
    pub const fn kind(&self) -> TargetKind {
        self.kind
    }

    /// The size the target was measured at, in physical pixels.
    #[must_use]
    pub const fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The properties backend selection reasons over.
    ///
    /// # Errors
    ///
    /// [`SessionError::TargetHasNoPixels`] when either dimension is zero, which
    /// is what Windows reports for a minimised window. A capture started
    /// against one produces an encoder session that fails on its first frame,
    /// so it is refused here with something a user can act on.
    pub(crate) fn properties(&self) -> Result<TargetProperties, SessionError> {
        let size =
            FrameSize::new(self.width, self.height).ok_or(SessionError::TargetHasNoPixels)?;
        Ok(TargetProperties::new(self.kind, size).with_content_protected(self.content_protected))
    }

    /// The target a backend is initialised against.
    ///
    /// # Errors
    ///
    /// As [`properties`](Self::properties).
    pub(crate) fn target(&self) -> Result<CaptureTarget, SessionError> {
        Ok(CaptureTarget::new(
            TargetHandle::from_raw(self.handle),
            self.properties()?,
        ))
    }
}

/// How large the encoded video should be.
///
/// [`Fixed`](Self::Fixed) is accepted only when it names the size the target is
/// already producing. There is no scaler in the pipeline yet — a captured
/// texture goes to the encoder untouched, which is the whole point of the
/// zero-copy path — so a genuine resize would have to be a GPU blit that does
/// not exist, and quietly recording at the source size instead would be a
/// setting that silently does nothing (AGENTS.md section 27).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResolutionSetting {
    /// Encode at whatever size the capture target is.
    #[default]
    Source,
    /// Encode at exactly this size.
    Fixed {
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
    },
}

/// Which codec to produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CodecPreference {
    /// Let the session choose, preferring the most efficient codec the chosen
    /// encoder was *measured* to support (SPEC.md section 9).
    #[default]
    Automatic,
    /// This codec, or a failure explaining why it could not be produced.
    Fixed(Codec),
}

/// Which encoder family to encode with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EncoderPreference {
    /// Let the session choose, in the order
    /// [`clipped_encoder::recommend`] ranks them, falling back to the next
    /// candidate when one refuses to open.
    #[default]
    Automatic,
    /// This encoder, or a failure explaining why it could not be opened. No
    /// fallback: a user who named an encoder wants to know it was not used.
    Fixed(EncoderKind),
}

/// Everything a recording needs to be told before it starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingSettings {
    target: CaptureTargetSettings,
    output: PathBuf,
    resolution: ResolutionSetting,
    framerate: u32,
    codec: CodecPreference,
    encoder: EncoderPreference,
    capture_cursor: bool,
    audio_requested: bool,
    overwrite: bool,
}

impl RecordingSettings {
    /// A recording of `target` into `output`, with every other choice left to
    /// the session.
    #[must_use]
    pub fn new(target: CaptureTargetSettings, output: PathBuf) -> Self {
        Self {
            target,
            output,
            resolution: ResolutionSetting::Source,
            framerate: DEFAULT_FRAMERATE,
            codec: CodecPreference::Automatic,
            encoder: EncoderPreference::Automatic,
            capture_cursor: false,
            audio_requested: false,
            overwrite: false,
        }
    }

    /// Sets the size to encode at.
    #[must_use]
    pub const fn with_resolution(mut self, resolution: ResolutionSetting) -> Self {
        self.resolution = resolution;
        self
    }

    /// Sets the frame rate to encode at, which is also the ceiling the capture
    /// loop holds the recording to.
    #[must_use]
    pub const fn with_framerate(mut self, framerate: u32) -> Self {
        self.framerate = framerate;
        self
    }

    /// Sets the codec to produce.
    #[must_use]
    pub const fn with_codec(mut self, codec: CodecPreference) -> Self {
        self.codec = codec;
        self
    }

    /// Sets the encoder to encode with.
    #[must_use]
    pub const fn with_encoder(mut self, encoder: EncoderPreference) -> Self {
        self.encoder = encoder;
        self
    }

    /// Includes or excludes the mouse cursor, where the backend can.
    #[must_use]
    pub const fn with_capture_cursor(mut self, capture: bool) -> Self {
        self.capture_cursor = capture;
        self
    }

    /// Records that the caller asked for one or more audio tracks.
    ///
    /// The session cannot produce them yet, and this is how it knows to say so
    /// once rather than writing a file that silently has no sound in it. It is
    /// deliberately a bare flag and not a device selection: carrying a
    /// selection the pipeline cannot honour would look like support for it.
    #[must_use]
    pub const fn with_audio_requested(mut self, requested: bool) -> Self {
        self.audio_requested = requested;
        self
    }

    /// Allows an existing recording at [`output`](Self::output) to be replaced.
    ///
    /// Off by default and never inferred. A recording cannot be made again, so
    /// replacing one is a thing the caller has to have said (AGENTS.md section
    /// 56).
    #[must_use]
    pub const fn with_overwrite(mut self, overwrite: bool) -> Self {
        self.overwrite = overwrite;
        self
    }

    /// What to capture.
    #[must_use]
    pub const fn target(&self) -> &CaptureTargetSettings {
        &self.target
    }

    /// Where the recording is written.
    #[must_use]
    pub fn output(&self) -> &Path {
        &self.output
    }

    /// The size to encode at.
    #[must_use]
    pub const fn resolution(&self) -> ResolutionSetting {
        self.resolution
    }

    /// The frame rate to encode at.
    #[must_use]
    pub const fn framerate(&self) -> u32 {
        self.framerate
    }

    /// The codec to produce.
    #[must_use]
    pub const fn codec(&self) -> CodecPreference {
        self.codec
    }

    /// The encoder to encode with.
    #[must_use]
    pub const fn encoder(&self) -> EncoderPreference {
        self.encoder
    }

    /// Whether the cursor should appear in the recording.
    #[must_use]
    pub const fn capture_cursor(&self) -> bool {
        self.capture_cursor
    }

    /// Whether the caller asked for audio that this build cannot record.
    #[must_use]
    pub const fn audio_requested(&self) -> bool {
        self.audio_requested
    }

    /// Whether an existing recording at the output path may be replaced.
    #[must_use]
    pub const fn overwrite(&self) -> bool {
        self.overwrite
    }

    /// The size the encoder will be configured for, given what capture is
    /// actually producing.
    ///
    /// # Errors
    ///
    /// [`SessionError::ScalingNotSupported`] when a fixed size was asked for
    /// that is not the size the capture is producing.
    pub(crate) fn encode_size(&self, captured: FrameSize) -> Result<(u32, u32), SessionError> {
        match self.resolution {
            ResolutionSetting::Source => Ok((captured.width(), captured.height())),
            ResolutionSetting::Fixed { width, height }
                if width == captured.width() && height == captured.height() =>
            {
                Ok((width, height))
            }
            ResolutionSetting::Fixed { width, height } => Err(SessionError::ScalingNotSupported {
                requested: (width, height),
                captured: (captured.width(), captured.height()),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(width: u32, height: u32) -> FrameSize {
        FrameSize::new(width, height).expect("a real size")
    }

    #[test]
    fn a_minimised_window_is_refused_rather_than_captured_at_no_size() {
        // Windows reports a zero client area for a minimised window, and a
        // capture started against one fails at the encoder's first frame with
        // a message about the encoder.
        let target = CaptureTargetSettings::window(0x1234, 0, 0);
        assert!(matches!(
            target.properties(),
            Err(SessionError::TargetHasNoPixels)
        ));
    }

    #[test]
    fn source_resolution_follows_whatever_capture_produced() {
        let settings = RecordingSettings::new(
            CaptureTargetSettings::window(1, 1280, 720),
            PathBuf::from("out.mkv"),
        );
        assert_eq!(
            settings
                .encode_size(size(1280, 720))
                .expect("the source size is always encodable"),
            (1280, 720)
        );
        // The target was measured at 1280x720 and capture handed over 1284x741,
        // which is what a window capture including its chrome looks like. The
        // encoder follows the frames, not the measurement.
        assert_eq!(
            settings
                .encode_size(size(1284, 741))
                .expect("the source size is always encodable"),
            (1284, 741)
        );
    }

    #[test]
    fn a_fixed_size_that_matches_the_source_is_accepted() {
        let settings = RecordingSettings::new(
            CaptureTargetSettings::window(1, 1280, 720),
            PathBuf::from("out.mkv"),
        )
        .with_resolution(ResolutionSetting::Fixed {
            width: 1280,
            height: 720,
        });
        assert_eq!(
            settings
                .encode_size(size(1280, 720))
                .expect("the source size is always encodable"),
            (1280, 720)
        );
    }

    #[test]
    fn a_fixed_size_that_would_need_scaling_is_refused_rather_than_ignored() {
        // The failure this prevents is a `--resolution 1920x1080` that produces
        // a 2560x1440 file: a setting that silently does nothing.
        let settings = RecordingSettings::new(
            CaptureTargetSettings::window(1, 2560, 1440),
            PathBuf::from("out.mkv"),
        )
        .with_resolution(ResolutionSetting::Fixed {
            width: 1920,
            height: 1080,
        });

        let error = settings
            .encode_size(size(2560, 1440))
            .expect_err("nothing in this build can scale a frame");
        let message = error.to_string();
        assert!(
            message.contains("1920x1080") && message.contains("2560x1440"),
            "the refusal must name both sizes: {message}"
        );
    }

    #[test]
    fn the_defaults_are_the_ones_the_command_line_documents() {
        let settings = RecordingSettings::new(
            CaptureTargetSettings::window(1, 1280, 720),
            PathBuf::from("out.mkv"),
        );
        assert_eq!(settings.framerate(), DEFAULT_FRAMERATE);
        assert_eq!(settings.resolution(), ResolutionSetting::Source);
        assert_eq!(settings.codec(), CodecPreference::Automatic);
        assert_eq!(settings.encoder(), EncoderPreference::Automatic);
        assert!(!settings.capture_cursor());
        assert!(!settings.audio_requested());
    }
}
