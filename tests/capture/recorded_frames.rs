//! The frames in a finished recording are the frames the source drew, in order.
//!
//! [Issue #183](https://github.com/wildware-uk/clipped/issues/183).
//! `apps/recorder/tests/record_end_to_end.rs` already asserts that every frame
//! the recorder said it encoded decodes back out of the file, and that the
//! codec, size, duration and timestamps are what it reported. What none of that
//! can say is whether those pictures are the *source's* frames: a pipeline that
//! wrote one frame twice, or wrote them out of order, produces a file with
//! exactly the right count of perfectly decodable pictures.
//!
//! `video-pattern` draws a decodable counter into every frame it presents, so
//! the answer is in the pixels. This records the subject, extracts the recorded
//! pictures, reads the counters back out of them and asks
//! [`CounterRun`](clipped_video_pattern::sequence::CounterRun) the three
//! questions a frame count cannot answer.
//!
//! # Why the test lives here
//!
//! It cannot live beside the recording tests. `clipped-video-pattern` sits
//! *above* `clipped-recorder` in the workspace layering, so that nothing in the
//! product can depend on a test application
//! (`tests/integration/tests/workspace_layering.rs`, and a dev-dependency
//! counts). A test that needs both the recorder and the pattern decoder has to
//! be owned by the crate at the top, which is this one — and reimplementing the
//! decoder further down is the duplication AGENTS.md section 55 exists to
//! prevent. That is why #183 was raised rather than written into
//! `record_end_to_end.rs`.
//!
//! # What it does not claim
//!
//! That no frame is ever missing. A recorder holding a rate below the source's
//! drops frames on purpose, and one whose writer falls behind drops them and
//! *says so* — `RecordingReport` carries both counts. So the missing counters
//! are checked against what the recorder itself reported rather than against
//! zero, and it is duplication and disorder that are held to zero, because
//! neither has an honest cause.
//!
//! ```text
//! cargo test -p clipped-video-pattern --test recorded_frames -- --ignored --nocapture
//! ```

#![cfg(windows)]

use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use clipped_session::{
    record_into, CaptureTargetSettings, RecordingOutputs, RecordingSettings, StopSignal,
};
use clipped_test_exclusion::{Exclusive, Resource};
use clipped_video_pattern::harness::TestApp;
use clipped_video_pattern::pattern::{self, Region, Surface};
use clipped_video_pattern::sequence::CounterRun;

/// The rate the subject presents at, and the rate the recording asks for.
///
/// The same on both sides deliberately. A recording asked for half the
/// source's rate is *supposed* to be missing every other counter, which makes
/// "a missing counter" mean two things at once; asking for the rate the source
/// draws at leaves a gap meaning one thing — a frame that did not make it.
const FPS: u32 = 60;

/// How long to record for.
///
/// Long enough that the numbers are about the pipeline rather than about its
/// first moments: at [`FPS`] this is around 240 frames.
const RECORD_FOR: Duration = Duration::from_secs(4);

/// How long the subject is given to put its window up and say so.
const READY_TIMEOUT: Duration = Duration::from_secs(20);

/// The fraction of the run that may be missing before this fails, on top of
/// whatever the recorder reported dropping itself.
///
/// Frames go missing for reasons that are not faults in the ordering this test
/// is about — a compositor that skipped a present, a capture that arrived while
/// the encoder was busy — and the recorder counts the ones it knows about. This
/// is the allowance for the ones nobody counted, and it is deliberately small:
/// the point of the bound is that a run which lost a *tenth* of its frames is
/// not evidence about frame order, whatever else it is.
const UNEXPLAINED_ALLOWANCE: f64 = 0.05;

/// A stop signal a scope can raise.
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

/// A directory of this test's own.
fn scratch() -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "clipped-recorded-frames-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("a scratch directory can be made");
    directory
}

