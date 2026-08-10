//! Turning an [`EncoderConfig`] into the structures NVENC is initialised with.
//!
//! Kept apart from the session because it is the half that can be tested
//! without hardware: these are pure functions from Clipped's vocabulary to
//! NVIDIA's, and the tests at the bottom check them on any Windows machine,
//! GPU or not. What is left in `session.rs` is the sequence of calls, which
//! needs a real encoder.
//!
//! # What is deliberately left to the preset
//!
//! NVENC's configuration structure has upwards of two hundred fields. This
//! module starts from the preset configuration the driver returns — NVIDIA's
//! own tuning for the chosen point on the quality-for-speed curve — and
//! overwrites only what Clipped has an opinion about: rate control, the
//! keyframe interval, the colour description, and the absence of B-frames.
//! Anything else is NVIDIA's default, which is both better tuned than a guess
//! here and maintained by the people who make the silicon (AGENTS.md section
//! 1).

use crate::codec::Codec;
use crate::config::SurfaceFormat;
use crate::config::{ColourSpace, EncodePreset, EncoderConfig, KeyframeInterval, RateControl};

use super::sys;

/// Whether two identifiers are the same.
///
/// `GUID` comes from a C header and carries no equality of its own; comparing
/// the fields is what comparing two identifiers means.
pub(super) fn same_guid(left: &sys::GUID, right: &sys::GUID) -> bool {
    left.Data1 == right.Data1
        && left.Data2 == right.Data2
        && left.Data3 == right.Data3
        && left.Data4 == right.Data4
}

/// A GUID written the way `nvEncodeAPI.h` writes one.
const fn guid(data1: u32, data2: u16, data3: u16, data4: [u8; 8]) -> sys::GUID {
    sys::GUID {
        Data1: data1,
        Data2: data2,
        Data3: data3,
        Data4: data4,
    }
}

/// The codec identifier NVENC knows this codec by.
///
/// Transcribed from the `NV_ENC_CODEC_*_GUID` constants in `nvEncodeAPI.h`;
/// bindgen cannot emit them, because a `static const` in a header has no symbol
/// to link against (see `sys.rs`).
pub(super) const fn codec_guid(codec: Codec) -> sys::GUID {
    match codec {
        // {6BC82762-4E63-4ca4-AA85-1E50F321F6BF}
        Codec::H264 => guid(
            0x6bc8_2762,
            0x4e63,
            0x4ca4,
            [0xaa, 0x85, 0x1e, 0x50, 0xf3, 0x21, 0xf6, 0xbf],
        ),
        // {790CDC88-4522-4d7b-9425-BDA9975F7603}
        Codec::Hevc => guid(
            0x790c_dc88,
            0x4522,
            0x4d7b,
            [0x94, 0x25, 0xbd, 0xa9, 0x97, 0x5f, 0x76, 0x03],
        ),
        // {0A352289-0AA7-4759-862D-5D15CD16D254}
        Codec::Av1 => guid(
            0x0a35_2289,
            0x0aa7,
            0x4759,
            [0x86, 0x2d, 0x5d, 0x15, 0xcd, 0x16, 0xd2, 0x54],
        ),
    }
}

/// The preset identifier for a point on the quality-for-speed curve.
///
/// NVENC's presets are P1 (fastest) to P7 (best quality). Three of the seven
/// are used, because the difference between adjacent presets is not something
/// a user can judge and four unused options are four ways to be misconfigured.
pub(super) const fn preset_guid(preset: EncodePreset) -> sys::GUID {
    match preset {
        // P1 {FC0A8D3E-45F8-4CF8-80C7-298871590EBF}
        EncodePreset::Speed => guid(
            0xfc0a_8d3e,
            0x45f8,
            0x4cf8,
            [0x80, 0xc7, 0x29, 0x88, 0x71, 0x59, 0x0e, 0xbf],
        ),
        // P4 {90A7B826-DF06-4862-B9D2-CD6D73A08681}
        EncodePreset::Balanced => guid(
            0x90a7_b826,
            0xdf06,
            0x4862,
            [0xb9, 0xd2, 0xcd, 0x6d, 0x73, 0xa0, 0x86, 0x81],
        ),
        // P7 {84848C12-6F71-4C13-931B-53E283F57974}
        EncodePreset::Quality => guid(
            0x8484_8c12,
            0x6f71,
            0x4c13,
            [0x93, 0x1b, 0x53, 0xe2, 0x83, 0xf5, 0x79, 0x74],
        ),
    }
}

