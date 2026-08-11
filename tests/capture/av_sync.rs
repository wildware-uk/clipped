//! End-to-end: capture video and system audio at the same time, and measure how
//! far apart they drift.
//!
//! This is the measurement behind `docs/av-sync.md`. Everything else about
//! synchronisation can be argued from first principles — both Windows capture
//! APIs stamp frames with the performance counter, WASAPI reports positions
//! against the same counter, so the two are comparable — but the argument stops
//! at the audio *device*, whose sample clock is a crystal on a sound card and
//! runs at its own rate. Whether that rate matters is an empirical question, and
//! this file is how it is answered.
//!
//! # What it measures, and why that is the right quantity
//!
//! Every buffer `clipped-audio` hands over carries two accounts of the same
//! moment:
//!
//! - `timestamp()` — where the *track* puts it, which is the capture's anchor
//!   plus every frame emitted since, so the track is contiguous and as long as
//!   the recording;
//! - `device_timestamp()` — where the *endpoint* said it belongs, a
//!   performance-counter position, which is the same clock the video frames
//!   arriving on the other thread are stamped with.
//!
//! Their difference is how far the track has moved against the performance
//! counter, in nanoseconds, at that moment. Video timestamps are readings of
//! that same counter, so the way that difference grows over the run is the way
//! the audio moves against the picture, and its slope is the drift rate — the
//! number that decides whether a two-hour recording *ends* as well aligned as it
//! started.
//!
//! # What it cannot measure
//!
//! Three things, and the printed result means much less without them.
//!
//! **A constant offset.** The measurement is relative. `clipped-audio` anchors
//! its track on the first packet's own device position, so the first observation
//! of a run is zero by construction, and everything after it is measured from
//! there. An error that was already present when the capture started — the
//! endpoint's own reporting bias, or a session that starts the audio at a
//! different moment from the video — is invisible here at any size. What is
//! measured is the change.
//!
//! **What a file ends up containing.** Nothing here writes one. `clipped-muxer`
//! rescales media times to 1 ms container ticks and clamps any timestamp before
//! the file's origin to the start of it (`docs/muxing.md`), and both of those
//! alter the offset a finished recording has in it. This measures the
//! timestamps the pipeline produces, which is what a writer is given, not what
//! it wrote.
//!
//! **Physical synchronisation** — whether the sound leaving the speakers and the
//! light leaving the panel are simultaneous. That needs a microphone and a
//! photodiode, and it would be measuring Windows' output latency rather than
//! this recorder.
//!
//! What is in scope is the timestamp domain: the recorder is responsible for
//! placing each source at the moment its own hardware said it happened, and that
//! is what is asserted. The fitted rate is printed with its standard error, so
//! that the part of it which is scatter rather than crystal is on the same line.
//!
//! # Why the endpoint is kept awake
//!
//! WASAPI loopback delivers nothing at all while the endpoint is idle, and a
//! period the device never described has no device position to disagree with —
//! so a run on a quiet machine measures nothing. [`SilenceKeeper`] holds a
//! render stream open for the length of the run and hands the audio engine
//! buffers marked as silence. That makes no sound whatsoever: it is not a quiet
//! tone, it is `AUDCLNT_BUFFERFLAGS_SILENT`, which is the audio engine's own way
//! of saying "these frames are zero". The endpoint's clock runs, packets flow,
//! and nobody hears anything.
//!
//! # Why it is `#[ignore]`d
//!
//! It needs a GPU, a display, an audio endpoint and minutes of wall-clock time,
//! and it puts a window on a display. Being `#[ignore]`d rather than deciding at
//! run time whether it applies is deliberate: what runs it is a person or a job
//! that meant to, rather than a filter inside the test.
//!
//! It does report two conditions and carry on rather than failing — the default
//! output device refusing a render stream, and an endpoint that delivered no
//! packets at all — because both mean the machine could not present the
//! measurement with anything to measure, and neither is a fault in the code
//! under test. Both print `SKIPPED (av-sync): …`, and both fail the run outright
//! when `CLIPPED_REQUIRE_AUDIO` is set, which is how CI and anybody collecting
//! evidence should run it. A green run without that variable set is not on its
//! own evidence that an offset was measured; the printed lines are.
//!
//! ```text
//! # The default: about ninety seconds.
//! cargo test -p clipped-video-pattern --test av_sync -- --ignored --nocapture --test-threads=1
//!
//! # The long run the acceptance criteria ask for. CLIPPED_AV_SYNC_SECONDS is
//! # read by the same test.
//! CLIPPED_AV_SYNC_SECONDS=1800 cargo test -p clipped-video-pattern --test av_sync \
//!     -- --ignored --nocapture --test-threads=1 av_offset
//! ```

#![cfg(windows)]

mod readback;

use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use std::io::Write as _;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use clipped_audio::windows::SystemAudioCapture;
use clipped_audio::{Capture, SampleOrigin};
use clipped_capture::{
    registered_backend, registered_declarations, select, Acquisition, CaptureClock, CaptureConfig,
    CaptureError, CaptureTarget, CaptureTimestamp, DriftEstimator, FrameSize, MediaTime,
    SourceClock, SyncState, SyncTolerance, TargetHandle, TargetKind, TargetProperties,
    DEFAULT_DISCONTINUITY_STEP,
};
use clipped_video_pattern::harness::TestApp;
use clipped_video_pattern::pattern::{self, Surface};

