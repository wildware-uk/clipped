//! Watches a process tree's own audio track being captured, beside everything
//! else the machine is playing.
//!
//! Process-scoped capture is the feature Clipped exists for
//! ([ADR 0003](../../../docs/adr/0003-process-specific-audio-capture.md)) and
//! it is also the one whose behaviour is hardest to see: a track that contains
//! the wrong process's audio, or no audio at all, looks exactly like a track
//! that is correct until somebody opens the file. This probe is how a machine
//! is asked whether it can do this at all, and what it did.
//!
//! ```text
//! cargo run -p clipped-audio --example process_loopback_probe -- 30
//! cargo run -p clipped-audio --example process_loopback_probe -- 30 12345
//! ```
//!
//! The first argument is how long to run for in seconds. The second is the
//! process to scope the capture to; **with no second argument the probe scopes
//! the capture to itself**, which plays nothing, so a default run makes no
//! sound at all and still answers the questions that need answering on a shared
//! machine: whether `ActivateAudioInterfaceAsync` gives this build a
//! process-scoped client, what shape the audio engine accepted, whether packets
//! arrive, and whether the positions on them are performance-counter readings.
//!
//! # Both sides at once
//!
//! It opens the **pair** — everything the tree plays, and everything the
//! machine plays except the tree — because that is what a recording opens
//! ([issue #27](https://github.com/wildware-uk/clipped/issues/27)) and because
//! one side alone cannot show the thing worth seeing. The `peak` columns are
//! the answer: point it at a game, play a tone in a browser, and
//!
//! - `game` should rise only with the game and `other` only with the browser;
//! - a `peak` in **both** columns at once is the game's audio on two tracks,
//!   which is the defect the excluding side exists to prevent;
//! - a `peak` in **neither** while something is plainly playing is audio that
//!   reached no track at all.
//!
//! Read that against `scoped to`, which is the process both sides are scoped
//! through: they must always show the same number. This is a probe rather than
//! a test: the same claim, automated on real hardware and measured per frequency
//! rather than as a peak, is `tests/audio/track_isolation.rs`
//! ([issue #34](https://github.com/wildware-uk/clipped/issues/34)). What the
//! probe still answers that the test does not is what a *real* game and a *real*
//! browser do, which is worth watching for a few seconds before believing a
//! machine.
//!
//! Nothing it captures is written anywhere.

#![cfg(windows)]

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use clipped_audio::windows::ProcessLoopbackCapture;
use clipped_audio::{Capture, SampleOrigin};
use clipped_logging::LogSettings;

/// What one side of the pair has done, published for the reporting thread.
///
/// Atomics rather than a lock because the writers are capture threads and a
/// capture thread waits on nothing (AGENTS.md section 20).
#[derive(Debug, Default)]
struct Side {
    /// The loudest sample since the last report, as `f32` bits.
    ///
    /// Every value stored is non-negative, and the bit patterns of
    /// non-negative `f32`s compare in the same order as the floats, so
    /// `fetch_max` on the bits is a maximum of the samples.
    peak_bits: AtomicU32,
    frames: AtomicU64,
    silence: AtomicU64,
    reopened: AtomicU64,
    discontinuities: AtomicU64,
    /// Runs of audio this tap lost when its stream set changed, and the frames
    /// they held (issue #626). The one figure this probe exists to watch go up
    /// while nothing else in the recorder reacts at all.
    dropouts: AtomicU64,
    dropout_frames: AtomicU64,
    scoped_to: AtomicU32,
    running: AtomicBool,
    /// Frames handed over by the drain after the capture was told to finish.
    drained: AtomicU64,
}

impl Side {
    fn observe_peak(&self, peak: f32) {
        self.peak_bits.fetch_max(peak.to_bits(), Ordering::Relaxed);
    }

    /// The loudest sample since this was last called, and resets it.
    fn take_peak(&self) -> f32 {
        f32::from_bits(self.peak_bits.swap(0, Ordering::Relaxed))
    }

    /// One column of the report line.
    fn column(&self, rate: f64) -> String {
        let frames = self.frames.load(Ordering::Relaxed);
        format!(
            "{:>9} {:>6.2}s  silence {:>9}  reopened {:>3}  disc {:>3}  lost {:>3} ({:>6.2}s)  \
             peak {:.4}",
            frames,
            frames as f64 / rate,
            self.silence.load(Ordering::Relaxed),
            self.reopened.load(Ordering::Relaxed),
            self.discontinuities.load(Ordering::Relaxed),
            self.dropouts.load(Ordering::Relaxed),
            self.dropout_frames.load(Ordering::Relaxed) as f64 / rate,
            self.take_peak(),
        )
    }
}

