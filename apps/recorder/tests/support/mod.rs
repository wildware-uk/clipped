//! Shared machinery for the recorder's process-level tests: sending a real
//! Ctrl+C, and putting a real window on screen to record.
//!
//! Both test binaries in this directory need the same three things — a console
//! to address, a child in a process group of its own, and a bounded wait for it
//! to exit — so they live here rather than in one of them.

// Each test binary compiles this module separately and uses the part of it that
// it needs, so anything used by only one of them is "unused" in the others.
#![allow(dead_code)]
#![cfg(windows)]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use windows_sys::Win32::System::Console::{AllocConsole, GenerateConsoleCtrlEvent, CTRL_C_EVENT};

/// <https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags>
pub(crate) const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// No console at all, which is how the desktop application starts a recorder.
///
/// `crates/ipc/src/supervisor/platform.rs` spawns it with exactly
/// `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`, and the first of those is what
/// makes [issue #220](https://github.com/wildware-uk/clipped/issues/220) a
/// problem: a process with no console cannot receive `CTRL_C_EVENT`, so before
/// the `shutdown` command there was no way to end one but Task Manager.
/// `tests/shutdown_command.rs` starts a recorder the same way, so that what it
/// proves is about the recorder a user actually has.
pub(crate) const DETACHED_PROCESS: u32 = 0x0000_0008;

/// How long a child is given to do something before a test gives up on it.
///
/// Generous on purpose. The assertion is "this happens", not "this happens
/// quickly", and a bound tight enough to trip on a loaded machine is a failure
/// nobody can tell from a real one (AGENTS.md section 25).
pub(crate) const PATIENCE: Duration = Duration::from_secs(30);

/// Makes sure this process has a console, so it can address one.
///
/// `cargo test` normally provides one. When it does not — a test run started
/// from a process with no console — `GenerateConsoleCtrlEvent` would fail with
/// no console to send to, and a child would inherit nothing to receive on, so
/// the console has to exist before the child is spawned.
pub(crate) fn ensure_console() {
    // SAFETY: `AllocConsole` takes no arguments and returns a BOOL. It either
    // attaches a console to this process or fails because one is already
    // attached; there is no pointer to get wrong and no state it can leave
    // half-initialised, so the call is sound in both outcomes. The result is
    // deliberately ignored: "already had one" is the expected outcome under
    // `cargo test`, and it is the only outcome in which this process has
    // handles to lose — Microsoft documents a successful `AllocConsole` as
    // initialising the process's standard handles for the new console, which is
    // precisely what is wanted in the case this call exists for, a test run
    // that started with no console at all.
    unsafe {
        AllocConsole();
    }
}

/// Sends a real Ctrl+C to the child's process group.
pub(crate) fn send_ctrl_c(child: &Child) {
    assert!(
        try_send_ctrl_c(child),
        "GenerateConsoleCtrlEvent failed: {}",
        std::io::Error::last_os_error()
    );
}

/// Sends a real Ctrl+C to the child's process group, and says whether Windows
/// accepted it.
///
/// Separate from [`send_ctrl_c`] for the one caller that expects a refusal:
/// `tests/shutdown_command.rs` sends one to a **detached** child to show it
/// cannot arrive, and there the call failing is the point rather than a fault.
pub(crate) fn try_send_ctrl_c(child: &Child) -> bool {
    // SAFETY: `GenerateConsoleCtrlEvent` takes two integers by value. The group
    // id is the child's process id, which is a group id because the child was
    // created with CREATE_NEW_PROCESS_GROUP, so the event cannot reach this
    // process or the rest of the test run.
    let sent = unsafe { GenerateConsoleCtrlEvent(CTRL_C_EVENT, child.id()) };
    sent != 0
}

/// Waits for the child, killing it rather than hanging the suite for ever.
pub(crate) fn wait_for_exit(child: &mut Child, what: &str) -> ExitStatus {
    let deadline = Instant::now() + PATIENCE;
    loop {
        match child.try_wait().expect("the child can be polled") {
            Some(status) => return status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{what} was still running {PATIENCE:?} after it should have stopped");
            }
            None => thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// A path in the temporary directory that no other test will collide with.
pub(crate) fn unique_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "clipped-recorder-{label}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ))
}

/// The directory cargo put this test's binaries in.
fn target_profile_directory() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_clipped-recorder"))
        .parent()
        .expect("the recorder binary is in a directory")
        .to_path_buf()
}