use readback::FrameReader;

use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioClient, IAudioRenderClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoIncrementMTAUsage, CoTaskMemFree, CLSCTX_ALL,
};

/// How long a run lasts unless [`RUN_SECONDS`] says otherwise.
///
/// Ninety seconds is long enough for the fit to mean something — nine thousand
/// audio packets — and short enough to be worth running on a whim. The thirty
/// minutes the acceptance criteria ask for is a deliberate act, not a default
/// somebody trips over.
const DEFAULT_RUN: Duration = Duration::from_secs(90);

/// The environment variable that lengthens a run, in seconds.
const RUN_SECONDS: &str = "CLIPPED_AV_SYNC_SECONDS";

/// The environment variable that turns "this machine has no audio endpoint"
/// from a skip into a failure.
const REQUIRE_AUDIO: &str = "CLIPPED_REQUIRE_AUDIO";

/// The frames per second the test pattern is presented at.
///
/// Thirty rather than sixty: the point of the subject is that the compositor
/// always has new content for the whole run, and thirty is enough for that at
/// half the cost on a machine somebody is using.
const PATTERN_FPS: u32 = 30;

/// One frame in this many is copied back and decoded.
///
/// Decoding every frame of a thirty-minute run would be fifty thousand
/// full-frame GPU readbacks, which measures the readback rather than the
/// capture. One a second is enough to prove the frames arriving are the
/// subject's, and enough to fit the source's presentation interval against the
/// reference clock.
const DECODE_EVERY: u64 = PATTERN_FPS as u64;

/// The largest drift rate that is a measurement rather than a fault, in parts
/// per million.
///
/// A commodity audio crystal is specified at ±50 ppm and the worst consumer
/// parts are a few hundred. A thousand — 60 ms per minute — is not a clock, it
/// is a broken conversion somewhere in this pipeline, and the test should say so
/// rather than dutifully reporting it.
const IMPLAUSIBLE_PPM: f64 = 1_000.0;

/// How many times the subject is started again after its window goes away.
///
/// It goes away for reasons this test does not control: the machine it runs on
/// belongs to somebody, the subject is a topmost window on one of their
/// displays, and half an hour is long enough for them to close it or for the
/// session to lock. Losing the whole run to that would mean the long
/// measurement could only be taken on an idle machine, which is not where
/// recording software has to work.
///
/// Three, not unlimited: if somebody is closing the window, putting it back for
/// half an hour is worse than stopping. After the third loss the video side
/// gives up and says so, and the audio side — which is where the offset is
/// actually measured — carries on.
const MAX_SUBJECT_RESTARTS: u32 = 3;

/// How much of a run the video capture has to cover for the run to count.
///
/// The A/V offset is measured from the audio endpoint's positions against the
/// reference clock, and that continues whatever happens to the window, so a
/// subject that dies late does not invalidate the measurement. What it does
/// invalidate is the claim that video and audio were captured *together*, so
/// there is a floor: half the run.
///
/// Coverage, not the interval from the first frame to the last. Those are the
/// same number until a subject has to be restarted, and after that the interval
/// includes the time nothing was being captured — which is exactly the case the
/// floor exists to catch.
const MINIMUM_VIDEO_COVERAGE: f64 = 0.5;

/// Reports that the test could not run here.
///
/// Written through `std::io::stderr()` rather than with `eprintln!` because
/// libtest captures the macros.
fn skipped(reason: &str) {
    if std::env::var_os(REQUIRE_AUDIO).is_some_and(|value| !value.is_empty()) {
        panic!("{REQUIRE_AUDIO} is set, so this must not be skipped: {reason}");
    }
    let _ = writeln!(std::io::stderr(), "SKIPPED (av-sync): {reason}");
}

fn note(message: &str) {
    let _ = writeln!(std::io::stderr(), "[av-sync] {message}");
}

fn run_length() -> Duration {
    match std::env::var(RUN_SECONDS).ok().and_then(|value| {
        let seconds: u64 = value.trim().parse().ok()?;
        (seconds > 0).then(|| Duration::from_secs(seconds))
    }) {
        Some(length) => length,
        None => DEFAULT_RUN,
    }
}

#[test]
#[ignore = "needs a GPU, a display, an audio endpoint and minutes of wall-clock time"]
fn av_offset_stays_within_tolerance_while_video_and_audio_are_captured_together() {
    let run = run_length();

    let keeper = match SilenceKeeper::start() {
        Ok(keeper) => Some(keeper),
        Err(reason) => {
            // Not fatal on its own: the run still measures the video side and
            // whatever the machine happens to be playing. It is reported
            // loudly, because a run with no endpoint packets measures no drift.
            skipped(&format!("the endpoint could not be kept awake: {reason}"));
            None
        }
    };

    let mut audio = AudioRun::start();

    let video = capture_video(run);

    let audio = audio.finish();
    drop(keeper);

    let report = Report::build(&video, &audio);
    report.print();
    report.assert_healthy(run);
}

