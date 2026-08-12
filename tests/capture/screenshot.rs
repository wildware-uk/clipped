//! End-to-end: photograph a real window through the real capture backend, save
//! the file, and read the test pattern back out of the picture that was saved.
//!
//! This is the acceptance criterion "screenshots are correct for windowed,
//! borderless and fullscreen games" made checkable
//! ([issue #67](https://github.com/wildware-uk/clipped/issues/67)). Nothing here
//! is mocked and nothing is installed: the subject is
//! `test-apps/video-pattern`, the capture is the backend the recorder uses, and
//! the assertion is that the **file on disk** decodes back to the pattern the
//! application drew.
//!
//! # Why decoding the saved file is the whole point
//!
//! A screenshot test that checks the file exists, or that it begins with the
//! PNG signature, passes for a picture of the wrong window, the wrong size, the
//! wrong colours, or a frame assembled from two others. The pattern survives
//! none of those: its cells carry a magic sequence and the source's own frame
//! number, and a picture whose background and marker disagree with that number
//! fails to decode (`clipped_video_pattern::pattern`). So what is asserted is
//! that the bytes Clipped wrote decode as a frame the subject drew, rather than
//! that the bytes are self-consistent.
//!
//! # Why it is `#[ignore]`d
//!
//! It puts a window on a display and needs a GPU, a desktop session and a
//! compositor, so it is not part of the pull-request CI job — the same reason
//! `tests/capture/README.md` gives for every other file in this directory.
//! `#[ignore]` rather than a silent skip is deliberate: a test that decides for
//! itself that it could not run reads as a pass.
//!
//! Run it, on a machine with a display:
//!
//! ```text
//! cargo test -p clipped-video-pattern --test screenshot -- --ignored --nocapture --test-threads=1
//! ```

use core::time::Duration;
use std::path::{Path, PathBuf};

use clipped_session::screenshot::{
    capture_still, file_name, write, ScreenshotFormat, ScreenshotSettings, StillFrame,
};
use clipped_session::CaptureTargetSettings;
use clipped_video_pattern::harness::TestApp;
use clipped_video_pattern::pattern::{self, Region, Surface};

/// The rate the test application presents at.
///
/// Only one frame is kept, so the rate matters only in that the window has to be
/// drawing when the screenshot asks — which is the case being tested.
const SOURCE_FPS: u32 = 30;

/// How long the application is given to appear before the test gives up.
const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the application is given to stop after it is asked to.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

#[test]
#[ignore = "needs a GPU, a desktop session and a display; see the module documentation"]
fn a_screenshot_of_a_borderless_window_is_the_pattern_the_application_drew() {
    let app = start("borderless");
    let still = photograph(&app);

    let region = locate(&still);
    assert_eq!(
        (region.x, region.y),
        (0, 0),
        "a borderless window has no chrome, so its pattern starts at the picture's top-left \
         corner"
    );
    save_and_decode(&still, region, ScreenshotFormat::Png);

    app.stop(STOP_TIMEOUT).expect("the application stops");
}

#[test]
#[ignore = "needs a GPU, a desktop session and a display; see the module documentation"]
fn a_screenshot_of_a_bordered_window_is_the_whole_window_with_the_pattern_inside_it() {
    // The case a borderless capture cannot exercise: Windows Graphics Capture
    // captures a *window*, so the picture includes the title bar and the border
    // and the pattern is offset inside it. A screenshot that had cropped to the
    // wrong rectangle, or shifted by the border, fails to locate the pattern at
    // all rather than looking slightly wrong.
    let app = start("windowed");
    let still = photograph(&app);

    let region = locate(&still);
    println!(
        "the pattern sits at ({}, {}) inside the window",
        region.x, region.y
    );
    save_and_decode(&still, region, ScreenshotFormat::Png);

    app.stop(STOP_TIMEOUT).expect("the application stops");
}

