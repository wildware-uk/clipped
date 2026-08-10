//! Tests for the NVENC backend, including ones that encode real video.
//!
//! # What runs where
//!
//! The tests that need NVIDIA hardware skip on a machine that has none, which
//! is every hosted CI runner (`.github/workflows/ci.yml`). Skipping silently
//! would make "the encoder tests passed" and "the encoder tests ran" the same
//! sentence, so each skip says why, and setting `CLIPPED_REQUIRE_ENCODER=1`
//! turns a skip into a failure — the same lever `clipped-capture` uses for
//! capture, and the one used to produce the evidence on issue #15.
//!
//! # What "verified" means here
//!
//! A successful call is not a valid recording (AGENTS.md section 22). The
//! hardware tests therefore parse what came out — Annex B for H.264 and HEVC,
//! OBUs for AV1 — and, where `ffprobe` is on the path, hand the file to it and
//! assert on what it reports. `ffprobe` is a development tool here and nothing
//! else: nothing in the recorder shells out to FFmpeg.

use core::ffi::c_void;
use core::time::Duration;
use std::path::{Path, PathBuf};
use std::process::Command;

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET,
    D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
    D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory1};

use crate::backend::VideoEncoder;
use crate::codec::{Codec, EncoderKind, Resolution, Vendor};
use crate::config::{
    BitRate, ColourSpace, EncoderConfig, FrameRate, KeyframeInterval, RateControl, SurfaceFormat,
};
use crate::error::{EncodeError, EncodeErrorKind};
use crate::frame::{DeviceKind, GraphicsDevice, SourceFrame, SourceTexture, SurfaceKind};

use super::NvencEncoder;

/// The picture size the hardware tests encode at. Large enough to be a real
/// encode rather than a toy one, and small enough that three codecs' worth of
/// it does not make the suite slow.
const TEST_SIZE: Resolution = Resolution::new(1280, 720);

/// How many frames each hardware test encodes.
const TEST_FRAMES: usize = 90;

// ---------------------------------------------------------------------------
// Tests that need no hardware
// ---------------------------------------------------------------------------

#[test]
fn an_odd_picture_size_is_refused_with_a_reason() {
    // 4:2:0 chroma has no representation for an odd dimension. NVENC would
    // refuse it too, with NV_ENC_ERR_INVALID_PARAM and no mention of which
    // parameter.
    let error = open_expecting_failure(Resolution::new(1919, 1080), Codec::H264);

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
            .starts_with("NVIDIA NVENC could not encode 1919x1080 H.264"),
        "{error}"
    );
}

#[test]
fn a_device_of_the_wrong_kind_is_refused_before_anything_is_loaded() {
    let config = config_for(Codec::H264, TEST_SIZE);
    // SAFETY: the handle is never dereferenced — `open` rejects the device kind
    // before it reaches the runtime, which is what this test asserts.
    let device = unsafe { GraphicsDevice::new(DeviceKind::D3d11, core::ptr::null_mut()) };

    let error = NvencEncoder::open(&device, config).expect_err("a null device is not a device");
    assert!(
        error.to_string().contains("null"),
        "the message does not say what is wrong: {error}"
    );
}

