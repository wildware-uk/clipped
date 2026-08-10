//! What an encoder is asked to produce.
//!
//! One structure, [`EncoderConfig`], describes a stream in terms every encoder
//! family understands: a codec, a picture size, a frame rate, how bits are
//! spent, how often a keyframe appears, what the colours mean and what the
//! incoming frames look like. Nothing in here is NVIDIA-specific, because the
//! same values have to configure AMF
//! ([#16](https://github.com/wildware-uk/clipped/issues/16)), Quick Sync
//! ([#17](https://github.com/wildware-uk/clipped/issues/17)) and the software
//! encoder ([#18](https://github.com/wildware-uk/clipped/issues/18)).
//!
//! Where a vendor offers a knob nobody else has, it does not appear here. The
//! backend picks a defensible value and documents it; a configuration surface
//! that is the union of four vendors' options is one nobody can reason about,
//! and SPEC.md section 9 asks for a recorder a user can point at a game rather
//! than an encoder front end.

use core::fmt;
use core::num::NonZeroU32;
use core::time::Duration;

use crate::codec::{Codec, Resolution};

/// How the pixels of an incoming frame are laid out.
///
/// `clipped-capture` has a `PixelFormat` naming the same layouts. They are not
/// shared because the two crates are in the same layer and neither may depend
/// on the other (README, "Dependency direction"), and because they answer
/// different questions: that one describes what a capture API produced, this
/// one describes what an encoder was configured to accept. `clipped-session`
/// converts, being the crate above both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SurfaceFormat {
    /// 8 bits per channel, blue-green-red-alpha order, unsigned normalised —
    /// `DXGI_FORMAT_B8G8R8A8_UNORM`. What both Windows capture APIs produce for
    /// ordinary SDR content, and the only format any encoder here accepts
    /// today.
    Bgra8Unorm,
    /// 10 bits per colour channel and 2 for alpha, unsigned normalised —
    /// `DXGI_FORMAT_R10G10B10A2_UNORM`. The usual HDR capture format, listed so
    /// that an encoder can refuse it by name rather than by silence
    /// ([#99](https://github.com/wildware-uk/clipped/issues/99)).
    Rgb10A2Unorm,
}

impl fmt::Display for SurfaceFormat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Bgra8Unorm => "BGRA8 unorm",
            Self::Rgb10A2Unorm => "RGB10A2 unorm",
        })
    }
}

/// Frames per second, as the exact ratio a container will store.
///
/// A ratio rather than a number because the rates that matter are not integers:
/// 59.94 is 60000/1001, and rounding it to 60 drifts by a frame every sixteen
/// seconds — an hour of recording ends three and a half seconds out of step with
/// its audio (AGENTS.md section 22).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameRate {
    numerator: NonZeroU32,
    denominator: NonZeroU32,
}

impl FrameRate {
    /// 30 frames per second.
    pub const FPS_30: Self = Self::whole(30);
    /// 60 frames per second.
    pub const FPS_60: Self = Self::whole(60);

    /// A rate of `numerator / denominator` frames per second.
    ///
    /// Returns [`None`] if either part is zero, which is not a frame rate.
    #[must_use]
    pub const fn new(numerator: u32, denominator: u32) -> Option<Self> {
        match (NonZeroU32::new(numerator), NonZeroU32::new(denominator)) {
            (Some(numerator), Some(denominator)) => Some(Self {
                numerator,
                denominator,
            }),
            _ => None,
        }
    }

    /// A whole number of frames per second.
    ///
    /// # Panics
    ///
    /// If `fps` is zero. This is a `const fn` so the panic happens while the
    /// crate is compiled for the constants above; a rate arrived at from
    /// configuration should go through [`new`](Self::new).
    #[must_use]
    pub const fn whole(fps: u32) -> Self {
        match Self::new(fps, 1) {
            Some(rate) => rate,
            None => panic!("a frame rate of zero frames per second is not a frame rate"),
        }
    }

    /// The numerator of the ratio.
    #[must_use]
    pub const fn numerator(&self) -> u32 {
        self.numerator.get()
    }

    /// The denominator of the ratio.
    #[must_use]
    pub const fn denominator(&self) -> u32 {
        self.denominator.get()
    }

