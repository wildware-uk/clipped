//! Puts the FFmpeg runtime libraries beside the binaries this build produces.
//!
//! The behaviour, and the reasoning behind it, are in `clipped-ffmpeg-runtime`.
//! It lives in its own crate because `clipped-encoder` links FFmpeg too and
//! needs exactly the same thing, and two copies of a staleness rule would drift
//! (issue #158).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    clipped_ffmpeg_runtime::place_runtime_libraries();
}