/// Reads the counters out of every picture in `recording`.
///
/// The frames are streamed rather than collected. Four seconds of 1280x720
/// BGRA is about 885 MB, and a test that held all of it to look at each frame
/// once would be measuring this machine's memory pressure alongside the
/// recorder's frame order.
///
/// The pattern is located once, in the first frame that holds it, and that
/// region is used for the rest: the subject does not move within the picture
/// during a recording, and searching every frame would be searching for
/// something already found.
fn counters_in(
    ffmpeg: &Path,
    recording: &Path,
    width: u32,
    height: u32,
) -> (CounterRun, Vec<String>) {
    let mut child = Command::new(ffmpeg)
        .args(["-v", "error", "-i"])
        .arg(recording)
        // Exactly the frames the file holds, and no others.
        //
        // Without this ffmpeg resamples to a constant rate on the way out and
        // **pads with duplicates** wherever the timestamps are further apart
        // than the declared rate — so a recording made while the machine was
        // busy comes back with thousands of repeated pictures that were never
        // in it. Measured: a contended run of this test reported 3568
        // duplicates against 228 distinct counters, which reads as the recorder
        // writing every frame fifteen times and is entirely this flag
        // (issue #194).
        .args(["-fps_mode", "passthrough"])
        // Raw BGRA, which is the shape `Surface` reads and the shape the
        // pattern was drawn in. Anything with chroma subsampling would decide
        // the pattern's fate before the decoder saw it.
        .args(["-f", "rawvideo", "-pix_fmt", "bgra", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the pinned ffmpeg can be started");

    let mut stdout = child.stdout.take().expect("stdout was piped");
    let stride = width as usize * 4;
    let mut frame = vec![0u8; stride * height as usize];

    let mut run = CounterRun::new();
    let mut region: Option<Region> = None;
    let mut undecodable: Vec<String> = Vec::new();

    loop {
        match stdout.read_exact(&mut frame) {
            Ok(()) => {}
            // The end of the stream, which is the end of the recording. A
            // partial frame is the same thing: ffmpeg does not write half a
            // picture, so a short read is the tail of a complete one.
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(error) => panic!("the frames could not be read from ffmpeg: {error}"),
        }

        let surface =
            Surface::new(&frame, stride, width, height).expect("a frame ffmpeg wrote is a surface");

        let found = match region {
            Some(known) => Some(known),
            None => {
                region = pattern::locate(&surface, width, height);
                region
            }
        };

        let Some(known) = found else {
            // Before the pattern has been found at all. The first frames of a
            // recording can be the window before it has drawn, and those are
            // not undecodable frames — they are frames with nothing in them
            // yet.
            continue;
        };

        match pattern::decode(&surface, known) {
            Ok(decoded) => run.record(decoded.index()),
            Err(error) if undecodable.len() < 8 => undecodable.push(error.to_string()),
            Err(_) => undecodable.push(String::new()),
        }
    }

    let finished = child.wait_with_output().expect("ffmpeg can be waited for");
    assert!(
        finished.status.success(),
        "ffmpeg could not read the recording: {}",
        String::from_utf8_lossy(&finished.stderr)
    );

    (run, undecodable)
}

#[test]
#[ignore = "records a real window through a real encoder; see the module documentation"]
fn the_frames_in_a_recording_are_the_frames_the_source_drew_in_order() {
    let Some(tools) = clipped_media_validation::require_media_tools() else {
        return;
    };

    // One capture measurement on this machine at a time (issue #194). Held for
    // the whole test, because what it protects is the frame accounting at the
    // end rather than the recording at the start: run beside a second capture
    // suite, this test reports the recorder writing a frame twice, which is a
    // bug report about the recorder and would be wrong.
    let _measuring = Exclusive::acquire(Resource::CaptureMeasurement)
        .unwrap_or_else(|contended| panic!("{contended}"));

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
    let directory = scratch();
    let output = directory.join("recording.mkv");
    let settings = RecordingSettings::new(target, output.clone()).with_framerate(FPS);

    let stop = Flag::default();
    let report = std::thread::scope(|scope| {
        let recorder = scope.spawn(|| record_into(&settings, &stop, &RecordingOutputs::default()));
        std::thread::sleep(RECORD_FOR);
        stop.raise();
        recorder
            .join()
            .expect("the recording thread does not panic")
            .expect("a window that is drawing can be recorded on this machine")
    });

    let (encoded_width, encoded_height) = report.size();
    let (run, undecodable) = counters_in(tools.ffmpeg(), &output, encoded_width, encoded_height);

    // What the recorder itself says it lost, which is the part of any gap that
    // is accounted for.
    let admitted = report.frames_dropped_writer_behind() + report.frames_missed_by_source();
    let allowance = admitted + (run.presented() as f64 * UNEXPLAINED_ALLOWANCE).ceil() as u64;

    println!(
        "\n=== recorded_frames ===\n\
         encoder            : {} {}\n\
         picture            : {encoded_width}x{encoded_height} at {} fps\n\
         ran for            : {:.2}s\n\
         frames encoded     : {}\n\
         pictures decoded   : {}\n\
         counters           : {:?} to {:?} ({} presented)\n\
         missing            : {} (recorder admitted {admitted}, allowance {allowance})\n\
         duplicated         : {}\n\
         out of order       : {}\n\
         undecodable        : {}\n",
        report.encoder(),
        report.codec(),
        report.requested_framerate(),
        report.duration().as_secs_f64(),
        report.frames_encoded(),
        run.decoded(),
        run.first(),
        run.last(),
        run.presented(),
        run.missing(),
        run.duplicated(),
        run.out_of_order(),
        undecodable.len(),
    );

    assert!(
        run.decoded() > 0,
        "no picture in the recording held the pattern, so either the wrong window was recorded \
         or nothing was: {} frames were encoded",
        report.frames_encoded()
    );

    // Enough to be measuring the pipeline rather than its first moments.
    let expected = u64::from(FPS) * RECORD_FOR.as_secs();
    assert!(
        run.decoded() >= expected / 3,
        "only {} pictures decoded from {:.0}s of recording a {FPS} fps source, which is too few \
         to conclude anything about frame order; expected around {expected}",
        run.decoded(),
        RECORD_FOR.as_secs_f64()
    );

    assert!(
        undecodable.is_empty(),
        "{} picture(s) in the recording did not decode as the pattern once it had been found, \
         which means what was written is not what was drawn: {undecodable:?}",
        undecodable.len()
    );

    // The two with no honest cause. A recorder may drop a frame; it may never
    // write one twice, and it may never write them out of the order the source
    // drew them in.
    assert_eq!(
        run.duplicated(),
        0,
        "the recording holds the same source frame more than once, which is a frame written \
         twice rather than a frame dropped: counters {:?} to {:?}",
        run.first(),
        run.last()
    );
    assert_eq!(
        run.out_of_order(),
        0,
        "the recording holds source frames in an order the source did not draw them in: \
         counters {:?} to {:?}",
        run.first(),
        run.last()
    );

    assert!(
        run.missing() <= allowance,
        "{} of the {} source frames in this span are missing from the recording, and the \
         recorder admitted only {admitted} of them — so {} went missing without anything \
         counting them, which is more than the {:.0}% this test allows",
        run.missing(),
        run.presented(),
        run.missing().saturating_sub(admitted),
        UNEXPLAINED_ALLOWANCE * 100.0
    );

    let _ = std::fs::remove_dir_all(&directory);
}