    /// The rate as a number, for arithmetic and for display.
    #[must_use]
    pub fn as_f64(&self) -> f64 {
        f64::from(self.numerator()) / f64::from(self.denominator())
    }

    /// How many frames of this rate fit in `duration`, rounded to the nearest
    /// whole frame and never zero.
    ///
    /// Used to turn a keyframe interval expressed in seconds into the frame
    /// count every encoder API actually wants.
    #[must_use]
    pub fn frames_in(&self, duration: Duration) -> NonZeroU32 {
        let frames = duration.as_secs_f64() * self.as_f64();
        let rounded = frames.round();
        // Saturating rather than `as`: an absurd interval should clamp to a
        // representable one, not wrap around to a small number and produce a
        // keyframe every other frame.
        let clamped = if rounded >= f64::from(u32::MAX) {
            u32::MAX
        } else if rounded < 1.0 {
            1
        } else {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            {
                rounded as u32
            }
        };
        NonZeroU32::new(clamped).unwrap_or(NonZeroU32::MIN)
    }
}

impl fmt::Display for FrameRate {
    /// `60 fps`, or `59.94 fps` where the ratio is not a whole number.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.denominator() == 1 {
            write!(formatter, "{} fps", self.numerator())
        } else {
            write!(formatter, "{:.2} fps", self.as_f64())
        }
    }
}

/// A bit rate, in bits per second.
///
/// Bits rather than bytes because that is the unit every encoder API, every
/// container and every user-facing recorder uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BitRate(NonZeroU32);

impl BitRate {
    /// A rate in bits per second, or [`None`] for zero.
    #[must_use]
    pub const fn bits_per_second(bits: u32) -> Option<Self> {
        match NonZeroU32::new(bits) {
            Some(bits) => Some(Self(bits)),
            None => None,
        }
    }

    /// A rate in megabits per second, the unit a settings screen shows.
    ///
    /// # Panics
    ///
    /// If `megabits` is zero or larger than about 4295, which no encoder here
    /// accepts anyway.
    #[must_use]
    pub const fn megabits_per_second(megabits: u32) -> Self {
        match Self::bits_per_second(megabits * 1_000_000) {
            Some(rate) => rate,
            None => panic!("a bit rate of zero is not a bit rate"),
        }
    }

    /// The rate in bits per second.
    #[must_use]
    pub const fn as_bits_per_second(&self) -> u32 {
        self.0.get()
    }
}

impl fmt::Display for BitRate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bits = self.as_bits_per_second();
        if bits >= 1_000_000 {
            write!(formatter, "{:.1} Mbit/s", f64::from(bits) / 1_000_000.0)
        } else {
            write!(formatter, "{:.0} kbit/s", f64::from(bits) / 1_000.0)
        }
    }
}

/// How good the picture should be when quality, rather than size, is the
/// target.
///
/// The scale is the one every hardware encoder speaks: a quantisation level
/// from 1 to 51, where **lower is better**. It is inverted compared to the way
/// a user would put it, which is why this is a type with a documented range
/// rather than a bare integer that some call site is bound to read backwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QualityTarget(u8);

impl QualityTarget {
    /// The best quality the scale can express.
    pub const BEST: Self = Self(1);
    /// The worst quality the scale can express.
    pub const WORST: Self = Self(51);
    /// A sensible default for game footage: visually clean without spending
    /// bits on detail a recording of a game does not contain.
    pub const DEFAULT: Self = Self(23);

    /// A level from 1 (best) to 51 (worst), or [`None`] outside that range.
    #[must_use]
    pub const fn level(level: u8) -> Option<Self> {
        if level >= Self::BEST.0 && level <= Self::WORST.0 {
            Some(Self(level))
        } else {
            None
        }
    }

    /// The level, 1 to 51.
    #[must_use]
    pub const fn as_level(&self) -> u8 {
        self.0
    }
}

impl Default for QualityTarget {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl fmt::Display for QualityTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "quality level {}", self.0)
    }
}

