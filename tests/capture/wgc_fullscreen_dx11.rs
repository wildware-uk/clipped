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
//! # The answer, and the thing that nearly hid it
//!
//! Windows Graphics Capture does capture a window holding a display
//! exclusively. On Windows 11 build 26200 with an RTX 4090, one run of this
//! test decoded **274 of a possible 300 frames in five seconds, with zero
//! acquisition timeouts and zero undecodable frames**, from a subject that
//! reported `exclusive=yes` and that said afterwards it had presented all 318
//! of its frames with the display held exclusively.
//!
//! What made that hard to find is worth writing down, because it will waste
//! somebody's afternoon otherwise. **A display that Windows has powered off
//! makes every capture measurement meaningless.** With both displays asleep on
//! the idle timeout, the compositor runs at about 4 Hz — measured, repeatedly,
//! at 3.97 fps with a median interval of 251.6 ms — so Windows Graphics Capture
//! delivers about one frame in fifteen for *any* target, and
//! `SetFullscreenState` is refused with `DXGI_ERROR_NOT_CURRENTLY_AVAILABLE
//! (0x887A0022)`. Nothing about that is a capture defect and none of it
//! reproduces once the display is awake. `tests/capture/README.md` says how to
//! tell, and `docs/capture-pipeline.md` has the numbers.
//!
//! # What it decides, and what it only reports
//!
//! The subject asks DXGI for the display and prints whether it was given it, so
//! this test reads the `exclusive` field rather than assuming (AGENTS.md
//! section 16):
//!
//! - **Granted (`exclusive=yes`).** This is the case issue #12 exists to check,
//!   and it is asserted: the subject has to survive the run, the frames have to
//!   arrive, and every one of them has to decode as the pattern. A future
//!   Windows that stops composing a window which owns its display would fail
//!   here, which is the point.
//! - **Refused (`exclusive=no`).** The subject is then a borderless window
//!   covering the display, which is what a game in "fullscreen (windowed)" mode
//!   is, and the same assertions apply — but the run is *not* evidence about
//!   exclusive fullscreen and says so in as many words before it ends. A test
//!   that quietly passed either way would be no evidence at all (AGENTS.md
//!   section 54).
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
use std::io::Write as _;
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

/// The environment variable that turns "this machine could not run the test"
/// from a pass into a failure, as everywhere else in `clipped-capture`.
const REQUIRE_CAPTURE: &str = "CLIPPED_REQUIRE_CAPTURE";

