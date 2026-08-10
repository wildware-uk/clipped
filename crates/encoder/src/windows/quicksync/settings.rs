//! Turning an [`EncoderConfig`] into the structures oneVPL is initialised with.
//!
//! Kept apart from the session because it is the half that can be tested
//! without hardware: these are pure functions from Clipped's vocabulary to
//! Intel's, and the tests at the bottom check them on any Windows machine, GPU
//! or not. What is left in `mod.rs` is the sequence of calls, which needs a
//! real encoder — and, on the machine this was written on, has never had one
//! (issue #17).
//!
//! # Two things oneVPL does differently from NVENC
//!
//! - **Bit rates are 16-bit.** `TargetKbps`, `MaxKbps` and `BufferSizeInKB` are
//!   `mfxU16`, and anything above 65535 has to be divided down by
//!   `BRCParamMultiplier`. A recorder at 100 Mbit/s is over that line, so
//!   [`BitRateParams`] does the arithmetic and a test checks it: getting it
//!   wrong silently encodes at a fraction of the requested rate.
//! - **The picture size is aligned.** oneVPL wants `Width` and `Height`
//!   rounded up to a multiple of 16 and the real size in the crop rectangle.
//!   The crop is what ends up in the stream; the alignment is what the encoder
//!   allocates.

use core::time::Duration;

use crate::codec::Codec;
use crate::config::{
    ColourRange, ColourSpace, EncodePreset, EncoderConfig, KeyframeInterval, RateControl,
    SurfaceFormat,
};
use crate::packet::PictureKind;

use super::sys;

/// What this backend accepts, for the error message when it is offered
/// something else.
pub(super) const SUPPORTED_FORMATS: &[SurfaceFormat] = &[SurfaceFormat::Bgra8Unorm];

/// The alignment oneVPL requires of an encoder's surface dimensions.
///
/// 16 for progressive content, which is all a screen recorder produces. The
/// real picture size travels in the crop rectangle, so a 1080-line recording is
/// allocated 1088 lines and encodes 1080 of them.
const ALIGNMENT: u32 = 16;

/// The oneVPL FourCC for a captured surface layout, or [`None`] if this backend
/// cannot take it.
///
/// `MFX_FOURCC_RGB4` is Intel's name for a packed 32-bit surface whose bytes
/// are blue, green, red, alpha — which is exactly `DXGI_FORMAT_B8G8R8A8_UNORM`,
/// the format both Windows capture APIs produce. Handing it over directly is
/// what keeps the frame on the GPU: the encoder converts to 4:2:0 as part of
/// encoding, so nothing here has to run a shader or a copy first.
///
/// Whether a *particular* Intel generation's encoder accepts RGB4 input is not
/// something this table can promise, which is why the session asks
/// `MFXVideoENCODE_Query` before it opens (see `mod.rs`).
pub(super) const fn fourcc(format: SurfaceFormat) -> Option<sys::mfxU32> {
    match format {
        SurfaceFormat::Bgra8Unorm => Some(sys::MFX_FOURCC_RGB4 as sys::mfxU32),
        // 10-bit HDR needs a 10-bit profile, a different colour description and
        // a way to test it, none of which exist yet
        // (https://github.com/wildware-uk/clipped/issues/99).
        SurfaceFormat::Rgb10A2Unorm => None,
    }
}

/// The identifier oneVPL knows this codec by.
pub(super) const fn codec_id(codec: Codec) -> sys::mfxU32 {
    (match codec {
        Codec::H264 => sys::MFX_CODEC_AVC,
        Codec::Hevc => sys::MFX_CODEC_HEVC,
        Codec::Av1 => sys::MFX_CODEC_AV1,
    }) as sys::mfxU32
}

/// The profile to encode in.
///
/// High for H.264 and Main for HEVC and AV1: the widest decoder base for 8-bit
/// 4:2:0, which is all this build produces.
pub(super) const fn profile(codec: Codec) -> sys::mfxU16 {
    (match codec {
        Codec::H264 => sys::MFX_PROFILE_AVC_HIGH,
        Codec::Hevc => sys::MFX_PROFILE_HEVC_MAIN,
        Codec::Av1 => sys::MFX_PROFILE_AV1_MAIN,
    }) as sys::mfxU16
}

/// Where on oneVPL's seven-point quality-for-speed scale a preset sits.
pub(super) const fn target_usage(preset: EncodePreset) -> sys::mfxU16 {
    (match preset {
        EncodePreset::Speed => sys::MFX_TARGETUSAGE_BEST_SPEED,
        EncodePreset::Balanced => sys::MFX_TARGETUSAGE_BALANCED,
        EncodePreset::Quality => sys::MFX_TARGETUSAGE_BEST_QUALITY,
    }) as sys::mfxU16
}

