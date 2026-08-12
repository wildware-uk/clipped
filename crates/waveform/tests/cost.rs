//! What generating a waveform actually costs.
//!
//! AGENTS.md section 19 asks for measurements rather than adjectives, and issue
//! #66 asks specifically for the cost on stated hardware rather than an
//! assertion that it is fast. These tests take the measurements every run and
//! print them; `docs/waveforms.md` records what they said on the development
//! machine, with the hardware, the workload and the build profile named.
//!
//! # Two workloads, because one of them is not the workload
//!
//! The first measurement is a minute of raw PCM in a WAV: no video, no
//! compression. It isolates the part this crate is responsible for — decode and
//! peak accumulation — and it runs on any checkout, because a WAV header is 44
//! bytes and PCM is the samples.
//!
//! It is **not** what a recording costs, and a figure taken from it must not be
//! extrapolated to one. A Clipped recording is a Matroska file whose video
//! packets are most of the bytes the demuxer reads (`src/analyse.rs`, "Costs":
//! there is no way to reach the last second of audio without reading past the
//! last second of video) and whose audio is compressed rather than copied. The
//! second measurement is over a container of that shape, and reports the
//! throughput — megabytes of container per second — which is the number that
//! extrapolates, rather than a per-minute figure that does not.
//!
//! # Why the assertions are loose
//!
//! A number these tests could fail on would be a number that depends on what
//! else the machine is doing, and a benchmark that fails because a second test
//! binary was running is a benchmark that gets deleted. What is asserted is the
//! property the design depends on: summarising a recording is far faster than
//! playing it, so a library scan finishes rather than running for ever. Run with
//! `--nocapture` to see the figures.

mod support;

use core::time::Duration;
use std::time::Instant;

use clipped_media_validation::TemporaryDirectory;
use clipped_waveform::analyse;

use support::{write_recording_shaped_container, write_wav, Tone};

/// How much audio is measured.
const SECONDS: f64 = 60.0;

/// The sample rate and channel count Clipped records at (SPEC.md section 11).
const RATE: u32 = 48_000;
const CHANNELS: usize = 2;

/// The recording-shaped container: long enough to measure, short enough that
/// encoding it does not dominate the suite.
const CONTAINER_SECONDS: u32 = 10;

/// Its audio tracks: game, other system audio and microphone (SPEC.md section
/// 11).
const CONTAINER_TRACKS: usize = 3;

/// Its video bitrate.
///
/// Roughly 40 times the audio's, which is the ratio that decides how much of
/// this work is demuxing video the analyser then throws away. Below what Clipped
/// records 1440p at, so the figure this produces is conservative rather than
/// flattering.
const CONTAINER_VIDEO_KILOBITS: u32 = 20_000;

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

#[test]
fn a_container_shaped_like_a_recording_is_summarised_at_disk_speed() {
    let directory = TemporaryDirectory::new("waveform-cost");
    let path = directory.file("recording.mkv");
    if !write_recording_shaped_container(
        &path,
        CONTAINER_SECONDS,
        CONTAINER_TRACKS,
        CONTAINER_VIDEO_KILOBITS,
    ) {
        return;
    }

    let bytes = std::fs::metadata(&path).expect("the file exists").len();
    let started = Instant::now();
    let waveform = analyse(&path).expect("the container can be summarised");
    let elapsed = started.elapsed();

    assert_eq!(waveform.tracks().len(), CONTAINER_TRACKS);
    let seconds = f64::from(CONTAINER_SECONDS);
    let drift = waveform.duration().as_secs_f64() - seconds;
    assert!(drift.abs() < 0.2, "{:?}", waveform.duration());

    let megabytes = bytes as f64 / (1024.0 * 1024.0);
    let taken = elapsed.as_secs_f64().max(f64::EPSILON);
    println!(
        "waveform cost, recording-shaped: {:.0} ms for {seconds:.0} s of 1280x720 30 fps H.264 \
         at {CONTAINER_VIDEO_KILOBITS} kb/s with {CONTAINER_TRACKS} AAC tracks at 160 kb/s \
         ({megabytes:.1} MB), which is {:.0} ms per minute of recording, {:.0} MB/s of \
         container, {:.0}x faster than real time [{} build]",
        elapsed.as_secs_f64() * 1_000.0,
        taken * 60_000.0 / seconds,
        megabytes / taken,
        seconds / taken,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
    );

    // Same property as above, and the one the design rests on: reading a
    // recording to summarise it is far quicker than the recording is long, so a
    // library scan converges. The throughput in the line above is what a real
    // recording's cost is extrapolated from, because that cost is set by how
    // many bytes there are to read and not by how much audio is in them.
    assert!(
        elapsed < Duration::from_secs_f64(seconds),
        "summarising a {seconds} s recording took {elapsed:?}, which is longer than playing it"
    );
}
