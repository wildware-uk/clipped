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

/// Where one of a recording's audio tracks comes from.
///
/// One of these per source — system audio and the microphone — rather than a
/// single "record audio" flag, because the two are chosen independently and
/// each produces a track of its own that must never be silently merged with the
/// other (AGENTS.md section 21).
///
/// [`Off`](Self::Off) is not "record silence": no device is opened, and the
/// recording's layout declares no track for that source at all. A track that is
/// declared and empty means something different — the device was opened and
/// produced nothing — and the two must stay distinguishable in the file
/// (`docs/audio-routing.md`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AudioSourceSetting {
    /// Do not record this source.
    ///
    /// The default, and deliberately so. A library call must not open somebody's
    /// microphone because its caller forgot a field (AGENTS.md section 14);
    /// `clipped-recorder` is the product surface that defaults both sources on,
    /// and it says so explicitly.
    #[default]
    Off,
    /// Whatever Windows currently makes the default device, following it if the
    /// user changes it mid-recording.
    SystemDefault,
    /// A device whose name contains this text, and only that device.
    ///
    /// A capture on a chosen device waits for it rather than quietly recording
    /// a different one, so unplugging a headset produces silence on that track
    /// and not the audio of whatever Windows promoted (`docs/audio-routing.md`).
    Named(String),
}

impl AudioSourceSetting {
    /// Whether this source is recorded at all.
    #[must_use]
    pub const fn is_off(&self) -> bool {
        matches!(self, Self::Off)
    }
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
    system_audio: AudioSourceSetting,
    microphone: AudioSourceSetting,
    overwrite: bool,
    minimum_free_space: u64,
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
            system_audio: AudioSourceSetting::Off,
            microphone: AudioSourceSetting::Off,
            overwrite: false,
            minimum_free_space: crate::disk::DEFAULT_MINIMUM_FREE_SPACE,
        }
    }

    /// Sets how much of the output drive the recording refuses to consume.
    ///
    /// The recording is refused before it starts, and stopped cleanly if it is
    /// already running, when the volume it writes to has this much left
    /// (`crate::disk`). Zero turns the guard off, which is the caller saying it
    /// would rather fill the disk than lose the end of the recording.
    #[must_use]
    pub const fn with_minimum_free_space(mut self, bytes: u64) -> Self {
        self.minimum_free_space = bytes;
        self
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

    /// Sets where the system-audio track comes from.
    ///
    /// [`AudioSourceSetting::Named`] is not honoured for this source in this
    /// build and is refused when the recording starts: WASAPI loopback is
    /// opened against the endpoint Windows is *playing through*, and
    /// `clipped-audio` offers no way to name a different one
    /// ([issue #316](https://github.com/wildware-uk/clipped/issues/316)).
    /// Recording the default endpoint instead would be a setting that silently
    /// does something else (AGENTS.md section 27).
    #[must_use]
    pub fn with_system_audio(mut self, source: AudioSourceSetting) -> Self {
        self.system_audio = source;
        self
    }

    /// Sets which microphone the microphone track comes from.
    #[must_use]
    pub fn with_microphone(mut self, source: AudioSourceSetting) -> Self {
        self.microphone = source;
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

    /// Where the system-audio track comes from.
    #[must_use]
    pub const fn system_audio(&self) -> &AudioSourceSetting {
        &self.system_audio
    }

    /// Where the microphone track comes from.
    #[must_use]
    pub const fn microphone(&self) -> &AudioSourceSetting {
        &self.microphone
    }

    /// Whether this recording will open any audio device at all.
    #[must_use]
    pub const fn records_audio(&self) -> bool {
        !self.system_audio.is_off() || !self.microphone.is_off()
    }

    /// Whether an existing recording at the output path may be replaced.
    #[must_use]
    pub const fn overwrite(&self) -> bool {
        self.overwrite
    }

    /// How much of the output drive the recording refuses to consume.
    #[must_use]
    pub const fn minimum_free_space(&self) -> u64 {
        self.minimum_free_space
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
        assert!(
            !settings.records_audio(),
            "a library caller that said nothing must not have its microphone opened"
        );
        assert_eq!(settings.system_audio(), &AudioSourceSetting::Off);
        assert_eq!(settings.microphone(), &AudioSourceSetting::Off);
        assert_eq!(
            settings.minimum_free_space(),
            crate::disk::DEFAULT_MINIMUM_FREE_SPACE,
            "a caller that says nothing must still get the disk guard"
        );
    }

    #[test]
    fn each_audio_source_is_turned_on_and_off_independently_of_the_other() {
        // The pair has to stay two answers rather than one. A recording with
        // the microphone off and system audio on is the ordinary case, and a
        // settings object that collapsed them into "audio: yes" would either
        // open a microphone nobody asked for or leave the game silent.
        let base = RecordingSettings::new(
            CaptureTargetSettings::window(1, 1280, 720),
            PathBuf::from("out.mkv"),
        );

        let system_only = base
            .clone()
            .with_system_audio(AudioSourceSetting::SystemDefault);
        assert!(system_only.records_audio());
        assert_eq!(
            system_only.system_audio(),
            &AudioSourceSetting::SystemDefault
        );
        assert!(system_only.microphone().is_off());

        let microphone_only = base
            .clone()
            .with_microphone(AudioSourceSetting::Named("Yeti".to_owned()));
        assert!(microphone_only.records_audio());
        assert!(microphone_only.system_audio().is_off());
        assert_eq!(
            microphone_only.microphone(),
            &AudioSourceSetting::Named("Yeti".to_owned())
        );

        assert!(
            !base
                .with_system_audio(AudioSourceSetting::Off)
                .with_microphone(AudioSourceSetting::Off)
                .records_audio(),
            "both off is a video-only recording, and nothing should be opened for it"
        );
    }

    #[test]
    fn a_caller_that_would_rather_fill_the_disk_can_turn_the_guard_off() {
        // Zero has to survive the builder: a setting that is silently replaced
        // by the default is a control that does nothing (AGENTS.md section 27).
        let settings = RecordingSettings::new(
            CaptureTargetSettings::window(1, 1280, 720),
            PathBuf::from("out.mkv"),
        )
        .with_minimum_free_space(0);
        assert_eq!(settings.minimum_free_space(), 0);
    }
}
