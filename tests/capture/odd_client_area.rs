//! End-to-end: record a window whose client area has an odd dimension, and get
//! a file rather than a failure.
//!
//! # The defect this reproduces
//!
//! [Issue #561](https://github.com/wildware-uk/clipped/issues/561). 4:2:0 chroma
//! samples colour at half resolution in both directions, so an odd width or
//! height has no representation in it, and all four encoders in
//! `clipped-encoder` refuse one by name. Nothing between the capture backend and
//! the encoder used to do anything about it, and an ordinary bordered window
//! sized to 1000x600 has a client area of **986x593** on Windows 11 at 96 DPI —
//! so a perfectly ordinary window could not be recorded at all. It failed before
//! its first frame with `encoder-unavailable`, and under
//! [ADR 0012](../../docs/adr/0012-a-session-follows-a-resize-with-a-new-file.md)
//! a window *resized* into that shape failed every recording for the rest of the
//! sitting: measured on 2026-08-17, three consecutive failures over 24 seconds,
//! five seconds apart, until the game exited.
//!
//! Run against the fix reverted, this test fails at the `expect` on
//! `record_into` with "no encoder could be opened", which is exactly what the
//! issue measured.
//!
//! # What it measures
//!
//! One three-second recording of `test-apps/video-pattern`, whose window is a
//! borderless 986x593 — so the client area is exactly the odd shape, with no
//! dependence on how thick this Windows version draws a border.
//!
//! - **The recording happens at all**, and reports 986x592: the largest picture
//!   inside the window that 4:2:0 chroma can represent.
//! - **The file says the same thing.** The track's dimensions are read back out
//!   of the container with `ffprobe` and have to be 986x592, and its pictures
//!   have to decode at that size — because a track that declared 986x593 while
//!   carrying 986x592 pictures is the failure AGENTS.md section 22 is about, and
//!   a rounding applied only to the encoder's configuration would produce
//!   exactly that.
//! - **The pictures are real.** A floor on decoded frames, so a file with a
//!   correct header and nothing in it fails.
//!
//! What it deliberately does not measure is *which* row went: that is one
//! `CopySubresourceRegion` with a fixed box, and it is checked pixel by pixel
//! without a window or an encoder in `clipped_capture::windows::crop`.
//!
//! # Why it is `#[ignore]`d
//!
//! It puts a window on a display, needs a GPU, a desktop session and an encoder,
//! and writes a video file. Same as every other file in this directory;
//! `tests/capture/README.md` has the reasoning.
//!
//! ```text
//! cargo test -p clipped-video-pattern --test odd_client_area -- --ignored --nocapture
//! ```

use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use std::path::PathBuf;

use clipped_media_validation::{require_media_tools, Media, VideoStream};
use clipped_session::{
    record_into, CaptureTargetSettings, RecordingOutputs, RecordingSettings, StopSignal,
};
use clipped_video_pattern::harness::TestApp;

/// The client area of an ordinary bordered window sized to 1000x600 on Windows
/// 11 at 96 DPI, which is the measurement on issue #561.
const CLIENT: (u32, u32) = (986, 593);

/// What that window can be recorded at, and nothing else can.
const RECORDED: (u32, u32) = (986, 592);

/// The rate the subject presents at, and the rate the recording asks for.
const FPS: u32 = 30;

/// How long the recording runs.
///
/// Long enough that a floor on the frames in the file means something, short
/// enough to belong in a suite somebody runs before a pull request.
const RECORD_FOR: Duration = Duration::from_secs(3);

/// The share of the frames a perfect run would encode that the file has to hold.
///
/// A real machine drops some, and this test is not about frame rate; a third is
/// far below any healthy run and far above the empty file a broken one leaves.
const FRAME_FLOOR: u64 = (RECORD_FOR.as_secs() * FPS as u64) / 3;

/// How long the application is given to appear.
const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the application is given to stop.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// A stop the test can raise from another thread.
#[derive(Debug, Default)]
struct Flag(AtomicBool);

