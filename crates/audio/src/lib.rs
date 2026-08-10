//! Audio capture, per-source routing and track assembly.
//!
//! Independent, editable audio tracks are a core product feature, so sources
//! that the user expects to stay separate are never silently combined
//! (AGENTS.md section 21).
//!
//! # What exists
//!
//! System audio capture, on Windows: [`windows::SystemAudioCapture`] records
//! the endpoint Windows is playing through, using WASAPI loopback, and produces
//! timestamped `f32` buffers that form a continuous timeline whatever the
//! endpoint does. That is the foundation the rest of the audio work is built on
//! — it is one stream, not a track model — and
//! [`docs/audio-routing.md`](../../../docs/audio-routing.md) describes its
//! behaviour in full.
//!
//! Nothing else is built. Process-scoped capture, which is what actually
//! separates the game from everything else, is milestone M2 and
//! [ADR 0003](../../../docs/adr/0003-process-specific-audio-capture.md).
//! Microphone capture is
//! [issue #20](https://github.com/wildware-uk/clipped/issues/20), the
//! compatibility mix is
//! [issue #29](https://github.com/wildware-uk/clipped/issues/29), and
//! resampling between capture clocks is
//! [issue #30](https://github.com/wildware-uk/clipped/issues/30). Nothing
//! consumes what this crate produces yet: the muxer is
//! [issue #28](https://github.com/wildware-uk/clipped/issues/28).
//!
//! # Responsibilities
//!
//! - Capturing system, per-process and microphone audio.
//! - Resampling and clock-drift correction between sources.
//! - Assembling the configured set of output tracks.
//!
//! # Not responsible for
//!
//! Writing containers (see `clipped-muxer`) or choosing which tracks a game
//! should record (see `clipped-session`).
//!
//! # Position in the architecture
//!
//! Sits above `clipped-windows` and below `clipped-session`.
//!
//! # The three rules this crate is written around
//!
//! **Silence is data.** WASAPI loopback delivers nothing at all while the
//! endpoint is quiet, so a capture that concatenates what it is given produces
//! a track shorter than its recording, and every sound after the first quiet
//! passage lands too early. Quiet periods are therefore filled with silence
//! measured against the device's clock; the `timeline` module is that
//! arithmetic and the reasoning behind it.
//!
//! **Timestamps come from the audio device.** [`AudioTimestamp`] has no
//! `now()`. A buffer's timestamp is the position WASAPI attached to it, on the
//! Windows performance counter — the same clock a captured video frame is
//! stamped on — so the two can be compared without a conversion nobody can
//! check.
//!
//! **A recording outlives its audio device.** The default endpoint changing,
//! being unplugged, or not existing at all does not end a capture; the track
//! becomes silence of the right length and the capture moves to whatever
//! Windows is playing through now (AGENTS.md sections 16 and 17). The only
//! thing that stops a capture is the caller.
//!
//! # Example
//!
//! Recording a second of system audio and reporting how much of it the endpoint
//! actually produced:
//!
//! ```no_run
//! use std::time::{Duration, Instant};
//!
//! use clipped_audio::{Capture, SampleOrigin, windows::SystemAudioCapture};
//!
//! let mut capture = SystemAudioCapture::open()?;
//! println!("Recording {}", capture.format());
//!
//! let until = Instant::now() + Duration::from_secs(1);
//! while Instant::now() < until {
//!     match capture.read(Duration::from_millis(100))? {
//!         Capture::Samples(audio) => println!(
//!             "{} frames at {}{}",
//!             audio.frames(),
//!             audio.timestamp(),
//!             match audio.origin() {
//!                 SampleOrigin::Endpoint => "",
//!                 SampleOrigin::SynthesisedSilence => " (silence)",
//!             }
//!         ),
//!         Capture::Idle => {}
//!         // Someone plugged in a headset that sounds different from the
//!         // speakers. The recording carries on, silently, until it is
//!         // restarted.
//!         Capture::FormatChanged(format) => println!("the output device is now {format}"),
//!     }
//! }
//! # Ok::<(), clipped_audio::AudioError>(())
//! ```

mod buffer;
mod error;
mod format;
mod time;
mod timeline;

#[cfg(windows)]
pub mod windows;

pub use buffer::{CapturedAudio, SampleOrigin};
pub use error::{AudioError, Capture};
pub use format::{AudioFormat, ChannelMask, SampleFormat};
pub use time::AudioTimestamp;
