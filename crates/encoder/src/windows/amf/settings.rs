//! Turning an [`EncoderConfig`] into the properties an AMF encoder is
//! configured with.
//!
//! Kept apart from the session because it is the half that can be tested
//! without hardware: these are pure functions from Clipped's vocabulary to
//! AMD's, and the tests at the bottom check them on any machine, AMD GPU or
//! not. What is left in `mod.rs` is the sequence of calls, which needs a real
//! encoder.
//!
//! # Why a list rather than a structure
//!
//! AMF has no configuration structure. An encoder is configured by setting
//! named properties on the component, one call each, before `Init`. So the
//! mapping produces a list of name-and-value pairs and the session sets them in
//! order — which also means the whole configuration of an encoder is one value
//! a test can assert on, rather than a sequence of calls a test would have to
//! intercept.
//!
//! Order matters in exactly one place: `Usage` comes first, because AMF
//! documents it as configuring a whole parameter set, and everything after it
//! overrides part of what it chose.
//!
//! # What is deliberately left to AMD
//!
//! Everything not listed here. The usage preset is AMD's own tuning for a
//! transcoding workload, and this module overwrites only what Clipped has an
//! opinion about: rate control, the keyframe interval, parameter set
//! repetition, the colour description, and the absence of B-frames (AGENTS.md
//! section 1).

use crate::codec::Codec;
use crate::config::{
    ColourRange, ColourSpace, EncodePreset, EncoderConfig, KeyframeInterval, RateControl,
    SurfaceFormat,
};
use crate::packet::PictureKind;

use super::api::{self, wide};
use super::sys;

/// One property, as AMF takes it.
pub(super) struct Property {
    /// The name, nul-terminated, as AMF's headers spell it.
    pub(super) name: &'static [u16],
    /// The value.
    pub(super) value: sys::AMFVariantStruct,
}

impl Property {
    /// A property with a whole-number value.
    fn int64(name: &'static [u16], value: i64) -> Self {
        Self {
            name,
            value: api::int64(value),
        }
    }
}

impl core::fmt::Debug for Property {
    /// The name and the tag. The value is a union and printing it would mean
    /// reading a member the tag may not select.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Property")
            .field("name", &api::name_to_string(self.name))
            .field("type", &self.value.type_)
            .finish()
    }
}

/// The component identifier AMF creates this codec's encoder from, or [`None`]
/// where this backend does not implement one.
///
/// AV1 is deliberately absent. AMF has an AV1 encoder component and this
/// backend does not configure it: the only AMD hardware available to develop
/// against decodes AV1 and does not encode it, so an AV1 path here could be
/// written and never run — and a backend nobody has ever seen produce a frame
/// is not support, it is a claim
/// ([#165](https://github.com/wildware-uk/clipped/issues/165)).
///
/// Transcribed from the `AMFVideoEncoderVCE_AVC` and `AMFVideoEncoder_HEVC`
/// macros in AMD's headers. Note that the HEVC *macro* and the string it
/// expands to do not match: the identifier is `AMFVideoEncoderHW_HEVC`.
pub(super) const fn component_id(codec: Codec) -> Option<&'static [u16]> {
    match codec {
        Codec::H264 => Some(wide!("AMFVideoEncoderVCE_AVC")),
        Codec::Hevc => Some(wide!("AMFVideoEncoderHW_HEVC")),
        Codec::Av1 => None,
    }
}

/// The capability properties an encoder's `AMFCaps` answers to, for one codec.
///
/// AMF namespaces its property names by codec — `MaxThroughput` for H.264 and
/// `HevcMaxThroughput` for HEVC — so the names have to be chosen per codec
/// rather than shared. A name AMF does not recognise is answered with
/// `AMF_NOT_FOUND` rather than an error, which would turn a typo here into a
/// measurement quietly never taken; the test at the bottom of this file
/// compares each one against the constant bindgen generated from AMD's headers.
#[derive(Debug, Clone, Copy)]
pub(super) struct CapabilityNames {
    /// How many macroblocks a second the encoder can process.
    pub(super) max_throughput: &'static [u16],
    /// Whether B-frames are available, where the codec's encoder has such a
    /// capability at all.
    pub(super) b_frames: Option<&'static [u16]>,
    /// The highest profile the encoder offers, which is where 10-bit is
    /// answered from (see [`ten_bit_from_profile`]).
    pub(super) max_profile: &'static [u16],
}

/// The capability names for a codec this backend can create a component for.
pub(super) const fn capability_names(codec: Codec) -> Option<CapabilityNames> {
    match codec {
        Codec::H264 => Some(CapabilityNames {
            max_throughput: wide!("MaxThroughput"),
            b_frames: Some(wide!("BFrames")),
            max_profile: wide!("MaxProfile"),
        }),
        // AMF's HEVC encoder has no B-frame capability to ask about: the
        // property does not exist in `VideoEncoderHEVC.h`, because the encoder
        // does not produce them. That is a fact about the API rather than a
        // measurement of this machine, so it is left to the reference table
        // rather than reported as measured.
        Codec::Hevc => Some(CapabilityNames {
            max_throughput: wide!("HevcMaxThroughput"),
            b_frames: None,
            max_profile: wide!("HevcMaxProfile"),
        }),
        // No component identifier, so no component and nothing to ask.
        Codec::Av1 => None,
    }
}

