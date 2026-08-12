//! Turning one decoded audio frame's bytes into sample values.
//!
//! libavcodec hands back whatever layout the decoder works in: PCM in a
//! Matroska file arrives as interleaved 16-bit integers, Opus and AAC as planar
//! 32-bit floats, FLAC as 16- or 32-bit integers. All of them have to become the
//! same thing before a peak can be taken of them.
//!
//! # Why this is not `swresample`
//!
//! FFmpeg ships a converter that would do this, and it is deliberately not used.
//! A peak measured after a resampler is a peak of the resampler's output:
//! `swresample` dithers when it narrows a sample, and applies whatever gain a
//! channel-layout remix implies. Neither is wrong for playback and both are
//! wrong for a measurement — a waveform is supposed to describe the file, not a
//! processed version of it. Reading the bytes directly is exact, it is one
//! fewer FFmpeg library to hold a lifetime for, and every case is testable
//! without a decoder ([the tests below](self)).
//!
//! What it costs is that an unknown sample format is refused rather than
//! silently converted. That is the right way round: a format this does not know
//! becomes a named error and an issue, not a waveform that is quietly wrong.

use core::fmt;

/// The sample layouts a decoder in the pinned FFmpeg build can produce.
///
/// Each is either interleaved — every channel's sample for one instant, then
/// the next instant — or planar, where each channel is a separate contiguous
/// buffer. FFmpeg spells that as a separate sample format for each; here it is
/// a flag, because it changes only where a channel's samples are, not what they
/// mean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SampleKind {
    /// Unsigned 8-bit, centred on 128.
    U8,
    /// Signed 16-bit.
    S16,
    /// Signed 32-bit.
    S32,
    /// Signed 64-bit.
    S64,
    /// 32-bit float, nominally in ±1.0.
    F32,
    /// 64-bit float, nominally in ±1.0.
    F64,
}

impl SampleKind {
    /// How many bytes one sample of this kind occupies.
    pub(crate) fn width(self) -> usize {
        match self {
            Self::U8 => 1,
            Self::S16 => 2,
            Self::S32 | Self::F32 => 4,
            Self::S64 | Self::F64 => 8,
        }
    }

    /// Reads one sample and scales it to ±1.0.
    ///
    /// `bytes` must be exactly [`width`](Self::width) long; the caller slices
    /// it, which is what keeps the bounds check in one place.
    fn read(self, bytes: &[u8]) -> f32 {
        match self {
            // FFmpeg's `AV_SAMPLE_FMT_U8` is offset binary: silence is 128.
            Self::U8 => (f32::from(bytes[0]) - 128.0) / 128.0,
            Self::S16 => f32::from(i16::from_le_bytes(take(bytes))) / 32_768.0,
            Self::S32 => i32::from_le_bytes(take(bytes)) as f32 / 2_147_483_648.0,
            Self::S64 => i64::from_le_bytes(take(bytes)) as f32 / 9_223_372_036_854_775_808.0,
            Self::F32 => f32::from_le_bytes(take(bytes)),
            // Narrowed to `f32` because a peak is quantised to eight bits in
            // the end; the extra mantissa cannot survive that.
            Self::F64 => f64::from_le_bytes(take(bytes)) as f32,
        }
    }
}

/// The first `N` bytes of a slice the caller has already sized.
fn take<const N: usize>(bytes: &[u8]) -> [u8; N] {
    let mut buffer = [0u8; N];
    buffer.copy_from_slice(&bytes[..N]);
    buffer
}

/// A decoded frame's sample format: what a sample is, and where a channel's
/// samples are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SampleLayout {
    kind: SampleKind,
    planar: bool,
}

impl SampleLayout {
    pub(crate) fn new(kind: SampleKind, planar: bool) -> Self {
        Self { kind, planar }
    }

    /// What one sample is. Only the tests need to ask; everything else goes
    /// through [`plane_bytes`](Self::plane_bytes) and
    /// [`read_channel`](Self::read_channel).
    #[cfg(test)]
    pub(crate) fn kind(self) -> SampleKind {
        self.kind
    }

    /// Whether each channel has its own buffer.
    ///
    /// Which is also how many of `AVFrame::extended_data`'s pointers are
    /// meaningful: one per channel when planar, one in total when not.
    pub(crate) fn is_planar(self) -> bool {
        self.planar
    }

    /// How many bytes of one plane hold `frames` frames of `channels` channels.
    pub(crate) fn plane_bytes(self, frames: usize, channels: usize) -> usize {
        let per_frame = if self.planar { 1 } else { channels };
        self.kind.width() * per_frame * frames
    }

    /// Reads one channel out of one plane, appending to `out`.
    ///
    /// `plane` is the whole plane for a planar layout, or the whole interleaved
    /// buffer otherwise; `channel` is which channel to pick out of it, and is
    /// ignored when planar because the plane *is* the channel.
    ///
    /// Stops at whatever `plane` actually holds rather than trusting `frames`,
    /// so a frame whose buffer is shorter than its declared size — which is a
    /// corrupt file, not a bug here — produces fewer samples instead of reading
    /// past the end.
    pub(crate) fn read_channel(
        self,
        plane: &[u8],
        channels: usize,
        channel: usize,
        frames: usize,
        out: &mut Vec<f32>,
    ) {
        out.clear();
        let width = self.kind.width();
        let (stride, offset) = if self.planar {
            (width, 0)
        } else {
            (width * channels.max(1), width * channel)
        };

        for frame in 0..frames {
            let start = frame * stride + offset;
            let Some(sample) = plane.get(start..start + width) else {
                break;
            };
            out.push(self.kind.read(sample));
        }
    }
}

