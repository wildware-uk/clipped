//! What keeps waveform generation out of a game's way, checked rather than
//! described.
//!
//! Issue #66 asks that generation "never runs at a priority that affects an
//! active game (measured or deferred)". Two of the three mechanisms can be
//! measured here, on any machine, with no game and no foreground:
//!
//! - the worker thread's priority, read back from Windows rather than assumed;
//! - suspension, which has to be a real stop rather than a flag nothing reads.
//!
//! The third — the effect on a game's frame times — needs a game, a GPU and a
//! machine to itself, so it is a manual measurement recorded against the issue
//! rather than a test here (`docs/waveforms.md`, "Measurements").

mod support;

use core::time::Duration;
use std::sync::mpsc;

use clipped_media_validation::TemporaryDirectory;
use clipped_waveform::{
    RequestOutcome, ServiceOptions, WaveformCache, WaveformService, WaveformState,
};

use support::{write_wav, Tone};

/// Long enough that analysing it is not instantaneous, short enough that the
/// suite stays quick.
const SECONDS: f64 = 4.0;

/// How long a test waits for the worker before deciding it is not coming.
///
/// Generous: this machine may be running several test binaries at once, and the
/// worker is deliberately the lowest-priority thread on it.
const PATIENCE: Duration = Duration::from_secs(30);

fn recording(directory: &TemporaryDirectory, name: &str) -> std::path::PathBuf {
    let path = directory.file(name);
    write_wav(
        &path,
        48_000,
        &[vec![
            Tone::at(SECONDS / 2.0, 0.8),
            Tone::silence(SECONDS / 2.0),
        ]],
    );
    path
}

/// Waits for `condition`, or gives up after [`PATIENCE`].
fn wait_for(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + PATIENCE;
    while std::time::Instant::now() < deadline {
        if condition() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("waited {PATIENCE:?} for {what}");
}

#[test]
fn the_worker_runs_at_the_lowest_priority_windows_will_give_it() {
    let directory = TemporaryDirectory::new("waveform-service");
    let service = WaveformService::start(
        WaveformCache::at(directory.file("cache")),
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
        // Background mode is what lowers *disk* priority, which is the half
        // that matters while a recording is being written to the same disk.
        assert!(
            priority.background_mode(),
            "the worker did not enter background I/O mode"
        );
        // And whatever Windows calls the resulting priority, it is never above
        // the lowest ordinary one. On this build it reports -4, below the -2 of
        // THREAD_PRIORITY_LOWEST, because background mode is lower still.
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
    let directory = TemporaryDirectory::new("waveform-service");
    let path = recording(&directory, "match.wav");
    let (finished, finishes) = mpsc::channel();

    let service = WaveformService::start(
        WaveformCache::at(directory.file("cache")),
        ServiceOptions::new().on_finished(move |completion| {
            let _ = finished.send(completion.recording);
        }),
    );

    // Suspended before anything is asked for, which is the state a host puts it
    // in when a recording starts.
    service.suspend_for_recording();
    assert!(service.is_suspended());
    assert_eq!(service.request(&path), RequestOutcome::Queued);

    // Nothing may come out while it is suspended. This is the assertion that
    // fails if suspension is a flag nobody reads.
    assert_eq!(
        finishes.recv_timeout(Duration::from_millis(750)),
        Err(mpsc::RecvTimeoutError::Timeout),
        "a waveform was generated while generation was suspended"
    );
    assert_eq!(service.finished(), 0);

    service.resume();
    assert!(!service.is_suspended());
    let done = finishes
        .recv_timeout(PATIENCE)
        .expect("the waveform is generated once generation resumes");
    assert_eq!(done, path);
    assert!(service.cache().lookup(&path).is_ready());
}

#[test]
fn a_recording_is_generated_once_and_then_read_from_the_cache() {
    let directory = TemporaryDirectory::new("waveform-service");
    let path = recording(&directory, "match.wav");
    let service = WaveformService::start(
        WaveformCache::at(directory.file("cache")),
        ServiceOptions::new(),
    );

    // The call a timeline makes. The first answer is "not yet", and it is an
    // answer rather than an error.
    let first = service.waveform(&path);
    assert!(matches!(first, WaveformState::Pending));
    assert!(first.tracks().is_empty());

    wait_for("the worker to finish", || service.finished() == 1);

    let state = service.waveform(&path);
    let waveform = state.waveform().expect("the peaks are ready now");
    assert_eq!(waveform.tracks().len(), 1);
    let drift = waveform.duration().as_secs_f64() - SECONDS;
    assert!(drift.abs() < 0.05, "{:?}", waveform.duration());

    // Asking again does not queue it again.
    assert_eq!(service.queued(), 0);
    assert_eq!(service.finished(), 1);
}

#[test]
fn a_recording_that_cannot_be_decoded_is_analysed_once_rather_than_on_every_lookup() {
    let directory = TemporaryDirectory::new("waveform-service");
    // A recording truncated by a crash, or written by something that was
    // killed: a file that exists, can be stat-ed, and holds no container
    // FFmpeg can open. AGENTS.md section 16 says to expect exactly this.
    let path = directory.file("truncated.mkv");
    std::fs::write(&path, b"this was a recording once").expect("the file can be written");

    let service = WaveformService::start(
        WaveformCache::at(directory.file("cache")),
        ServiceOptions::new(),
    );

    assert!(matches!(service.waveform(&path), WaveformState::Pending));
    wait_for("the first analysis to finish", || service.finished() == 1);

    // Suspended, so the worker takes nothing off the queue from here on:
    // anything these lookups ask for is still sitting in it to be counted.
    service.suspend_for_recording();

    // The call a timeline makes on every redraw. Without the failure written
    // down, each of these misses the cache, is told `Pending`, and queues
    // another full read and re-demux of the file — for a real recording, of
    // several gigabytes, which is precisely the background disk load the
    // priority and suspension design exists to avoid (AGENTS.md section 18).
    for _ in 0..5 {
        let state = service.waveform(&path);
        assert!(!state.is_ready());
        assert!(state.tracks().is_empty());
        assert!(
            state.reason().is_some(),
            "the timeline was not told why there is no waveform"
        );
    }

    assert_eq!(
        service.queued(),
        0,
        "an undecodable recording was queued for analysis again on every lookup"
    );
    assert_eq!(service.finished(), 1);

    // And a repaired recording is analysed again: what is remembered belongs to
    // the version of the file that failed, not to the path.
    write_wav(
        &path,
        48_000,
        &[vec![Tone::at(0.2, 0.8), Tone::silence(0.2)]],
    );
    assert!(matches!(service.waveform(&path), WaveformState::Pending));
    assert_eq!(service.queued(), 1);
    service.resume();
    wait_for("the repaired recording to be analysed", || {
        service.finished() == 2
    });
    assert!(service.waveform(&path).is_ready());
}

#[test]
fn the_queue_is_bounded_and_drops_the_oldest_waiting_request() {
    let directory = TemporaryDirectory::new("waveform-service");
    let service = WaveformService::start(
        WaveformCache::at(directory.file("cache")),
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
    let directory = TemporaryDirectory::new("waveform-service");
    let path = recording(&directory, "match.wav");
    let service = WaveformService::start(
        WaveformCache::at(directory.file("cache")),
        ServiceOptions::new(),
    );
    service.suspend_for_recording();
    service.request(&path);

    // Suspended with work outstanding: shutdown has to break the wait, not
    // deadlock behind it. The test hanging is the failure.
    service.shutdown();
}
