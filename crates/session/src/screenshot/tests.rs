//! Screenshots, end to end, with a frame this test painted instead of a game.
//!
//! # What this can prove without a GPU
//!
//! Nearly everything that is actually decided here. A [`StillFrame`] is pixels,
//! a stride and a format, and it can be built by hand — so the naming, the
//! directory, the collision rule, the encoding, the atomic write and the
//! failure when a disk refuses are all exercised against real files and real
//! FFmpeg encoders on any machine (AGENTS.md section 26).
//!
//! What it cannot prove is that the frame came off a real capture. That is
//! `crates/capture/src/windows/still.rs`'s tests, which do it against a real
//! Direct3D texture, and the end-to-end system test in
//! `tests/capture/screenshot.rs`, which does it against a real window and is
//! gated behind `CLIPPED_REQUIRE_CAPTURE` for the reason `docs/testing.md`
//! gives.
//!
//! # The pictures are decoded again before they are believed
//!
//! A test that asserts a PNG is "some bytes beginning with the PNG signature"
//! passes for a PNG of the wrong size, the wrong colours or the wrong picture
//! entirely. So every format written here is read back with `ffprobe` — the
//! same tool `tests/media` uses for recordings — and checked for its codec and
//! its dimensions (AGENTS.md section 22).

use core::time::Duration;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::SystemTime;

use clipped_capture::{
    CaptureTimestamp, FrameFormat, FrameSize, PixelFormat, SourceClock, StillFrame,
};

use super::{
    default_directory, file_name, write, Screenshot, ScreenshotError, ScreenshotFormat,
    ScreenshotRequests, ScreenshotSettings, ServedStill,
};

/// A moment with a known calendar form: 2026-08-11T14:32:05Z.
fn moment() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_786_458_725)
}

/// A directory that deletes itself.
///
/// The same shape `clipped-media-validation` provides, written here because
/// this crate's unit tests must run without the media harness's FFmpeg
/// discovery — the encoder is linked, not invoked.
#[derive(Debug)]
struct TemporaryDirectory {
    path: PathBuf,
}

