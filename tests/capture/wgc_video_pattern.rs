//! End-to-end: start the video pattern application, capture its window through
//! the real Windows Graphics Capture backend, and check the frames that arrive
//! against the frames it says it presented.
//!
//! This is the test AGENTS.md sections 25 and 26 ask for. Nothing here is
//! mocked and nothing is installed: the subject is
//! `test-apps/video-pattern`, the capture is the backend the recorder will use,
//! and the assertions are about pixels that came out of the compositor.
//!
//! # What it can decide
//!
//! - **That capture delivers the subject's content at all.** Every frame is
//!   decoded, and a frame that is not the pattern fails the test rather than
//!   being skipped.
//! - **Dropped frames.** The counter drawn into each frame is the source's own
//!   frame number, so a gap in the counters is exactly the frames the source
//!   presented that never arrived — measured, not inferred from a clock.
//! - **Duplicated frames.** The same counter twice is the same source frame
//!   delivered twice, and `Accounting::assert_healthy` fails a run that saw one.
//!   The checker itself is proved by the tests at the bottom of this file, which
//!   need no display and run everywhere: a counter that is asserted on only when
//!   somebody has a GPU to hand is a counter nobody has watched fail.
//! - **Torn frames.** The pattern's background and marker have to agree with
//!   its counter, so a frame assembled from two source frames fails to decode
//!   (`clipped_video_pattern::pattern`).
//! - **That a window capture finds its subject inside the window.** The second
//!   test runs the application with a title bar and a border, which is the case
//!   where the client area does not start at the frame's top-left corner.
//!
//! # Why it is `#[ignore]`d
//!
//! It needs a GPU, a desktop session, a compositor and about fifteen seconds of
//! wall-clock time, and it puts a window on a display. `tests/capture/README.md`
//! says these tests are not part of the pull-request CI job for exactly that
//! reason: a hosted runner has no GPU, so what it would test is the runner.
//! Being `#[ignore]`d rather than silently skipped is deliberate — a test that
//! decides for itself that it could not run reads as a pass, and the point of
//! this file is that somebody can tell the difference.
//!
//! Run it, on a machine with a display:
//!
//! ```text
//! cargo test -p clipped-video-pattern --test wgc_video_pattern -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `--nocapture` is worth typing: each test prints the frame accounting it
//! measured, which is the evidence AGENTS.md section 53 asks to be recorded on
//! the issue.

mod readback;

use core::time::Duration;
use std::time::Instant;

use clipped_capture::{
    registered_backend, registered_declarations, select, Acquisition, CaptureConfig, CaptureError,
    CaptureMethodSetting, CaptureTarget, CaptureTimestamp, FrameSize, TargetHandle, TargetKind,
    TargetProperties,
};
use clipped_video_pattern::harness::TestApp;
use clipped_video_pattern::pattern::{self, Region, Surface};

use readback::FrameReader;

/// The rate the test application presents at.
///
/// Half of a common refresh rate, deliberately. The compositor produces a
/// capture frame when the window's content changes, so a source slower than the
/// display is the case where every presented frame should arrive, which makes a
/// missing counter a real dropped frame rather than a monitor that could not
/// keep up.
const SOURCE_FPS: u32 = 30;

/// How long each test captures for.
const CAPTURE_FOR: Duration = Duration::from_secs(6);

/// How long the application is given to appear before the test gives up.
const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the application is given to stop after it is asked to.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// The most frames a run may lose, as a fraction of the frames the source
/// presented while capture was running.
///
/// Not zero, and not a guess: the source is at half the display's rate on any
/// modern monitor, so a healthy machine loses nothing at all, and every run
/// recorded on issue #23 lost nothing. The margin is for a machine that is
/// genuinely busy — a build running beside the test — where the interesting
/// failure is "capture stopped delivering", not "capture missed one frame in
/// four hundred".
const ACCEPTABLE_DROP_FRACTION: f64 = 0.05;