/// The command that rebuilds the examples these tests drive.
const BUILD_EXAMPLES: &str = "cargo build -p clipped-recorder --examples";

/// Where cargo put an `examples/<name>.rs` binary.
///
/// Cargo exports the path of every *binary* target to an integration test but
/// not of an example, so they are found next to the binary instead.
///
/// # Why this checks the file's age
///
/// Selecting one test target — `cargo test -p clipped-recorder --test
/// supervision`, which is the command these modules document — narrows what
/// cargo builds to that target and the binaries it exports paths for. **It does
/// not build examples.** So an edit to `crates/ipc` produces a freshly built
/// test binary driving a fixture compiled from the code as it was yesterday,
/// and the run passes. For a suite whose whole subject is process supervision,
/// silently testing the wrong supervisor is the worst failure it could have,
/// and it has happened in review.
///
/// [`assert_fresh`] closes it: an example older than any source it is built
/// from fails with the command that fixes it, rather than passing on evidence
/// about a binary nobody meant to run.
pub(crate) fn example_binary(name: &str) -> PathBuf {
    let path = target_profile_directory()
        .join("examples")
        .join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    assert!(
        path.exists(),
        "the `{name}` example was not built at {}; run `{BUILD_EXAMPLES}`",
        path.display()
    );
    assert_fresh(&path, name);
    path
}

/// The workspace root, as this package's manifest locates it.
fn workspace_root() -> PathBuf {
    // Popped rather than joined with `..`, so that a failure message names
    // `…\clipped\crates\ipc\…` and not `…\apps\recorder\..\..\crates\ipc\…`.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("this package is two levels below the workspace root")
        .to_path_buf();
    assert!(
        root.join("Cargo.lock").exists(),
        "expected the workspace root two levels above {}, and found no Cargo.lock at {}",
        env!("CARGO_MANIFEST_DIR"),
        root.display()
    );
    root
}

/// Fails when `binary` is older than something it was built from.
///
/// What is scanned is exactly what an example in this package compiles from:
/// every crate of the workspace, this package's own library, its own source
/// file, and the manifests and lock file that decide which versions of
/// everything else come with them. `tests/` is deliberately *not* scanned —
/// editing the test that drives a fixture does not make the fixture stale, and a
/// check that said otherwise would cry wolf on the most ordinary change anybody
/// makes here.
///
/// Neither are the *other* examples, for the same reason and a sharper one: each
/// is a single file that compiles from none of the others, so adding one — as
/// `examples/cannot_be_loaded.rs` did — would otherwise declare every fixture
/// beside it stale until something unrelated caused it to be relinked, and
/// rebuilding would not clear it.
///
/// The comparison is against sources rather than against sibling artefacts on
/// purpose. Two binaries built by one `cargo` invocation are linked in whatever
/// order cargo felt like, so "the example is older than the test binary" would
/// be a coin toss; "the example is older than a file it was compiled from" is
/// never true of an example that was just built.
fn assert_fresh(binary: &Path, name: &str) {
    let built = modified(binary);
    let root = workspace_root();

    let mut newest: Option<(PathBuf, SystemTime)> = None;
    let mut consider = |path: PathBuf| {
        let Ok(modified) = path.metadata().and_then(|data| data.modified()) else {
            return;
        };
        if newest.as_ref().is_none_or(|(_, latest)| modified > *latest) {
            newest = Some((path, modified));
        }
    };

    for directory in [
        root.join("crates"),
        root.join("apps").join("recorder").join("src"),
    ] {
        walk(&directory, &mut consider);
    }
    for file in [
        root.join("apps")
            .join("recorder")
            .join("examples")
            .join(format!("{name}.rs")),
        root.join("Cargo.toml"),
        root.join("Cargo.lock"),
        root.join("apps").join("recorder").join("Cargo.toml"),
    ] {
        consider(file);
    }

    if let Some((source, changed)) = newest {
        assert!(
            changed <= built,
            "the `{name}` example at {} was built before {} was last changed, so this run would \
             be testing a fixture compiled from code that is no longer there.\n\
             Run `{BUILD_EXAMPLES}` (or `cargo test --workspace`, which builds examples) and try \
             again.\n\
             Selecting a single test target — `cargo test -p clipped-recorder --test NAME` — does \
             not build examples; that is the whole reason this check exists.",
            binary.display(),
            source.display()
        );
    }
}

