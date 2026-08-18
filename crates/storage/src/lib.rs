//! SQLite persistence, schema migrations and on-disk layout.
//!
//! Media files stay as ordinary files on disk and the database holds only
//! references and metadata, so that a user keeps access to their recordings
//! even if they stop using this application (AGENTS.md sections 31 and 32).
//!
//! # What is here
//!
//! - [`Database`], which opens the file, puts it into the state the
//!   recorder-writes/UI-reads model needs, and migrates it.
//! - The [`migrations`] framework and the schema itself, in
//!   `crates/storage/migrations/`.
//!
//! [docs/storage.md](https://github.com/wildware-uk/clipped/blob/main/docs/storage.md)
//! is the prose: the tables and why each one is shaped as it is, the
//! concurrency model, and how this relates to the session sidecars.
//!
//! # The one rule
//!
//! **Nothing in this crate opens a media file.** A recording is a `path` column
//! and a few facts about the file at that path — its size, whether it is still
//! there. Every failure this crate can have is a failure to write metadata, and
//! metadata can be rebuilt from the session sidecars written beside the
//! recordings (`docs/sessions.md`). That separation is what AGENTS.md
//! section 17 asks for when it says a metadata database failure must not corrupt
//! video files, and `tests/recordings_are_never_touched.rs` holds it to it.
//!
//! # Its relationship to the session sidecars
//!
//! The recorder writes one JSON sidecar per session beside the recordings, and
//! has done since automatic recording arrived
//! ([#46](https://github.com/wildware-uk/clipped/issues/46)). This database
//! **ingests** those files; it does not replace them and is not a second copy of
//! the same mechanism (AGENTS.md section 55).
//!
//! They are different things with different lifetimes. The sidecar is the
//! recorder's crash-safe record of one session, written where the files are, by
//! the process that made them, and it is what makes a recording legible to
//! somebody who has stopped using Clipped. The database is the *index*: it
//! answers questions across all of them — every session of one game, the clips
//! from last week, what is taking up 40 GB — which is not a question a directory
//! of JSON files can answer without reading all of them.
//!
//! So the sidecar stays authoritative for a single session's facts, the database
//! is derived and can be rebuilt from the sidecars, and a database that is lost
//! or corrupt costs an index rather than a library. Ingestion itself belongs to
//! `clipped-library`, which is the crate whose remit is reconciling the index
//! against what is on disk.
//!
//! # Who writes, and from what thread
//!
//! **Nothing that must not stall touches this crate.** AGENTS.md section 20
//! says a capture thread must not wait on the database, and the way that is
//! kept is that no capture, encoder or muxer crate depends on this one at all —
//! `no_crate_on_a_capture_or_encoding_path_can_reach_the_database` in
//! `tests/integration/tests/workspace_layering.rs` is that as a rule rather
//! than as a habit.
//!
//! What does write is the recorder's library indexer, on a thread of its own
//! that nothing waits for, and the connection threads that answer a window's
//! `set_favourite`, `set_lock`, `restore_from_trash` and `empty_trash` — each
//! of which is a write the caller asked for and is waiting to hear the result
//! of. `docs/storage.md` has the table of which write runs where.
//!
//! This crate once carried a `WriteQueue`: a bounded channel a capture thread
//! could drop a closure on, drained by a batching writer thread. It was written
//! before there were any callers, no caller ever appeared, and by the time one
//! did the writes had all landed somewhere a stall does not matter — so it was
//! removed rather than wired in ([issue #605](https://github.com/wildware-uk/clipped/issues/605)).
//! Bring it back only for a *producer* that cannot wait and does not need the
//! answer; every write there is today needs the answer.
//!
//! # Position in the architecture
//!
//! A leaf crate. Higher layers depend on it; it depends on no other
//! `clipped-*` crate. That is why it does not resolve where the database lives:
//! reading `%LOCALAPPDATA%` is `clipped_logging::application_directory`'s job
//! and this crate sits beside it rather than above it, so the caller passes a
//! path.

mod database;
mod error;
pub mod migrations;

#[cfg(test)]
mod test_support;

pub use database::{Database, APPLICATION_ID};
pub use error::StorageError;
pub use migrations::{MigrationOutcome, SCHEMA_VERSION};

/// The SQLite binding this crate is built on, re-exported.
///
/// Deliberate, and the alternative was worse. This crate owns the database file
/// and the schema; it does not own the queries, because a hand-written accessor
/// for every column would be a second schema to keep in step with the first and
/// would put `clipped-library`'s vocabulary into the crate that is meant to sit
/// underneath it. Callers therefore write SQL, and writing SQL means naming
/// `rusqlite`'s types — so they get them from here rather than depending on a
/// version of `rusqlite` that might not be this one.
pub use rusqlite;
