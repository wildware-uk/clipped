//! What the endpoint's mix format is, and what is converted where.
//!
//! Windows decides the shared-mode mix format, not this crate: it is whatever
//! `IAudioClient::GetMixFormat` returns for the endpoint that happens to be the
//! default, and on the machines this was written against it is 48 kHz stereo
//! 32-bit float — but a USB interface at 44.1 kHz, a 7.1 receiver and a 24-bit
//! endpoint are all legitimate, so nothing here assumes.
//!
//! # What is converted, and what is not
//!
//! **Sample format is converted; sample rate and channel count are not.**
//!
//! Every captured buffer leaves this crate as interleaved `f32` in `[-1.0,
//! 1.0]`, whatever integer or float format the endpoint delivered
//! ([`SampleFormat`], and `convert` below). That conversion is lossless for
//! every integer width Windows can present, it is what mixing and per-source
//! gain (SPEC.md section 14) will want anyway, and it means one sample type
//! crosses the crate boundary rather than four.
//!
//! The sample rate and the channel count are passed through untouched and
//! reported in [`AudioFormat`]. What a *format* declares never changes here —
//! a 44.1 kHz endpoint stays a 44.1 kHz track — even though
//! [`crate::resample`] now nudges the number of frames a source's own samples
//! occupy, continuously and by a fraction of a percent, to keep that source's
//! clock aligned with the reference clock
//! ([issue #30](https://github.com/wildware-uk/clipped/issues/30)). Converting
//! between two endpoints that genuinely disagree about the rate — a recording
//! that has to keep running when the default device moves from 48 kHz to
//! 44.1 kHz — is not that, and is still refused: half a track of the user's own
//! samples and half a track of resampled ones, with nothing in the file saying
//! where the join is, is what AGENTS.md section 22 rules out.
//! [`crate::mix::rate`](crate::Mixer) does convert between rates, but only on
//! the compatibility mix's own copy of a source, which is a derived track and
//! not this one; see `docs/audio-routing.md`.
//! Downmixing is a decision about what the user hears, which this crate is not
//! entitled to make on its own (AGENTS.md section 21).

use core::fmt;
use core::num::{NonZeroU16, NonZeroU32};

/// How the endpoint presents each sample before this crate converts it.
///
/// Shared-mode WASAPI is float in practice, and these are the widths a Windows
/// endpoint can offer besides. The list is closed: an endpoint presenting
/// anything else is reported as
/// [`AudioError::UnsupportedFormat`](crate::AudioError::UnsupportedFormat)
/// rather than guessed at, because guessing produces noise at full scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SampleFormat {
    /// 32-bit IEEE float, already in `[-1.0, 1.0]`. Converted by copying.
    Float32,
    /// 16-bit signed integer.
    Int16,
    /// 24-bit signed integer in a three-byte container.
    Int24,
    /// 32-bit signed integer, or 24 valid bits in a four-byte container: the
    /// unused low bits are zero either way, so the conversion is the same.
    Int32,
}

impl SampleFormat {
    /// Bytes one sample of one channel occupies in the endpoint's buffer.
    #[must_use]
    pub const fn bytes_per_sample(self) -> usize {
        match self {
            Self::Float32 | Self::Int32 => 4,
            Self::Int16 => 2,
            Self::Int24 => 3,
        }
    }
}

impl fmt::Display for SampleFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Float32 => "32-bit float",
            Self::Int16 => "16-bit integer",
            Self::Int24 => "24-bit integer",
            Self::Int32 => "32-bit integer",
        })
    }
}

/// Which speaker each channel of an interleaved buffer belongs to.
///
/// This is `WAVEFORMATEXTENSIBLE::dwChannelMask` carried through unchanged.
/// AGENTS.md section 21 lists channel layouts among the things audio code has
/// to account for, and the way to account for them here is to report the
/// endpoint's own answer rather than to infer a layout from the channel count:
/// two channels are almost always front left and right, and "almost always" is
/// how a 5.1 recording ends up with the centre channel in the wrong place.
///
/// A mask of zero means the endpoint did not say — a plain `WAVEFORMATEX` with
/// no extensible part — and the caller has nothing to go on but the channel
/// count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ChannelMask(u32);

