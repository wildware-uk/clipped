//! Tests for the software encoder, including ones that encode real video.
//!
//! # What runs where
//!
//! Nearly all of it runs anywhere Windows does, which is the difference between
//! this backend and the NVENC one. A software encoder needs no encoding
//! hardware, and the only Direct3D it needs is a device to hold a texture — so
//! when there is no usable GPU, these tests fall back to WARP, the software
//! rasteriser that ships with Windows, and keep going. That means the encoder
//! that a user with no GPU will be recording with is the one covered on every
//! CI run rather than only on a maintainer's machine.
//!
//! What it cannot fall back on is FFmpeg, and "a missing FFmpeg" is three
//! different things here, only one of which is a skip:
//!
//! - **The `ffmpeg` and `ffprobe` executables**, which is how a stream is
//!   checked to be a stream, are looked for on `PATH` and beside the test
//!   binary. Missing, they are a skip that says so on standard error, and
//!   `CLIPPED_REQUIRE_ENCODER=1` turns that skip into a failure — so "the
//!   encoder tests passed" cannot quietly mean "the encoder tests did nothing".
//! - **The FFmpeg libraries** are linked, not loaded, so they are not a skip
//!   and cannot be: without `avcodec-62.dll` and its companions beside the test
//!   binary the process never starts, and Windows reports
//!   `STATUS_DLL_NOT_FOUND` before a test is named. In a build tree
//!   `crates/muxer/build.rs` puts them there
//!   ([#158](https://github.com/wildware-uk/clipped/issues/158), and
//!   `docs/encoder-pipeline.md`).
//! - **`libopenh264` inside those libraries.** ADR 0004 pins a build that
//!   carries it and `crates/muxer/tests/ffmpeg_linkage.rs` asserts it is still
//!   there, so a build without it is a broken pin rather than a machine these
//!   tests should tiptoe around: `open_encoder(...).expect(...)` fails the test
//!   with the encoder's own error. Skipping there would hide the one thing the
//!   ADR promises.
//!
//! # What "verified" means here
//!
//! A successful call is not a valid recording (AGENTS.md section 22). So the
//! stream is parsed in-process for the units a decoder needs, handed to
//! `ffprobe`, and — because "it decodes" and "it is the picture that went in"
//! are different claims — decoded back to pixels and checked for the moving
//! band and the colours that were submitted.

use core::ffi::c_void;
use core::time::Duration;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use windows::core::Interface as _;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP, D3D_FEATURE_LEVEL_11_0,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET,
    D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
    D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

use crate::backend::VideoEncoder;
use crate::claim::Evidence;
use crate::codec::{Codec, EncoderKind, Resolution, Vendor};
use crate::config::{
    BitRate, ColourSpace, EncoderConfig, FrameRate, KeyframeInterval, QualityTarget, RateControl,
    SurfaceFormat,
};
use crate::detection::detect;
use crate::error::{EncodeError, EncodeErrorKind};
use crate::frame::{DeviceKind, GraphicsDevice, SourceFrame, SourceTexture, SurfaceKind};
use crate::probe::{EncoderObservations, RuntimeObservation, RuntimeOutcome, SystemFacts};
use crate::recommendation::{recommend, ChoiceReason};

use super::SoftwareEncoder;

/// The picture size the encoding tests use.
///
/// 720p rather than the 1440p the measurements are taken at: this runs on every
/// pull request, on a hosted runner, and a software encoder is slow enough that
/// the difference is minutes rather than seconds.
const TEST_SIZE: Resolution = Resolution::new(1280, 720);

/// How many frames each encoding test encodes.
const TEST_FRAMES: usize = 90;

/// How far the bright band moves between frames, in pixels.
///
/// Chosen so that the decoded band moves by a whole number of columns in the
/// reduced picture the motion test looks at: 40 pixels of 1280 is four columns
/// of 128.
const BAND_STEP: usize = 40;

/// How wide the bright band is, in pixels.
const BAND_WIDTH: usize = 64;

// ---------------------------------------------------------------------------
// Tests that need nothing at all
// ---------------------------------------------------------------------------

#[test]
fn an_odd_picture_size_is_refused_with_a_reason() {
    let error = open_expecting_failure(config_for(Codec::H264, Resolution::new(1919, 1080)));

    assert!(
        matches!(error.kind(), EncodeErrorKind::Configuration { .. }),
        "{error}"
    );
    assert!(
        error.to_string().contains("odd dimension"),
        "the message does not say what is wrong: {error}"
    );
    assert!(
        error
            .to_string()
            .starts_with("Software (CPU) could not encode 1919x1080 H.264"),
        "{error}"
    );
}

#[test]
fn a_codec_this_backend_does_not_produce_is_refused_before_anything_is_opened() {
    // The capability report claims H.264 and only H.264 for the software
    // encoder (`crate::reference`), so producing anything else here would make
    // the report a lie. HEVC and AV1 software encoding is issue #157.
    for codec in [Codec::Hevc, Codec::Av1] {
        let error = open_expecting_failure(config_for(codec, TEST_SIZE));
        assert!(
            matches!(error.kind(), EncodeErrorKind::CodecUnsupported),
            "{codec} should be refused by name, not with {error}"
        );
        assert!(
            error.to_string().contains(&codec.to_string()),
            "the message does not name the codec: {error}"
        );
    }
}

#[test]
fn a_null_device_is_refused_before_anything_is_loaded() {
    // SAFETY: the handle is never dereferenced — `open` rejects it before it
    // reaches Direct3D, which is what this test asserts.
    let device = unsafe { GraphicsDevice::new(DeviceKind::D3d11, core::ptr::null_mut()) };

    let error = SoftwareEncoder::open(&device, config_for(Codec::H264, TEST_SIZE))
        .expect_err("a null device is not a device");
    assert!(
        error.to_string().contains("null"),
        "the message does not say what is wrong: {error}"
    );
}

#[test]
fn a_surface_format_this_backend_cannot_read_is_refused_by_name() {
    let config = config_for(Codec::H264, TEST_SIZE).with_source_format(SurfaceFormat::Rgb10A2Unorm);
    let error = open_expecting_failure(config);

    assert!(
        matches!(
            error.kind(),
            EncodeErrorKind::SurfaceFormatUnsupported { .. }
        ),
        "{error}"
    );
    assert!(
        error.to_string().contains("BGRA8 unorm"),
        "the message does not say what would work: {error}"
    );
}

