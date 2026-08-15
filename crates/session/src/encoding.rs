//! Choosing an encoder for a recording, and opening it.
//!
//! `clipped-encoder` deliberately has no factory: it ranks encoder families
//! from a capability report ([`clipped_encoder::recommend`]), and each backend
//! is opened by its own function against a platform device that no abstract
//! factory could supply. The dispatch over [`EncoderKind`] therefore belongs to
//! the crate that has more than one backend to dispatch to, which is this one
//! (`crates/encoder/src/backend.rs`, "There is no factory").
//!
//! # How a candidate is chosen
//!
//! With the encoder left automatic, the ranked list is tried in order and the
//! first one that *opens* wins. Trying rather than trusting is deliberate:
//! detection answers "does this machine have an NVENC runtime?", and the
//! question that actually decides a recording is "can a session be opened on
//! the device these frames are on?" — which differs on a machine with two GPUs,
//! where the capture may be on the other one. A candidate that refuses is
//! logged with its reason and the next one is tried.
//!
//! With an encoder named explicitly there is no fallback. Someone who typed
//! `--encoder nvenc` wants to know it was not used, not to discover afterwards
//! that the recording was made on the CPU.
//!
//! There is one exception, and it is the caller's to ask for: a recording given
//! [`UnavailableChoice::Substitute`] tries the encoder it was told to use first
//! and then the ranked candidates, because that encoder came from a settings
//! file rather than from a command line and losing a game's footage over a
//! stale setting is the worse failure ([`crate::settings::UnavailableChoice`]
//! says which caller is which). The substitution is logged at `warn` naming
//! both encoders, so it is never something a user has to infer from a file.

use clipped_capture::PixelFormat;
use clipped_encoder::{
    detect_cached, measured_codecs, recommend, AmfEncoder, BitRate, CapabilityCache,
    CapabilityReport, Codec, EncodeError, EncoderConfig, EncoderKind, FrameRate, GraphicsDevice,
    KeyframeInterval, NvencEncoder, Probing, QuickSyncEncoder, RateControl, Resolution,
    SoftwareEncoder, SurfaceFormat, SystemProbe, VideoEncoder, WindowsProbe,
};

use crate::error::SessionError;
use crate::settings::{CodecPreference, EncoderPreference, RecordingSettings, UnavailableChoice};

/// Bits spent per pixel per frame, before any clamping.
///
/// The command line has no bitrate option yet — SPEC.md section 10 asks for one
/// and it is
/// [issue #181](https://github.com/wildware-uk/clipped/issues/181) — so the
/// session has to choose, and choosing a fixed number would be wrong at every
/// resolution but one. Scaling with pixels
/// and rate gives 4.1 Mbit/s for 720p30 and 33 Mbit/s for 1440p60, which is in
/// the region recorders recommend for H.264 game footage and is generous for
/// HEVC and AV1.
const BITS_PER_PIXEL_PER_FRAME: f64 = 0.15;

/// The least a recording may be given, whatever the arithmetic says.
const MINIMUM_BITRATE: u32 = 2_000_000;

/// The most a recording may be given.
///
/// A 8K 144 fps recording works out at 716 Mbit/s, which fills a disk in
/// minutes. The ceiling is a guard against the arithmetic, not a quality
/// judgement.
const MAXIMUM_BITRATE: u32 = 120_000_000;

/// An encoder session, and what it turned out to be.
pub(crate) struct OpenedEncoder {
    /// The live session.
    pub(crate) encoder: Box<dyn VideoEncoder>,
    /// Which family opened it.
    pub(crate) kind: EncoderKind,
    /// What it is producing.
    pub(crate) codec: Codec,
    /// How many bits a second it was configured for.
    ///
    /// Carried out of here rather than recomputed by whoever wants it, because
    /// it is chosen from the size capture is *actually* producing rather than
    /// the size that was asked for — and a replay buffer sizes its memory
    /// ceiling from it (`crate::replay`, `docs/replay-buffer.md`). Two places
    /// deriving a bitrate from a picture would be two answers (AGENTS.md
    /// section 55).
    pub(crate) bitrate: BitRate,
}

