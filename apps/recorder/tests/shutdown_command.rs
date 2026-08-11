//! A **detached** recorder, asked over the protocol to exit, does.
//!
//! [Issue #220](https://github.com/wildware-uk/clipped/issues/220) is the case
//! `tests/ctrl_c.rs` cannot reach. A recorder started by the desktop
//! application is spawned `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`
//! (`crates/ipc/src/supervisor/platform.rs`), so it has no console, so
//! `CTRL_C_EVENT` cannot reach it — and before the `shutdown` command the only
//! way to end one was Task Manager. This file drives that recorder the way the
//! tray's Exit does and asserts it goes.
//!
//! Nothing here is simulated. It starts the built `clipped-recorder serve`
//! binary with the same creation flags the supervisor uses, opens the actual
//! named pipe with `clipped-ipc`'s own client, sends an actual `shutdown`, and
//! waits for the actual process.
//!
//! # What this covers and what it does not
//!
//! `ctrl_c.rs` is the precedent and the shape is deliberately the same: the
//! signal path end to end, exit code and all, with no GPU and no desktop, so it
//! runs on every machine and in CI. What it leaves to
//! `tests/record_end_to_end.rs` is the same half `ctrl_c.rs` leaves there —
//! a real recording of a real window, and the file it finishes — because that
//! needs hardware and is `#[ignore]`d. The shutdown *path* is identical either
//! way: `clipped-ipc` stops the listener and `serve` then runs the exact
//! sequence Ctrl+C runs, which is the whole point of doing it that way
//! (`apps/recorder/src/serve.rs`).
//!
//! # Endpoints
//!
//! The pipe namespace is machine-wide, so every test names an endpoint of its
//! own. A test that used the default would be stopping whatever recorder the
//! person at the keyboard is running.

#![cfg(windows)]

mod support;

use std::io::{BufRead, BufReader};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use clipped_ipc::supervisor::wait_for_recorder_to_exit;
use clipped_ipc::{
    Client, ClientError, Command as IpcCommand, Endpoint, ErrorCode, Reply, Shutdown,
    StartRecording,
};

use support::{
    collected_stderr, ensure_console, is_running, read_stderr, recorder_binary, try_send_ctrl_c,
    wait_for_exit, CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS,
};

/// How long a client waits for a recorder that is starting.
const PATIENCE: Duration = Duration::from_secs(10);

/// How long the endpoint is given to disappear once a shutdown is accepted.
///
/// The same order as the desktop application's own wait, and for the same
/// reason: what it covers is finalising a container, which `docs/muxing.md`
/// measures in hundreds of milliseconds, on a machine that may be busy.
const GOING_AWAY: Duration = Duration::from_secs(20);

/// How long a recorder is watched for, to show that it did *not* go.
///
/// The one bound in this file that is a wait rather than a deadline, because
/// the assertion it serves is a negative one. Short enough not to be felt in a
/// test run and long enough to cover the whole of a shutdown that had actually
/// started: a recorder with nothing recording has no file to finalise, so its
/// exit is as quick as it will ever be, and `a_detached_recorder_asked_over_the_protocol_to_exit_does_so_cleanly`
/// measures that at well under a second on this project's machine.
const UNMOVED: Duration = Duration::from_secs(2);

/// This test binary's name for itself in a handshake.
const CLIENT_NAME: &str = "clipped-shutdown-integration-test";

/// A `clipped-recorder serve` started the way the desktop application starts
/// one: no console, and a process group of its own.
struct DetachedRecorder {
    child: Child,
    endpoint: Endpoint,
    diagnostics: Receiver<String>,
}

impl DetachedRecorder {
    /// Starts a detached recorder and waits until it says it is listening.
    fn start(label: &str) -> Self {
        // Not because this test sends a Ctrl+C it expects to work — the whole
        // point is that one cannot arrive — but because
        // `a_detached_recorder_cannot_be_reached_by_ctrl_c` sends one to prove
        // it, and `GenerateConsoleCtrlEvent` needs a console to send from.
        ensure_console();

        let name = unique_endpoint_name(label);
        let mut child = Command::new(recorder_binary())
            .args(["serve", "--endpoint", &name])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // The two flags the supervisor uses, together. `DETACHED_PROCESS`
            // is the one under test; `CREATE_NEW_PROCESS_GROUP` is there
            // because the supervisor passes it and because it means a console
            // control event this test sends cannot reach `cargo test`.
            .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
            .spawn()
            .expect("the recorder binary can be started");

        let diagnostics = read_stderr(&mut child);
        let stdout = child.stdout.take().expect("stdout was piped");
        let endpoint = read_ready_line(stdout, &name);

        Self {
            child,
            endpoint,
            diagnostics,
        }
    }

    /// A control connection, handshaken.
    fn client(&self) -> Client {
        Client::connect(&self.endpoint, CLIENT_NAME, "0.0.0", PATIENCE)
            .expect("the recorder that just said it was listening accepts a connection")
    }
}

