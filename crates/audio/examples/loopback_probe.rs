//! Watches system audio capture while somebody unplugs things.
//!
//! Two of issue #19's behaviours cannot be asserted by an automated test on an
//! ordinary machine, because they need a hand on a cable: the default output
//! device genuinely moving, and the endpoint genuinely going silent because
//! nothing on the machine is playing. This example is how those are verified,
//! and how a support report is produced when a user says their recording went
//! quiet.
//!
//! It opens exactly the capture the recorder will open, reads it in a loop, and
//! prints a line a second saying how much audio arrived, how much silence had
//! to be synthesised, and which device it is on. Clipped's own logging is
//! installed, so the endpoint-change lines appear as they will in a real
//! session.
//!
//! ```text
//! cargo run -p clipped-audio --example loopback_probe -- 60
//! ```
//!
//! # What to do while it runs
//!
//! - Unplug or switch off the output device. The recording must not stop: the
//!   line count keeps rising, `silence` starts growing and `endpoint` says
//!   `<none>`.
//! - Plug it back in, or pick a different device in the volume flyout. There
//!   should be one `endpoint changed` log line and `endpoint` should become the
//!   new device's name.
//! - Stop everything that plays audio and leave the machine quiet. `frames`
//!   must keep rising at the sample rate — that is the silence WASAPI does not
//!   deliver being synthesised — and `silence` rises with it.
//!
//! What the run must never show is `frames` standing still, which would mean a
//! recording whose audio track is shorter than its video.

#![cfg(windows)]

use std::time::{Duration, Instant};

use clipped_audio::windows::SystemAudioCapture;
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

    let mut capture = SystemAudioCapture::open()?;
    let format = capture.format();
    println!("Recording system audio: {format}");
    println!("Device: {}", capture.endpoint_name().unwrap_or("<none>"));
    println!("Unplug, switch or silence the output device while this runs.\n");

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
                println!(
                    "the output device now presents {format}, which this capture cannot \
                          follow; the track is silence from here"
                );
            }
        }

        if reported.elapsed() >= Duration::from_secs(1) {
            let stats = capture.stats();
            println!(
                "{:>5.1}s  frames {:>9} ({:>6.2}s)  silence {:>9} ({:>6.2}s)  \
                 changes {}  discontinuities {}  peak {peak:.4}  endpoint {}",
                started.elapsed().as_secs_f32(),
                stats.frames,
                stats.frames as f64 / rate,
                stats.synthesised_silence_frames,
                stats.synthesised_silence_frames as f64 / rate,
                stats.endpoint_changes,
                stats.discontinuities,
                capture.endpoint_name().unwrap_or("<none>"),
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
