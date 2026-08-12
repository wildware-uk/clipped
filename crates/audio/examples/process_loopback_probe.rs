//! Watches a process tree's own audio track being captured.
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
//! Point it at a game's process identifier to watch a real track: `frames` rises
//! at the sample rate whatever happens, `silence` is the part of that the game
//! was not playing, and `peak` is how loud what it *was* playing got. A run
//! whose `peak` stays at zero while a game is plainly making a noise is the
//! failure this probe exists to find.
//!
//! Nothing it captures is written anywhere.

#![cfg(windows)]

use std::time::{Duration, Instant};

use clipped_audio::windows::ProcessLoopbackCapture;
use clipped_audio::{Capture, SampleOrigin};
use clipped_logging::LogSettings;

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

    let mut capture = ProcessLoopbackCapture::open(target)?;
    let format = capture.format();
    println!("Recording process {target} and everything it started: {format}");
    if target == std::process::id() {
        println!("That is this probe, which plays nothing: expect silence, and no sound.");
    }
    println!();

    let rate = f64::from(format.sample_rate().get());
    let started = Instant::now();
    let mut reported = Instant::now();
    let mut peak = 0.0f32;

    while started.elapsed() < Duration::from_secs(seconds) {
        match capture.read(Duration::from_millis(200))? {
            Capture::Samples(samples) => {
                if samples.origin() == SampleOrigin::Endpoint {
                    peak = samples
                        .samples()
                        .iter()
                        .fold(peak, |peak, sample| peak.max(sample.abs()));
                }
            }
            Capture::Idle => {}
            Capture::FormatChanged(format) => {
                println!("the capture now presents {format}, which it cannot follow");
            }
        }

        if reported.elapsed() >= Duration::from_secs(1) {
            reported = Instant::now();
            let stats = capture.stats();
            println!(
                "frames {:>9}  {:>6.2}s  silence {:>9}  reopened {:>3}  discontinuities {:>3}  \
                 peak {peak:.4}  scoped to {}  {}",
                stats.frames,
                stats.frames as f64 / rate,
                stats.synthesised_silence_frames,
                stats.endpoint_changes,
                stats.discontinuities,
                capture.scoped_to(),
                if capture.target_is_running() {
                    "running"
                } else {
                    "the game has exited"
                }
            );
            peak = 0.0;
        }
    }

    // The end of a recording, done the way a recording ends it: stop the
    // stream, hand over what the audio engine still held, and only then let go.
    let before_drain = capture.stats().frames;
    capture.finish();
    while let Ok(Capture::Samples(samples)) = capture.read(Duration::from_millis(100)) {
        let _ = samples;
    }
    let drained = capture.stats().frames - before_drain;
    println!(
        "\ndrained {drained} frames ({:.1} ms) the audio engine still held when the capture \
         stopped",
        drained as f64 / rate * 1_000.0
    );

    Ok(())
}