/// How the encoder decides how many bits to spend.
///
/// The two modes SPEC.md section 10 asks for, and no more. A recorder that
/// fills a disk has two sensible policies — "this many bits per second,
/// whatever the picture" and "this good, whatever it costs" — and every further
/// mode a vendor offers is a variation on one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RateControl {
    /// Spend a fixed number of bits per second.
    ///
    /// Predictable file sizes, which is what a replay buffer needs to bound its
    /// memory, at the cost of wasting bits on a static menu and starving a busy
    /// scene.
    Bitrate {
        /// The rate to hold on average.
        average: BitRate,
        /// The most the encoder may spend in a burst, if it should be allowed
        /// to vary at all. [`None`] asks for a constant rate.
        peak: Option<BitRate>,
    },
    /// Spend whatever it takes to reach a quality, up to an optional ceiling.
    ///
    /// Better use of a disk over a long session — a quiet scene costs almost
    /// nothing — at the cost of a file size nobody can predict, which is why
    /// the ceiling exists.
    Quality {
        /// How good the picture should be.
        target: QualityTarget,
        /// A rate the encoder may not exceed even to reach `target`. [`None`]
        /// leaves it unbounded, which on a 4K stream can mean hundreds of
        /// megabits per second.
        ceiling: Option<BitRate>,
    },
}

impl RateControl {
    /// A constant bit rate of `average`.
    #[must_use]
    pub const fn constant(average: BitRate) -> Self {
        Self::Bitrate {
            average,
            peak: None,
        }
    }

    /// A quality target with no ceiling on the bit rate.
    #[must_use]
    pub const fn quality(target: QualityTarget) -> Self {
        Self::Quality {
            target,
            ceiling: None,
        }
    }
}

impl fmt::Display for RateControl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bitrate {
                average,
                peak: None,
            } => write!(formatter, "{average} constant"),
            Self::Bitrate {
                average,
                peak: Some(peak),
            } => write!(formatter, "{average} average, {peak} peak"),
            Self::Quality {
                target,
                ceiling: None,
            } => write!(formatter, "{target}"),
            Self::Quality {
                target,
                ceiling: Some(ceiling),
            } => write!(formatter, "{target}, at most {ceiling}"),
        }
    }
}

/// How often the stream restarts with a frame that depends on nothing before
/// it.
///
/// This is the granularity of everything the product does with a recording
/// afterwards: seeking, trimming and — most of all — the replay buffer, which
/// can only start a saved clip at a keyframe (SPEC.md section 7). Long
/// intervals compress better; short ones make a clip start where the user asked
/// rather than up to an interval earlier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyframeInterval {
    /// A keyframe every `n` frames.
    Frames(NonZeroU32),
    /// Only the first frame is a keyframe.
    ///
    /// Smallest possible file and useless for seeking. Present because a
    /// low-latency streaming path wants it and because "never" has to be
    /// expressible; a recorder should not choose it.
    Never,
}

impl KeyframeInterval {
    /// The default: a keyframe every two seconds, which is what the replay
    /// buffer's clip boundaries are worth in compression.
    pub const DEFAULT: Duration = Duration::from_secs(2);

    /// A keyframe every `interval` of real time at `rate`.
    #[must_use]
    pub fn every(interval: Duration, rate: FrameRate) -> Self {
        Self::Frames(rate.frames_in(interval))
    }

    /// The interval in frames, or [`None`] for [`Never`](Self::Never).
    #[must_use]
    pub const fn as_frames(&self) -> Option<NonZeroU32> {
        match self {
            Self::Frames(frames) => Some(*frames),
            Self::Never => None,
        }
    }
}

impl fmt::Display for KeyframeInterval {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frames(frames) => write!(formatter, "a keyframe every {frames} frames"),
            Self::Never => formatter.write_str("no keyframes after the first"),
        }
    }
}

/// What the numbers in the encoded picture mean.
///
/// Colour is where recordings go subtly wrong: a stream that is tagged one way
/// and encoded another plays back with washed-out or oversaturated colour, and
/// nobody notices until they compare it against the game. The tag is therefore
/// part of the configuration rather than a default buried in a backend, and
/// each field carries the code point the bitstream stores so that a backend
/// writes the number rather than inventing a mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ColourSpace {
    primaries: ColourPrimaries,
    transfer: TransferFunction,
    matrix: ColourMatrix,
    range: ColourRange,
}

