//! A continuous sine wave, rendered to the default output endpoint until it is
//! told to stop.
//!
//! # Why this is a module of its own
//!
//! [`crate::tone`] is the *other* kind of sound this package makes: short
//! bursts placed at the moment a named frame is presented, for measuring the
//! absolute offset between a recording's picture and its sound
//! (`docs/av-sync.md`). Nothing about that helps a test which needs a
//! frequency to be *continuously* present in a track, because a 30 ms burst
//! every five seconds is silence to a quarter-second analysis window.
//!
//! An audio isolation test needs the opposite: one frequency, held for the
//! whole of a recording, so that "this track carries the game's tone and not
//! the neighbour's" is a question a Goertzel filter can answer over any window
//! of the file (`tests/audio/track_isolation.rs`).
//!
//! This module was `test-apps/process-tree-audio/src/tone.rs` and moved down
//! here when `video-pattern` needed the same loop. Two test applications
//! rendering a sine to the same endpoint through the same [`RenderStream`]
//! should be one loop, not two that drift apart (AGENTS.md section 55).
//!
//! # Ownership
//!
//! The [`RenderStream`] is owned by the thread that opens it and stops the
//! client when it drops, so no path — a panic in the loop included — leaves a
//! stream playing into somebody's speakers (AGENTS.md section 58).

use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use crate::render_stream::{RenderStream, Samples};

/// How long the loop sleeps between looks at how much room the endpoint has.
///
/// Short against the 200 ms the stream is opened with, so the buffer never runs
/// dry, and long enough that a subject is not a busy loop on a machine that is
/// also recording.
const FEED_INTERVAL: Duration = Duration::from_millis(2);

/// The tone a caller plays unless it has a reason to pick another, in hertz.
///
/// 997 Hz: the frequency digital audio has used for measurements for a century,
/// because it sits between B5 and C6 and no instrument plays it. That matters
/// here for a specific, measured reason — `crates/audio/tests/system_audio.rs`
/// used 440 Hz, which is A above middle C exactly, and failed on a developer's
/// machine because music playing on it put 0.013 into the 440 Hz bin against
/// the tone's 0.037. Background music contributes almost nothing to 997 Hz.
///
/// AGENTS.md section 26's plan names 440, 880 and 1320 Hz, and the tests that
/// use those numbers are the ones whose samples are *synthesised*
/// (`crates/muxer/tests/multi_track_audio.rs`,
/// `crates/session/src/audio/tests.rs`): nothing else is playing in a buffer a
/// test filled itself. A tone that has to survive a real endpoint on a machine
/// somebody is using needs a bin nothing else is in.
pub const FREQUENCY: f32 = 997.0;

/// A second tone, for the source a test is asserting the *absence* of.
///
/// 1373 Hz is neither a harmonic of [`FREQUENCY`] nor a musical note, so
/// neither tone can be mistaken for the other and nothing on the machine
/// produces either by accident. Both matter: an isolation test asserts that one
/// tone is present *and* that another is not, and a second frequency that was a
/// harmonic of the first would fail the second half for a reason that has
/// nothing to do with the capture. 880 Hz and 1320 Hz — section 26's other two
/// — are the second and third harmonics of 440 Hz, which is exactly the trap.
pub const SECOND_FREQUENCY: f32 = 1373.0;

/// The peak amplitude of a rendered tone, as a fraction of full scale.
///
/// About −28 dBFS. A Goertzel filter finds a tone far below this; the volume is
/// set by politeness on a machine somebody is using rather than by what the
/// measurement needs.
pub const AMPLITUDE: f32 = 0.04;

/// A tone playing on a thread of its own, for as long as this is held.
///
/// [`play`] is a loop and owns the calling thread; a program which has
/// something else to do — a window to present, a recording to drive — holds one
/// of these instead. It is the same loop, on a thread whose only job is that
/// loop, so nothing about the sound can be delayed by whatever the caller is
/// doing (AGENTS.md section 20).
#[derive(Debug)]
pub struct SteadyToneOutput {
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    frequency: f32,
    rate: u32,
    channels: u16,
}

impl SteadyToneOutput {
    /// Opens the default output endpoint and starts playing `frequency`.
    ///
    /// Returns once the tone is actually reaching the endpoint, which is what
    /// makes it safe for a caller to announce that this run makes a sound: a
    /// stream that has not started yet is silence, and a test that started
    /// recording against that announcement would measure the silence.
    ///
    /// # Errors
    ///
    /// Why this machine cannot play a tone, as a sentence. See [`play`].
    pub fn start(frequency: f32, amplitude: f32) -> Result<Self, String> {
        let running = Arc::new(AtomicBool::new(true));
        let (ready, started) = channel();

        let thread = std::thread::Builder::new()
            .name("video-pattern-steady-tone".to_owned())
            .spawn({
                let running = Arc::clone(&running);
                move || {
                    let mut ready = Some(ready);
                    let played = play(frequency, amplitude, None, &running, |rate, channels| {
                        if let Some(ready) = ready.take() {
                            let _ = ready.send(Ok((rate, channels)));
                        }
                    });
                    // Only reached without the endpoint having started when
                    // opening it failed, and then this is the only report there
                    // will ever be.
                    if let (Some(ready), Err(reason)) = (ready, &played) {
                        let _ = ready.send(Err(reason.clone()));
                    }
                }
            })
            .map_err(|error| format!("the tone thread could not be started: {error}"))?;

        match started.recv() {
            Ok(Ok((rate, channels))) => Ok(Self {
                running,
                thread: Some(thread),
                frequency,
                rate,
                channels,
            }),
            Ok(Err(reason)) => {
                running.store(false, Ordering::Relaxed);
                let _ = thread.join();
                Err(reason)
            }
            Err(_) => Err("the tone thread stopped before it reported anything".to_owned()),
        }
    }

    /// The frequency being played, in hertz.
    #[must_use]
    pub const fn frequency(&self) -> f32 {
        self.frequency
    }

    /// The endpoint's sample rate and channel count.
    #[must_use]
    pub const fn format(&self) -> (u32, u16) {
        (self.rate, self.channels)
    }
}

impl Drop for SteadyToneOutput {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

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
