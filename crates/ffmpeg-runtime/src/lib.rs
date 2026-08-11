//! Puts the FFmpeg runtime libraries where the binaries this workspace builds
//! will find them.
//!
//! Clipped links dynamically against a prebuilt FFmpeg (see
//! `docs/adr/0004-ffmpeg-dependency-strategy.md`), so the variables in the
//! workspace's `.cargo/config.toml` are enough to *link*, but not enough to
//! *run*: Windows resolves a DLL from the directory of the executable that
//! needs it long before it consults anything else. The alternatives were
//! putting the FFmpeg `bin` directory on `PATH` — which every contributor and
//! every CI job would have to arrange, and which silently picks up whichever
//! FFmpeg is first on `PATH` when it goes wrong — or copying the libraries next
//! to the binaries, which is also exactly what the installed application will
//! do.
//!
//! # Why this is a crate rather than a build script
//!
//! It began as `crates/muxer/build.rs`, when the muxer was the only crate that
//! linked FFmpeg. Since the software encoder fallback
//! ([issue #18](https://github.com/wildware-uk/clipped/issues/18)),
//! `clipped-encoder` links it too — and the encoder is not a dependent of the
//! muxer, so nothing put the libraries beside *its* test executables. On a
//! fresh checkout `cargo test -p clipped-encoder` failed with
//! `STATUS_DLL_NOT_FOUND` and no indication why
//! ([issue #158](https://github.com/wildware-uk/clipped/issues/158)).
//!
//! Copying the script into the second crate would have left two copies of a
//! staleness rule to keep in step, so the behaviour lives here instead and both
//! build scripts are three lines that call [`place_runtime_libraries`]. A crate
//! that starts linking FFmpeg in future needs the same three lines; there is
//! nothing to reimplement and nothing to get subtly different.
//!
//! # Staying correct when the copies are removed
//!
//! A build script only re-runs when something it declared changed. If it
//! declared only `build.rs` and the environment, then deleting a copied DLL —
//! or `cargo clean`ing another crate's artefacts out from under it — would
//! leave the script skipped and the binary unable to start, with
//! `STATUS_DLL_NOT_FOUND` and nothing naming the cause. So the destinations are
//! declared as inputs too: `cargo:rerun-if-changed` on a path that does not
//! exist makes Cargo re-run the script until it does, which turns a deleted DLL
//! into a re-copy on the next build rather than into a puzzle.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Copies the pinned FFmpeg runtime libraries beside the binaries this build
/// produces.
///
/// Call this from the `build.rs` of any crate that links FFmpeg:
///
/// ```no_run
/// // in build.rs, inside fn main
/// println!("cargo:rerun-if-changed=build.rs");
/// clipped_ffmpeg_runtime::place_runtime_libraries();
/// ```
///
/// It emits its own `cargo:rerun-if-changed` and `cargo:rerun-if-env-changed`
/// directives, including one per destination, so the caller does not need to.
///
/// Does nothing when the target is not Windows, or when `FFMPEG_DIR` is unset.
/// `FFMPEG_DIR` is Clipped's own variable, naming the prefix that
/// `scripts/fetch-ffmpeg.ps1` installed and that `.cargo/config.toml` points
/// at. Its absence means the workspace configuration was not read — a build
/// from outside the repository — or that `rusty_ffmpeg` was pointed at FFmpeg
/// some other way, by vcpkg or by `FFMPEG_LIBS_DIR` set by hand. Either way it
/// is not this crate's business to say where the runtime libraries should have
/// come from.
///
/// # Panics
///
/// When `FFMPEG_DIR` names a prefix whose `bin` holds no DLLs, and when a copy
/// fails for any reason other than a concurrent build having already written
/// the same bytes. See [`copy_if_stale`].
pub fn place_runtime_libraries() {
    println!("cargo:rerun-if-env-changed=FFMPEG_DIR");

    // Only Windows binaries need this treatment, and the pinned build is a
    // Windows one. On any other host there is nothing sensible to copy, and the
    // link itself will have been arranged by pkg-config.
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let Some(ffmpeg_dir) = env::var_os("FFMPEG_DIR").map(PathBuf::from) else {
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

    for destination_dir in binary_directories() {
        for library in &libraries {
            let destination = destination_dir.join(library.file_name().expect("a file name"));
            println!("cargo:rerun-if-changed={}", destination.display());
            copy_if_stale(library, &destination);
        }
    }
}

/// Lists the DLLs shipped in the FFmpeg build's `bin` directory.
///
/// Everything is copied rather than a hand-written list of the libraries a
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
/// `target/<profile>` holds binaries, `target/<profile>/deps` holds test
/// executables and `target/<profile>/examples` holds examples; all three need
/// the libraries beside them, and leaving any of them out makes "nothing has to
/// be on `PATH`" quietly false the first time somebody uses it. The paths are
/// derived from `OUT_DIR`, which Cargo documents as
/// `target/<profile>/build/<crate>-<hash>/out`, because Cargo exposes no
/// variable naming the profile directory directly.
///
/// The cost is stated rather than hidden: the pinned build's seven DLLs are
/// 136 MB, so this is 409 MB inside the target tree per profile built,
/// including while `examples` is still empty. `docs/ffmpeg.md` and
/// `docs/prerequisites.md` say so too, because it is the contributor's disk it
/// comes out of. Copying unconditionally was preferred to copying only where
/// something has already been linked, because the alternative is a rule about
/// when the libraries appear, and the symptom of getting that rule wrong is an
/// executable that will not start.
///
/// Two crates calling this in one build write the same bytes to the same
/// destinations. That is why [`copy_if_stale`] compares before it writes, and
/// why it tolerates the one failure a concurrent identical write can produce.
fn binary_directories() -> Vec<PathBuf> {
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"));
    let profile_dir = out_dir
        .ancestors()
        .nth(3)
        .expect("OUT_DIR is target/<profile>/build/<crate>-<hash>/out")
        .to_path_buf();

    vec![
        profile_dir.join("deps"),
        profile_dir.join("examples"),
        profile_dir,
    ]
}

/// Copies `source` to `destination` unless an identical copy is already there.
///
/// Rewriting ~70 MB of libraries on every incremental build would be a
/// noticeable tax on the edit-compile loop, and replacing a DLL that a running
/// test executable has mapped fails on Windows. Size and modification time are
/// enough to tell "already copied" from "the pin moved".
///
/// # Panics
///
/// When the copy fails and the destination is not already a byte-length match
/// for the source. A `cargo:warning` would not do: a build script does not
/// re-run on a successful build, so a failure demoted to a warning is
/// permanent, and the next thing anybody sees is `STATUS_DLL_NOT_FOUND` from a
/// binary that will not start. The one benign failure — another build holding
/// the destination open, having already written the same bytes there — is cheap
/// to recognise, so it is recognised rather than used to excuse every other
/// failure (AGENTS.md section 15).
fn copy_if_stale(source: &Path, destination: &Path) {
    let source_metadata = source
        .metadata()
        .unwrap_or_else(|error| panic!("could not read {}: {error}", source.display()));

    if let Ok(existing) = destination.metadata() {
        if existing.len() == source_metadata.len()
            && existing.modified().ok() == source_metadata.modified().ok()
        {
            return;
        }
    }

    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|error| {
            panic!("could not create {}: {error}", parent.display());
        });
    }

    let Err(error) = fs::copy(source, destination) else {
        return;
    };

    let already_there = destination
        .metadata()
        .is_ok_and(|existing| existing.len() == source_metadata.len());
    if already_there {
        println!(
            "cargo:warning=could not replace {} with {} ({error}), but the file \
             already there is the same size, so it is almost certainly the same \
             library held open by a concurrent build.",
            destination.display(),
            source.display(),
        );
        return;
    }

    panic!(
        "could not copy the FFmpeg runtime library {} to {}: {error}. Without \
         it, binaries built here will not start. Close anything running from \
         the target directory and build again.",
        source.display(),
        destination.display(),
    );
}