/// The `SPEAKER_*` bits, in the order `dwChannelMask` assigns them, with the
/// names to print.
const SPEAKERS: [(u32, &str); 18] = [
    (0x0000_0001, "front left"),
    (0x0000_0002, "front right"),
    (0x0000_0004, "front centre"),
    (0x0000_0008, "low frequency"),
    (0x0000_0010, "back left"),
    (0x0000_0020, "back right"),
    (0x0000_0040, "front left of centre"),
    (0x0000_0080, "front right of centre"),
    (0x0000_0100, "back centre"),
    (0x0000_0200, "side left"),
    (0x0000_0400, "side right"),
    (0x0000_0800, "top centre"),
    (0x0000_1000, "top front left"),
    (0x0000_2000, "top front centre"),
    (0x0000_4000, "top front right"),
    (0x0000_8000, "top back left"),
    (0x0001_0000, "top back centre"),
    (0x0002_0000, "top back right"),
];

impl ChannelMask {
    /// Wraps a `dwChannelMask` value.
    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    /// The mask as Windows reported it.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// How many speaker positions the mask names.
    ///
    /// Equal to the format's channel count when the endpoint is consistent,
    /// and zero when it named no positions at all.
    #[must_use]
    pub const fn named_positions(self) -> u32 {
        self.0.count_ones()
    }
}

impl fmt::Display for ChannelMask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 == 0 {
            return f.write_str("unspecified");
        }
        let mut first = true;
        for (bit, name) in SPEAKERS {
            if self.0 & bit != 0 {
                if !first {
                    f.write_str(", ")?;
                }
                f.write_str(name)?;
                first = false;
            }
        }
        // Bits above the documented set exist (`SPEAKER_RESERVED`), and a
        // layout that used them would otherwise print as an incomplete list
        // that reads as though it were complete.
        let known: u32 = SPEAKERS.iter().map(|(bit, _)| bit).sum();
        if self.0 & !known != 0 {
            if !first {
                f.write_str(", ")?;
            }
            write!(f, "and unnamed positions ({:#010x})", self.0 & !known)?;
        }
        Ok(())
    }
}

/// The shape of the samples a capture delivers.
///
/// Fixed for the life of one capture. A capture whose endpoint is replaced by
/// one with a different mix format reports the change rather than quietly
/// changing shape underneath the caller; see
/// [`Capture`](crate::Capture).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AudioFormat {
    sample_rate: NonZeroU32,
    channels: NonZeroU16,
    channel_mask: ChannelMask,
    endpoint_samples: SampleFormat,
}

impl AudioFormat {
    /// Describes an endpoint's mix format.
    #[must_use]
    pub const fn new(
        sample_rate: NonZeroU32,
        channels: NonZeroU16,
        channel_mask: ChannelMask,
        endpoint_samples: SampleFormat,
    ) -> Self {
        Self {
            sample_rate,
            channels,
            channel_mask,
            endpoint_samples,
        }
    }

    /// Frames per second, as the endpoint reported it.
    #[must_use]
    pub const fn sample_rate(&self) -> NonZeroU32 {
        self.sample_rate
    }

    /// Samples per frame.
    #[must_use]
    pub const fn channels(&self) -> NonZeroU16 {
        self.channels
    }

    /// Which speaker each channel belongs to.
    #[must_use]
    pub const fn channel_mask(&self) -> ChannelMask {
        self.channel_mask
    }

    /// The sample format the endpoint delivers, before conversion to `f32`.
    ///
    /// Informational: every buffer this crate produces is `f32` whatever this
    /// says. It is reported because a recording that sounds wrong is usually
    /// diagnosed by asking what the endpoint actually was.
    #[must_use]
    pub const fn endpoint_samples(&self) -> SampleFormat {
        self.endpoint_samples
    }

    /// How long `frames` frames last, in nanoseconds.
    ///
    /// Exact within one nanosecond per call, and — because callers convert a
    /// running total rather than accumulating per-buffer conversions — the
    /// rounding error does not grow with the length of a recording.
    #[must_use]
    pub const fn frames_to_nanos(&self, frames: u64) -> u64 {
        ((frames as u128 * 1_000_000_000) / self.sample_rate.get() as u128) as u64
    }

    /// How many whole frames fit in `nanos` nanoseconds.
    #[must_use]
    pub const fn nanos_to_frames(&self, nanos: u64) -> u64 {
        ((nanos as u128 * self.sample_rate.get() as u128) / 1_000_000_000) as u64
    }