impl ColourSpace {
    /// BT.709 with limited (16–235) range: what SDR video has meant since
    /// high definition arrived, and what every player assumes when a stream
    /// says nothing.
    pub const BT709_LIMITED: Self = Self {
        primaries: ColourPrimaries::Bt709,
        transfer: TransferFunction::Bt709,
        matrix: ColourMatrix::Bt709,
        range: ColourRange::Limited,
    };

    /// BT.709 with full (0–255) range.
    pub const BT709_FULL: Self = Self {
        range: ColourRange::Full,
        ..Self::BT709_LIMITED
    };

    /// The colour primaries.
    #[must_use]
    pub const fn primaries(&self) -> ColourPrimaries {
        self.primaries
    }

    /// The transfer function.
    #[must_use]
    pub const fn transfer(&self) -> TransferFunction {
        self.transfer
    }

    /// The matrix used to derive luma and chroma.
    #[must_use]
    pub const fn matrix(&self) -> ColourMatrix {
        self.matrix
    }

    /// Whether the samples use the full range of their bit depth.
    #[must_use]
    pub const fn range(&self) -> ColourRange {
        self.range
    }
}

impl Default for ColourSpace {
    fn default() -> Self {
        Self::BT709_LIMITED
    }
}

impl fmt::Display for ColourSpace {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.matrix, self.range)
    }
}

/// The chromaticities of the primaries, by their ITU-T H.273 code point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ColourPrimaries {
    /// BT.709, the SDR high-definition primaries. Code point 1.
    Bt709,
    /// BT.2020, the wide-gamut primaries HDR content uses. Code point 9.
    Bt2020,
}

impl ColourPrimaries {
    /// The value written into the bitstream (ITU-T H.273 table 2).
    #[must_use]
    pub const fn code_point(self) -> u32 {
        match self {
            Self::Bt709 => 1,
            Self::Bt2020 => 9,
        }
    }
}

/// The opto-electronic transfer function, by its ITU-T H.273 code point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TransferFunction {
    /// The BT.709 curve, which BT.601 and BT.2020 SDR share. Code point 1.
    Bt709,
    /// SMPTE ST 2084, the perceptual quantiser used by HDR10. Code point 16.
    Pq,
}

impl TransferFunction {
    /// The value written into the bitstream (ITU-T H.273 table 3).
    #[must_use]
    pub const fn code_point(self) -> u32 {
        match self {
            Self::Bt709 => 1,
            Self::Pq => 16,
        }
    }
}

/// The matrix that turns red, green and blue into luma and chroma, by its
/// ITU-T H.273 code point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ColourMatrix {
    /// BT.709. Code point 1.
    Bt709,
    /// BT.601 625-line, the standard-definition matrix. Code point 5.
    Bt601,
    /// BT.2020 non-constant luminance. Code point 9.
    Bt2020Ncl,
}

impl ColourMatrix {
    /// The value written into the bitstream (ITU-T H.273 table 4).
    #[must_use]
    pub const fn code_point(self) -> u32 {
        match self {
            Self::Bt709 => 1,
            Self::Bt601 => 5,
            Self::Bt2020Ncl => 9,
        }
    }
}

impl fmt::Display for ColourMatrix {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Bt709 => "BT.709",
            Self::Bt601 => "BT.601",
            Self::Bt2020Ncl => "BT.2020",
        })
    }
}

/// Whether samples use the whole numeric range or the broadcast subset of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColourRange {
    /// 16–235 for luma, 16–240 for chroma at 8 bits. The default for video.
    Limited,
    /// 0–255 at 8 bits.
    Full,
}

impl fmt::Display for ColourRange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Limited => "limited range",
            Self::Full => "full range",
        })
    }
}

/// Where to sit on the quality-for-speed curve every hardware encoder exposes.
///
/// Three points rather than a vendor's seven, because the difference between
/// adjacent points on a vendor scale is not something a user can judge, and
/// because the three have to mean the same thing on four different encoders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum EncodePreset {
    /// The fastest setting, for hardware that is struggling to keep up.
    Speed,
    /// The default: the best quality that still leaves the GPU to the game.
    #[default]
    Balanced,
    /// The best quality the encoder offers, for hardware with headroom.
    Quality,
}

impl fmt::Display for EncodePreset {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Speed => "speed",
            Self::Balanced => "balanced",
            Self::Quality => "quality",
        })
    }
}