impl TemporaryDirectory {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "clipped-screenshot-{label}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("a temporary directory");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

const WIDTH: u32 = 64;
const HEIGHT: u32 = 32;

/// The colour of pixel `(x, y)` in the test picture, as BGRA8 stores one.
///
/// A gradient in two channels and a diagonal in the third: an image written
/// with the wrong stride, transposed, or shifted by a row differs from this in
/// a way an assertion catches, where a flat colour would survive all three.
fn pixel(x: u32, y: u32) -> [u8; 4] {
    let red = u8::try_from(x % 256).expect("modulo 256 fits in a byte");
    let green = u8::try_from(y % 256).expect("modulo 256 fits in a byte");
    let blue = u8::try_from((x + y) % 256).expect("modulo 256 fits in a byte");
    [blue, green, red, 0xFF]
}

/// A frame of [`pixel`], with the row padding a real capture has.
///
/// The padding is not decoration. Direct3D hands back rows at whatever pitch
/// the driver chose, and a `stride == width * 4` fixture would let a stride bug
/// through every one of these tests.
fn still() -> StillFrame {
    let stride = WIDTH as usize * 4 + 48;
    let mut pixels = vec![0_u8; stride * HEIGHT as usize];
    for y in 0..HEIGHT {
        let row = &mut pixels[y as usize * stride..][..WIDTH as usize * 4];
        for x in 0..WIDTH {
            row[x as usize * 4..][..4].copy_from_slice(&pixel(x, y));
        }
    }

    StillFrame::new(
        pixels,
        stride,
        FrameFormat::new(
            FrameSize::new(WIDTH, HEIGHT).expect("the test size is not zero"),
            PixelFormat::Bgra8Unorm,
        ),
        CaptureTimestamp::from_source(SourceClock::PerformanceCounter, 42),
    )
    .expect("the fixture holds every row it claims to")
}

/// Writes the fixture into `directory` and returns what was written.
fn save(directory: &Path, format: ScreenshotFormat) -> Result<Screenshot, ScreenshotError> {
    write(
        &still(),
        &ScreenshotSettings::new(directory).with_format(format),
        "counter-strike-2",
        moment(),
        None,
    )
}

#[test]
fn a_screenshot_is_named_like_the_recordings_of_the_same_session() {
    // The consistency the acceptance criteria ask for: a directory listing of
    // screenshots sorts the same way, and beside, the recordings of the game
    // they were taken in. `clipped-<game>-<yyyymmdd>-<hhmmss>` is the stem
    // `crate::automatic::SessionId` already uses.
    let name = file_name("counter-strike-2", moment(), ScreenshotFormat::Png, 1);
    assert!(name.starts_with("clipped-counter-strike-2-"), "{name}");
    assert!(name.ends_with(".png"), "{name}");
    assert!(
        !name.contains([':', '/', '\\', '*', '?', '"', '<', '>', '|']),
        "a screenshot name ends up on a Windows filesystem: {name}"
    );

    // The stamp is local time, so the digits depend on where this machine is —
    // but the shape does not, and neither does the length.
    let stamp = name
        .trim_start_matches("clipped-counter-strike-2-")
        .trim_end_matches(".png");
    assert_eq!(stamp.len(), 15, "unexpected stamp in {name}");
    assert!(
        stamp.chars().all(|c| c.is_ascii_digit() || c == '-'),
        "unexpected stamp in {name}"
    );
}

#[test]
fn a_screenshot_with_no_game_is_filed_under_the_word_a_session_uses() {
    // Not "unknown", not the empty string, and not a name with two hyphens in
    // it where the game should be: `unattributed` is what a session whose game
    // the catalogue would not name is already filed under, and a screenshot
    // taken outside any session is the same situation.
    let name = file_name("", moment(), ScreenshotFormat::Jpeg, 1);
    assert!(name.starts_with("clipped-unattributed-"), "{name}");
    assert!(name.ends_with(".jpg"), "{name}");
}

#[test]
fn each_format_gets_the_extension_the_operating_system_expects() {
    assert_eq!(ScreenshotFormat::Png.extension(), "png");
    assert_eq!(ScreenshotFormat::Jpeg.extension(), "jpg");
    assert_eq!(ScreenshotFormat::WebP.extension(), "webp");

    for format in ScreenshotFormat::ALL {
        assert_eq!(
            ScreenshotFormat::from_name(format.name()),
            Some(format),
            "{format} does not parse back from its own name"
        );
    }
}

#[test]
fn two_screenshots_in_the_same_second_are_two_files() {
    // The one that protects a picture. Named to the second and written without
    // this rule, the second press of the key silently replaces the first — a
    // screenshot the user took and can never take again (AGENTS.md section 56).
    let directory = TemporaryDirectory::new("collide");

    let first = save(directory.path(), ScreenshotFormat::Png).expect("the first is written");
    let second = save(directory.path(), ScreenshotFormat::Png).expect("the second is written");
    let third = save(directory.path(), ScreenshotFormat::Png).expect("the third is written");

    assert_ne!(first.path(), second.path());
    assert_ne!(second.path(), third.path());
    assert!(first.path().exists() && second.path().exists() && third.path().exists());

    assert!(
        second
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with("-2.png")),
        "the second screenshot of a second should be -2: {}",
        second.path().display()
    );
}

#[test]
fn the_directory_is_created_rather_than_the_screenshot_being_refused() {
    // %USERPROFILE%\Pictures\Clipped does not exist until Clipped makes it, and
    // the first screenshot somebody ever takes must not be the one that fails.
    let directory = TemporaryDirectory::new("create");
    let nested = directory.path().join("Pictures").join("Clipped");
    assert!(!nested.exists());

    let screenshot = save(&nested, ScreenshotFormat::Png).expect("the directory is created");
    assert!(screenshot.path().starts_with(&nested));
    assert!(nested.is_dir());
}

