//! Detection of running games and their launchers.
//!
//! Detection is provider-based so that support for a new launcher is an
//! addition rather than a change to shared logic (SPEC.md section 6).
//!
//! # Responsibilities
//!
//! - Watching for process start and exit.
//! - Matching processes against the known-game database and launcher installs.
//! - User overrides: manual registration, renaming and exclusion.
//!
//! # Not responsible for
//!
//! Starting or stopping recordings; it reports what is running and
//! `clipped-session` decides what to do about it.
//!
//! # Position in the architecture
//!
//! Sits above `clipped-windows` and below `clipped-session`.