/// What the video side of a run produced.
#[derive(Debug, Default)]
struct VideoRun {
    /// Every delivered frame's source timestamp, in arrival order.
    timestamps: Vec<CaptureTimestamp>,
    /// How much of the run the capture actually had frames for: the sum of each
    /// subject's own first-to-last span, so that the downtime between a subject
    /// dying and the next one starting is not counted as covered.
    covered: Duration,
    /// How many subjects delivered at least one frame, which is how many pieces
    /// [`covered`](Self::covered) is the sum of.
    pieces: u32,
    /// `(pattern counter, source timestamp)` for the sampled frames of the
    /// longest single run of the subject. Only one run's worth, because the
    /// counter restarts at zero with the subject and a fit spanning a restart
    /// would read the reset as time going backwards.
    decoded: Vec<(u32, CaptureTimestamp)>,
    /// Sampled frames that decoded as the pattern, across every run.
    decoded_total: u64,
    timeouts: u64,
    undecodable: u64,
    backend_missed: u64,
    /// How many times the subject had to be started again because its window
    /// went away. See [`MAX_SUBJECT_RESTARTS`].
    restarts: u32,
    /// Which display the subject was last on.
    monitor: String,
    /// What ended the subject's own run, in its words.
    stopped: Option<String>,
}

/// What the audio side of a run produced.
#[derive(Debug, Default)]
struct AudioRun {
    /// `(endpoint position, track position)` in nanoseconds on the performance
    /// counter, one pair per buffer the endpoint delivered.
    observations: Vec<(u64, u64)>,
    /// Frames of silence this crate synthesised because the endpoint said
    /// nothing.
    synthesised_frames: u64,
    /// Frames handed over in total, real and synthesised.
    frames: u64,
    sample_rate: u32,
    /// Every endpoint the capture was on, in the order it was on them. More
    /// than one means the default output device changed under the run.
    endpoints: Vec<String>,
    /// How many times the capture reported that it could not follow the new
    /// default endpoint and continued as silence.
    format_changes: u64,
    failed: Option<String>,
}

/// The audio side, running on its own thread for the length of the run.
///
/// One capture, one thread, as `clipped-audio`'s module documentation requires:
/// `SystemAudioCapture` is `Send` and not `Sync`, and reading it blocks, so it
/// is opened on the thread that will read it and never touched from anywhere
/// else. Nothing is shared with the video thread except an [`AtomicBool`].
#[derive(Debug)]
struct AudioThread {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<AudioRun>>,
}

impl AudioRun {
    fn start() -> AudioThread {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = std::thread::spawn({
            let stop = Arc::clone(&stop);
            move || Self::run(&stop)
        });
        AudioThread {
            stop,
            thread: Some(thread),
        }
    }

    fn run(stop: &AtomicBool) -> Self {
        let mut run = Self::default();

        let mut capture = match SystemAudioCapture::open() {
            Ok(capture) => capture,
            Err(error) => {
                run.failed = Some(format!("system audio capture could not be opened: {error}"));
                return run;
            }
        };
        // The track's format, not the endpoint's. `clipped-audio` never changes
        // the shape of what it hands over mid-capture: an endpoint it cannot
        // follow becomes silence in this format and is reported as
        // `FormatChanged` (`crates/audio/src/error.rs`). So this rate stays the
        // right divisor for the frame count for the whole run.
        run.sample_rate = capture.format().sample_rate().get();
        let mut endpoint = capture.endpoint_name().unwrap_or("<none>").to_owned();
        run.endpoints.push(endpoint.clone());

        while !stop.load(Ordering::Relaxed) {
            match capture.read(Duration::from_millis(100)) {
                Ok(Capture::Samples(samples)) => {
                    run.frames += samples.frames() as u64;
                    if samples.origin() == SampleOrigin::SynthesisedSilence {
                        run.synthesised_frames += samples.frames() as u64;
                    }
                    if let Some(device) = samples.device_timestamp() {
                        run.observations
                            .push((device.as_nanos(), samples.timestamp().as_nanos()));
                    }
                }
                Ok(Capture::Idle) => {}
                Ok(Capture::FormatChanged(format)) => {
                    // The default endpoint moved to one this capture cannot
                    // follow, so the track continues as silence in the original
                    // format. There are no more device positions to compare
                    // against from here on, which is worth saying loudly: the
                    // rest of the run measures nothing.
                    run.format_changes += 1;
                    note(&format!(
                        "the default output device changed to {format}, which this capture \
                         cannot follow; the track continues as silence and no further A/V \
                         offset can be observed"
                    ));
                }
                Err(error) => {
                    run.failed = Some(format!("the audio capture failed mid-run: {error}"));
                    break;
                }
            }

            // Read after the match, because the borrow the samples held on the
            // capture has ended by here. An endpoint can also be replaced by an
            // *interchangeable* one — same rate, same channels — which the
            // capture follows silently and reports through no event at all. It
            // is still a different crystal, so the report has to name every
            // device it was on rather than the one it opened with.
            let current = capture.endpoint_name().unwrap_or("<none>");
            if current != endpoint {
                note(&format!(
                    "the endpoint changed from {endpoint:?} to {current:?} mid-run; the \
                     drift measured before the change is a different crystal's"
                ));
                endpoint = current.to_owned();
                run.endpoints.push(endpoint.clone());
            }
        }

        capture.close();
        run
    }
}

