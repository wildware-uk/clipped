//! End-to-end: take screenshots out of a recording that is running, and show
//! that the recording did not notice.
//!
//! # The criterion this makes checkable
//!
//! [Issue #67](https://github.com/wildware-uk/clipped/issues/67)'s third
//! acceptance criterion is that **capture does not interrupt an active
//! recording**. The design says it cannot: a screenshot taken while something
//! is being recorded does not open a second capture of a window that is already
//! being captured — it asks the recording for a frame it has already got
//! (`ScreenshotRequests`, `crates/session/src/recording.rs`). The rendezvous
//! itself is unit-tested.
//!
//! What was missing is the measurement. "It cannot interrupt the recording" is
//! a claim about a capture loop, a Direct3D copy issued on the recording's own
//! device, and an encoder being fed at a fixed rate — and none of those is
//! exercised by a test of the channel between them. A copy that blocked the
//! loop, a texture that was not released, or a readback that stalled the GPU
//! would all leave the rendezvous working perfectly and the recording full of
//! holes.
//!
//! # What it measures
//!
//! One recording of `test-apps/video-pattern`, with screenshots taken out of it
//! at intervals while it runs. Afterwards:
//!
//! - **The recording ran its whole length and ended because it was asked to**,
//!   rather than because something failed.
//! - **It encoded the frames it should have.** A floor rather than an exact
//!   count — a shared machine drops frames for reasons that have nothing to do
//!   with this — but a floor high enough that a stall of even a fraction of a
//!   second around each screenshot would miss it.
//! - **Every screenshot is the pattern the subject drew**, decoded out of the
//!   pixels the recording handed over, so a still that came back sheared, stale
//!   or empty fails rather than being counted.
//! - **The subject's frame numbers advance between screenshots.** This is the
//!   direct evidence: the pattern carries the source's own frame number, so
//!   stills taken a second apart carrying *increasing* numbers mean the thing
//!   being recorded went on being drawn and went on being captured across each
//!   one.
//!
//! # Why it is `#[ignore]`d
//!
//! It puts a window on a display, needs a GPU, a desktop session and an
//! encoder, and writes a video file. Same as every other file in this
//! directory; `tests/capture/README.md` has the reasoning.
//!
//! ```text
//! cargo test -p clipped-video-pattern --test screenshot_during_recording -- --ignored --nocapture
//! ```

use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use std::path::PathBuf;

use clipped_session::screenshot::{ScreenshotRequests, StillFrame};
use clipped_session::{
    record_into, CaptureTargetSettings, RecordingOutputs, RecordingSettings, StopSignal,
};
use clipped_video_pattern::harness::TestApp;
use clipped_video_pattern::pattern::{self, Surface};

/// The rate the subject presents at, and the rate the recording asks for.
const FPS: u32 = 60;

/// How many screenshots are taken out of the running recording.
const SCREENSHOTS: u32 = 4;

/// How long to leave between them.
///
/// Over a second, so that the source's frame number has moved by tens between
/// one still and the next: a gap of a few frames could be a scheduling
/// accident, and this is meant to be unambiguous.
const BETWEEN: Duration = Duration::from_millis(1_200);

/// How long the recording goes on after the last screenshot.
///
/// So that the end of the run is a stretch with no screenshot in it, and the
/// frame count below is not entirely made of the gaps between them.
const TAIL: Duration = Duration::from_secs(1);

/// The shortest the recording can honestly have run.
///
/// The span from the **first** screenshot to the end, not the sum of this
/// test's sleeps. A recording measures itself from its own first frame, which
/// is after the encoder has opened, so its duration is legitimately shorter
/// than the wall clock this thread slept for — 5.71s against 5.8s on the
/// machine this was written on. Everything from the first screenshot onwards is
/// inside the recording by construction, so this is a bound that holds however
/// long the encoder took to open.

/// The share of the frames a perfect run would encode that this one has to.
///
/// Not one, and deliberately: the subject presents on a real machine and the
/// encoder runs on a real GPU, so a run on a busy desktop legitimately loses a
/// few. Two thirds is far below anything a healthy run produces and far above
/// what a recording stalled four times would manage — each screenshot would
/// have to cost less than a fiftieth of the run to stay above it.
const FRAME_FLOOR: f64 = 2.0 / 3.0;

/// See [`RECORD_FOR`]'s documentation.
const RECORD_FOR: Duration = Duration::from_millis(1_200 * (SCREENSHOTS as u64 - 1) + 1_000);

