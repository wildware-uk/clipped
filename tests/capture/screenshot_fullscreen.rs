//! End-to-end: photograph a subject that has taken a whole display, and read
//! the test pattern back out of the file that was saved.
//!
//! # What this is for
//!
//! [Issue #67](https://github.com/wildware-uk/clipped/issues/67)'s first
//! acceptance criterion is that screenshots are correct for windowed,
//! borderless **and fullscreen** games. `tests/capture/screenshot.rs` covers
//! the first two against `test-apps/video-pattern`, and could not cover the
//! third: the fullscreen subject is a different package, and a test belongs to
//! the package that owns the binary it starts
//! ([issue #452](https://github.com/wildware-uk/clipped/issues/452)).
//!
//! # Why fullscreen is not obviously the same path
//!
//! It would be easy to assume a still is a still. Two findings say otherwise.
//! [#12](https://github.com/wildware-uk/clipped/issues/12) found that DXGI
//! grants exclusive fullscreen only under conditions automation struggles to
//! reach, and [#178](https://github.com/wildware-uk/clipped/issues/178) found
//! that Windows can revoke the mode one frame after granting it. A still copied
//! out of a frame delivered under either of those is exactly where a size, a
//! stride or a stale frame would differ — and a test that only checked the file
//! existed would photograph the difference and pass.
//!
//! So the assertion is the same one the windowed cases make: the **file on
//! disk** decodes back to a frame the subject drew. The pattern's cells carry a
//! magic sequence and the source's own frame number, so a picture that came out
//! the wrong size, offset by a border, sheared by a wrong stride, or assembled
//! from two frames fails to decode rather than looking slightly wrong.
//!
//! # Windows decides whether the exclusive case happens
//!
//! `SetFullscreenState` needs the foreground, and on this machine what decides
//! whether a test-started process gets it is whether a process that synthesised
//! an input event is still running — `tests/capture/README.md` has the
//! measurements. There is no way for this test to create that state, and it does
//! not try.
//!
//! What it does instead is say which case it got, exactly as
//! `wgc_fullscreen_dx11.rs` does:
//!
//! - **Granted.** The screenshot is of a display held exclusively, which is the
//!   case #67 names, and it is asserted in full.
//! - **Refused.** The subject is then a borderless window covering the display —
//!   worth photographing, and asserted just as hard — but the run says nothing
//!   about exclusive fullscreen. It prints `NOT EXERCISED`, and under
//!   `CLIPPED_REQUIRE_CAPTURE` it fails. A green run that never reached the case
//!   it is named for is worse than no run at all (AGENTS.md section 54).
//!
//! # Why it is `#[ignore]`d
//!
//! It takes a display away from whoever is using the machine, and needs a GPU
//! and a desktop session. That is not something `cargo test` should do because
//! somebody typed `cargo test`.
//!
//! ```text
//! cargo test -p clipped-fullscreen-dx11 --test screenshot_fullscreen -- --ignored --nocapture
//! ```

use core::time::Duration;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use clipped_session::screenshot::{
    capture_still, write, ScreenshotFormat, ScreenshotSettings, StillFrame,
};
use clipped_session::CaptureTargetSettings;
use clipped_video_pattern::harness::TestApp;
use clipped_video_pattern::pattern::{self, Region, Surface};

/// The rate the subject presents at: a game-like 60.
///
/// Only one frame is photographed, so the rate matters only in that the display
/// has to be drawing when the screenshot asks — which is the case being tested.
const SOURCE_FPS: u32 = 60;

/// How long the application is given to appear.
///
/// Longer than the windowed screenshot test's: a mode switch takes a moment,
/// and the `ready` line comes after it.
const READY_TIMEOUT: Duration = Duration::from_secs(45);

/// How long the application is given to stop, and give the display back.
const STOP_TIMEOUT: Duration = Duration::from_secs(15);

/// A backstop far beyond this test, so a panic cannot leave a display held.
const SUBJECT_SECONDS: &str = "60";

/// The environment variable that turns "this machine could not run the test"
/// from a pass into a failure, as everywhere else in `clipped-capture`.
const REQUIRE_CAPTURE: &str = "CLIPPED_REQUIRE_CAPTURE";

/// Reports that the test could not run here.
///
/// Panics rather than skipping when [`REQUIRE_CAPTURE`] is set, so a machine
/// that is supposed to capture cannot quietly stop testing capture, and writes
/// through `std::io::stderr()` rather than `eprintln!` because libtest captures
/// the macro — a skip nobody can see is the failure this exists to prevent.
fn skipped(reason: &str) {
    assert!(
        std::env::var_os(REQUIRE_CAPTURE).is_none_or(|value| value.is_empty()),
        "{REQUIRE_CAPTURE} is set, so this must not be skipped: {reason}"
    );
    let _ = writeln!(std::io::stderr(), "SKIPPED (capture): {reason}");
}

/// Reports that the run happened but never reached exclusive fullscreen.
///
/// A different statement from [`skipped`]: the machine could run it, it did run,
/// and the one case it is named for did not occur because Windows refused the
/// display. Everything below is still asserted against the borderless subject
/// that results — the screenshot has to be the pattern either way — so this is a
/// note about what the run is *evidence* of rather than about whether it passed.
fn not_exercised(reason: &str) {
    assert!(
        std::env::var_os(REQUIRE_CAPTURE).is_none_or(|value| value.is_empty()),
        "{REQUIRE_CAPTURE} is set, so this run had to photograph an exclusive fullscreen \
         subject and did not: {reason} tests/capture/README.md has what decides it and the \
         procedure that produces a grant."
    );
    let _ = writeln!(
        std::io::stderr(),
        "\n*** NOT EXERCISED: {reason} This run photographed a borderless window covering the \
         display and says nothing about the exclusive case. Set {REQUIRE_CAPTURE} to make that \
         a failure rather than a pass; tests/capture/README.md has the procedure. ***\n"
    );
}

