//! The buffers a capture hands over, and where each one came from.

use core::time::Duration;

use crate::format::AudioFormat;
use crate::time::AudioTimestamp;

/// Whether the samples in a buffer were captured or synthesised.
///
/// The distinction is not decoration. A track that contains synthesised
/// silence contains it because the endpoint said nothing for that period, and
/// somebody diagnosing a recording with a suspicious quiet passage needs to
/// know whether the silence was the machine's or this crate's. It is also what
/// a future mixer needs in order to skip work it cannot hear
/// ([issue #29](https://github.com/wildware-uk/clipped/issues/29)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SampleOrigin {
    /// Samples WASAPI delivered.
    Endpoint,
    /// Silence this crate synthesised to cover a period the endpoint produced
    /// nothing for; see the `timeline` module.
    SynthesisedSilence,
}

/// A block of captured audio, borrowed from the capture that produced it.
///
/// Samples are interleaved `f32` in `[-1.0, 1.0]`, whatever the endpoint's own
/// sample format is (`format` module). The buffer borrows the capture mutably,
/// so the compiler refuses to let a caller hold two at once or keep one across
/// the next read — which is what lets the capture reuse one allocation for the
/// whole recording instead of allocating per packet.
///
/// # Privacy
///
/// [`Debug`] describes the buffer and never its contents; see the
/// implementation below. Nothing else in this crate can print the samples
/// either, but this type is the one that leaves it, and the guarantee
/// `docs/audio-routing.md` makes about microphone audio never reaching a log
/// (AGENTS.md section 13) is only worth making if it survives the first
/// consumer that writes `tracing::debug!(?buffer)`.
pub struct CapturedAudio<'a> {
    samples: &'a [f32],
    format: AudioFormat,
    timestamp: AudioTimestamp,
    device_timestamp: Option<AudioTimestamp>,
    origin: SampleOrigin,
}

impl core::fmt::Debug for CapturedAudio<'_> {
    /// Describes the buffer: how many frames, when, in what shape, from where.
    ///
    /// Written by hand rather than derived because the derived one prints the
    /// samples, and these samples may be a microphone — a whole instalment of
    /// somebody's room, in a log file, from one `{:?}` written months from now
    /// in another crate. `clipped-logging` keeps its fields safe by giving them
    /// types that cannot hold user content; this does the same thing for the
    /// one type in this crate that holds any.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("CapturedAudio")
            .field("frames", &self.frames())
            .field("timestamp", &self.timestamp)
            .field("format", &self.format)
            .field("origin", &self.origin)
            .finish()
    }
}

impl<'a> CapturedAudio<'a> {
    /// Wraps a block of interleaved samples.
    #[must_use]
    pub const fn new(
        samples: &'a [f32],
        format: AudioFormat,
        timestamp: AudioTimestamp,
        origin: SampleOrigin,
    ) -> Self {
        Self {
            samples,
            format,
            timestamp,
            device_timestamp: None,
            origin,
        }
    }

    /// Records the position the endpoint itself reported for these samples.
    ///
    /// See [`device_timestamp`](Self::device_timestamp) for what it is for.
    /// Only [`SampleOrigin::Endpoint`] buffers have one, because synthesised
    /// silence covers a period the device never described.
    #[must_use]
    pub const fn with_device_timestamp(mut self, device: AudioTimestamp) -> Self {
        self.device_timestamp = Some(device);
        self
    }

