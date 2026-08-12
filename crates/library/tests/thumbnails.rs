//! Thumbnails, against recordings whose right answer is known before the code
//! under test runs.
//!
//! AGENTS.md section 22 asks that generated media be validated rather than
//! assumed valid, and the same argument applies in reverse to something that
//! reads media: a picture that "looks plausible" proves nothing. So the subject
//! of the first test is a recording that is **deliberately black for its first
//! half** and a moving pattern for its second, which makes "did it avoid the
//! loading screen?" an assertion rather than an impression. Every JPEG this
//! produces is then handed to `ffprobe` rather than eyeballed.
//!
//! These tests open no window, no capture device and no audio device. They write
//! files with the pinned FFmpeg build and read them back, and they skip cleanly
//! on a checkout that has no `ffmpeg.exe` (`CLIPPED_REQUIRE_MEDIA` turns that
//! skip into a failure, which is how CI is configured).

use core::time::Duration;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::Instant;

use clipped_library::thumbnail::{
    render, RequestOutcome, ServiceOptions, ThumbnailCache, ThumbnailOptions, ThumbnailService,
    ThumbnailState, DEFAULT_WIDTH,
};
use clipped_media_validation::{Media, TemporaryDirectory};

/// How long a test waits for the worker before deciding it is not coming.
///
/// Generous: this machine may be running several test binaries at once, and the
/// worker is deliberately the lowest-priority thread on it.
const PATIENCE: Duration = Duration::from_secs(60);

/// The two halves of the subject recording, in seconds.
///
/// Black first, because that is what a game's first seconds look like — a fade,
/// a loading screen, an anti-cheat splash — and a thumbnail taken from there is
/// the failure this whole module exists to avoid.
const BLACK_SECONDS: u32 = 6;
const PATTERN_SECONDS: u32 = 6;

