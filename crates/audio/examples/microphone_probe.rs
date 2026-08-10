//! Watches microphone capture while somebody unplugs things.
//!
//! Two of issue #20's behaviours cannot be asserted by an automated test on an
//! ordinary machine, because they need a hand on a cable: the microphone
//! genuinely being unplugged mid-recording, and Windows genuinely moving the
//! default input device. This example is how those are verified, and how a
//! support report is produced when a user says their microphone track was
//! silent.
//!
//! It opens exactly the capture the recorder will open, reads it in a loop, and
//! prints a line a second saying how much audio arrived, how much silence had
//! to be synthesised, whether Windows has the device muted, and which device it
//! is on. Clipped's own logging is installed, so the device-change lines appear
//! as they will in a real session.
//!
//! ```text
//! cargo run -p clipped-audio --example microphone_probe -- 60
//! cargo run -p clipped-audio --example microphone_probe -- 60 "{0.0.1.00000000}.{...}"
//! ```
//!
//! With no second argument it records the default microphone and follows it
//! when Windows moves it. With a device identifier — the ones printed at the
//! start — it records that microphone and only that one, which is what a user
//! who has chosen a device in the settings gets.
//!
//! # What it prints, and what it does not
//!
//! Frame counts, durations, device names and one peak level per second. It does
//! not write the audio anywhere, keep it, or derive anything else from the
//! samples: a microphone hears the room, and a diagnostic that recorded it
//! would be a worse privacy problem than the one it was meant to solve
//! (AGENTS.md section 13). The peak is there so that "the track is silent" can
//! be told apart from "the track is quiet".
//!
//! # What to do while it runs
//!
//! - Unplug the microphone, or switch it off. The recording must not stop: the
//!   line count keeps rising, `silence` starts growing and `device` says
//!   `<none>`. Plug it back in and there should be one device-change log line
//!   and the microphone's name again.
//! - Mute the microphone in Windows. `muted` becomes `yes` and `peak` falls to
//!   zero, which is the pair of facts that explains a silent track.
//! - With no argument, make a different microphone the default in Windows'
//!   sound settings. The capture should move to it. With a device identifier
//!   argument it must *not* move: a chosen device is waited for, not replaced.
//!
//! What the run must never show is `frames` standing still, which would mean a
//! recording whose microphone track is shorter than its video.

#![cfg(windows)]

use std::time::{Duration, Instant};

use clipped_audio::windows::{microphones, MicrophoneCapture, MicrophoneSelection};
use clipped_audio::{Capture, SampleOrigin};
use clipped_logging::LogSettings;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let seconds: u64 = std::env::args()
        .nth(1)
        .map_or(Ok(30), |argument| argument.parse())
        .map_err(|error| format!("the first argument is how many seconds to run for: {error}"))?;

    let _logging = clipped_logging::init(
        &LogSettings::default()
            .without_level_file()
            .with_default_level("info"),
    )?;

    println!("Microphones on this machine:");
    for microphone in microphones()? {
        println!(
            "  {}{}\n    {}",
            microphone.name(),
            if microphone.is_default() {
                "  (Windows' default)"
            } else {
                ""
            },
            microphone.id(),
        );
    }
    println!();

    let selection = match std::env::args().nth(2) {
        None => MicrophoneSelection::SystemDefault,
        Some(id) => {
            let name = microphones()?
                .into_iter()
                .find(|microphone| microphone.id() == id)
                .map_or_else(
                    || "the chosen microphone".to_owned(),
                    |m| m.name().to_owned(),
                );
            MicrophoneSelection::device(id, name)
        }
    };

    let mut capture = MicrophoneCapture::open(&selection)?;
    let format = capture.format();
    println!("Recording the microphone: {format}");
    println!("Device: {}", capture.device_name().unwrap_or("<none>"));
    println!("Unplug, switch, mute or change the microphone while this runs.");
    println!("Nothing captured here is written anywhere.\n");

    let rate = f64::from(format.sample_rate().get());
    let started = Instant::now();
    let mut reported = Instant::now();
    let mut peak = 0.0f32;

    while started.elapsed() < Duration::from_secs(seconds) {
        match capture.read(Duration::from_millis(200))? {
            Capture::Samples(samples) => {
                if samples.origin() == SampleOrigin::Endpoint {
                    // A level, not the audio: the peak of this buffer, thrown
                    // away every time a line is printed.
                    peak = samples
                        .samples()
                        .iter()
                        .fold(peak, |peak, sample| peak.max(sample.abs()));
                }
            }
            Capture::Idle => {}
            Capture::FormatChanged(format) => {
                println!(
                    "the microphone now presents {format}, which this capture cannot follow; \
                     the track is silence from here"
                );
            }
        }

        if reported.elapsed() >= Duration::from_secs(1) {
            let stats = capture.stats();
            println!(
                "{:>5.1}s  frames {:>9} ({:>6.2}s)  silence {:>9} ({:>6.2}s)  \
                 changes {}  discontinuities {}  peak {peak:.4}  muted {}  device {}",
                started.elapsed().as_secs_f32(),
                stats.frames,
                stats.frames as f64 / rate,
                stats.synthesised_silence_frames,
                stats.synthesised_silence_frames as f64 / rate,
                stats.endpoint_changes,
                stats.discontinuities,
                match capture.is_muted() {
                    Some(true) => "yes",
                    Some(false) => "no",
                    None => "unknown",
                },
                capture.device_name().unwrap_or("<none>"),
            );
            reported = Instant::now();
            peak = 0.0;
        }
    }

    let stats = capture.stats();
    println!(
        "\n{:.2}s of audio for {:.2}s of wall time, {:.2}s of it synthesised silence",
        stats.frames as f64 / rate,
        started.elapsed().as_secs_f64(),
        stats.synthesised_silence_frames as f64 / rate,
    );
    Ok(())
}