#[test]
fn nothing_is_left_behind_when_a_screenshot_is_written() {
    // The temporary file the atomic write goes through must not survive as a
    // `.png.tmp` in the user's pictures folder, and must not be counted as a
    // screenshot by storage accounting.
    let directory = TemporaryDirectory::new("atomic");
    let screenshot = save(directory.path(), ScreenshotFormat::Png).expect("the file is written");

    let left: Vec<_> = fs::read_dir(directory.path())
        .expect("the directory is readable")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .collect();

    assert_eq!(left.len(), 1, "unexpected files left behind: {left:?}");
    assert_eq!(
        left[0],
        screenshot
            .path()
            .file_name()
            .expect("a written screenshot has a name")
    );
}

#[test]
fn a_png_screenshot_is_the_frame_pixel_for_pixel() {
    // The assertion that makes this a screenshot rather than an image of the
    // right size. A PNG is lossless, so the decoded picture must equal the
    // frame exactly — a stride bug, a channel swap or a dropped row all fail
    // here, and none of them would fail a "the file starts with 0x89 PNG" test.
    let directory = TemporaryDirectory::new("png-pixels");
    let screenshot = save(directory.path(), ScreenshotFormat::Png).expect("the PNG is written");

    let Some(decoded) = crate::screenshot::tests::decode_rgb(screenshot.path()) else {
        eprintln!("skipped: no ffmpeg in this checkout to decode the screenshot with");
        return;
    };

    assert_eq!(decoded.len(), (WIDTH * HEIGHT * 3) as usize);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let [blue, green, red, _] = pixel(x, y);
            let offset = ((y * WIDTH + x) * 3) as usize;
            assert_eq!(
                (decoded[offset], decoded[offset + 1], decoded[offset + 2]),
                (red, green, blue),
                "pixel ({x}, {y}) came back wrong"
            );
        }
    }
}

#[test]
fn every_format_this_build_has_produces_a_picture_of_the_right_size() {
    // Written, then read back with ffprobe: the codec in the file and the
    // dimensions of the picture, not the extension the file was given
    // (AGENTS.md section 22).
    let directory = TemporaryDirectory::new("formats");

    for format in ScreenshotFormat::ALL {
        if !format.is_available() {
            // A build without libwebp says so rather than being silently
            // skipped; see `an_unavailable_format_is_refused_by_name`.
            eprintln!("skipped {format}: this FFmpeg build has no encoder for it");
            continue;
        }

        let screenshot = save(directory.path(), format).expect("the screenshot is written");
        assert_eq!(screenshot.width(), WIDTH);
        assert_eq!(screenshot.height(), HEIGHT);
        assert!(screenshot.bytes() > 0);
        assert_eq!(
            fs::metadata(screenshot.path())
                .expect("the file is on disk")
                .len(),
            screenshot.bytes(),
            "{format}: the reported size is not the file's"
        );

        let Some((codec, width, height)) = probe(screenshot.path()) else {
            eprintln!("skipped the probe of {format}: no ffprobe in this checkout");
            continue;
        };
        assert_eq!(
            (width, height),
            (WIDTH, HEIGHT),
            "{format} is the wrong size"
        );
        assert_eq!(
            codec,
            match format {
                ScreenshotFormat::Png => "png",
                ScreenshotFormat::Jpeg => "mjpeg",
                ScreenshotFormat::WebP => "webp",
            },
            "{format} was written as something else"
        );
    }
}