impl AudioThread {
    fn finish(&mut self) -> AudioRun {
        self.stop.store(true, Ordering::Relaxed);
        self.thread
            .take()
            .expect("a run is finished once")
            .join()
            .expect("the audio thread should not panic")
    }
}

impl Drop for AudioThread {
    fn drop(&mut self) {
        // Only reached when the video side panicked, which is exactly when a
        // thread holding an open audio endpoint must not be left running
        // (AGENTS.md section 58).
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Starts the subject, asking it to outlive the rest of the run by a margin so
/// that the measurement is never cut short by the application's own deadline.
///
/// It still stops when this test closes its standard input, and `TestApp::drop`
/// kills it if it does not, so the margin costs nothing.
fn start_subject(remaining: Duration) -> Result<TestApp, String> {
    TestApp::start(
        env!("CARGO_BIN_EXE_video-pattern"),
        [
            "--fps".to_owned(),
            PATTERN_FPS.to_string(),
            "--seconds".to_owned(),
            (remaining.as_secs() + 120).to_string(),
            "--mode".to_owned(),
            "borderless".to_owned(),
        ],
        Duration::from_secs(20),
    )
    .map_err(|error| error.to_string())
}

/// Captures the subject's window for `run`, keeping every frame's timestamp and
/// decoding one frame a second.
///
/// Owns the subject, because it may have to start it again: a window on a
/// display belonging to somebody who is using the machine can be closed, and a
/// half-hour run that ends the moment that happens measures nothing. A lost
/// target is reported, the subject is started again, and the run continues —
/// [`MAX_SUBJECT_RESTARTS`] times, after which the video side gives up and the
/// audio side, which is where the offset is actually measured, carries on to the
/// deadline.
fn capture_video(run: Duration) -> VideoRun {
    let mut video = VideoRun::default();
    let deadline = Instant::now() + run;
    let mut method = None;

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }

        let app = match start_subject(remaining) {
            Ok(app) => app,
            Err(reason) => {
                note(&format!("the subject could not be started: {reason}"));
                break;
            }
        };
        video.monitor = app.monitor().to_owned();

        let (width, height) = app.client_size();
        let size = FrameSize::new(width, height).expect("the application announced a real size");
        let properties = TargetProperties::new(TargetKind::Window, size);

        let selection = select(
            &registered_declarations(),
            &properties,
            clipped_capture::CaptureMethodSetting::Automatic,
        )
        .expect("this machine should have a capture backend for a window");
        let factory = registered_backend(selection.method())
            .expect("selection only ever chooses a registered backend");
        let mut backend = factory.create().expect("the backend should be creatable");

        let target = CaptureTarget::new(TargetHandle::from_raw(app.window() as u64), properties);
        let format = backend
            .initialise(
                &target,
                &CaptureConfig::default().with_capture_cursor(false),
            )
            .expect("capturing the test application's window should start");
        if method.is_none() {
            note(&format!(
                "capturing {} through {} at {format} for {:.0} s",
                video.monitor,
                selection.method(),
                run.as_secs_f64()
            ));
            method = Some(selection.method());
        }

        // The counter is the subject's own and restarts at zero with it, so the
        // frames decoded from one subject are fitted separately from the next
        // one's — a fit spanning a restart would read the reset as a jump back
        // in time.
        let mut decoded_here = Vec::new();
        let mut region = None;
        // A new subject means a new backend and therefore a new Direct3D
        // device, and the reader caches the device and the staging texture it
        // copies into. Carrying one across a restart is reading a texture on a
        // device that no longer exists, which fails as
        // `DXGI_ERROR_DEVICE_REMOVED` several frames later — so the reader is
        // built beside the backend it reads from and dies with it.
        let mut reader = FrameReader::default();
        // Where this subject's timestamps start in the run's list, so that what
        // it covered can be added up separately from the gap before the next
        // one starts.
        let first_of_this_subject = video.timestamps.len();
        let lost = loop {
            if Instant::now() >= deadline {
                break false;
            }
            match backend.acquire(Duration::from_millis(100)) {
                Ok(Acquisition::Frame(frame)) => {
                    let timestamp = frame.timestamp();
                    video.timestamps.push(timestamp);
                    if let Some(missed) = frame.frames_missed() {
                        video.backend_missed += u64::from(missed);
                    }

                    if video.timestamps.len() as u64 % DECODE_EVERY != 0 {
                        continue;
                    }

                    let image = reader.read(frame.texture()).unwrap_or_else(|error| {
                        panic!("a captured frame could not be read: {error}")
                    });
                    let surface =
                        Surface::new(&image.pixels, image.stride, image.width, image.height)
                            .expect("a mapped texture describes itself");

                    let found = match region {
                        Some(found) => found,
                        None => match pattern::locate(&surface, width, height) {
                            Some(found) => {
                                region = Some(found);
                                found
                            }
                            // The first frames of a capture can be the window
                            // before it had drawn anything.
                            None => continue,
                        },
                    };

                    match pattern::decode(&surface, found) {
                        Ok(decoded) => {
                            video.decoded_total += 1;
                            decoded_here.push((decoded.index(), timestamp));
                        }
                        Err(_) => video.undecodable += 1,
                    }
                }
                Ok(Acquisition::Timeout) => video.timeouts += 1,
                Ok(Acquisition::SizeChanged(size)) => panic!(
                    "the captured window changed size to {size} during a run that never \
                     resized it"
                ),
                Err(CaptureError::TargetLost { .. }) => break true,
                Err(error) => panic!("the capture failed during the run: {error}"),
            }
        };

        if decoded_here.len() > video.decoded.len() {
            video.decoded = decoded_here;
        }

        let this_subject = &video.timestamps[first_of_this_subject..];
        if let (Some(first), Some(last)) = (this_subject.first(), this_subject.last()) {
            video.covered += last.duration_since(*first).unwrap_or_default();
            video.pieces += 1;
        }

        if !lost {
            // The run reached its deadline with the subject still up, which is
            // the ordinary case: stop it cleanly and record what it said.
            match app.stop(Duration::from_secs(10)) {
                Ok(summary) => {
                    video.stopped = Some(format!(
                        "{} frames presented, stopped by {}",
                        summary.frames, summary.reason
                    ));
                }
                Err(error) => panic!("the test application did not stop cleanly: {error}"),
            }
            break;
        }

        // The window went away under the capture. `TestApp::drop` closes
        // standard input and kills whatever is left of the process, so the
        // restart below starts from a clean slate.
        video.restarts += 1;
        note(&format!(
            "the subject's window went away {:.0} s into the run; starting it again \
             (restart {} of {MAX_SUBJECT_RESTARTS})",
            run.as_secs_f64()
                - deadline
                    .saturating_duration_since(Instant::now())
                    .as_secs_f64(),
            video.restarts,
        ));
        drop(app);

        if video.restarts >= MAX_SUBJECT_RESTARTS {
            note(
                "the subject would not stay up, so the video side of this run stops here; \
                 the audio side continues to the deadline",
            );
            break;
        }
    }