/// Whether to ask for the fixed-function encoder rather than the one that also
/// uses the execution units.
///
/// On for AV1, which Intel only implements on the fixed-function path, and
/// unspecified for the rest so the runtime picks what the silicon prefers. The
/// fixed-function path is also the one that leaves the shader cores to the game
/// (AGENTS.md section 18), so a future measurement may well turn this on
/// everywhere — that is a change that wants Intel hardware to justify it.
pub(super) const fn low_power(codec: Codec) -> sys::mfxU16 {
    (match codec {
        Codec::Av1 => sys::MFX_CODINGOPTION_ON,
        Codec::H264 | Codec::Hevc => sys::MFX_CODINGOPTION_UNKNOWN,
    }) as sys::mfxU16
}

/// Rounds a dimension up to what oneVPL wants to allocate.
const fn aligned(dimension: u32) -> u32 {
    dimension.div_ceil(ALIGNMENT) * ALIGNMENT
}

/// Describes the surfaces going in and the picture coming out.
pub(super) fn frame_info(config: &EncoderConfig) -> sys::mfxFrameInfo {
    let resolution = config.resolution();

    // SAFETY: `mfxFrameInfo` is plain data — integers, and a union of two
    // structures of integers — for which all-zeroes is a valid value, and
    // oneVPL requires every field it is not told about to be zero.
    let mut info: sys::mfxFrameInfo = unsafe { core::mem::zeroed() };

    info.FourCC = fourcc(config.source_format()).unwrap_or(0);
    info.BitDepthLuma = 8;
    info.BitDepthChroma = 8;
    info.Shift = 0;
    // The coded picture is 4:2:0 whatever the surface holds; the encoder
    // converts as part of encoding.
    info.ChromaFormat = sys::MFX_CHROMAFORMAT_YUV420 as sys::mfxU16;
    info.PicStruct = sys::MFX_PICSTRUCT_PROGRESSIVE as sys::mfxU16;
    info.FrameRateExtN = config.frame_rate().numerator();
    info.FrameRateExtD = config.frame_rate().denominator();
    // Square pixels, which is what a captured desktop or game window always
    // has.
    info.AspectRatioW = 1;
    info.AspectRatioH = 1;

    // SAFETY: the union's other member is the one holding `BufferSize`, which
    // only applies to the linear buffers a JPEG decoder produces. The whole
    // structure was zeroed above, both members are plain integers for which
    // all-zeroes is valid, and nothing in this backend ever reads the other
    // member.
    let size = unsafe { &mut info.__bindgen_anon_1.__bindgen_anon_1 };
    size.Width = aligned(resolution.width) as sys::mfxU16;
    size.Height = aligned(resolution.height) as sys::mfxU16;
    size.CropX = 0;
    size.CropY = 0;
    size.CropW = resolution.width as sys::mfxU16;
    size.CropH = resolution.height as sys::mfxU16;

    info
}

/// The bit-rate fields, already divided down by the multiplier oneVPL needs for
/// anything above 65535.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct BitRateParams {
    /// What every other field here is multiplied by. 1 for ordinary rates.
    pub(super) multiplier: sys::mfxU16,
    /// The rate control mode, one of `MFX_RATECONTROL_*`.
    pub(super) method: sys::mfxU16,
    /// `TargetKbps`, or the ICQ quality level when `method` is
    /// `MFX_RATECONTROL_ICQ` — the two share a union.
    pub(super) target_or_quality: sys::mfxU16,
    /// `MaxKbps`. Zero when the mode has no ceiling.
    pub(super) max_kbps: sys::mfxU16,
    /// `BufferSizeInKB`. Zero leaves the choice to the runtime.
    pub(super) buffer_kb: sys::mfxU16,
    /// `InitialDelayInKB`. Zero leaves the choice to the runtime.
    pub(super) initial_delay_kb: sys::mfxU16,
}

