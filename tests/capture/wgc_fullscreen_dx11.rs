//! End-to-end: cover a display with the test pattern — exclusively, where
//! Windows allows it — and capture that window.
//!
//! # What this exists to find out
//!
//! Exclusive fullscreen is the presentation
//! [issue #12](https://github.com/wildware-uk/clipped/issues/12) could not
//! verify, because there was no subject that took a display the way a game
//! does. `test-apps/fullscreen-dx11` is that subject, and this is the test that
//! points a capture at it.
//!
//! # What it can and cannot decide
//!
//! Windows decides whether the application gets the display exclusively:
//! `SetFullscreenState` needs the foreground, and Windows does not give the
//! foreground to a process the user has not interacted with. So this test reads
//! what the application was granted and asserts accordingly:
//!
//! - **Granted (`exclusive=yes`).** Whether Windows Graphics Capture keeps
//!   delivering frames for a window that owns its display is a fact about
//!   Windows, not a property Clipped can assert into being — so a run that
//!   delivers nothing is *reported*, loudly, rather than failed. What is
//!   asserted is that capture does not error, that every frame that does arrive
//!   is the pattern, and that the display is given back.
//! - **Refused (`exclusive=no`).** The application is then a borderless window
//!   covering the display, which is what a game in "fullscreen (windowed)" mode
//!   is, and frames must arrive and must decode. That case is asserted in full.
//!
//! Being explicit about that split is the point. A test that quietly passed
//! either way would be no evidence at all (AGENTS.md section 54).
//!
//! # Why it is `#[ignore]`d
//!
//! Beyond needing a GPU and a desktop, this one takes a display away from
//! whoever is using the machine for the length of the run, and can change its
//! mode. That is not something `cargo test` should do because somebody typed
//! `cargo test`.
//!
//! ```text
//! cargo test -p clipped-fullscreen-dx11 --test wgc_fullscreen_dx11 -- --ignored --nocapture
//! ```

mod readback;

use core::time::Duration;
use std::time::Instant;

use clipped_capture::{
    registered_backend, registered_declarations, select, Acquisition, CaptureConfig, CaptureError,
    CaptureMethodSetting, CaptureTarget, FrameSize, TargetHandle, TargetKind, TargetProperties,
};
use clipped_video_pattern::harness::TestApp;
use clipped_video_pattern::pattern::{self, Surface};

use readback::FrameReader;

/// The rate the test application presents at: a game-like 60.
const SOURCE_FPS: u32 = 60;

/// How long the test captures for.
///
/// Short on purpose. A display held exclusively is a display nobody else can
/// use, and five seconds at 60 fps is still three hundred frames of evidence.
const CAPTURE_FOR: Duration = Duration::from_secs(5);

/// How long the application is given to appear. Longer than the windowed
/// tests: a mode switch takes a moment, and the `ready` line comes after it.
const READY_TIMEOUT: Duration = Duration::from_secs(45);

/// How long the application is given to stop, and give the display back.
const STOP_TIMEOUT: Duration = Duration::from_secs(15);