#[test]
#[ignore = "takes over a display and needs a GPU; see the module docs"]
fn a_screenshot_of_a_fullscreen_subject_is_the_pattern_it_drew() {
    // Before any size is read, for the reason `wgc_fullscreen_dx11.rs` does it:
    // the subject is per-monitor DPI aware and reports physical pixels, and a
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

    let app = TestApp::start(
        env!("CARGO_BIN_EXE_fullscreen-dx11"),
        [
            "--mode",
            "exclusive",
            "--fps",
            &SOURCE_FPS.to_string(),
            "--seconds",
            SUBJECT_SECONDS,
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

    let display = (
        expected.bounds().size().width(),
        expected.bounds().size().height(),
    );
    assert_eq!(
        app.client_size(),
        display,
        "a fullscreen application should cover the whole display"
    );

    let granted = app.is_exclusive();
    eprintln!(
        "[info] Windows {} the display exclusively",
        if granted { "granted" } else { "refused" }
    );

    // The photograph, and the decode, before anything is said about which case
    // this was: the assertions are the same either way, and a refusal that
    // stopped the test would leave the borderless case unchecked too.
    let still = photograph(&app);
    assert_eq!(
        (still.size().width(), still.size().height()),
        display,
        "the screenshot of a subject covering the display should be the size of the display"
    );

    let region = locate(&still);
    assert_eq!(
        (region.x, region.y),
        (0, 0),
        "a window covering a display has no chrome, so its pattern starts at the picture's \
         top-left corner"
    );

    let found = save_and_decode(&still, region);

    eprintln!(
        "\n=== screenshot_fullscreen ===\n\
         exclusive granted: {}\n\
         display          : {} {}x{}\n\
         screenshot       : {}x{}, rows {} bytes apart\n\
         decoded          : source frame {}\n",
        if granted { "yes" } else { "no" },
        expected.device_name(),
        display.0,
        display.1,
        still.size().width(),
        still.size().height(),
        still.stride(),
        found
    );

    app.stop(STOP_TIMEOUT)
        .expect("the application stops and gives the display back");

    if !granted {
        not_exercised("Windows refused the display exclusively.");
    }
}

/// Takes one screenshot of the subject's window, by exactly the path the
/// recorder takes when nothing is being recorded.
fn photograph(app: &TestApp) -> StillFrame {
    let (width, height) = app.client_size();
    let target = CaptureTargetSettings::window(app.window() as u64, width, height);

    capture_still(&target).expect(
        "a display that is being drawn on produces a frame to photograph within the timeout",
    )
}

/// Finds the pattern inside the captured frame.
fn locate(still: &StillFrame) -> Region {
    let surface = surface_of(still.as_bytes(), still.stride(), still);
    pattern::locate(&surface, still.size().width(), still.size().height())
        .expect("the screenshot should contain the test pattern the application drew")
}

/// Saves the frame as a PNG, decodes the file, and returns the source frame
/// number the picture turned out to be.
///
/// PNG only, and deliberately: which formats survive a round trip is
/// `screenshot.rs`'s question and it asks it of all of them, over a subject that
/// costs nobody a display. What is new here is the *source* of the pixels, so
/// one lossless format is what isolates it.
fn save_and_decode(still: &StillFrame, region: Region) -> u32 {
    let directory = scratch();
    let settings = ScreenshotSettings::new(&directory).with_format(ScreenshotFormat::Png);

    let screenshot = write(
        still,
        &settings,
        "fullscreen-dx11",
        std::time::SystemTime::now(),
        None,
    )
    .expect("the screenshot is written");

    assert_eq!(screenshot.width(), still.size().width());
    assert_eq!(screenshot.height(), still.size().height());

    let decoded = decode_bgra(screenshot.path(), screenshot.width(), screenshot.height())
        .expect("ffmpeg from the pinned build decodes what Clipped wrote");
    let surface = surface_of(&decoded, screenshot.width() as usize * 4, still);

    let found = pattern::decode(&surface, region).unwrap_or_else(|error| {
        panic!(
            "the PNG at {} is not the pattern that was captured: {error}",
            screenshot.path().display()
        )
    });

    let _ = std::fs::remove_dir_all(&directory);
    found.index()
}

/// A pattern surface over pixels of the captured frame's size.
fn surface_of<'pixels>(
    pixels: &'pixels [u8],
    stride: usize,
    still: &StillFrame,
) -> Surface<'pixels> {
    Surface::new(pixels, stride, still.size().width(), still.size().height())
        .expect("the picture describes its own shape")
}

/// `path` decoded to tightly packed BGRA8, through the pinned build's `ffmpeg`.
///
/// The same tool and the same lookup `screenshot.rs` uses: `FFMPEG_DIR` is what
/// `scripts/fetch-ffmpeg.ps1` sets and what the build scripts already read.
fn decode_bgra(path: &Path, width: u32, height: u32) -> Option<Vec<u8>> {
    let ffmpeg = PathBuf::from(std::env::var_os("FFMPEG_DIR")?)
        .join("bin")
        .join("ffmpeg.exe");
    if !ffmpeg.is_file() {
        return None;
    }

    let output = std::process::Command::new(ffmpeg)
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "rawvideo", "-pix_fmt", "bgra", "-"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let wanted = (width as usize) * (height as usize) * 4;
    (output.stdout.len() == wanted).then_some(output.stdout)
}

/// A directory of this test's own, removed once the picture in it has been read.
fn scratch() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "clipped-screenshot-fullscreen-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("a scratch directory");
    path
}