#[test]
fn the_software_encoder_is_the_answer_with_no_hardware_and_never_before_one_this_build_can_open() {
    // The two halves of "selectable when no hardware encoder is available, and
    // never chosen ahead of one this build can open". The second half is
    // narrower than "ahead of one that is available" on purpose: since #175 a
    // detected family with no proven backend ranks *below* this one, so NVENC
    // is used here rather than Quick Sync, which would rank the other way by
    // design. The ranking itself lives in `crate::recommendation`; this asserts
    // the property that makes this backend a *fallback*, from the crate that
    // implements the fallback.
    let no_hardware = detect(&SystemFacts::new(Vec::new(), EncoderObservations::none()));
    let first = recommend(&no_hardware)[0];
    assert_eq!(first.encoder(), EncoderKind::Software);
    assert_eq!(
        first.codec(),
        Codec::H264,
        "the codec this backend produces is the one Automatic resolves to"
    );
    assert_eq!(first.reason(), ChoiceReason::SoftwareFallback);
    assert_eq!(first.codec_evidence(), Evidence::Inferred);

    let with_nvidia = detect(&SystemFacts::new(
        vec![crate::adapter::Adapter::new(
            crate::adapter::AdapterId::from_luid(1, 0),
            "NVIDIA GeForce RTX 4090",
            Vendor::Nvidia,
            0x2684,
            24 * 1024 * 1024 * 1024,
            false,
        )],
        EncoderObservations::none().with_runtime(RuntimeObservation::new(
            EncoderKind::Nvenc,
            "nvEncodeAPI64.dll",
            RuntimeOutcome::Loaded,
        )),
    ));
    let ranked: Vec<EncoderKind> = recommend(&with_nvidia)
        .iter()
        .map(crate::recommendation::Recommendation::encoder)
        .collect();
    assert_eq!(
        ranked,
        vec![EncoderKind::Nvenc, EncoderKind::Software],
        "the CPU must never outrank encoding hardware this build has a proven backend for"
    );
}

// ---------------------------------------------------------------------------
// Tests that encode real video
// ---------------------------------------------------------------------------

#[test]
fn h264_output_is_a_decodable_stream() {
    let Some(gpu) = TestGpu::open() else {
        return;
    };

    let mut encoder = gpu
        .open_encoder(config_for(Codec::H264, TEST_SIZE))
        .expect("the software encoder opens");

    // Copied out before the loop: a packet borrows the encoder, so a caller
    // holding one cannot also ask the encoder anything — which is the borrow
    // discipline `VideoEncoder` is built around.
    let parameter_sets = encoder.parameter_sets().to_vec();
    assert!(
        !parameter_sets.is_empty(),
        "a muxer needs the parameter sets before the first frame"
    );

    let mut stream = Vec::new();
    let mut packets = 0usize;
    let mut keyframes = Vec::new();
    let mut previous: Option<Duration> = None;
    let mut latencies = Vec::with_capacity(TEST_FRAMES);

    for index in 0..TEST_FRAMES {
        let texture = gpu.pattern_texture(index);
        let submitted = std::time::Instant::now();

        encoder
            .submit(&source_frame(&texture, index))
            .expect("the frame is submitted");

        while let Some(packet) = encoder.next_packet().expect("a packet is produced") {
            latencies.push(submitted.elapsed());
            assert!(
                !packet.data().is_empty(),
                "an empty packet is not a coded picture"
            );
            assert_eq!(
                packet.presentation_time(),
                packet.decode_time(),
                "libopenh264 reorders nothing, so nothing may be reordered here"
            );
            if let Some(previous) = previous {
                assert!(
                    packet.presentation_time() > previous,
                    "packet timestamps must increase: {:?} followed {previous:?}",
                    packet.presentation_time()
                );
            }
            previous = Some(packet.presentation_time());
            if packet.is_keyframe() {
                keyframes.push(packet.presentation_time());
                assert!(
                    packet.data().starts_with(&parameter_sets),
                    "a keyframe that does not carry the parameter sets is not a cut point: a \
                     decoder handed the clip that starts there cannot begin"
                );
            }
            packets += 1;
            stream.extend_from_slice(packet.data());
        }
    }

    encoder.finish().expect("the stream can be finished");
    while let Some(packet) = encoder.next_packet().expect("a packet is produced") {
        packets += 1;
        stream.extend_from_slice(packet.data());
    }

    assert_eq!(
        packets, TEST_FRAMES,
        "every submitted frame should produce exactly one packet"
    );
    // The interval is a *bound* on this encoder, not a spacing: `libopenh264`
    // inserts an IDR of its own whenever its scene-change detector fires, and
    // FFmpeg exposes no option to turn that off. What a replay buffer needs is
    // the bound — never more than an interval of recording between cut points
    // (SPEC.md section 7) — and that is what is asserted here. That the
    // configured interval is honoured exactly when nothing changes scene is
    // asserted by `keyframes_land_on_the_configured_interval`.
    assert_eq!(
        keyframes.first(),
        Some(&Duration::ZERO),
        "the first frame of a recording has to be a cut point"
    );
    for pair in keyframes.windows(2) {
        let gap = pair[1] - pair[0];
        assert!(
            gap <= Duration::from_secs(1),
            "{gap:?} of recording between two keyframes, with a one-second interval configured"
        );
    }
    check_structure(&stream);
    report_cost(&gpu, &encoder, &latencies, stream.len());

    let file = TempFile::new("clipped-software", "h264");
    std::fs::write(file.path(), &stream).expect("the stream can be written");
    probe(file.path());
}

