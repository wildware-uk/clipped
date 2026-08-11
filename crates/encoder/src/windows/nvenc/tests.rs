//! Tests for the NVENC backend, including ones that encode real video.
//!
//! # What runs where
//!
//! The tests that need NVIDIA hardware skip on a machine that has none, which
//! is every hosted CI runner (`.github/workflows/ci.yml`). Skipping silently
//! would make "the encoder tests passed" and "the encoder tests ran" the same
//! sentence, so each skip says why through [`skipped`], and setting
//! `CLIPPED_REQUIRE_ENCODER=1` turns it into a failure — the same lever
//! `clipped-capture` uses for capture, and the one used to produce the evidence
//! on issue #15.
//!
//! That lever covers a missing FFmpeg as well as a missing GPU, because the
//! acceptance criterion of issue #15 is what `ffprobe` reports: a machine with
//! an NVIDIA card and no FFmpeg would otherwise run every hardware test and
//! check none of the criterion. The only skip it does not turn into a failure
//! is a codec this GPU genuinely cannot encode — AV1 arrived with Ada — which
//! is a fact about the hardware rather than about the checkout.
//!
//! # What "verified" means here
//!
//! A successful call is not a valid recording (AGENTS.md section 22). The
//! hardware tests therefore parse what came out — Annex B for H.264 and HEVC,
//! OBUs for AV1 — and hand the file to `ffprobe` to assert on what it reports.
//! `ffprobe` and `ffmpeg` are development tools here and nothing else: nothing
//! in the recorder shells out to FFmpeg. They are looked for beside the pinned
//! FFmpeg that `scripts/fetch-ffmpeg.ps1` installs, and then on the path.

use core::ffi::c_void;
use core::time::Duration;
use std::io::Write as _;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard};

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
use crate::packet::PictureKind;

use super::{classify, picture_kind, settings, sys, EndCause, NvencEncoder};

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

#[test]
fn the_statuses_a_caller_can_act_on_are_classified_rather_than_passed_through() {
    // `EncodeErrorKind` is what a session loop will branch on, so these
    // mappings carry the whole of "session limits and driver reset are
    // recognised". They need a status code and nothing else, so there is no
    // reason for them to be exercised only on a machine with an NVIDIA GPU.
    let detail = || "EncodeAPI Internal Error".to_owned();
    let kind = |status| classify(status, "nvEncEncodePicture", detail());

    // A driver reset or a device that vanished: transient, because plugging the
    // display back in or waiting out the reset makes it work again.
    assert!(matches!(
        kind(sys::NV_ENC_ERR_DEVICE_NOT_EXIST),
        EncodeErrorKind::DeviceLost
    ));
    assert!(matches!(
        kind(sys::NV_ENC_ERR_INVALID_DEVICE),
        EncodeErrorKind::DeviceLost
    ));
    assert!(EncodeError::new(
        crate::error::EncodeContext::new(EncoderKind::Nvenc, Codec::H264, TEST_SIZE),
        kind(sys::NV_ENC_ERR_DEVICE_NOT_EXIST)
    )
    .is_transient());

    assert!(matches!(
        kind(sys::NV_ENC_ERR_OUT_OF_MEMORY),
        EncodeErrorKind::OutOfMemory
    ));
    assert!(matches!(
        kind(sys::NV_ENC_ERR_UNSUPPORTED_PARAM),
        EncodeErrorKind::CodecUnsupported
    ));
    assert!(matches!(
        kind(sys::NV_ENC_ERR_UNIMPLEMENTED),
        EncodeErrorKind::CodecUnsupported
    ));

    // Anything else keeps NVIDIA's own vocabulary, because a status this build
    // has never seen is exactly the one whose name and number matter.
    let other = kind(sys::NV_ENC_ERR_GENERIC);
    let EncodeErrorKind::Api {
        operation,
        status,
        status_name,
        detail: reported,
    } = other
    else {
        panic!("an unclassified status should arrive as EncodeErrorKind::Api, not {other:?}");
    };
    assert_eq!(operation, "nvEncEncodePicture");
    assert_eq!(status, sys::NV_ENC_ERR_GENERIC);
    assert_eq!(status_name, "NV_ENC_ERR_GENERIC");
    assert_eq!(reported, detail());
}