/// Whether the highest profile an encoder offers implies 10-bit encoding.
///
/// [`None`] means the question cannot be answered from a profile number, and
/// the published limit stands:
///
/// - **HEVC** can be answered. `AMF_VIDEO_ENCODER_HEVC_PROFILE_ENUM` is `Main`
///   then `Main10`, in that order, so a maximum at or above `Main10` is an
///   encoder that produces 10-bit and one below it is an encoder that does not.
/// - **H.264** cannot. `AMF_VIDEO_ENCODER_PROFILE_ENUM` has no 10-bit profile
///   in it at all — it ends at Constrained High — so the largest value it can
///   report says nothing about bit depth, and a comparison would be arithmetic
///   on unrelated numbers. AMD's H.264 encoder is 8-bit, which is what the
///   reference table already infers, and inferring it is honest where measuring
///   it is not available.
pub(super) const fn ten_bit_from_profile(codec: Codec, max_profile: i64) -> Option<bool> {
    match codec {
        Codec::Hevc => Some(max_profile >= sys::AMF_VIDEO_ENCODER_HEVC_PROFILE_MAIN_10 as i64),
        Codec::H264 | Codec::Av1 => None,
    }
}

/// The property holding the out-of-band parameter sets a container needs.
pub(super) const fn extra_data(codec: Codec) -> &'static [u16] {
    match codec {
        Codec::Hevc => wide!("HevcExtraData"),
        _ => wide!("ExtraData"),
    }
}

/// The property naming what kind of picture an output buffer holds.
pub(super) const fn output_data_type(codec: Codec) -> &'static [u16] {
    match codec {
        Codec::Hevc => wide!("HevcOutputDataType"),
        _ => wide!("OutputDataType"),
    }
}

/// What kind of picture an `OutputDataType` value describes.
///
/// The enumerations differ between the two codecs — H.264 has a B value and
/// HEVC does not — so the codec has to be part of the question. Only an IDR is
/// a cut point: an intra picture that is not one may still be predicted across,
/// so a clip starting there decodes into rubbish (see [`crate::packet`]).
pub(super) const fn picture_kind(codec: Codec, value: i64) -> PictureKind {
    match codec {
        Codec::Hevc => match value {
            // AMF_VIDEO_ENCODER_HEVC_OUTPUT_DATA_TYPE_ENUM
            0 => PictureKind::Keyframe,
            1 => PictureKind::Intra,
            2 => PictureKind::Predicted,
            _ => PictureKind::Unknown,
        },
        // AMF_VIDEO_ENCODER_OUTPUT_DATA_TYPE_ENUM
        _ => match value {
            0 => PictureKind::Keyframe,
            1 => PictureKind::Intra,
            2 => PictureKind::Predicted,
            3 => PictureKind::Bidirectional,
            _ => PictureKind::Unknown,
        },
    }
}

/// The properties set on a frame that has to be coded as an IDR — one the
/// caller asked to be a keyframe, or the first picture of the stream (see
/// [`super::force_idr`]).
///
/// Two things, not one: the picture type, and — for H.264 — the parameter sets
/// in front of it. A keyframe a decoder cannot start at is not a cut point, and
/// the periodic repetition configured in [`properties`] is aligned to the
/// configured interval rather than to a frame that asked out of turn (SPEC.md
/// section 7). HEVC needs no second property because its header insertion mode
/// is IDR-aligned, which covers a forced IDR too.
pub(super) fn forced_keyframe(codec: Codec) -> Vec<Property> {
    match codec {
        Codec::Hevc => vec![Property::int64(
            wide!("HevcForcePictureType"),
            // AMF_VIDEO_ENCODER_HEVC_PICTURE_TYPE_IDR
            2,
        )],
        _ => vec![
            // AMF_VIDEO_ENCODER_PICTURE_TYPE_IDR
            Property::int64(wide!("ForcePictureType"), 2),
            Property {
                name: wide!("InsertSPS"),
                value: api::boolean(true),
            },
            Property {
                name: wide!("InsertPPS"),
                value: api::boolean(true),
            },
        ],
    }
}

/// The AMF surface format for a captured surface layout, or [`None`] if this
/// backend cannot take it.
///
/// `AMF_SURFACE_BGRA` is AMD's name for a packed 32-bit surface whose bytes are
/// blue, green, red, alpha — which is exactly `DXGI_FORMAT_B8G8R8A8_UNORM`, the
/// format both Windows capture APIs produce. Feeding it directly is what keeps
/// the frame on the GPU: the AMF encoder component converts to 4:2:0 on the GPU
/// as part of encoding, so nothing here has to run a shader or a copy first.
pub(super) const fn surface_format(format: SurfaceFormat) -> Option<sys::AMF_SURFACE_FORMAT> {
    match format {
        SurfaceFormat::Bgra8Unorm => Some(sys::AMF_SURFACE_BGRA),
        // 10-bit HDR needs a 10-bit profile, a different colour description and
        // a way to test it, none of which exist yet
        // (https://github.com/wildware-uk/clipped/issues/99).
        SurfaceFormat::Rgb10A2Unorm => None,
    }
}

/// What this backend accepts, for the error message when it is offered
/// something else.
pub(super) const SUPPORTED_FORMATS: &[SurfaceFormat] = &[SurfaceFormat::Bgra8Unorm];