#[test]
#[ignore = "needs a GPU, a desktop session and about fifteen seconds; see the module docs"]
fn a_borderless_window_is_captured_frame_for_frame() {
    let app = TestApp::start(
        env!("CARGO_BIN_EXE_video-pattern"),
        [
            "--mode",
            "borderless",
            "--fps",
            &SOURCE_FPS.to_string(),
            // A hard backstop well beyond the capture window: if this test
            // panics between starting the application and dropping it, the
            // application still goes away on its own.
            "--seconds",
            "120",
        ],
        READY_TIMEOUT,
    )
    .expect("the video pattern application should start and announce itself");

    assert_eq!(
        app.presentation(),
        "borderless",
        "this test asked for a borderless window"
    );

    let accounting = capture_and_decode(&app);
    accounting.report("borderless");

    assert_eq!(
        accounting.region.map(|region| (region.x, region.y)),
        Some((0, 0)),
        "a borderless window has no chrome, so its pattern starts at the frame's \
         top-left corner"
    );
    accounting.assert_healthy();

    let stopped = app
        .stop(STOP_TIMEOUT)
        .expect("the application should stop when its standard input closes");
    accounting.assert_agrees_with(&stopped);
}

#[test]
#[ignore = "needs a GPU, a desktop session and about fifteen seconds; see the module docs"]
fn a_window_with_a_border_is_captured_with_its_chrome_around_the_pattern() {
    // The case a borderless capture cannot exercise: Windows Graphics Capture
    // captures a *window*, so the frame includes the title bar and the border
    // and the client area is offset inside it. Anything downstream that assumed
    // the content started at (0, 0) would be cropping every recording of an
    // ordinary window, and this is what would notice.
    let app = TestApp::start(
        env!("CARGO_BIN_EXE_video-pattern"),
        [
            "--mode",
            "windowed",
            "--fps",
            &SOURCE_FPS.to_string(),
            "--seconds",
            "120",
        ],
        READY_TIMEOUT,
    )
    .expect("the video pattern application should start and announce itself");

    let accounting = capture_and_decode(&app);
    accounting.report("windowed");

    let region = accounting
        .region
        .expect("the pattern should have been found inside the captured window");
    assert!(
        region.x > 0 || region.y > 0,
        "an ordinary window has chrome around its client area, so the pattern should \
         not have been at the frame's top-left corner: found {region}"
    );
    accounting.assert_healthy();

    let stopped = app
        .stop(STOP_TIMEOUT)
        .expect("the application should stop when its standard input closes");
    accounting.assert_agrees_with(&stopped);
}

/// What a capture run saw.
#[derive(Debug, Default)]
struct Accounting {
    /// Where the pattern was found in the frame, once it had been.
    region: Option<Region>,
    /// Frames the backend handed over.
    delivered: u64,
    /// Frames that decoded as a pattern.
    decoded: u64,
    /// Frames that did not decode after the pattern had been found once, with
    /// the first few reasons.
    undecodable: Vec<String>,
    /// Frames the backend had nothing to give.
    timeouts: u64,
    /// The first and last counters decoded.
    first: Option<u32>,
    last: Option<u32>,
    /// Counters the source presented in that span that never arrived.
    dropped: u64,
    /// Counters that arrived more than once.
    duplicated: u64,
    /// Counters that went backwards, which would mean frames delivered out of
    /// order.
    out_of_order: u64,
    /// What the backend itself reported as missed, for comparison.
    backend_missed: u64,
    /// Whether the backend reported a missed count at all.
    backend_reports_missed: bool,
    /// The span the capture covered, on the source's clock.
    first_timestamp: Option<CaptureTimestamp>,
    last_timestamp: Option<CaptureTimestamp>,
}

impl Accounting {
    /// Records one decoded frame against the one before it.
    fn record(&mut self, index: u32) {
        match self.last {
            None => self.first = Some(index),
            Some(previous) if index == previous => self.duplicated += 1,
            Some(previous) if index < previous => self.out_of_order += 1,
            Some(previous) => self.dropped += u64::from(index - previous) - 1,
        }
        self.last = Some(index);
        self.decoded += 1;
    }

    /// The frames the source presented between the first and last one seen.
    fn presented(&self) -> u64 {
        match (self.first, self.last) {
            (Some(first), Some(last)) if last >= first => u64::from(last - first) + 1,
            _ => 0,
        }
    }