/// The profile to encode in.
///
/// `AUTOSELECT` for H.264: the encoder picks High, which is what every decoder
/// made this century supports. HEVC and AV1 name Main explicitly, because their
/// autoselect can land on a profile with a narrower decoder base and this build
/// only produces 8-bit 4:2:0 anyway.
pub(super) const fn profile_guid(codec: Codec) -> sys::GUID {
    match codec {
        // {BFD6F8E7-233C-4341-8B3E-4818523803F4}
        Codec::H264 => guid(
            0xbfd6_f8e7,
            0x233c,
            0x4341,
            [0x8b, 0x3e, 0x48, 0x18, 0x52, 0x38, 0x03, 0xf4],
        ),
        // {B514C39A-B55B-40fa-878F-F1253B4DFDEC}
        Codec::Hevc => guid(
            0xb514_c39a,
            0xb55b,
            0x40fa,
            [0x87, 0x8f, 0xf1, 0x25, 0x3b, 0x4d, 0xfd, 0xec],
        ),
        // {5F2A39F5-F14E-4F95-9A9E-B76D568FCF97}
        Codec::Av1 => guid(
            0x5f2a_39f5,
            0xf14e,
            0x4f95,
            [0x9a, 0x9e, 0xb7, 0x6d, 0x56, 0x8f, 0xcf, 0x97],
        ),
    }
}

/// The tuning NVENC should optimise for.
///
/// High quality for every preset, deliberately: Clipped writes to a local disk,
/// so the latency tunings — which trade picture quality for a shorter pipeline
/// — buy nothing. A streaming path would want `LOW_LATENCY`, and that is a
/// decision for the ticket that adds streaming rather than a knob nobody can
/// currently reach. `EncodePreset` still chooses the preset itself, which is
/// where the speed-against-quality trade lives (see [`preset_guid`]).
pub(super) const TUNING: sys::NV_ENC_TUNING_INFO = sys::NV_ENC_TUNING_INFO_HIGH_QUALITY;

/// The NVENC buffer format for a captured surface layout, or [`None`] if this
/// backend cannot take it.
///
/// `NV_ENC_BUFFER_FORMAT_ARGB` is NVIDIA's name for a packed 32-bit surface
/// whose bytes are blue, green, red, alpha — which is exactly
/// `DXGI_FORMAT_B8G8R8A8_UNORM`, the format both Windows capture APIs produce.
/// Feeding it directly is what keeps the frame on the GPU: NVENC does the
/// conversion to 4:2:0 in hardware as part of encoding, so nothing here has to
/// run a shader or a copy first.
pub(super) const fn buffer_format(format: SurfaceFormat) -> Option<sys::NV_ENC_BUFFER_FORMAT> {
    match format {
        SurfaceFormat::Bgra8Unorm => Some(sys::NV_ENC_BUFFER_FORMAT_ARGB),
        // 10-bit HDR needs a 10-bit profile, a different colour description and
        // a way to test it, none of which exist yet
        // (https://github.com/wildware-uk/clipped/issues/99).
        SurfaceFormat::Rgb10A2Unorm => None,
    }
}

/// What this backend accepts, for the error message when it is offered
/// something else.
pub(super) const SUPPORTED_FORMATS: &[SurfaceFormat] = &[SurfaceFormat::Bgra8Unorm];

