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
use std::time::{Duration, Instant};

use windows_sys::Win32::System::Console::{AllocConsole, GenerateConsoleCtrlEvent, CTRL_C_EVENT};

/// <https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags>
pub(crate) const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

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
    // SAFETY: `GenerateConsoleCtrlEvent` takes two integers by value. The group
    // id is the child's process id, which is a group id because the child was
    // created with CREATE_NEW_PROCESS_GROUP, so the event cannot reach this
    // process or the rest of the test run.
    let sent = unsafe { GenerateConsoleCtrlEvent(CTRL_C_EVENT, child.id()) };
    assert_ne!(
        sent,
        0,
        "GenerateConsoleCtrlEvent failed: {}",
        std::io::Error::last_os_error()
    );
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

/// Where cargo put `examples/shutdown_fixture.rs`.
///
/// Cargo exports the path of every *binary* target to an integration test but
/// not of an example, so it is found next to the binary instead. `cargo test`
/// builds examples, so it is there by the time this runs.
pub(crate) fn fixture_binary() -> PathBuf {
    let path = target_profile_directory()
        .join("examples")
        .join(format!("shutdown_fixture{}", std::env::consts::EXE_SUFFIX));
    assert!(
        path.exists(),
        "the shutdown fixture was not built at {}; run `cargo test`, which builds examples",
        path.display()
    );
    path
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
