//! Puts the FFmpeg runtime libraries where the binaries this workspace builds
//! will find them.
//!
//! Clipped links dynamically against a prebuilt FFmpeg (see
//! `docs/adr/0004-ffmpeg-dependency-strategy.md`), so `FFMPEG_DIR` is enough to
//! *link*, but not enough to *run*: Windows resolves a DLL from the directory
//! of the executable that needs it long before it consults anything else. The
//! alternatives were putting the FFmpeg `bin` directory on `PATH` — which every
//! contributor and every CI job would have to arrange, and which silently picks
//! up whichever FFmpeg is first on `PATH` when it goes wrong — or copying the
//! libraries next to the binaries, which is also exactly what the installed
//! application will do.
//!
//! This script therefore copies the DLLs into the target directory beside both
//! the built executables and the test executables. Writing outside `OUT_DIR` is
//! not something a build script should do casually, but the alternative here is
//! worse, and the files written are confined to the target directory Cargo owns.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=FFMPEG_DIR");

    // Only Windows binaries need this treatment, and the pinned build is a
    // Windows one. On any other host there is nothing sensible to copy, and the
    // link itself will have been arranged by pkg-config.
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let Some(ffmpeg_dir) = env::var_os("FFMPEG_DIR").map(PathBuf::from) else {
        // Reaching here means `ffmpeg-sys-the-third` linked without FFMPEG_DIR
        // — through vcpkg or pkg-config — so it is not this script's business
        // to say where the libraries should have come from.
        return;
    };

    let library_dir = ffmpeg_dir.join("bin");
    let libraries = collect_libraries(&library_dir);
    assert!(
        !libraries.is_empty(),
        "FFMPEG_DIR is set to {} but {} contains no .dll files. Run \
         scripts/fetch-ffmpeg.ps1 to install the pinned FFmpeg build, or point \
         FFMPEG_DIR at a shared build that has one.",
        ffmpeg_dir.display(),
        library_dir.display(),
    );

    for destination in binary_directories() {
        for library in &libraries {
            copy_if_stale(
                library,
                &destination.join(library.file_name().expect("a file name")),
            );
        }
    }
}

/// Lists the DLLs shipped in the FFmpeg build's `bin` directory.
///
/// Everything is copied rather than a hand-written list of the libraries this
/// crate links against, because `avformat` loads its siblings and because the
/// set changes with the FFmpeg version. A list here would be a second thing to
/// keep in step with the pin, and would fail as a missing-DLL dialogue at
/// runtime rather than as a build error.
fn collect_libraries(library_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(library_dir) else {
        return Vec::new();
    };

    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("dll"))
        })
        .collect()
}

/// The directories Cargo puts executables in for this build.
///
/// `target/<profile>` holds binaries and `target/<profile>/deps` holds test
/// executables, and both need the libraries beside them. The paths are derived
/// from `OUT_DIR`, which Cargo documents as
/// `target/<profile>/build/<crate>-<hash>/out`, because Cargo exposes no
/// variable naming the profile directory directly.
fn binary_directories() -> Vec<PathBuf> {
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"));
    let profile_dir = out_dir
        .ancestors()
        .nth(3)
        .expect("OUT_DIR is target/<profile>/build/<crate>-<hash>/out")
        .to_path_buf();

    vec![profile_dir.join("deps"), profile_dir]
}

/// Copies `source` to `destination` unless an identical copy is already there.
///
/// Rewriting ~70 MB of libraries on every incremental build would be a
/// noticeable tax on the edit-compile loop, and replacing a DLL that a running
/// test executable has mapped fails on Windows. Size and modification time are
/// enough to tell "already copied" from "the pin moved".
fn copy_if_stale(source: &Path, destination: &Path) {
    if let (Ok(from), Ok(to)) = (source.metadata(), destination.metadata()) {
        if from.len() == to.len() && from.modified().ok() == to.modified().ok() {
            return;
        }
    }

    if let Some(parent) = destination.parent() {
        let _ = fs::create_dir_all(parent);
    }

    // A failure here is worth reporting but not worth failing the build over:
    // the common cause is another target in the same workspace holding the
    // destination file open, having already copied the same bytes there.
    if let Err(error) = fs::copy(source, destination) {
        println!(
            "cargo:warning=could not copy {} to {}: {error}",
            source.display(),
            destination.display()
        );
    }
}