/// Overwrites the parts of a preset configuration that Clipped has an opinion
/// about.
///
/// `config` comes from `nvEncGetEncodePresetConfigEx` and is left alone
/// everywhere this does not write.
pub(super) fn apply(settings: &EncoderConfig, config: &mut sys::NV_ENC_CONFIG) {
    config.version = super::api::CONFIG_VER;
    config.profileGUID = profile_guid(settings.codec());

    // No B-frames. Two reasons, and the second is the one that matters: a
    // recorder gains a few per cent of compression from them and pays for it
    // with reordered output, which means a decode timestamp that differs from
    // the presentation timestamp and a muxer that has to reconstruct it. Until
    // there is a muxer that does (issue #21), every packet this encoder
    // produces is in presentation order and pts equals dts.
    config.frameIntervalP = 1;

    // No lookahead, whatever the preset returned. The header's own words about
    // the flag are the reason: "if lookahead is enabled, input frames must
    // remain available to the encoder until encode completion". The frames this
    // backend is given are borrowed from a capture backend that recycles them
    // as soon as `submit` returns, so an encoder that buffered them would read
    // a surface that had been overwritten. With this off and `frameIntervalP`
    // at 1, every picture is coded on the submission that carries it.
    config.rcParams.set_enableLookahead(0);
    config.rcParams.lookaheadDepth = 0;

    let gop = match settings.keyframe_interval() {
        KeyframeInterval::Frames(frames) => frames.get(),
        KeyframeInterval::Never => sys::NVENC_INFINITE_GOPLENGTH,
    };
    config.gopLength = gop;

    apply_rate_control(settings.rate_control(), &mut config.rcParams);

    // The keyframe interval and the colour description live in the
    // codec-specific half of the configuration, which is a union: only the
    // member matching the codec being encoded may be written.
    match settings.codec() {
        // SAFETY: touching a union member is only sound if it is the member
        // that was written, and here that holds for a reason stronger than
        // convention. The whole union arrived zeroed from
        // `nvEncGetEncodePresetConfigEx`, every member is plain data for which
        // all-zeroes is a valid value, and the arm is selected by the same
        // codec the encoder is being configured for — so nothing writes one
        // member and reads another. The same argument covers the two arms
        // below.
        Codec::H264 => unsafe {
            config.encodeCodecConfig.h264Config.idrPeriod = gop;
            // Repeat the parameter sets at every keyframe rather than only at
            // the start of the stream. This is what makes a replay buffer
            // possible: a clip cut from the middle of a recording begins at a
            // keyframe, and a decoder handed a keyframe with no parameter sets
            // in front of it cannot start (SPEC.md section 7).
            config.encodeCodecConfig.h264Config.set_repeatSPSPPS(1);
            apply_colour(
                settings.colour_space(),
                &mut config.encodeCodecConfig.h264Config.h264VUIParameters,
            );
        },
        // SAFETY: as the H.264 arm above argues, for the HEVC member.
        Codec::Hevc => unsafe {
            config.encodeCodecConfig.hevcConfig.idrPeriod = gop;
            config.encodeCodecConfig.hevcConfig.set_repeatSPSPPS(1);
            apply_colour(
                settings.colour_space(),
                &mut config.encodeCodecConfig.hevcConfig.hevcVUIParameters,
            );
        },
        // SAFETY: as the H.264 arm above argues, for the AV1 member.
        Codec::Av1 => unsafe {
            let colour = settings.colour_space();
            let av1 = &mut config.encodeCodecConfig.av1Config;
            av1.idrPeriod = gop;
            // AV1's equivalent of repeating the parameter sets.
            av1.set_repeatSeqHdr(1);
            av1.colorPrimaries = colour.primaries().code_point() as sys::NV_ENC_VUI_COLOR_PRIMARIES;
            av1.transferCharacteristics =
                colour.transfer().code_point() as sys::NV_ENC_VUI_TRANSFER_CHARACTERISTIC;
            av1.matrixCoefficients = colour.matrix().code_point() as sys::NV_ENC_VUI_MATRIX_COEFFS;
            av1.colorRange = full_range_flag(colour);
        },
    }
}

/// Writes the VUI parameters H.264 and HEVC share.
///
/// A stream that does not say what its colours mean is guessed at by the
/// player, and different players guess differently: the same recording comes
/// out washed out in one and oversaturated in another. Saying so costs a few
/// bits once per keyframe.
fn apply_colour(colour: ColourSpace, vui: &mut sys::NV_ENC_CONFIG_H264_VUI_PARAMETERS) {
    vui.videoSignalTypePresentFlag = 1;
    vui.videoFormat = sys::NV_ENC_VUI_VIDEO_FORMAT_UNSPECIFIED;
    vui.videoFullRangeFlag = full_range_flag(colour);
    vui.colourDescriptionPresentFlag = 1;
    vui.colourPrimaries = colour.primaries().code_point() as sys::NV_ENC_VUI_COLOR_PRIMARIES;
    vui.transferCharacteristics =
        colour.transfer().code_point() as sys::NV_ENC_VUI_TRANSFER_CHARACTERISTIC;
    vui.colourMatrix = colour.matrix().code_point() as sys::NV_ENC_VUI_MATRIX_COEFFS;
}