impl BitRateParams {
    /// Translates the rate control policy.
    ///
    /// Constant and variable bit rate map straight onto oneVPL's CBR and VBR.
    /// A quality target maps onto ICQ, whose scale — 1 to 51, lower is better —
    /// is the same scale [`QualityTarget`](crate::QualityTarget) already
    /// speaks, so the number travels unchanged.
    pub(super) fn from(rate_control: RateControl) -> Self {
        match rate_control {
            RateControl::Bitrate { average, peak } => {
                let target = kilobits(average.as_bits_per_second());
                let max = peak.map_or(target, |peak| kilobits(peak.as_bits_per_second()));
                // One second of video in the buffer, which is the same choice
                // the NVENC backend makes and for the same reason: the
                // single-frame buffer a low-latency stream wants holds the rate
                // very precisely and costs visible quality whenever the picture
                // changes.
                let buffer = target / 8;
                let multiplier = multiplier_for(&[target, max, buffer]);

                Self {
                    multiplier,
                    method: if peak.is_some() {
                        sys::MFX_RATECONTROL_VBR as sys::mfxU16
                    } else {
                        sys::MFX_RATECONTROL_CBR as sys::mfxU16
                    },
                    target_or_quality: scaled(target, multiplier),
                    max_kbps: scaled(max, multiplier),
                    buffer_kb: scaled(buffer, multiplier),
                    // Half the buffer before playback may start, which is
                    // oneVPL's own convention and the middle of the range a
                    // decoder can accept.
                    initial_delay_kb: scaled(buffer / 2, multiplier),
                }
            }
            RateControl::Quality { target, .. } => Self {
                multiplier: 1,
                method: sys::MFX_RATECONTROL_ICQ as sys::mfxU16,
                target_or_quality: sys::mfxU16::from(target.as_level()),
                // ICQ spends what the quality needs and has no ceiling to give:
                // `MaxKbps` shares its union with the B-frame quantiser and is
                // unused in this mode. A configured ceiling is reported at
                // `open` rather than quietly dropped — see
                // `QuickSyncEncoder::open`.
                max_kbps: 0,
                buffer_kb: 0,
                initial_delay_kb: 0,
            },
        }
    }
}

/// Bits per second as the kilobits per second oneVPL counts in.
fn kilobits(bits_per_second: u32) -> u32 {
    bits_per_second.div_ceil(1000)
}

/// The smallest multiplier that brings every value inside 16 bits.
fn multiplier_for(values: &[u32]) -> sys::mfxU16 {
    let largest = values.iter().copied().max().unwrap_or(0);
    let multiplier = largest.div_ceil(u32::from(sys::mfxU16::MAX)).max(1);
    // A rate that needs a multiplier above 65535 is 4 petabits per second, so
    // the clamp is unreachable arithmetic hygiene rather than a real case.
    sys::mfxU16::try_from(multiplier).unwrap_or(sys::mfxU16::MAX)
}

/// A value divided by the multiplier, rounded up so that the product is never
/// less than what was asked for.
fn scaled(value: u32, multiplier: sys::mfxU16) -> sys::mfxU16 {
    let scaled = value.div_ceil(u32::from(multiplier.max(1)));
    sys::mfxU16::try_from(scaled).unwrap_or(sys::mfxU16::MAX)
}

/// How many frames apart the keyframes should be.
///
/// [`KeyframeInterval::Never`] becomes the largest group of pictures the field
/// can hold rather than zero, which oneVPL reads as "you decide".
pub(super) fn gop_size(interval: KeyframeInterval) -> sys::mfxU16 {
    interval.as_frames().map_or(sys::mfxU16::MAX, |frames| {
        sys::mfxU16::try_from(frames.get()).unwrap_or(sys::mfxU16::MAX)
    })
}

