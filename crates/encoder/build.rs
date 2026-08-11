//! Puts the FFmpeg runtime libraries beside the binaries this build produces.
//!
//! This crate links FFmpeg for the software encoder fallback (issue #18), and
//! it is not a dependent of `clipped-muxer`, so nothing else places the runtime
//! libraries beside its test executables. Without this, `cargo test -p
//! clipped-encoder` on a fresh checkout fails with `STATUS_DLL_NOT_FOUND`
//! (issue #158).
//!
//! The behaviour, and the reasoning behind it, are in `clipped-ffmpeg-runtime`.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    clipped_ffmpeg_runtime::place_runtime_libraries();
}
