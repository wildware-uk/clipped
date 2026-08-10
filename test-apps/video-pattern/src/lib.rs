//! A controlled test application that renders a deterministic, machine-readable
//! video pattern, and the pieces a test needs to drive and read it.
//!
//! # Why this exists
//!
//! Capture cannot be tested against a game. A game is not installed on every
//! machine, does not render the same thing twice, and cannot tell a test which
//! frame it just presented — so "did the capture drop a frame?" becomes a
//! person watching a recording and forming an opinion. AGENTS.md sections 25
//! and 26 rule that out and ask for controlled test applications instead. This
//! is the video one.
//!
//! It draws frames whose content *is* the answer: each frame carries its own
//! counter, a checksum of it, a background colour and a marker position that
//! both agree with the counter. A test captures the window, decodes what
//! arrived, and knows exactly which source frames it saw, which it never saw,
//! and which it saw twice — programmatically, with no person watching
//! ([`pattern`]).
//!
//! # The three pieces
//!
//! | Module | What it is | Where it runs |
//! | --- | --- | --- |
//! | [`pattern`] | The pattern: how a frame is drawn and how one is read back | Anywhere. It is pixel arithmetic, and its tests need no GPU |
//! | [`harness`] | Starting the application from a test and stopping it afterwards | Anywhere |
//! | The renderer and the run loop | A Direct3D 11 window presenting the pattern at a fixed rate | Windows |
//!
//! Only the last of those is platform code, and it is compiled only on Windows
//! (AGENTS.md section 5). Everything the pattern promises can therefore be
//! tested on any machine, which is what stops the pattern and the decoder
//! drifting apart between capture runs.
//!
//! # How it is used
//!
//! By hand, and by a test, exactly the same way — the application has one
//! interface and it is the command line, with a line of standard output saying
//! what it did. `docs/testing.md` is the guide; the short version is:
//!
//! ```text
//! cargo run -p clipped-video-pattern --bin video-pattern -- --seconds 30
//! ```
//!
//! and, from a test, [`harness::TestApp`].
//!
//! # What it is not
//!
//! It does not capture, measure or assert anything. It is the *subject*: a
//! window with known content. The capture belongs to the test looking at it,
//! and the measurement of a capture backend's pacing and leaks belongs to
//! `crates/capture/examples/wgc_probe.rs`. `docs/testing.md` records what the
//! two share and why they are still separate programs.

pub mod harness;
pub mod pattern;

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod render;

#[cfg(windows)]
pub use app::{AppError, MonitorChoice, Options, Presentation, StopReason, Summary};

/// Runs the application until it is stopped.
///
/// See [`Options`] for what a run can be asked for, and `src/app.rs` for the
/// protocol it announces itself with.
///
/// # Errors
///
/// [`AppError`] if the window, the graphics device or a frame could not be
/// made.
#[cfg(windows)]
pub fn run(options: Options) -> Result<Summary, AppError> {
    app::run(options)
}