/// When a file was last written, or a panic naming it.
fn modified(path: &Path) -> SystemTime {
    path.metadata()
        .and_then(|data| data.modified())
        .unwrap_or_else(|error| {
            panic!(
                "the age of {} could not be read, so its freshness cannot be checked: {error}",
                path.display()
            )
        })
}

/// Hands every file under `directory` to `visit`, depth first.
///
/// A directory that cannot be read is skipped rather than reported: this is a
/// freshness check over source that is normally all present, and a permission
/// error on one subdirectory is not a reason to fail a supervision test.
fn walk(directory: &Path, visit: &mut impl FnMut(PathBuf)) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => walk(&path, visit),
            Ok(kind) if kind.is_file() => visit(path),
            _ => {}
        }
    }
}

/// Where cargo put `examples/shutdown_fixture.rs`.
pub(crate) fn fixture_binary() -> PathBuf {
    example_binary("shutdown_fixture")
}

/// Ends a process that is not this process's child.
///
/// `Child::kill` is `TerminateProcess` on a handle cargo already owns; a
/// recorder started *by* a fixture is this process's grandchild and detached
/// from it, so there is no `Child` for it and the handle has to be opened by
/// identifier. That is the whole difference — what lands on the recorder is the
/// same `TerminateProcess`, with no notification, no destructors and no flush,
/// which is what makes it a kill rather than a stop
/// (`crates/muxer/tests/abrupt_termination.rs`).
///
/// A process that has already exited is not an error: that is one of the
/// outcomes this is used to arrange.
#[cfg(windows)]
pub(crate) fn terminate(process_id: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};

    // SAFETY: `OpenProcess` takes three integers by value and returns a handle
    // or null. Nothing is dereferenced.
    let handle = unsafe { OpenProcess(PROCESS_TERMINATE, 0, process_id) };
    if handle.is_null() {
        // Already gone, or never ours to end.
        return;
    }

    // SAFETY: `handle` was just opened with PROCESS_TERMINATE and is owned
    // solely here; the exit code is an integer by value.
    unsafe {
        TerminateProcess(handle, 1);
        CloseHandle(handle);
    }
}

/// Whether a process is still running.
///
/// Opened with the smallest right that answers the question:
/// `PROCESS_QUERY_LIMITED_INFORMATION` is enough to read an exit code and is
/// granted for a process this account owns even when the fuller query right is
/// not.
#[cfg(windows)]
pub(crate) fn is_running(process_id: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // SAFETY: as `terminate` above; three integers in, a handle or null out.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
    if handle.is_null() {
        return false;
    }

    let mut code: u32 = 0;
    // SAFETY: `handle` is live and owned here, and the out-parameter points at
    // a live local.
    let queried = unsafe { GetExitCodeProcess(handle, &raw mut code) };
    // SAFETY: closed exactly once, and not used afterwards.
    unsafe {
        CloseHandle(handle);
    }

    queried != 0 && code == STILL_ACTIVE as u32
}

/// The `video-pattern` test application, built beside this test's binaries.
///
/// It belongs to another package, so cargo does not export its path here. It is
/// found next to the recorder for the same reason the fixture above is, and a
/// missing one is reported with the command that builds it rather than as a
/// spawn failure several lines later.
pub(crate) fn video_pattern_binary() -> PathBuf {
    target_profile_directory().join(format!("video-pattern{}", std::env::consts::EXE_SUFFIX))
}

/// A running `video-pattern` window, and what it announced about itself.
///
/// # Why this is not `clipped_video_pattern::harness::TestApp`
///
/// It should be, and it cannot be. That harness does all of this and more, but
/// it lives in `clipped-video-pattern`, which the workspace's layering places
/// *above* `clipped-recorder` so that nothing in the product can depend on a
/// test application (`tests/integration/tests/workspace_layering.rs`). A
/// dev-dependency counts, so this package cannot name it at all.
///
/// What is duplicated is therefore kept to the minimum a recording test needs:
/// start the process, read the one `ready` line it prints, and make sure it is
/// gone afterwards. The *pattern* — the part that would be genuinely expensive
/// to reimplement, and the part `docs/testing.md` warns against copying — is
/// not touched here.
pub(crate) struct PatternApp {
    child: Child,
    process_id: u32,
    client: (u32, u32),
}