/// Fills in everything about the stream to be produced except the extension
/// buffers, which the session attaches because they have to outlive the call.
///
/// The returned structure describes video memory in and a bitstream out, one
/// frame at a time: `AsyncDepth` is 1 because the input surface belongs to the
/// capture backend and must be released before `submit` returns, so there is
/// nothing to be gained by letting the encoder run ahead
/// (`docs/encoder-pipeline.md`).
pub(super) fn video_params(config: &EncoderConfig) -> sys::mfxVideoParam {
    // SAFETY: `mfxVideoParam` is plain data — integers, a pointer, and a union
    // of two structures of integers — for which all-zeroes is a valid value,
    // and oneVPL requires every field it is not told about to be zero.
    let mut params: sys::mfxVideoParam = unsafe { core::mem::zeroed() };

    params.AsyncDepth = 1;
    params.IOPattern = sys::MFX_IOPATTERN_IN_VIDEO_MEMORY as sys::mfxU16;

    let info = frame_info(config);
    let rate = BitRateParams::from(config.rate_control());

    // SAFETY: `mfxVideoParam` holds a union of `mfxInfoMFX` and `mfxInfoVPP`,
    // and this backend only ever encodes: every write and every read of that
    // union in this crate goes through the `mfx` member. The structure was
    // zeroed above, and both members are plain data for which all-zeroes is
    // valid, so nothing observes a member that was never written. The same
    // argument covers the nested unions below, each of which is selected by the
    // rate control mode written beside it.
    unsafe {
        let mfx = &mut params.__bindgen_anon_1.mfx;
        mfx.CodecId = codec_id(config.codec());
        mfx.CodecProfile = profile(config.codec());
        mfx.CodecLevel = sys::MFX_LEVEL_UNKNOWN as sys::mfxU16;
        mfx.LowPower = low_power(config.codec());
        mfx.BRCParamMultiplier = rate.multiplier;
        mfx.FrameInfo = info;

        let encoding = &mut mfx.__bindgen_anon_1.__bindgen_anon_1;
        encoding.TargetUsage = target_usage(config.preset());
        encoding.GopPicSize = gop_size(config.keyframe_interval());
        // No B-frames. A recorder gains a few per cent of compression from them
        // and pays with reordered output — a decode timestamp that differs from
        // the presentation timestamp, and a muxer that has to reconstruct it
        // (issue #21) — and, worse here, with an encoder that holds input
        // surfaces it does not own.
        encoding.GopRefDist = 1;
        // Every I-frame is an IDR, so every keyframe is a point a clip can
        // start from (SPEC.md section 7).
        encoding.IdrInterval = 0;
        encoding.RateControlMethod = rate.method;
        encoding.__bindgen_anon_2.TargetKbps = rate.target_or_quality;
        encoding.__bindgen_anon_3.MaxKbps = rate.max_kbps;
        encoding.BufferSizeInKB = rate.buffer_kb;
        encoding.__bindgen_anon_1.InitialDelayInKB = rate.initial_delay_kb;
    }

    params
}

/// The header every extension buffer starts with.
const fn ext_header(id: sys::mfxU32, size: usize) -> sys::mfxExtBuffer {
    sys::mfxExtBuffer {
        BufferId: id,
        BufferSz: size as sys::mfxU32,
    }
}

/// What the colours in the stream mean.
///
/// A stream that does not say is guessed at by the player, and different
/// players guess differently: the same recording comes out washed out in one
/// and oversaturated in another. Saying so costs a few bits once per keyframe.
pub(super) fn video_signal_info(colour: ColourSpace) -> sys::mfxExtVideoSignalInfo {
    sys::mfxExtVideoSignalInfo {
        Header: ext_header(
            sys::MFX_EXTBUFF_VIDEO_SIGNAL_INFO as sys::mfxU32,
            core::mem::size_of::<sys::mfxExtVideoSignalInfo>(),
        ),
        VideoFormat: 5, // Unspecified, as ITU-T H.273 numbers it.
        VideoFullRange: match colour.range() {
            ColourRange::Full => 1,
            ColourRange::Limited => 0,
        },
        ColourDescriptionPresent: 1,
        ColourPrimaries: colour.primaries().code_point() as sys::mfxU16,
        TransferCharacteristics: colour.transfer().code_point() as sys::mfxU16,
        MatrixCoefficients: colour.matrix().code_point() as sys::mfxU16,
    }
}

/// The second block of coding options, which is where repeating the parameter
/// sets lives.
///
/// A clip cut from the middle of a recording begins at a keyframe, and a
/// decoder handed a keyframe with no parameter sets in front of it cannot start
/// (SPEC.md section 7). oneVPL repeats them by default; saying so explicitly
/// means the recording does not depend on a default staying what it is.
pub(super) fn coding_option2() -> sys::mfxExtCodingOption2 {
    // SAFETY: `mfxExtCodingOption2` is plain data — integers only — for which
    // all-zeroes is a valid value, and zero is `MFX_CODINGOPTION_UNKNOWN` for
    // every option in it, which is oneVPL's "you decide".
    let mut options: sys::mfxExtCodingOption2 = unsafe { core::mem::zeroed() };
    options.Header = ext_header(
        sys::MFX_EXTBUFF_CODING_OPTION2 as sys::mfxU32,
        core::mem::size_of::<sys::mfxExtCodingOption2>(),
    );
    options.RepeatPPS = sys::MFX_CODINGOPTION_ON as sys::mfxU16;
    options
}

