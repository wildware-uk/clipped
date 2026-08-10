//! The Windows audio stack, kept in one place.
//!
//! Every WASAPI call, every COM interface and every `unsafe` block in this
//! crate is under this module, behind `#[cfg(windows)]`, so that the
//! platform-neutral half — the sample-format vocabulary, the timeline that
//! keeps a track the same length as its recording, the sample conversion —
//! compiles and runs its tests anywhere (AGENTS.md section 5).

mod apartment;
mod endpoint;
mod endpoint_capture;
mod loopback;
mod microphone;
mod notifications;

pub use endpoint_capture::CaptureStats;
pub use loopback::SystemAudioCapture;
pub use microphone::{microphones, Microphone, MicrophoneCapture, MicrophoneSelection};