/// Opens an encoder for `settings`, against the device the frames are on.
///
/// `size` is what capture is actually producing, not what the target was
/// measured at: a window capture includes the window's chrome, so the two
/// differ for an ordinary window.
pub(crate) fn open(
    device: &GraphicsDevice,
    settings: &RecordingSettings,
    size: (u32, u32),
    pixel_format: PixelFormat,
) -> Result<OpenedEncoder, SessionError> {
    let source_format = surface_format(pixel_format)?;
    let frame_rate = FrameRate::new(settings.framerate(), 1).ok_or(SessionError::ZeroFramerate)?;
    let resolution = Resolution::new(size.0, size.1);

    let probe = WindowsProbe::new();
    // `WithoutSessions`: opening a recording is exactly the moment an extra
    // encoder session must not be opened on the user's behalf (see
    // `apps/recorder/src/capabilities.rs` and `crates/encoder/src/probe.rs`,
    // `Probing`). The published limits are enough to rank candidates; the
    // numeric limits `--refresh` measures are not consulted here.
    let detection = detect_cached(
        &probe as &dyn SystemProbe,
        &capability_cache(),
        Probing::WithoutSessions,
    )?;

    let bitrate = bitrate_for(size, frame_rate);

    let mut attempts = Vec::new();
    for (kind, codec) in candidates(settings, detection.report()) {
        let config = EncoderConfig::new(
            codec,
            resolution,
            frame_rate,
            RateControl::constant(bitrate),
        )
        .with_keyframe_interval(KeyframeInterval::every(
            KeyframeInterval::DEFAULT,
            frame_rate,
        ))
        .with_source_format(source_format);

        match open_one(kind, device, config) {
            Ok(encoder) => {
                tracing::info!(
                    encoder = %kind.log_encoder_family(),
                    codec = codec.log_value(),
                    configuration = %config,
                    "encoder session opened"
                );
                // Said out loud rather than left to be worked out from the two
                // lines above: this recording was configured for one encoder
                // and is being made with another (AGENTS.md section 45).
                if let EncoderPreference::Fixed(requested) = settings.encoder() {
                    if requested != kind {
                        tracing::warn!(
                            configured = %requested.log_encoder_family(),
                            encoder = %kind.log_encoder_family(),
                            "the encoder configured for this recording could not be opened, so it \
                             is being recorded with another one rather than not at all"
                        );
                    }
                }
                return Ok(OpenedEncoder {
                    encoder,
                    kind,
                    codec,
                    bitrate,
                });
            }
            Err(error) => {
                tracing::warn!(
                    encoder = %kind.log_encoder_family(),
                    codec = codec.log_value(),
                    %error,
                    "this encoder could not be opened for the recording"
                );
                attempts.push((kind, error.to_string()));
            }
        }
    }

    Err(SessionError::NoEncoder { attempts })
}

/// The cache detection reads and writes, or one that never answers.
///
/// The same cache `clipped-recorder capabilities` uses, so a recording started
/// straight after a capability report does not probe the machine again. A
/// machine with no `%LOCALAPPDATA%` — which Windows always provides, but a
/// stripped-down environment may not — probes every time rather than failing.
fn capability_cache() -> CapabilityCache {
    CapabilityCache::default_path().map_or_else(CapabilityCache::disabled, CapabilityCache::at)
}

/// The encoders to try, most preferred first, each with the codec to ask it
/// for.
fn candidates(
    settings: &RecordingSettings,
    report: &CapabilityReport,
) -> Vec<(EncoderKind, Codec)> {
    let requested = match settings.codec() {
        CodecPreference::Automatic => None,
        CodecPreference::Fixed(codec) => Some(codec),
    };

    let ranked = || -> Vec<(EncoderKind, Codec)> {
        recommend(report)
            .into_iter()
            .map(|recommendation| {
                (
                    recommendation.encoder(),
                    requested.unwrap_or_else(|| recommendation.codec()),
                )
            })
            .collect()
    };

    match settings.encoder() {
        EncoderPreference::Fixed(kind) => {
            let named = (
                kind,
                requested.unwrap_or_else(|| best_codec_for(kind, report)),
            );
            match settings.unavailable_choice() {
                UnavailableChoice::Refuse => vec![named],
                // The named encoder first, so a machine that has it still uses
                // it, and the ranked list behind it so a machine that no longer
                // has it still records.
                UnavailableChoice::Substitute => std::iter::once(named)
                    .chain(ranked().into_iter().filter(|(other, _)| *other != kind))
                    .collect(),
            }
        }
        EncoderPreference::Automatic => ranked(),
    }
}