    // The audio side is still running, and it is where the offset is measured.
    // Leaving early because the video side gave up would shorten the very
    // measurement this run exists to take, so the deadline is waited out.
    let remaining = deadline.saturating_duration_since(Instant::now());
    if !remaining.is_zero() {
        note(&format!(
            "the video side finished early; waiting out the remaining {:.0} s so the \
             audio side still covers the whole run",
            remaining.as_secs_f64()
        ));
        std::thread::sleep(remaining);
    }

    video
}

/// Everything measured, in one place, so that the printing and the assertions
/// read the same numbers.
#[derive(Debug)]
struct Report {
    clock: CaptureClock,
    video_frames: usize,
    /// First frame to last, downtime between subjects included.
    video_span: Duration,
    /// The sum of each subject's own first-to-last span: how much of the run
    /// the capture had frames for. Equal to [`Self::video_span`] unless the
    /// subject had to be restarted.
    video_covered: Duration,
    /// How many pieces [`Self::video_covered`] is the sum of.
    video_pieces: u32,
    video_timeouts: u64,
    video_missed: u64,
    video_undecodable: u64,
    video_restarts: u32,
    video_monitor: String,
    video_stopped: Option<String>,
    decoded: u64,
    source_interval_nanos: Option<f64>,
    audio: AudioSummary,
    estimator: DriftEstimator,
    tolerance: SyncTolerance,
}

#[derive(Debug)]
struct AudioSummary {
    endpoints: Vec<String>,
    format_changes: u64,
    sample_rate: u32,
    frames: u64,
    synthesised_frames: u64,
    endpoint_buffers: usize,
    failed: Option<String>,
    first_media: Option<MediaTime>,
    last_media: Option<MediaTime>,
}