impl Flag {
    fn raise(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl StopSignal for Flag {
    fn is_requested(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[test]
#[ignore = "records a real window through a real encoder; see the module documentation"]
fn a_window_with_an_odd_client_area_is_recorded_one_row_short_of_it_rather_than_not_at_all() {
    let Some(_tools) = require_media_tools() else {
        return;
    };

    let app = TestApp::start(
        env!("CARGO_BIN_EXE_video-pattern"),
        [
            "--mode",
            "borderless",
            "--size",
            &format!("{}x{}", CLIENT.0, CLIENT.1),
            "--fps",
            &FPS.to_string(),
            // A backstop well beyond the recording, so a panicking test still
            // leaves nothing on screen.
            "--seconds",
            "120",
        ],
        READY_TIMEOUT,
    )
    .expect("the video pattern application should start and announce itself");

    // The premise. A borderless window's client area is the size it was created
    // at; if that ever stops being true this test is measuring something else.
    assert_eq!(
        app.client_size(),
        CLIENT,
        "this test is about a window whose client area has an odd dimension"
    );

    let (width, height) = app.client_size();
    let target = CaptureTargetSettings::window(app.window() as u64, width, height);
    let output = scratch().join("recording.mkv");
    let settings = RecordingSettings::new(target, output.clone()).with_framerate(FPS);
    let stop = Flag::default();

    let report = std::thread::scope(|scope| {
        let recorder = scope.spawn(|| record_into(&settings, &stop, &RecordingOutputs::default()));
        std::thread::sleep(RECORD_FOR);
        stop.raise();
        recorder
            .join()
            .expect("the recording thread does not panic")
    })
    // This is the defect. Before issue #561 the failure arrived here, as
    // `encoder-unavailable`: every encoder refused 986x593, and the recording
    // ended before its first frame with no file at all.
    .expect("a window whose client area has an odd dimension must still be recordable");

    println!(
        "\n=== odd_client_area ===\n\
         window          : {}x{}\n\
         encoder         : {} {}\n\
         recorded        : {}x{}\n\
         ran for         : {:.2}s\n\
         ended because   : {:?}\n\
         frames captured : {}\n\
         frames encoded  : {}\n",
        CLIENT.0,
        CLIENT.1,
        report.encoder(),
        report.codec(),
        report.size().0,
        report.size().1,
        report.duration().as_secs_f64(),
        report.end_reason(),
        report.frames_captured(),
        report.frames_encoded(),
    );

    assert_eq!(
        report.size(),
        RECORDED,
        "a {}x{} window is recorded at the largest even size inside it, so that the one row \
         that cannot be encoded is the only thing lost",
        CLIENT.0,
        CLIENT.1
    );
    assert!(
        report.frames_encoded() >= FRAME_FLOOR,
        "the recording encoded {} frames in {:.2}s at {FPS} fps, which is below the floor of \
         {FRAME_FLOOR}",
        report.frames_encoded(),
        report.duration().as_secs_f64()
    );

    // And the file agrees. The track's dimensions go in the Matroska header and
    // are what a player believes; a recording that declared the window's own odd
    // size while carrying pictures a row shorter would pass every assertion
    // above and be wrong (AGENTS.md section 22).
    let media = Media::open(&output)
        .unwrap_or_else(|error| panic!("the recording is not usable at all: {error}"));
    media
        .validate()
        .video_stream_count(1)
        .video(
            VideoStream::default()
                .resolution(RECORDED.0, RECORDED.1)
                .decoded_frames_at_least(FRAME_FLOOR),
        )
        .monotonic_timestamps()
        .assert_valid();

    app.stop(STOP_TIMEOUT).expect("the application stops");
    let _ = std::fs::remove_dir_all(output.parent().expect("the file is in a directory"));
}

/// A directory of this test's own for the recording it writes.
fn scratch() -> PathBuf {
    let path = std::env::temp_dir().join(format!("clipped-odd-client-area-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("a scratch directory");
    path
}