/// The most efficient codec this machine was *measured* to support on `kind`.
///
/// Falls back to H.264, which every encoder in the workspace can produce: a
/// codec nothing measured is a codec the driver did not register a hardware
/// encoder for, and asking for it anyway is how a recording fails at the
/// encoder instead of in the settings (`docs/encoder-capabilities.md`).
fn best_codec_for(kind: EncoderKind, report: &CapabilityReport) -> Codec {
    let measured = report
        .encoder(kind)
        .map(measured_codecs)
        .unwrap_or_default();

    Codec::EFFICIENCY_ORDER
        .into_iter()
        .find(|codec| measured.contains(codec))
        .unwrap_or(Codec::H264)
}

/// Opens one encoder family.
///
/// The whole of the dispatch, in one place, so that a new backend is one arm
/// rather than a search.
fn open_one(
    kind: EncoderKind,
    device: &GraphicsDevice,
    config: EncoderConfig,
) -> Result<Box<dyn VideoEncoder>, EncodeError> {
    Ok(match kind {
        EncoderKind::Nvenc => Box::new(NvencEncoder::open(device, config)?),
        EncoderKind::Amf => Box::new(AmfEncoder::open(device, config)?),
        EncoderKind::QuickSync => Box::new(QuickSyncEncoder::open(device, config)?),
        EncoderKind::Software => Box::new(SoftwareEncoder::open(device, config)?),
    })
}

/// How many bits a second a recording of this size and rate is given.
///
/// The rate itself rather than the [`RateControl`] wrapping it, because two
/// things need the answer: the encoder is configured with it, and a replay
/// buffer running alongside the recording sizes its memory ceiling from it
/// (`crate::replay`). Deriving it twice would be two answers to one question
/// (AGENTS.md section 55).
fn bitrate_for(size: (u32, u32), frame_rate: FrameRate) -> BitRate {
    let bits =
        f64::from(size.0) * f64::from(size.1) * frame_rate.as_f64() * BITS_PER_PIXEL_PER_FRAME;

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let clamped = bits.clamp(f64::from(MINIMUM_BITRATE), f64::from(MAXIMUM_BITRATE)) as u32;

    BitRate::bits_per_second(clamped)
        .unwrap_or_else(|| BitRate::megabits_per_second(MINIMUM_BITRATE / 1_000_000))
}

