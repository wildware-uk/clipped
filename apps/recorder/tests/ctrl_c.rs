//! Ctrl+C reaches the shutdown seam, and the finalisation hook runs.
//!
//! This is the half of acceptance criterion 3 of
//! [issue #9](https://github.com/wildware-uk/clipped/issues/9) that can be
//! satisfied without a capture engine. It sends a genuine `CTRL_C_EVENT` to a
//! genuine child process — no in-process simulation of the signal — and asserts
//! that the process finalised and exited cleanly rather than being killed.
//!
//! The remaining half, "produces a playable file", needs the capture, encoder
//! and muxer work of milestone M1 and is not asserted here or anywhere else.
//!
//! # How the signal is delivered
//!
//! `GenerateConsoleCtrlEvent` sends to a *process group*. Spawning the fixture
//! with `CREATE_NEW_PROCESS_GROUP` puts it in a group of its own, so the event
//! reaches it and not `cargo test`. The cost is that Ctrl+C starts out disabled
//! in the child, which is why the fixture re-enables it.

#![cfg(windows)]

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use std::os::windows::process::CommandExt;

use windows_sys::Win32::System::Console::{AllocConsole, GenerateConsoleCtrlEvent, CTRL_C_EVENT};

/// <https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags>
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// How long the child is given to install its handler, and then to shut down.
///
/// Generous on purpose. The assertion is "this happens", not "this happens
/// quickly", and a bound tight enough to trip on a loaded machine is a failure
/// nobody can tell from a real one (AGENTS.md section 25).
const PATIENCE: Duration = Duration::from_secs(30);

#[test]
fn ctrl_c_runs_the_finalisation_hook_and_exits_cleanly() {
    ensure_console();

    let marker = unique_path("ctrl-c-marker");
    let _ = std::fs::remove_file(&marker);

    let mut child = Command::new(fixture_binary())
        .arg(&marker)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .creation_flags(CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .expect("the shutdown fixture can be started");

    wait_until_ready(&mut child);
    send_ctrl_c(&child);

    let status = wait_for_exit(&mut child);
    assert!(
        status.success(),
        "Ctrl+C should end the process cleanly, not kill it; exit status was {status}"
    );

    let reason = std::fs::read_to_string(&marker).unwrap_or_else(|error| {
        panic!(
            "the finalisation hook should have written {}: {error}",
            marker.display()
        )
    });
    assert_eq!(
        reason, "interrupted",
        "the hook should have been told why the run ended"
    );

    let _ = std::fs::remove_file(&marker);
}

#[test]
fn the_fixture_reports_its_usage_rather_than_panicking_without_a_marker_path() {
    // Cheap, but it keeps the fixture itself honest: a fixture that panics on a
    // bad invocation makes the test above hard to diagnose when it breaks.
    let status = Command::new(fixture_binary())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("the shutdown fixture can be started");
    assert!(!status.success());
}

/// Where cargo put `examples/shutdown_fixture.rs`.
///
/// Cargo exports the path of every *binary* target to an integration test but
/// not of an example, so it is found next to the binary instead. `cargo test`
/// builds examples, so it is there by the time this runs.
fn fixture_binary() -> PathBuf {
    let recorder = PathBuf::from(env!("CARGO_BIN_EXE_clipped-recorder"));
    let path = recorder
        .parent()
        .expect("the recorder binary is in a directory")
        .join("examples")
        .join(format!("shutdown_fixture{}", std::env::consts::EXE_SUFFIX));
    assert!(
        path.exists(),
        "the shutdown fixture was not built at {}; run `cargo test`, which builds examples",
        path.display()
    );
    path
}

/// A path in the temporary directory that no other test will collide with.
fn unique_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "clipped-recorder-{label}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ))
}

/// Makes sure this process has a console, so it can address one.
///
/// `cargo test` normally provides one. When it does not — a test run started
/// from a process with no console — `GenerateConsoleCtrlEvent` would fail with
/// no console to send to, and the fixture would inherit nothing to receive on,
/// so the console has to exist before the child is spawned.
fn ensure_console() {
    // SAFETY: `AllocConsole` takes no arguments and returns a BOOL. It either
    // attaches a console to this process or fails because one is already
    // attached; there is no pointer to get wrong and no state it can leave
    // half-initialised, so the call is sound in both outcomes. The result is
    // deliberately ignored: "already had one" is the expected outcome under
    // `cargo test`, and it is the only outcome in which this process has
    // handles to lose — Microsoft documents a successful `AllocConsole` as
    // initialising the process's standard handles for the new console, which
    // is precisely what is wanted in the case this call exists for, a test run
    // that started with no console at all.
    unsafe {
        AllocConsole();
    }
}

/// Blocks until the fixture says its handler is installed.
fn wait_until_ready(child: &mut Child) {
    let stdout = child.stdout.take().expect("stdout was piped");
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .expect("the fixture announces itself before waiting");
    assert_eq!(
        line.trim(),
        "ready",
        "the fixture should announce that its handler is installed"
    );
}

/// Sends a real Ctrl+C to the child's process group.
fn send_ctrl_c(child: &Child) {
    // SAFETY: `GenerateConsoleCtrlEvent` takes two integers by value. The
    // group id is the child's process id, which is a group id because the child
    // was created with CREATE_NEW_PROCESS_GROUP, so the event cannot reach this
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
fn wait_for_exit(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + PATIENCE;
    loop {
        match child.try_wait().expect("the child can be polled") {
            Some(status) => return status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "the fixture was still running {PATIENCE:?} after Ctrl+C: \
                     the handler did not stop it"
                );
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}