/// How long `QueryOutput` may block waiting for a coded picture, in
/// milliseconds.
///
/// Waiting inside the driver rather than spinning: the encoding thread has
/// nothing else to do, and a poll loop would take CPU from the game. Small
/// enough that the deadline in `mod.rs` still bounds a hardware that has
/// stopped answering.
const QUERY_TIMEOUT_MS: i64 = 5;

/// Everything an encoder for `config` is told before `Init`.
///
/// # Panics
///
/// If `config.codec()` is one [`component_id`] does not name, which
/// `AmfEncoder::open` refuses before it reaches here.
pub(super) fn properties(config: &EncoderConfig) -> Vec<Property> {
    match config.codec() {
        Codec::Hevc => hevc_properties(config),
        Codec::H264 => h264_properties(config),
        Codec::Av1 => unreachable!("AV1 has no AMF component here; open refuses it first"),
    }
}

/// The H.264 property set.
fn h264_properties(config: &EncoderConfig) -> Vec<Property> {
    let resolution = config.resolution();
    let colour = config.colour_space();

    let mut properties = vec![
        // First, and deliberately: AMF documents `Usage` as configuring a whole
        // parameter set, so anything set before it would be overwritten.
        // Transcoding is the general-purpose one; the low-latency usages trade
        // picture quality for a shorter pipeline, which buys nothing for a
        // recorder writing to a local disk.
        // AMF_VIDEO_ENCODER_USAGE_TRANSCODING
        Property::int64(wide!("Usage"), 0),
        // High profile: what every H.264 decoder made this century supports,
        // and more efficient than Main at the same bit rate.
        // AMF_VIDEO_ENCODER_PROFILE_HIGH
        Property::int64(wide!("Profile"), 100),
        Property {
            name: wide!("FrameSize"),
            value: api::size(resolution.width, resolution.height),
        },
        Property {
            name: wide!("FrameRate"),
            value: api::rate(
                config.frame_rate().numerator(),
                config.frame_rate().denominator(),
            ),
        },
        Property::int64(wide!("QualityPreset"), h264_preset(config.preset())),
        // No B-frames. A recorder gains a few per cent of compression from them
        // and pays for it with reordered output, which means a decode timestamp
        // that differs from the presentation timestamp and a muxer that has to
        // reconstruct it. Until there is a muxer that does (issue #21), every
        // packet this encoder produces is in presentation order and pts equals
        // dts.
        Property::int64(wide!("BPicturesPattern"), 0),
    ];

    let gop = gop_length(config.keyframe_interval());
    properties.push(Property::int64(wide!("IDRPeriod"), gop));
    // Repeat the parameter sets at every keyframe rather than only at the start
    // of the stream. This is what makes a replay buffer possible: a clip cut
    // from the middle of a recording begins at a keyframe, and a decoder handed
    // a keyframe with no parameter sets in front of it cannot start (SPEC.md
    // section 7). AMF's range for the spacing stops at 1000, so a longer
    // interval repeats them more often than it needs to, which costs a few
    // dozen bytes and never leaves a keyframe uncovered.
    properties.push(Property::int64(
        wide!("HeaderInsertionSpacing"),
        gop.clamp(0, 1000),
    ));

    h264_rate_control(config.rate_control(), &mut properties);

    // What the encoded colours mean. A stream that does not say is guessed at by
    // the player, and different players guess differently: the same recording
    // comes out washed out in one and oversaturated in another.
    properties.push(Property::int64(
        wide!("OutColorProfile"),
        colour_profile(colour),
    ));
    properties.push(Property::int64(
        wide!("OutColorTransferChar"),
        i64::from(colour.transfer().code_point()),
    ));
    properties.push(Property::int64(
        wide!("OutColorPrimaries"),
        i64::from(colour.primaries().code_point()),
    ));

    properties.push(Property::int64(wide!("QueryTimeout"), QUERY_TIMEOUT_MS));
    properties
}