impl Report {
    fn build(video: &VideoRun, audio: &AudioRun) -> Self {
        let first = *video
            .timestamps
            .first()
            .expect("a run with no video frames at all has nothing to measure");
        let last = *video
            .timestamps
            .last()
            .expect("a run with at least one frame has a last one");

        // The epoch is the first frame the recording keeps, which is what a
        // session will use. Audio that arrived before it gets a negative media
        // time, which is representable and is exactly the point.
        let clock = CaptureClock::start_at(first);

        let mut estimator = DriftEstimator::new(DEFAULT_DISCONTINUITY_STEP);
        let mut first_media = None;
        let mut last_media = None;
        for (device, track) in &audio.observations {
            let reference = clock
                .media_time_on(SourceClock::PerformanceCounter, *device)
                .expect("the audio endpoint reports positions on the performance counter");
            let observed = clock
                .media_time_on(SourceClock::PerformanceCounter, *track)
                .expect("the audio track is timed on the performance counter");
            estimator.observe(reference, observed);
            first_media.get_or_insert(observed);
            last_media = Some(observed);
        }

        Self {
            clock,
            video_frames: video.timestamps.len(),
            video_span: last.duration_since(first).unwrap_or_default(),
            video_covered: video.covered,
            video_pieces: video.pieces,
            video_timeouts: video.timeouts,
            video_missed: video.backend_missed,
            video_undecodable: video.undecodable,
            video_restarts: video.restarts,
            video_monitor: video.monitor.clone(),
            video_stopped: video.stopped.clone(),
            decoded: video.decoded_total,
            source_interval_nanos: source_interval(&video.decoded),
            audio: AudioSummary {
                endpoints: audio.endpoints.clone(),
                format_changes: audio.format_changes,
                sample_rate: audio.sample_rate,
                frames: audio.frames,
                synthesised_frames: audio.synthesised_frames,
                endpoint_buffers: audio.observations.len(),
                failed: audio.failed.clone(),
                first_media,
                last_media,
            },
            estimator,
            tolerance: SyncTolerance::default(),
        }
    }

    fn print(&self) {
        let millis = |nanos: i64| nanos as f64 / 1e6;

        note(&format!("epoch {}", self.clock.epoch()));
        note(&format!(
            "subject: on {}, {} restart(s); {}",
            self.video_monitor,
            self.video_restarts,
            self.video_stopped
                .as_deref()
                .unwrap_or("it did not report a clean stop"),
        ));
        note(&format!(
            "video: {} frames covering {:.3} s of the run, {} acquisition timeouts, \
             {} frames missed, {} of {} sampled frames undecodable",
            self.video_frames,
            self.video_covered.as_secs_f64(),
            self.video_timeouts,
            self.video_missed,
            self.video_undecodable,
            self.decoded + self.video_undecodable,
        ));
        if self.video_restarts > 0 {
            note(&format!(
                "video: the first frame to the last spans {:.3} s, so {:.3} s of that was \
                 downtime between subjects and is not counted as covered",
                self.video_span.as_secs_f64(),
                self.video_span
                    .saturating_sub(self.video_covered)
                    .as_secs_f64(),
            ));
        }
        if let Some(interval) = self.source_interval_nanos {
            note(&format!(
                "video: the source presented a frame every {:.4} ms measured against the \
                 reference clock (nominal {:.4} ms at {PATTERN_FPS} fps)",
                interval / 1e6,
                1e3 / f64::from(PATTERN_FPS),
            ));
        }
        note(&format!(
            "audio: endpoint {} at {} Hz, {} frames ({:.3} s), {} synthesised, \
             {} endpoint buffers",
            if self.audio.endpoints.is_empty() {
                "<none>".to_owned()
            } else {
                self.audio
                    .endpoints
                    .iter()
                    .map(|name| format!("{name:?}"))
                    .collect::<Vec<_>>()
                    .join(", then ")
            },
            self.audio.sample_rate,
            self.audio.frames,
            self.audio.frames as f64 / f64::from(self.audio.sample_rate.max(1)),
            self.audio.synthesised_frames,
            self.audio.endpoint_buffers,
        ));
        if self.audio.format_changes > 0 {
            note(&format!(
                "audio: {} format change(s), after each of which the track is synthesised \
                 silence with no device position of its own and contributes no observations",
                self.audio.format_changes,
            ));
        }
        if let (Some(first), Some(last)) = (self.audio.first_media, self.audio.last_media) {
            note(&format!(
                "audio: track media time runs from {:.3} ms to {:.3} ms",
                millis(first.as_nanos()),
                millis(last.as_nanos()),
            ));
        }

        // Printed beside the numbers rather than left to the document, because
        // the numbers are what gets quoted.
        note(
            "A/V offset: the track's position minus the endpoint's, anchored on the first \
             buffer — so these are the change over the run, and any constant offset the \
             two already had is not in them",
        );
        match (self.estimator.first(), self.estimator.latest()) {
            (Some(first), Some(latest)) => note(&format!(
                "A/V offset: first {:+.3} ms, last {:+.3} ms, peak {:+.3} ms, {} \
                 ({} observations, {} discontinuities, tolerance {})",
                millis(first),
                millis(latest),
                millis(self.estimator.peak()),
                self.estimator
                    .state(&self.tolerance)
                    .map_or_else(|| "no observations".to_owned(), |state| state.to_string()),
                self.estimator.observations(),
                self.estimator.discontinuities(),
                self.tolerance,
            )),
            _ => note("A/V offset: the endpoint delivered nothing, so there is nothing to measure"),
        }

        match self.estimator.rate() {
            Some(rate) => {
                // The budget is the limit the offset is moving *towards*. A
                // negative rate is a track stamping events early, so it spends
                // the lead allowance; a positive one spends the lag allowance.
                // They differ by half, so reporting the wrong one overstates
                // the headroom by 50% (EBU R37: 40 ms of lead, 60 ms of lag).
                let (limit, direction) = if rate.as_ratio() < 0.0 {
                    (self.tolerance.ahead(), "lead")
                } else {
                    (self.tolerance.behind(), "lag")
                };
                note(&format!(
                    "drift: {rate}, standard error {} over {:.1} s of correction-free \
                     capture; at that rate the tolerance ({} ms of {direction}) is reached \
                     after {} of recording",
                    self.estimator.rate_standard_error().map_or_else(
                        || "unknown".to_owned(),
                        |error| format!("{:.4} ppm", error.parts_per_million())
                    ),
                    self.estimator.segment_span_nanos() as f64 / 1e9,
                    limit.as_millis(),
                    rate.time_to(limit).map_or_else(
                        || "never".to_owned(),
                        |budget| format!("{:.1} minutes", budget.as_secs_f64() / 60.0)
                    ),
                ));
            }
            None => note("drift: not enough of a correction-free segment to fit a rate to"),
        }
    }

