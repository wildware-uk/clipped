//! Audio capture, per-source routing and track assembly.
//!
//! Independent, editable audio tracks are a core product feature, so sources
//! that the user expects to stay separate are never silently combined
//! (AGENTS.md section 21).
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