    /// Prints the accounting, which is the evidence a run produces.
    fn report(&self, what: &str) {
        let presented = self.presented();
        let seconds = self
            .first_timestamp
            .zip(self.last_timestamp)
            .and_then(|(first, last)| last.duration_since(first))
            .map_or(f64::NAN, |span| span.as_secs_f64());

        eprintln!(
            "\n=== wgc_video_pattern ({what}) ===\n\
             pattern found at    : {}\n\
             frames delivered    : {}\n\
             frames decoded      : {}\n\
             acquisition timeouts: {}\n\
             source frames in run: {presented} (counters {} to {})\n\
             dropped             : {} ({:.2}%)\n\
             duplicated          : {}\n\
             out of order        : {}\n\
             backend said missed : {}\n\
             span on source clock: {seconds:.2}s\n\
             undecodable frames  : {}{}\n",
            self.region
                .map_or_else(|| "not found".to_owned(), |region| region.to_string()),
            self.delivered,
            self.decoded,
            self.timeouts,
            self.first.map_or(0, |first| first),
            self.last.map_or(0, |last| last),
            self.dropped,
            if presented == 0 {
                0.0
            } else {
                self.dropped as f64 * 100.0 / presented as f64
            },
            self.duplicated,
            self.out_of_order,
            if self.backend_reports_missed {
                self.backend_missed.to_string()
            } else {
                "not reported".to_owned()
            },
            self.undecodable.len(),
            if self.undecodable.is_empty() {
                String::new()
            } else {
                format!("\n  first: {}", self.undecodable[0])
            }
        );
    }

    /// Asserts everything that has to be true of any healthy run.
    fn assert_healthy(&self) {
        assert!(
            self.region.is_some(),
            "the test pattern was never found in a captured frame: {} frames were \
             delivered and none of them held it, so either the wrong window was captured \
             or capture is not delivering the subject's content",
            self.delivered
        );

        // Enough frames to be measuring capture rather than luck: six seconds
        // of a 30 fps source is around 180, and a run that produced a tenth of
        // that is a run whose numbers mean nothing.
        let expected = u64::from(SOURCE_FPS) * CAPTURE_FOR.as_secs();
        assert!(
            self.decoded >= expected / 3,
            "only {} frames decoded in {:.0}s of capturing a {SOURCE_FPS} fps source, \
             which is too few to conclude anything from; expected around {expected}",
            self.decoded,
            CAPTURE_FOR.as_secs_f64()
        );

        assert!(
            self.undecodable.is_empty(),
            "{} captured frames did not hold a readable pattern once the pattern had \
             been found. A frame that decodes as neither one source frame nor another is \
             a frame the compositor or the backend assembled from two, and an encoder \
             would have written it into the recording. First: {}",
            self.undecodable.len(),
            self.undecodable[0]
        );

        assert_eq!(
            self.out_of_order, 0,
            "frames arrived in a different order from the one they were presented in, \
             which would put the recording's timeline out of step with the game's"
        );

        // Zero, with no tolerance, unlike the dropped-frame check below. The
        // margin there is for a machine too busy to hand every frame over in
        // time; no amount of load makes a backend hand the *same* source frame
        // over twice, so a duplicate is a defect in the pipeline rather than a
        // symptom of the machine. Every measured run on issue #23 saw none.
        assert_eq!(
            self.duplicated, 0,
            "{} captured frames carried a counter that had already arrived, so the same \
             source frame was delivered more than once. An encoder would have written \
             each of them into the recording, which is a stutter at playback and a \
             timeline longer than the run",
            self.duplicated
        );

        let presented = self.presented();
        let dropped = self.dropped as f64 / presented.max(1) as f64;
        assert!(
            dropped <= ACCEPTABLE_DROP_FRACTION,
            "{} of the {presented} frames the source presented never arrived ({:.1}%), \
             which is more than the {:.1}% a busy machine explains",
            self.dropped,
            dropped * 100.0,
            ACCEPTABLE_DROP_FRACTION * 100.0
        );
    }

    /// Checks what was captured against what the application says it drew.
    ///
    /// The two are independent accounts of the same run — one from the pixels
    /// that came out of the compositor, one from the source's own counter — and
    /// a decoder that read the wrong bits would have to be wrong in a way that
    /// happened to stay inside the source's range to get past this.
    fn assert_agrees_with(&self, stopped: &clipped_video_pattern::harness::Stopped) {
        eprintln!(
            "[info] the application presented {} frames and stopped because of the {}",
            stopped.frames, stopped.reason
        );
        assert_eq!(
            stopped.reason, "stop-requested",
            "the application should have stopped because the test asked it to, not \
             because of anything else"
        );

        let last = self.last.expect("a healthy run decoded at least one frame");
        assert!(
            last < stopped.frames,
            "the last counter captured was {last} and the application says it only ever \
             presented {} frames, so the decoded counters cannot all have come from it",
            stopped.frames
        );
    }
}