#[test]
fn an_idr_is_a_cut_point_and_a_plain_intra_picture_is_not() {
    // The replay buffer cuts clips at keyframes, and only an IDR is one: an
    // intra picture that is not an IDR may still be predicted across, so a clip
    // starting there decodes into rubbish (see `crate::packet`). Swapping these
    // two arms would leave every other test in this file green.
    assert_eq!(
        picture_kind(sys::NV_ENC_PIC_TYPE_IDR),
        PictureKind::Keyframe
    );
    assert_eq!(picture_kind(sys::NV_ENC_PIC_TYPE_I), PictureKind::Intra);
    assert_eq!(
        picture_kind(sys::NV_ENC_PIC_TYPE_INTRA_REFRESH),
        PictureKind::Intra
    );
    assert_eq!(picture_kind(sys::NV_ENC_PIC_TYPE_P), PictureKind::Predicted);
    assert_eq!(
        picture_kind(sys::NV_ENC_PIC_TYPE_NONREF_P),
        PictureKind::Predicted
    );
    assert_eq!(
        picture_kind(sys::NV_ENC_PIC_TYPE_B),
        PictureKind::Bidirectional
    );
    assert_eq!(
        picture_kind(sys::NV_ENC_PIC_TYPE_UNKNOWN),
        PictureKind::Unknown
    );

    assert!(PictureKind::Keyframe.is_keyframe());
    assert!(!PictureKind::Intra.is_keyframe());
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
fn a_panic_while_nvenc_holds_the_texture_still_gives_it_back() {
    // `Session::drop` states that nothing derived from a caller's texture can
    // be outstanding by the time a session is destroyed. Releasing by hand at
    // the end of `submit` made that true only for as long as nothing in
    // between could unwind — the calls there are FFI plus `String` formatting,
    // so a panic was close to unreachable, but "close to" is not what the
    // comment says (issue #149).
    //
    // So this runs the unwind the comment has to survive, through the real
    // call: `Session::code_frame` panics on request at the point where NVENC
    // holds a registration on a texture this encoder does not own and an output
    // buffer is spoken for, and the panic is raised from inside
    // `NvencEncoder::submit` rather than beside it.
    //
    // Two things are watched, because `submit` has two resources in flight
    // there. `registrations` counts live `Registration`s, each of which unmaps
    // and unregisters in its `Drop`, so a count of zero after the unwind is the
    // release having happened rather than a driver-side query — delete the
    // `Drop` impl and this fails on the count. The free list has to be the
    // length it was, because a buffer taken out of it and put back only on the
    // error arm would be lost to every unwind until the session had none left;
    // take the buffer before `code_frame` instead of after and this fails on
    // the length. The encode afterwards is there because neither number can say
    // that what the guard did left a usable session.
    let Some(gpu) = TestGpu::open() else {
        return;
    };

    let mut encoder = gpu
        .open_encoder(config_for(Codec::H264, TEST_SIZE))
        .expect("NVENC encodes H.264");
    let texture = gpu.pattern_texture(0);

    let session = encoder.session.as_ref().expect("the session is open");
    let free_before = session.free.len();
    assert!(
        free_before > 0 && session.registrations.get() == 0,
        "the session should start with output buffers and no registrations"
    );
    session.panic_holding_texture.set(true);

    let unwound = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let _ = encoder.submit(&source_frame(&texture, 0));
    }));
    let panic = unwound.expect_err("the submission was supposed to panic");
    assert_eq!(
        panic.downcast_ref::<&str>().copied(),
        Some("a deliberate panic while NVENC holds the caller's texture"),
        "something other than the injected panic unwound, so this proves nothing"
    );

    let session = encoder.session.as_ref().expect("the session is still open");
    assert_eq!(
        session.registrations.get(),
        0,
        "a registration outlived the unwind, so `Session::drop` would destroy a session that \
         still holds the caller's texture"
    );
    assert_eq!(
        session.free.len(),
        free_before,
        "an output buffer was lost to the unwind, so enough panics would exhaust a session whose \
         buffers are all idle"
    );

    encoder
        .submit(&source_frame(&texture, 1))
        .expect("the session still encodes after the unwind");
    drain(&mut encoder);
}

