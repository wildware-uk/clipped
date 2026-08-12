//! Rendering a sine wave to the default output endpoint until told to stop.
//!
//! The whole of the "player" role: open the endpoint, fill it with a sine, and
//! stop when standard input closes or the run is over. Opening and feeding the
//! endpoint is [`clipped_video_pattern::render_stream`], which exists to be
//! shared by more than one subject (AGENTS.md section 55); what is here is the
//! waveform and the run loop.
//!
//! # Ownership
//!
//! The [`RenderStream`] is owned by the thread that opens it and stops the
//! client when it drops, so no path — a panic in the loop included — leaves a
//! stream playing into somebody's speakers (AGENTS.md section 58).

use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use std::time::Instant;

use clipped_video_pattern::render_stream::{RenderStream, Samples};

/// How long the loop sleeps between looks at how much room the endpoint has.
///
/// Short against the 200 ms the stream is opened with, so the buffer never runs
/// dry, and long enough that a subject is not a busy loop on a machine that is
/// also recording.
const FEED_INTERVAL: Duration = Duration::from_millis(2);

/// What a finished run has to say for itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Played {
    /// Frames written to the endpoint.
    pub frames: u64,
    /// The rate they were written at.
    pub rate: u32,
    /// How many channels each frame had.
    pub channels: u16,
}

/// Plays `frequency` until `running` is cleared or `limit` has passed.
///
/// `announce` is called once, with the endpoint's rate and channel count, as
/// soon as the stream is playing — which is what a test waits for before it
/// starts capturing, because a subject that has not reached its endpoint yet is
/// silence that looks like a failure.
///
/// # Errors
///
/// Why this machine cannot play a tone, as a sentence: no output device, an
/// endpoint that refuses a shared-mode stream, or one whose mix format is not
/// 32-bit float. Each is a legitimate outcome for a test to skip on rather than
/// a fault (AGENTS.md section 25).
pub fn play(
    frequency: f32,
    amplitude: f32,
    limit: Option<Duration>,
    running: &AtomicBool,
    announce: impl FnOnce(u32, u16),
) -> Result<Played, String> {
    let stream = RenderStream::open(Samples::Float32)?;
    let rate = stream.rate();
    let channels = stream.channels();
    announce(rate, channels);

    let step = 2.0 * core::f32::consts::PI * frequency / rate as f32;
    let mut phase = 0.0f32;
    let mut frames = 0u64;
    let started = Instant::now();

    while running.load(Ordering::Relaxed) {
        if limit.is_some_and(|limit| started.elapsed() >= limit) {
            break;
        }

        let free = stream
            .buffer_frames()
            .saturating_sub(stream.queued_frames()?);
        if free == 0 {
            std::thread::sleep(FEED_INTERVAL);
            continue;
        }

        stream.write(free, |samples| {
            for frame in samples.chunks_exact_mut(usize::from(channels)) {
                let value = phase.sin() * amplitude;
                // Kept inside one turn rather than growing without limit, so
                // that a subject asked to play for an hour has the same phase
                // accuracy at the end as at the start.
                phase = (phase + step) % (2.0 * core::f32::consts::PI);
                frame.fill(value);
            }
        })?;
        frames += u64::from(free);

        std::thread::sleep(FEED_INTERVAL);
    }

    Ok(Played {
        frames,
        rate,
        channels,
    })
}
