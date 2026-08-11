//! Ctrl+C reaches the shutdown seam, and the finalisation hook runs.
//!
//! This is the half of acceptance criterion 3 of
//! [issue #9](https://github.com/wildware-uk/clipped/issues/9) that needs
//! neither a GPU nor a desktop session, so it runs on every machine and in CI.
//! It sends a genuine `CTRL_C_EVENT` to a genuine child process — no in-process
//! simulation of the signal — and asserts that the process finalised and exited
//! cleanly rather than being killed.
//!
//! The other half, "produces a playable file", is
//! `tests/record_end_to_end.rs`: the same signal, sent to a real recording of a
//! real window, with the file it leaves behind decoded afterwards
//! ([issue #126](https://github.com/wildware-uk/clipped/issues/126)). That one
//! needs a GPU and a desktop and is `#[ignore]`d; this one is what keeps the
//! signal path covered when it is not run.
//!
//! # How the signal is delivered
//!
//! `GenerateConsoleCtrlEvent` sends to a *process group*. Spawning the fixture
//! with `CREATE_NEW_PROCESS_GROUP` puts it in a group of its own, so the event
//! reaches it and not `cargo test`. The cost is that Ctrl+C starts out disabled
//! in the child, which is why the fixture — and the recorder itself — calls
//! `clipped_recorder::shutdown::allow_ctrl_c`.
//!
//! # Running it on its own
//!
//! ```text
//! cargo build -p clipped-recorder --examples
//! cargo test  -p clipped-recorder --test ctrl_c
//! ```
//!
//! The build line first, because selecting one test target does not build
//! examples and `examples/shutdown_fixture.rs` is what this drives.
//! `support::example_binary` refuses one older than the code it was built from,
//! rather than letting a run report on a binary nobody meant to test.

#![cfg(windows)]

mod support;

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

use std::os::windows::process::CommandExt;

use support::{
    ensure_console, fixture_binary, send_ctrl_c, unique_path, wait_for_exit,
    CREATE_NEW_PROCESS_GROUP,
};

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

    let status = wait_for_exit(&mut child, "the shutdown fixture");
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