    fn assert_healthy(&self, run: Duration) {
        if let Some(failure) = &self.audio.failed {
            panic!("the audio side of the run did not complete: {failure}");
        }

        assert!(
            self.video_frames > 0,
            "the capture delivered no frames at all"
        );
        assert!(
            self.video_undecodable == 0,
            "{} of the sampled frames were not the test pattern, so the frames being \
             timed may not be the subject's",
            self.video_undecodable
        );
        assert!(
            self.decoded > 0,
            "no sampled frame decoded as the test pattern, so nothing proves the capture \
             found its subject"
        );

        // The video capture has to have covered the run. Summed per subject,
        // not first frame to last: a subject that dies at 5 s and comes back at
        // 60 s of a 70 s run spans 65 s while covering 15, and this assertion
        // is the only thing tying the offset measurement to the claim that
        // video and audio were captured together.
        assert!(
            self.video_covered.as_secs_f64() > run.as_secs_f64() * MINIMUM_VIDEO_COVERAGE,
            "the video capture covered {:.1} s of a {:.1} s run, in {} piece(s) spanning \
             {:.1} s, which is too little of it to claim video and audio were captured \
             together",
            self.video_covered.as_secs_f64(),
            run.as_secs_f64(),
            self.video_pieces,
            self.video_span.as_secs_f64(),
        );

        let Some(state) = self.estimator.state(&self.tolerance) else {
            skipped(
                "the endpoint delivered no packets during the run, so no A/V offset could be \
                 measured; something has to be rendering for the device clock to run",
            );
            return;
        };

        assert_eq!(
            state,
            SyncState::InTolerance,
            "the audio track ended {:+.3} ms from the reference clock, outside {}",
            self.estimator.latest().unwrap_or_default() as f64 / 1e6,
            self.tolerance,
        );
        assert_eq!(
            self.tolerance.classify(self.estimator.peak()),
            SyncState::InTolerance,
            "the audio track reached {:+.3} ms from the reference clock during the run, \
             outside {}",
            self.estimator.peak() as f64 / 1e6,
            self.tolerance,
        );

        // The two positions being compared have to be genuinely independent
        // accounts of the same moment. If `device_timestamp` were quietly the
        // track's own timestamp, every assertion above would pass for ever and
        // drift would be permanently invisible — the worst possible outcome for
        // this test, because it would read as proof of perfect synchronisation.
        // Over thousands of buffers the sample count and the counter cannot
        // agree to the nanosecond every single time.
        assert_ne!(
            self.estimator.peak(),
            0,
            "the endpoint's positions and the track's never differed by a nanosecond \
             across {} buffers, which means they are the same number rather than in \
             agreement",
            self.estimator.observations(),
        );

        if let Some(rate) = self.estimator.rate() {
            assert!(
                rate.parts_per_million().abs() < IMPLAUSIBLE_PPM,
                "a drift of {rate} is not a crystal, it is a broken conversion"
            );
        }
    }
}

/// The mean interval between the source's own frames, in nanoseconds, measured
/// against the reference clock.
///
/// A least-squares slope of capture timestamp against the counter the source
/// drew into the frame, so that a dropped sample or a missed frame changes the
/// fit rather than invalidating it.
fn source_interval(decoded: &[(u32, CaptureTimestamp)]) -> Option<f64> {
    if decoded.len() < 2 {
        return None;
    }
    let (first_index, first_timestamp) = decoded[0];
    let points: Vec<(f64, f64)> = decoded
        .iter()
        .map(|(index, timestamp)| {
            (
                f64::from(index.wrapping_sub(first_index)),
                timestamp
                    .duration_since(first_timestamp)
                    .unwrap_or_default()
                    .as_nanos() as f64,
            )
        })
        .collect();

    let n = points.len() as f64;
    let sum_x: f64 = points.iter().map(|(x, _)| x).sum();
    let sum_y: f64 = points.iter().map(|(_, y)| y).sum();
    let sum_xy: f64 = points.iter().map(|(x, y)| x * y).sum();
    let sum_xx: f64 = points.iter().map(|(x, _)| x * x).sum();

    let denominator = n * sum_xx - sum_x * sum_x;
    (denominator != 0.0).then(|| (n * sum_xy - sum_x * sum_y) / denominator)
}

