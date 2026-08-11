//! Tests for the AMF backend, including ones that encode real video.
//!
//! # What runs where
//!
//! The tests that need AMD hardware skip on a machine that has none, which is
//! every hosted CI runner (`.github/workflows/ci.yml`). Skipping silently would
//! make "the encoder tests passed" and "the encoder tests ran" the same
//! sentence, so each skip says why through [`skipped`], and setting
//! `CLIPPED_REQUIRE_ENCODER=1` turns it into a failure — the same lever the
//! NVENC backend uses, and the one used to produce the evidence on issue #16.
//!
//! That lever covers a missing FFmpeg as well as a missing GPU, because the
//! acceptance criterion of issue #16 is what `ffprobe` reports: a machine with
//! an AMD GPU and no FFmpeg would otherwise run every hardware test and check
//! none of the criterion. The only skip it does not turn into a failure is a
//! codec this GPU genuinely cannot encode, which is a fact about the silicon
//! rather than about the checkout.
//!
//! # What "verified" means here
//!
//! A successful call is not a valid recording (AGENTS.md section 22). The
//! hardware tests therefore parse what came out — Annex B NAL units — and hand
//! the file to `ffprobe` to assert on what it reports, including that every
//! submitted frame comes back out of a decoder.
//!
//! That extends to the encoder's own account of itself. `PictureKind` is read
//! from an AMF property, so every keyframe assertion here is checked twice: once
//! against what the encoder reported, and once against the NAL unit types in the
//! bytes it produced ([`contains_idr`]). A backend that read the wrong property,
//! or the wrong codec's enumeration, would otherwise report keyframes in exactly
//! the right places while producing a stream that has none.
//!
//! # The harness
//!
//! Deliberately a second copy of the NVENC backend's, rather than a shared one:
//! that module's harness is private to it, and lifting it out is a change to a
//! file this ticket does not own. Sharing the two is
//! [#166](https://github.com/wildware-uk/clipped/issues/166).

use core::ffi::c_void;
use core::time::Duration;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard};

use windows::core::Interface;
use windows::Win32::Foundation::FreeLibrary;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET,
    D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
    D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory1};
use windows::Win32::System::LibraryLoader::{LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32};

use crate::backend::VideoEncoder;
use crate::codec::{Codec, EncoderKind, Resolution, Vendor};
use crate::config::{
    BitRate, ColourSpace, EncoderConfig, FrameRate, KeyframeInterval, RateControl, SurfaceFormat,
};
use crate::error::{EncodeError, EncodeErrorKind};
use crate::frame::{DeviceKind, GraphicsDevice, SourceFrame, SourceTexture, SurfaceKind};
use crate::probe::EncoderLimits;

use super::{classify, sys, AmfEncoder};

/// The picture size the hardware tests encode at. Large enough to be a real
/// encode rather than a toy one, and small enough that two codecs' worth of it
/// does not make the suite slow.
const TEST_SIZE: Resolution = Resolution::new(1280, 720);

/// How many frames each hardware test encodes.
const TEST_FRAMES: usize = 90;

// ---------------------------------------------------------------------------
// Tests that need no hardware
// ---------------------------------------------------------------------------

#[test]
fn an_odd_picture_size_is_refused_with_a_reason() {
    // 4:2:0 chroma has no representation for an odd dimension.
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
            .starts_with("AMD AMF could not encode 1919x1080 H.264"),
        "{error}"
    );
}