#[test]
fn the_decoded_pictures_are_the_moving_picture_that_was_submitted() {
    // "It decodes" and "it is what went in" are different claims, and a
    // recorder that produced 90 copies of one frame would satisfy the first
    // (AGENTS.md section 22). The submitted pattern has a bright band that
    // moves a fixed distance every frame, so the decoded pictures have to show
    // it moving by that distance.
    let Some(gpu) = TestGpu::open() else {
        return;
    };
    let Some(ffmpeg) = tool("ffmpeg") else {
        skipped(
            "ffmpeg is not beside the pinned FFmpeg or on the path, so nothing can decode \
                 the result. Run scripts/fetch-ffmpeg.ps1.",
        );
        return;
    };

    let frames = 30;
    let mut encoder = gpu
        .open_encoder(config_for(Codec::H264, TEST_SIZE))
        .expect("the software encoder opens");

    let mut stream = Vec::new();
    for index in 0..frames {
        let texture = gpu.pattern_texture(index);
        encoder
            .submit(&source_frame(&texture, index))
            .expect("the frame is submitted");
        while let Some(packet) = encoder.next_packet().expect("a packet is produced") {
            stream.extend_from_slice(packet.data());
        }
    }
    encoder.finish().expect("the stream can be finished");
    while let Some(packet) = encoder.next_packet().expect("a packet is produced") {
        stream.extend_from_slice(packet.data());
    }
    drop(encoder);

    // One row from the middle of each picture, reduced to 128 columns of luma:
    // enough to find the band, small enough that the whole decoded sequence is
    // a few kilobytes.
    let columns = 128;
    let rows = decode_luma_rows(&ffmpeg, &stream, columns);
    assert_eq!(
        rows.len(),
        frames,
        "the decoder produced {} pictures for {frames} submitted frames",
        rows.len()
    );

    #[allow(clippy::cast_precision_loss)]
    let step = (BAND_STEP * columns) as f64 / f64::from(TEST_SIZE.width);
    let positions: Vec<f64> = rows.iter().map(|row| band_position(row)).collect();
    for (index, pair) in positions.windows(2).enumerate() {
        let moved = pair[1] - pair[0];
        assert!(
            (moved - step).abs() < 1.0,
            "the band moved {moved:.2} columns between decoded frames {index} and {}, not the \
             {step:.2} it moved between the frames that were submitted: {positions:?}",
            index + 1
        );
    }
}

#[test]
fn the_encoded_colour_survives_a_round_trip() {
    // The software path converts colour itself, in `super::convert`, and then
    // tells the stream what the samples mean. If the two disagree every
    // recording plays back washed out or oversaturated with nothing in any log
    // to say so (AGENTS.md section 22). Red, green and blue go in; red, green
    // and blue have to come back.
    let Some(gpu) = TestGpu::open() else {
        return;
    };
    let Some(ffmpeg) = tool("ffmpeg") else {
        skipped(
            "ffmpeg is not beside the pinned FFmpeg or on the path, so nothing can decode \
                 the result. Run scripts/fetch-ffmpeg.ps1.",
        );
        return;
    };

    let colours: [[u8; 3]; 3] = [[255, 0, 0], [0, 255, 0], [0, 0, 255]];
    let config = config_for(Codec::H264, TEST_SIZE)
        .with_colour_space(ColourSpace::BT709_LIMITED)
        // Every frame a keyframe, so each colour is coded from scratch and the
        // comparison is not measuring how well the encoder predicted.
        .with_keyframe_interval(KeyframeInterval::every(
            Duration::from_millis(1),
            FrameRate::FPS_60,
        ));
    let mut encoder = gpu
        .open_encoder(config)
        .expect("the software encoder opens");

    let mut stream = Vec::new();
    for (index, colour) in colours.iter().enumerate() {
        let texture = gpu.solid_texture(*colour);
        encoder
            .submit(&source_frame(&texture, index))
            .expect("the frame is submitted");
        while let Some(packet) = encoder.next_packet().expect("a packet is produced") {
            stream.extend_from_slice(packet.data());
        }
    }
    encoder.finish().expect("the stream can be finished");
    while let Some(packet) = encoder.next_packet().expect("a packet is produced") {
        stream.extend_from_slice(packet.data());
    }
    drop(encoder);

    let decoded = decode_middle_pixels(&ffmpeg, &stream, colours.len());
    for (index, expected) in colours.iter().enumerate() {
        for channel in 0..3 {
            let difference = i32::from(decoded[index][channel]) - i32::from(expected[channel]);
            assert!(
                difference.abs() <= 16,
                "frame {index} decoded as {:?} rather than {expected:?}: the colour description \
                 in the stream does not match the conversion that produced the samples",
                decoded[index]
            );
        }
    }
}

#[test]
fn a_texture_can_be_reused_the_moment_submit_returns() {
    // The contract every backend owes a capture backend: when `submit` returns,
    // the encoder holds nothing derived from the texture, so a frame pool may
    // put that surface straight back into rotation (see `crate::frame`).
    //
    // Unlike the hardware path, this one is not a timing question: the pixels
    // have been copied into system memory before `submit` returns, so a
    // backend that got this wrong here would fail rather than usually work.
    let Some(gpu) = TestGpu::open() else {
        return;
    };
    let Some(ffmpeg) = tool("ffmpeg") else {
        skipped(
            "ffmpeg is not beside the pinned FFmpeg or on the path, so nothing can decode \
                 the result. Run scripts/fetch-ffmpeg.ps1.",
        );
        return;
    };

    let colours: [[u8; 3]; 3] = [[255, 0, 0], [0, 255, 0], [0, 0, 255]];
    let config = config_for(Codec::H264, TEST_SIZE)
        .with_colour_space(ColourSpace::BT709_LIMITED)
        .with_keyframe_interval(KeyframeInterval::every(
            Duration::from_millis(1),
            FrameRate::FPS_60,
        ));
    let mut encoder = gpu
        .open_encoder(config)
        .expect("the software encoder opens");

    let surface = gpu.solid_texture(colours[0]);
    let mut stream = Vec::new();

    for (index, colour) in colours.iter().enumerate() {
        if index > 0 {
            gpu.overwrite(&surface, *colour);
        }
        encoder
            .submit(&source_frame(&surface, index))
            .expect("the frame is submitted");

        // Recycled immediately, before a single packet has been drained.
        gpu.overwrite(&surface, [40, 40, 40]);

        while let Some(packet) = encoder.next_packet().expect("a packet is produced") {
            stream.extend_from_slice(packet.data());
        }
    }
    encoder.finish().expect("the stream can be finished");
    while let Some(packet) = encoder.next_packet().expect("a packet is produced") {
        stream.extend_from_slice(packet.data());
    }
    drop(encoder);

    let decoded = decode_middle_pixels(&ffmpeg, &stream, colours.len());
    for (index, expected) in colours.iter().enumerate() {
        for channel in 0..3 {
            let difference = i32::from(decoded[index][channel]) - i32::from(expected[channel]);
            assert!(
                difference.abs() <= 16,
                "frame {index} came back as {:?}, which is the picture that replaced it: the \
                 encoder was still reading the texture after `submit` returned",
                decoded[index]
            );
        }
    }
}

