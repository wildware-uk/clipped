//! Generates what the Tauri runtime reads at start-up: the application
//! manifest, the Windows resources, and the permission schemas derived from
//! `tauri.conf.json`.

use std::fs;
use std::path::Path;

/// Where `scripts/stage-installer-payload.ps1` puts the recorder and the FFmpeg
/// runtime libraries, and what `bundle.resources` in `tauri.conf.json` maps into
/// the installation directory (issue #226).
const INSTALLER_PAYLOAD: &str = "installer-payload";

fn main() {
    ensure_installer_payload_directory_exists();
    tauri_build::build();
}

/// Creates the installer payload directory when it is not there.
///
/// It holds build outputs — a release recorder and 136 MB of FFmpeg DLLs — so it
/// is not in version control, which means a fresh clone does not have it. That
/// matters because `tauri_build::build()` copies every declared resource into the
/// Cargo target directory from here, on *every* build of this crate: `cargo
/// build`, `cargo clippy --all-targets` and `cargo test` all run this script, and
/// CI's Desktop UI job runs the last two. A declared resource path that does not
/// exist is an error in `tauri_utils::resources::ResourcePaths`, so without this
/// line a clean checkout could not even run this crate's tests.
///
/// An *empty* directory is skipped by that same code, which is what makes the
/// arrangement work: an ordinary build of the window finds nothing here and
/// copies nothing, and only `tauri build` fills it — its `beforeBuildCommand`
/// runs the staging script, which refuses to continue when the recorder or the
/// FFmpeg libraries are missing rather than producing an installer that cannot
/// record.
///
/// # Panics
///
/// When the directory cannot be created, because the alternative is
/// `tauri_build::build()` failing immediately afterwards with a message about a
/// resource path rather than about a directory this build was supposed to make.
fn ensure_installer_payload_directory_exists() {
    // Declared as an input so that staging the payload after a build re-runs
    // this script and re-copies it into the target directory. Cargo re-runs a
    // build script only when something it declared changed, and adding or
    // removing a file changes the directory's own modification time.
    println!("cargo:rerun-if-changed={INSTALLER_PAYLOAD}");

    let payload = Path::new(INSTALLER_PAYLOAD);
    fs::create_dir_all(payload).unwrap_or_else(|error| {
        panic!(
            "could not create {}, which tauri.conf.json declares as a bundle resource: {error}",
            payload.display()
        )
    });
}
