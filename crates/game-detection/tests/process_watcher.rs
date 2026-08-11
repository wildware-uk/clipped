//! The process watcher against real processes.
//!
//! The debounce is unit tested against constructed process trees, where the
//! interesting cases can be built exactly. What those tests cannot show is that
//! Windows ever says anything at all — that the subscription is correct, that
//! the properties are read from the right place, that a process this test
//! starts is noticed and that its exit is noticed too. That is what this file
//! is for, and it is why it uses a subject it controls completely rather than
//! an installed game (AGENTS.md sections 25 and 26).
//!
//! # The subject
//!
//! `cmd.exe` with its standard input piped and nothing else attached. It is on
//! every Windows machine, it starts immediately, and it exits when its input
//! closes — so the test knows the exact moment the process began and the exact
//! moment it ended, which is what makes the latency it prints a measurement
//! rather than an impression.

#![cfg(windows)]

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use clipped_game_detection::{Next, ProcessWatcher, WatchConfig, WatchEvent};

/// How long a test waits for an event before giving up.
///
/// Generous on purpose. The worst case for a launch is one notification
/// interval plus one quiet period — two and a half seconds with the default
/// settings — and this runs on continuous integration hardware that is shared,
/// virtualised and sometimes very slow. A tight bound here would fail for
/// reasons that have nothing to do with the code (AGENTS.md section 25).
const PATIENCE: Duration = Duration::from_secs(30);

/// Starts a process this test can end when it chooses.
fn start_subject() -> Child {
    Command::new("cmd.exe")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("cmd.exe is present on every Windows machine")
}

/// Ends the subject and waits for Windows to agree that it has gone.
fn end_subject(mut subject: Child) {
    // Closing its standard input is how `cmd.exe` is asked to leave: it reads
    // end of file and exits of its own accord, so this is an ordinary exit and
    // not a kill.
    drop(subject.stdin.take());
    subject
        .wait()
        .expect("the subject exits when its input closes");
}

/// Waits for an event about `pid`, or reports how long it waited in vain.
fn wait_for(
    watcher: &mut ProcessWatcher,
    pid: u32,
    mut matches: impl FnMut(&WatchEvent, u32) -> bool,
) -> Duration {
    let start = Instant::now();
    while start.elapsed() < PATIENCE {
        let event = match watcher.next_event(Duration::from_millis(250)) {
            Next::Event(event) => event,
            Next::Idle => continue,
            Next::Finished => panic!("the watcher had already finished"),
        };
        if let WatchEvent::Stopped(reason) = &event {
            panic!("the watcher lost every source it had: {reason}");
        }
        if matches(&event, pid) {
            return start.elapsed();
        }
    }

    panic!("no event about process {pid} arrived within {PATIENCE:?}");
}

fn launch_contains(event: &WatchEvent, pid: u32) -> bool {
    match event {
        WatchEvent::Launched(launch) => launch.processes.iter().any(|process| process.pid == pid),
        _ => false,
    }
}

fn exit_of(event: &WatchEvent, pid: u32) -> bool {
    match event {
        WatchEvent::Exited(exit) => exit.process.pid == pid,
        _ => false,
    }
}

#[test]
fn a_process_that_starts_is_reported_with_its_executable_and_its_parent() {
    let mut watcher = ProcessWatcher::start(WatchConfig::default())
        .expect("a process watcher can be started on this machine");
    println!("source: {}", watcher.source());
    if let Some(declined) = watcher.declined_source() {
        println!("the preferred source was declined: {declined}");
    }

    let subject = start_subject();
    let pid = subject.id();
    let started = Instant::now();

    let mut reported = None;
    let latency = wait_for(&mut watcher, pid, |event, pid| {
        if let WatchEvent::Launched(launch) = event {
            if let Some(process) = launch.processes.iter().find(|process| process.pid == pid) {
                reported = Some(process.clone());
                return true;
            }
        }
        false
    });
    println!(
        "launch detected in {latency:?} (started {:?} ago)",
        started.elapsed()
    );

    let reported = reported.expect("the matching launch was captured");
    assert_eq!(reported.pid, pid);
    assert_eq!(
        reported.parent_pid,
        std::process::id(),
        "the parent identifier must be this test process, which started it"
    );
    assert_eq!(reported.image_name.to_lowercase(), "cmd.exe");
    assert!(
        reported
            .image_path
            .as_ref()
            .is_some_and(|path| path.is_absolute()),
        "expected an absolute executable path, got {:?}",
        reported.image_path
    );

    end_subject(subject);
}

#[test]
fn a_process_that_exits_is_reported_gone_with_what_it_was() {
    let mut watcher = ProcessWatcher::start(WatchConfig::default())
        .expect("a process watcher can be started on this machine");

    let subject = start_subject();
    let pid = subject.id();
    wait_for(&mut watcher, pid, launch_contains);

    end_subject(subject);
    let ended = Instant::now();

    let mut reported = None;
    let latency = wait_for(&mut watcher, pid, |event, pid| {
        if let WatchEvent::Exited(exit) = event {
            if exit.process.pid == pid {
                reported = Some(exit.clone());
                return true;
            }
        }
        false
    });
    println!(
        "exit detected in {latency:?} (ended {:?} ago)",
        ended.elapsed()
    );

    let reported = reported.expect("the matching exit was captured");
    assert_eq!(
        reported.process.image_name.to_lowercase(),
        "cmd.exe",
        "an exit carries what the process was, which nothing could look up now"
    );
    assert!(
        !reported.launch.is_already_running(),
        "this process was observed starting, so it belongs to a launch"
    );
}

#[test]
fn the_watcher_knows_what_was_already_running_before_it_started() {
    let subject = start_subject();
    let pid = subject.id();

    let watcher = ProcessWatcher::start(WatchConfig::default())
        .expect("a process watcher can be started on this machine");

    let already = watcher
        .already_running()
        .iter()
        .find(|process| process.pid == pid)
        .expect("a process running before the watcher started is in its baseline");
    assert_eq!(already.image_name.to_lowercase(), "cmd.exe");
    assert!(
        watcher
            .already_running()
            .iter()
            .any(|process| process.pid == std::process::id()),
        "the test process itself was running too"
    );

    drop(watcher);
    end_subject(subject);
}

#[test]
fn a_process_that_comes_and_goes_inside_the_debounce_window_is_never_reported() {
    // The debounce rule that matters most in practice, exercised against a real
    // process rather than a constructed one: a launcher that hands over and
    // disappears, or any of the short-lived helpers a machine starts every few
    // seconds, must not reach the caller at all.
    let mut watcher = ProcessWatcher::start(WatchConfig::default())
        .expect("a process watcher can be started on this machine");

    let subject = start_subject();
    let pid = subject.id();
    end_subject(subject);

    // Long enough for a launch to have been reported twice over.
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        let event = match watcher.next_event(Duration::from_millis(250)) {
            Next::Event(event) => event,
            Next::Idle => continue,
            Next::Finished => panic!("the watcher lost every source it had"),
        };
        assert!(
            !launch_contains(&event, pid) && !exit_of(&event, pid),
            "a process that lived less than the debounce window was reported: {event:?}"
        );
    }
}