#[test]
fn a_forced_keyframe_arrives_where_it_was_asked_for() {
    let Some(gpu) = TestGpu::open() else {
        return;
    };

    // A keyframe interval far longer than the test, so the only keyframes are
    // the first one and the one asked for. This is what the replay buffer
    // depends on: a clip has to be able to start where the user pressed the
    // key, not at the next scheduled keyframe (SPEC.md section 7).
    let config = config_for(Codec::H264, TEST_SIZE).with_keyframe_interval(KeyframeInterval::Never);
    let mut encoder = gpu
        .open_encoder(config)
        .expect("the software encoder opens");

    // A still picture, so that the only keyframes are the ones this test asks
    // for: `libopenh264` inserts its own at a scene change, and a moving
    // pattern would make "where it was asked for" a claim about the encoder's
    // scene detector rather than about the request.
    let still = gpu.solid_texture([120, 130, 140]);
    let mut keyframes = Vec::new();
    for index in 0..12 {
        let frame = source_frame(&still, index);
        let frame = if index == 7 {
            frame.forcing_keyframe()
        } else {
            frame
        };

        encoder.submit(&frame).expect("the frame is submitted");
        while let Some(packet) = encoder.next_packet().expect("a packet is produced") {
            if packet.is_keyframe() {
                keyframes.push(packet.presentation_time());
            }
        }
    }

    assert_eq!(
        keyframes,
        [Duration::ZERO, frame_time(7)],
        "keyframes should be the first frame and the one that asked to be one"
    );
}

#[test]
fn keyframes_land_on_the_configured_interval() {
    // On a still picture, where nothing can trigger `libopenh264`'s own
    // scene-change IDRs, the interval that was configured is the interval that
    // arrives — which is what says the configuration reached the encoder at all
    // rather than the encoder happening to agree with it.
    let Some(gpu) = TestGpu::open() else {
        return;
    };

    let mut encoder = gpu
        .open_encoder(config_for(Codec::H264, TEST_SIZE))
        .expect("the software encoder opens");

    let still = gpu.solid_texture([120, 130, 140]);
    let mut keyframes = Vec::new();
    for index in 0..TEST_FRAMES {
        encoder
            .submit(&source_frame(&still, index))
            .expect("the frame is submitted");
        while let Some(packet) = encoder.next_packet().expect("a packet is produced") {
            if packet.is_keyframe() {
                keyframes.push(packet.presentation_time());
            }
        }
    }

    let expected: Vec<Duration> = (0..TEST_FRAMES).step_by(60).map(frame_time).collect();
    assert_eq!(
        keyframes, expected,
        "a one-second keyframe interval at 60 fps puts a keyframe every sixtieth frame over the \
         {TEST_FRAMES} submitted, and nowhere else"
    );
}

#[test]
fn a_timestamp_that_goes_backwards_is_refused() {
    let Some(gpu) = TestGpu::open() else {
        return;
    };

    let mut encoder = gpu
        .open_encoder(config_for(Codec::H264, TEST_SIZE))
        .expect("the software encoder opens");

    let texture = gpu.pattern_texture(0);
    encoder
        .submit(&source_frame(&texture, 4))
        .expect("the first frame is submitted");
    drain(&mut encoder);

    let error = encoder
        .submit(&source_frame(&texture, 1))
        .expect_err("a frame from the past is not encodable");

    assert!(
        matches!(error.kind(), EncodeErrorKind::TimestampWentBackwards { .. }),
        "{error}"
    );
}

#[test]
fn a_frame_of_the_wrong_size_is_refused_rather_than_encoded() {
    // Two checks in one: the size the caller declared, and the size of the
    // texture behind it. The second is what stops `CopyResource` — which
    // returns `void` — silently doing nothing and the encoder coding whatever
    // was in the staging texture last.
    let Some(gpu) = TestGpu::open() else {
        return;
    };

    let mut encoder = gpu
        .open_encoder(config_for(Codec::H264, TEST_SIZE))
        .expect("the software encoder opens");

    let texture = gpu.pattern_texture(0);
    // SAFETY: the texture outlives the frame, which is dropped inside this
    // function.
    let surface = unsafe { SourceTexture::new(SurfaceKind::D3d11Texture2D, texture.as_raw()) };
    let frame = SourceFrame::new(
        surface,
        SurfaceFormat::Bgra8Unorm,
        Resolution::new(640, 360),
        Duration::ZERO,
    );
    let error = encoder
        .submit(&frame)
        .expect_err("a frame of another size cannot be encoded by this session");
    assert!(
        error.to_string().contains("640x360"),
        "the message does not say what arrived: {error}"
    );

    let smaller = gpu.texture_of(Resolution::new(640, 360), &[0u8; 640 * 360 * 4]);
    // SAFETY: as above.
    let surface = unsafe { SourceTexture::new(SurfaceKind::D3d11Texture2D, smaller.as_raw()) };
    let lying = SourceFrame::new(
        surface,
        SurfaceFormat::Bgra8Unorm,
        TEST_SIZE,
        Duration::ZERO,
    );
    let error = encoder
        .submit(&lying)
        .expect_err("a texture of another size cannot be copied into the staging texture");
    assert!(
        error.to_string().contains("640x360"),
        "the message does not say what the texture actually is: {error}"
    );
}