/// Holds a render stream open so that the endpoint's clock keeps running.
///
/// # What it plays
///
/// Nothing. Every buffer is released with `AUDCLNT_BUFFERFLAGS_SILENT`, which
/// tells the audio engine the frames are zero without their contents ever being
/// read. There is no tone, no attenuated signal and no chance of a noise
/// escaping onto somebody's speakers, which matters because this runs for half
/// an hour on a machine somebody is using.
///
/// # Ownership
///
/// The thread owns the `IAudioClient` and the `IAudioRenderClient` for its whole
/// life and releases both when it returns; [`Drop`] sets the flag and joins, so
/// a panicking test cannot leave a render stream open (AGENTS.md section 58).
#[derive(Debug)]
struct SilenceKeeper {
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl SilenceKeeper {
    fn start() -> Result<Self, String> {
        let running = Arc::new(AtomicBool::new(true));
        let (ready, started) = std::sync::mpsc::channel();
        let thread = std::thread::spawn({
            let running = Arc::clone(&running);
            move || keep_awake(&running, &ready)
        });

        match started.recv() {
            Ok(Ok(())) => Ok(Self {
                running,
                thread: Some(thread),
            }),
            Ok(Err(reason)) => {
                running.store(false, Ordering::Relaxed);
                let _ = thread.join();
                Err(reason)
            }
            Err(_) => Err("the render thread stopped before it reported anything".to_owned()),
        }
    }
}

impl Drop for SilenceKeeper {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The render thread's body: open the default endpoint and feed it silence.
fn keep_awake(running: &AtomicBool, ready: &std::sync::mpsc::Sender<Result<(), String>>) {
    // SAFETY: `CoIncrementMTAUsage` takes a process-wide reference to the
    // multi-threaded apartment. The reference is deliberately never given back,
    // which is what makes it safe to take from a thread that will exit — the
    // same reasoning as `crates/audio/src/windows/apartment.rs`.
    if let Err(error) = unsafe { CoIncrementMTAUsage() } {
        let _ = ready.send(Err(format!("COM is unavailable: {error}")));
        return;
    }

    let prepared = (|| -> windows::core::Result<(IAudioClient, IAudioRenderClient, u32)> {
        // SAFETY: `MMDeviceEnumerator` is the class identifier for
        // `IMMDeviceEnumerator`, which is the interface the return type asks
        // for.
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }?;
        // SAFETY: both arguments are values of the enumerations named, and the
        // enumerator is live.
        let device = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }?;
        // SAFETY: `device` is live; the interface is fixed by the return type.
        let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }?;
        // SAFETY: `client` is live and uninitialised, which is when
        // `GetMixFormat` is valid. The `WAVEFORMATEX` it returns is a
        // `CoTaskMemAlloc` allocation this code now owns, and it is given back
        // below (AGENTS.md section 58).
        let mix = unsafe { client.GetMixFormat() }?;
        // SAFETY: `mix` is the format Windows just reported for this endpoint,
        // so it is a format the endpoint accepts, and shared mode never takes
        // the device away from anything else. `Initialize` copies what it needs
        // out of the format, so the allocation is released straight afterwards
        // whether or not it succeeded.
        let initialised =
            unsafe { client.Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 2_000_000, 0, mix, None) };
        // SAFETY: `mix` came from `GetMixFormat`, has not been freed, and is
        // not used again after this point.
        unsafe { CoTaskMemFree(Some(mix.cast())) };
        initialised?;
        // SAFETY: `client` is initialised, which is when `GetService` is valid.
        let render: IAudioRenderClient = unsafe { client.GetService() }?;
        // SAFETY: `client` is initialised.
        let frames = unsafe { client.GetBufferSize() }?;
        Ok((client, render, frames))
    })();

    let (client, render, buffer_frames) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            let _ = ready.send(Err(format!(
                "the default output device would not accept a render stream: {error}"
            )));
            return;
        }
    };

    // SAFETY: `client` is initialised and has not been started.
    if let Err(error) = unsafe { client.Start() } {
        let _ = ready.send(Err(format!("the render stream would not start: {error}")));
        return;
    }
    let _ = ready.send(Ok(()));

    while running.load(Ordering::Relaxed) {
        // SAFETY: `client` is a started `IAudioClient`.
        let Ok(padding) = (unsafe { client.GetCurrentPadding() }) else {
            break;
        };
        let free = buffer_frames.saturating_sub(padding);
        if free == 0 {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }

        // SAFETY: `free` is at most the free space `GetCurrentPadding` just
        // reported, which is what `GetBuffer` requires. The returned pointer is
        // deliberately not written to: `ReleaseBuffer` below is given
        // `AUDCLNT_BUFFERFLAGS_SILENT`, which tells the engine to treat the
        // frames as zero and not to read the buffer at all.
        let Ok(_buffer) = (unsafe { render.GetBuffer(free) }) else {
            break;
        };
        // SAFETY: `free` is the frame count `GetBuffer` was asked for, and the
        // silent flag is what makes leaving the buffer unwritten correct.
        if unsafe { render.ReleaseBuffer(free, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) }.is_err() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    // SAFETY: `client` was started, and stopping it is what releases the
    // endpoint.
    let _ = unsafe { client.Stop() };
}