#[test]
#[ignore = "takes over a display and needs a GPU; see the module docs"]
fn a_fullscreen_application_is_captured_and_gives_its_display_back() {
    // Before any size is read, and for the same reason the test application
    // does it (`test-apps/video-pattern/src/app.rs`): the subject is
    // per-monitor DPI aware, so it reports and draws physical pixels, and a
    // DPI-unaware test process is shown the smaller virtualised numbers on any
    // display scaled above 100%. Without this the size assertion below compares
    // 2560x1440 against 1707x960 and blames the application for the test's own
    // DPI mode (AGENTS.md section 25).
    let awareness = clipped_windows::enable_per_monitor_dpi_awareness()
        .expect("the sizes this test compares are physical pixels, which needs this mode");
    eprintln!("[info] per-monitor DPI awareness: {awareness:?}");

    let monitors = clipped_windows::enumerate_monitors().expect("this machine has displays");
    let expected = monitors
        .iter()
        .find(|monitor| !monitor.is_primary())
        .or_else(|| monitors.first())
        .expect("a machine running this test has at least one display");
    eprintln!(
        "[info] expecting the application to cover {} ({})",
        expected.device_name(),
        expected.bounds()
    );

    let app = TestApp::start(
        env!("CARGO_BIN_EXE_fullscreen-dx11"),
        [
            "--mode",
            "exclusive",
            "--fps",
            &SOURCE_FPS.to_string(),
            // A backstop far beyond the capture window, so a panicking test
            // cannot leave a display held.
            "--seconds",
            "60",
        ],
        READY_TIMEOUT,
    )
    .expect("the fullscreen test application should start and announce itself");

    assert_eq!(
        app.presentation(),
        "fullscreen-exclusive",
        "this test asked for an exclusive fullscreen run"
    );
    assert_eq!(
        app.monitor(),
        expected.device_name(),
        "the application should have chosen the same display this test did"
    );
    assert_eq!(
        app.client_size(),
        (
            expected.bounds().size().width(),
            expected.bounds().size().height()
        ),
        "a fullscreen application should cover the whole display"
    );

    let granted = app.is_exclusive();
    eprintln!(
        "[info] Windows {} the display exclusively",
        if granted { "granted" } else { "refused" }
    );

    let outcome = capture(&app);

    eprintln!(
        "\n=== wgc_fullscreen_dx11 ===\n\
         exclusive granted   : {}\n\
         client size         : {}x{}\n\
         frames delivered    : {}\n\
         frames decoded      : {}\n\
         acquisition timeouts: {}\n\
         counters            : {} to {}\n\
         undecodable frames  : {}{}\n",
        if granted { "yes" } else { "no" },
        app.client_size().0,
        app.client_size().1,
        outcome.delivered,
        outcome.decoded,
        outcome.timeouts,
        outcome.first.map_or(0, |first| first),
        outcome.last.map_or(0, |last| last),
        outcome.undecodable.len(),
        outcome
            .undecodable
            .first()
            .map_or_else(String::new, |first| format!("\n  first: {first}"))
    );

    assert!(
        outcome.undecodable.is_empty(),
        "{} frames arrived that were not the pattern the application was drawing. \
         First: {}",
        outcome.undecodable.len(),
        outcome.undecodable.first().map_or("", String::as_str)
    );

    if granted && outcome.decoded == 0 {
        // A finding, not a pass and not a failure: Windows Graphics Capture is
        // asked for a window's composed content, and a window that owns its
        // display through DXGI may not be composed at all. Whoever runs this
        // needs to see it said plainly.
        eprintln!(
            "\n*** FINDING: the display was held exclusively, and in {:.0}s Windows Graphics \
             Capture delivered {} frames, {} acquisition timeouts, and not one frame that \
             held the test pattern. A recorder relying on this backend alone would record \
             nothing while a game is in exclusive fullscreen. Record this on issue #23 and \
             raise it against the capture backend. ***\n",
            CAPTURE_FOR.as_secs_f64(),
            outcome.delivered,
            outcome.timeouts
        );
    } else {
        assert!(
            outcome.decoded >= u64::from(SOURCE_FPS) * CAPTURE_FOR.as_secs() / 4,
            "only {} frames decoded in {:.0}s of capturing a {SOURCE_FPS} fps fullscreen \
             application, which is too few to conclude anything from",
            outcome.decoded,
            CAPTURE_FOR.as_secs_f64()
        );
    }

    let stopped = app
        .stop(STOP_TIMEOUT)
        .expect("the application should stop cleanly, which is what gives the display back");
    eprintln!(
        "[info] the application presented {} frames and stopped because of the {}",
        stopped.frames, stopped.reason
    );
    assert_eq!(
        stopped.reason, "stop-requested",
        "the application should have stopped because the test asked it to"
    );

    if let Some(last) = outcome.last {
        // The two accounts of the same run, cross-checked: the counters that
        // came out of the compositor against the count the application kept.
        // This mode is the one where they could drift — the warm-up frames DXGI
        // needs before a fullscreen transition are presented before the run
        // loop, and if they were not counted the last counter captured could
        // exceed the number of frames the application says it presented.
        assert!(
            last < stopped.frames,
            "the last counter captured was {last} and the application says it presented \
             only {} frames, so the counter it draws is not the count of frames it has \
             presented",
            stopped.frames
        );
    }

    // The display is back: the same enumeration that chose it reports the same
    // size it had before the run. A mode switch that was not undone would show
    // up here rather than as a puzzled maintainer.
    let after = clipped_windows::enumerate_monitors().expect("this machine still has displays");
    let after = after
        .iter()
        .find(|monitor| monitor.device_name() == expected.device_name())
        .expect("the display the application covered is still attached");
    assert_eq!(
        after.bounds(),
        expected.bounds(),
        "the display is not the shape it was before the run: the application changed its \
         mode and did not put it back"
    );
}