#[test]
fn a_frame_whose_texture_is_the_wrong_shape_is_refused_rather_than_encoded() {
    // The size and the format are not the whole of what `CopyResource`
    // requires: it copies a whole resource, so a texture of exactly the right
    // size and format in a different shape — a mip chain, a texture array, a
    // multisampled surface — cannot be copied into the staging texture either.
    // The call returns `void`, so without this check the copy would do nothing
    // and the encoder would code whatever the staging texture held last, which
    // is the previous frame. That is the stale-picture failure the check exists
    // to make impossible, and it was not covered for these three shapes.
    let Some(gpu) = TestGpu::open() else {
        return;
    };

    let mut encoder = gpu
        .open_encoder(config_for(Codec::H264, TEST_SIZE))
        .expect("the software encoder opens");

    // A real frame first, so that the staging texture holds a picture: a check
    // that only ever sees an empty staging texture proves nothing about the
    // stale-frame case it is written for.
    let good = gpu.pattern_texture(0);
    encoder
        .submit(&source_frame(&good, 0))
        .expect("a frame of the session's shape is encoded");
    drain(&mut encoder);

    for (shape, mip_levels, array_size, samples, expected) in [
        ("a mip chain", 4, 1, 1, "mip levels"),
        ("a texture array", 1, 2, 1, "array of 2 slices"),
        ("a multisampled texture", 1, 1, 4, "multisampled"),
    ] {
        let Some(texture) = gpu.oddly_shaped_texture(mip_levels, array_size, samples) else {
            // Only the multisampled shape can fail here, and only on a device
            // that supports no 4x multisampling at this size. Said out loud
            // rather than passed over, because a shape that was never created
            // is a guard that was never exercised.
            skipped(&format!(
                "this Direct3D device will not create {shape} of the test size"
            ));
            continue;
        };

        // SAFETY: the texture outlives the frame, which is dropped inside this
        // loop body, and it was created on the device the encoder was opened
        // against.
        let surface = unsafe { SourceTexture::new(SurfaceKind::D3d11Texture2D, texture.as_raw()) };
        let frame = SourceFrame::new(surface, SurfaceFormat::Bgra8Unorm, TEST_SIZE, frame_time(1));

        let Err(error) = encoder.submit(&frame) else {
            panic!("{shape} must not be accepted as a frame: the copy would do nothing");
        };
        assert!(
            error.to_string().contains(expected),
            "the message for {shape} does not say what is wrong: {error}"
        );
    }
}

#[test]
fn a_session_can_be_shut_down_twice_and_used_no_further() {
    let Some(gpu) = TestGpu::open() else {
        return;
    };

    let mut encoder = gpu
        .open_encoder(config_for(Codec::H264, TEST_SIZE))
        .expect("the software encoder opens");

    let texture = gpu.pattern_texture(0);
    encoder
        .submit(&source_frame(&texture, 0))
        .expect("the frame is submitted");
    drain(&mut encoder);

    encoder.shut_down();
    // Idempotent by contract: `Drop` calls this too, so a second call has to be
    // harmless or every encoder would free its session twice.
    encoder.shut_down();

    let error = encoder
        .submit(&source_frame(&texture, 1))
        .expect_err("a shut-down session cannot encode");
    assert!(
        matches!(error.kind(), EncodeErrorKind::NotRunning),
        "{error}"
    );
}

#[test]
fn many_sessions_can_be_opened_and_closed_in_turn() {
    // A recorder opens and closes an encoder for every recording, and a machine
    // that runs for days does that thousands of times. Each of these owns a
    // codec context, a picture, a scaler and a staging texture — several
    // megabytes of it at this size — so a leak here is a process that grows
    // until it is killed (AGENTS.md sections 58 and 59).
    let Some(gpu) = TestGpu::open() else {
        return;
    };

    for round in 0..16 {
        let mut encoder = gpu
            .open_encoder(config_for(Codec::H264, TEST_SIZE))
            .unwrap_or_else(|error| panic!("session {round} could not be opened: {error}"));

        let texture = gpu.pattern_texture(round);
        encoder
            .submit(&source_frame(&texture, 0))
            .expect("the frame is submitted");
        drain(&mut encoder);
        // Dropped without `shut_down`, which is the path an unwind takes.
    }
}

#[test]
fn a_quality_target_is_encodable_even_though_the_encoder_has_no_such_mode() {
    // `libopenh264` targets a bit rate and nothing else, so `RateControl::
    // Quality` becomes one (see `super::avcodec`). What matters is that asking
    // for it produces a stream rather than a refusal.
    let Some(gpu) = TestGpu::open() else {
        return;
    };

    let config = EncoderConfig::new(
        Codec::H264,
        TEST_SIZE,
        FrameRate::FPS_60,
        RateControl::quality(QualityTarget::DEFAULT),
    );
    let mut encoder = gpu
        .open_encoder(config)
        .expect("a quality target is encodable");

    let mut bytes = 0;
    for index in 0..12 {
        let texture = gpu.pattern_texture(index);
        encoder
            .submit(&source_frame(&texture, index))
            .expect("the frame is submitted");
        while let Some(packet) = encoder.next_packet().expect("a packet is produced") {
            bytes += packet.data().len();
        }
    }
    assert!(bytes > 0, "a quality-targeted session coded nothing");
}

// ---------------------------------------------------------------------------
// Checking what came out
// ---------------------------------------------------------------------------

/// Parses the bitstream far enough to know a decoder could start on it.
///
/// Deliberately not a decode: this runs everywhere, including where there is no
/// FFmpeg on the path, and a stream with no parameter sets or no keyframe is
/// broken whatever a decoder would say about it.
fn check_structure(stream: &[u8]) {
    let units = annex_b_units(stream);
    assert!(
        units.len() >= 3,
        "the encoder produced {} NAL units, which is not a stream",
        units.len()
    );

    let types: Vec<u8> = units.iter().map(|unit| unit[0] & 0x1F).collect();
    assert!(
        types.contains(&7),
        "no sequence parameter set, so no decoder can start"
    );
    assert!(types.contains(&8), "no picture parameter set");
    assert!(types.contains(&5), "no IDR, so there is no cut point");
}

/// Splits an Annex B stream into its NAL units.
fn annex_b_units(stream: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut index = 0;
    while index + 3 < stream.len() {
        if stream[index] == 0 && stream[index + 1] == 0 && stream[index + 2] == 1 {
            starts.push(index + 3);
            index += 3;
        } else if index + 4 < stream.len()
            && stream[index] == 0
            && stream[index + 1] == 0
            && stream[index + 2] == 0
            && stream[index + 3] == 1
        {
            starts.push(index + 4);
            index += 4;
        } else {
            index += 1;
        }
    }

    starts
        .iter()
        .enumerate()
        .map(|(position, start)| {
            let end = starts.get(position + 1).map_or(stream.len(), |next| *next);
            &stream[*start..end]
        })
        .filter(|unit| !unit.is_empty())
        .collect()
}

