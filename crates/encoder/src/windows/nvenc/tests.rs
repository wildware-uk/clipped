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
//!
//! # The harness
//!
//! `TestGpu`, `TempFile`, the FFmpeg lookup and the structural bitstream
//! checks are shared with the AMF backend's hardware tests, in
//! `crate::windows::hardware_test` (issue
//! [#166](https://github.com/wildware-uk/clipped/issues/166)). What stays
//! here is what only NVENC does: opening an `NvencEncoder` from a
//! [`TestGpu`], and the whole of `encode_and_verify` — NVENC's own copy
//! checks things AMF's does not, such as where IDR pictures land and that the
//! configured colour matrix survives a round trip through the driver.

use core::ffi::c_void;
use core::time::Duration;
use std::panic::AssertUnwindSafe;
use std::sync::Mutex;

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;

use crate::backend::VideoEncoder;
use crate::codec::{Codec, EncoderKind, Resolution, Vendor};
use crate::config::{
    BitRate, ColourSpace, EncoderConfig, FrameRate, KeyframeInterval, RateControl, SurfaceFormat,
};
use crate::error::{EncodeError, EncodeErrorKind};
use crate::frame::{DeviceKind, GraphicsDevice, SourceFrame, SourceTexture, SurfaceKind};
use crate::packet::PictureKind;
use crate::windows::hardware_test::{
    assert_colour_close, check_structure, decode_middle_pixels, extension, probe, report_latency,
    skipped, tool, unsupported_here, TempFile, TestGpu,
};

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
    let Some(gpu) = test_gpu() else {
        return;
    };

    // A keyframe interval far longer than the test, so the only keyframes are
    // the first one and the one asked for. This is what the replay buffer
    // depends on: a clip has to be able to start where the user pressed the
    // key, not at the next scheduled keyframe (SPEC.md section 7).
    let config = config_for(Codec::H264, TEST_SIZE).with_keyframe_interval(KeyframeInterval::Never);
    let mut encoder = open_encoder(&gpu, config).expect("NVENC encodes H.264");

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
    let Some(gpu) = test_gpu() else {
        return;
    };

    let mut encoder =
        open_encoder(&gpu, config_for(Codec::H264, TEST_SIZE)).expect("NVENC encodes H.264");

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
    let Some(gpu) = test_gpu() else {
        return;
    };

    let mut encoder =
        open_encoder(&gpu, config_for(Codec::H264, TEST_SIZE)).expect("NVENC encodes H.264");

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
    let Some(gpu) = test_gpu() else {
        return;
    };

    let mut encoder =
        open_encoder(&gpu, config_for(Codec::H264, TEST_SIZE)).expect("NVENC encodes H.264");

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
    let Some(gpu) = test_gpu() else {
        return;
    };

    let mut encoder =
        open_encoder(&gpu, config_for(Codec::H264, TEST_SIZE)).expect("NVENC encodes H.264");
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
    let Some(gpu) = test_gpu() else {
        return;
    };

    let mut encoder =
        open_encoder(&gpu, config_for(Codec::H264, TEST_SIZE)).expect("NVENC encodes H.264");
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
    let Some(gpu) = test_gpu() else {
        return;
    };

    let mut encoder =
        open_encoder(&gpu, config_for(Codec::H264, TEST_SIZE)).expect("NVENC encodes H.264");
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
    let Some(gpu) = test_gpu() else {
        return;
    };

    for round in 0..16 {
        let mut encoder = open_encoder(&gpu, config_for(Codec::H264, TEST_SIZE))
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
    let Some(gpu) = test_gpu() else {
        return;
    };

    // Holds every session it opened until it returns, so the table is full at
    // the point of the failure and empty again immediately afterwards. The
    // bound keeps a card with a large limit from opening sessions all day.
    let exhaust = || -> Option<(usize, EncodeError)> {
        let mut open = Vec::new();
        for _ in 0..32 {
            match open_encoder(&gpu, config_for(Codec::H264, TEST_SIZE)) {
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

    // Whether the table comes back.
    //
    // `second >= first`, asked once, is not only a statement about this
    // process. The session table belongs to the *driver*, so anything else on
    // the machine that opens a session between the two passes takes a slot this
    // one then cannot have — and the assertion failed, blaming a leak for what
    // was contention. It was seen twice on this project's machine, where a
    // second checkout running the suite at the same time is routine, and on a
    // contributor's machine the other party is OBS, a browser or a game
    // ([issue #236](https://github.com/wildware-uk/clipped/issues/236)).
    //
    // The two are told apart by asking more than once, because they differ in
    // kind rather than in size:
    //
    // - **A leak is permanent.** Every failed open would keep a slot the driver
    //   never gets back, so once one has happened *no* later pass can reach
    //   `first` again, however many times it is tried.
    // - **Contention is transient.** Whatever took a slot gives it back, so a
    //   pass that is not obstructed reaches `first`.
    //
    // So the answer is the best of several attempts rather than the first, which
    // is the same argument `crates/logging/tests/hot_loop_cost.rs` makes about a
    // contended host — and, as there, a real regression is present in every
    // attempt so taking the best still shows it.
    const ATTEMPTS: usize = 4;

    let mut reached = Vec::with_capacity(ATTEMPTS);
    for _ in 0..ATTEMPTS {
        let Some((count, _)) = exhaust() else {
            // The table stopped being fillable at all between passes. That is a
            // machine that freed a card rather than anything about this code,
            // and it cannot be told from one that did.
            unsupported_here(
                "the session table stopped being fillable between passes, so whether a failed \
                 open kept anything cannot be told here",
            );
            return;
        };
        reached.push(count);
        if count >= first {
            return;
        }
    }

    panic!(
        "the first pass opened {first} sessions and no later pass reached that again \
         ({reached:?}), so a failed open kept something the driver never got back. A leak \
         shows as exactly one slot fewer per failed open and never recovers; something else \
         on this machine holding a session shows as an arbitrary shortfall that comes back \
         within {ATTEMPTS} attempts"
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
    let Some(gpu) = test_gpu() else {
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
    let mut encoder = open_encoder(&gpu, config).expect("NVENC encodes H.264");

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

    let decoded =
        decode_middle_pixels(&ffmpeg, &stream, colours.len(), TEST_SIZE, "clipped-colour");
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
    let Some(gpu) = test_gpu() else {
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
    let mut encoder = open_encoder(&gpu, config).expect("NVENC encodes H.264");

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

    let decoded =
        decode_middle_pixels(&ffmpeg, &stream, colours.len(), TEST_SIZE, "clipped-colour");
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
    let Some(gpu) = test_gpu() else {
        return;
    };

    let config = config_for(codec, TEST_SIZE);
    let mut encoder = match open_encoder(&gpu, config) {
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

    report_latency(codec, TEST_SIZE, &latencies, stream.len());

    let file = TempFile::new("clipped-nvenc", extension(codec));
    std::fs::write(file.path(), &stream).expect("the stream can be written");
    probe(codec, TEST_SIZE, TEST_FRAMES, file.path());
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

/// Opens a Direct3D 11 device on the NVIDIA adapter, holding [`SESSIONS`] for
/// as long as it is alive.
///
/// See [`TestGpu::open`] for what a machine with no NVIDIA GPU does here.
fn test_gpu() -> Option<TestGpu> {
    TestGpu::open(Vendor::Nvidia, TEST_SIZE, &SESSIONS)
}

/// Opens an NVENC session against `gpu`.
fn open_encoder(gpu: &TestGpu, config: EncoderConfig) -> Result<NvencEncoder, EncodeError> {
    NvencEncoder::open(&gpu.graphics_device(), config)
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

#[test]
fn the_capability_queries_describe_every_codec_this_card_encodes() {
    // Issue #133: the limits `recorder capabilities` prints come from here on a
    // machine with an NVIDIA card. Nothing asserts a particular number — a 4090
    // is not every card — but an encoder that says it produces a codec has to
    // say how large a picture it takes, whether it has B-frames and whether it
    // encodes 10-bit, or there was no point opening the session.
    let Some(gpu) = test_gpu() else { return };
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
    let Some(gpu) = test_gpu() else { return };

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

/// The refusal issue #443 added to this backend, driven against whatever
/// non-NVIDIA adapter this machine has.
///
/// NVENC was the one hardware backend with no adapter check: AMF and Quick Sync
/// have refused another vendor's device by name for some time, and NVENC left
/// `NvEncOpenEncodeSessionEx` to refuse it with a status that names no adapter.
/// The machine that reaches it is the mirror of the one in that issue — a laptop
/// whose integrated adapter is the default one, so capture lands on Intel or AMD
/// and NVENC is handed that device.
///
/// So this test is also the first observation of what that arrangement does to
/// NVENC on real silicon: nothing here has ever watched
/// `NvEncOpenEncodeSessionEx` be given somebody else's device, which is why the
/// refusal is placed before the runtime is loaded rather than mapped from a
/// status nobody has seen.
///
/// A machine with only NVIDIA adapters has nothing to ask and says so. It takes
/// no session lock: nothing here opens an encoder, because the whole point is
/// that the refusal happens before the runtime is touched.
#[test]
fn a_device_on_another_vendors_adapter_is_refused_by_name() {
    use crate::windows::device::ProbeDevice;
    use crate::windows::dxgi;

    let adapters = dxgi::adapters().expect("DXGI can be asked on a Windows machine");
    let Some(other) = adapters
        .iter()
        .find(|adapter| adapter.can_host_hardware_encoder() && adapter.vendor() != Vendor::Nvidia)
    else {
        // Not `skipped`: `CLIPPED_REQUIRE_ENCODER` is about a machine that has
        // the encoder under test, and this test needs a machine that has
        // somebody else's.
        eprintln!(
            "SKIPPED (adapter): every adapter here is NVIDIA's, so there is no other vendor's \
             device to offer NVENC"
        );
        return;
    };

    let Ok(device) = ProbeDevice::on(other.id()) else {
        eprintln!(
            "SKIPPED (adapter): no Direct3D device could be created on {}",
            other.description()
        );
        return;
    };

    let error = NvencEncoder::open(
        &device.as_graphics_device(),
        config_for(Codec::H264, Resolution::new(1280, 720)),
    )
    .expect_err("NVENC cannot encode a texture belonging to another vendor's adapter");
    let message = error.to_string();

    assert!(
        message.contains("NVENC encodes on NVIDIA graphics")
            && message.contains(&other.vendor().to_string()),
        "the refusal has to name this backend's vendor and the one whose device arrived, \
         rather than a status code that names neither: {message}"
    );
}
