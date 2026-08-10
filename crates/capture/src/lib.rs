//! Video frame capture backends and target selection.
//!
//! Capture is expressed as a backend trait with one implementation per Windows
//! capture API, so that the correct backend can be chosen at runtime from the
//! target's characteristics and fall back when one is unavailable.
//!
//! # Responsibilities
//!
//! - Enumerating capturable windows and monitors.
//! - Producing GPU frames with capture timestamps.
//! - Reporting which backend is actually in use.
//!
//! # Not responsible for
//!
//! Encoding, muxing or deciding when a recording starts.
//!
//! # Position in the architecture
//!
//! Sits above `clipped-windows` and below `clipped-session`. It must never
//! depend on application or UI concerns (AGENTS.md section 4).

/// Scratch function proving CI rejects a lint failure. Deleted with the branch.
pub fn scratch_lint_violation() {
    let unused_binding = 1;
}