#[test]
fn a_lossless_webp_is_lossless() {
    // "Lossless WebP" is a promise, and libwebp's default is lossy. A build
    // that forgot the option would produce a file that opens, looks right and
    // is not what it says it is (AGENTS.md section 54).
    if !ScreenshotFormat::WebP.is_available() {
        eprintln!("skipped: this FFmpeg build has no WebP encoder");
        return;
    }

    let directory = TemporaryDirectory::new("webp-lossless");
    let screenshot = save(directory.path(), ScreenshotFormat::WebP).expect("the WebP is written");

    let Some(decoded) = decode_rgb(screenshot.path()) else {
        eprintln!("skipped: no ffmpeg in this checkout to decode the screenshot with");
        return;
    };

    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let [blue, green, red, _] = pixel(x, y);
            let offset = ((y * WIDTH + x) * 3) as usize;
            assert_eq!(
                (decoded[offset], decoded[offset + 1], decoded[offset + 2]),
                (red, green, blue),
                "pixel ({x}, {y}) is not the pixel that was captured, so this WebP is lossy"
            );
        }
    }
}

#[test]
fn the_jpeg_quality_setting_reaches_the_encoder() {
    // A setting that does nothing is worse than no setting (AGENTS.md section
    // 27), and this one is easy to get wrong in a way nothing else notices:
    // MJPEG reads `global_quality` only when `AV_CODEC_FLAG_QSCALE` is set, and
    // the per-frame `quality` field has to be set as well. Miss either and
    // every JPEG is encoded at libavcodec's default — identical bytes at both
    // ends of the scale, which is what this compares.
    let directory = TemporaryDirectory::new("quality");

    let best = write(
        &still(),
        &ScreenshotSettings::new(directory.path())
            .with_format(ScreenshotFormat::Jpeg)
            .with_jpeg_quality(super::BEST_JPEG_QUALITY),
        "game",
        moment(),
        None,
    )
    .expect("the best-quality JPEG is written");

    let worst = write(
        &still(),
        &ScreenshotSettings::new(directory.path())
            .with_format(ScreenshotFormat::Jpeg)
            .with_jpeg_quality(super::WORST_JPEG_QUALITY),
        "game",
        moment(),
        None,
    )
    .expect("the worst-quality JPEG is written");

    assert!(
        worst.bytes() < best.bytes(),
        "quantiser scale {} produced {} bytes and scale {} produced {}; the setting is not \
         reaching the encoder",
        super::WORST_JPEG_QUALITY,
        worst.bytes(),
        super::BEST_JPEG_QUALITY,
        best.bytes()
    );
}

#[test]
fn a_screenshot_that_cannot_be_written_names_the_file() {
    // A full or disconnected drive is something only the user can fix, so the
    // message has to say which file (AGENTS.md sections 15 and 45).
    let directory = TemporaryDirectory::new("unwritable");
    // A *file* where the directory should be. `create_dir_all` refuses, and it
    // is the case a user reaches by pointing the setting at a file.
    let blocked = directory.path().join("Clipped");
    fs::write(&blocked, b"not a directory").expect("the blocking file is written");

    let error = save(&blocked, ScreenshotFormat::Png)
        .expect_err("a file where the directory should be is not a directory");
    let message = error.to_string();
    assert!(
        message.contains("Clipped"),
        "the message does not name the folder: {message}"
    );
    assert!(
        matches!(error, ScreenshotError::DirectoryNotCreated { .. }),
        "unexpected error: {error}"
    );
}

#[test]
fn a_frame_this_build_cannot_read_is_refused_rather_than_written_as_noise() {
    // An HDR capture reaches here. Encoding its 10-bit samples as though they
    // were 8-bit produces a picture, and the picture is wrong.
    let directory = TemporaryDirectory::new("hdr");
    let hdr = StillFrame::new(
        vec![0_u8; 4 * WIDTH as usize * HEIGHT as usize],
        WIDTH as usize * 4,
        FrameFormat::new(
            FrameSize::new(WIDTH, HEIGHT).expect("the test size is not zero"),
            PixelFormat::Rgb10A2Unorm,
        ),
        CaptureTimestamp::from_source(SourceClock::PerformanceCounter, 1),
    )
    .expect("a well-formed 10-bit buffer");

    let error = write(
        &hdr,
        &ScreenshotSettings::new(directory.path()),
        "game",
        moment(),
        None,
    )
    .expect_err("this build cannot encode a 10-bit frame");
    assert!(
        matches!(error, ScreenshotError::Encode { .. }),
        "unexpected error: {error}"
    );

    assert_eq!(
        fs::read_dir(directory.path())
            .expect("the directory is readable")
            .count(),
        0,
        "a refused screenshot must not leave a file behind"
    );
}