#[test]
fn a_finish_after_a_flush_that_failed_does_not_report_success() {
    // `NV_ENC_PIC_FLAG_EOS` is what completes whatever NVENC is still holding,
    // so a `finish` that returns `Ok` after one that did not land tells a
    // recorder its file is complete when it may be a flush short. The session
    // is marked as ending before the submission — deliberately, so that no
    // unwind out of it leaves the session taking frames — and that marking must
    // not be read back as "already finished, nothing to do" (issue #149).
    //
    // A working driver will not refuse an end of stream to order, so the
    // refusal is injected: `fail_end_of_stream` stays set, so a second `finish`
    // that reported `Ok` could only have short-circuited. Clearing it and
    // asking again shows the retry is a real submission rather than a
    // bookkeeping change.
    let Some(gpu) = TestGpu::open() else {
        return;
    };

    let mut encoder = gpu
        .open_encoder(config_for(Codec::H264, TEST_SIZE))
        .expect("NVENC encodes H.264");
    let texture = gpu.pattern_texture(0);
    let format = settings::buffer_format(SurfaceFormat::Bgra8Unorm)
        .expect("BGRA is what this backend binds");

    encoder
        .submit(&source_frame(&texture, 0))
        .expect("the frame is submitted");
    drain(&mut encoder);

    let session = encoder.session.as_mut().expect("the session is open");
    session.fail_end_of_stream.set(true);
    session
        .finish()
        .expect_err("the end of stream was refused, so the flush failed");
    session
        .finish()
        .expect_err("a finish after a flush that failed must not report success");

    // The session is finished either way: a stream whose end was submitted
    // takes no more frames, whatever the driver said about it.
    let error = session
        .submit(&source_frame(&texture, 1), format, TEST_SIZE)
        .expect_err("a session past its end of stream takes no more frames");
    assert!(
        error.to_string().contains("has been finished"),
        "the message does not say why the frame was refused: {error}"
    );

    session.fail_end_of_stream.set(false);
    session
        .finish()
        .expect("with the driver answering, the retry ends the stream for real");
    session
        .finish()
        .expect("an end of stream NVENC accepted is not submitted twice");
}

#[test]
fn a_session_that_has_reached_the_end_of_its_stream_refuses_further_frames() {
    // Two levels, because there are two ways to reach the end of a stream and
    // only one of them goes through `NvencEncoder::finish`. The other is
    // `Session::deferred_picture`, which flushes the encoder to get a borrowed
    // texture back and reports the failure — and used to leave the session
    // taking frames afterwards, so a caller that retried would have been
    // submitting to an encoder that had already ended (issue #149).
    //
    // The deferred path itself cannot be reached here: it needs NVENC to buffer
    // a picture, which this backend's configuration forbids — B-frames and
    // lookahead are both off. What is exercised instead is the refusal it now
    // relies on, at the session level where that path leaves the flag, reached
    // through the other caller of `end_of_stream`.
    let Some(gpu) = TestGpu::open() else {
        return;
    };

    let mut encoder = gpu
        .open_encoder(config_for(Codec::H264, TEST_SIZE))
        .expect("NVENC encodes H.264");
    let texture = gpu.pattern_texture(0);
    let format = settings::buffer_format(SurfaceFormat::Bgra8Unorm)
        .expect("BGRA is what this backend binds");

    encoder
        .submit(&source_frame(&texture, 0))
        .expect("the frame is submitted");
    drain(&mut encoder);
    encoder.finish().expect("the stream can be finished");
    drain(&mut encoder);

    let error = encoder
        .submit(&source_frame(&texture, 1))
        .expect_err("a finished stream takes no more frames");
    assert!(
        matches!(error.kind(), EncodeErrorKind::Configuration { .. }),
        "{error}"
    );
    assert!(
        error.to_string().contains("has been finished"),
        "the message does not say why the frame was refused: {error}"
    );

    // Straight at the session, which is what a retry after `deferred_picture`
    // would reach: `NvencEncoder::finished` is not in the way there, because
    // that path never set it.
    let session = encoder.session.as_mut().expect("the session is still open");
    let error = session
        .submit(&source_frame(&texture, 2), format, TEST_SIZE)
        .expect_err("a session past its end of stream takes no more frames");
    assert!(
        matches!(error.kind(), EncodeErrorKind::Configuration { .. }),
        "{error}"
    );
    assert!(
        error.to_string().contains("has been finished"),
        "the message does not say why the frame was refused: {error}"
    );
}

