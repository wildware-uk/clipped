//! The Clipped recording process.
//!
//! This is a deliberately separate process from the desktop application: if the
//! UI crashes or is closed, recording must continue (SPEC.md section 5).
//!
//! Command-line parsing and the capture commands themselves are added by the
//! recording engine milestone; this binary currently only reports its identity
//! so that the workspace has a buildable entry point to grow from.

fn main() {
    println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    println!("No capture commands are implemented yet. See the M1 milestone.");
}