/// Hands the file to `ffprobe` and asserts on what it says.
fn probe(path: &Path) {
    let Some(ffprobe) = tool("ffprobe") else {
        // The in-process parse checks a NAL type; it does not check that
        // anything can decode the stream, which is what issue #18 asks for. So
        // this is a skip of the acceptance criterion itself, not of an extra.
        skipped(
            "ffprobe is not beside the pinned FFmpeg or on the path, so nothing checked that the \
             stream is decodable. Run scripts/fetch-ffmpeg.ps1.",
        );
        return;
    };

    let output = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-count_frames",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name,profile,width,height,pix_fmt,color_space,color_range,nb_read_frames",
            "-of",
            "default=noprint_wrappers=1",
        ])
        .arg(path)
        .output()
        .expect("ffprobe runs");

    let report = String::from_utf8_lossy(&output.stdout);
    eprintln!("ffprobe {}:\n{report}", path.display());
    assert!(
        output.status.success(),
        "ffprobe could not read the stream: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    for expected in [
        "codec_name=h264".to_owned(),
        format!("width={}", TEST_SIZE.width),
        format!("height={}", TEST_SIZE.height),
        "pix_fmt=yuv420p".to_owned(),
        format!("nb_read_frames={TEST_FRAMES}"),
    ] {
        assert!(
            report.contains(&expected),
            "ffprobe does not report {expected}:\n{report}"
        );
    }
}

/// Decodes the stream and returns one reduced row of luma per frame.
fn decode_luma_rows(ffmpeg: &Path, stream: &[u8], columns: usize) -> Vec<Vec<u8>> {
    let file = TempFile::new("clipped-software-motion", "h264");
    std::fs::write(file.path(), stream).expect("the stream can be written");
    let decoded = TempFile::new("clipped-software-motion", "gray");

    let status = Command::new(ffmpeg)
        .args(["-y", "-v", "error", "-i"])
        .arg(file.path())
        .args([
            "-vf",
            // Converted to grey *before* the crop: `crop` rounds its height
            // down to a whole chroma sample, so cropping one row of a 4:2:0
            // picture asks for a zero-height picture and fails.
            &format!(
                "format=gray,crop={}:1:0:{},scale={columns}:1",
                TEST_SIZE.width,
                TEST_SIZE.height / 2
            ),
            "-pix_fmt",
            "gray",
            "-f",
            "rawvideo",
        ])
        .arg(decoded.path())
        .status()
        .expect("ffmpeg runs");
    assert!(status.success(), "ffmpeg could not decode the stream");

    let raw = std::fs::read(decoded.path()).expect("the decoded rows can be read");
    raw.chunks_exact(columns).map(<[u8]>::to_vec).collect()
}

/// Where the bright band is in a row, as a fractional column.
///
/// The centroid of everything brighter than the pattern's background rather
/// than the brightest column, because the band is several columns wide after
/// the reduction and its plateau has no single brightest sample — an argmax
/// there measures the rounding rather than the movement.
fn band_position(row: &[u8]) -> f64 {
    /// Above the background of the test pattern (90 at most) and below the
    /// band (235), with room for what a trip through 4:2:0 costs.
    const BAND: u8 = 160;

    let mut weight = 0.0;
    let mut moment = 0.0;
    for (column, value) in row.iter().enumerate() {
        let above = f64::from(value.saturating_sub(BAND));
        weight += above;
        #[allow(clippy::cast_precision_loss)]
        {
            moment += above * column as f64;
        }
    }
    assert!(
        weight > 0.0,
        "the decoded row has nothing in it brighter than the pattern's background, so the band \
         the encoder was given is not in the picture that came back: {row:?}"
    );
    moment / weight
}

/// Decodes a stream and returns the middle pixel of each frame as red, green,
/// blue.
fn decode_middle_pixels(ffmpeg: &Path, stream: &[u8], frames: usize) -> Vec<[u8; 3]> {
    let file = TempFile::new("clipped-software-colour", "h264");
    std::fs::write(file.path(), stream).expect("the stream can be written");
    let decoded = TempFile::new("clipped-software-colour", "rgb");

    let status = Command::new(ffmpeg)
        .args(["-y", "-v", "error", "-i"])
        .arg(file.path())
        .args(["-pix_fmt", "rgb24", "-f", "rawvideo"])
        .arg(decoded.path())
        .status()
        .expect("ffmpeg runs");
    assert!(status.success(), "ffmpeg could not decode the stream");

    let raw = std::fs::read(decoded.path()).expect("the decoded frames can be read");
    let frame_bytes = (TEST_SIZE.width as usize) * (TEST_SIZE.height as usize) * 3;
    assert_eq!(
        raw.len(),
        frame_bytes * frames,
        "the decoder did not produce one frame per submitted frame"
    );

    (0..frames)
        .map(|index| {
            let pixel = index * frame_bytes
                + ((TEST_SIZE.height as usize / 2) * TEST_SIZE.width as usize
                    + TEST_SIZE.width as usize / 2)
                    * 3;
            [raw[pixel], raw[pixel + 1], raw[pixel + 2]]
        })
        .collect()
}

