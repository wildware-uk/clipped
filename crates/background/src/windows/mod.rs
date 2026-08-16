//! The Windows API calls this crate makes, which are all about thread
//! priority.
//!
//! Isolated here rather than spread through `src/worker.rs` (AGENTS.md
//! section 5), so that everything above it is platform-neutral and compiles
//! and tests anywhere.

pub(crate) mod priority;
