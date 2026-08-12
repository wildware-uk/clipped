//! What generating a waveform actually costs, per minute of audio.
//!
//! AGENTS.md section 19 asks for measurements rather than adjectives, and issue
//! #66 asks specifically for the cost per minute of audio on stated hardware
//! rather than an assertion that it is fast. This test takes that measurement
//! every run and prints it; `docs/waveforms.md` records what it said on the
//! development machine, with the hardware and the build profile named.
//!
//! The assertion is deliberately loose. A number this test can fail on would be
//! a number that depends on what else the machine is doing, and a benchmark that
//! fails because a second test binary was running is a benchmark that gets
//! deleted. What is asserted is the property the design depends on: summarising
//! audio is far faster than playing it, so a library scan finishes rather than
//! running for ever. Run it with `--nocapture` to see the figure.

mod support;

use core::time::Duration;
use std::time::Instant;

use clipped_media_validation::TemporaryDirectory;
use clipped_waveform::analyse;

use support::{write_wav, Tone};

/// How much audio is measured.
const SECONDS: f64 = 60.0;

/// The sample rate and channel count Clipped records at (SPEC.md section 11).
const RATE: u32 = 48_000;
const CHANNELS: usize = 2;

#[test]
fn a_minute_of_audio_is_summarised_far_faster_than_it_could_be_played() {
    let directory = TemporaryDirectory::new("waveform-cost");
    let path = directory.file("minute.wav");

    // Real content rather than silence: a decoder given digital silence can
    // take a shortcut, and the peak accumulator's work is the same either way
    // only if there is something to accumulate.
    let channel = vec![
        Tone::at(SECONDS / 3.0, 0.9),
        Tone::silence(SECONDS / 3.0),
        Tone::at(SECONDS / 3.0, 0.3),
    ];
    write_wav(&path, RATE, &vec![channel; CHANNELS]);

    let bytes = std::fs::metadata(&path).expect("the file exists").len();
    let started = Instant::now();
    let waveform = analyse(&path).expect("the file can be summarised");
    let elapsed = started.elapsed();

    assert_eq!(waveform.tracks().len(), 1);
    let drift = waveform.duration().as_secs_f64() - SECONDS;
    assert!(drift.abs() < 0.05, "{:?}", waveform.duration());

    let tracks = waveform.tracks().len();
    let per_minute = elapsed.as_secs_f64() * 60.0 / SECONDS;
    println!(
        "waveform cost: {:.0} ms for {SECONDS:.0} s of {RATE} Hz {CHANNELS}-channel PCM \
         ({:.1} MB), which is {:.0} ms per minute of audio, {:.0}x faster than real time \
         [{} build, {tracks} track(s)]",
        elapsed.as_secs_f64() * 1_000.0,
        bytes as f64 / (1024.0 * 1024.0),
        per_minute * 1_000.0,
        SECONDS / elapsed.as_secs_f64().max(f64::EPSILON),
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
    );

    assert!(
        elapsed < Duration::from_secs_f64(SECONDS),
        "summarising {SECONDS} s of audio took {elapsed:?}, which is slower than playing it; \
         a library scan at that rate would never finish"
    );
}