    /// The interleaved samples, `channels()` of them per frame.
    #[must_use]
    pub const fn samples(&self) -> &'a [f32] {
        self.samples
    }

    /// The shape of the samples.
    #[must_use]
    pub const fn format(&self) -> AudioFormat {
        self.format
    }

    /// When the first frame in this buffer was heard.
    ///
    /// Consecutive buffers from one capture are exactly contiguous: this
    /// timestamp is always the previous buffer's timestamp plus the previous
    /// buffer's duration, with no gaps and no overlaps, however the endpoint
    /// behaved.
    #[must_use]
    pub const fn timestamp(&self) -> AudioTimestamp {
        self.timestamp
    }

    /// Where the *endpoint* said this buffer's first frame belongs, as opposed
    /// to where the track puts it.
    ///
    /// [`timestamp`](Self::timestamp) counts samples: it is the track's anchor
    /// plus every frame emitted since, so consecutive buffers are exactly
    /// contiguous and the track is the length of the recording. This is the
    /// other account of the same moment — the performance-counter position
    /// WASAPI attached to the packet the samples came from, adjusted for any
    /// frames trimmed off its front.
    ///
    /// **The difference between the two is the audio/video offset.** The sample
    /// count advances at the endpoint's own rate and the counter position
    /// advances at the reference clock's, so the gap between them is exactly how
    /// far the audio track has slid against the video, in nanoseconds, at that
    /// moment. Nothing else in the pipeline can see it: by the time the samples
    /// reach a muxer the two accounts have been reconciled into one timestamp.
    /// Feeding the pair to `clipped_capture::DriftEstimator` is what turns "the
    /// recording sounded fine" into a number; `docs/av-sync.md` is the model.
    ///
    /// [`None`] for [`SampleOrigin::SynthesisedSilence`], which covers a period
    /// the endpoint never described and therefore has no position of its own to
    /// disagree with.
    #[must_use]
    pub const fn device_timestamp(&self) -> Option<AudioTimestamp> {
        self.device_timestamp
    }

    /// Whether these samples were captured or synthesised.
    #[must_use]
    pub const fn origin(&self) -> SampleOrigin {
        self.origin
    }

    /// Frames in this buffer.
    #[must_use]
    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.format.channels().get())
    }

    /// How long this buffer lasts.
    #[must_use]
    pub fn duration(&self) -> Duration {
        Duration::from_nanos(self.format.frames_to_nanos(self.frames() as u64))
    }
}

#[cfg(test)]
mod tests {
    use core::num::{NonZeroU16, NonZeroU32};

    use super::*;
    use crate::format::{ChannelMask, SampleFormat};

    #[test]
    fn a_buffers_length_is_read_in_frames_rather_than_samples() {
        // The mistake this guards against is dividing a stereo buffer's sample
        // count by nothing and reporting a track twice as long as it is.
        let format = AudioFormat::new(
            NonZeroU32::new(48_000).expect("48 kHz is not zero"),
            NonZeroU16::new(2).expect("stereo is not zero channels"),
            ChannelMask::from_bits(0x3),
            SampleFormat::Float32,
        );
        let samples = vec![0.0f32; 960];
        let buffer = CapturedAudio::new(
            &samples,
            format,
            AudioTimestamp::from_nanos(0),
            SampleOrigin::Endpoint,
        );

        assert_eq!(buffer.frames(), 480);
        assert_eq!(buffer.duration(), Duration::from_millis(10));
    }

    #[test]
    fn printing_a_buffer_describes_it_and_never_prints_what_it_contains() {
        // AGENTS.md section 13: microphone content must not reach a log. A
        // derived `Debug` would put a whole instalment of it there the first
        // time anybody wrote `tracing::debug!(?buffer)`, so the samples here
        // are values that could not occur by accident and must not appear.
        let format = AudioFormat::new(
            NonZeroU32::new(48_000).expect("48 kHz is not zero"),
            NonZeroU16::new(1).expect("mono is not zero channels"),
            ChannelMask::from_bits(0x4),
            SampleFormat::Float32,
        );
        let samples = [0.123_456_79_f32, -0.987_654_3, 0.246_913_58];
        let buffer = CapturedAudio::new(
            &samples,
            format,
            AudioTimestamp::from_nanos(1_000),
            SampleOrigin::Endpoint,
        );

        let printed = format!("{buffer:?}");
        for sample in samples {
            let value = format!("{sample}");
            assert!(
                !printed.contains(&value),
                "a captured sample ({value}) reached a printed buffer: {printed}"
            );
        }
        assert!(
            printed.contains("frames: 3"),
            "the buffer still has to describe itself: {printed}"
        );
        assert!(
            printed.contains("Endpoint"),
            "the buffer still has to say where its samples came from: {printed}"
        );
    }
}