/// Writes a recording that is black for [`BLACK_SECONDS`] and then a moving
/// pattern for [`PATTERN_SECONDS`].
///
/// Returns `false` when this checkout has no `ffmpeg.exe`, having already
/// reported the skip; the caller returns without asserting.
fn write_recording_with_a_black_opening(destination: &Path) -> bool {
    let Some(tools) = clipped_media_validation::require_media_tools() else {
        return false;
    };

    let output = Command::new(tools.ffmpeg())
        .args(["-nostdin", "-y"])
        .args(["-f", "lavfi", "-i"])
        .arg(format!("color=c=black:s=1280x720:r=30:d={BLACK_SECONDS}"))
        .args(["-f", "lavfi", "-i"])
        .arg(format!(
            "testsrc=size=1280x720:rate=30:duration={PATTERN_SECONDS}"
        ))
        .args([
            "-filter_complex",
            "[0:v][1:v]concat=n=2:v=1:a=0,format=yuv420p[v]",
        ])
        .args(["-map", "[v]"])
        // A keyframe a second, so that seeking to a candidate lands close to it
        // rather than at the start of a ten-second group of pictures.
        .args(["-c:v", "libopenh264", "-g", "30", "-b:v", "4000k"])
        .arg(destination)
        .output()
        .expect("ffmpeg can be started");

    assert!(
        output.status.success(),
        "ffmpeg failed to write the subject recording: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

/// Writes a recording shaped like a real one: 1080p, compressible-resistant
/// content, and a bitrate that puts real bytes on the disk.
///
/// The cost of making a thumbnail is set by how far into a file the seek lands
/// and how large its pictures are, not by how long the recording is, so the
/// measurement has to be taken over something the size a recording is.
fn write_recording_shaped_container(destination: &Path, seconds: u32, kilobits: u32) -> bool {
    let Some(tools) = clipped_media_validation::require_media_tools() else {
        return false;
    };

    let output = Command::new(tools.ffmpeg())
        .args(["-nostdin", "-y"])
        .args(["-f", "lavfi", "-i"])
        .arg(format!("testsrc=size=1920x1080:rate=30:duration={seconds}"))
        // `testsrc` is a synthetic pattern and compresses to almost nothing, so
        // an encode of it produces a file far smaller than a recording. Noise
        // defeats that and puts the bitrate where it was asked for.
        .args(["-vf", "noise=alls=48:allf=t+u,format=yuv420p"])
        .args(["-c:v", "libopenh264", "-g", "30", "-b:v"])
        .arg(format!("{kilobits}k"))
        .arg(destination)
        .output()
        .expect("ffmpeg can be started");

    assert!(
        output.status.success(),
        "ffmpeg failed to write a recording-shaped container: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

/// Writes a file with audio and no video at all.
fn write_audio_only(destination: &Path) -> bool {
    let Some(tools) = clipped_media_validation::require_media_tools() else {
        return false;
    };

    let output = Command::new(tools.ffmpeg())
        .args(["-nostdin", "-y"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=2"])
        .args(["-c:a", "aac"])
        .arg(destination)
        .output()
        .expect("ffmpeg can be started");

    assert!(
        output.status.success(),
        "ffmpeg failed to write an audio-only file: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

/// Asserts that `path` really is a JPEG of the given size, through `ffprobe`
/// rather than through this crate's own opinion of what it wrote.
fn assert_is_a_jpeg(path: &Path, width: u32, height: u32) {
    let bytes = std::fs::read(path).expect("the picture is on disk");
    assert!(
        bytes.starts_with(&[0xFF, 0xD8, 0xFF]) && bytes.ends_with(&[0xFF, 0xD9]),
        "the picture does not begin with a JPEG marker and end with one ({} bytes)",
        bytes.len()
    );

    let media = Media::open(path).expect("ffprobe can read the picture");
    let streams = media.video_streams();
    assert_eq!(streams.len(), 1, "{}", media.inventory());
    assert_eq!(streams[0].field("codec_name"), Some("mjpeg"));
    assert_eq!(streams[0].number("width"), Some(f64::from(width)));
    assert_eq!(streams[0].number("height"), Some(f64::from(height)));
}

/// Waits for `condition`, or gives up after [`PATIENCE`].
fn wait_for(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("waited {PATIENCE:?} for {what}");
}

#[test]
fn a_thumbnail_comes_from_the_game_rather_than_the_loading_screen() {
    let directory = TemporaryDirectory::new("thumbnail-choice");
    let recording = directory.file("match.mkv");
    if !write_recording_with_a_black_opening(&recording) {
        return;
    }

    let thumbnail = render(&recording, ThumbnailOptions::new()).expect("a frame can be chosen");

    // The assertion this whole module exists for. The first six seconds of this
    // recording are black; a thumbnail taken from them would be a black tile,
    // and taking the first frame — or any fixed early offset — would produce
    // exactly that.
    assert!(
        thumbnail.at() >= Duration::from_secs(u64::from(BLACK_SECONDS)),
        "the frame was taken at {:?}, which is inside the black opening",
        thumbnail.at()
    );
    assert!(
        !thumbnail.is_blank(),
        "the chosen frame scored {}, which is a flat colour",
        thumbnail.score()
    );

    // And it is a real picture of the right shape: 1280x720 scaled to the
    // default width, aspect ratio kept.
    assert_eq!(thumbnail.width(), DEFAULT_WIDTH);
    assert_eq!(thumbnail.height(), 360);

    let written = directory.file("thumbnail.jpg");
    std::fs::write(&written, thumbnail.jpeg()).expect("the picture can be written");
    assert_is_a_jpeg(&written, thumbnail.width(), thumbnail.height());
}

#[test]
fn a_requested_width_is_what_comes_out_and_the_recording_is_never_enlarged() {
    let directory = TemporaryDirectory::new("thumbnail-size");
    let recording = directory.file("match.mkv");
    if !write_recording_with_a_black_opening(&recording) {
        return;
    }

    let small =
        render(&recording, ThumbnailOptions::new().with_width(320)).expect("a frame can be chosen");
    assert_eq!((small.width(), small.height()), (320, 180));

    // The source is 1280 wide, so asking for more than that gets the source's
    // own size rather than an enlargement: a soft picture and a larger file.
    let large = render(&recording, ThumbnailOptions::new().with_width(1_920))
        .expect("a frame can be chosen");
    assert_eq!((large.width(), large.height()), (1_280, 720));

    let written = directory.file("small.jpg");
    std::fs::write(&written, small.jpeg()).expect("the picture can be written");
    assert_is_a_jpeg(&written, 320, 180);
}

#[test]
fn a_recording_with_no_picture_in_it_is_reported_rather_than_guessed_at() {
    let directory = TemporaryDirectory::new("thumbnail-audio-only");
    let recording = directory.file("audio.mka");
    if !write_audio_only(&recording) {
        return;
    }

    let error = render(&recording, ThumbnailOptions::new())
        .expect_err("a file with no video stream has no thumbnail");
    assert!(
        error.to_string().contains("no video stream"),
        "the reason given was {error}"
    );
}

#[test]
fn a_recording_that_cannot_be_decoded_leaves_the_recording_usable() {
    // Issue #57's third acceptance criterion. A file that exists, can be
    // stat-ed, and holds no container FFmpeg can open — a recording truncated by
    // a crash, which AGENTS.md section 16 says to expect.
    let directory = TemporaryDirectory::new("thumbnail-broken");
    let recording = directory.file("truncated.mkv");
    let contents: &[u8] = b"this was a recording once";
    std::fs::write(&recording, contents).expect("the file can be written");

    let service = ThumbnailService::start(
        ThumbnailCache::at(directory.file("cache")),
        ServiceOptions::new(),
    );

    assert!(matches!(
        service.thumbnail(&recording),
        ThumbnailState::Pending
    ));
    wait_for("the first attempt to finish", || service.finished() == 1);

    // Suspended, so the worker takes nothing off the queue from here on:
    // anything these lookups ask for is still sitting in it to be counted.
    service.suspend_for_recording();

    // The call a library screen makes for every tile it draws. Without the
    // failure written down, each of these misses the cache, is told `Pending`,
    // and queues another decode of the same broken file — for a real recording,
    // of several gigabytes, which is precisely the background disk load the
    // priority and suspension design exists to avoid.
    for _ in 0..5 {
        let state = service.thumbnail(&recording);
        assert!(!state.is_ready());
        assert!(state.image_path().is_none());
        assert!(
            state.reason().is_some(),
            "the screen was not told why there is no thumbnail"
        );
    }
    assert_eq!(
        service.queued(),
        0,
        "an undecodable recording was queued for another attempt on every lookup"
    );
    assert_eq!(service.finished(), 1);

    // And the recording itself is untouched. Nothing in this crate writes to a
    // recording, and a failed thumbnail must not have been the exception
    // (AGENTS.md section 56).
    assert_eq!(
        std::fs::read(&recording).expect("the recording is still there"),
        contents
    );
}

#[test]
fn a_recording_is_thumbnailed_once_and_then_read_from_the_cache() {
    let directory = TemporaryDirectory::new("thumbnail-cache");
    let recording = directory.file("match.mkv");
    if !write_recording_with_a_black_opening(&recording) {
        return;
    }

    let service = ThumbnailService::start(
        ThumbnailCache::at(directory.file("cache")),
        ServiceOptions::new(),
    );

    // The call a library screen makes. The first answer is "not yet", and it is
    // an answer rather than an error.
    let first = service.thumbnail(&recording);
    assert!(matches!(first, ThumbnailState::Pending));
    assert!(first.image_path().is_none());

    wait_for("the worker to finish", || service.finished() == 1);

    let state = service.thumbnail(&recording);
    let thumbnail = state.thumbnail().expect("the picture is ready now");
    assert_is_a_jpeg(
        thumbnail.image_path(),
        thumbnail.width(),
        thumbnail.height(),
    );
    assert!(thumbnail.at() >= Duration::from_secs(u64::from(BLACK_SECONDS)));

    // Asking again does not queue it again, and does not decode again.
    assert_eq!(service.queued(), 0);
    assert_eq!(service.finished(), 1);

    // A second service over the same directory finds it without a worker having
    // run at all, which is what makes this a cache rather than a memo.
    let other = ThumbnailCache::at(directory.file("cache"));
    assert!(other.lookup(&recording).is_ready());
}

#[test]
fn a_thumbnail_for_a_recording_that_is_gone_is_cleaned_up_with_it() {
    let directory = TemporaryDirectory::new("thumbnail-cleanup");
    let recording = directory.file("match.mkv");
    if !write_recording_with_a_black_opening(&recording) {
        return;
    }

    let cache = ThumbnailCache::at(directory.file("cache"));
    let thumbnail = cache
        .store(&render(&recording, ThumbnailOptions::new()).expect("a frame can be chosen"))
        .expect("the picture can be stored");
    let picture = thumbnail.image_path().to_path_buf();
    assert!(picture.is_file());

    std::fs::remove_file(&recording).expect("the recording can be deleted");
    let report = cache.prune();

    assert_eq!(report.orphans_removed, 1, "{report:?}");
    assert!(
        !picture.exists(),
        "the picture outlived the recording it was made from"
    );
    assert_eq!(report.remaining_bytes, 0, "{report:?}");
}

#[test]
fn the_worker_runs_at_the_lowest_priority_windows_will_give_it() {
    let directory = TemporaryDirectory::new("thumbnail-priority");
    let service = ThumbnailService::start(
        ThumbnailCache::at(directory.file("cache")),
        ServiceOptions::new(),
    );

    wait_for("the worker to report its priority", || {
        service.worker_priority().is_some()
    });
    let priority = service.worker_priority().expect("just waited for it");

    #[cfg(windows)]
    {
        // Read back from `GetThreadPriority`, not from the fact that
        // `SetThreadPriority` was called: the claim being made is about the
        // thread, not about this crate's intentions.
        assert!(
            priority.is_lowest(),
            "the worker is at priority {}, not THREAD_PRIORITY_LOWEST",
            priority.observed()
        );
        // Background mode is what lowers *disk* priority, which is the half that
        // matters while a recording is being written to the same disk.
        assert!(
            priority.background_mode(),
            "the worker did not enter background I/O mode"
        );
        assert!(
            priority.observed() <= -2,
            "the worker ended up at priority {}, above THREAD_PRIORITY_LOWEST",
            priority.observed()
        );
    }
    #[cfg(not(windows))]
    {
        // Nothing was lowered, and the report says so rather than claiming it
        // was.
        assert!(!priority.is_lowest());
        assert!(!priority.background_mode());
    }
}

#[test]
fn suspending_generation_stops_the_worker_and_resuming_lets_it_finish() {
    let directory = TemporaryDirectory::new("thumbnail-suspend");
    let recording = directory.file("match.mkv");
    if !write_recording_with_a_black_opening(&recording) {
        return;
    }
    let (finished, finishes) = mpsc::channel();

    let service = ThumbnailService::start(
        ThumbnailCache::at(directory.file("cache")),
        ServiceOptions::new().on_finished(move |completion| {
            let _ = finished.send(completion.recording);
        }),
    );

    // Suspended before anything is asked for, which is the state a host puts it
    // in when a recording starts. This is the "deferred until the session ends"
    // half of issue #57's second acceptance criterion.
    service.suspend_for_recording();
    assert!(service.is_suspended());
    assert_eq!(service.request(&recording), RequestOutcome::Queued);

    // Nothing may come out while it is suspended. This is the assertion that
    // fails if suspension is a flag nobody reads.
    assert_eq!(
        finishes.recv_timeout(Duration::from_millis(750)),
        Err(mpsc::RecvTimeoutError::Timeout),
        "a thumbnail was generated while generation was suspended"
    );
    assert_eq!(service.finished(), 0);

    service.resume();
    assert!(!service.is_suspended());
    let done = finishes
        .recv_timeout(PATIENCE)
        .expect("the thumbnail is generated once generation resumes");
    assert_eq!(done, recording);
    assert!(service.cache().lookup(&recording).is_ready());
}

#[test]
fn the_queue_is_bounded_and_drops_the_oldest_waiting_request() {
    let directory = TemporaryDirectory::new("thumbnail-queue");
    let service = ThumbnailService::start(
        ThumbnailCache::at(directory.file("cache")),
        ServiceOptions::new().with_queue_capacity(2),
    );
    // Suspended so the worker takes nothing off the queue while this runs.
    service.suspend_for_recording();

    let first = directory.file("first.mkv");
    let second = directory.file("second.mkv");
    let third = directory.file("third.mkv");

    assert_eq!(service.request(&first), RequestOutcome::Queued);
    assert_eq!(service.request(&second), RequestOutcome::Queued);
    // The same recording twice is one request, not two.
    assert_eq!(service.request(&second), RequestOutcome::AlreadyQueued);
    assert_eq!(service.queued(), 2);

    // Full. The oldest goes, and the caller is told which.
    assert_eq!(
        service.request(&third),
        RequestOutcome::QueuedInPlaceOf(first.clone())
    );
    assert_eq!(service.queued(), 2);
    // Asking again for the one that was dropped costs the one that is now
    // oldest. The queue is a fixed size whatever the caller does.
    assert_eq!(
        service.request(&first),
        RequestOutcome::QueuedInPlaceOf(second.clone())
    );
    assert_eq!(service.queued(), 2);
}

#[test]
fn shutting_down_stops_the_worker_rather_than_waiting_for_the_queue() {
    let directory = TemporaryDirectory::new("thumbnail-shutdown");
    let service = ThumbnailService::start(
        ThumbnailCache::at(directory.file("cache")),
        ServiceOptions::new(),
    );
    service.suspend_for_recording();
    service.request(directory.file("waiting.mkv"));

    // Suspended with work outstanding: shutdown has to break the wait, not
    // deadlock behind it. The test hanging is the failure.
    service.shutdown();
}

/// How long the recording the cost is measured over is.
const COST_SECONDS: u32 = 12;

/// Its bitrate, which is what puts real bytes between the start of the file and
/// the frame a seek is looking for.
const COST_KILOBITS: u32 = 20_000;

#[test]
fn a_thumbnail_costs_a_small_fraction_of_what_playing_the_recording_would() {
    // AGENTS.md section 19 asks for measurements rather than adjectives, and
    // issue #57 asks specifically for the cost per thumbnail on stated hardware
    // rather than an assertion that it is cheap. This takes the measurement
    // every run and prints it; `docs/thumbnails.md` records what it said on the
    // development machine, with the hardware and the workload named.
    //
    // The assertion is deliberately loose. A number this could fail on would be
    // a number that depends on what else the machine is doing, and a benchmark
    // that fails because a second test binary was running is a benchmark that
    // gets deleted. What is asserted is the property the design rests on: a
    // thumbnail is far cheaper than reading the recording, so a library scan
    // converges rather than running for ever. Run with `--nocapture` to see the
    // figures.
    let directory = TemporaryDirectory::new("thumbnail-cost");
    let recording = directory.file("recording.mkv");
    if !write_recording_shaped_container(&recording, COST_SECONDS, COST_KILOBITS) {
        return;
    }

    let bytes = std::fs::metadata(&recording)
        .expect("the file exists")
        .len();
    let started = Instant::now();
    let thumbnail = render(&recording, ThumbnailOptions::new()).expect("a frame can be chosen");
    let elapsed = started.elapsed();

    let megabytes = bytes as f64 / (1024.0 * 1024.0);
    println!(
        "thumbnail cost: {:.0} ms for a {COST_SECONDS} s 1920x1080 30 fps H.264 recording at \
         {COST_KILOBITS} kb/s ({megabytes:.1} MB), producing a {}x{} JPEG of {} bytes from the \
         frame at {:.1} s [{} build]",
        elapsed.as_secs_f64() * 1_000.0,
        thumbnail.width(),
        thumbnail.height(),
        thumbnail.jpeg().len(),
        thumbnail.at().as_secs_f64(),
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
    );

    // What a second stored size would cost, which is the number behind
    // `docs/thumbnails.md`'s decision that one is enough. Reported rather than
    // asserted: it is a size on disk, and disks differ in nothing that matters
    // here.
    for width in [320, 640, 1_280] {
        let sized = render(&recording, ThumbnailOptions::new().with_width(width))
            .expect("a frame can be chosen");
        println!(
            "thumbnail size: {}x{} is {} bytes at quality {}",
            sized.width(),
            sized.height(),
            sized.jpeg().len(),
            ThumbnailOptions::new().quality(),
        );
    }

    // The other end of the range: a recording whose first two candidates are
    // blank, so every seek and every frame the bound allows is spent. This is
    // what a game with a long loading screen costs.
    let awkward = directory.file("black-opening.mkv");
    if write_recording_with_a_black_opening(&awkward) {
        let started = Instant::now();
        let searched = render(&awkward, ThumbnailOptions::new()).expect("a frame can be chosen");
        println!(
            "thumbnail cost, every early candidate blank: {:.0} ms for a {} s 1280x720 recording, \
             taking the frame at {:.1} s",
            started.elapsed().as_secs_f64() * 1_000.0,
            BLACK_SECONDS + PATTERN_SECONDS,
            searched.at().as_secs_f64(),
        );
    }

    assert!(
        elapsed < Duration::from_secs(u64::from(COST_SECONDS)),
        "making a thumbnail of a {COST_SECONDS} s recording took {elapsed:?}, which is longer \
         than playing it; a library scan at that rate would never finish"
    );
}

#[test]
fn a_library_of_recordings_is_thumbnailed_without_reading_any_of_them_twice() {
    // "Thumbnails appear for existing and new recordings" — issue #57's first
    // acceptance criterion — is a statement about a *library*, not about one
    // file: a screen listing recordings that were made before this feature
    // existed has to fill in, and one recorded afterwards has to appear beside
    // them without anything else being redone.
    let directory = TemporaryDirectory::new("thumbnail-library");
    let existing: Vec<PathBuf> = (0..3)
        .map(|index| directory.file(&format!("existing-{index}.mkv")))
        .collect();
    for recording in &existing {
        if !write_recording_with_a_black_opening(recording) {
            return;
        }
    }

    let service = ThumbnailService::start(
        ThumbnailCache::at(directory.file("cache")),
        ServiceOptions::new(),
    );
    for recording in &existing {
        service.request(recording);
    }
    wait_for("the existing library to be thumbnailed", || {
        service.finished() == existing.len() as u64
    });

    for recording in &existing {
        let state = service.thumbnail(recording);
        let thumbnail = state.thumbnail().expect("every recording has a picture");
        assert_is_a_jpeg(
            thumbnail.image_path(),
            thumbnail.width(),
            thumbnail.height(),
        );
    }

    // A recording made after the scan. It is the only one looked at again.
    let new = directory.file("new.mkv");
    if !write_recording_with_a_black_opening(&new) {
        return;
    }
    assert!(matches!(service.thumbnail(&new), ThumbnailState::Pending));
    wait_for("the new recording to be thumbnailed", || {
        service.finished() == existing.len() as u64 + 1
    });
    assert!(service.thumbnail(&new).is_ready());

    assert_eq!(
        service.finished(),
        existing.len() as u64 + 1,
        "a recording was decoded more than once"
    );
}