/// Captures the application's window for [`CAPTURE_FOR`] and decodes every
/// frame.
fn capture_and_decode(app: &TestApp) -> Accounting {
    let (width, height) = app.client_size();
    let size = FrameSize::new(width, height).expect("the application announced a real size");
    let properties = TargetProperties::new(TargetKind::Window, size);

    let selection = select(
        &registered_declarations(),
        &properties,
        CaptureMethodSetting::Automatic,
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
    eprintln!(
        "[info] capturing {} through {} at {format}",
        app.monitor(),
        selection.method()
    );

    let mut accounting = Accounting::default();
    let mut reader = FrameReader::default();
    let deadline = Instant::now() + CAPTURE_FOR;

    while Instant::now() < deadline {
        match backend.acquire(Duration::from_millis(100)) {
            Ok(Acquisition::Frame(frame)) => {
                accounting.delivered += 1;
                if let Some(missed) = frame.frames_missed() {
                    accounting.backend_reports_missed = true;
                    accounting.backend_missed += u64::from(missed);
                }
                accounting.first_timestamp.get_or_insert(frame.timestamp());
                accounting.last_timestamp = Some(frame.timestamp());

                // The copy happens while the frame is held, which is the whole
                // of the ownership contract: the texture belongs to the
                // backend, and the borrow ends here rather than being carried
                // into the decoding below (`docs/capture-pipeline.md`).
                let image = reader
                    .read(frame.texture())
                    .unwrap_or_else(|error| panic!("a captured frame could not be read: {error}"));

                let surface = Surface::new(&image.pixels, image.stride, image.width, image.height)
                    .expect("a mapped texture describes itself");

                let region = match accounting.region {
                    Some(region) => region,
                    None => {
                        // The first frames of a capture can legitimately be the
                        // window before it had drawn anything, so failing to
                        // find the pattern is only a failure at the end of the
                        // run — `assert_healthy` decides that.
                        match pattern::locate(&surface, width, height) {
                            Some(region) => {
                                accounting.region = Some(region);
                                region
                            }
                            None => continue,
                        }
                    }
                };

                match pattern::decode(&surface, region) {
                    Ok(decoded) => accounting.record(decoded.index()),
                    Err(error) => {
                        if accounting.undecodable.len() < 8 {
                            accounting.undecodable.push(error.to_string());
                        }
                    }
                }
            }
            Ok(Acquisition::Timeout) => accounting.timeouts += 1,
            // The pattern window stays on screen for the whole run. One that
            // did not would make every frame count below a count of Alt-Tab.
            Ok(Acquisition::TargetMinimised) => panic!(
                "the pattern window was minimised during the run, so the accounting below \
                 describes a minimised window rather than capture; run it again \
                 with the desktop left alone"
            ),
            Ok(Acquisition::SizeChanged(size)) => {
                // Nothing in this test resizes the window, so a size change is
                // worth failing on rather than absorbing: it would mean the
                // pattern is no longer the size the application announced.
                panic!(
                    "the captured window changed size to {size} during a run that never \
                     resized it"
                );
            }
            Err(CaptureError::TargetLost { .. }) => {
                panic!(
                    "the test application's window went away while it was being captured; \
                     it was asked to run for two minutes and this run is {:.0} seconds long",
                    CAPTURE_FOR.as_secs_f64()
                );
            }
            Err(error) => panic!(
                "capture failed after {} frames: {error}",
                accounting.delivered
            ),
        }
    }

    backend.shut_down();
    accounting
}

/// Tests of the checker itself, which need neither a GPU nor a display and are
/// therefore not `#[ignore]`d.
///
/// The capture tests above can only be run by somebody with a desktop session,
/// so everything they assert rests on an [`Accounting`] nobody watches fail. A
/// counter that is printed but never asserted on, or asserted on in a way that
/// cannot fail, would let a backend that delivered every source frame twice pass
/// both of them silently — which is exactly the hollow test AGENTS.md section 54
/// forbids. These feed the checker the runs it exists to reject and require it
/// to reject them.
mod accounting {
    use std::panic::{self, AssertUnwindSafe};

    use super::{Accounting, Region, SOURCE_FPS};

    /// The counters a healthy six-second run at [`SOURCE_FPS`] decodes.
    fn healthy_run() -> Vec<u32> {
        (0..SOURCE_FPS * 6).collect()
    }

    /// An accounting of a run that decoded `counters`, in the order they
    /// arrived, with the pattern found where a borderless window puts it.
    fn accounting_of(counters: impl IntoIterator<Item = u32>) -> Accounting {
        let mut accounting = Accounting {
            region: Some(Region::origin(1280, 720)),
            ..Accounting::default()
        };
        for index in counters {
            accounting.delivered += 1;
            accounting.record(index);
        }
        accounting
    }

    /// Runs `assert_healthy` and returns the message it failed with, or [`None`]
    /// if it passed.
    fn rejection(accounting: &Accounting) -> Option<String> {
        let previous = panic::take_hook();
        // The panics below are expected, and the default hook would print a
        // backtrace note for each of them into the output of a passing test.
        panic::set_hook(Box::new(|_| {}));
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| accounting.assert_healthy()));
        panic::set_hook(previous);

        outcome.err().map(|payload| {
            payload.downcast_ref::<String>().map_or_else(
                || "a panic that was not an assertion message".to_owned(),
                Clone::clone,
            )
        })
    }

    #[test]
    fn the_same_counter_twice_is_counted_as_one_duplicate() {
        let accounting = accounting_of([5, 6, 6, 7]);
        assert_eq!(accounting.duplicated, 1);
        assert_eq!(accounting.dropped, 0);
        assert_eq!(accounting.out_of_order, 0);
        assert_eq!(accounting.decoded, 4);
    }

    #[test]
    fn a_healthy_run_is_accepted() {
        // The control. Without it the tests below would pass just as well
        // against a checker that rejected everything.
        let accounting = accounting_of(healthy_run());
        assert_eq!(
            rejection(&accounting),
            None,
            "a run that lost nothing and saw nothing twice has to pass, or the tests \
             below prove nothing"
        );
    }

    #[test]
    fn a_run_in_which_every_source_frame_arrived_twice_is_rejected() {
        // What a backend that handed each captured frame over twice would
        // produce: every counter the source drew, delivered in pairs. Before
        // `assert_healthy` checked for duplicates this run passed, printing
        // `duplicated: 180` on its way through.
        let doubled = healthy_run().into_iter().flat_map(|index| [index, index]);
        let accounting = accounting_of(doubled);
        assert_eq!(accounting.duplicated, u64::from(SOURCE_FPS) * 6);

        let message = rejection(&accounting).expect(
            "a run in which every frame arrived twice must fail, or a duplicate-delivery \
             regression in the capture backend ships green",
        );
        assert!(
            message.contains("delivered more than once"),
            "the failure should name what happened; it said: {message}"
        );
    }

    #[test]
    fn a_single_duplicated_frame_is_rejected() {
        // One duplicate in six seconds, which is the regression a tolerance
        // would hide. Nothing about a busy machine produces one.
        let mut counters = healthy_run();
        counters.insert(100, 99);
        let accounting = accounting_of(counters);
        assert_eq!(accounting.duplicated, 1);
        assert!(rejection(&accounting).is_some());
    }

    #[test]
    fn a_run_that_lost_half_its_frames_is_rejected() {
        // The other half of the acceptance criterion, and the run the
        // deliberate break on issue #23 produced: the source counted in twos,
        // so every other counter never arrived.
        let accounting = accounting_of(healthy_run().into_iter().filter(|index| index % 2 == 0));
        let message =
            rejection(&accounting).expect("a run that lost half the source's frames must fail");
        assert!(
            message.contains("never arrived"),
            "the failure should name what happened; it said: {message}"
        );
    }

    #[test]
    fn a_run_whose_frames_arrived_backwards_is_rejected() {
        let mut counters = healthy_run();
        counters.swap(100, 101);
        let accounting = accounting_of(counters);
        assert_eq!(accounting.out_of_order, 1);
        assert!(rejection(&accounting).is_some());
    }

    #[test]
    fn a_run_that_never_found_the_pattern_is_rejected() {
        let mut accounting = accounting_of(healthy_run());
        accounting.region = None;
        assert!(rejection(&accounting).is_some());
    }

    #[test]
    fn a_run_with_too_few_frames_to_mean_anything_is_rejected() {
        // Ten frames in six seconds is not a 30 fps source being captured; it
        // is a run whose percentages would be arithmetic on noise.
        let accounting = accounting_of(0..10);
        assert!(rejection(&accounting).is_some());
    }
}