/// Everything an encoder needs to be told before it can encode.
///
/// Built with [`new`](Self::new) and refined with the `with_*` methods, so that
/// adding an option later does not break every call site:
///
/// ```
/// use clipped_encoder::{
///     BitRate, Codec, EncoderConfig, FrameRate, KeyframeInterval, RateControl, Resolution,
/// };
///
/// let config = EncoderConfig::new(
///     Codec::Hevc,
///     Resolution::new(2560, 1440),
///     FrameRate::FPS_60,
///     RateControl::constant(BitRate::megabits_per_second(40)),
/// )
/// .with_keyframe_interval(KeyframeInterval::every(
///     KeyframeInterval::DEFAULT,
///     FrameRate::FPS_60,
/// ));
///
/// assert_eq!(config.codec(), Codec::Hevc);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EncoderConfig {
    codec: Codec,
    resolution: Resolution,
    frame_rate: FrameRate,
    rate_control: RateControl,
    keyframe_interval: KeyframeInterval,
    colour: ColourSpace,
    source_format: SurfaceFormat,
    preset: EncodePreset,
}

impl EncoderConfig {
    /// The four things that have no defensible default, and defaults for the
    /// rest: a keyframe every two seconds, BT.709 limited range, BGRA8 frames
    /// and the balanced preset.
    #[must_use]
    pub fn new(
        codec: Codec,
        resolution: Resolution,
        frame_rate: FrameRate,
        rate_control: RateControl,
    ) -> Self {
        Self {
            codec,
            resolution,
            frame_rate,
            rate_control,
            keyframe_interval: KeyframeInterval::every(KeyframeInterval::DEFAULT, frame_rate),
            colour: ColourSpace::default(),
            source_format: SurfaceFormat::Bgra8Unorm,
            preset: EncodePreset::default(),
        }
    }

    /// Sets how often a keyframe appears.
    #[must_use]
    pub const fn with_keyframe_interval(mut self, interval: KeyframeInterval) -> Self {
        self.keyframe_interval = interval;
        self
    }

    /// Sets what the encoded colours mean.
    #[must_use]
    pub const fn with_colour_space(mut self, colour: ColourSpace) -> Self {
        self.colour = colour;
        self
    }

    /// Sets the layout of the frames that will be submitted.
    #[must_use]
    pub const fn with_source_format(mut self, format: SurfaceFormat) -> Self {
        self.source_format = format;
        self
    }

    /// Sets where to sit on the quality-for-speed curve.
    #[must_use]
    pub const fn with_preset(mut self, preset: EncodePreset) -> Self {
        self.preset = preset;
        self
    }

    /// The codec to produce.
    #[must_use]
    pub const fn codec(&self) -> Codec {
        self.codec
    }

    /// The picture size.
    #[must_use]
    pub const fn resolution(&self) -> Resolution {
        self.resolution
    }

    /// The frame rate the stream declares.
    #[must_use]
    pub const fn frame_rate(&self) -> FrameRate {
        self.frame_rate
    }

    /// How bits are spent.
    #[must_use]
    pub const fn rate_control(&self) -> RateControl {
        self.rate_control
    }

    /// How often a keyframe appears.
    #[must_use]
    pub const fn keyframe_interval(&self) -> KeyframeInterval {
        self.keyframe_interval
    }

    /// What the encoded colours mean.
    #[must_use]
    pub const fn colour_space(&self) -> ColourSpace {
        self.colour
    }

    /// The layout of the frames that will be submitted.
    #[must_use]
    pub const fn source_format(&self) -> SurfaceFormat {
        self.source_format
    }

    /// Where the configuration sits on the quality-for-speed curve.
    #[must_use]
    pub const fn preset(&self) -> EncodePreset {
        self.preset
    }
}

impl fmt::Display for EncoderConfig {
    /// One line for a log: `2560x1440 HEVC at 60 fps, 40.0 Mbit/s constant, a
    /// keyframe every 120 frames, BT.709 limited range`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} {} at {}, {}, {}, {}",
            self.resolution,
            self.codec,
            self.frame_rate,
            self.rate_control,
            self.keyframe_interval,
            self.colour
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_broadcast_frame_rate_keeps_its_ratio() {
        // 59.94 is 60000/1001 exactly. Storing it as 60 would drift by a frame
        // every 16.7 seconds, which is what this type exists to prevent.
        let rate = FrameRate::new(60_000, 1001).expect("59.94 is a frame rate");
        assert_eq!(rate.numerator(), 60_000);
        assert_eq!(rate.denominator(), 1001);
        assert!((rate.as_f64() - 59.94).abs() < 0.001);
        assert_eq!(rate.to_string(), "59.94 fps");
    }