/// Prints what the three stages cost, which is what issue #18 asks to be
/// measured.
///
/// Printed rather than asserted on: a threshold here would be a test that fails
/// on somebody else's slower machine (AGENTS.md section 25). Run with
/// `cargo test -- --nocapture` to see it.
fn report_cost(gpu: &TestGpu, encoder: &SoftwareEncoder, latencies: &[Duration], bytes: usize) {
    let Some(session) = encoder.session.as_ref() else {
        return;
    };
    let (readback, frames) = session.readback.elapsed();
    let converted = session.converter.elapsed();
    let (encoded, _) = session.codec.elapsed();
    if frames == 0 || latencies.is_empty() {
        return;
    }

    let mut sorted: Vec<Duration> = latencies.to_vec();
    sorted.sort_unstable();
    let total: Duration = sorted.iter().sum();
    #[allow(clippy::cast_precision_loss)]
    let per_frame = |duration: Duration| duration.as_secs_f64() * 1000.0 / frames as f64;
    let mean = per_frame(total);

    eprintln!(
        "software H.264 {TEST_SIZE} on {}: {frames} frames, submit-to-packet mean {mean:.2} ms \
         ({:.0} frames a second), of which readback {:.2} ms, colour conversion {:.2} ms, \
         libopenh264 {:.2} ms; median {:.2} ms, p95 {:.2} ms, worst {:.2} ms, {} kB of bitstream",
        gpu.driver,
        1000.0 / mean,
        per_frame(readback),
        per_frame(converted),
        per_frame(encoded),
        sorted[sorted.len() / 2].as_secs_f64() * 1000.0,
        sorted[sorted.len() * 95 / 100].as_secs_f64() * 1000.0,
        sorted.last().expect("the list is not empty").as_secs_f64() * 1000.0,
        bytes / 1024,
    );
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// The configuration the tests encode with.
///
/// A one-second keyframe interval rather than the default two, so that
/// [`TEST_FRAMES`] frames contain more than one keyframe and the interval is
/// something a test can assert on rather than take on trust.
fn config_for(codec: Codec, resolution: Resolution) -> EncoderConfig {
    EncoderConfig::new(
        codec,
        resolution,
        FrameRate::FPS_60,
        RateControl::constant(BitRate::megabits_per_second(20)),
    )
    .with_keyframe_interval(KeyframeInterval::every(
        Duration::from_secs(1),
        FrameRate::FPS_60,
    ))
}

/// Opens an encoder that is expected to fail before it touches Direct3D.
fn open_expecting_failure(config: EncoderConfig) -> EncodeError {
    // SAFETY: the handle is never dereferenced: `open` validates the
    // configuration before it looks at the device.
    let device =
        unsafe { GraphicsDevice::new(DeviceKind::D3d11, std::ptr::dangling_mut::<c_void>()) };
    SoftwareEncoder::open(&device, config).expect_err("this configuration cannot be encoded")
}

/// When a frame sits in the recording, at 60 frames a second.
fn frame_time(index: usize) -> Duration {
    Duration::from_nanos((index as u64) * 1_000_000_000 / 60)
}

/// Builds the frame the encoder is given.
fn source_frame<'texture>(
    texture: &'texture ID3D11Texture2D,
    index: usize,
) -> SourceFrame<'texture> {
    // SAFETY: the texture is a live `ID3D11Texture2D` on the device the encoder
    // was opened against, and the borrow ties the frame's lifetime to it, so it
    // cannot outlive the texture.
    let surface = unsafe { SourceTexture::new(SurfaceKind::D3d11Texture2D, texture.as_raw()) };
    SourceFrame::new(
        surface,
        SurfaceFormat::Bgra8Unorm,
        TEST_SIZE,
        frame_time(index),
    )
}

/// Drains whatever the encoder has ready, discarding it.
fn drain(encoder: &mut SoftwareEncoder) {
    while encoder
        .next_packet()
        .expect("a packet is produced")
        .is_some()
    {}
}

/// Looks for a development tool, beside the pinned FFmpeg first and then on the
/// path.
fn tool(name: &str) -> Option<PathBuf> {
    let file = format!("{name}{}", if cfg!(windows) { ".exe" } else { "" });

    let bundled = std::env::var_os("FFMPEG_DIR")
        .map(|directory| PathBuf::from(directory).join("bin").join(&file))
        .filter(|candidate| candidate.is_file());
    if bundled.is_some() {
        return bundled;
    }

    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|directory| directory.join(&file))
            .find(|candidate| candidate.is_file())
    })
}

/// Reports that a test could not run here.
///
/// Panics instead of skipping when `CLIPPED_REQUIRE_ENCODER` is set, so a
/// machine that is meant to exercise the encoder cannot quietly stop doing it.
/// Writes through `std::io::stderr()` rather than `eprintln!` because libtest
/// captures the macro: a skip printed with `eprintln!` is invisible in a
/// passing run, which is exactly the failure mode this guards against.
fn skipped(reason: &str) {
    assert!(
        std::env::var_os(REQUIRE_ENCODER).is_none_or(|value| value.is_empty()),
        "{REQUIRE_ENCODER} is set, so this must not be skipped: {reason}"
    );
    let _ = writeln!(std::io::stderr(), "SKIPPED (encoder): {reason}");
}

/// The environment variable that turns "this machine could not run the test"
/// from a pass into a failure.
const REQUIRE_ENCODER: &str = "CLIPPED_REQUIRE_ENCODER";

/// A Direct3D 11 device and the textures the tests feed through it.
///
/// # Ownership
///
/// Owns the device and every texture it creates; both are reference-counted COM
/// interfaces released when this is dropped, which happens after the encoder
/// under test — the encoder is created from it and dropped first in every test.
struct TestGpu {
    device: ID3D11Device,
    /// Which driver the device is on, for the measurement line: a readback from
    /// WARP is a copy inside system memory and a readback from a graphics card
    /// crosses the bus, so a number is meaningless without this.
    driver: &'static str,
}

impl TestGpu {
    /// Opens a device on the graphics hardware, falling back to WARP.
    ///
    /// A software encoder needs no encoding hardware, so unlike the NVENC tests
    /// there is nothing here to skip for: WARP is part of Windows. The fallback
    /// is what lets these tests cover the fallback encoder on a hosted runner.
    fn open() -> Option<Self> {
        for (driver, kind) in [
            ("graphics hardware", D3D_DRIVER_TYPE_HARDWARE),
            ("WARP", D3D_DRIVER_TYPE_WARP),
        ] {
            if let Some(device) = Self::try_open(kind) {
                return Some(Self { device, driver });
            }
        }

        skipped("no Direct3D 11 device could be created, on hardware or on WARP");
        None
    }