#[test]
fn a_surface_format_this_backend_cannot_bind_is_refused_by_name() {
    let config = config_for(Codec::H264, TEST_SIZE).with_source_format(SurfaceFormat::Rgb10A2Unorm);
    // SAFETY: never dereferenced; the format check happens first. A dangling
    // pointer rather than null, because null is what the previous test rejects
    // and this one has to get past that check to reach the format one.
    let device =
        unsafe { GraphicsDevice::new(DeviceKind::D3d11, std::ptr::dangling_mut::<c_void>()) };

    let error = NvencEncoder::open(&device, config).expect_err("10-bit input is not supported yet");
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

// ---------------------------------------------------------------------------
// Tests that encode real video
// ---------------------------------------------------------------------------

#[test]
fn h264_output_is_a_decodable_stream() {
    encode_and_verify(Codec::H264);
}

#[test]
fn hevc_output_is_a_decodable_stream() {
    encode_and_verify(Codec::Hevc);
}

#[test]
fn av1_output_is_a_decodable_stream() {
    encode_and_verify(Codec::Av1);
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
    let mut encoder = gpu.open_encoder(config).expect("NVENC encodes H.264");

    let mut keyframes = Vec::new();
    for index in 0..12 {
        let texture = gpu.pattern_texture(index);
        let frame = source_frame(&texture, index);
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

    let expected = [Duration::ZERO, frame_time(7)];
    assert_eq!(
        keyframes, expected,
        "keyframes should be the first frame and the one that asked to be one"
    );
}

#[test]
fn a_timestamp_that_goes_backwards_is_refused() {
    let Some(gpu) = TestGpu::open() else {
        return;
    };

    let mut encoder = gpu
        .open_encoder(config_for(Codec::H264, TEST_SIZE))
        .expect("NVENC encodes H.264");

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
    let Some(gpu) = TestGpu::open() else {
        return;
    };

    let mut encoder = gpu
        .open_encoder(config_for(Codec::H264, TEST_SIZE))
        .expect("NVENC encodes H.264");

    let texture = gpu.pattern_texture(0);
    // SAFETY: the texture outlives the frame, which is dropped inside this
    // function.
    let surface = unsafe { SourceTexture::new(SurfaceKind::D3d11Texture2D, texture_ptr(&texture)) };
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
}

#[test]
fn a_session_can_be_shut_down_twice_and_used_no_further() {
    let Some(gpu) = TestGpu::open() else {
        return;
    };

    let mut encoder = gpu
        .open_encoder(config_for(Codec::H264, TEST_SIZE))
        .expect("NVENC encodes H.264");

    let texture = gpu.pattern_texture(0);
    encoder
        .submit(&source_frame(&texture, 0))
        .expect("the frame is submitted");
    drain(&mut encoder);

    encoder.shut_down();
    // Idempotent by contract: `Drop` calls this too, so a second call has to be
    // harmless or every encoder would double-free its session.
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
    // that runs for days does that thousands of times. A session that leaked
    // would be visible within a handful of iterations, because consumer cards
    // cap how many may exist at once (AGENTS.md sections 58 and 59).
    let Some(gpu) = TestGpu::open() else {
        return;
    };

    for round in 0..16 {
        let mut encoder = gpu
            .open_encoder(config_for(Codec::H264, TEST_SIZE))
            .unwrap_or_else(|error| {
                panic!("session {round} could not be opened, which means an earlier one leaked: {error}")
            });

        let texture = gpu.pattern_texture(round);
        encoder
            .submit(&source_frame(&texture, 0))
            .expect("the frame is submitted");
        drain(&mut encoder);
        // Dropped without `shut_down`, which is the path an unwind takes.
    }
}

#[test]
fn the_encoded_colour_survives_a_round_trip() {
    // The frames going in are red, green and blue; the frames coming out are
    // decoded back to red, green and blue. This is the end-to-end check that
    // the colour description written into the stream describes the conversion
    // NVENC actually performed — tag one thing and encode another, and every
    // recording comes out washed out or oversaturated with nothing in any log
    // to say so (AGENTS.md section 22).
    let Some(gpu) = TestGpu::open() else {
        return;
    };
    let Some(ffmpeg) = tool("ffmpeg") else {
        eprintln!("skipped: ffmpeg is not on the path, so nothing can decode the result");
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
    let mut encoder = gpu.open_encoder(config).expect("NVENC encodes H.264");

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

    let file = TempFile::new("clipped-colour", "h264");
    std::fs::write(file.path(), &stream).expect("the stream can be written");

    // Decode to raw RGB and read the first pixel of each frame back.
    let decoded = TempFile::new("clipped-colour", "rgb");
    let status = Command::new(&ffmpeg)
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
        frame_bytes * colours.len(),
        "the decoder did not produce one frame per submitted frame"
    );

    for (index, expected) in colours.iter().enumerate() {
        // The middle of the picture, away from any edge the codec may have
        // padded.
        let pixel = index * frame_bytes
            + ((TEST_SIZE.height as usize / 2) * TEST_SIZE.width as usize
                + TEST_SIZE.width as usize / 2)
                * 3;
        let got = [raw[pixel], raw[pixel + 1], raw[pixel + 2]];

        for channel in 0..3 {
            let difference = i32::from(got[channel]) - i32::from(expected[channel]);
            assert!(
                difference.abs() <= 12,
                "frame {index} decoded as {got:?} rather than {expected:?}: the colour \
                 description in the stream does not match the conversion the encoder performed"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The body of the codec tests
// ---------------------------------------------------------------------------

/// Encodes [`TEST_FRAMES`] frames of a moving pattern, checks the bitstream
/// structurally, and hands it to `ffprobe` where that is available.
fn encode_and_verify(codec: Codec) {
    let Some(gpu) = TestGpu::open() else {
        return;
    };

    let config = config_for(codec, TEST_SIZE);
    let mut encoder = match gpu.open_encoder(config) {
        Ok(encoder) => encoder,
        Err(error) if matches!(error.kind(), EncodeErrorKind::CodecUnsupported) => {
            // Not every NVIDIA GPU encodes every codec: AV1 arrived with Ada.
            // The report `recorder capabilities` prints says which, and this is
            // the same answer arriving through the encoder.
            eprintln!("skipped: this GPU does not encode {codec}");
            return;
        }
        Err(error) => panic!("{error}"),
    };

    assert!(
        !encoder.parameter_sets().is_empty(),
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
                "this encoder is configured without B-frames, so nothing is reordered"
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
    assert_eq!(
        keyframes,
        vec![frame_time(0), frame_time(60)],
        "a one-second keyframe interval over {TEST_FRAMES} frames at 60 fps puts a keyframe at \
         the first frame and at the sixtieth, and nowhere else"
    );
    check_structure(codec, &stream);

    report_latency(codec, &latencies, stream.len());

    let file = TempFile::new("clipped-nvenc", extension(codec));
    std::fs::write(file.path(), &stream).expect("the stream can be written");
    probe(codec, file.path());
}

/// Parses the bitstream far enough to know it is the codec it claims to be.
///
/// Deliberately not a decode: this runs on every machine with an NVIDIA GPU,
/// including ones with no FFmpeg, and a stream with no parameter sets or no
/// keyframe is broken whatever a decoder would say about it.
fn check_structure(codec: Codec, stream: &[u8]) {
    match codec {
        Codec::H264 | Codec::Hevc => {
            let units = annex_b_units(stream);
            assert!(
                units.len() >= 3,
                "{codec} produced {} NAL units, which is not a stream",
                units.len()
            );

            let types: Vec<u8> = units
                .iter()
                .map(|unit| match codec {
                    // H.264: five bits of type in the low bits of one header
                    // byte. HEVC: six bits, one bit up, in a two-byte header.
                    Codec::H264 => unit[0] & 0x1F,
                    _ => (unit[0] >> 1) & 0x3F,
                })
                .collect();

            let (sequence_header, keyframe): (&[u8], &[u8]) = match codec {
                Codec::H264 => (&[7], &[5]),
                _ => (&[33], &[19, 20]),
            };
            assert!(
                types.iter().any(|kind| sequence_header.contains(kind)),
                "{codec} produced no sequence parameter set, so no decoder can start"
            );
            assert!(
                types.iter().any(|kind| keyframe.contains(kind)),
                "{codec} produced no keyframe"
            );
        }
        Codec::Av1 => {
            // The low-overhead OBU stream AV1 uses: every unit starts with a
            // header byte whose top bit is zero and whose type is in bits 3-6.
            assert!(stream.len() > 16, "AV1 produced {} bytes", stream.len());
            assert_eq!(
                stream[0] & 0x80,
                0,
                "the first OBU header has its forbidden bit set, so this is not an OBU stream"
            );
            let first_type = (stream[0] >> 3) & 0x0F;
            assert!(
                // A temporal delimiter (2) or a sequence header (1).
                first_type == 1 || first_type == 2,
                "an AV1 stream starts with a temporal delimiter or a sequence header, not OBU \
                 type {first_type}"
            );
        }
    }
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

/// Hands the file to `ffprobe` and asserts on what it says, when `ffprobe` is
/// on the path.
fn probe(codec: Codec, path: &Path) {
    let Some(ffprobe) = tool("ffprobe") else {
        eprintln!(
            "skipped the ffprobe assertions: ffprobe is not on the path. The stream was still \
             parsed in-process."
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
            "stream=codec_name,width,height,pix_fmt,nb_read_frames",
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

    let expected_codec = match codec {
        Codec::H264 => "h264",
        Codec::Hevc => "hevc",
        Codec::Av1 => "av1",
    };
    assert!(
        report.contains(&format!("codec_name={expected_codec}")),
        "ffprobe reports a different codec:\n{report}"
    );
    assert!(
        report.contains(&format!("width={}", TEST_SIZE.width)),
        "ffprobe reports a different width:\n{report}"
    );
    assert!(
        report.contains(&format!("height={}", TEST_SIZE.height)),
        "ffprobe reports a different height:\n{report}"
    );
    assert!(
        report.contains("pix_fmt=yuv420p"),
        "ffprobe reports a pixel format this build does not produce:\n{report}"
    );
    assert!(
        report.contains(&format!("nb_read_frames={TEST_FRAMES}")),
        "ffprobe counted a different number of frames from the {TEST_FRAMES} submitted:\n{report}"
    );
}

/// Prints the encode latency, which issue #15 asks to be measured.
///
/// Printed rather than asserted on: a threshold here would be a test that fails
/// on somebody else's slower GPU (AGENTS.md section 25). Run with
/// `cargo test -- --nocapture` to see it.
fn report_latency(codec: Codec, latencies: &[Duration], bytes: usize) {
    if latencies.is_empty() {
        return;
    }

    let mut sorted: Vec<Duration> = latencies.to_vec();
    sorted.sort_unstable();
    let total: Duration = sorted.iter().sum();
    let mean = total / u32::try_from(sorted.len()).unwrap_or(1);
    let median = sorted[sorted.len() / 2];
    let p95 = sorted[sorted.len() * 95 / 100];
    let worst = *sorted.last().expect("the list is not empty");

    eprintln!(
        "{codec} {TEST_SIZE}: {} frames, submit-to-packet mean {:.2} ms, median {:.2} ms, \
         p95 {:.2} ms, worst {:.2} ms, {} kB of bitstream",
        sorted.len(),
        mean.as_secs_f64() * 1000.0,
        median.as_secs_f64() * 1000.0,
        p95.as_secs_f64() * 1000.0,
        worst.as_secs_f64() * 1000.0,
        bytes / 1024
    );
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// The configuration the tests encode with.
///
/// A one-second keyframe interval rather than the default two, so that
/// [`TEST_FRAMES`] frames contain more than one keyframe and the interval is
/// something the test can assert on rather than take on trust.
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

/// Opens an encoder that is expected to fail before it touches hardware.
fn open_expecting_failure(resolution: Resolution, codec: Codec) -> EncodeError {
    // SAFETY: the handle is never dereferenced: `open` validates the
    // configuration before it looks at the device.
    let device =
        unsafe { GraphicsDevice::new(DeviceKind::D3d11, std::ptr::dangling_mut::<c_void>()) };
    NvencEncoder::open(&device, config_for(codec, resolution))
        .expect_err("this configuration cannot be encoded")
}

/// When a frame sits in the recording, at 60 frames a second.
fn frame_time(index: usize) -> Duration {
    Duration::from_nanos((index as u64) * 1_000_000_000 / 60)
}

/// The raw pointer of a texture, for handing to the encoder.
fn texture_ptr(texture: &ID3D11Texture2D) -> *mut c_void {
    texture.as_raw()
}

/// Builds the frame the encoder is given.
fn source_frame<'texture>(
    texture: &'texture ID3D11Texture2D,
    index: usize,
) -> SourceFrame<'texture> {
    // SAFETY: the texture is a live `ID3D11Texture2D` on the device the encoder
    // was opened against, and the borrow ties the frame's lifetime to it, so it
    // cannot outlive the texture.
    let surface = unsafe { SourceTexture::new(SurfaceKind::D3d11Texture2D, texture_ptr(texture)) };
    SourceFrame::new(
        surface,
        SurfaceFormat::Bgra8Unorm,
        TEST_SIZE,
        frame_time(index),
    )
}

/// Drains whatever the encoder has ready, discarding it.
fn drain(encoder: &mut NvencEncoder) {
    while encoder
        .next_packet()
        .expect("a packet is produced")
        .is_some()
    {}
}

/// Looks for a development tool on the path.
fn tool(name: &str) -> Option<PathBuf> {
    let extension = if cfg!(windows) { ".exe" } else { "" };
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|directory| directory.join(format!("{name}{extension}")))
            .find(|candidate| candidate.is_file())
    })
}

/// A Direct3D 11 device on the NVIDIA adapter, and the textures the tests feed
/// through it.
///
/// # Ownership
///
/// Owns the device and every texture it creates; both are reference-counted COM
/// interfaces released when this is dropped, which happens after the encoder
/// under test — the encoder is created from it and dropped first in every test.
struct TestGpu {
    device: ID3D11Device,
}

impl TestGpu {
    /// Opens a device on the NVIDIA adapter, or says why it could not.
    ///
    /// Returns [`None`] on a machine with no NVIDIA GPU, unless
    /// `CLIPPED_REQUIRE_ENCODER` is set, in which case the absence is a
    /// failure. That is the lever that stops "the encoder tests passed" from
    /// quietly meaning "the encoder tests did nothing".
    fn open() -> Option<Self> {
        match Self::try_open() {
            Ok(gpu) => Some(gpu),
            Err(reason) => {
                assert!(
                    std::env::var_os("CLIPPED_REQUIRE_ENCODER").is_none(),
                    "CLIPPED_REQUIRE_ENCODER is set and the encoder could not be exercised: \
                     {reason}"
                );
                eprintln!("skipped: {reason}");
                None
            }
        }
    }

    fn try_open() -> Result<Self, String> {
        let adapter = nvidia_adapter()?;
        let mut device: Option<ID3D11Device> = None;

        // SAFETY: `adapter` is a live DXGI adapter, the driver type must be
        // UNKNOWN when an adapter is named, the module handle is unused for
        // that driver type, the feature level list and the out-parameters are
        // live locals, and `D3D11_SDK_VERSION` is the constant the header
        // requires. On success `device` holds one reference, released on drop.
        unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                windows::Win32::Foundation::HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
        }
        .map_err(|error| format!("no Direct3D 11 device on the NVIDIA adapter: {error}"))?;

        device
            .map(|device| Self { device })
            .ok_or_else(|| "D3D11CreateDevice reported success without a device".to_owned())
    }

    /// Opens an encoder against this device.
    fn open_encoder(&self, config: EncoderConfig) -> Result<NvencEncoder, EncodeError> {
        // SAFETY: the device is alive for as long as `self` is, and every
        // encoder opened from it is dropped inside the test that opened it.
        let device = unsafe { GraphicsDevice::new(DeviceKind::D3d11, self.device.as_raw()) };
        NvencEncoder::open(&device, config)
    }

    /// A texture holding a moving pattern, so that successive frames differ and
    /// the encoder has something to predict.
    fn pattern_texture(&self, index: usize) -> ID3D11Texture2D {
        let width = TEST_SIZE.width as usize;
        let height = TEST_SIZE.height as usize;
        let mut pixels = vec![0u8; width * height * 4];
        let offset = (index * 11) % width;

        for y in 0..height {
            for x in 0..width {
                let at = (y * width + x) * 4;
                let bar = (x + offset) % 128 < 64;
                // BGRA, which is what `DXGI_FORMAT_B8G8R8A8_UNORM` stores.
                pixels[at] = if bar { 200 } else { 20 };
                pixels[at + 1] = ((y * 255) / height) as u8;
                pixels[at + 2] = ((x * 255) / width) as u8;
                pixels[at + 3] = 255;
            }
        }

        self.texture_from(&pixels)
    }

    /// A texture of one solid colour, given as red, green, blue.
    fn solid_texture(&self, colour: [u8; 3]) -> ID3D11Texture2D {
        let width = TEST_SIZE.width as usize;
        let height = TEST_SIZE.height as usize;
        let mut pixels = vec![0u8; width * height * 4];

        for pixel in pixels.chunks_exact_mut(4) {
            pixel[0] = colour[2];
            pixel[1] = colour[1];
            pixel[2] = colour[0];
            pixel[3] = 255;
        }

        self.texture_from(&pixels)
    }

    /// Uploads pixels into a texture NVENC can bind.
    fn texture_from(&self, pixels: &[u8]) -> ID3D11Texture2D {
        let description = D3D11_TEXTURE2D_DESC {
            Width: TEST_SIZE.width,
            Height: TEST_SIZE.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let initial = D3D11_SUBRESOURCE_DATA {
            pSysMem: pixels.as_ptr().cast::<c_void>(),
            SysMemPitch: TEST_SIZE.width * 4,
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

/// The first NVIDIA adapter in the machine.
fn nvidia_adapter() -> Result<IDXGIAdapter, String> {
    // SAFETY: the factory is created and released by this function and its
    // caller; nothing outlives it.
    let factory: IDXGIFactory1 =
        unsafe { CreateDXGIFactory1() }.map_err(|error| format!("no DXGI factory: {error}"))?;

    for index in 0.. {
        // SAFETY: enumeration stops at the first failure, which is how DXGI
        // reports the end of the list.
        let Ok(adapter) = (unsafe { factory.EnumAdapters(index) }) else {
            break;
        };
        // SAFETY: `adapter` is live and `GetDesc` fills a live local.
        let Ok(description) = (unsafe { adapter.GetDesc() }) else {
            continue;
        };
        if Vendor::from_pci_id(description.VendorId) == Vendor::Nvidia {
            return Ok(adapter);
        }
    }

    Err("there is no NVIDIA adapter in this machine".to_owned())
}

/// A file in the temporary directory that deletes itself.
///
/// The tests write real bitstreams to disk so that `ffprobe` can read them, and
/// leaving them behind would fill a developer's temporary directory a megabyte
/// at a time.
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

/// The file extension that tells `ffprobe` what an elementary stream is.
const fn extension(codec: Codec) -> &'static str {
    match codec {
        Codec::H264 => "h264",
        Codec::Hevc => "hevc",
        Codec::Av1 => "obu",
    }
}

#[test]
fn the_encoder_kind_is_the_one_this_module_implements() {
    // Cheap, hardware-free, and the thing every log line and every capability
    // report is keyed on.
    assert_eq!(EncoderKind::Nvenc.vendor(), Some(Vendor::Nvidia));
    assert!(EncoderKind::Nvenc.is_hardware());
}
