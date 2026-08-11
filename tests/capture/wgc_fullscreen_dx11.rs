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
//! # The answer
//!
//! Windows Graphics Capture does capture a window that holds a display through
//! `IDXGISwapChain::SetFullscreenState`. It delivers about nine frames in ten
//! of the three hundred a 60 fps subject could present in five seconds —
//! measurably fewer than the *same* run delivers when Windows refuses the
//! transition and the subject is a borderless window covering the same display,
//! which returns 290 to 301 of 300. That gap is a finding in its own right and
//! is not yet explained; `docs/capture-pipeline.md` has the table and
//! [issue #192](https://github.com/wildware-uk/clipped/issues/192) owns it.
//!
//! # Windows decides whether there is anything to measure
//!
//! `SetFullscreenState` needs the foreground, and on this machine exactly one
//! thing decides whether a test-started process gets it: **whether a process
//! that synthesised an input event is still running.** Not the launch path, not
//! how long the session has been idle, and not whether the displays are powered
//! on — all three were varied and none of them moved the answer.
//! `tests/capture/README.md` has the measurements and the procedure; there is
//! no way for this test to create that state for itself, and it does not try —
//! injecting input into somebody's session because they ran `cargo test` is not
//! this test's business.
//!
//! So an unattended run is worth nothing unless it says which case it got:
//!
//! - **Granted (`exclusive=yes`).** This is the case issue #12 exists to check,
//!   and it is asserted: the subject has to survive the run, the frames have to
//!   arrive, and every one of them has to decode as the pattern.
//! - **Refused (`exclusive=no`).** The subject is then a borderless window
//!   covering the display, which is what a game in "fullscreen (windowed)" mode
//!   is. The same assertions apply, against a tighter frame floor — but the run
//!   is *not* evidence about exclusive fullscreen, so it prints `NOT EXERCISED`
//!   and, under `CLIPPED_REQUIRE_CAPTURE`, **fails**. A green run that never
//!   reached the one case the test is named for is worse than no run at all
//!   (AGENTS.md section 54), and on an untouched machine it is the run you get.
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
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};

use readback::FrameReader;

/// The rate the test application presents at: a game-like 60.
const SOURCE_FPS: u32 = 60;

/// How long the test captures for.
///
/// Short on purpose. A display held exclusively is a display nobody else can
/// use, and five seconds at 60 fps is still three hundred frames of evidence.
const CAPTURE_FOR: Duration = Duration::from_secs(5);

/// The frames the source could have presented while capture was running, which
/// is what every count below is a fraction of.
const POSSIBLE_FRAMES: u64 = SOURCE_FPS as u64 * CAPTURE_FOR.as_secs();

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

/// Reports that the run happened but never reached exclusive fullscreen.
///
/// A different statement from [`skipped`], and the one this test needs most:
/// the machine could run it, it did run, and the single case it is named for
/// did not occur, because Windows refused the display. Under
/// [`REQUIRE_CAPTURE`] that is a failure for exactly the reason a skip is — a
/// run whose numbers are being recorded must not report "ok" for a case it
/// never entered (AGENTS.md section 54). Without the variable it is a loud
/// note, because a refusal is what an untouched machine gets, and failing every
/// casual run over Windows' focus policy would teach people to ignore this
/// test.
fn not_exercised(reason: &str) {
    assert!(
        std::env::var_os(REQUIRE_CAPTURE).is_none_or(|value| value.is_empty()),
        "{REQUIRE_CAPTURE} is set, so this run had to exercise exclusive fullscreen and did \
         not: {reason} tests/capture/README.md has what decides it and the procedure that \
         produces a grant."
    );
    let _ = writeln!(
        std::io::stderr(),
        "\n*** NOT EXERCISED: {reason} This run covered borderless fullscreen only and says \
         nothing about the exclusive case. Set {REQUIRE_CAPTURE} to make that a failure \
         rather than a pass; tests/capture/README.md has the procedure. ***\n"
    );
}