/// The HEVC property set.
///
/// The same decisions as [`h264_properties`], spelled with HEVC's own names —
/// AMF gives the two encoders separate property namespaces rather than sharing
/// one.
fn hevc_properties(config: &EncoderConfig) -> Vec<Property> {
    let resolution = config.resolution();
    let colour = config.colour_space();

    let mut properties = vec![
        // AMF_VIDEO_ENCODER_HEVC_USAGE_TRANSCODING
        Property::int64(wide!("HevcUsage"), 0),
        // Main profile and the main tier: 8-bit 4:2:0, which is all this build
        // produces, and the tier every decoder supports.
        // AMF_VIDEO_ENCODER_HEVC_PROFILE_MAIN
        Property::int64(wide!("HevcProfile"), 1),
        // AMF_VIDEO_ENCODER_HEVC_TIER_MAIN
        Property::int64(wide!("HevcTier"), 0),
        Property {
            name: wide!("HevcFrameSize"),
            value: api::size(resolution.width, resolution.height),
        },
        Property {
            name: wide!("HevcFrameRate"),
            value: api::rate(
                config.frame_rate().numerator(),
                config.frame_rate().denominator(),
            ),
        },
        Property::int64(wide!("HevcQualityPreset"), hevc_preset(config.preset())),
    ];

    match config.keyframe_interval() {
        KeyframeInterval::Frames(frames) => {
            properties.push(Property::int64(
                wide!("HevcGOPSize"),
                i64::from(frames.get()),
            ));
            // Every group of pictures starts with an IDR, which is what makes
            // the interval a sequence of cut points rather than of intra
            // pictures a clip cannot start at.
            properties.push(Property::int64(wide!("HevcGOPSPerIDR"), 1));
        }
        KeyframeInterval::Never => {
            // AMD's own words for this property: "The frequency to insert IDR as
            // start of a GOP. 0 means no IDR will be inserted." The group of
            // pictures keeps AMF's default length, so the stream still restarts
            // periodically with an intra picture — which is not a cut point, and
            // "never" is exactly a request for no more cut points.
            //
            // "No IDR will be inserted" is literal, and includes the first
            // picture: measured, this opens the stream with a `CRA_NUT` (NAL
            // type 21) that AMF reports as intra, where H.264's `IDRPeriod = 0`
            // opens with a real IDR. The first picture is therefore forced by
            // the session rather than left to this property — see
            // `super::force_idr`.
            properties.push(Property::int64(wide!("HevcGOPSPerIDR"), 0));
        }
    }
    // Parameter sets in front of every IDR, for the same reason as H.264.
    // AMF_VIDEO_ENCODER_HEVC_HEADER_INSERTION_MODE_IDR_ALIGNED
    properties.push(Property::int64(wide!("HevcHeaderInsertionMode"), 2));

    hevc_rate_control(config.rate_control(), &mut properties);

    properties.push(Property::int64(
        wide!("HevcOutColorProfile"),
        colour_profile(colour),
    ));
    properties.push(Property::int64(
        wide!("HevcOutColorTransferChar"),
        i64::from(colour.transfer().code_point()),
    ));
    properties.push(Property::int64(
        wide!("HevcOutColorPrimaries"),
        i64::from(colour.primaries().code_point()),
    ));
    properties.push(Property::int64(
        wide!("HevcNominalRange"),
        match colour.range() {
            // AMF_VIDEO_ENCODER_HEVC_NOMINAL_RANGE_FULL / _STUDIO
            ColourRange::Full => 1,
            ColourRange::Limited => 0,
        },
    ));

    properties.push(Property::int64(wide!("HevcQueryTimeout"), QUERY_TIMEOUT_MS));
    properties
}

/// Translates the rate control policy into H.264's properties.
fn h264_rate_control(rate_control: RateControl, properties: &mut Vec<Property>) {
    match rate_control {
        RateControl::Bitrate {
            average,
            peak: None,
        } => {
            let bits = i64::from(average.as_bits_per_second());
            // AMF_VIDEO_ENCODER_RATE_CONTROL_METHOD_CBR
            properties.push(Property::int64(wide!("RateControlMethod"), 1));
            properties.push(Property::int64(wide!("TargetBitrate"), bits));
            properties.push(Property::int64(wide!("PeakBitrate"), bits));
            // One second of video in the buffer, which is what a recording
            // wants: a buffer of a frame or two holds the rate very precisely
            // and costs visible quality whenever the picture changes.
            properties.push(Property::int64(wide!("VBVBufferSize"), bits));
            // 64 is AMF's way of writing "full", not a number of bits.
            properties.push(Property::int64(wide!("InitialVBVBufferFullness"), 64));
        }
        RateControl::Bitrate {
            average,
            peak: Some(peak),
        } => {
            // AMF_VIDEO_ENCODER_RATE_CONTROL_METHOD_PEAK_CONSTRAINED_VBR
            properties.push(Property::int64(wide!("RateControlMethod"), 2));
            properties.push(Property::int64(
                wide!("TargetBitrate"),
                i64::from(average.as_bits_per_second()),
            ));
            properties.push(Property::int64(
                wide!("PeakBitrate"),
                i64::from(peak.as_bits_per_second()),
            ));
        }
        RateControl::Quality { target, ceiling } => {
            // AMF's quality-targeted mode: variable bit rate aiming at a
            // quality level on the same 1-51 scale `QualityTarget` uses, which
            // is why no conversion happens here.
            // AMF_VIDEO_ENCODER_RATE_CONTROL_METHOD_QUALITY_VBR
            properties.push(Property::int64(wide!("RateControlMethod"), 4));
            properties.push(Property::int64(
                wide!("QvbrQualityLevel"),
                i64::from(target.as_level()),
            ));
            if let Some(ceiling) = ceiling {
                properties.push(Property::int64(
                    wide!("PeakBitrate"),
                    i64::from(ceiling.as_bits_per_second()),
                ));
            }
        }
    }
}