#[test]
fn a_null_device_is_refused_before_anything_is_loaded() {
    let config = config_for(Codec::H264, TEST_SIZE);
    // SAFETY: the handle is never dereferenced — `open` rejects it before it
    // reaches the runtime, which is what this test asserts.
    let device = unsafe { GraphicsDevice::new(DeviceKind::D3d11, core::ptr::null_mut()) };

    let error = AmfEncoder::open(&device, config).expect_err("a null device is not a device");
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

    let error = AmfEncoder::open(&device, config).expect_err("10-bit input is not supported yet");
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
fn av1_is_refused_as_a_missing_backend_rather_than_as_missing_hardware() {
    // This backend does not configure AMF's AV1 encoder, and the difference
    // between "your GPU cannot" and "Clipped cannot yet" is the difference
    // between a user buying a new GPU and a user waiting for a release
    // (AGENTS.md section 15).
    let error = open_expecting_failure(TEST_SIZE, Codec::Av1);

    assert!(
        matches!(error.kind(), EncodeErrorKind::Configuration { .. }),
        "an unimplemented codec is not the same failure as unsupported hardware: {error}"
    );
    assert!(
        error.to_string().contains("does not implement AV1"),
        "the message does not say whose limitation this is: {error}"
    );
    assert!(
        error.to_string().contains("issues/165"),
        "the message does not say where to follow it: {error}"
    );
}

#[test]
fn the_statuses_a_caller_can_act_on_are_classified_rather_than_passed_through() {
    // `EncodeErrorKind` is what a session loop will branch on, so these mappings
    // carry the whole of "driver reset and an unavailable encoder are
    // recognised". They need a status code and nothing else, so there is no
    // reason for them to be exercised only on a machine with an AMD GPU.
    let kind = |status| classify(status, "AMFComponent::SubmitInput");

    // A driver reset or a device that vanished: transient, because waiting out
    // the reset makes it work again.
    assert!(matches!(
        kind(sys::AMF_NO_DEVICE),
        EncodeErrorKind::DeviceLost
    ));
    assert!(matches!(
        kind(sys::AMF_DIRECTX_FAILED),
        EncodeErrorKind::DeviceLost
    ));
    assert!(EncodeError::new(
        crate::error::EncodeContext::new(EncoderKind::Amf, Codec::H264, TEST_SIZE),
        kind(sys::AMF_NO_DEVICE)
    )
    .is_transient());

    assert!(matches!(
        kind(sys::AMF_OUT_OF_MEMORY),
        EncodeErrorKind::OutOfMemory
    ));
    for unsupported in [
        sys::AMF_NOT_SUPPORTED,
        sys::AMF_NOT_IMPLEMENTED,
        sys::AMF_CODEC_NOT_SUPPORTED,
        sys::AMF_ENCODER_NOT_PRESENT,
    ] {
        assert!(
            matches!(kind(unsupported), EncodeErrorKind::CodecUnsupported),
            "{} should mean the encoder is not there",
            super::api::status_name(unsupported)
        );
    }

    // Anything else keeps AMD's own vocabulary, because a status this build has
    // never seen is exactly the one whose name and number matter.
    let other = kind(sys::AMF_WRONG_STATE);
    let EncodeErrorKind::Api {
        operation,
        status,
        status_name,
        ..
    } = other
    else {
        panic!("an unclassified status should arrive as EncodeErrorKind::Api, not {other:?}");
    };
    assert_eq!(operation, "AMFComponent::SubmitInput");
    assert_eq!(status, sys::AMF_WRONG_STATE);
    assert_eq!(status_name, "AMF_WRONG_STATE");
}

#[test]
fn the_encoder_kind_is_the_one_this_module_implements() {
    // Cheap, hardware-free, and the thing every log line and every capability
    // report is keyed on.
    assert_eq!(EncoderKind::Amf.vendor(), Some(Vendor::Amd));
    assert!(EncoderKind::Amf.is_hardware());
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
fn a_forced_keyframe_arrives_where_it_was_asked_for() {
    // Both codecs, and not for symmetry. HEVC's keyframe handling is a
    // different set of AMF properties from H.264's, and running only H.264 is
    // exactly how a real defect survived review once: `HevcGOPSPerIDR = 0`
    // means AMF inserts no IDR at all, so with `KeyframeInterval::Never` the
    // stream opened with a `CRA_NUT` that was reported as intra and the first
    // packet of the recording was not a cut point (see `super::force_idr`).
    for codec in [Codec::H264, Codec::Hevc] {
        forced_keyframes_land_where_they_were_asked_for(codec);
    }
}

/// The body of the test above, for one codec.
fn forced_keyframes_land_where_they_were_asked_for(codec: Codec) {
    /// Which frame asks to be a keyframe out of turn.
    const FORCED: usize = 7;
    /// How many frames the stream is.
    const FRAMES: usize = 12;

    let Some(gpu) = TestGpu::open() else {
        return;
    };

    // A keyframe interval far longer than the test, so the only keyframes are
    // the first one and the one asked for. This is what the replay buffer
    // depends on: a clip has to be able to start where the user pressed the key,
    // not at the next scheduled keyframe (SPEC.md section 7).
    let config = config_for(codec, TEST_SIZE).with_keyframe_interval(KeyframeInterval::Never);
    let mut encoder = match gpu.open_encoder(config) {
        Ok(encoder) => encoder,
        Err(error) => {
            unsupported_or_panic(codec, &error);
            return;
        }
    };

    let mut reported = Vec::new();
    let mut in_the_bitstream = Vec::new();
    for index in 0..FRAMES {
        let texture = gpu.pattern_texture(index);
        let frame = source_frame(&texture, index);
        let frame = if index == FORCED {
            frame.forcing_keyframe()
        } else {
            frame
        };

        encoder.submit(&frame).expect("the frame is submitted");
        while let Some(packet) = encoder.next_packet().expect("a packet is produced") {
            if packet.is_keyframe() {
                reported.push(packet.presentation_time());
            }
            // The same question asked of the coded bytes rather than of the
            // encoder. Everything else in this suite reads AMF's own
            // `OutputDataType` property, which is the encoder marking its own
            // homework; this reads the NAL unit types that a decoder, a muxer
            // and the replay buffer will actually see.
            if contains_idr(codec, packet.data()) {
                in_the_bitstream.push(packet.presentation_time());
            }
        }
    }

    let expected = [Duration::ZERO, amf_time(frame_time(FORCED))];
    assert_eq!(
        in_the_bitstream, expected,
        "{codec}: the IDR pictures in the bitstream should be the first frame and the one that \
         asked to be one"
    );
    assert_eq!(
        reported, expected,
        "{codec}: the packets flagged as keyframes should be exactly the IDR pictures in the \
         bitstream"
    );
}

#[test]
fn the_first_picture_of_a_stream_is_always_coded_as_a_keyframe() {
    // The rule `submit` applies, on its own and without hardware, so that CI
    // sees it too. The second argument is how many pictures this session has
    // already coded.
    assert!(
        super::force_idr(false, 0),
        "the first picture has to be an IDR even when nothing asked, or the recording has no cut \
         point at its own beginning"
    );
    assert!(
        !super::force_idr(false, 1),
        "a later picture that nothing asked about must be left to the configured interval"
    );
    assert!(
        super::force_idr(true, 1),
        "a frame that asked to be a keyframe has to be one wherever it falls"
    );
}

#[test]
fn a_timestamp_that_goes_backwards_is_refused() {
    let Some(gpu) = TestGpu::open() else {
        return;
    };
    let Some(mut encoder) = gpu.encoder_for(Codec::H264) else {
        return;
    };

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
    let Some(mut encoder) = gpu.encoder_for(Codec::H264) else {
        return;
    };

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
    let Some(mut encoder) = gpu.encoder_for(Codec::H264) else {
        return;
    };

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
fn dropping_a_session_gives_back_every_amf_object_it_held() {
    // AMF objects are reference counted, so "did teardown release everything?"
    // has an exact answer rather than a hopeful one — which matters here more
    // than it looks, because AMD hardware has no small cap on concurrent
    // encoding sessions and a leaked one would therefore *not* show up as a
    // later session refusing to open (see the test below).
    //
    // The trick is to take a reference of this test's own to each object before
    // the encoder is dropped, so that the objects survive teardown and can be
    // asked what is left. Giving that reference back must take the count to
    // zero: anything else is a reference the session kept.
    let Some(gpu) = TestGpu::open() else {
        return;
    };
    let Some(encoder) = gpu.encoder_for(Codec::H264) else {
        return;
    };

    // One thing has to be arranged before the encoder is dropped: a session
    // releases the AMF module when it goes, and objects whose code lives in that
    // module cannot be asked anything afterwards. So this test pins the module
    // for its own use — which is exactly the invariant the session upholds by
    // releasing every object before its runtime goes. It is loaded here rather
    // than at the top of the test because a machine with no AMD driver has no
    // such library, and this must skip there rather than fail.
    let library: Vec<u16> = "amfrt64.dll"
        .encode_utf16()
        .chain(core::iter::once(0))
        .collect();
    // SAFETY: `library` is a nul-terminated UTF-16 string that outlives the
    // call, and the flag is a documented `LOAD_LIBRARY_SEARCH_*` value. The
    // reference is released at the end of this test.
    let module = unsafe {
        LoadLibraryExW(
            windows::core::PCWSTR(library.as_ptr()),
            None,
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    }
    .expect("the session above opened, so its runtime is loadable");

    let session = encoder.session.as_ref().expect("the session is open");
    let context = session.context.cast::<sys::AMFInterface>();
    let component = session.component.cast::<sys::AMFInterface>();
    // SAFETY: both are live objects the session holds a reference to, and the
    // references taken here are given back below.
    unsafe {
        super::api::acquire(context);
        super::api::acquire(component);
    }

    drop(encoder);

    // The component first: it holds a reference to the context, which it gives
    // back when it is destroyed.
    // SAFETY: the reference taken above is given back exactly once, here.
    let component_left = unsafe { super::api::release(component) };
    // SAFETY: as above.
    let context_left = unsafe { super::api::release(context) };

    // SAFETY: the reference taken at the top of this test is given back exactly
    // once, here, after nothing else needs the module.
    let _ = unsafe { FreeLibrary(module) };

    assert_eq!(
        component_left, 0,
        "the encoder component outlived the session that owned it"
    );
    assert_eq!(
        context_left, 0,
        "the AMF context outlived the session that owned it, and with it the reference it holds \
         on the caller's Direct3D device"
    );
}

#[test]
fn many_sessions_can_be_opened_and_closed_in_turn() {
    // A recorder opens and closes an encoder for every recording, and a machine
    // that runs for days does that thousands of times (AGENTS.md sections 58 and
    // 59).
    //
    // Honesty about what this does *not* prove. On NVIDIA hardware the
    // equivalent test detects a leaked session, because consumer cards cap how
    // many may exist at once; AMD's do not, and disabling the component release
    // in `Session::drop` was measured to leave this test green through all
    // sixteen rounds. What detects that is
    // `dropping_a_session_gives_back_every_amf_object_it_held` above, which asks
    // the reference count directly. This one is the other half: that opening and
    // closing sessions in a loop keeps working, which is what a recorder does
    // all day.
    let Some(gpu) = TestGpu::open() else {
        return;
    };

    for round in 0..16 {
        let mut encoder = match gpu.open_encoder(config_for(Codec::H264, TEST_SIZE)) {
            Ok(encoder) => encoder,
            Err(error) if round == 0 => {
                unsupported_or_panic(Codec::H264, &error);
                return;
            }
            Err(error) => panic!(
                "session {round} could not be opened, which means an earlier one leaked: {error}"
            ),
        };

        let texture = gpu.pattern_texture(round);
        encoder
            .submit(&source_frame(&texture, 0))
            .expect("the frame is submitted");
        drain(&mut encoder);
        // Dropped without `shut_down`, which is the path an unwind takes.
    }
}

#[test]
fn a_texture_can_be_reused_the_moment_submit_returns() {
    // The contract every backend owes a capture backend: when `submit` returns,
    // AMF holds nothing derived from the texture, so the frame pool may put that
    // surface straight back into rotation (see `crate::frame`).
    //
    // So this test does what a frame pool does — one texture, overwritten as
    // soon as `submit` comes back — and asserts the coded pictures are the
    // colours that were submitted rather than the ones that replaced them.
    //
    // Honesty about what it demonstrates: it was also run against a deliberately
    // broken backend, one whose `submit` returned as soon as `SubmitInput` had
    // been called and only collected the picture from a later `next_packet`, and
    // it passed there too — on Adrenalin 32.0.21043.5001 the encoder is finished
    // with a 1280x720 frame before an `UpdateSubresource` from the CPU can land
    // on it. So this is a regression guard for the contract in
    // `crate::frame::SourceTexture`, not a reproduction of corruption. A timing
    // this driver happens to make safe is not a guarantee, which is why the code
    // does not rely on it: `submit` waits for the coded picture and then for
    // AMF's own reference to the wrapper to go.
    let Some(gpu) = TestGpu::open() else {
        return;
    };
    let Some(ffmpeg) = tool("ffmpeg") else {
        skipped(
            "ffmpeg is not beside the pinned FFmpeg or on the path, so nothing can decode the \
             result. Run scripts/fetch-ffmpeg.ps1.",
        );
        return;
    };

    let colours: [[u8; 3]; 3] = [[255, 0, 0], [0, 255, 0], [0, 0, 255]];
    let Some(mut encoder) = gpu.encoder_for_colours(&colours) else {
        return;
    };

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

    let decoded = decode_middle_pixels(&ffmpeg, &stream, colours.len());
    for (index, expected) in colours.iter().enumerate() {
        assert_colour_close(
            decoded[index],
            *expected,
            &format!(
                "frame {index} came back as the pixels that replaced it, so the encoder was still \
                 reading the texture after `submit` returned"
            ),
        );
    }
}

#[test]
fn the_encoded_colour_survives_a_round_trip() {
    // The frames going in are red, green and blue; the frames coming out are
    // decoded back to red, green and blue. This is the end-to-end check that the
    // colour description written into the stream agrees with the conversion AMF
    // performed — tag one thing and encode another, and every recording comes
    // out washed out or oversaturated with nothing in any log to say so
    // (AGENTS.md section 22).
    //
    // What it does not check is *which* profile was chosen. Tagging full range
    // while asking for limited was measured not to fail this test: AMF follows
    // the configured colour profile for its own conversion, so the tag and the
    // conversion moved together and red stayed red. What does fail it is a
    // conversion that disagrees with the pixels — telling AMF the BGRA texture
    // is RGBA decodes frame 0 as [0, 0, 251] rather than [255, 0, 0]. Proving
    // the choice of matrix needs a decode with the matrix forced, which is issue
    // #147.
    let Some(gpu) = TestGpu::open() else {
        return;
    };
    let Some(ffmpeg) = tool("ffmpeg") else {
        skipped(
            "ffmpeg is not beside the pinned FFmpeg or on the path, so nothing can decode the \
             result. Run scripts/fetch-ffmpeg.ps1.",
        );
        return;
    };

    let colours: [[u8; 3]; 3] = [[255, 0, 0], [0, 255, 0], [0, 0, 255]];
    let Some(mut encoder) = gpu.encoder_for_colours(&colours) else {
        return;
    };

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

    let decoded = decode_middle_pixels(&ffmpeg, &stream, colours.len());
    for (index, expected) in colours.iter().enumerate() {
        assert_colour_close(
            decoded[index],
            *expected,
            &format!(
                "frame {index} decoded as a different colour: the colour description in the \
                 stream does not match the conversion the encoder performed"
            ),
        );
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
    let Some(mut encoder) = gpu.encoder_for(codec) else {
        return;
    };

    assert!(
        !encoder.parameter_sets().is_empty(),
        "a muxer needs the parameter sets before the first frame"
    );

    let mut stream = Vec::new();
    let mut packets = 0usize;
    let mut keyframes = Vec::new();
    let mut timeline = Vec::with_capacity(TEST_FRAMES);
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
            timeline.push(packet.presentation_time());
            // What the encoder says about the picture, checked against what it
            // actually coded. `PictureKind` comes from AMF's own
            // `OutputDataType` property, and a backend that read the wrong
            // property or the wrong enumeration would report keyframes in
            // exactly the right places while producing a stream with none.
            assert_eq!(
                packet.is_keyframe(),
                contains_idr(codec, packet.data()),
                "{codec}: the encoder reported {:?} at {:?}, which the bitstream disagrees with",
                packet.picture(),
                packet.presentation_time()
            );
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
    // Every frame's position in the recording comes back out, and it is the
    // position that went in rather than one the encoder invented. `amf_time` is
    // the only licence taken: AMF counts in hundred-nanosecond ticks, so the
    // submitted time is reported rounded down to one (see `presentation_units`).
    let expected_timeline: Vec<Duration> = (0..TEST_FRAMES)
        .map(|index| amf_time(frame_time(index)))
        .collect();
    assert_eq!(
        timeline, expected_timeline,
        "the timestamps that came out are not the ones that went in"
    );
    let expected_keyframes: Vec<Duration> = (0..TEST_FRAMES)
        .step_by(60)
        .map(|index| amf_time(frame_time(index)))
        .collect();
    assert_eq!(
        keyframes, expected_keyframes,
        "a one-second keyframe interval at 60 fps puts a keyframe every sixtieth frame over the \
         {TEST_FRAMES} submitted, and nowhere else"
    );
    check_structure(codec, &stream);

    report_latency(codec, &latencies, stream.len());

    let file = TempFile::new("clipped-amf", extension(codec));
    std::fs::write(file.path(), &stream).expect("the stream can be written");
    probe(codec, file.path());
}

/// Parses the bitstream far enough to know it is the codec it claims to be.
///
/// Deliberately not a decode: this runs on every machine with an AMD GPU,
/// including ones with no FFmpeg, and a stream with no parameter sets or no
/// keyframe is broken whatever a decoder would say about it.
fn check_structure(codec: Codec, stream: &[u8]) {
    let units = annex_b_units(stream);
    assert!(
        units.len() >= 3,
        "{codec} produced {} NAL units, which is not a stream",
        units.len()
    );

    let types: Vec<u8> = units
        .iter()
        .map(|unit| match codec {
            // H.264: five bits of type in the low bits of one header byte. HEVC:
            // six bits, one bit up, in a two-byte header.
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

/// Whether a coded picture is an IDR, read out of the bitstream.
///
/// The independent half of every keyframe assertion here: the encoder's own
/// answer is a property AMF fills in, and this is the NAL unit type a decoder
/// reads.
///
/// H.264 numbers an IDR slice 5. HEVC has two, `IDR_W_RADL` (19) and
/// `IDR_N_LP` (20), and deliberately does not count `CRA_NUT` (21): a clean
/// random access picture may be followed by leading pictures that reference
/// across it, so it is not the unconditional cut point a clip needs (see
/// [`crate::packet::PictureKind`]).
fn contains_idr(codec: Codec, packet: &[u8]) -> bool {
    annex_b_units(packet).iter().any(|unit| match codec {
        Codec::H264 => unit[0] & 0x1F == 5,
        _ => matches!((unit[0] >> 1) & 0x3F, 19 | 20),
    })
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

/// Hands the file to `ffprobe` and asserts on what it says, when `ffprobe` is on
/// the path.
fn probe(codec: Codec, path: &Path) {
    let Some(ffprobe) = tool("ffprobe") else {
        // The in-process parse checks a NAL type; it does not check that
        // anything can decode the stream, which is what issue #16 asks for. So
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

/// Decodes a stream with FFmpeg and returns the middle pixel of each frame as
/// red, green, blue.
///
/// The middle of the picture, away from any edge the codec may have padded.
fn decode_middle_pixels(ffmpeg: &Path, stream: &[u8], frames: usize) -> Vec<[u8; 3]> {
    let file = TempFile::new("clipped-amf-colour", "h264");
    std::fs::write(file.path(), stream).expect("the stream can be written");

    let decoded = TempFile::new("clipped-amf-colour", "rgb");
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

/// Fails unless a decoded colour is the one that was encoded, allowing for the
/// rounding a trip through 4:2:0 costs.
fn assert_colour_close(got: [u8; 3], expected: [u8; 3], because: &str) {
    for channel in 0..3 {
        let difference = i32::from(got[channel]) - i32::from(expected[channel]);
        assert!(
            difference.abs() <= 12,
            "decoded {got:?} rather than {expected:?}: {because}"
        );
    }
}

/// Prints the encode latency, which is the cost of `submit` waiting for the
/// picture.
///
/// Printed rather than asserted on: a threshold here would be a test that fails
/// on somebody else's slower GPU (AGENTS.md section 25). Run with `cargo test --
/// --nocapture` to see it.
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
    AmfEncoder::open(&device, config_for(codec, resolution))
        .expect_err("this configuration cannot be encoded")
}

/// When a frame sits in the recording, at 60 frames a second.
fn frame_time(index: usize) -> Duration {
    Duration::from_nanos((index as u64) * 1_000_000_000 / 60)
}

/// The same instant as AMF can represent it.
///
/// AMF counts time in hundred-nanosecond ticks, so a submitted timestamp comes
/// back rounded down to one: 116.666666 ms goes in and 116.6666 ms comes out.
/// That is a ten-thousandth of a frame at 60 frames a second and it does not
/// accumulate, and it is applied here rather than papered over with a tolerance
/// so that a *real* drift — one that grows with the recording — still fails
/// (see `super::presentation_units`).
fn amf_time(time: Duration) -> Duration {
    Duration::from_nanos((time.as_nanos() as u64 / 100) * 100)
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
fn drain(encoder: &mut AmfEncoder) {
    while encoder
        .next_packet()
        .expect("a packet is produced")
        .is_some()
    {}
}

/// Reports a codec this GPU cannot encode, or fails on anything else.
fn unsupported_or_panic(codec: Codec, error: &EncodeError) {
    assert!(
        matches!(error.kind(), EncodeErrorKind::CodecUnsupported),
        "{error}"
    );
    // Not every AMD part encodes every codec. The report `recorder capabilities`
    // prints says which, and this is the same answer arriving through the
    // encoder.
    unsupported_here(&format!("this GPU does not encode {codec}"));
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

/// Reports that a test could not run here, and returns whether the caller should
/// give up.
///
/// Panics instead of skipping when `CLIPPED_REQUIRE_ENCODER` is set, so a
/// machine that is meant to exercise the encoder cannot quietly stop doing it.
/// Writes through `std::io::stderr()` rather than `eprintln!` because libtest
/// captures the macro: a skip printed with `eprintln!` is invisible in a passing
/// run, which is exactly the failure mode this guards against.
fn skipped(reason: &str) -> bool {
    assert!(
        !env_is_set(REQUIRE_ENCODER),
        "{REQUIRE_ENCODER} is set, so this must not be skipped: {reason}"
    );
    let _ = writeln!(std::io::stderr(), "SKIPPED (encoder): {reason}");
    true
}

/// Reports a codec this GPU cannot encode.
///
/// Not a failure even under [`REQUIRE_ENCODER`]: which codecs a card offers is a
/// property of the silicon, and the same answer reaches a user through `recorder
/// capabilities`. Everything the machine *can* encode is still checked.
fn unsupported_here(reason: &str) -> bool {
    let _ = writeln!(std::io::stderr(), "SKIPPED (encoder, hardware): {reason}");
    true
}

/// The environment variable that turns "this machine could not run the test"
/// from a pass into a failure.
const REQUIRE_ENCODER: &str = "CLIPPED_REQUIRE_ENCODER";

fn env_is_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

/// Held for as long as a test is using the AMD encoder.
///
/// How many encoding sessions the hardware will run at once is a property of the
/// *machine*, not of a process, so two tests encoding at once are competing for
/// it. libtest runs tests in parallel by default, so without this the suite
/// would be a race whose outcome depends on which test reached the driver first
/// (AGENTS.md section 25).
///
/// Visible to the rest of `windows` for the same reason NVENC's is: the
/// capability probe's tests create AMF components through `WindowsProbe`, which
/// competes with these for the same hardware encoder
/// (`crate::windows::tests`).
pub(in crate::windows) static SESSIONS: Mutex<()> = Mutex::new(());

/// A Direct3D 11 device on the AMD adapter, and the textures the tests feed
/// through it.
///
/// # Ownership
///
/// Owns the device and every texture it creates; both are reference-counted COM
/// interfaces released when this is dropped, which happens after the encoder
/// under test — the encoder is created from it and dropped first in every test.
///
/// It also holds [`SESSIONS`] for its whole life, which is what serialises the
/// tests that need the hardware.
struct TestGpu {
    device: ID3D11Device,
    _sessions: MutexGuard<'static, ()>,
}

impl TestGpu {
    /// Opens a device on the AMD adapter, or says why it could not.
    ///
    /// Returns [`None`] on a machine with no AMD GPU, unless
    /// `CLIPPED_REQUIRE_ENCODER` is set, in which case the absence is a failure.
    fn open() -> Option<Self> {
        match Self::try_open() {
            Ok(gpu) => Some(gpu),
            Err(reason) => {
                skipped(&reason);
                None
            }
        }
    }

    fn try_open() -> Result<Self, String> {
        // A test that panicked while encoding poisoned the lock; the hardware is
        // no worse for it, and refusing to run every later test because an
        // earlier one failed would hide the rest of the suite behind the first
        // failure.
        let sessions = SESSIONS.lock().unwrap_or_else(|held| held.into_inner());
        Self::try_open_device(sessions)
    }

    fn try_open_device(sessions: MutexGuard<'static, ()>) -> Result<Self, String> {
        let adapter = amd_adapter()?;
        let mut device: Option<ID3D11Device> = None;

        // SAFETY: `adapter` is a live DXGI adapter, the driver type must be
        // UNKNOWN when an adapter is named, the module handle is unused for that
        // driver type, the feature level list and the out-parameters are live
        // locals, and `D3D11_SDK_VERSION` is the constant the header requires.
        // On success `device` holds one reference, released on drop.
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
        .map_err(|error| format!("no Direct3D 11 device on the AMD adapter: {error}"))?;

        device
            .map(|device| Self {
                device,
                _sessions: sessions,
            })
            .ok_or_else(|| "D3D11CreateDevice reported success without a device".to_owned())
    }

    /// Opens an encoder against this device.
    fn open_encoder(&self, config: EncoderConfig) -> Result<AmfEncoder, EncodeError> {
        AmfEncoder::open(&self.graphics_device(), config)
    }

    /// This device, as the crate's own borrowed handle.
    fn graphics_device(&self) -> GraphicsDevice {
        // SAFETY: the device is alive for as long as `self` is, and everything
        // opened from it is dropped inside the test that opened it.
        unsafe { GraphicsDevice::new(DeviceKind::D3d11, self.device.as_raw()) }
    }

    /// An encoder for the tests' standard configuration, or [`None`] with a
    /// skip already reported.
    fn encoder_for(&self, codec: Codec) -> Option<AmfEncoder> {
        match self.open_encoder(config_for(codec, TEST_SIZE)) {
            Ok(encoder) => Some(encoder),
            Err(error) => {
                unsupported_or_panic(codec, &error);
                None
            }
        }
    }

    /// An encoder configured so that every frame is coded from scratch, for the
    /// colour tests: a wrong colour must not be blamable on prediction from its
    /// neighbour.
    fn encoder_for_colours(&self, colours: &[[u8; 3]]) -> Option<AmfEncoder> {
        assert!(!colours.is_empty());
        let config = config_for(Codec::H264, TEST_SIZE)
            .with_colour_space(ColourSpace::BT709_LIMITED)
            .with_keyframe_interval(KeyframeInterval::every(
                Duration::from_millis(1),
                FrameRate::FPS_60,
            ));
        match self.open_encoder(config) {
            Ok(encoder) => Some(encoder),
            Err(error) => {
                unsupported_or_panic(Codec::H264, &error);
                None
            }
        }
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
        self.texture_from(&solid_pixels(colour))
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

    /// Uploads pixels into a texture AMF can wrap.
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

/// The first AMD adapter in the machine.
fn amd_adapter() -> Result<IDXGIAdapter, String> {
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
        if Vendor::from_pci_id(description.VendorId) == Vendor::Amd {
            return Ok(adapter);
        }
    }

    Err("there is no AMD adapter in this machine".to_owned())
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

#[test]
fn the_capabilities_describe_every_codec_this_backend_can_create_a_component_for() {
    // Issue #133: the limits `recorder capabilities` prints come from here on a
    // machine with an AMD GPU. Nothing asserts a particular number — the
    // integrated part this was written against is not every Radeon — but a
    // component that was created has to describe the size it takes, and a codec
    // this backend has no component for must not be answered at all, because
    // AMF was never asked.
    let Some(gpu) = TestGpu::open() else { return };
    let measured = super::measure_limits(&gpu.graphics_device());

    let asked: Vec<Codec> = measured.iter().map(EncoderLimits::codec).collect();
    assert_eq!(
        asked,
        vec![Codec::Hevc, Codec::H264],
        "AV1 has no component in this backend, so it must not appear as an answer"
    );

    for limits in &measured {
        if limits.supported() != Some(true) {
            continue;
        }
        let resolution = limits
            .max_resolution()
            .unwrap_or_else(|| panic!("{} has no maximum size", limits.codec()));
        assert!(
            resolution.width >= 1920 && resolution.height >= 1080,
            "{} reported a maximum of {resolution}, which no AMF encoder would say",
            limits.codec()
        );
    }
}

#[test]
fn ten_bit_is_answered_for_hevc_and_left_alone_for_h264() {
    // The asymmetry `settings::ten_bit_from_profile` explains, checked against
    // the hardware rather than only against the mapping: AMF's HEVC profile
    // enumeration reaches Main10 and can therefore answer, and its H.264 one
    // has no 10-bit profile in it at all, so the H.264 row must stay with the
    // published limit rather than being handed a number derived from unrelated
    // profile values.
    let Some(gpu) = TestGpu::open() else { return };
    let measured = super::measure_limits(&gpu.graphics_device());

    let for_codec = |codec| {
        measured
            .iter()
            .find(|limits| limits.codec() == codec)
            .unwrap_or_else(|| panic!("{codec} was asked about"))
    };

    let hevc = for_codec(Codec::Hevc);
    if hevc.supported() == Some(true) {
        assert!(
            hevc.hdr().is_some(),
            "AMF answered its HEVC profile and 10-bit was still not decided"
        );
    }
    assert_eq!(
        for_codec(Codec::H264).hdr(),
        None,
        "H.264 10-bit cannot be read from an AMF profile number and must not be claimed"
    );
}

/// The file extension that tells `ffprobe` what an elementary stream is.
const fn extension(codec: Codec) -> &'static str {
    match codec {
        Codec::H264 => "h264",
        Codec::Hevc => "hevc",
        Codec::Av1 => "obu",
    }
}