/// What the encoder should be told the incoming frames look like.
///
/// The two crates name the same layouts in enumerations of their own — neither
/// may depend on the other — and this is the conversion `clipped-encoder`'s own
/// documentation says belongs here (`crates/encoder/src/config.rs`).
fn surface_format(format: PixelFormat) -> Result<SurfaceFormat, SessionError> {
    match format {
        PixelFormat::Bgra8Unorm => Ok(SurfaceFormat::Bgra8Unorm),
        PixelFormat::Rgb10A2Unorm => Ok(SurfaceFormat::Rgb10A2Unorm),
        // No encoder here accepts it, and there is no HDR path to convert it
        // through yet (issue #99). Refusing by name beats a driver refusing by
        // number several seconds later.
        other => Err(SessionError::UnsupportedPixelFormat {
            format: other.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use clipped_encoder::{detect, EncoderObservations, SystemFacts};

    use super::*;
    use crate::settings::CaptureTargetSettings;

    /// A report of a machine with no display adapter at all, which is the one
    /// report that can be built anywhere.
    fn bare_machine() -> CapabilityReport {
        detect(&SystemFacts::new(Vec::new(), EncoderObservations::none()))
    }

    fn settings() -> RecordingSettings {
        RecordingSettings::new(
            CaptureTargetSettings::window(1, 1280, 720),
            PathBuf::from("out.mkv"),
        )
    }

    fn rate_of(width: u32, height: u32, fps: u32) -> u32 {
        // Through `RateControl` rather than reading the bitrate directly, so
        // that what these figures describe is still what the encoder is
        // configured with.
        match RateControl::constant(bitrate_for(
            (width, height),
            FrameRate::new(fps, 1).expect("a real rate"),
        )) {
            RateControl::Bitrate { average, .. } => average.as_bits_per_second(),
            other => panic!("expected a bitrate, got {other}"),
        }
    }

    #[test]
    fn a_bitrate_scales_with_the_picture_and_the_rate() {
        // 1280x720 at 30 is 4.1 Mbit/s; 2560x1440 at 60 is 33 Mbit/s.
        assert_eq!(rate_of(1280, 720, 30), 4_147_200);
        assert_eq!(rate_of(2560, 1440, 60), 33_177_600);
    }

    #[test]
    fn a_tiny_recording_still_gets_a_usable_bitrate() {
        // 128x128 at 1 fps works out at 2.5 kbit/s, which is not a video.
        assert_eq!(rate_of(128, 128, 1), MINIMUM_BITRATE);
    }

    #[test]
    fn an_enormous_recording_is_capped_rather_than_filling_the_disk() {
        assert_eq!(rate_of(7680, 4320, 144), MAXIMUM_BITRATE);
    }

    #[test]
    fn a_floating_point_frame_format_is_refused_by_name() {
        let error = surface_format(PixelFormat::Rgba16Float)
            .expect_err("no encoder here accepts half-float frames");
        assert!(
            error.to_string().contains("RGBA16 float"),
            "the refusal must name the format: {error}"
        );
    }

    #[test]
    fn the_ordinary_capture_format_is_accepted() {
        assert_eq!(
            surface_format(PixelFormat::Bgra8Unorm).expect("BGRA8 is what capture produces"),
            SurfaceFormat::Bgra8Unorm
        );
    }

    #[test]
    fn a_named_encoder_is_the_only_candidate() {
        // No fallback, deliberately: `--encoder nvenc` on a machine without one
        // has to fail rather than quietly encode on the CPU.
        let settings = settings()
            .with_encoder(EncoderPreference::Fixed(EncoderKind::Nvenc))
            .with_codec(CodecPreference::Fixed(Codec::Av1));

        assert_eq!(
            candidates(&settings, &bare_machine()),
            vec![(EncoderKind::Nvenc, Codec::Av1)]
        );
    }

    #[test]
    fn a_configured_encoder_is_tried_first_and_then_the_ones_this_machine_has() {
        // The ticket's second acceptance criterion. An encoder that came from a
        // settings file is a choice made once, possibly before this machine had
        // the graphics card it has now, and a game that launches with nobody
        // watching must not go unrecorded because of it. It is still tried
        // first, so a machine that does have it uses it.
        let report = bare_machine();
        let settings = settings()
            .with_encoder(EncoderPreference::Fixed(EncoderKind::Nvenc))
            .with_unavailable_choice(UnavailableChoice::Substitute);

        let offered: Vec<EncoderKind> = candidates(&settings, &report)
            .into_iter()
            .map(|(kind, _)| kind)
            .collect();

        assert_eq!(
            offered.first(),
            Some(&EncoderKind::Nvenc),
            "the configured encoder must still be the first thing tried: {offered:?}"
        );
        assert!(
            offered.contains(&EncoderKind::Software),
            "a machine with nothing else must still record: {offered:?}"
        );
        let mut once = offered.clone();
        once.sort_unstable();
        once.dedup();
        assert_eq!(
            once.len(),
            offered.len(),
            "an encoder that has already refused must not be tried again: {offered:?}"
        );
    }

    #[test]
    fn a_named_encoder_with_no_measured_codec_is_asked_for_the_one_everything_produces() {
        // Nothing was measured on a machine with no adapters, and asking for
        // AV1 anyway would fail at the encoder rather than in the settings.
        let settings = settings().with_encoder(EncoderPreference::Fixed(EncoderKind::Software));

        assert_eq!(
            candidates(&settings, &bare_machine()),
            vec![(EncoderKind::Software, Codec::H264)]
        );
    }

    #[test]
    fn an_automatic_encoder_offers_every_ranked_candidate_in_order() {
        // The fallback the module documentation promises: the list is what
        // `recommend` ranked, in the same order, so that an encoder that will
        // not open is followed by the next one.
        let report = bare_machine();
        let ranked: Vec<EncoderKind> = recommend(&report)
            .iter()
            .map(clipped_encoder::Recommendation::encoder)
            .collect();
        assert!(!ranked.is_empty(), "there is always the software fallback");

        let offered: Vec<EncoderKind> = candidates(&settings(), &report)
            .into_iter()
            .map(|(kind, _)| kind)
            .collect();
        assert_eq!(offered, ranked);
    }

    #[test]
    fn a_named_codec_is_asked_of_every_candidate() {
        // Otherwise a fallback would silently change the codec as well as the
        // encoder, and a recording made with `--codec hevc` would be H.264.
        let settings = settings().with_codec(CodecPreference::Fixed(Codec::Hevc));
        for (_, codec) in candidates(&settings, &bare_machine()) {
            assert_eq!(codec, Codec::Hevc);
        }
    }

    /// A Direct3D 11 device, made directly rather than out of a captured frame.
    ///
    /// Capture needs a display that is scanning something out
    /// (`clipped_capture`, issue #461); opening a device does not. Making one
    /// here is what lets the substitution test below run on a machine whose
    /// screen has gone to sleep.
    fn a_device() -> Option<windows::Win32::Graphics::Direct3D11::ID3D11Device> {
        use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
        use windows::Win32::Graphics::Direct3D11::{
            D3D11CreateDevice, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
        };

        let mut device = None;
        // SAFETY: no adapter is named, so a driver type must be; the module
        // handle is null as that requires, and the out parameter is a live
        // local of the projected type.
        let created = unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                windows::Win32::Foundation::HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&raw mut device),
                None,
                None,
            )
        };
        created.ok().and(device)
    }

    #[test]
    fn an_encoder_this_machine_does_not_have_records_with_another_one_rather_than_not_at_all() {
        // The second acceptance criterion of
        // [issue #61](https://github.com/wildware-uk/clipped/issues/61), end to
        // end rather than as a candidate list. `a_configured_encoder_is_tried_first…`
        // above proves the ranking; this proves that `open` actually falls back
        // when the configured encoder is not there, which is the part a user
        // meets.
        //
        // A settings file naming an encoder is a choice made once, possibly
        // before this machine had the graphics card it has now, and a game that
        // launches with nobody watching must not go unrecorded because of it.
        use clipped_encoder::DeviceKind;
        use windows::core::Interface as _;

        let Some(device) = a_device() else {
            note("this machine would not create a Direct3D 11 device");
            return;
        };
        // SAFETY: `device` is live for the whole of this test, and the handle
        // is borrowed rather than owned, so it does not outlive it.
        let graphics = unsafe { GraphicsDevice::new(DeviceKind::D3d11, device.as_raw()) };

        // Every encoder kind in turn: whichever ones this machine lacks are the
        // ones worth asking for, and a machine that has them all simply has
        // nothing to prove here.
        let mut substituted = 0;
        for kind in [EncoderKind::Nvenc, EncoderKind::Amf, EncoderKind::QuickSync] {
            let settings = settings()
                .with_encoder(EncoderPreference::Fixed(kind))
                .with_unavailable_choice(UnavailableChoice::Substitute);

            let Ok(opened) = open(&graphics, &settings, (1280, 720), PixelFormat::Bgra8Unorm)
            else {
                // Nothing at all opened, which is a machine with no encoder
                // rather than a failure to substitute.
                continue;
            };
            if opened.kind != kind {
                substituted += 1;
                note(&format!(
                    "{} is not on this machine and it recorded with {} instead",
                    kind.log_encoder_family(),
                    opened.kind.log_encoder_family()
                ));

                // The half that stops this passing for the wrong reason. If the
                // same request refuses to substitute, it has to fail — otherwise
                // the encoder was available all along and nothing was
                // substituted.
                let refusing = settings.with_unavailable_choice(UnavailableChoice::Refuse);
                assert!(
                    open(&graphics, &refusing, (1280, 720), PixelFormat::Bgra8Unorm).is_err(),
                    "{} opened when it was told not to substitute, so the fallback above                      proved nothing",
                    kind.log_encoder_family()
                );
            }
        }

        if substituted == 0 {
            note("every encoder asked for is present here, so nothing had to be substituted");
        }
    }

    /// Says something a person reading the test output would want to know.
    fn note(message: &str) {
        println!("note: {message}");
    }
}