/// AV1's stream-level options.
///
/// The one that matters is `WriteIVFHeaders`: oneVPL can wrap AV1 output in IVF
/// container headers, and a muxer handed those would write them into the file
/// as though they were part of the bitstream. This interface produces
/// elementary streams, so they are switched off.
pub(super) fn av1_bitstream_param() -> sys::mfxExtAV1BitstreamParam {
    // SAFETY: plain data — integers only — for which all-zeroes is valid.
    let mut params: sys::mfxExtAV1BitstreamParam = unsafe { core::mem::zeroed() };
    params.Header = ext_header(
        sys::MFX_EXTBUFF_AV1_BITSTREAM_PARAM as sys::mfxU32,
        core::mem::size_of::<sys::mfxExtAV1BitstreamParam>(),
    );
    params.WriteIVFHeaders = sys::MFX_CODINGOPTION_OFF as sys::mfxU16;
    params
}

/// An empty buffer for oneVPL to write the sequence and picture parameter sets
/// into.
///
/// `SPSBuffer` and `PPSBuffer` are filled in by the session, which owns the
/// storage they point at.
pub(super) fn parameter_set_request() -> sys::mfxExtCodingOptionSPSPPS {
    // SAFETY: plain data — two pointers and four integers — for which
    // all-zeroes is valid. The pointers are filled in by the caller before the
    // structure is handed to oneVPL.
    let mut request: sys::mfxExtCodingOptionSPSPPS = unsafe { core::mem::zeroed() };
    request.Header = ext_header(
        sys::MFX_EXTBUFF_CODING_OPTION_SPSPPS as sys::mfxU32,
        core::mem::size_of::<sys::mfxExtCodingOptionSPSPPS>(),
    );
    request
}

/// The presentation time in the 90 kHz units oneVPL carries timestamps in.
///
/// Saturating rather than wrapping: a `Duration` beyond six million years
/// cannot happen in a recording, and if it somehow did, clamping keeps
/// timestamps monotonic where wrapping would send them back to zero.
pub(super) fn timestamp_90khz(time: Duration) -> sys::mfxU64 {
    const TICKS_PER_SECOND: u128 = 90_000;
    const NANOS_PER_SECOND: u128 = 1_000_000_000;

    let ticks = time.as_nanos() * TICKS_PER_SECOND / NANOS_PER_SECOND;
    sys::mfxU64::try_from(ticks).unwrap_or(sys::mfxU64::MAX)
}

/// The other direction: a timestamp oneVPL reported, as a duration.
///
/// [`None`] for `MFX_TIMESTAMP_UNKNOWN`, which is how the runtime says it has
/// no timestamp to give — a packet flushed out of an encoder that was never
/// told one, for instance.
pub(super) fn duration_from_90khz(ticks: sys::mfxU64) -> Option<Duration> {
    const TICKS_PER_SECOND: u128 = 90_000;
    const NANOS_PER_SECOND: u128 = 1_000_000_000;

    if ticks == sys::MFX_TIMESTAMP_UNKNOWN as sys::mfxU64 {
        return None;
    }

    let nanos = u128::from(ticks) * NANOS_PER_SECOND / TICKS_PER_SECOND;
    Some(Duration::from_nanos(
        u64::try_from(nanos).unwrap_or(u64::MAX),
    ))
}

