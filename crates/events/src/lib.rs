//! Vocabulary of game and recording events shared across the application.
//!
//! Game integrations report what happened in a game through the abstract event
//! model defined here rather than exposing each game's native protocol to the
//! rest of the application (AGENTS.md section 33).
//!
//! # Responsibilities
//!
//! - The universal event types and their payloads.
//! - Event timing relative to a recording.
//!
//! # Not responsible for
//!
//! Producing events (see `clipped-plugins`), persisting them (see
//! `clipped-storage`) or acting on them (see `clipped-session`).
//!
//! # Position in the architecture
//!
//! A leaf crate of shared types. It depends on no other `clipped-*` crate so
//! that both the plugin surface and the persistence layer can use it without
//! creating a cycle.