/// Translates the rate control policy into HEVC's properties.
///
/// The same policies, and one difference worth noticing: HEVC's rate control
/// enumeration is not in the same order as H.264's, so the numbers below are
/// not the numbers above.
fn hevc_rate_control(rate_control: RateControl, properties: &mut Vec<Property>) {
    match rate_control {
        RateControl::Bitrate {
            average,
            peak: None,
        } => {
            let bits = i64::from(average.as_bits_per_second());
            // AMF_VIDEO_ENCODER_HEVC_RATE_CONTROL_METHOD_CBR
            properties.push(Property::int64(wide!("HevcRateControlMethod"), 3));
            properties.push(Property::int64(wide!("HevcTargetBitrate"), bits));
            properties.push(Property::int64(wide!("HevcPeakBitrate"), bits));
            properties.push(Property::int64(wide!("HevcVBVBufferSize"), bits));
            properties.push(Property::int64(wide!("HevcInitialVBVBufferFullness"), 64));
        }
        RateControl::Bitrate {
            average,
            peak: Some(peak),
        } => {
            // AMF_VIDEO_ENCODER_HEVC_RATE_CONTROL_METHOD_PEAK_CONSTRAINED_VBR
            properties.push(Property::int64(wide!("HevcRateControlMethod"), 2));
            properties.push(Property::int64(
                wide!("HevcTargetBitrate"),
                i64::from(average.as_bits_per_second()),
            ));
            properties.push(Property::int64(
                wide!("HevcPeakBitrate"),
                i64::from(peak.as_bits_per_second()),
            ));
        }
        RateControl::Quality { target, ceiling } => {
            // AMF_VIDEO_ENCODER_HEVC_RATE_CONTROL_METHOD_QUALITY_VBR
            properties.push(Property::int64(wide!("HevcRateControlMethod"), 4));
            properties.push(Property::int64(
                wide!("HevcQvbrQualityLevel"),
                i64::from(target.as_level()),
            ));
            if let Some(ceiling) = ceiling {
                properties.push(Property::int64(
                    wide!("HevcPeakBitrate"),
                    i64::from(ceiling.as_bits_per_second()),
                ));
            }
        }
    }
}

/// The keyframe interval as the number H.264's `IDRPeriod` takes.
///
/// Zero for [`KeyframeInterval::Never`]; anything else is the interval in
/// frames.
///
/// What zero means is not documented. AMD's header describes
/// `AMF_VIDEO_ENCODER_IDR_PERIOD` only as "IDR Period in frames; default =
/// depends on USAGE" and says nothing about zero, unlike HEVC's
/// `HevcGOPSPerIDR`, whose zero case AMD does spell out. It is set because it
/// is the closest thing this property offers to the request, and nothing here
/// depends on being right about it: the tests observe a twelve-frame stream,
/// which is shorter than AMF's default group of pictures and so cannot tell the
/// two apart, and the one thing that must be true either way — that the first
/// picture is a cut point — is made true by [`super::force_idr`] rather than by
/// trusting this.
const fn gop_length(interval: KeyframeInterval) -> i64 {
    match interval {
        KeyframeInterval::Frames(frames) => frames.get() as i64,
        KeyframeInterval::Never => 0,
    }
}

/// Where H.264 sits on the quality-for-speed curve.
///
/// `AMF_VIDEO_ENCODER_QUALITY_PRESET_ENUM`: balanced is zero, and speed and
/// quality follow it.
const fn h264_preset(preset: EncodePreset) -> i64 {
    match preset {
        EncodePreset::Balanced => 0,
        EncodePreset::Speed => 1,
        EncodePreset::Quality => 2,
    }
}

/// Where HEVC sits on the quality-for-speed curve.
///
/// `AMF_VIDEO_ENCODER_HEVC_QUALITY_PRESET_ENUM` numbers the same three points
/// completely differently from H.264's, which is the sort of detail that
/// produces a recorder encoding at the wrong speed and never failing a call.
const fn hevc_preset(preset: EncodePreset) -> i64 {
    match preset {
        EncodePreset::Quality => 0,
        EncodePreset::Balanced => 5,
        EncodePreset::Speed => 10,
    }
}

