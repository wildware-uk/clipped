//! Puts the FFmpeg runtime libraries beside the binaries this build produces.
//!
//! The behaviour, and the reasoning behind it, are in `clipped-ffmpeg-runtime`.
//! This crate links FFmpeg to *read* recordings, so its own test executables
//! need the DLLs beside them whether or not a crate it depends on has already
//! placed them somewhere else (issue #158).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    clipped_ffmpeg_runtime::place_runtime_libraries();
}
