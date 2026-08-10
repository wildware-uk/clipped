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
#[derive(Debug)]
pub struct CapturedAudio<'a> {
    samples: &'a [f32],
    format: AudioFormat,
    timestamp: AudioTimestamp,
    origin: SampleOrigin,
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
            origin,
        }
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
}