/// Reads one capture until `stop`, then drains it the way a recording does.
fn pump(mut capture: ProcessLoopbackCapture, side: &Side, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        match capture.read(Duration::from_millis(200)) {
            Ok(Capture::Samples(samples)) => {
                if samples.origin() == SampleOrigin::Endpoint {
                    let peak = samples
                        .samples()
                        .iter()
                        .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
                    side.observe_peak(peak);
                }
            }
            Ok(Capture::Idle) => {}
            Ok(Capture::FormatChanged(format)) => {
                println!("the capture now presents {format}, which it cannot follow");
            }
            Err(error) => {
                println!("this capture stopped: {error}");
                break;
            }
        }

        let stats = capture.stats();
        side.frames.store(stats.frames, Ordering::Relaxed);
        side.silence
            .store(stats.synthesised_silence_frames, Ordering::Relaxed);
        side.reopened
            .store(stats.endpoint_changes, Ordering::Relaxed);
        side.discontinuities
            .store(stats.discontinuities, Ordering::Relaxed);
        side.dropouts
            .store(stats.unflagged_dropouts, Ordering::Relaxed);
        side.dropout_frames
            .store(stats.unflagged_dropout_frames, Ordering::Relaxed);
        side.scoped_to.store(capture.scoped_to(), Ordering::Relaxed);
        side.running
            .store(capture.target_is_running(), Ordering::Relaxed);
    }

    // The end of a recording, done the way a recording ends it: stop the
    // stream, hand over what the audio engine still held, and only then let go.
    let before_drain = capture.stats().frames;
    capture.finish();
    while let Ok(Capture::Samples(samples)) = capture.read(Duration::from_millis(100)) {
        let _ = samples;
    }
    side.drained
        .store(capture.stats().frames - before_drain, Ordering::Relaxed);
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let seconds: u64 = arguments
        .next()
        .map_or(Ok(30), |argument| argument.parse())
        .map_err(|error| format!("the first argument is how many seconds to run for: {error}"))?;
    let target: u32 = arguments
        .next()
        .map_or_else(|| Ok(std::process::id()), |argument| argument.parse())
        .map_err(|error| {
            format!("the second argument is the process to capture the audio of: {error}")
        })?;

    let _logging = clipped_logging::init(
        &LogSettings::default()
            .without_level_file()
            .with_default_level("debug"),
    )?;

    let (game, other) = ProcessLoopbackCapture::open_pair(target)?;
    let format = game.format();
    println!("Recording process {target} and everything it started: {format}");
    println!(
        "…and everything the machine plays except it: {}",
        other.format()
    );
    if target == std::process::id() {
        println!("That is this probe, which plays nothing: expect silence in the game column.");
    }
    println!();

    let rate = f64::from(format.sample_rate().get());
    let stop = Arc::new(AtomicBool::new(false));
    let game_side = Arc::new(Side::default());
    let other_side = Arc::new(Side::default());

    // One thread each, as a recording gives them, so that a stall on one side
    // is visible as a stall on that side rather than as a stall on both.
    let workers = [
        (game, Arc::clone(&game_side)),
        (other, Arc::clone(&other_side)),
    ]
    .map(|(capture, side)| {
        let stopping = Arc::clone(&stop);
        thread::spawn(move || pump(capture, &side, &stopping))
    });

    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(seconds) {
        thread::sleep(Duration::from_secs(1));
        println!("game   {}", game_side.column(rate));
        println!(
            "other  {}  scoped to {} / {}  {}",
            other_side.column(rate),
            game_side.scoped_to.load(Ordering::Relaxed),
            other_side.scoped_to.load(Ordering::Relaxed),
            if game_side.running.load(Ordering::Relaxed) {
                "running"
            } else {
                "the game has exited"
            }
        );
    }

    stop.store(true, Ordering::Relaxed);
    for worker in workers {
        // A panicked capture thread is worth reporting rather than hiding, but
        // it is not a reason to leave the other one unjoined.
        if worker.join().is_err() {
            println!("one of the capture threads panicked");
        }
    }

    println!(
        "\ndrained {} and {} frames ({:.1} ms and {:.1} ms) the audio engine still held when \
         the captures stopped",
        game_side.drained.load(Ordering::Relaxed),
        other_side.drained.load(Ordering::Relaxed),
        game_side.drained.load(Ordering::Relaxed) as f64 / rate * 1_000.0,
        other_side.drained.load(Ordering::Relaxed) as f64 / rate * 1_000.0,
    );

    Ok(())
}