#[test]
fn the_jpeg_quality_setting_is_clamped_to_what_the_encoder_accepts() {
    let settings = ScreenshotSettings::new("C:\\").with_jpeg_quality(0);
    assert_eq!(settings.jpeg_quality(), super::BEST_JPEG_QUALITY);

    let settings = ScreenshotSettings::new("C:\\").with_jpeg_quality(9_999);
    assert_eq!(settings.jpeg_quality(), super::WORST_JPEG_QUALITY);

    assert_eq!(
        ScreenshotSettings::new("C:\\").jpeg_quality(),
        super::DEFAULT_JPEG_QUALITY
    );
    assert_eq!(
        ScreenshotSettings::new("C:\\").format(),
        ScreenshotFormat::Png,
        "the default must be the lossless one"
    );
}

#[test]
fn the_default_directory_is_not_inside_the_recordings_one() {
    // Storage accounting requires that no root contains another
    // (docs/storage-management.md), and `Screenshots` and `Recordings` are two
    // of its categories. A screenshots folder nested in the recordings folder
    // would have its bytes counted under whichever root won.
    let Some(screenshots) = default_directory() else {
        eprintln!("skipped: this environment has no home directory");
        return;
    };

    assert!(
        screenshots.ends_with(Path::new("Pictures").join("Clipped")),
        "unexpected screenshot directory: {}",
        screenshots.display()
    );
    assert!(
        !screenshots
            .components()
            .any(|component| component.as_os_str() == "Videos"),
        "the screenshot directory is under the recordings directory: {}",
        screenshots.display()
    );
}

#[test]
fn a_request_nobody_serves_times_out_rather_than_waiting_for_ever() {
    // A screenshot key pressed with no recording running, or with a window that
    // has stopped drawing. Blocking for ever here would hang the tray menu.
    let requests = ScreenshotRequests::new();
    let started = std::time::Instant::now();

    let error = requests
        .take_within(Duration::from_millis(50))
        .expect_err("nothing is serving this request");

    assert!(
        matches!(error, ScreenshotError::NoFrame { .. }),
        "unexpected error: {error}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the request waited far longer than it was asked to"
    );
}

#[test]
fn a_served_request_returns_the_frame_and_its_position() {
    // The rendezvous, in the shape the capture loop drives it: the waiter is on
    // another thread, the "capture loop" claims the request and serves it, and
    // the pixels and the timeline position both arrive.
    let requests = ScreenshotRequests::new();
    let waiter = requests.clone();
    let asking = std::thread::spawn(move || waiter.take_within(Duration::from_secs(5)));

    // Wait for the request to be registered, as a capture loop would by
    // checking between frames.
    let id = loop {
        if let Some(id) = requests.claim() {
            break id;
        }
        std::thread::yield_now();
    };
    requests.serve(
        id,
        Ok(ServedStill {
            still: still(),
            position: Some(Duration::from_millis(12_500)),
        }),
    );

    let served = asking
        .join()
        .expect("the waiting thread did not panic")
        .expect("the request was served");
    assert_eq!(served.position, Some(Duration::from_millis(12_500)));
    assert_eq!(served.still.size().width(), WIDTH);
    assert_eq!(served.still.row(0), still().row(0));
}