/// What the capture saw.
#[derive(Debug, Default)]
struct Outcome {
    delivered: u64,
    decoded: u64,
    timeouts: u64,
    first: Option<u32>,
    last: Option<u32>,
    undecodable: Vec<String>,
}

/// Captures the application's window for [`CAPTURE_FOR`].
fn capture(app: &TestApp) -> Outcome {
    let (width, height) = app.client_size();
    let size = FrameSize::new(width, height).expect("the application announced a real size");
    let properties = TargetProperties::new(TargetKind::Window, size);

    let selection = select(
        &registered_declarations(),
        &properties,
        CaptureMethodSetting::Automatic,
    )
    .expect("this machine should have a capture backend for a window");
    let mut backend = registered_backend(selection.method())
        .expect("selection only ever chooses a registered backend")
        .create()
        .expect("the backend should be creatable");

    let target = CaptureTarget::new(TargetHandle::from_raw(app.window() as u64), properties);
    let format = backend
        .initialise(
            &target,
            &CaptureConfig::default().with_capture_cursor(false),
        )
        .expect("capturing a fullscreen application's window should start");
    eprintln!(
        "[info] capturing through {} at {format}",
        selection.method()
    );

    let mut outcome = Outcome::default();
    let mut reader = FrameReader::default();
    let mut region = None;
    let deadline = Instant::now() + CAPTURE_FOR;

    while Instant::now() < deadline {
        match backend.acquire(Duration::from_millis(100)) {
            Ok(Acquisition::Frame(frame)) => {
                outcome.delivered += 1;
                // Copied while the frame is held: the texture belongs to the
                // backend, and the borrow ends here rather than being carried
                // into the decoding below (`docs/capture-pipeline.md`).
                let image = reader
                    .read(frame.texture())
                    .unwrap_or_else(|error| panic!("a captured frame could not be read: {error}"));

                let surface = Surface::new(&image.pixels, image.stride, image.width, image.height)
                    .expect("a mapped texture describes itself");

                let found = match region {
                    Some(found) => found,
                    None => match pattern::locate(&surface, width, height) {
                        Some(found) => {
                            region = Some(found);
                            found
                        }
                        None => continue,
                    },
                };

                match pattern::decode(&surface, found) {
                    Ok(decoded) => {
                        outcome.first.get_or_insert(decoded.index());
                        outcome.last = Some(decoded.index());
                        outcome.decoded += 1;
                    }
                    Err(error) => {
                        if outcome.undecodable.len() < 8 {
                            outcome.undecodable.push(error.to_string());
                        }
                    }
                }
            }
            Ok(Acquisition::Timeout) => outcome.timeouts += 1,
            Ok(Acquisition::SizeChanged(size)) => {
                // A fullscreen transition can legitimately change the shape of
                // what is being captured, so this is followed rather than
                // treated as a fault — and the pattern is looked for again.
                eprintln!("[info] the captured window changed size to {size}");
                backend.resize(size).expect("the pool can be recreated");
                region = None;
            }
            Err(CaptureError::TargetLost { .. }) => {
                panic!("the fullscreen application's window went away mid-capture");
            }
            Err(error) => panic!("capture failed after {} frames: {error}", outcome.delivered),
        }
    }

    backend.shut_down();
    outcome
}