impl PatternApp {
    /// Starts the pattern application and waits for it to be on screen.
    ///
    /// It is placed on a display that is not the primary one, and it is
    /// topmost and never focused — the application's own defaults — so a test
    /// run does not take over the screen of whoever is at the keyboard
    /// (AGENTS.md section 25).
    pub(crate) fn start(fps: u32, seconds: u64) -> Self {
        let binary = video_pattern_binary();
        assert!(
            binary.exists(),
            "{} has not been built. Run `cargo build --workspace`, or `cargo build -p \
             clipped-video-pattern`, before this test.",
            binary.display()
        );

        let mut child = Command::new(&binary)
            .args([
                "--mode",
                "borderless",
                "--fps",
                &fps.to_string(),
                "--seconds",
                &seconds.to_string(),
                "--monitor",
                "auto",
            ])
            // Standard input is a pipe so that dropping it stops the
            // application, which is what stops it outliving the test.
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap_or_else(|error| panic!("{} could not be started: {error}", binary.display()));

        let stdout = child.stdout.take().expect("stdout was piped");
        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .expect("the pattern application announces itself before rendering");

        let client = client_size(&line);
        let process_id = child.id();
        Self {
            child,
            process_id,
            client,
        }
    }

    /// The process identifier, which is what `record --pid` is pointed at.
    ///
    /// A process rather than a title, so that two of these running at once
    /// cannot make the selector ambiguous.
    pub(crate) fn process_id(&self) -> u32 {
        self.process_id
    }

    /// The exact size of the pattern in physical pixels, which is what a
    /// borderless window's capture is.
    pub(crate) fn client_size(&self) -> (u32, u32) {
        self.client
    }

    /// Waits for the application to stop on its own.
    pub(crate) fn wait_for_exit(&mut self) {
        wait_for_exit(&mut self.child, "the pattern application");
    }
}

impl Drop for PatternApp {
    /// Whether the test passed or panicked, nothing of it may still be
    /// rendering on somebody's second monitor a minute later.
    fn drop(&mut self) {
        drop(self.child.stdin.take());

        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => thread::sleep(Duration::from_millis(20)),
                Err(_) => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The `client=WIDTHxHEIGHT` field of the application's `ready` line.
fn client_size(line: &str) -> (u32, u32) {
    let field = line
        .split_whitespace()
        .find_map(|field| field.strip_prefix("client="))
        .unwrap_or_else(|| panic!("the ready line has no client size: {line}"));
    let (width, height) = field
        .split_once('x')
        .unwrap_or_else(|| panic!("the client size is not WIDTHxHEIGHT: {field}"));
    (
        width.parse().expect("the client width is a number"),
        height.parse().expect("the client height is a number"),
    )
}

/// Reads a child's standard error on a thread of its own.
///
/// A thread, rather than reading after the child exits, because a child whose
/// standard error fills up blocks in `write` — and this one would then stop
/// recording, because the recorder logs to standard error as well as to its log
/// files (docs/logging.md).
pub(crate) fn read_stderr(child: &mut Child) -> Receiver<String> {
    let stderr = child.stderr.take().expect("stderr was piped");
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("recorder-stderr".to_owned())
        .spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        })
        .expect("a thread can be started to read the recorder's diagnostics");
    receiver
}

/// Everything the child wrote to standard error, once it has finished writing.
///
/// Waits for the reading thread to see end of file rather than taking what
/// happens to have arrived: the child exiting and the thread reading the last
/// bytes are separate events with nothing ordering them, and a summary line
/// lost to that race would fail a test for reasons that have nothing to do with
/// the recording.
pub(crate) fn collected_stderr(lines: &Receiver<String>) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut collected = String::new();
    loop {
        match lines.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(line) => {
                collected.push_str(&line);
                collected.push('\n');
            }
            Err(RecvTimeoutError::Disconnected) => return collected,
            Err(RecvTimeoutError::Timeout) => {
                panic!("the thread reading the recorder's diagnostics never finished")
            }
        }
    }
}

/// The recorder binary.
pub(crate) fn recorder_binary() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_clipped-recorder"))
}
