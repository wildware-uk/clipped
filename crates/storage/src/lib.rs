//! SQLite persistence, schema migrations and on-disk layout.
//!
//! Media files stay as ordinary files on disk and the database holds only
//! references and metadata, so that a user keeps access to their recordings
//! even if they stop using this application (AGENTS.md sections 31 and 32).
//!
//! # Responsibilities
//!
//! - Opening the database and applying migrations.
//! - Defining where recordings, clips and thumbnails live on disk.
//!
//! # Not responsible for
//!
//! Interpreting library data (see `clipped-library`) or media contents.
//!
//! # Position in the architecture
//!
//! A leaf crate. Higher layers depend on it; it depends on no other
//! `clipped-*` crate.