    #[test]
    fn zero_is_not_a_frame_rate() {
        assert!(FrameRate::new(0, 1).is_none());
        assert!(FrameRate::new(60, 0).is_none());
    }

    #[test]
    fn a_keyframe_interval_in_seconds_becomes_a_whole_number_of_frames() {
        let interval = KeyframeInterval::every(Duration::from_secs(2), FrameRate::FPS_60);
        assert_eq!(interval.as_frames().map(NonZeroU32::get), Some(120));

        // 59.94 fps for two seconds is 119.88 frames; a keyframe interval has
        // to be whole, and rounding to 120 is closer than truncating to 119.
        let broadcast = FrameRate::new(60_000, 1001).expect("59.94 is a frame rate");
        let interval = KeyframeInterval::every(Duration::from_secs(2), broadcast);
        assert_eq!(interval.as_frames().map(NonZeroU32::get), Some(120));
    }

    #[test]
    fn an_interval_shorter_than_a_frame_is_still_one_frame() {
        // Every frame a keyframe is a legitimate, if expensive, request. Zero
        // frames is not, and would mean "never" to most encoder APIs — the
        // opposite of what was asked for.
        let interval = KeyframeInterval::every(Duration::from_millis(1), FrameRate::FPS_60);
        assert_eq!(interval.as_frames().map(NonZeroU32::get), Some(1));
    }

    #[test]
    fn never_is_not_a_frame_count() {
        assert_eq!(KeyframeInterval::Never.as_frames(), None);
    }

    #[test]
    fn a_quality_level_stays_inside_the_scale() {
        assert_eq!(QualityTarget::level(0), None);
        assert_eq!(QualityTarget::level(52), None);
        assert_eq!(QualityTarget::level(1), Some(QualityTarget::BEST));
        assert_eq!(QualityTarget::level(51), Some(QualityTarget::WORST));
    }

    #[test]
    fn colour_code_points_match_the_standard() {
        // ITU-T H.273. These numbers end up in the bitstream, so a wrong one
        // produces a file that plays with visibly wrong colour and no error
        // anywhere.
        assert_eq!(ColourPrimaries::Bt709.code_point(), 1);
        assert_eq!(ColourPrimaries::Bt2020.code_point(), 9);
        assert_eq!(TransferFunction::Bt709.code_point(), 1);
        assert_eq!(TransferFunction::Pq.code_point(), 16);
        assert_eq!(ColourMatrix::Bt709.code_point(), 1);
        assert_eq!(ColourMatrix::Bt601.code_point(), 5);
        assert_eq!(ColourMatrix::Bt2020Ncl.code_point(), 9);
    }

    #[test]
    fn a_configuration_prints_as_one_line_a_log_can_carry() {
        let config = EncoderConfig::new(
            Codec::Hevc,
            Resolution::new(2560, 1440),
            FrameRate::FPS_60,
            RateControl::constant(BitRate::megabits_per_second(40)),
        );

        assert_eq!(
            config.to_string(),
            "2560x1440 HEVC at 60 fps, 40.0 Mbit/s constant, a keyframe every 120 frames, \
             BT.709 limited range"
        );
    }

    #[test]
    fn the_defaults_are_the_ones_documented() {
        let config = EncoderConfig::new(
            Codec::H264,
            Resolution::HD_1080P,
            FrameRate::FPS_30,
            RateControl::quality(QualityTarget::DEFAULT),
        );

        assert_eq!(config.colour_space(), ColourSpace::BT709_LIMITED);
        assert_eq!(config.source_format(), SurfaceFormat::Bgra8Unorm);
        assert_eq!(config.preset(), EncodePreset::Balanced);
        assert_eq!(
            config.keyframe_interval().as_frames().map(NonZeroU32::get),
            Some(60),
            "two seconds at 30 fps"
        );
    }
}