#[test]
fn a_refusal_says_which_of_the_two_ends_of_a_stream_it_was() {
    // Both refusals are the same sentence to a caller that finished the stream
    // itself, because that caller knows what it did. A caller that hits the
    // other end — the encoder buffered a picture and the stream was flushed to
    // get its texture back — never finished anything, and telling it "the
    // stream has been finished" and no more misattributes the cause of a
    // failure in the middle of a recording (AGENTS.md section 15).
    let asked = EndCause::Finish.detail();
    let flushed = EndCause::DeferredPicture.detail();

    assert!(
        asked.contains("has been finished") && flushed.contains("has been finished"),
        "both refusals should say the stream is over: {asked} / {flushed}"
    );
    assert!(
        flushed.contains("nothing called `finish`") && flushed.contains("buffered a picture"),
        "the flushed refusal does not say what ended the stream: {flushed}"
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
fn a_full_session_table_is_reported_and_leaves_nothing_behind() {
    // Two things at once. That the session limit — the failure a recorder has
    // to survive on a machine already streaming — arrives as
    // `SessionLimitReached` rather than as a status code nobody can act on. And
    // that a *failed* open leaves nothing behind: the header requires
    // `nvEncDestroyEncoder` after one, and if a failure leaked driver-side
    // state then the second pass below would not get as far as the first.
    let Some(gpu) = TestGpu::open() else {
        return;
    };

    // Holds every session it opened until it returns, so the table is full at
    // the point of the failure and empty again immediately afterwards. The
    // bound keeps a card with a large limit from opening sessions all day.
    let exhaust = || -> Option<(usize, EncodeError)> {
        let mut open = Vec::new();
        for _ in 0..32 {
            match gpu.open_encoder(config_for(Codec::H264, TEST_SIZE)) {
                Ok(encoder) => open.push(encoder),
                Err(error) => return Some((open.len(), error)),
            }
        }
        None
    };

    let Some((first, error)) = exhaust() else {
        unsupported_here("this GPU allows at least 32 concurrent sessions, so nothing was refused");
        return;
    };
    assert!(
        matches!(error.kind(), EncodeErrorKind::SessionLimitReached),
        "a full session table should be recognised rather than passed through: {error}"
    );

    let Some((second, _)) = exhaust() else {
        panic!("the second pass opened more sessions than the first, which cannot happen");
    };
    assert!(
        second >= first,
        "the first pass opened {first} sessions and the second only {second}, so the failed open \
         in between kept something the driver never got back"
    );
}

#[test]
fn a_texture_can_be_reused_the_moment_submit_returns() {
    // The contract every backend owes a capture backend: when `submit` returns,
    // the encoder holds nothing derived from the texture, so the frame pool may
    // put that surface straight back into rotation (see `crate::frame`).
    //
    // So this test does what a frame pool does — one texture, overwritten as
    // soon as `submit` comes back — and asserts the coded pictures are the
    // colours that were submitted rather than the ones that replaced them.
    //
    // Honesty about what it demonstrates: it was also run against the previous
    // shape of this backend, which kept the registration and the mapping alive
    // until a later `next_packet`, and it passed there too — on driver 610.74
    // the encode of a 1280x720 frame finishes before an `UpdateSubresource` from
    // the CPU can land on it. So this is a regression guard for a contract taken
    // from `nvEncodeAPI.h` ("the client should not access any input buffer while
    // they are mapped by the encoder"), not a reproduction of corruption. A
    // timing this driver happens to make safe is not a guarantee, which is why
    // the code no longer relies on it.
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
    let config = config_for(Codec::H264, TEST_SIZE)
        .with_colour_space(ColourSpace::BT709_LIMITED)
        // Every frame a keyframe, so each colour is coded from scratch and a
        // wrong one cannot be blamed on prediction from its neighbour.
        .with_keyframe_interval(KeyframeInterval::every(
            Duration::from_millis(1),
            FrameRate::FPS_60,
        ));
    let mut encoder = gpu.open_encoder(config).expect("NVENC encodes H.264");

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
    // decoded back to red, green and blue. This is the end-to-end check that
    // the colour description written into the stream agrees with the conversion
    // NVENC performed — tag one thing and encode another, and every recording
    // comes out washed out or oversaturated with nothing in any log to say so
    // (AGENTS.md section 22).
    //
    // What it does not check is *which* matrix was chosen. Tagging BT.601 was
    // measured not to fail this test: NVENC follows the configured matrix for
    // its own conversion, so the tag and the conversion moved together. Proving
    // the choice needs a decode with the matrix forced, which is issue #147.
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

/// Decodes an H.264 stream with FFmpeg and returns the middle pixel of each
/// frame as red, green, blue.
///
/// The middle of the picture, away from any edge the codec may have padded.
fn decode_middle_pixels(ffmpeg: &Path, stream: &[u8], frames: usize) -> Vec<[u8; 3]> {
    let file = TempFile::new("clipped-colour", "h264");
    std::fs::write(file.path(), stream).expect("the stream can be written");

    let decoded = TempFile::new("clipped-colour", "rgb");
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
            unsupported_here(&format!("this GPU does not encode {codec}"));
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
    let expected_keyframes: Vec<Duration> = (0..TEST_FRAMES).step_by(60).map(frame_time).collect();
    assert_eq!(
        keyframes, expected_keyframes,
        "a one-second keyframe interval at 60 fps puts a keyframe every sixtieth frame over the \
         {TEST_FRAMES} submitted, and nowhere else"
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
        // The in-process parse checks a NAL type; it does not check that
        // anything can decode the stream, which is what issue #15 asks for. So
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

/// Looks for a development tool, beside the pinned FFmpeg first and then on the
/// path.
///
/// `FFMPEG_DIR` is set for every process Cargo runs by `.cargo/config.toml`, so
/// a checkout that has run `scripts/fetch-ffmpeg.ps1` — which is every checkout
/// that can build `clipped-muxer` — has `ffprobe.exe` and `ffmpeg.exe` here
/// without anybody adding them to `PATH`.
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

/// Reports that a test could not run here, and returns whether the caller
/// should give up.
///
/// Panics instead of skipping when `CLIPPED_REQUIRE_ENCODER` is set, so a
/// machine that is meant to exercise the encoder cannot quietly stop doing it.
/// Writes through `std::io::stderr()` rather than `eprintln!` because libtest
/// captures the macro: a skip printed with `eprintln!` is invisible in a
/// passing run, which is exactly the failure mode this guards against.
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
/// Not a failure even under [`REQUIRE_ENCODER`]: which codecs a card offers is
/// a property of the silicon — AV1 encoding arrived with Ada — and the same
/// answer reaches a user through `recorder capabilities`. Everything the
/// machine *can* encode is still checked.
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

/// Held for as long as a test is using encoding sessions.
///
/// The number of concurrent NVENC sessions is a property of the *machine*, not
/// of a process, so two tests encoding at once are competing for it — and one of
/// them fills it deliberately. libtest runs tests in parallel by default, so
/// without this the suite would be a race whose outcome depends on which test
/// reached the driver first (AGENTS.md section 25).
///
/// Visible to the rest of `windows` because the capability probe's own tests
/// open NVENC sessions too, through `WindowsProbe` rather than through
/// [`TestGpu`], and a second mutex would serialise them against a different set
/// of tests from the one that competes with them (`crate::windows::tests`).
pub(in crate::windows) static SESSIONS: Mutex<()> = Mutex::new(());

/// A Direct3D 11 device on the NVIDIA adapter, and the textures the tests feed
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
                skipped(&reason);
                None
            }
        }
    }

    fn try_open() -> Result<Self, String> {
        // A test that panicked while encoding poisoned the lock; the hardware
        // is no worse for it, and refusing to run every later test because an
        // earlier one failed would hide the rest of the suite behind the first
        // failure.
        let sessions = SESSIONS.lock().unwrap_or_else(|held| held.into_inner());
        Self::try_open_device(sessions)
    }

    fn try_open_device(sessions: MutexGuard<'static, ()>) -> Result<Self, String> {
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
            .map(|device| Self {
                device,
                _sessions: sessions,
            })
            .ok_or_else(|| "D3D11CreateDevice reported success without a device".to_owned())
    }

    /// Opens an encoder against this device.
    fn open_encoder(&self, config: EncoderConfig) -> Result<NvencEncoder, EncodeError> {
        NvencEncoder::open(&self.graphics_device(), config)
    }

    /// This device, as the crate's own borrowed handle.
    fn graphics_device(&self) -> GraphicsDevice {
        // SAFETY: the device is alive for as long as `self` is, and everything
        // opened from it is dropped inside the test that opened it.
        unsafe { GraphicsDevice::new(DeviceKind::D3d11, self.device.as_raw()) }
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
fn the_capability_queries_describe_every_codec_this_card_encodes() {
    // Issue #133: the limits `recorder capabilities` prints come from here on a
    // machine with an NVIDIA card. Nothing asserts a particular number — a 4090
    // is not every card — but an encoder that says it produces a codec has to
    // say how large a picture it takes, whether it has B-frames and whether it
    // encodes 10-bit, or there was no point opening the session.
    let Some(gpu) = TestGpu::open() else { return };
    let measured = super::measure_limits(&gpu.graphics_device());

    assert_eq!(
        measured.len(),
        Codec::EFFICIENCY_ORDER.len(),
        "every codec has to be asked about, including the ones the card refuses"
    );

    for limits in &measured {
        let supported = limits
            .supported()
            .unwrap_or_else(|| panic!("{} support was not answered", limits.codec()));
        if !supported {
            continue;
        }

        let resolution = limits
            .max_resolution()
            .unwrap_or_else(|| panic!("{} has no maximum size", limits.codec()));
        assert!(
            resolution.width >= 1920 && resolution.height >= 1080,
            "{} reported a maximum of {resolution}, which no NVENC generation would say",
            limits.codec()
        );
        assert!(
            limits.b_frames().is_some(),
            "{} did not answer whether it has B-frames",
            limits.codec()
        );
        assert!(
            limits.hdr().is_some(),
            "{} did not answer whether it encodes 10-bit",
            limits.codec()
        );
    }
}

#[test]
fn the_framerate_ceiling_is_left_inferred_because_the_driver_understates_it() {
    // The one capability this backend declines to report. NVENC answers
    // `NV_ENC_CAPS_MB_PER_SEC_MAX` with 983,040 on a GeForce RTX 4090 — 121
    // frames a second at 1080p — and the same card encodes 1280x720 at over a
    // thousand frames a second through this very backend. Publishing the
    // driver's figure as a *measurement* would tell a user their encoder cannot
    // do something it demonstrably can, with no `(i)` to soften it.
    //
    // So this asserts the decision rather than the number: whatever NVENC says
    // about its throughput, none of it reaches the report.
    let Some(gpu) = TestGpu::open() else { return };

    for limits in super::measure_limits(&gpu.graphics_device()) {
        assert_eq!(
            limits.max_luma_samples_per_second(),
            None,
            "{} published a framerate ceiling from a driver figure this backend does not \
             trust",
            limits.codec()
        );
    }
}

#[test]
fn the_encoder_kind_is_the_one_this_module_implements() {
    // Cheap, hardware-free, and the thing every log line and every capability
    // report is keyed on.
    assert_eq!(EncoderKind::Nvenc.vendor(), Some(Vendor::Nvidia));
    assert!(EncoderKind::Nvenc.is_hardware());
}