#[test]
fn a_request_the_recording_could_not_serve_says_why() {
    // The path `crate::recording::Screenshots::abandon` takes when a game exits
    // between the key press and the next frame. The waiter must be told, not
    // left to time out.
    let requests = ScreenshotRequests::new();
    let waiter = requests.clone();
    let asking = std::thread::spawn(move || waiter.take_within(Duration::from_secs(5)));

    let id = loop {
        if let Some(id) = requests.claim() {
            break id;
        }
        std::thread::yield_now();
    };
    requests.serve(id, Err("the recording ended".to_owned()));

    let error = asking
        .join()
        .expect("the waiting thread did not panic")
        .expect_err("the recording refused");
    assert!(
        matches!(error, ScreenshotError::NotCaptured { .. }),
        "unexpected error: {error}"
    );
    assert!(error.to_string().contains("the recording ended"), "{error}");
}

#[test]
fn several_requests_are_each_answered_once() {
    // Two people — a hotkey and the tray — can ask at the same moment. Serving
    // one answer to both would give one of them a frame the other is holding,
    // and dropping one would leave a thread waiting for its whole timeout.
    let requests = ScreenshotRequests::new();

    let threads: Vec<_> = (0..3)
        .map(|_| {
            let waiter = requests.clone();
            std::thread::spawn(move || waiter.take_within(Duration::from_secs(5)))
        })
        .collect();

    let mut served = 0;
    while served < 3 {
        if let Some(id) = requests.claim() {
            requests.serve(
                id,
                Ok(ServedStill {
                    still: still(),
                    position: None,
                }),
            );
            served += 1;
        } else {
            std::thread::yield_now();
        }
    }

    for thread in threads {
        let outcome = thread.join().expect("no waiting thread panicked");
        assert!(outcome.is_ok(), "a waiter was not answered: {outcome:?}");
    }
}

#[test]
fn a_withdrawn_request_is_not_left_for_a_capture_loop_to_serve() {
    // A waiter that gave up must not leave a request behind: the next frame
    // would spend a texture copy on a screenshot nobody will ever collect.
    let requests = ScreenshotRequests::new();
    let error = requests
        .take_within(Duration::from_millis(20))
        .expect_err("nothing served it");
    assert!(matches!(error, ScreenshotError::NoFrame { .. }));

    assert!(
        requests.claim().is_none(),
        "a request whose waiter gave up is still queued"
    );
}

/// The codec and dimensions `ffprobe` reports for `path`, if there is one.
fn probe(path: &Path) -> Option<(String, u32, u32)> {
    let output = std::process::Command::new(ffprobe()?)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name,width,height",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let mut fields = text.trim().split(',');
    let codec = fields.next()?.to_owned();
    let width = fields.next()?.parse().ok()?;
    let height = fields.next()?.parse().ok()?;
    Some((codec, width, height))
}

/// `path` decoded to tightly packed 24-bit RGB, if `ffmpeg` is in the checkout.
fn decode_rgb(path: &Path) -> Option<Vec<u8>> {
    let output = std::process::Command::new(ffmpeg()?)
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

/// The pinned build's `ffprobe`, if this checkout has one.
fn ffprobe() -> Option<PathBuf> {
    tool("ffprobe.exe")
}

/// The pinned build's `ffmpeg`, if this checkout has one.
fn ffmpeg() -> Option<PathBuf> {
    tool("ffmpeg.exe")
}

/// A tool from the FFmpeg build this crate was linked against.
///
/// `FFMPEG_DIR` is what `scripts/fetch-ffmpeg.ps1` sets and what the build
/// scripts already read, so this asks the same question rather than inventing a
/// second way to find FFmpeg. A checkout without one skips the probe instead of
/// failing: the encoder is linked into this test binary and does not need the
/// executables, and a contributor without the fetch script run should still be
/// able to run the rest of these tests.
fn tool(name: &str) -> Option<PathBuf> {
    let directory = std::env::var_os("FFMPEG_DIR")?;
    let path = PathBuf::from(directory).join("bin").join(name);
    path.is_file().then_some(path)
}
