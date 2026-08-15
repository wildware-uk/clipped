//! Real coded video for the tests that save a clip and then decode it.
//!
//! A replay buffer holds whatever the encoder gave it and hands the same bytes
//! to the writer, so a test built on synthetic packets can prove the *selection*
//! and proves nothing at all about the file: a clip whose every packet fails to
//! decode still has one video stream, the right resolution and a plausible
//! duration. `decoded_frames_at_least` is the assertion that separates the two,
//! and it needs a real H.264 elementary stream to have anything to decode.
//!
//! The encoding is `clipped_media_validation::CodedVideo`, which is where that
//! fixture lives now that `apps/recorder` needs one too — see `tests/media`'s
//! `src/fixture.rs` for why it is built with the FFmpeg programs rather than
//! with `clipped-encoder`. What is left here is the mapping onto this crate's
//! own types, which the harness deliberately does not know about: it sits below
//! `clipped-encoder` and `clipped-muxer` in the layering.
//!
//! The fixture is produced once per test binary and shared, because encoding
//! seventy seconds of video for each of three tests would be the slowest thing
//! in the crate.

// Each test binary compiles this module separately and uses the part of it that
// it needs, so anything used by only one of them is "unused" in the others.
#![allow(dead_code)]

use std::sync::OnceLock;
use std::time::Duration;

use clipped_encoder::{EncodedPacket, PictureKind};
use clipped_media_validation::CodedVideo as Fixture;
use clipped_muxer::{FrameRate, VideoCodec, VideoTrack};

/// The picture size the fixture is encoded at.
///
/// Small: the tests care about how many frames come out and whether they
/// decode, not about how big they are, and seventy seconds of 1080p would make
/// this the slowest test in the workspace for nothing.
pub(crate) const WIDTH: u32 = 640;
pub(crate) const HEIGHT: u32 = 360;

/// Frames a second, and therefore the spacing of the timestamps the buffer is
/// fed.
pub(crate) const FRAMES_PER_SECOND: u64 = 60;

/// How many frames apart the keyframes are: two seconds, which is
/// `clipped_encoder::KeyframeInterval::DEFAULT` and `DEFAULT_SEGMENT`.
pub(crate) const KEYFRAME_INTERVAL: u64 = 120;

/// How much video the fixture holds.
///
/// Longer than the sixty seconds the first acceptance criterion asks for, so
/// that a buffer of a sixty-second window is measured while it is evicting
/// rather than while it is still filling.
pub(crate) const FIXTURE_SECONDS: u64 = 70;

/// When frame `index` is presented, in media time.
///
/// Integer nanoseconds from the frame number rather than a clock, so the tests
/// behave identically however fast the machine runs them (AGENTS.md section 25).
pub(crate) fn presentation_time(index: u64) -> Duration {
    Duration::from_nanos(index * 1_000_000_000 / FRAMES_PER_SECOND)
}

/// How long one frame occupies.
pub(crate) fn frame_interval() -> Duration {
    Duration::from_nanos(1_000_000_000 / FRAMES_PER_SECOND)
}

/// The shared fixture, in this crate's own vocabulary.
#[derive(Debug)]
pub(crate) struct CodedVideo {
    inner: Fixture,
}

impl CodedVideo {
    /// How many coded pictures there are.
    pub(crate) fn len(&self) -> usize {
        self.inner.len()
    }

    /// The packet for frame `index` of the stream, timed at `at`.
    ///
    /// The fixture is finite and the tests that push for a while go round it
    /// more than once, so the index wraps while the timestamp does not: what
    /// matters to the buffer is that the pictures are real and that a keyframe
    /// arrives every two seconds.
    pub(crate) fn packet(&self, index: u64, at: Duration) -> EncodedPacket<'_> {
        EncodedPacket::new(
            self.inner.picture(index),
            at,
            at,
            if self.inner.is_keyframe(index) {
                PictureKind::Keyframe
            } else {
                PictureKind::Predicted
            },
        )
    }

    /// The track description a clip of this video needs.
    ///
    /// The sequence and picture parameter sets are the container's mandatory
    /// out-of-band header; without them the file lists a video stream that
    /// nothing can decode.
    pub(crate) fn track(&self) -> VideoTrack {
        VideoTrack::new(VideoCodec::H264, WIDTH, HEIGHT)
            .with_frame_rate(
                FrameRate::per_second(u32::try_from(FRAMES_PER_SECOND).expect("60 fits"))
                    .expect("a real rate"),
            )
            .with_codec_private(self.inner.parameter_sets().to_vec())
            .with_name("Gameplay")
    }
}

/// The shared fixture, or [`None`] when the FFmpeg programs are not on this
/// machine.
///
/// A missing FFmpeg is a clean skip, exactly as it is everywhere else that
/// validates media — unless `CLIPPED_REQUIRE_MEDIA` is set, which
/// `require_media_tools` turns into a failure so that a machine which is
/// supposed to validate media cannot quietly stop doing it.
pub(crate) fn coded_video() -> Option<&'static CodedVideo> {
    static FIXTURE: OnceLock<Option<CodedVideo>> = OnceLock::new();

    FIXTURE
        .get_or_init(|| {
            Fixture::encode(
                WIDTH,
                HEIGHT,
                u32::try_from(FRAMES_PER_SECOND).expect("60 fits"),
                u32::try_from(KEYFRAME_INTERVAL).expect("120 fits"),
                u32::try_from(FIXTURE_SECONDS).expect("70 fits"),
            )
            .map(|inner| CodedVideo { inner })
        })
        .as_ref()
}
