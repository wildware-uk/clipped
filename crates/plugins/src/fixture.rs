//! Test-only machinery: a temporary plugins directory, and the example
//! binaries installed into it as real plugins.
//!
//! The isolation rules are only worth anything if they are tested against a
//! real process doing the real thing (AGENTS.md section 54), so the tests in
//! this crate install `examples/example_plugin.rs` and
//! `examples/misbehaving_plugin.rs` exactly as a user would install a plugin:
//! a directory, a manifest, and the executable the manifest names.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crate::discovery::{discover, EnabledPlugin};
use crate::manifest::ObservedProcess;
use crate::report::SessionDetails;

/// How long a test waits for something a child process has to do.
///
/// Generous on purpose. Starting a process on a machine that several test
/// binaries — and, on this project, several agents — are sharing takes as long
/// as it takes, and a tight bound here would fail for reasons that have nothing
/// to do with the code (AGENTS.md section 25).
pub(crate) const PATIENCE: Duration = Duration::from_secs(30);

/// A directory under the system temporary folder, removed when it is dropped.
#[derive(Debug)]
pub(crate) struct TemporaryDirectory {
    path: PathBuf,
}

impl TemporaryDirectory {
    /// A new directory, named after `purpose` and this process.
    pub(crate) fn new(purpose: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "clipped-plugins-{purpose}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("a temporary directory can be created");
        Self { path }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Writes a plugin directory: a manifest, and optionally the file it names.
    pub(crate) fn install(&self, directory: &str, manifest: &str, executable: Option<&str>) {
        let plugin = self.path.join(directory);
        fs::create_dir_all(&plugin).expect("a plugin directory can be created");
        fs::write(plugin.join(crate::discovery::MANIFEST_FILE), manifest)
            .expect("a manifest can be written");
        if let Some(executable) = executable {
            fs::write(plugin.join(executable), b"not really an executable")
                .expect("an executable can be written");
        }
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        // Best effort: a plugin that was killed a moment ago may still have its
        // executable open, and a temporary directory that outlives a test run
        // is a nuisance rather than a failure.
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// An example binary, built beside this test binary.
///
/// Cargo publishes no environment variable naming an example's path — only
/// binaries get `CARGO_BIN_EXE_*` — but it does put examples in
/// `<target>/<profile>/examples`, and a test executable is in
/// `<target>/<profile>/deps`. The same reasoning as `crates/muxer`'s test
/// support module, which finds its example the same way.
pub(crate) fn example_binary(name: &str) -> PathBuf {
    let test_executable = std::env::current_exe().expect("a test knows its own path");
    let profile = test_executable
        .parent()
        .and_then(Path::parent)
        .expect("a test executable lives in <target>/<profile>/deps");
    let example = profile
        .join("examples")
        .join(name)
        .with_extension(std::env::consts::EXE_EXTENSION);

    assert!(
        example.is_file(),
        "{} has not been built. `cargo test` builds the examples; if this test was run some \
         other way, build them first with `cargo build -p clipped-plugins --examples`.",
        example.display()
    );
    example
}

/// Installs `example` as a plugin called `id`, under the file name
/// `installed_as` — which is what tells `misbehaving_plugin` how to misbehave.
///
/// Returns it enabled, with consent to exactly what it declares, which is the
/// only way to get something that can be started.
pub(crate) fn install_example(
    root: &TemporaryDirectory,
    id: &str,
    example: &str,
    installed_as: &str,
) -> EnabledPlugin {
    let executable = format!("{installed_as}.{}", std::env::consts::EXE_EXTENSION);
    let executable = executable.trim_end_matches('.').to_owned();
    let manifest = format!(
        r#"{{
            "contract": 1,
            "id": "{id}",
            "name": "Test plugin {id}",
            "version": "0.0.0",
            "description": "Installed by a test.",
            "executable": "{executable}",
            "supports": {{ "executables": ["cs2.exe"] }}
        }}"#
    );

    let directory = root.path().join(id);
    fs::create_dir_all(&directory).expect("a plugin directory can be created");
    fs::write(directory.join(crate::discovery::MANIFEST_FILE), manifest)
        .expect("a manifest can be written");
    fs::copy(example_binary(example), directory.join(&executable))
        .expect("an example can be installed as a plugin");

    let installed = discover(root.path())
        .installed
        .into_iter()
        .find(|plugin| plugin.id().as_str() == id)
        .expect("the plugin that was just installed is discovered");
    let consent = installed.consent_token();
    installed
        .enable(&consent)
        .expect("consent to what it declares now")
}

/// The session a test attaches plugins to.
pub(crate) fn session() -> SessionDetails {
    SessionDetails {
        session: "test-session".to_owned(),
        process: ObservedProcess::new("cs2.exe", std::process::id()),
    }
}

/// Runs `step` until it answers `true`, or fails the test after [`PATIENCE`].
///
/// `step` is the caller's own loop body — polling a supervisor, draining a
/// queue — so what is being waited for and what is being checked while waiting
/// stay in one place.
pub(crate) fn until(what: &str, mut step: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        if step() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("waited {PATIENCE:?} for {what}, and it did not happen");
}
