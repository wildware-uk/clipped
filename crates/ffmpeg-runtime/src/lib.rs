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
use std::sync::atomic::{AtomicU64, Ordering};

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

/// Distinguishes one build script's temporary files from another's.
///
/// The process id alone is not enough: `cargo` runs several build scripts in
/// one process only when they are the same crate, but a counter costs nothing
/// and removes the question.
static TEMPORARY_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Copies `source` to `destination` unless an identical copy is already there.
///
/// Rewriting ~70 MB of libraries on every incremental build would be a
/// noticeable tax on the edit-compile loop, and replacing a DLL that a running
/// test executable has mapped fails on Windows. Size and modification time are
/// enough to tell "already copied" from "the pin moved".
///
/// # Why it copies via a temporary file
///
/// More than one crate links FFmpeg, so more than one build script calls
/// [`place_runtime_libraries`], and `cargo clippy --workspace --all-targets`
/// runs those build scripts in parallel. Two of them copying the same DLL to
/// the same destination at the same time is a plain race, and on Windows the
/// loser's `fs::copy` fails with `ERROR_SHARING_VIOLATION` rather than waiting
/// ([issue #212](https://github.com/wildware-uk/clipped/issues/212)).
///
/// Writing to a unique temporary name in the destination's own directory and
/// renaming over the target fixes both halves of that. The rename is atomic on
/// the same volume, so a concurrent reader sees either the old library or the
/// new one and never a half-written file — which is what makes the "already
/// there and the same size" check below sound, where before it could match a
/// copy still in progress.
///
/// A retry loop was the other option and is explicitly not what this does: it
/// would hide a genuinely locked file behind a delay and still fail in the end.
///
/// # Panics
///
/// When the library cannot be placed and the destination does not already hold
/// a copy the same size as the source. A `cargo:warning` would not do: a build
/// script does not re-run on a successful build, so a failure demoted to a
/// warning is permanent, and the next thing anybody sees is
/// `STATUS_DLL_NOT_FOUND` from a binary that will not start. The one benign
/// failure — another build having already put the same library there, and
/// something holding it open — is cheap to recognise, so it is recognised
/// rather than used to excuse every other failure (AGENTS.md section 15).
fn copy_if_stale(source: &Path, destination: &Path) {
    let source_metadata = source
        .metadata()
        .unwrap_or_else(|error| panic!("could not read {}: {error}", source.display()));

    if is_already_there(destination, &source_metadata) {
        return;
    }

    let Some(parent) = destination.parent() else {
        panic!("{} has no directory to be placed in", destination.display());
    };
    fs::create_dir_all(parent).unwrap_or_else(|error| {
        panic!("could not create {}: {error}", parent.display());
    });

    // Unique per process and per call, so two build scripts racing over the
    // same library are writing to two different files and neither can see a
    // partial copy of the other's.
    let temporary = parent.join(format!(
        "{}.{}.{}.tmp",
        destination
            .file_name()
            .expect("a destination with a parent has a file name")
            .to_string_lossy(),
        std::process::id(),
        TEMPORARY_FILE_COUNTER.fetch_add(1, Ordering::Relaxed),
    ));

    let placed = fs::copy(source, &temporary)
        .and_then(|_| fs::rename(&temporary, destination))
        .inspect_err(|_| {
            // Nothing else will ever look at this file, and leaving one behind
            // per failed build would slowly fill the target directory.
            let _ = fs::remove_file(&temporary);
        });

    let Err(error) = placed else {
        return;
    };

    // The rename lost a race, or the destination is held open by something
    // running from the target directory. Either way, what matters is whether a
    // usable library is there now — and after this change it cannot be a
    // half-written one.
    if is_same_size(destination, &source_metadata) {
        println!(
            "cargo:warning=could not replace {} with {} ({error}), but the file              already there is the same size, so it is the same library, placed              by a concurrent build or held open by something running from the              target directory.",
            destination.display(),
            source.display(),
        );
        return;
    }

    panic!(
        "could not place the FFmpeg runtime library {} at {}: {error}. Without          it, binaries built here will not start. Either something is running          from the target directory and holding the file open — close it and          build again — or the copy could not be completed at all, in which case          check there is free space on the volume and that {} is readable.",
        source.display(),
        destination.display(),
        source.display(),
    );
}

/// Whether `destination` is already the library `source_metadata` describes.
///
/// Size and modification time together: `fs::copy` and `fs::rename` both carry
/// the source's timestamps over on Windows, so an untouched copy matches on
/// both and a moved pin matches on neither.
fn is_already_there(destination: &Path, source_metadata: &fs::Metadata) -> bool {
    destination.metadata().is_ok_and(|existing| {
        existing.len() == source_metadata.len()
            && existing.modified().ok() == source_metadata.modified().ok()
    })
}