/// Translates the frame type oneVPL reports on a coded picture.
///
/// The flags are a bitmask and an IDR is also an I-frame, so the order of these
/// tests is the meaning: the most specific kind wins.
pub(super) const fn picture_kind(frame_type: sys::mfxU16) -> PictureKind {
    const IDR: sys::mfxU16 = sys::MFX_FRAMETYPE_IDR as sys::mfxU16;
    const INTRA: sys::mfxU16 = sys::MFX_FRAMETYPE_I as sys::mfxU16;
    const BIDIRECTIONAL: sys::mfxU16 = sys::MFX_FRAMETYPE_B as sys::mfxU16;
    const PREDICTED: sys::mfxU16 = sys::MFX_FRAMETYPE_P as sys::mfxU16;

    if frame_type & IDR != 0 {
        PictureKind::Keyframe
    } else if frame_type & INTRA != 0 {
        PictureKind::Intra
    } else if frame_type & BIDIRECTIONAL != 0 {
        PictureKind::Bidirectional
    } else if frame_type & PREDICTED != 0 {
        PictureKind::Predicted
    } else {
        PictureKind::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::Resolution;
    use crate::config::{BitRate, FrameRate, QualityTarget};
    use core::num::NonZeroU32;

    fn settings(codec: Codec, rate_control: RateControl) -> EncoderConfig {
        EncoderConfig::new(
            codec,
            Resolution::new(2560, 1440),
            FrameRate::FPS_60,
            rate_control,
        )
    }

    /// The encoding half of the union `video_params` writes.
    ///
    /// # Safety
    ///
    /// Reading a union member is only sound if it is the member that was
    /// written, and `video_params` writes exactly this one — see the argument
    /// beside it there.
    unsafe fn encoding(params: &sys::mfxVideoParam) -> sys::mfxInfoMFX__bindgen_ty_1__bindgen_ty_1 {
        // SAFETY: as the doc comment argues.
        unsafe {
            params
                .__bindgen_anon_1
                .mfx
                .__bindgen_anon_1
                .__bindgen_anon_1
        }
    }

    #[test]
    fn the_surface_size_is_aligned_and_the_real_size_is_cropped() {
        // 1440 is a multiple of 16 and 1080 is not, so the second is the case
        // that matters: an encoder told to allocate 1080 lines rounds it up
        // itself and then encodes a picture whose bottom eight lines are
        // whatever was in the surface.
        let info = frame_info(&EncoderConfig::new(
            Codec::H264,
            Resolution::new(1920, 1080),
            FrameRate::FPS_60,
            RateControl::constant(BitRate::megabits_per_second(20)),
        ));

        // SAFETY: `frame_info` writes this member of the union and no other.
        let size = unsafe { info.__bindgen_anon_1.__bindgen_anon_1 };
        assert_eq!(size.Width, 1920);
        assert_eq!(size.Height, 1088, "1080 is not a multiple of 16");
        assert_eq!(size.CropW, 1920);
        assert_eq!(size.CropH, 1080, "the stream must still say 1080 lines");
        assert_eq!(info.FourCC, sys::MFX_FOURCC_RGB4 as sys::mfxU32);
        assert_eq!(info.FrameRateExtN, 60);
        assert_eq!(info.FrameRateExtD, 1);
    }

    #[test]
    fn an_ordinary_bit_rate_needs_no_multiplier() {
        let rate = BitRateParams::from(RateControl::constant(BitRate::megabits_per_second(40)));

        assert_eq!(rate.multiplier, 1);
        assert_eq!(rate.method, sys::MFX_RATECONTROL_CBR as sys::mfxU16);
        assert_eq!(rate.target_or_quality, 40_000);
        assert_eq!(
            rate.max_kbps, 40_000,
            "constant means target equals maximum"
        );
        assert_eq!(
            rate.buffer_kb, 5_000,
            "one second of 40 Mbit/s in kilobytes"
        );
    }

    #[test]
    fn a_bit_rate_above_sixteen_bits_is_divided_down() {
        // The failure this guards is silent: 100_000 kbit/s does not fit in an
        // mfxU16, and a backend that truncated it would encode at 34_464
        // kbit/s and report success.
        let rate = BitRateParams::from(RateControl::constant(BitRate::megabits_per_second(100)));

        assert!(rate.multiplier > 1, "100 Mbit/s does not fit in 16 bits");
        let effective = u32::from(rate.target_or_quality) * u32::from(rate.multiplier);
        assert!(
            (100_000..=100_000 + u32::from(rate.multiplier)).contains(&effective),
            "asked for 100000 kbit/s and would encode at {effective}"
        );
        assert!(
            u32::from(rate.buffer_kb) * u32::from(rate.multiplier) >= 12_500,
            "the buffer shrank when it was scaled"
        );
    }

    #[test]
    fn a_peak_rate_asks_for_variable_bit_rate() {
        let rate = BitRateParams::from(RateControl::Bitrate {
            average: BitRate::megabits_per_second(30),
            peak: Some(BitRate::megabits_per_second(60)),
        });

        assert_eq!(rate.method, sys::MFX_RATECONTROL_VBR as sys::mfxU16);
        assert_eq!(rate.target_or_quality, 30_000);
        assert_eq!(rate.max_kbps, 60_000);
    }

    #[test]
    fn a_quality_target_travels_on_the_scale_it_was_written_on() {
        // oneVPL's ICQ scale is 1 to 51 with lower better, which is the scale
        // `QualityTarget` already uses, so the number must not be converted.
        let rate = BitRateParams::from(RateControl::quality(
            QualityTarget::level(20).expect("20 is on the scale"),
        ));

        assert_eq!(rate.method, sys::MFX_RATECONTROL_ICQ as sys::mfxU16);
        assert_eq!(rate.target_or_quality, 20);
        assert_eq!(rate.max_kbps, 0, "ICQ has no ceiling to give");
    }

    #[test]
    fn the_keyframe_interval_reaches_the_group_of_pictures() {
        let params = video_params(
            &settings(
                Codec::Hevc,
                RateControl::constant(BitRate::megabits_per_second(20)),
            )
            .with_keyframe_interval(KeyframeInterval::Frames(
                NonZeroU32::new(120).expect("120 is not zero"),
            )),
        );

        // SAFETY: the encoding member is the one `video_params` wrote.
        let encoding = unsafe { encoding(&params) };
        assert_eq!(encoding.GopPicSize, 120);
        assert_eq!(
            encoding.IdrInterval, 0,
            "an I-frame that is not an IDR is not a point a clip can start from"
        );
    }

    #[test]
    fn never_asking_for_a_keyframe_is_the_longest_group_of_pictures() {
        // Not zero: oneVPL reads zero as "you decide", which is the opposite of
        // what was asked for.
        assert_eq!(gop_size(KeyframeInterval::Never), sys::mfxU16::MAX);
    }

    #[test]
    fn no_b_frames_are_asked_for() {
        // The promise the packet timestamps depend on, and the one that keeps a
        // borrowed input surface from being held past `submit`.
        let params = video_params(&settings(
            Codec::H264,
            RateControl::constant(BitRate::megabits_per_second(20)),
        ));

        // SAFETY: the encoding member is the one `video_params` wrote.
        assert_eq!(unsafe { encoding(&params) }.GopRefDist, 1);
    }

    #[test]
    fn one_frame_is_in_flight_at_a_time() {
        let params = video_params(&settings(
            Codec::Av1,
            RateControl::constant(BitRate::megabits_per_second(20)),
        ));

        assert_eq!(
            params.AsyncDepth, 1,
            "a deeper pipeline would hold input surfaces the caller owns"
        );
        assert_eq!(
            params.IOPattern,
            sys::MFX_IOPATTERN_IN_VIDEO_MEMORY as sys::mfxU16
        );
    }

    #[test]
    fn the_colour_description_is_written_into_the_stream() {
        let signal = video_signal_info(ColourSpace::BT709_LIMITED);

        assert_eq!(signal.ColourDescriptionPresent, 1);
        assert_eq!(signal.ColourPrimaries, 1, "BT.709");
        assert_eq!(signal.TransferCharacteristics, 1, "BT.709");
        assert_eq!(signal.MatrixCoefficients, 1, "BT.709");
        assert_eq!(signal.VideoFullRange, 0, "limited range");
        assert_eq!(
            signal.Header.BufferId,
            sys::MFX_EXTBUFF_VIDEO_SIGNAL_INFO as sys::mfxU32
        );
        assert_eq!(
            signal.Header.BufferSz as usize,
            core::mem::size_of::<sys::mfxExtVideoSignalInfo>(),
            "oneVPL rejects an extension buffer whose size does not match its identifier"
        );
    }

    #[test]
    fn full_range_reaches_the_stream_as_well() {
        assert_eq!(video_signal_info(ColourSpace::BT709_FULL).VideoFullRange, 1);
    }

    #[test]
    fn av1_output_is_an_elementary_stream_rather_than_an_ivf_file() {
        // The default is not what this interface wants: IVF headers written
        // into the packets would be muxed into the file as though they were
        // part of the bitstream.
        assert_eq!(
            av1_bitstream_param().WriteIVFHeaders,
            sys::MFX_CODINGOPTION_OFF as sys::mfxU16
        );
    }

    #[test]
    fn every_extension_buffer_declares_its_own_size() {
        // A `BufferSz` that does not match the structure is how oneVPL detects
        // a caller built against a different header, and getting it wrong
        // produces MFX_ERR_INVALID_VIDEO_PARAM with nothing to say which buffer
        // was wrong.
        let options = coding_option2();
        assert_eq!(
            options.Header.BufferSz as usize,
            core::mem::size_of::<sys::mfxExtCodingOption2>()
        );
        assert_eq!(options.RepeatPPS, sys::MFX_CODINGOPTION_ON as sys::mfxU16);

        let av1 = av1_bitstream_param();
        assert_eq!(
            av1.Header.BufferSz as usize,
            core::mem::size_of::<sys::mfxExtAV1BitstreamParam>()
        );

        let request = parameter_set_request();
        assert_eq!(
            request.Header.BufferSz as usize,
            core::mem::size_of::<sys::mfxExtCodingOptionSPSPPS>()
        );
        assert!(request.SPSBuffer.is_null(), "the session owns the storage");
    }

    #[test]
    fn every_codec_and_preset_has_a_distinct_identifier() {
        // A copy-and-paste error between two of these would encode the wrong
        // codec at the wrong speed and never fail a call.
        let codecs = [
            codec_id(Codec::H264),
            codec_id(Codec::Hevc),
            codec_id(Codec::Av1),
        ];
        for (index, first) in codecs.iter().enumerate() {
            assert!(!codecs[index + 1..].contains(first), "{first} is repeated");
        }

        assert_eq!(
            target_usage(EncodePreset::Speed),
            sys::MFX_TARGETUSAGE_BEST_SPEED as sys::mfxU16
        );
        assert_eq!(
            target_usage(EncodePreset::Balanced),
            sys::MFX_TARGETUSAGE_BALANCED as sys::mfxU16
        );
        assert_eq!(
            target_usage(EncodePreset::Quality),
            sys::MFX_TARGETUSAGE_BEST_QUALITY as sys::mfxU16
        );
    }

    #[test]
    fn av1_asks_for_the_fixed_function_encoder() {
        // Intel implements AV1 encoding only on the low-power path, so an AV1
        // session that does not ask for it is refused by the runtime.
        assert_eq!(
            low_power(Codec::Av1),
            sys::MFX_CODINGOPTION_ON as sys::mfxU16
        );
        assert_eq!(
            low_power(Codec::H264),
            sys::MFX_CODINGOPTION_UNKNOWN as sys::mfxU16
        );
    }

    #[test]
    fn only_the_formats_this_backend_can_bind_have_a_fourcc() {
        assert_eq!(
            fourcc(SurfaceFormat::Bgra8Unorm),
            Some(sys::MFX_FOURCC_RGB4 as sys::mfxU32)
        );
        assert_eq!(fourcc(SurfaceFormat::Rgb10A2Unorm), None);
        assert_eq!(SUPPORTED_FORMATS, &[SurfaceFormat::Bgra8Unorm]);
    }

    #[test]
    fn timestamps_are_converted_to_the_units_onevpl_counts_in() {
        assert_eq!(timestamp_90khz(Duration::ZERO), 0);
        assert_eq!(timestamp_90khz(Duration::from_secs(1)), 90_000);
        // One frame at 60 per second is exactly 1500 ticks, which is why 90 kHz
        // is the unit every video container uses — but a sixtieth of a second
        // is not a whole number of nanoseconds, so a `Duration` that has been
        // truncated to 16_666_666 ns lands a tick low. That is 11 microseconds,
        // and it never reaches a packet: a submitted frame carries the duration
        // it arrived with rather than the one that came back (see
        // `Session::collect`).
        assert_eq!(timestamp_90khz(Duration::from_nanos(16_666_667)), 1_500);
        assert_eq!(timestamp_90khz(Duration::from_nanos(16_666_666)), 1_499);
        assert_eq!(timestamp_90khz(Duration::MAX), sys::mfxU64::MAX);
    }

    #[test]
    fn a_timestamp_survives_the_round_trip() {
        // The conversion back is only used for pictures flushed at the end of a
        // stream. It is exact for anything a whole number of milliseconds long,
        // which is 90 ticks; the 11.1 microseconds between ticks is why every
        // *submitted* frame carries the duration it arrived with rather than the
        // one that came back (see `Session::collect`).
        for millis in 0..1_000u64 {
            let time = Duration::from_millis(millis);
            assert_eq!(
                duration_from_90khz(timestamp_90khz(time)),
                Some(time),
                "{millis} ms came back as a different time"
            );
        }
    }

    #[test]
    fn a_picture_with_no_timestamp_says_so() {
        assert_eq!(
            duration_from_90khz(sys::MFX_TIMESTAMP_UNKNOWN as sys::mfxU64),
            None
        );
    }

    #[test]
    fn a_keyframe_is_recognised_as_one() {
        // MFX_FRAMETYPE_IDR arrives with MFX_FRAMETYPE_I set alongside it, so a
        // test that only checks the flags in isolation would pass while every
        // keyframe was reported as an ordinary intra frame — and the replay
        // buffer would have no cut points.
        let idr = (sys::MFX_FRAMETYPE_IDR | sys::MFX_FRAMETYPE_I) as sys::mfxU16;
        assert_eq!(picture_kind(idr), PictureKind::Keyframe);
        assert_eq!(
            picture_kind(sys::MFX_FRAMETYPE_I as sys::mfxU16),
            PictureKind::Intra
        );
        assert_eq!(
            picture_kind(sys::MFX_FRAMETYPE_P as sys::mfxU16),
            PictureKind::Predicted
        );
        assert_eq!(
            picture_kind(sys::MFX_FRAMETYPE_B as sys::mfxU16),
            PictureKind::Bidirectional
        );
        assert_eq!(picture_kind(0), PictureKind::Unknown);
    }
}