/// 1 for full range, 0 for limited, which is how every codec spells it.
const fn full_range_flag(colour: ColourSpace) -> u32 {
    match colour.range() {
        crate::config::ColourRange::Full => 1,
        crate::config::ColourRange::Limited => 0,
    }
}

/// Translates the rate control policy.
///
/// `params.version` is deliberately left as `nvEncGetEncodePresetConfigEx`
/// returned it. `NV_ENC_RC_PARAMS` is separately versioned — the header has an
/// `NV_ENC_RC_PARAMS_VER` macro of its own — and NVIDIA's samples and FFmpeg
/// both leave the preset's value alone rather than assert one here.
fn apply_rate_control(rate_control: RateControl, params: &mut sys::NV_ENC_RC_PARAMS) {
    match rate_control {
        RateControl::Bitrate {
            average,
            peak: None,
        } => {
            params.rateControlMode = sys::NV_ENC_PARAMS_RC_CBR;
            params.averageBitRate = average.as_bits_per_second();
            params.maxBitRate = average.as_bits_per_second();
            // One second of video in the buffer. NVENC's default for constant
            // bit rate is a single frame, which holds the rate very precisely
            // and costs visible quality whenever the picture changes — the
            // trade a low-latency stream wants and a recording does not.
            params.vbvBufferSize = average.as_bits_per_second();
            params.vbvInitialDelay = average.as_bits_per_second();
        }
        RateControl::Bitrate {
            average,
            peak: Some(peak),
        } => {
            params.rateControlMode = sys::NV_ENC_PARAMS_RC_VBR;
            params.averageBitRate = average.as_bits_per_second();
            params.maxBitRate = peak.as_bits_per_second();
            params.vbvBufferSize = 0;
            params.vbvInitialDelay = 0;
        }
        RateControl::Quality { target, ceiling } => {
            // NVENC's quality-targeted mode is variable bit rate with a target
            // quality and no average to aim at: leaving `averageBitRate` at
            // zero is what tells it to spend whatever the target needs.
            params.rateControlMode = sys::NV_ENC_PARAMS_RC_VBR;
            params.averageBitRate = 0;
            params.maxBitRate = ceiling.map_or(0, |ceiling| ceiling.as_bits_per_second());
            params.targetQuality = target.as_level();
            params.targetQualityLSB = 0;
            params.vbvBufferSize = 0;
            params.vbvInitialDelay = 0;
        }
    }
}