/// Whether `destination` holds a file the same size as the source.
///
/// Weaker than [`is_already_there`] on purpose: it is asked only after a
/// placement has failed, to tell "somebody else already put the right library
/// here" from "there is nothing usable here".
fn is_same_size(destination: &Path, source_metadata: &fs::Metadata) -> bool {
    destination
        .metadata()
        .is_ok_and(|existing| existing.len() == source_metadata.len())
}
#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};
    use std::thread;

    use super::*;

    /// A file of `size` bytes, in a directory that is removed with it.
    struct Fixture {
        directory: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let directory = env::temp_dir().join(format!(
                "clipped-ffmpeg-runtime-{name}-{}-{}",
                std::process::id(),
                TEMPORARY_FILE_COUNTER.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir_all(&directory).expect("a temporary directory can be created");
            Self { directory }
        }

        fn write(&self, name: &str, size: usize) -> PathBuf {
            let path = self.directory.join(name);
            let mut file = fs::File::create(&path).expect("the fixture file can be created");
            file.write_all(&vec![0x5a; size])
                .expect("the fixture file can be written");
            path
        }

        fn path(&self, name: &str) -> PathBuf {
            self.directory.join(name)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn a_library_is_placed_where_it_was_asked_for() {
        let fixture = Fixture::new("places");
        let source = fixture.write("avcodec-62.dll", 4096);
        let destination = fixture.path("deps/avcodec-62.dll");

        copy_if_stale(&source, &destination);

        assert_eq!(
            fs::read(&destination).expect("the library was placed"),
            fs::read(&source).expect("the source is readable"),
            "the placed library must be the source, byte for byte"
        );
    }

    #[test]
    fn a_library_being_placed_is_never_visible_half_written() {
        // The regression #212 is about, tested at the property that actually
        // distinguishes the fix.
        //
        // The old implementation copied straight into the destination, so while
        // a copy was running the destination held a partial file. That is what
        // made concurrent build scripts fail: the loser's `fs::copy` hit a
        // sharing violation, and the "is the right library already there?"
        // check it fell back on compared against a length that was still
        // growing, so it panicked and failed the build.
        //
        // Writing to a unique temporary name and renaming over the destination
        // means the destination is only ever the old library or the new one. A
        // reader watching it while a placement runs is the direct test of that,
        // and it fails against the old implementation.
        //
        // Two sizes, alternating, because a placement that finds the library
        // already there does nothing — a single repeated source would exercise
        // the fast path rather than the copy.
        const SMALL: usize = 3 * 1024 * 1024;
        const LARGE: usize = 9 * 1024 * 1024;
        const ROUNDS: usize = 6;

        let fixture = Fixture::new("atomic");
        let small = Arc::new(fixture.write("small.dll", SMALL));
        let large = Arc::new(fixture.write("large.dll", LARGE));
        let destination = Arc::new(fixture.path("deps/avformat-62.dll"));

        let watching = Arc::new(AtomicBool::new(true));
        let observed = Arc::new(Mutex::new(Vec::new()));

        let watcher = {
            let destination = Arc::clone(&destination);
            let watching = Arc::clone(&watching);
            let observed = Arc::clone(&observed);
            thread::spawn(move || {
                while watching.load(Ordering::Relaxed) {
                    if let Ok(metadata) = destination.metadata() {
                        let length = metadata.len();
                        let mut observed = observed.lock().expect("the log is not poisoned");
                        if observed.last() != Some(&length) {
                            observed.push(length);
                        }
                    }
                }
            })
        };

        for round in 0..ROUNDS {
            let source = if round % 2 == 0 { &small } else { &large };
            copy_if_stale(source, &destination);
        }

        watching.store(false, Ordering::Relaxed);
        watcher.join().expect("the watching thread does not panic");

        let observed = observed.lock().expect("the log is not poisoned").clone();
        let partial: Vec<u64> = observed
            .iter()
            .copied()
            .filter(|length| *length != SMALL as u64 && *length != LARGE as u64)
            .collect();

        assert!(
            partial.is_empty(),
            "the destination was seen at {partial:?} bytes, which is neither              {SMALL} nor {LARGE}: a reader can observe a half-written library,              so a concurrent build script can too"
        );
        assert!(
            observed.len() > 1,
            "the watcher never saw the library change, so this test proved              nothing; observed {observed:?}"
        );
    }

    #[test]
    fn a_library_that_is_already_there_is_not_copied_again() {
        let fixture = Fixture::new("skips");
        let source = fixture.write("avutil-60.dll", 2048);
        let destination = fixture.path("deps/avutil-60.dll");

        copy_if_stale(&source, &destination);
        let placed = destination
            .metadata()
            .expect("the library was placed")
            .modified()
            .expect("a modification time");

        copy_if_stale(&source, &destination);

        assert_eq!(
            destination
                .metadata()
                .expect("the library is still there")
                .modified()
                .expect("a modification time"),
            placed,
            "an unchanged library must not be rewritten: ~136 MB per build is a \
             real tax on the edit-compile loop"
        );
    }

    #[test]
    fn a_moved_pin_replaces_the_library_that_was_there() {
        let fixture = Fixture::new("replaces");
        let destination = fixture.path("deps/avfilter-11.dll");

        let old = fixture.write("old.dll", 1024);
        copy_if_stale(&old, &destination);

        let new = fixture.write("new.dll", 3072);
        copy_if_stale(&new, &destination);

        assert_eq!(
            fs::read(&destination).expect("the library is there").len(),
            3072,
            "a library of a different size is a moved pin and must be replaced"
        );
    }

    #[test]
    fn no_temporary_files_are_left_behind() {
        let fixture = Fixture::new("tidy");
        let source = fixture.write("swscale-9.dll", 4096);
        let destination = fixture.path("deps/swscale-9.dll");

        copy_if_stale(&source, &destination);

        let leftovers: Vec<_> = fs::read_dir(fixture.path("deps"))
            .expect("the destination directory exists")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();

        assert!(
            leftovers.is_empty(),
            "temporary files must not accumulate in the target directory: {leftovers:?}"
        );
    }
}
