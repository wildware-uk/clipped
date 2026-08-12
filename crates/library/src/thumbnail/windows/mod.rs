//! The Windows API calls thumbnail generation makes, which are all about thread
//! priority.
//!
//! Isolated here rather than spread through the service (AGENTS.md section 5),
//! so that everything above it is platform-neutral and compiles and tests
//! anywhere.

pub(super) mod priority;