    fn try_open(kind: D3D_DRIVER_TYPE) -> Option<ID3D11Device> {
        let mut device: Option<ID3D11Device> = None;
        // SAFETY: no adapter is named, which is what the driver types used here
        // require; the module handle is unused for them; the feature level list
        // and the out-parameter are live locals; and `D3D11_SDK_VERSION` is the
        // constant the header requires. On success `device` holds one
        // reference, released on drop.
        unsafe {
            D3D11CreateDevice(
                None,
                kind,
                windows::Win32::Foundation::HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
        }
        .ok()?;
        device
    }

    /// Opens an encoder against this device.
    fn open_encoder(&self, config: EncoderConfig) -> Result<SoftwareEncoder, EncodeError> {
        // SAFETY: the device is alive for as long as `self` is, and every
        // encoder opened from it is dropped inside the test that opened it.
        let device = unsafe { GraphicsDevice::new(DeviceKind::D3d11, self.device.as_raw()) };
        SoftwareEncoder::open(&device, config)
    }

    /// A texture holding the moving test pattern for frame `index`.
    ///
    /// A scrolling chequerboard, so that the encoder has real detail to code
    /// rather than a flat picture it can compress to nothing, and one bright
    /// band whose position is [`BAND_STEP`] pixels further along every frame,
    /// so that a decoded picture can be told from its neighbours by more than
    /// eye.
    fn pattern_texture(&self, index: usize) -> ID3D11Texture2D {
        let width = TEST_SIZE.width as usize;
        let height = TEST_SIZE.height as usize;
        let mut pixels = vec![0u8; width * height * 4];
        // Clear of the left edge at frame zero, so the whole band is in the
        // picture from the first frame: the motion test measures where its
        // middle is, and half a band would put that in the wrong place.
        let band = (BAND_WIDTH + index * BAND_STEP) % width;

        for y in 0..height {
            for x in 0..width {
                let at = (y * width + x) * 4;
                let checker = ((x + index * 4) / 8 + y / 8) % 2 == 0;
                let value = if x.abs_diff(band) < BAND_WIDTH / 2 {
                    235
                } else if checker {
                    90
                } else {
                    40
                };
                // BGRA, which is what `DXGI_FORMAT_B8G8R8A8_UNORM` stores. Grey
                // rather than coloured, so the luma the motion test looks at is
                // the value written here.
                pixels[at] = value;
                pixels[at + 1] = value;
                pixels[at + 2] = value;
                pixels[at + 3] = 255;
            }
        }

        self.texture_of(TEST_SIZE, &pixels)
    }

    /// A texture of one solid colour, given as red, green, blue.
    fn solid_texture(&self, colour: [u8; 3]) -> ID3D11Texture2D {
        self.texture_of(TEST_SIZE, &solid_pixels(colour))
    }

    /// Overwrites a texture in place, the way a capture frame pool overwrites a
    /// recycled surface.
    fn overwrite(&self, texture: &ID3D11Texture2D, colour: [u8; 3]) {
        let pixels = solid_pixels(colour);

        // SAFETY: the device is live and the immediate context it hands back is
        // released when the local goes out of scope.
        let context = unsafe { self.device.GetImmediateContext() }
            .expect("a device has an immediate context");

        // SAFETY: `texture` was created by this device with `MipLevels: 1` and
        // `ArraySize: 1`, so subresource 0 is the whole of it; the box is null,
        // meaning the whole resource; and `pixels` holds `Width * Height * 4`
        // bytes, which is what the row pitch declares.
        unsafe {
            context.UpdateSubresource(
                texture,
                0,
                None,
                pixels.as_ptr().cast::<c_void>(),
                TEST_SIZE.width * 4,
                0,
            );
            // Make the write reach the GPU now rather than whenever the driver
            // next flushes: the point of the test is that the encoder has
            // finished with the surface before this happens.
            context.Flush();
        }
    }

    /// A texture of the session's size and format but not its *shape*.
    ///
    /// `CopyResource` copies whole resources, so a mip chain, a texture array
    /// or a multisampled surface cannot be copied into the staging texture even
    /// when its first slice is the right picture. No initial data is supplied:
    /// a multi-subresource texture needs one entry per subresource, and nothing
    /// here reads the pixels — the copy is meant to be refused before it
    /// happens.
    ///
    /// `None` when the device will not create that shape, which is only a
    /// possibility for the multisampled one.
    fn oddly_shaped_texture(
        &self,
        mip_levels: u32,
        array_size: u32,
        samples: u32,
    ) -> Option<ID3D11Texture2D> {
        let description = D3D11_TEXTURE2D_DESC {
            Width: TEST_SIZE.width,
            Height: TEST_SIZE.height,
            MipLevels: mip_levels,
            ArraySize: array_size,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: samples,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            #[allow(clippy::cast_sign_loss)]
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };

        let mut texture: Option<ID3D11Texture2D> = None;
        // SAFETY: the description is a live local, no initial data is supplied
        // — which `CreateTexture2D` allows for a default-usage texture — and
        // `texture` is a live out-parameter receiving one reference.
        unsafe {
            self.device
                .CreateTexture2D(&description, None, Some(&mut texture))
        }
        .ok()?;
        texture
    }

    /// Uploads pixels into a texture the encoder can read.
    fn texture_of(&self, resolution: Resolution, pixels: &[u8]) -> ID3D11Texture2D {
        let description = D3D11_TEXTURE2D_DESC {
            Width: resolution.width,
            Height: resolution.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            #[allow(clippy::cast_sign_loss)]
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let initial = D3D11_SUBRESOURCE_DATA {
            pSysMem: pixels.as_ptr().cast::<c_void>(),
            SysMemPitch: resolution.width * 4,
            SysMemSlicePitch: 0,
        };

        let mut texture: Option<ID3D11Texture2D> = None;
        // SAFETY: the description and the initial data are live locals, the
        // pixel buffer is `Width * Height * 4` bytes as `SysMemPitch` declares,
        // and `texture` is a live out-parameter.
        unsafe {
            self.device
                .CreateTexture2D(&description, Some(&initial), Some(&mut texture))
        }
        .expect("a texture can be created");

        texture.expect("CreateTexture2D reported success without a texture")
    }
}

/// One solid colour, given as red, green, blue, as the BGRA bytes
/// `DXGI_FORMAT_B8G8R8A8_UNORM` stores.
fn solid_pixels(colour: [u8; 3]) -> Vec<u8> {
    let mut pixels = vec![0u8; (TEST_SIZE.width as usize) * (TEST_SIZE.height as usize) * 4];
    for pixel in pixels.chunks_exact_mut(4) {
        pixel[0] = colour[2];
        pixel[1] = colour[1];
        pixel[2] = colour[0];
        pixel[3] = 255;
    }
    pixels
}

/// A file in the temporary directory that deletes itself.
struct TempFile {
    path: PathBuf,
}

impl TempFile {
    fn new(prefix: &str, extension: &str) -> Self {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "{prefix}-{}-{unique}.{extension}",
            std::process::id()
        ));
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