/// How long the session has been idle, as Windows measures it for the display
/// timeout.
///
/// Printed with every result. It is not what decides whether the display is
/// granted — that was measured, at idle times from thirty seconds to twenty
/// minutes, and it made no difference — but it is the cheapest thing that says
/// how awake the machine was, and a run taken on a machine nobody has touched
/// can deliver materially fewer frames because the *subject* presents fewer. A
/// frame count without it is not reproducible.
fn session_idle() -> Duration {
    let size = u32::try_from(size_of::<LASTINPUTINFO>()).unwrap_or(0);
    let mut info = LASTINPUTINFO {
        cbSize: size,
        dwTime: 0,
    };
    // SAFETY: `info` is a live local with `cbSize` set, which is the call's one
    // requirement; it only reads the session's last-input tick.
    if !unsafe { GetLastInputInfo(&raw mut info) }.as_bool() {
        return Duration::ZERO;
    }
    // SAFETY: no preconditions — it reads a counter. `wrapping_sub` because
    // both are the same 32-bit millisecond tick, which wraps every 49 days.
    let now = unsafe { GetTickCount() };
    Duration::from_millis(u64::from(now.wrapping_sub(info.dwTime)))
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
    let idle = session_idle();
    eprintln!(
        "[info] Windows {} the display exclusively",
        if granted { "granted" } else { "refused" }
    );

    let outcome = capture(&app, selection.method());

    eprintln!(
        "\n=== wgc_fullscreen_dx11 ===\n\
         exclusive granted    : {}\n\
         client size          : {}x{}\n\
         session idle at start: {:.0}s\n\
         frames delivered     : {} of a possible {POSSIBLE_FRAMES}\n\
         frames decoded       : {}\n\
         acquisition timeouts : {}\n\
         backend said missed  : {}\n\
         counters             : {} to {}\n\
         subject survived     : {}\n\
         undecodable frames   : {}{}\n",
        if granted { "yes" } else { "no" },
        app.client_size().0,
        app.client_size().1,
        idle.as_secs_f64(),
        outcome.delivered,
        outcome.decoded,
        outcome.timeouts,
        outcome
            .missed
            .map_or_else(|| "not reported".to_owned(), |missed| missed.to_string()),
        outcome.first.unwrap_or(0),
        outcome.last.unwrap_or(0),
        if outcome.lost_after.is_some() {
            "no"
        } else {
            "yes"
        },
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
        outcome.lost_after.is_none(),
        "the subject's window went away {}, in a run it was asked to keep going for 60s, so \
         there was nothing left to capture. The known way to reach this is a display that \
         has been powered off — Windows then revokes the exclusive mode a frame after \
         granting it and the subject does not survive that, which is \
         https://github.com/wildware-uk/clipped/issues/178. The subject's own message is on \
         standard error above.",
        match outcome.lost_after {
            Some(after) if after.is_zero() => "before capture could start at all".to_owned(),
            Some(after) => format!("{:.1}s into the capture", after.as_secs_f64()),
            None => String::new(),
        }
    );

    let floor = frame_floor(granted);
    assert!(
        outcome.decoded >= floor,
        "only {} of the {POSSIBLE_FRAMES} frames a {SOURCE_FPS} fps source could present in \
         {:.0}s decoded, against a floor of {floor}, while capturing an application covering \
         a display{}. Windows Graphics Capture composed {} frames, timed out {} times and \
         reported {} frames missed; the session had been idle for {:.0}s when the run \
         started, and a machine nobody has touched winds its whole display pipeline down \
         (tests/capture/README.md).",
        outcome.decoded,
        CAPTURE_FOR.as_secs_f64(),
        if granted {
            " that was holding it exclusively, which is the case issue #12 exists to check"
        } else {
            " as a borderless window, Windows having refused the exclusive transition"
        },
        outcome.delivered,
        outcome.timeouts,
        outcome
            .missed
            .map_or_else(|| "no".to_owned(), |missed| missed.to_string()),
        idle.as_secs_f64()
    );

    if !granted {
        // After the assertions rather than before, so that a refused run still
        // checks everything it legitimately can, and so that this is the last
        // thing left on the screen.
        not_exercised("Windows refused the display exclusively.");
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

/// The fewest frames a run may decode and still be worth anything.
///
/// Two numbers, because the two cases differ by more than ten points of
/// delivery — which is the finding under "Exclusive fullscreen" in
/// `docs/capture-pipeline.md`, not an artefact of this test. Both were measured
/// on the development machine (Windows 11 build 26200, RTX 4090, 2560x1440 at
/// 60 fps, five seconds), same binary, same readback, interleaved within
/// minutes of each other:
///
/// - **Refused** — a borderless window covering the display — delivered 301,
///   300, 300 and 298 of 300 on an active session and 290 on one that had been
///   idle for ten minutes, so the floor is 90%. That is also the case a machine
///   without the state described in `tests/capture/README.md` gets, so it is
///   the run that happens most often, and it can afford to be the stricter of
///   the two.
/// - **Granted** — the window holding the display — delivered 266, 268, 270,
///   272, 272, 274, 274 and 279 of 300 on an active session, and 229 once on
///   one idle for ten minutes. So 90% is not defensible here however much one
///   would like it to be: an honest floor sits below the worst measurement,
///   which puts it at 70%. Anything that drops delivery further is a regression
///   this catches, and
///   [issue #192](https://github.com/wildware-uk/clipped/issues/192) is where
///   the gap between the two cases gets explained and this floor tightened.
///
/// Both floors sit below the worst figure measured for their case rather than
/// near the typical one, because the alternative is a test that fails for
/// reasons nobody can act on — and a floor that is only ever hit by a machine
/// nobody has touched teaches contributors to re-run rather than to look.
fn frame_floor(granted: bool) -> u64 {
    if granted {
        POSSIBLE_FRAMES * 7 / 10
    } else {
        POSSIBLE_FRAMES * 9 / 10
    }
}

/// What the capture saw.
#[derive(Debug, Default)]
struct Outcome {
    delivered: u64,
    decoded: u64,
    timeouts: u64,
    first: Option<u32>,
    last: Option<u32>,
    /// What the backend itself reported as missed, summed over the run. [`None`]
    /// when it never reported the figure at all, which is not the same as zero.
    ///
    /// Here to answer one question and no more: whether the frames this test
    /// does not receive are frames it was too slow to collect or frames the
    /// compositor never produced. The fuller accounting — drops, duplicates,
    /// ordering — already exists in `tests/capture/wgc_video_pattern.rs` and
    /// belongs in a module both tests share rather than copied into this one
    /// (AGENTS.md section 55, and issue #192).
    missed: Option<u64>,
    /// How far into the capture the subject's window went, when it did. [`None`]
    /// means it survived; `Some(Duration::ZERO)` means it had already gone
    /// before capture could start. The two print differently because they are
    /// debugged differently.
    lost_after: Option<Duration>,
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
            outcome.lost_after = Some(Duration::ZERO);
            return outcome;
        }
        Err(error) => panic!("capturing a fullscreen application's window should start: {error}"),
    }

    let mut reader = FrameReader::default();
    let mut region = None;
    let started = Instant::now();
    let deadline = started + CAPTURE_FOR;

    while Instant::now() < deadline {
        match backend.acquire(Duration::from_millis(100)) {
            Ok(Acquisition::Frame(frame)) => {
                outcome.delivered += 1;
                if let Some(missed) = frame.frames_missed() {
                    *outcome.missed.get_or_insert(0) += u64::from(missed);
                }
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
                outcome.lost_after = Some(started.elapsed());
                break;
            }
            Err(error) => panic!("capture failed after {} frames: {error}", outcome.delivered),
        }
    }

    backend.shut_down();
    outcome
}