/// Reports that the test could not run here.
///
/// It panics rather than skipping when [`REQUIRE_CAPTURE`] is set, so a machine
/// that is supposed to capture cannot quietly stop testing capture, and it
/// writes through `std::io::stderr()` rather than `eprintln!` because libtest
/// captures the macro — a skip nobody can see is the failure mode this exists to
/// prevent.
fn skipped(reason: &str) {
    assert!(
        std::env::var_os(REQUIRE_CAPTURE).is_none_or(|value| value.is_empty()),
        "{REQUIRE_CAPTURE} is set, so this must not be skipped: {reason}"
    );
    let _ = writeln!(std::io::stderr(), "SKIPPED (capture): {reason}");
}

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
    let Some(expected) = monitors
        .iter()
        .find(|monitor| !monitor.is_primary())
        .or_else(|| monitors.first())
    else {
        skipped("this machine reports no displays, so there is nothing to cover");
        return;
    };
    eprintln!(
        "[info] expecting the application to cover {} ({})",
        expected.device_name(),
        expected.bounds()
    );

    // Asked before anything is put on screen: a machine with no capture backend
    // should say so rather than take a display away and then find out.
    let size = FrameSize::new(
        expected.bounds().size().width(),
        expected.bounds().size().height(),
    )
    .expect("a display has a real size");
    let properties = TargetProperties::new(TargetKind::Window, size);
    let Ok(selection) = select(
        &registered_declarations(),
        &properties,
        CaptureMethodSetting::Automatic,
    ) else {
        skipped("this machine has no capture backend for a window");
        return;
    };

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

    let outcome = capture(&app, selection.method());

    eprintln!(
        "\n=== wgc_fullscreen_dx11 ===\n\
         exclusive granted   : {}\n\
         client size         : {}x{}\n\
         frames delivered    : {}\n\
         frames decoded      : {}\n\
         acquisition timeouts: {}\n\
         counters            : {} to {}\n\
         subject survived    : {}\n\
         undecodable frames  : {}{}\n",
        if granted { "yes" } else { "no" },
        app.client_size().0,
        app.client_size().1,
        outcome.delivered,
        outcome.decoded,
        outcome.timeouts,
        outcome.first.unwrap_or(0),
        outcome.last.unwrap_or(0),
        if outcome.target_lost { "no" } else { "yes" },
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

    assert!(
        !outcome.target_lost,
        "the subject's window went away {:.0}s into a run it was asked to keep going for 60s, \
         so there was nothing left to capture. The known way to reach this is a display that \
         has been powered off — Windows then revokes the exclusive mode a frame after granting \
         it and the subject does not survive that, which is \
         https://github.com/wildware-uk/clipped/issues/178. The subject's own message is on \
         standard error above.",
        CAPTURE_FOR.as_secs_f64()
    );

    // Half the source's frames, not all of them: this test reads every frame
    // back into system memory and decodes it, which a recorder does not, and
    // 2560x1440 of that at 60 fps is enough work to lose some. Half is far more
    // than is needed to tell "captured" from "not captured" — the run this was
    // written against decoded 274 of a possible 300 — and it is the number that
    // moves if the backend stops delivering, which is the regression worth
    // catching.
    let floor = u64::from(SOURCE_FPS) * CAPTURE_FOR.as_secs() / 2;
    assert!(
        outcome.decoded >= floor,
        "only {} of an expected {} frames decoded in {:.0}s of capturing a {SOURCE_FPS} fps \
         application covering a display{}. Windows Graphics Capture composed {} frames and \
         timed out {} times.",
        outcome.decoded,
        floor,
        CAPTURE_FOR.as_secs_f64(),
        if granted {
            " that was holding it exclusively, which is the case issue #12 exists to check"
        } else {
            ""
        },
        outcome.delivered,
        outcome.timeouts
    );

    if !granted {
        // Said after the assertions rather than before, so that it is the last
        // thing on the screen: a green run here is evidence about borderless
        // fullscreen and nothing else.
        eprintln!(
            "\n*** NOT EXERCISED: Windows refused the display exclusively, so this run \
             covered borderless fullscreen only, and says nothing about the exclusive case. \
             tests/capture/README.md has what makes the difference. ***\n"
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
    /// Whether the subject's window went away before the run was over — either
    /// before capture could start or during it.
    target_lost: bool,
    undecodable: Vec<String>,
}

/// Captures the application's window for [`CAPTURE_FOR`].
fn capture(app: &TestApp, method: clipped_capture::CaptureMethod) -> Outcome {
    let (width, height) = app.client_size();
    let size = FrameSize::new(width, height).expect("the application announced a real size");
    let properties = TargetProperties::new(TargetKind::Window, size);

    let mut backend = registered_backend(method)
        .expect("selection only ever chooses a registered backend")
        .create()
        .expect("the backend should be creatable");

    let mut outcome = Outcome::default();
    let target = CaptureTarget::new(TargetHandle::from_raw(app.window() as u64), properties);
    match backend.initialise(
        &target,
        &CaptureConfig::default().with_capture_cursor(false),
    ) {
        Ok(format) => eprintln!("[info] capturing through {method} at {format}"),
        // The subject announced a window and then lost it. That is a finding
        // about the subject, not a broken backend, and the caller decides what
        // it means — so it is recorded rather than panicked on.
        Err(CaptureError::TargetLost { .. }) => {
            eprintln!("[info] the subject's window had already gone when capture tried to start");
            outcome.target_lost = true;
            return outcome;
        }
        Err(error) => panic!("capturing a fullscreen application's window should start: {error}"),
    }

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
                eprintln!(
                    "[info] the subject's window went away after {} delivered frames",
                    outcome.delivered
                );
                outcome.target_lost = true;
                break;
            }
            Err(error) => panic!("capture failed after {} frames: {error}", outcome.delivered),
        }
    }

    backend.shut_down();
    outcome
}
