//! Recording session coordination across capture, audio, encode and mux.
//!
//! This is the crate that knows the application's rules: which game is running,
//! which settings apply to it, when a recording starts and stops, and what
//! happens when part of the pipeline fails mid-session.
//!
//! # Responsibilities
//!
//! - Session lifecycle and capture-mode behaviour.
//! - Resolving configuration, including per-game overrides.
//! - Wiring capture, audio, encoding and muxing together and recovering from
//!   failures in any of them.
//!
//! # Not responsible for
//!
//! Any user interface. The desktop application talks to the recorder over an
//! explicit service boundary and the recorder must keep running without it
//! (AGENTS.md section 5).
//!
//! # Position in the architecture
//!
//! The top layer of the workspace, depended on by `apps/recorder`.