impl Drop for DetachedRecorder {
    /// A recorder must not outlive the test that started it, whether that test
    /// passed, failed or panicked part way through — and this one is detached,
    /// so nothing else is going to end it.
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[test]
fn a_detached_recorder_asked_over_the_protocol_to_exit_does_so_cleanly() {
    let mut recorder = DetachedRecorder::start("exit");
    let endpoint = recorder.endpoint.clone();
    let mut client = recorder.client();

    let reply = client
        .call(&IpcCommand::Shutdown(Shutdown {
            finalise_recording: false,
        }))
        .expect("an idle recorder accepts a shutdown");
    assert_eq!(
        reply,
        Reply::ShuttingDown { finalising: None },
        "nothing was being recorded, so nothing is named as being finished"
    );
    // The reply arrives before the recorder winds up, and the connection is
    // dropped here rather than left open: a client still holding one is the
    // case `docs/ipc.md` promises does not delay the shutdown.
    drop(client);

    assert!(
        wait_for_recorder_to_exit(&endpoint, GOING_AWAY),
        "the endpoint should have gone away, which is the last thing a recorder does"
    );

    let status = wait_for_exit(&mut recorder.child, "the detached recorder");
    let diagnostics = collected_stderr(&recorder.diagnostics);
    assert!(
        status.success(),
        "a shutdown over the protocol should end the process cleanly, not leave it failing; exit \
         status was {status}.\n{diagnostics}"
    );
    assert!(
        diagnostics.contains("a client asked the recorder to finish and exit"),
        "the recorder should have taken the protocol's shutdown path rather than ended some other \
         way:\n{diagnostics}"
    );
}

#[test]
fn a_detached_recorder_cannot_be_reached_by_ctrl_c() {
    // The premise of issue #220, measured rather than asserted in prose. If a
    // console control event *could* reach a detached recorder there would be no
    // need for the `shutdown` command at all, and the test above would be
    // covering a path nobody needs.
    let mut recorder = DetachedRecorder::start("ctrl-c");
    let endpoint = recorder.endpoint.clone();

    // Deliberately unchecked. `GenerateConsoleCtrlEvent` reports success for a
    // group id in either case; what differs is whether anything receives it,
    // and that is what the wait below measures.
    try_send_ctrl_c(&recorder.child);

    // Patience of its own, because this is a negative assertion. A recorder
    // that *did* receive the event would take about as long to go as one asked
    // over the protocol does, so looking straight away would pass against one
    // already on its way out — which is exactly the mistake this is here to
    // catch.
    assert!(
        !wait_for_recorder_to_exit(&endpoint, UNMOVED),
        "a Ctrl+C that cannot be delivered must not have stopped anything"
    );
    assert!(is_running(recorder.child.id()));

    let mut client = recorder.client();
    assert_eq!(
        client
            .call(&IpcCommand::Ping)
            .expect("the recorder answers"),
        Reply::Pong,
        "and it must still be serving afterwards, not merely still a process"
    );

    let _ = client.call(&IpcCommand::Shutdown(Shutdown {
        finalise_recording: false,
    }));
    drop(client);
    wait_for_exit(&mut recorder.child, "the detached recorder");
}

#[test]
fn a_recorder_that_has_accepted_a_shutdown_refuses_to_start_a_recording() {
    // The other half of "exit must not end a recording nobody was asked about".
    // The permission a shutdown carries is decided by reading the status, and
    // seven other connections are being served while that happens — so once one
    // is accepted, nothing new may start. Driven here against a real recorder
    // because `crates/ipc`'s own test drives it against a byte buffer, and a
    // refusal that only exists in the dispatcher is not one a client can rely
    // on.
    let mut recorder = DetachedRecorder::start("no-new-recordings");
    let mut exiting = recorder.client();
    // A second connection, because that is the case: the shutdown and the
    // start arrive on different ones.
    let mut other = recorder.client();

    exiting
        .call(&IpcCommand::Shutdown(Shutdown {
            finalise_recording: false,
        }))
        .expect("an idle recorder accepts a shutdown");

    match other.call(&IpcCommand::StartRecording(StartRecording::default())) {
        Err(ClientError::Refused(refusal)) => assert_eq!(
            refusal.code,
            ErrorCode::ShuttingDown,
            "a recorder on its way out must not start a recording: {}",
            refusal.message
        ),
        // The recorder may have finished exiting before this reached it, which
        // is the same guarantee arriving by another route: the connection
        // broke, and no recording was started.
        Err(_) => {}
        Ok(reply) => panic!("a recorder that is exiting started a recording: {reply:?}"),
    }

    drop(exiting);
    drop(other);
    wait_for_exit(&mut recorder.child, "the detached recorder");
}

/// An endpoint name no other test, and no recorder anybody is using, will have.
fn unique_endpoint_name(label: &str) -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!(
        "clipped-shutdown-test.{label}.{}.{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    )
}

/// Reads the one line `serve` writes to standard output.
fn read_ready_line(stdout: std::process::ChildStdout, expected_name: &str) -> Endpoint {
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .expect("the recorder announces its endpoint before serving");

    let announced = line
        .trim()
        .strip_prefix("ready endpoint=")
        .unwrap_or_else(|| panic!("the recorder's first line should announce an endpoint: {line}"));
    assert!(
        announced.ends_with(expected_name),
        "the recorder should be listening where it was told to: {announced}"
    );

    Endpoint::named(expected_name).expect("the generated endpoint name is valid")
}
