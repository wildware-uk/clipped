//! The built executable, run as the host runs it.
//!
//! Everything else in `tests/` is the derivation, which is a pure function.
//! This is the other half: the process actually starting, speaking the contract
//! in `docs/plugin-api.md`, surviving an API that is not there, and going away
//! when its standard input closes.
//!
//! **What it does not test is a match.** Nothing is listening on
//! `127.0.0.1:2999` while this runs, so what is exercised is the unhappy path —
//! which is the one worth having automatically, because it is the state the
//! plugin is in for the whole of a loading screen and the whole of a machine
//! where League is not running. That the happy path produces events is what
//! `tests/live_api_payloads.rs` shows from payloads, and what only a real match
//! can show end to end.

#![cfg(windows)]

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use clipped_plugins::{read_command, HostCommand, PluginReport, CONTRACT};

/// How long any one thing here waits before the test has failed.
///
/// Generous: this starts a process, and the plugin's own poll interval is a
/// second. A hung test is a worse failure than a slow one, so nothing here
/// waits forever.
const PATIENCE: Duration = Duration::from_secs(15);

/// The plugin, started and attached, with its reports arriving on a channel.
struct RunningPlugin {
    child: Child,
    reports: Receiver<String>,
}

impl RunningPlugin {
    /// Starts the executable and writes the `attach` command the host writes.
    fn attach() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_clipped-league-plugin"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("the plugin executable should start");

        // The line the host writes, built by the host's own type so that this
        // test cannot drift from the wire it is testing.
        let attach = read_command(
            r#"{"command":"attach","contract":1,
                "session":{"session":"plugin-contract-test",
                           "process":{"executable":"League of Legends.exe","process_id":1}}}"#,
        )
        .expect("the attach line is what the host sends");
        assert!(matches!(attach, HostCommand::Attach { contract, .. } if contract == CONTRACT));
        child
            .stdin
            .as_mut()
            .expect("stdin was piped")
            .write_all(attach.to_line().as_bytes())
            .expect("the plugin should accept the attach command");

        let stdout = child.stdout.take().expect("stdout was piped");
        Self {
            child,
            reports: read_lines(stdout),
        }
    }

    /// The next report, or a failure naming what was being waited for.
    fn next_report(&self, waiting_for: &str) -> PluginReport {
        match self.reports.recv_timeout(PATIENCE) {
            Ok(line) => clipped_plugins::read_report(&line)
                .unwrap_or_else(|error| panic!("`{line}` should be a report: {error}")),
            Err(RecvTimeoutError::Timeout) => {
                panic!("waited {PATIENCE:?} for {waiting_for} and the plugin said nothing")
            }
            Err(RecvTimeoutError::Disconnected) => {
                panic!("the plugin stopped while waiting for {waiting_for}")
            }
        }
    }
}

impl Drop for RunningPlugin {
    fn drop(&mut self) {
        // A test that failed part way through must not leave a process behind.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Reads a pipe on a thread, because a pipe has no timed read.
fn read_lines(stdout: ChildStdout) -> Receiver<String> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    receiver
}

#[test]
fn it_says_hello_before_it_does_anything_that_can_fail() {
    // The host counts a plugin that has not introduced itself as still
    // starting, and gives up on it after a few seconds
    // (`SupervisionPolicy::hello_timeout`). Opening an HTTPS client first and
    // saying hello afterwards would make that a race with the machine's network
    // stack, so `hello` comes first and this is what holds it there.
    let plugin = RunningPlugin::attach();
    assert_eq!(
        plugin.next_report("the first thing it says"),
        PluginReport::Hello { contract: CONTRACT }
    );
}

#[test]
fn an_api_that_is_not_there_costs_the_heartbeat_nothing() {
    // Nothing is listening on 127.0.0.1:2999 while this test runs, which is the
    // state of every machine that is not in a match. The plugin must keep
    // saying it is alive through it: a plugin that went quiet because the game
    // had not started would be killed and restarted for the whole of a loading
    // screen (`docs/plugin-api.md`, "Supervision and restart").
    let plugin = RunningPlugin::attach();
    assert!(matches!(
        plugin.next_report("hello"),
        PluginReport::Hello { .. }
    ));

    for beat in 1..=2 {
        assert_eq!(
            plugin.next_report("a heartbeat"),
            PluginReport::Alive,
            "heartbeat {beat}: an unreachable API is not a reason to stop reporting"
        );
    }
}

#[test]
fn it_leaves_when_its_standard_input_closes() {
    // Which is how a session ends, and also how a host that died is noticed.
    // A plugin that outlived the recorder would be a process nobody owns.
    let mut plugin = RunningPlugin::attach();
    assert!(matches!(
        plugin.next_report("hello"),
        PluginReport::Hello { .. }
    ));

    drop(plugin.child.stdin.take());

    let deadline = Instant::now() + PATIENCE;
    loop {
        match plugin.child.try_wait().expect("the child can be waited on") {
            Some(status) => {
                assert!(
                    status.success(),
                    "the plugin should leave quietly: {status}"
                );
                return;
            }
            None => assert!(
                Instant::now() < deadline,
                "the plugin was still running {PATIENCE:?} after its standard input closed"
            ),
        }
        thread::sleep(Duration::from_millis(50));
    }
}