/// Builds the initialisation parameters around a configuration.
///
/// `encode_config` must point at the [`apply`]-ed configuration and must stay
/// alive until `nvEncInitializeEncoder` returns: NVENC reads through the
/// pointer rather than copying it.
pub(super) fn initialise_params(
    settings: &EncoderConfig,
    encode_config: *mut sys::NV_ENC_CONFIG,
) -> sys::NV_ENC_INITIALIZE_PARAMS {
    let resolution = settings.resolution();

    // SAFETY: `NV_ENC_INITIALIZE_PARAMS` is plain data — integers, GUIDs,
    // pointers and a bitfield — none of which has an invalid all-zero pattern,
    // and NVENC requires every field it is not told about to be zero.
    let mut params: sys::NV_ENC_INITIALIZE_PARAMS = unsafe { core::mem::zeroed() };

    params.version = super::api::INITIALIZE_PARAMS_VER;
    params.encodeGUID = codec_guid(settings.codec());
    params.presetGUID = preset_guid(settings.preset());
    params.encodeWidth = resolution.width;
    params.encodeHeight = resolution.height;
    // Display aspect ratio: square pixels, which is what a captured desktop or
    // game window always has.
    params.darWidth = resolution.width;
    params.darHeight = resolution.height;
    params.frameRateNum = settings.frame_rate().numerator();
    params.frameRateDen = settings.frame_rate().denominator();
    // Synchronous mode. The asynchronous mode signals a Windows event per
    // frame, which buys a lower latency this pipeline does not need and costs
    // an event object and a wait per frame on the capture thread.
    params.enableEncodeAsync = 0;
    // Let NVENC decide picture types, which is what makes the configured
    // keyframe interval happen at all.
    params.enablePTD = 1;
    params.encodeConfig = encode_config;
    params.maxEncodeWidth = resolution.width;
    params.maxEncodeHeight = resolution.height;
    params.tuningInfo = TUNING;
    params
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::Resolution;
    use crate::config::{BitRate, ColourSpace, FrameRate, QualityTarget};
    use core::num::NonZeroU32;

    fn settings(codec: Codec, rate_control: RateControl) -> EncoderConfig {
        EncoderConfig::new(
            codec,
            Resolution::new(2560, 1440),
            FrameRate::FPS_60,
            rate_control,
        )
    }

    /// A zeroed configuration, standing in for the one the driver's preset
    /// would have filled in. Everything [`apply`] does not touch stays zero,
    /// which is what makes these assertions meaningful.
    fn blank_config() -> sys::NV_ENC_CONFIG {
        // SAFETY: plain data, as `initialise_params` argues at more length.
        unsafe { core::mem::zeroed() }
    }

    #[test]
    fn a_constant_bit_rate_asks_for_constant_bit_rate() {
        let mut config = blank_config();
        apply(
            &settings(
                Codec::H264,
                RateControl::constant(BitRate::megabits_per_second(40)),
            ),
            &mut config,
        );

        assert_eq!(config.rcParams.rateControlMode, sys::NV_ENC_PARAMS_RC_CBR);
        assert_eq!(config.rcParams.averageBitRate, 40_000_000);
        assert_eq!(config.rcParams.maxBitRate, 40_000_000);
        assert_eq!(
            config.rcParams.vbvBufferSize, 40_000_000,
            "a one-second buffer, not NVENC's single-frame default"
        );
    }

    #[test]
    fn a_quality_target_spends_whatever_it_needs() {
        let mut config = blank_config();
        apply(
            &settings(
                Codec::Hevc,
                RateControl::quality(QualityTarget::level(20).expect("20 is on the scale")),
            ),
            &mut config,
        );

        assert_eq!(config.rcParams.rateControlMode, sys::NV_ENC_PARAMS_RC_VBR);
        assert_eq!(config.rcParams.targetQuality, 20);
        assert_eq!(
            config.rcParams.averageBitRate, 0,
            "a target bit rate would override the quality target"
        );
        assert_eq!(config.rcParams.maxBitRate, 0, "no ceiling was asked for");
    }

    #[test]
    fn a_quality_target_can_be_capped() {
        let mut config = blank_config();
        apply(
            &settings(
                Codec::Hevc,
                RateControl::Quality {
                    target: QualityTarget::DEFAULT,
                    ceiling: Some(BitRate::megabits_per_second(60)),
                },
            ),
            &mut config,
        );

        assert_eq!(config.rcParams.maxBitRate, 60_000_000);
    }

    #[test]
    fn the_keyframe_interval_reaches_both_places_that_decide_it() {
        // gopLength alone is not enough: each codec has its own IDR period, and
        // an encoder that sets one and not the other produces a stream with
        // intra frames that are not cut points — which looks fine in a player
        // and breaks every clip the replay buffer tries to save.
        for codec in [Codec::H264, Codec::Hevc, Codec::Av1] {
            let mut config = blank_config();
            let interval = KeyframeInterval::Frames(NonZeroU32::new(120).expect("120 is not zero"));
            apply(
                &settings(
                    codec,
                    RateControl::constant(BitRate::megabits_per_second(40)),
                )
                .with_keyframe_interval(interval),
                &mut config,
            );

            assert_eq!(config.gopLength, 120);

            // SAFETY: `apply` writes the union member matching `codec`, and
            // this reads the same one.
            let idr_period = unsafe {
                match codec {
                    Codec::H264 => config.encodeCodecConfig.h264Config.idrPeriod,
                    Codec::Hevc => config.encodeCodecConfig.hevcConfig.idrPeriod,
                    Codec::Av1 => config.encodeCodecConfig.av1Config.idrPeriod,
                }
            };
            assert_eq!(idr_period, 120, "{codec} keyframes would not be cut points");
        }
    }

    #[test]
    fn never_asking_for_a_keyframe_is_an_infinite_group_of_pictures() {
        let mut config = blank_config();
        apply(
            &settings(
                Codec::H264,
                RateControl::constant(BitRate::megabits_per_second(10)),
            )
            .with_keyframe_interval(KeyframeInterval::Never),
            &mut config,
        );

        assert_eq!(config.gopLength, sys::NVENC_INFINITE_GOPLENGTH);
    }

    #[test]
    fn the_parameter_sets_repeat_so_that_a_clip_can_start_anywhere() {
        let mut config = blank_config();
        apply(
            &settings(
                Codec::H264,
                RateControl::constant(BitRate::megabits_per_second(10)),
            ),
            &mut config,
        );

        // SAFETY: the H.264 member is the one `apply` wrote for this codec.
        let repeats = unsafe { config.encodeCodecConfig.h264Config.repeatSPSPPS() };
        assert_eq!(repeats, 1);
    }

    #[test]
    fn the_colour_description_is_written_into_the_stream() {
        let mut config = blank_config();
        apply(
            &settings(
                Codec::H264,
                RateControl::constant(BitRate::megabits_per_second(10)),
            )
            .with_colour_space(ColourSpace::BT709_LIMITED),
            &mut config,
        );

        // SAFETY: the H.264 member is the one `apply` wrote for this codec.
        let vui = unsafe { config.encodeCodecConfig.h264Config.h264VUIParameters };
        assert_eq!(vui.videoSignalTypePresentFlag, 1);
        assert_eq!(vui.colourDescriptionPresentFlag, 1);
        assert_eq!(vui.colourPrimaries, 1, "BT.709");
        assert_eq!(vui.transferCharacteristics, 1, "BT.709");
        assert_eq!(vui.colourMatrix, 1, "BT.709");
        assert_eq!(vui.videoFullRangeFlag, 0, "limited range");
    }

    #[test]
    fn full_range_reaches_the_stream_as_well() {
        let mut config = blank_config();
        apply(
            &settings(
                Codec::Av1,
                RateControl::constant(BitRate::megabits_per_second(10)),
            )
            .with_colour_space(ColourSpace::BT709_FULL),
            &mut config,
        );

        // SAFETY: the AV1 member is the one `apply` wrote for this codec.
        assert_eq!(unsafe { config.encodeCodecConfig.av1Config.colorRange }, 1);
    }

    #[test]
    fn no_b_frames_are_asked_for() {
        // The promise the packet timestamps depend on: with an IPPP structure
        // the encoder emits pictures in presentation order, so pts equals dts.
        let mut config = blank_config();
        apply(
            &settings(
                Codec::Hevc,
                RateControl::constant(BitRate::megabits_per_second(10)),
            ),
            &mut config,
        );

        assert_eq!(config.frameIntervalP, 1);
    }

    #[test]
    fn initialisation_carries_the_size_and_rate_it_was_given() {
        let settings = settings(
            Codec::Av1,
            RateControl::constant(BitRate::megabits_per_second(10)),
        );
        let mut config = blank_config();
        let params = initialise_params(&settings, &raw mut config);

        assert_eq!(params.encodeWidth, 2560);
        assert_eq!(params.encodeHeight, 1440);
        assert_eq!(params.maxEncodeWidth, 2560);
        assert_eq!(params.maxEncodeHeight, 1440);
        assert_eq!(params.frameRateNum, 60);
        assert_eq!(params.frameRateDen, 1);
        assert_eq!(params.enablePTD, 1);
        assert_eq!(params.enableEncodeAsync, 0);
        assert!(same_guid(&params.encodeGUID, &codec_guid(Codec::Av1)));
        assert_eq!(params.encodeConfig, &raw mut config);
    }

    #[test]
    fn every_codec_and_preset_has_a_distinct_identifier() {
        // A copy-and-paste error between two of these GUIDs would encode the
        // wrong codec at the wrong speed and never fail a call.
        let codecs = [
            codec_guid(Codec::H264),
            codec_guid(Codec::Hevc),
            codec_guid(Codec::Av1),
        ];
        for (index, first) in codecs.iter().enumerate() {
            for second in &codecs[index + 1..] {
                assert!(!same_guid(first, second));
            }
        }

        let presets = [
            preset_guid(EncodePreset::Speed),
            preset_guid(EncodePreset::Balanced),
            preset_guid(EncodePreset::Quality),
        ];
        for (index, first) in presets.iter().enumerate() {
            for second in &presets[index + 1..] {
                assert!(!same_guid(first, second));
            }
        }
    }

    #[test]
    fn only_the_formats_this_backend_can_bind_have_a_buffer_format() {
        assert_eq!(
            buffer_format(SurfaceFormat::Bgra8Unorm),
            Some(sys::NV_ENC_BUFFER_FORMAT_ARGB)
        );
        assert_eq!(buffer_format(SurfaceFormat::Rgb10A2Unorm), None);
        assert_eq!(SUPPORTED_FORMATS, &[SurfaceFormat::Bgra8Unorm]);
    }
}