impl fmt::Display for SampleLayout {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.kind {
            SampleKind::U8 => "u8",
            SampleKind::S16 => "s16",
            SampleKind::S32 => "s32",
            SampleKind::S64 => "s64",
            SampleKind::F32 => "flt",
            SampleKind::F64 => "dbl",
        };
        write!(formatter, "{kind}{}", if self.planar { "p" } else { "" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(layout: SampleLayout, plane: &[u8], channels: usize, channel: usize) -> Vec<f32> {
        let mut out = Vec::new();
        let frames =
            plane.len() / layout.kind().width() / if layout.is_planar() { 1 } else { channels };
        layout.read_channel(plane, channels, channel, frames, &mut out);
        out
    }

    #[test]
    fn unsigned_eight_bit_is_centred_on_128_rather_than_on_zero() {
        let layout = SampleLayout::new(SampleKind::U8, false);
        let samples = read(layout, &[128, 255, 0, 192], 1, 0);
        assert_eq!(samples, vec![0.0, 127.0 / 128.0, -1.0, 0.5]);
    }

    #[test]
    fn signed_formats_scale_to_full_scale() {
        let s16 = SampleLayout::new(SampleKind::S16, false);
        let samples = read(s16, &i16::to_le_bytes(-32_768), 1, 0);
        assert_eq!(samples, vec![-1.0]);
        let samples = read(s16, &i16::to_le_bytes(16_384), 1, 0);
        assert_eq!(samples, vec![0.5]);

        let s32 = SampleLayout::new(SampleKind::S32, false);
        let samples = read(s32, &i32::to_le_bytes(i32::MIN), 1, 0);
        assert_eq!(samples, vec![-1.0]);

        let s64 = SampleLayout::new(SampleKind::S64, false);
        let samples = read(s64, &i64::to_le_bytes(i64::MIN), 1, 0);
        assert_eq!(samples, vec![-1.0]);
    }

    #[test]
    fn float_formats_are_taken_as_they_are() {
        let f32_layout = SampleLayout::new(SampleKind::F32, false);
        assert_eq!(read(f32_layout, &f32::to_le_bytes(0.25), 1, 0), vec![0.25]);

        let f64_layout = SampleLayout::new(SampleKind::F64, false);
        assert_eq!(
            read(f64_layout, &f64::to_le_bytes(-0.75), 1, 0),
            vec![-0.75]
        );
    }

    #[test]
    fn an_interleaved_frame_is_read_one_channel_at_a_time() {
        // Two channels: left is a ramp up, right is a ramp down. Reading the
        // wrong stride would mix them, which is exactly the bug that makes two
        // audio tracks look identical.
        let layout = SampleLayout::new(SampleKind::S16, false);
        let mut bytes = Vec::new();
        for (left, right) in [(0i16, 0i16), (8_192, -8_192), (16_384, -16_384)] {
            bytes.extend_from_slice(&left.to_le_bytes());
            bytes.extend_from_slice(&right.to_le_bytes());
        }
        assert_eq!(read(layout, &bytes, 2, 0), vec![0.0, 0.25, 0.5]);
        assert_eq!(read(layout, &bytes, 2, 1), vec![0.0, -0.25, -0.5]);
    }

    #[test]
    fn a_planar_frame_reads_the_plane_it_was_given() {
        let layout = SampleLayout::new(SampleKind::F32, true);
        let mut plane = Vec::new();
        for value in [0.1f32, 0.2, 0.3] {
            plane.extend_from_slice(&value.to_le_bytes());
        }
        // `channel` is ignored: the plane is the channel.
        assert_eq!(read(layout, &plane, 2, 1), vec![0.1, 0.2, 0.3]);
        assert!(layout.is_planar());
        assert!(!SampleLayout::new(SampleKind::F32, false).is_planar());
    }

    #[test]
    fn a_plane_shorter_than_the_frame_claims_stops_rather_than_reading_past_it() {
        let layout = SampleLayout::new(SampleKind::S16, false);
        let mut out = Vec::new();
        // Two samples of bytes, but the frame says there are ten.
        layout.read_channel(&[0, 0, 0, 64], 1, 0, 10, &mut out);
        assert_eq!(out, vec![0.0, 0.5]);
    }

    #[test]
    fn a_plane_is_sized_from_the_layout() {
        let interleaved = SampleLayout::new(SampleKind::S16, false);
        assert_eq!(interleaved.plane_bytes(100, 2), 400);
        let planar = SampleLayout::new(SampleKind::S16, true);
        assert_eq!(planar.plane_bytes(100, 2), 200);
    }

    #[test]
    fn a_layout_names_itself_the_way_ffmpeg_does() {
        assert_eq!(SampleLayout::new(SampleKind::F32, true).to_string(), "fltp");
        assert_eq!(SampleLayout::new(SampleKind::S16, false).to_string(), "s16");
    }
}