/// How long the application is given to appear.
const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the application is given to stop.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a screenshot waits for the recording to hand a frame over.
///
/// The recording serves one from the frames it is already capturing, so this is
/// generous by an order of magnitude. It exists so that a failure is a failure
/// rather than a hang.
const SERVE_TIMEOUT: Duration = Duration::from_secs(5);

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
fn screenshots_taken_out_of_a_running_recording_do_not_interrupt_it() {
    let app = TestApp::start(
        env!("CARGO_BIN_EXE_video-pattern"),
        [
            "--mode",
            "borderless",
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

    let (width, height) = app.client_size();
    let target = CaptureTargetSettings::window(app.window() as u64, width, height);
    let output = scratch().join("recording.mkv");
    let settings = RecordingSettings::new(target, output.clone()).with_framerate(FPS);

    let requests = ScreenshotRequests::new();
    let stop = Flag::default();

    let report = std::thread::scope(|scope| {
        let recorder = scope.spawn(|| {
            let outputs = RecordingOutputs::default().with_screenshots(&requests);
            record_into(&settings, &stop, &outputs)
        });

        // Everything below runs while the recording above is running, which is
        // the whole point: a screenshot asked for after it stopped would prove
        // nothing about interrupting one.
        let stills = take_screenshots(&requests);
        std::thread::sleep(TAIL);
        stop.raise();

        let report = recorder
            .join()
            .expect("the recording thread does not panic")
            .expect("a window that is drawing can be recorded on this machine");

        check(&stills, width, height);
        report
    });

    println!(
        "\n=== screenshot_during_recording ===\n\
         encoder            : {} {}\n\
         picture            : {}x{} at {} fps\n\
         ran for            : {:.2}s\n\
         ended because      : {:?}\n\
         frames captured    : {}\n\
         frames encoded     : {}\n\
         dropped, writer    : {}\n\
         missed by source   : {}\n\
         screenshots served : {}\n",
        report.encoder(),
        report.codec(),
        report.size().0,
        report.size().1,
        report.requested_framerate(),
        report.duration().as_secs_f64(),
        report.end_reason(),
        report.frames_captured(),
        report.frames_encoded(),
        report.frames_dropped_writer_behind(),
        report.frames_missed_by_source(),
        SCREENSHOTS,
    );

    // The recording ran the whole length and ended because it was asked to. A
    // recording that a screenshot had broken would end for some other reason,
    // and this is what says so before any counting starts.
    assert!(
        report.duration() >= RECORD_FOR,
        "the recording lasted {:?}. Everything from the first screenshot to the stop is inside          it, which is at least {RECORD_FOR:?}, so a shorter recording means it ended early.",
        report.duration()
    );

    let possible = report.duration().as_secs_f64() * f64::from(FPS);
    #[allow(
        clippy::cast_precision_loss,
        reason = "a frame count of a few hundred is exact in f64"
    )]
    let encoded = report.frames_encoded() as f64;
    assert!(
        encoded >= possible * FRAME_FLOOR,
        "{} frames were encoded in {:.2}s at {FPS} fps, which is below the floor of {:.0}. \
         Four screenshots were taken during the run; a recording that stalled while serving \
         one is what this test exists to catch.",
        report.frames_encoded(),
        report.duration().as_secs_f64(),
        possible * FRAME_FLOOR
    );

    assert!(
        output.is_file(),
        "the recording wrote no file at {}",
        output.display()
    );

    app.stop(STOP_TIMEOUT).expect("the application stops");
    let _ = std::fs::remove_dir_all(output.parent().expect("the file is in a directory"));
}

/// Takes [`SCREENSHOTS`] stills out of the running recording, spaced out.
fn take_screenshots(requests: &ScreenshotRequests) -> Vec<StillFrame> {
    let mut stills = Vec::with_capacity(SCREENSHOTS as usize);

    for index in 0..SCREENSHOTS as usize {
        std::thread::sleep(BETWEEN);

        let served = requests
            .take_within(SERVE_TIMEOUT)
            .unwrap_or_else(|error| panic!("screenshot {index} was not served: {error}"));

        println!(
            "screenshot {index}: {}x{}, {} into the recording",
            served.still.size().width(),
            served.still.size().height(),
            served
                .position
                .map_or_else(|| "nowhere yet".to_owned(), |at| format!("{at:.2?}"))
        );
        stills.push(served.still);
    }

    stills
}

/// Every still is the pattern, and the numbers in them advance.
fn check(stills: &[StillFrame], width: u32, height: u32) {
    let mut indices = Vec::with_capacity(stills.len());

    for (nth, still) in stills.iter().enumerate() {
        assert_eq!(
            (still.size().width(), still.size().height()),
            (width, height),
            "screenshot {nth} is not the size of the window that was being recorded"
        );

        let surface = Surface::new(
            still.as_bytes(),
            still.stride(),
            still.size().width(),
            still.size().height(),
        )
        .expect("the still describes its own shape");

        let region = pattern::locate(&surface, still.size().width(), still.size().height())
            .unwrap_or_else(|| {
                panic!("screenshot {nth} does not contain the pattern the subject was drawing")
            });
        let found = pattern::decode(&surface, region)
            .unwrap_or_else(|error| panic!("screenshot {nth} is not a whole pattern: {error}"));

        indices.push(found.index());
    }

    println!("source frames: {indices:?}");

    // The direct evidence. The subject numbers its own frames, so stills taken
    // over a second apart carrying increasing numbers mean it went on drawing
    // and the recording went on capturing across every one of them. Equal or
    // decreasing numbers would mean the recording was serving the same frame
    // over and over — which is exactly what a capture loop stuck behind a
    // screenshot would do, and it would leave every other assertion here
    // passing.
    for pair in indices.windows(2) {
        assert!(
            pair[1] > pair[0],
            "the recording served source frame {} and then {}: it was not capturing new frames \
             between the two screenshots. All of them: {indices:?}",
            pair[0],
            pair[1]
        );
    }
}

/// A directory of this test's own for the recording it writes.
fn scratch() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "clipped-screenshot-during-recording-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("a scratch directory");
    path
}