/// The matrix and range together, as AMF's single colour profile value.
///
/// `AMF_VIDEO_CONVERTER_COLOR_PROFILE_ENUM` folds the two into one
/// enumeration, so the pair has to be mapped together.
const fn colour_profile(colour: ColourSpace) -> i64 {
    use crate::config::ColourMatrix;

    let profile = match (colour.matrix(), colour.range()) {
        (ColourMatrix::Bt601, ColourRange::Limited) => sys::AMF_VIDEO_CONVERTER_COLOR_PROFILE_601,
        (ColourMatrix::Bt601, ColourRange::Full) => sys::AMF_VIDEO_CONVERTER_COLOR_PROFILE_FULL_601,
        (ColourMatrix::Bt709, ColourRange::Limited) => sys::AMF_VIDEO_CONVERTER_COLOR_PROFILE_709,
        (ColourMatrix::Bt709, ColourRange::Full) => sys::AMF_VIDEO_CONVERTER_COLOR_PROFILE_FULL_709,
        (ColourMatrix::Bt2020Ncl, ColourRange::Limited) => {
            sys::AMF_VIDEO_CONVERTER_COLOR_PROFILE_2020
        }
        (ColourMatrix::Bt2020Ncl, ColourRange::Full) => {
            sys::AMF_VIDEO_CONVERTER_COLOR_PROFILE_FULL_2020
        }
    };
    profile as i64
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

    /// The whole-number value of the named property, or `None` if it was not
    /// set.
    fn value_of(properties: &[Property], name: &str) -> Option<i64> {
        properties
            .iter()
            .find(|property| api::name_to_string(property.name) == name)
            .and_then(|property| api::as_int64(&property.value))
    }

    #[test]
    fn usage_is_set_before_anything_it_would_overwrite() {
        // AMF documents `Usage` as configuring a whole parameter set. Setting it
        // second would silently undo the bit rate, the keyframe interval and the
        // preset, and every call would still report success.
        for codec in [Codec::H264, Codec::Hevc] {
            let properties = properties(&settings(
                codec,
                RateControl::constant(BitRate::megabits_per_second(20)),
            ));
            let first = api::name_to_string(properties[0].name);
            assert!(
                first == "Usage" || first == "HevcUsage",
                "{codec} configures {first} before its usage"
            );
        }
    }

    #[test]
    fn a_constant_bit_rate_asks_for_constant_bit_rate() {
        let properties = properties(&settings(
            Codec::H264,
            RateControl::constant(BitRate::megabits_per_second(40)),
        ));

        assert_eq!(
            value_of(&properties, "RateControlMethod"),
            Some(1),
            "AMF_VIDEO_ENCODER_RATE_CONTROL_METHOD_CBR"
        );
        assert_eq!(value_of(&properties, "TargetBitrate"), Some(40_000_000));
        assert_eq!(value_of(&properties, "PeakBitrate"), Some(40_000_000));
        assert_eq!(
            value_of(&properties, "VBVBufferSize"),
            Some(40_000_000),
            "a one-second buffer"
        );
    }

    #[test]
    fn the_two_codecs_number_constant_bit_rate_differently() {
        // HEVC's rate control enumeration is not H.264's with a prefix: CBR is 1
        // in one and 3 in the other. Copying the H.264 number into the HEVC path
        // would ask for peak-constrained VBR and never fail a call.
        let hevc = properties(&settings(
            Codec::Hevc,
            RateControl::constant(BitRate::megabits_per_second(40)),
        ));
        assert_eq!(
            value_of(&hevc, "HevcRateControlMethod"),
            Some(3),
            "AMF_VIDEO_ENCODER_HEVC_RATE_CONTROL_METHOD_CBR"
        );
        assert_eq!(value_of(&hevc, "HevcTargetBitrate"), Some(40_000_000));
        assert_eq!(value_of(&hevc, "HevcVBVBufferSize"), Some(40_000_000));
    }

    #[test]
    fn a_quality_target_is_asked_for_on_amfs_own_scale() {
        for (codec, method, level, peak) in [
            (
                Codec::H264,
                "RateControlMethod",
                "QvbrQualityLevel",
                "PeakBitrate",
            ),
            (
                Codec::Hevc,
                "HevcRateControlMethod",
                "HevcQvbrQualityLevel",
                "HevcPeakBitrate",
            ),
        ] {
            let targeted = properties(&settings(
                codec,
                RateControl::quality(QualityTarget::level(20).expect("20 is on the scale")),
            ));
            assert_eq!(
                value_of(&targeted, method),
                Some(4),
                "{codec} should ask for quality-targeted VBR"
            );
            assert_eq!(
                value_of(&targeted, level),
                Some(20),
                "AMF's QVBR level is the same 1-51 scale QualityTarget uses"
            );
            assert_eq!(
                value_of(&targeted, peak),
                None,
                "no ceiling was asked for, so none should be imposed"
            );

            let capped = properties(&settings(
                codec,
                RateControl::Quality {
                    target: QualityTarget::DEFAULT,
                    ceiling: Some(BitRate::megabits_per_second(60)),
                },
            ));
            assert_eq!(value_of(&capped, peak), Some(60_000_000));
        }
    }

    #[test]
    fn the_keyframe_interval_reaches_the_property_that_decides_it() {
        let interval = KeyframeInterval::Frames(NonZeroU32::new(120).expect("120 is not zero"));

        let h264 = properties(
            &settings(
                Codec::H264,
                RateControl::constant(BitRate::megabits_per_second(20)),
            )
            .with_keyframe_interval(interval),
        );
        assert_eq!(value_of(&h264, "IDRPeriod"), Some(120));
        assert_eq!(
            value_of(&h264, "HeaderInsertionSpacing"),
            Some(120),
            "the parameter sets have to repeat with the keyframes"
        );

        let hevc = properties(
            &settings(
                Codec::Hevc,
                RateControl::constant(BitRate::megabits_per_second(20)),
            )
            .with_keyframe_interval(interval),
        );
        assert_eq!(value_of(&hevc, "HevcGOPSize"), Some(120));
        assert_eq!(
            value_of(&hevc, "HevcGOPSPerIDR"),
            Some(1),
            "a group of pictures that does not start with an IDR is not a cut point"
        );
        assert_eq!(
            value_of(&hevc, "HevcHeaderInsertionMode"),
            Some(2),
            "IDR-aligned"
        );
    }

    #[test]
    fn never_asking_for_a_keyframe_asks_for_no_periodic_idr() {
        let h264 = properties(
            &settings(
                Codec::H264,
                RateControl::constant(BitRate::megabits_per_second(20)),
            )
            .with_keyframe_interval(KeyframeInterval::Never),
        );
        assert_eq!(value_of(&h264, "IDRPeriod"), Some(0));

        let hevc = properties(
            &settings(
                Codec::Hevc,
                RateControl::constant(BitRate::megabits_per_second(20)),
            )
            .with_keyframe_interval(KeyframeInterval::Never),
        );
        assert_eq!(value_of(&hevc, "HevcGOPSPerIDR"), Some(0));
        assert_eq!(
            value_of(&hevc, "HevcGOPSize"),
            None,
            "no interval was asked for, so none should be imposed"
        );
    }

    #[test]
    fn a_very_long_interval_still_repeats_the_parameter_sets() {
        // AMF's header insertion spacing stops at 1000. Passing a larger number
        // would be refused, and a stream whose parameter sets appear once is one
        // no clip can be cut from.
        let interval = KeyframeInterval::Frames(NonZeroU32::new(5_000).expect("5000 is not zero"));
        let h264 = properties(
            &settings(
                Codec::H264,
                RateControl::constant(BitRate::megabits_per_second(20)),
            )
            .with_keyframe_interval(interval),
        );

        assert_eq!(value_of(&h264, "IDRPeriod"), Some(5_000));
        assert_eq!(value_of(&h264, "HeaderInsertionSpacing"), Some(1_000));
    }

    #[test]
    fn no_b_frames_are_asked_for() {
        // The promise the packet timestamps depend on: with an IPPP structure
        // the encoder emits pictures in presentation order, so pts equals dts.
        let h264 = properties(&settings(
            Codec::H264,
            RateControl::constant(BitRate::megabits_per_second(20)),
        ));
        assert_eq!(value_of(&h264, "BPicturesPattern"), Some(0));
    }

    #[test]
    fn the_colour_description_is_written_into_the_stream() {
        for (codec, profile, transfer, primaries) in [
            (
                Codec::H264,
                "OutColorProfile",
                "OutColorTransferChar",
                "OutColorPrimaries",
            ),
            (
                Codec::Hevc,
                "HevcOutColorProfile",
                "HevcOutColorTransferChar",
                "HevcOutColorPrimaries",
            ),
        ] {
            let limited = properties(
                &settings(
                    codec,
                    RateControl::constant(BitRate::megabits_per_second(20)),
                )
                .with_colour_space(ColourSpace::BT709_LIMITED),
            );
            assert_eq!(
                value_of(&limited, profile),
                Some(1),
                "{codec}: AMF_VIDEO_CONVERTER_COLOR_PROFILE_709"
            );
            assert_eq!(value_of(&limited, transfer), Some(1), "{codec}: BT.709");
            assert_eq!(value_of(&limited, primaries), Some(1), "{codec}: BT.709");

            let full = properties(
                &settings(
                    codec,
                    RateControl::constant(BitRate::megabits_per_second(20)),
                )
                .with_colour_space(ColourSpace::BT709_FULL),
            );
            assert_eq!(
                value_of(&full, profile),
                Some(7),
                "{codec}: AMF_VIDEO_CONVERTER_COLOR_PROFILE_FULL_709"
            );
        }

        // HEVC signals the range twice over, and both have to agree.
        let full = properties(
            &settings(
                Codec::Hevc,
                RateControl::constant(BitRate::megabits_per_second(20)),
            )
            .with_colour_space(ColourSpace::BT709_FULL),
        );
        assert_eq!(value_of(&full, "HevcNominalRange"), Some(1));
    }

    #[test]
    fn the_two_codecs_number_the_presets_differently() {
        // Balanced is 0 for H.264 and 5 for HEVC; quality is 2 and 0. Sharing
        // one mapping would encode HEVC at the wrong point on the curve without
        // failing a call.
        for (preset, h264_value, hevc_value) in [
            (EncodePreset::Speed, 1, 10),
            (EncodePreset::Balanced, 0, 5),
            (EncodePreset::Quality, 2, 0),
        ] {
            assert_eq!(h264_preset(preset), h264_value, "{preset}");
            assert_eq!(hevc_preset(preset), hevc_value, "{preset}");
        }
    }

    #[test]
    fn only_an_idr_is_reported_as_a_cut_point() {
        // The replay buffer cuts clips at keyframes, and only an IDR is one.
        assert_eq!(picture_kind(Codec::H264, 0), PictureKind::Keyframe);
        assert_eq!(picture_kind(Codec::H264, 1), PictureKind::Intra);
        assert_eq!(picture_kind(Codec::H264, 2), PictureKind::Predicted);
        assert_eq!(picture_kind(Codec::H264, 3), PictureKind::Bidirectional);
        assert_eq!(picture_kind(Codec::H264, 4), PictureKind::Unknown);

        assert_eq!(picture_kind(Codec::Hevc, 0), PictureKind::Keyframe);
        assert_eq!(picture_kind(Codec::Hevc, 1), PictureKind::Intra);
        assert_eq!(picture_kind(Codec::Hevc, 2), PictureKind::Predicted);
        // HEVC's enumeration stops at P; a 3 here would be a value AMF does not
        // define, and guessing "bidirectional" would report a picture type the
        // encoder was configured never to produce.
        assert_eq!(picture_kind(Codec::Hevc, 3), PictureKind::Unknown);
    }

    #[test]
    fn each_codec_names_its_own_component_and_properties() {
        // A copy-and-paste error between the two namespaces would configure
        // nothing — AMF answers AMF_NOT_FOUND for a property it does not know —
        // and produce a recording at a default bit rate with no failure anywhere.
        assert_eq!(
            component_id(Codec::H264).map(api::name_to_string),
            Some("AMFVideoEncoderVCE_AVC".to_owned())
        );
        assert_eq!(
            component_id(Codec::Hevc).map(api::name_to_string),
            Some("AMFVideoEncoderHW_HEVC".to_owned()),
            "the HEVC component identifier is not spelled like its macro"
        );
        assert_eq!(
            component_id(Codec::Av1),
            None,
            "AV1 is not implemented by this backend"
        );

        assert_eq!(api::name_to_string(extra_data(Codec::H264)), "ExtraData");
        assert_eq!(
            api::name_to_string(extra_data(Codec::Hevc)),
            "HevcExtraData"
        );

        for property in properties(&settings(
            Codec::Hevc,
            RateControl::constant(BitRate::megabits_per_second(20)),
        )) {
            let name = api::name_to_string(property.name);
            assert!(
                name.starts_with("Hevc"),
                "{name} is an H.264 property in the HEVC set"
            );
        }
    }

    #[test]
    fn every_capability_name_is_the_one_amd_s_headers_define() {
        // The names below are hand-written, because AMF's are wide-string
        // macros and the generated bindings hold them as ASCII bytes. A typo
        // in one is not a failure: `GetProperty` answers AMF_NOT_FOUND, the
        // measurement is quietly never taken, and the report goes on showing an
        // inferred limit that a refresh was supposed to replace. So each one is
        // checked against the constant bindgen produced from the header.
        let header = |bytes: &'static [u8]| {
            String::from_utf8(bytes[..bytes.len() - 1].to_vec()).expect("AMF names are ASCII")
        };

        let h264 = capability_names(Codec::H264).expect("H.264 has a component");
        assert_eq!(
            api::name_to_string(h264.max_throughput),
            header(sys::AMF_VIDEO_ENCODER_CAP_MAX_THROUGHPUT)
        );
        assert_eq!(
            h264.b_frames.map(api::name_to_string),
            Some(header(sys::AMF_VIDEO_ENCODER_CAP_BFRAMES))
        );
        assert_eq!(
            api::name_to_string(h264.max_profile),
            header(sys::AMF_VIDEO_ENCODER_CAP_MAX_PROFILE)
        );

        let hevc = capability_names(Codec::Hevc).expect("HEVC has a component");
        assert_eq!(
            api::name_to_string(hevc.max_throughput),
            header(sys::AMF_VIDEO_ENCODER_HEVC_CAP_MAX_THROUGHPUT)
        );
        assert_eq!(
            hevc.b_frames, None,
            "AMF's HEVC encoder has no B-frame capability to ask about"
        );
        assert_eq!(
            api::name_to_string(hevc.max_profile),
            header(sys::AMF_VIDEO_ENCODER_HEVC_CAP_MAX_PROFILE)
        );

        assert!(
            capability_names(Codec::Av1).is_none(),
            "there is no AV1 component to ask, so there is nothing to name"
        );
    }

    #[test]
    fn ten_bit_is_read_from_a_profile_only_where_the_profile_says_it() {
        // HEVC's enumeration is Main, then Main10, so the comparison means what
        // it looks like it means.
        assert_eq!(
            ten_bit_from_profile(Codec::Hevc, sys::AMF_VIDEO_ENCODER_HEVC_PROFILE_MAIN.into()),
            Some(false)
        );
        assert_eq!(
            ten_bit_from_profile(
                Codec::Hevc,
                sys::AMF_VIDEO_ENCODER_HEVC_PROFILE_MAIN_10.into()
            ),
            Some(true)
        );

        // H.264's does not: `AMF_VIDEO_ENCODER_PROFILE_HIGH` is 100 and
        // `AMF_VIDEO_ENCODER_PROFILE_CONSTRAINED_HIGH` is 257, neither of them
        // a 10-bit profile, so no comparison against a maximum can answer the
        // question and this must decline to.
        assert_eq!(
            ten_bit_from_profile(
                Codec::H264,
                sys::AMF_VIDEO_ENCODER_PROFILE_CONSTRAINED_HIGH.into()
            ),
            None
        );
        assert_eq!(ten_bit_from_profile(Codec::Av1, 0), None);
    }

    #[test]
    fn a_forced_keyframe_asks_for_an_idr_and_the_headers_it_needs() {
        let h264: Vec<String> = forced_keyframe(Codec::H264)
            .iter()
            .map(|property| api::name_to_string(property.name))
            .collect();
        assert_eq!(h264, ["ForcePictureType", "InsertSPS", "InsertPPS"]);
        assert_eq!(
            api::as_int64(&forced_keyframe(Codec::H264)[0].value),
            Some(2),
            "AMF_VIDEO_ENCODER_PICTURE_TYPE_IDR"
        );

        let hevc = forced_keyframe(Codec::Hevc);
        assert_eq!(api::name_to_string(hevc[0].name), "HevcForcePictureType");
        assert_eq!(
            api::as_int64(&hevc[0].value),
            Some(2),
            "AMF_VIDEO_ENCODER_HEVC_PICTURE_TYPE_IDR"
        );
    }

    #[test]
    fn only_the_formats_this_backend_can_bind_have_a_surface_format() {
        assert_eq!(
            surface_format(SurfaceFormat::Bgra8Unorm),
            Some(sys::AMF_SURFACE_BGRA)
        );
        assert_eq!(surface_format(SurfaceFormat::Rgb10A2Unorm), None);
        assert_eq!(SUPPORTED_FORMATS, &[SurfaceFormat::Bgra8Unorm]);
    }
}