    /// Whether two formats are interchangeable for an in-progress recording.
    ///
    /// The channel mask is deliberately not part of this: an endpoint that
    /// reports the same rate, the same channel count and the same sample type
    /// but labels its two channels differently produces samples the muxer can
    /// keep writing, and refusing it would turn a cosmetic difference into a
    /// silent track.
    #[must_use]
    pub const fn is_interchangeable_with(&self, other: &Self) -> bool {
        self.sample_rate.get() == other.sample_rate.get()
            && self.channels.get() == other.channels.get()
    }
}

impl fmt::Display for AudioFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} Hz, {} channel(s) ({}), {} from the endpoint",
            self.sample_rate, self.channels, self.channel_mask, self.endpoint_samples
        )
    }
}

/// Converts one endpoint buffer into interleaved `f32`, appending to `out`.
///
/// `bytes` is the endpoint's buffer for whole frames only; a trailing partial
/// sample is ignored rather than read past. The integer conversions divide by
/// the format's negative full scale, which is the convention that maps the most
/// negative representable sample to exactly `-1.0` and therefore cannot produce
/// a value outside `[-1.0, 1.0]` — so nothing downstream has to clip a sample
/// this function produced.
pub(crate) fn append_as_f32(bytes: &[u8], format: SampleFormat, out: &mut Vec<f32>) {
    match format {
        SampleFormat::Float32 => {
            out.extend(
                bytes
                    .chunks_exact(4)
                    .map(|sample| f32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]])),
            );
        }
        SampleFormat::Int16 => {
            out.extend(
                bytes
                    .chunks_exact(2)
                    .map(|sample| f32::from(i16::from_le_bytes([sample[0], sample[1]])) / 32_768.0),
            );
        }
        SampleFormat::Int24 => {
            out.extend(bytes.chunks_exact(3).map(|sample| {
                // Sign-extend by putting the three bytes in the top of an i32
                // and shifting back down arithmetically.
                let raw = i32::from_le_bytes([0, sample[0], sample[1], sample[2]]) >> 8;
                raw as f32 / 8_388_608.0
            }));
        }
        SampleFormat::Int32 => {
            out.extend(bytes.chunks_exact(4).map(|sample| {
                let raw = i32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]);
                raw as f32 / 2_147_483_648.0
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo_48k(endpoint_samples: SampleFormat) -> AudioFormat {
        AudioFormat::new(
            NonZeroU32::new(48_000).expect("48 kHz is not zero"),
            NonZeroU16::new(2).expect("stereo is not zero channels"),
            ChannelMask::from_bits(0x3),
            endpoint_samples,
        )
    }

    #[test]
    fn a_full_scale_sample_of_every_endpoint_format_converts_to_full_scale() {
        // The property that matters for the tone test: an endpoint that hands
        // over integers must produce the same amplitude as one that hands over
        // floats, or a captured tone measured in decibels depends on which
        // sound card is plugged in.
        let mut out = Vec::new();

        append_as_f32(&(-1.0f32).to_le_bytes(), SampleFormat::Float32, &mut out);
        append_as_f32(&i16::MIN.to_le_bytes(), SampleFormat::Int16, &mut out);
        append_as_f32(&[0x00, 0x00, 0x80], SampleFormat::Int24, &mut out);
        append_as_f32(&i32::MIN.to_le_bytes(), SampleFormat::Int32, &mut out);

        assert_eq!(out, vec![-1.0, -1.0, -1.0, -1.0]);
    }

    #[test]
    fn silence_and_half_scale_convert_the_same_way_in_every_format() {
        let mut out = Vec::new();
        append_as_f32(&0.0f32.to_le_bytes(), SampleFormat::Float32, &mut out);
        append_as_f32(&0i16.to_le_bytes(), SampleFormat::Int16, &mut out);
        append_as_f32(&[0, 0, 0], SampleFormat::Int24, &mut out);
        append_as_f32(&0i32.to_le_bytes(), SampleFormat::Int32, &mut out);
        assert_eq!(out, vec![0.0, 0.0, 0.0, 0.0]);

        let mut out = Vec::new();
        append_as_f32(&(-16_384i16).to_le_bytes(), SampleFormat::Int16, &mut out);
        // -2^22 in three bytes, little-endian: the same fraction of full scale.
        append_as_f32(&[0x00, 0x00, 0xc0], SampleFormat::Int24, &mut out);
        append_as_f32(
            &(-1_073_741_824i32).to_le_bytes(),
            SampleFormat::Int32,
            &mut out,
        );
        assert_eq!(out, vec![-0.5, -0.5, -0.5]);
    }

    #[test]
    fn a_positive_twenty_four_bit_sample_is_not_sign_extended_into_a_negative_one() {
        // The conversion that is easy to get wrong: 24-bit samples have no
        // Rust integer type, so the sign has to be reconstructed by hand, and
        // getting it wrong turns the top half of every waveform inside out.
        let mut out = Vec::new();
        append_as_f32(&[0xff, 0xff, 0x7f], SampleFormat::Int24, &mut out);
        append_as_f32(&[0x00, 0x00, 0x40], SampleFormat::Int24, &mut out);
        append_as_f32(&[0xff, 0xff, 0xff], SampleFormat::Int24, &mut out);
        assert!((out[0] - 0.999_999_9).abs() < 1e-6, "got {}", out[0]);
        assert!((out[1] - 0.5).abs() < 1e-6, "got {}", out[1]);
        assert!((out[2] + 1.0 / 8_388_608.0).abs() < 1e-9, "got {}", out[2]);
    }

    #[test]
    fn a_trailing_partial_sample_is_ignored_rather_than_read_past() {
        let mut out = Vec::new();
        append_as_f32(&[0, 0, 0, 0, 0], SampleFormat::Float32, &mut out);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn frames_and_nanoseconds_convert_without_drifting() {
        let format = stereo_48k(SampleFormat::Float32);
        assert_eq!(format.frames_to_nanos(48_000), 1_000_000_000);
        assert_eq!(format.nanos_to_frames(1_000_000_000), 48_000);

        // Eight hours at 48 kHz is 1.38e9 frames; `frames * 1e9` is 1.4e18,
        // which fits in a u64 only just, and one more factor of ten would not.
        // The 128-bit arithmetic is what keeps a long session's timestamps
        // meaningful rather than wrapping.
        let eight_hours = 8 * 60 * 60 * 48_000;
        assert_eq!(
            format.frames_to_nanos(eight_hours),
            8 * 60 * 60 * 1_000_000_000
        );
    }

    #[test]
    fn a_forty_four_one_endpoint_converts_as_exactly_as_a_forty_eight() {
        let format = AudioFormat::new(
            NonZeroU32::new(44_100).expect("44.1 kHz is not zero"),
            NonZeroU16::new(2).expect("stereo is not zero channels"),
            ChannelMask::from_bits(0x3),
            SampleFormat::Float32,
        );
        assert_eq!(format.frames_to_nanos(44_100), 1_000_000_000);
        assert_eq!(format.nanos_to_frames(1_000_000_000), 44_100);
    }

    #[test]
    fn a_different_sample_type_alone_does_not_make_an_endpoint_unusable() {
        // What `is_interchangeable_with` is for: the default endpoint moving
        // from a float sound card to a 16-bit one is a conversion this crate
        // already does, so the recording continues. A different sample rate is
        // not, because nothing here resamples.
        let float = stereo_48k(SampleFormat::Float32);
        let integer = stereo_48k(SampleFormat::Int16);
        assert!(float.is_interchangeable_with(&integer));

        let forty_four = AudioFormat::new(
            NonZeroU32::new(44_100).expect("44.1 kHz is not zero"),
            NonZeroU16::new(2).expect("stereo is not zero channels"),
            ChannelMask::from_bits(0x3),
            SampleFormat::Float32,
        );
        assert!(!float.is_interchangeable_with(&forty_four));

        let surround = AudioFormat::new(
            NonZeroU32::new(48_000).expect("48 kHz is not zero"),
            NonZeroU16::new(6).expect("5.1 is not zero channels"),
            ChannelMask::from_bits(0x3f),
            SampleFormat::Float32,
        );
        assert!(!float.is_interchangeable_with(&surround));
    }

    #[test]
    fn a_channel_mask_prints_the_speakers_it_names() {
        assert_eq!(
            ChannelMask::from_bits(0x3).to_string(),
            "front left, front right"
        );
        assert_eq!(
            ChannelMask::from_bits(0x3f).to_string(),
            "front left, front right, front centre, low frequency, back left, back right"
        );
        assert_eq!(ChannelMask::from_bits(0).to_string(), "unspecified");
        assert_eq!(ChannelMask::from_bits(0x3f).named_positions(), 6);

        // A layout using a bit outside the documented set must not print as a
        // shorter list that reads as complete.
        assert_eq!(
            ChannelMask::from_bits(0x8000_0003).to_string(),
            "front left, front right, and unnamed positions (0x80000000)"
        );
    }
}