#[test]
#[ignore = "needs a GPU, a desktop session and a display; see the module documentation"]
fn every_format_this_build_writes_decodes_back_to_the_pattern() {
    // JPEG is lossy, so its pixels are not identical to the frame's — but the
    // pattern's cells are large flat blocks of saturated colour and survive it.
    // A picture that came out transposed, offset, or with its channels swapped
    // does not, whichever format it is in.
    let app = start("borderless");
    let still = photograph(&app);
    let region = locate(&still);

    for format in ScreenshotFormat::ALL {
        if format.is_available() {
            save_and_decode(&still, region, format);
        } else {
            println!("skipped {format}: this FFmpeg build has no encoder for it");
        }
    }

    app.stop(STOP_TIMEOUT).expect("the application stops");
}

/// Starts the test application in the presentation mode named, and waits for its
/// window.
fn start(mode: &str) -> TestApp {
    let app = TestApp::start(
        env!("CARGO_BIN_EXE_video-pattern"),
        [
            "--mode",
            mode,
            "--fps",
            &SOURCE_FPS.to_string(),
            // A hard backstop well beyond this test: if it panics between
            // starting the application and stopping it, the application still
            // goes away on its own.
            "--seconds",
            "120",
        ],
        READY_TIMEOUT,
    )
    .expect("the video pattern application should start and announce itself");

    assert_eq!(
        app.presentation(),
        mode,
        "this test asked for a {mode} window"
    );
    app
}

/// Takes one screenshot of the application's window, by exactly the path the
/// recorder takes when nothing is being recorded.
fn photograph(app: &TestApp) -> StillFrame {
    let (width, height) = app.client_size();
    let target = CaptureTargetSettings::window(app.window() as u64, width, height);

    let still = capture_still(&target)
        .expect("a window that is drawing produces a frame to photograph within the timeout");
    println!(
        "captured {}x{}, rows {} bytes apart",
        still.size().width(),
        still.size().height(),
        still.stride()
    );
    still
}

/// Finds the pattern inside the captured frame.
fn locate(still: &StillFrame) -> Region {
    let surface = surface_of(still.as_bytes(), still.stride(), still);
    pattern::locate(
        &surface,
        still.size().width().min(1_280),
        still.size().height().min(720),
    )
    .or_else(|| {
        // The pattern is the application's client area, whatever that turned
        // out to be; asking for its exact size is the second attempt so that
        // the common sizes above are tried first.
        pattern::locate(&surface, still.size().width(), still.size().height())
    })
    .expect("the screenshot should contain the test pattern the application drew")
}

/// Saves the frame in `format` and decodes the pattern back out of the file.
fn save_and_decode(still: &StillFrame, region: Region, format: ScreenshotFormat) {
    let directory = scratch(format.name());
    let settings = ScreenshotSettings::new(&directory).with_format(format);
    let taken_at = std::time::SystemTime::now();

    let screenshot = write(still, &settings, "video-pattern", taken_at, None)
        .expect("the screenshot is written");

    assert_eq!(
        screenshot.path().file_name().and_then(|name| name.to_str()),
        Some(file_name("video-pattern", taken_at, format, 1).as_str()),
        "the file is not named the way `file_name` says it is"
    );
    assert_eq!(screenshot.width(), still.size().width());
    assert_eq!(screenshot.height(), still.size().height());

    let decoded = decode_bgra(screenshot.path(), screenshot.width(), screenshot.height())
        .expect("ffmpeg from the pinned build decodes what Clipped wrote");
    let surface = surface_of(&decoded, screenshot.width() as usize * 4, still);

    let found = pattern::decode(&surface, region).unwrap_or_else(|error| {
        panic!(
            "the {format} at {} is not the pattern that was captured: {error}",
            screenshot.path().display()
        )
    });

    println!(
        "{format}: {} bytes, decoded source frame {}",
        screenshot.bytes(),
        found.index()
    );

    let _ = std::fs::remove_dir_all(&directory);
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
/// The same tool `tests/media` uses, found the same way: `FFMPEG_DIR` is what
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
fn scratch(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "clipped-screenshot-system-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("a scratch directory");
    path
}
