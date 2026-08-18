//! Audio capture, per-source routing and track assembly.
//!
//! Independent, editable audio tracks are a core product feature, so sources
//! that the user expects to stay separate are never silently combined
//! (AGENTS.md section 21).
//!
//! # What exists
//!
//! Three independent captures, on Windows, built on one engine:
//!
//! - [`windows::SystemAudioCapture`] records the endpoint Windows is playing
//!   through, using WASAPI loopback;
//! - [`windows::MicrophoneCapture`] records an input device, chosen with
//!   [`windows::MicrophoneSelection`] from the devices
//!   [`windows::microphones`] lists;
//! - [`windows::ProcessLoopbackCapture`] records everything one game's process
//!   tree plays and nothing else, which is what separates the game from the
//!   rest of the machine
//!   ([ADR 0003](../../../docs/adr/0003-process-specific-audio-capture.md)).
//!
//! All three produce timestamped `f32` buffers that form a continuous timeline
//! whatever the device or the game does, and any of them can run without the
//! others. That is the foundation the rest of the audio work is built on —
//! three streams, not a track model — and
//! [`docs/audio-routing.md`](../../../docs/audio-routing.md) describes their
//! behaviour in full.
//!
//! Beside them, and the one piece of the track model that lives here:
//! [`Mixer`] assembles the **compatibility mix**, the track a player that takes
//! one audio track arbitrarily should take (SPEC.md section 13). It is the only
//! place in Clipped where sources are deliberately combined, it combines copies,
//! and it holds the result under full scale rather than letting several loud
//! sources clip it (AGENTS.md section 21). Everything else about a source's
//! audio — the level it is mixed at, the limiting — is invisible on that
//! source's own track.
//!
//! The rest of the track model is not built. Capturing everything *except* a
//! game is [issue #27](https://github.com/wildware-uk/clipped/issues/27), and
//! microphone processing and the optional raw microphone track are
//! [issue #31](https://github.com/wildware-uk/clipped/issues/31) and
//! [issue #32](https://github.com/wildware-uk/clipped/issues/32).
//! [Issue #30](https://github.com/wildware-uk/clipped/issues/30) is two
//! different things that both change how many frames a buffer occupies, and
//! they are worth keeping apart: a source's own clock staying aligned with the
//! reference clock over a long recording (`timeline` and `resample`), and a
//! source captured at a genuinely different rate from the mix's being converted
//! to it (`mix::rate`). The first happens on every capture and the second only
//! on the mix's copy of a source whose rate differs — neither changes what an
//! isolated track contains. What consumes the mix is `clipped-session`'s
//! muxing, which registers one source per declared audio track and writes the
//! blocks this produces to track 1
//! ([issue #29](https://github.com/wildware-uk/clipped/issues/29)).
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
//! check. Every buffer carries two accounts of the same moment:
//! [`CapturedAudio::timestamp`] is where the track puts it, counting samples,
//! and [`CapturedAudio::device_timestamp`] is where the endpoint said it
//! belongs. The way the gap between them grows is the way the audio slides
//! against the picture; `docs/av-sync.md` is what measures it, and is exact
//! about what such a measurement does and does not cover.
//!
//! **A recording outlives its audio device.** The default endpoint changing,
//! being unplugged, or not existing at all does not end a capture; the track
//! becomes silence of the right length and the capture moves to whatever
//! Windows is playing through now, or waits for the microphone the user chose
//! to come back (AGENTS.md sections 16 and 17). The same holds for a game that
//! exits, or that exits the process it was launched as. The only thing that
//! stops a capture is the caller.
//!
//! A fourth rule applies to the microphone alone: **its samples never leave
//! this process except to the caller.** Nothing here writes them anywhere, and
//! no log line is derived from their values (AGENTS.md section 13). That is a
//! property of the type rather than a convention followed at each call site:
//! [`CapturedAudio`]'s [`Debug`] describes the buffer — frames, timestamp,
//! format, origin — and cannot print what is in it, so a consumer that writes
//! `tracing::debug!(?buffer)` still logs no audio.
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
mod dropout;
mod error;
mod format;
mod mix;
mod resample;
mod time;
mod timeline;

#[cfg(windows)]
pub mod windows;

pub use buffer::{CapturedAudio, SampleOrigin};
pub use error::{AudioError, Capture};
pub use format::{AudioFormat, ChannelMask, SampleFormat};
pub use mix::{Level, MixError, MixReport, MixSourceId, MixedAudio, Mixer};
pub use time::AudioTimestamp;
