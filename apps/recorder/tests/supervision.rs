//! The recorder as a supervised independent process, against real processes
//! that are really killed.
//!
//! [Issue #106](https://github.com/wildware-uk/clipped/issues/106) has three
//! acceptance criteria and every one of them is about a process ending:
//!
//! 1. killing the UI during a recording leaves the recording intact and
//!    continuing;
//! 2. killing the recorder leaves a playable file and the UI reports the failure
//!    accurately;
//! 3. a second launch of the application does not start a competing recorder.
//!
//! None of the three can be shown in one process. A Rust panic unwinds and runs
//! destructors, and destructors are exactly what a killed process does not get;
//! an in-process "kill" would therefore prove the opposite of what it claimed.
//! So the supervisor here is a real child process
//! (`examples/supervised_ui_fixture.rs`), the recorder is the real built binary
//! started detached by that supervisor, and both are ended with a real
//! `TerminateProcess` — the same instrument
//! `crates/muxer/tests/abrupt_termination.rs` uses, and for the same reason.
//!
//! # What runs where
//!
//! Everything here needs a named pipe and some processes and nothing more,
//! **except** the two tests at the bottom: those record a real window with a
//! real encoder and read the file back, so they need a GPU and a desktop session
//! and are `#[ignore]`d, exactly like `tests/record_end_to_end.rs`.
//!
//! ```text
//! cargo test -p clipped-recorder --test supervision
//! cargo test -p clipped-recorder --test supervision -- --ignored --nocapture --test-threads=1
//! ```
//!
//! The tests that run everywhere cover each criterion's process behaviour — the
//! recorder survives its supervisor, the second launch starts nothing, the loss
//! is reported and the retrying is bounded — and the `#[ignore]`d two add the
//! half that needs a recording: that the file is playable and that the recording
//! carried on.
//!
//! # Endpoints and instance names
//!
//! The pipe namespace and the kernel object namespace are both machine-wide, so
//! every test names its own endpoint and its own instance. A test that used the
//! defaults would be talking to the recorder the person at the keyboard is
//! running.

#![cfg(windows)]

mod support;

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use clipped_ipc::{Client, Command as IpcCommand, Endpoint, RecorderStatus, Reply, StopRecording};

use support::{example_binary, is_running, recorder_binary, terminate};

/// How long a client waits for a recorder that is starting.
const PATIENCE: Duration = Duration::from_secs(10);

/// How long a test waits for a line it expects the supervisor to print.
///
/// Generous on purpose, for the reason `support::PATIENCE` is: the assertion is
/// "this happens", not "this happens quickly", and a bound tight enough to trip
/// on a loaded machine is a failure nobody can tell from a real one
/// (AGENTS.md section 25).
const LINE_PATIENCE: Duration = Duration::from_secs(30);

/// This test binary's name for itself in a handshake.
const CLIENT_NAME: &str = "clipped-supervision-test";

/// A running `supervised_ui_fixture`, standing in for the desktop application.
///
/// Its standard output is piped and read on a thread of its own, so a test can
/// wait for a particular line without racing the fixture's buffering. Its
/// *recorder* is piped nowhere: the fixture starts that detached with its
/// streams pointed at nothing, which is the behaviour under test.
struct SupervisorFixture {
    child: Child,
    lines: Receiver<String>,
    /// Every line read so far, in order.
    seen: Vec<String>,
    /// How far the waits below have consumed `seen`.
    ///
    /// A cursor rather than a search of everything, so that "wait for the next
    /// `attached` state" means the next one rather than the one already
    /// asserted about.
    cursor: usize,
    /// Recorders this fixture started, to be ended when it is.
    ///
    /// They are held *here* rather than in the test because the order matters
    /// and a panicking test does not get to choose it: a supervisor that is
    /// still watching when its recorder is terminated does exactly what it is
    /// built to do and starts a replacement, and that replacement is nobody's
    /// child and nothing else would ever end it. Ending the fixture first, in
    /// one destructor, is the only ordering that holds however a test finishes.
    recorders: Vec<u32>,
}

impl SupervisorFixture {
    /// Starts the fixture with the arguments given.
    fn start(arguments: &[&str]) -> Self {
        let mut child = Command::new(example_binary("supervised_ui_fixture"))
            .args(arguments)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("the supervisor fixture can be started");

        let stdout = child.stdout.take().expect("stdout was piped");
        Self {
            child,
            lines: read_lines(stdout),
            seen: Vec::new(),
            cursor: 0,
            recorders: Vec::new(),
        }
    }

    /// Records a recorder for this fixture to end when it does, and returns it.
    fn owns_recorder(&mut self, process_id: u32) -> u32 {
        self.recorders.push(process_id);
        process_id
    }

    /// Waits for the next line, from the cursor onwards, that `wanted` accepts.
    ///
    /// `what` is what the failure message says was being waited for; a test that
    /// hangs says nothing to whoever broke the behaviour, and this one fails
    /// with a sentence and a transcript.
    fn wait_until(&mut self, what: &str, wanted: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + LINE_PATIENCE;

        loop {
            while self.cursor < self.seen.len() {
                let line = self.seen[self.cursor].clone();
                self.cursor += 1;
                if wanted(&line) {
                    return line;
                }
            }

            match self
                .lines
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(line) => self.seen.push(line),
                Err(_) => panic!(
                    "the supervisor never printed {what}. What it did print:\n{}",
                    self.transcript()
                ),
            }
        }
    }

    /// Waits for the next line beginning with `prefix`, and returns the rest.
    fn wait_for(&mut self, prefix: &str) -> String {
        let owned = prefix.to_owned();
        let line = self.wait_until(&format!("a line starting `{prefix}`"), |line| {
            line.starts_with(&owned)
        });
        line.split_at(prefix.len()).1.to_owned()
    }

    /// Waits for the next line containing `needle`.
    fn wait_for_containing(&mut self, needle: &str) -> String {
        let owned = needle.to_owned();
        self.wait_until(&format!("a line containing `{needle}`"), |line| {
            line.contains(&owned)
        })
    }

    /// Waits for an `attached` state naming a recorder other than `previous`.
    fn wait_for_a_recorder_other_than(&mut self, previous: u32) -> u32 {
        loop {
            let line = self.wait_for_containing(r#""attached""#);
            if let Some(process_id) = attached_recorder(&line) {
                if process_id != previous {
                    return process_id;
                }
            }
        }
    }

    /// Reads whatever has arrived without waiting for more.
    fn drain(&mut self) {
        while let Ok(line) = self.lines.try_recv() {
            self.seen.push(line);
        }
    }

    /// Whether any line read so far begins with `prefix`.
    fn ever_printed(&self, prefix: &str) -> bool {
        self.seen.iter().any(|line| line.starts_with(prefix))
    }

    /// How many lines read so far contain `needle`.
    fn count_containing(&self, needle: &str) -> usize {
        self.seen
            .iter()
            .filter(|line| line.contains(needle))
            .count()
    }

    /// Waits for the fixture to exit and returns its status.
    fn wait_for_exit(&mut self) -> ExitStatus {
        support::wait_for_exit(&mut self.child, "the supervisor fixture")
    }

    /// Ends the fixture the way a crash would: no notification, no destructors.
    fn kill(&mut self) {
        self.child.kill().expect("the fixture can be terminated");
        self.child.wait().expect("the fixture can be waited for");
    }

    /// Everything the fixture has printed so far, for a failure message.
    fn transcript(&self) -> String {
        if self.seen.is_empty() {
            return "  (nothing)".to_owned();
        }
        self.seen
            .iter()
            .map(|line| format!("  {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Drop for SupervisorFixture {
    /// Neither a supervisor nor a recorder may outlive the test that started
    /// it, whether that test passed, failed or panicked part way through.
    ///
    /// The supervisor goes first, for the reason `recorders` gives.
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }

        for recorder in &self.recorders {
            terminate(*recorder);
        }
    }
}

/// Reads a stream line by line on a thread of its own.
fn read_lines(stdout: ChildStdout) -> Receiver<String> {
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("supervisor-fixture-stdout".to_owned())
        .spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        })
        .expect("a thread can be started to read the fixture's output");
    receiver
}

/// An endpoint or instance name no other test, and nothing anybody is using,
/// will have.
fn unique_name(label: &str) -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!(
        "clipped-supervision-test.{label}.{}.{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    )
}

/// The recorder binary, as an argument.
fn recorder_argument() -> String {
    recorder_binary().to_string_lossy().into_owned()
}

/// The `pid=N` field of a `recorder=` line.
fn recorder_process_id(line: &str) -> u32 {
    line.split_whitespace()
        .find_map(|field| field.strip_prefix("pid="))
        .unwrap_or_else(|| panic!("the recorder line names no process: {line}"))
        .parse()
        .unwrap_or_else(|error| panic!("the process identifier is not a number: {line} ({error})"))
}

/// The recorder identifier inside an `attached` link state.
fn attached_recorder(line: &str) -> Option<u32> {
    let json = line.strip_prefix("link=")?;
    let state: serde_json::Value = serde_json::from_str(json).ok()?;
    state.get("recorder_process_id")?.as_u64()?.try_into().ok()
}

/// A control connection to the recorder at `endpoint`.
fn client(endpoint: &Endpoint) -> Client {
    Client::connect(endpoint, CLIENT_NAME, "0.0.0", PATIENCE)
        .unwrap_or_else(|error| panic!("no recorder is reachable at {endpoint}: {error}"))
}

#[test]
fn a_recorder_outlives_the_process_that_started_it() {
    // Acceptance criterion 1's process half, and the foundation of the other
    // two: the supervisor is killed outright — `TerminateProcess`, no
    // notification, no destructors, which is what a webview fault does — and the
    // recorder it started is still there afterwards and still serving.
    let name = unique_name("outlives");
    let mut supervisor =
        SupervisorFixture::start(&["--endpoint", &name, "--recorder", &recorder_argument()]);

    let started = supervisor.wait_for("recorder=");
    assert!(
        started.starts_with("started "),
        "nothing was listening, so the supervisor should have started a recorder: {started}"
    );
    let recorder = supervisor.owns_recorder(recorder_process_id(&started));
    supervisor.wait_for("ready");

    supervisor.kill();

    // Not merely a process that has not exited yet: it is answering the
    // protocol, which is the only thing "the recording continues" could ever be
    // built on.
    let endpoint = Endpoint::named(&name).expect("the generated name is valid");
    let mut surviving = client(&endpoint);
    assert_eq!(
        surviving.call(&IpcCommand::Ping).expect("ping is answered"),
        Reply::Pong,
        "the recorder must survive the process that started it (ADR 0002)"
    );
    assert_eq!(
        surviving
            .recorder_process_id()
            .expect("the pipe names its server"),
        recorder,
        "it should be the same recorder, not a replacement"
    );
    assert!(
        is_running(recorder),
        "the recorder should still be running after its supervisor was killed"
    );
}

#[test]
fn a_second_launch_attaches_to_the_running_recorder_rather_than_starting_another() {
    // Acceptance criterion 3, without the single-instance guard: even two
    // supervisors that both believe they should have a recorder end up talking
    // to one. `ensure_recorder` attaches by preference rather than starting
    // first and asking afterwards, and the endpoint is what would stop it
    // anyway (`FILE_FLAG_FIRST_PIPE_INSTANCE`).
    let name = unique_name("attach");
    let recorder_path = recorder_argument();

    let mut first = SupervisorFixture::start(&["--endpoint", &name, "--recorder", &recorder_path]);
    let started = first.wait_for("recorder=");
    assert!(started.starts_with("started "), "{started}");
    let recorder = first.owns_recorder(recorder_process_id(&started));
    first.wait_for("ready");

    let mut second = SupervisorFixture::start(&["--endpoint", &name, "--recorder", &recorder_path]);
    let attached = second.wait_for("recorder=");

    assert!(
        attached.starts_with("existing "),
        "the second launch should have found the first launch's recorder rather than starting \
         one: {attached}"
    );
    assert_eq!(
        recorder_process_id(&attached),
        recorder,
        "both supervisors should be talking to the same recorder"
    );
}

#[test]
fn a_second_launch_holding_the_same_instance_name_starts_no_recorder_at_all() {
    // Acceptance criterion 3 as it is written. The strongest form of "does not
    // start a competing recorder" is a second launch that never reaches the
    // recorder at all, so the assertion is deliberately about absence: a second
    // launch which attached and then exited would satisfy a weaker test and
    // would still have been a second supervisor for a moment.
    let name = unique_name("instance");
    let instance = unique_name("instance-name");
    let recorder_path = recorder_argument();

    let mut first = SupervisorFixture::start(&[
        "--endpoint",
        &name,
        "--recorder",
        &recorder_path,
        "--instance",
        &instance,
    ]);
    first.wait_for("instance=claimed");
    let started = recorder_process_id(&first.wait_for("recorder="));
    let recorder = first.owns_recorder(started);
    first.wait_for("ready");

    let mut second = SupervisorFixture::start(&[
        "--endpoint",
        &name,
        "--recorder",
        &recorder_path,
        "--instance",
        &instance,
    ]);

    assert_eq!(
        second.wait_for("instance="),
        "already-running",
        "the second launch should have found the first holding the name"
    );
    let status = second.wait_for_exit();
    assert!(
        status.success(),
        "a second launch is an ordinary thing for a user to do, not an error: {status}"
    );

    second.drain();
    assert!(
        !second.ever_printed("recorder="),
        "the second launch touched the recorder before exiting:\n{}",
        second.transcript()
    );

    // And the first one is undisturbed.
    let endpoint = Endpoint::named(&name).expect("the generated name is valid");
    assert_eq!(
        client(&endpoint)
            .recorder_process_id()
            .expect("the pipe names its server"),
        recorder
    );
}

#[test]
fn a_recorder_that_is_killed_is_reported_and_replaced_rather_than_silently_missing() {
    // Acceptance criterion 2's reporting half, without a recording: the
    // supervisor must not go on showing the state of a recorder that is gone
    // (AGENTS.md section 27). It says the link ended, says which recorder it
    // was, and the replacement it attaches to is visibly a different process.
    let name = unique_name("replaced");
    let mut supervisor = SupervisorFixture::start(&[
        "--endpoint",
        &name,
        "--recorder",
        &recorder_argument(),
        // Short, so the test is not several seconds of sleeping.
        "--restart-delay-ms",
        "200",
    ]);

    let started = recorder_process_id(&supervisor.wait_for("recorder="));
    let first = supervisor.owns_recorder(started);
    supervisor.wait_for("ready");
    let attached = supervisor.wait_for_containing(r#""attached""#);
    assert_eq!(
        attached_recorder(&attached),
        Some(first),
        "the supervisor should be attached to the recorder it started: {attached}"
    );

    terminate(first);

    let reconnecting = supervisor.wait_for_containing(r#""reconnecting""#);
    assert!(
        reconnecting.contains(&first.to_string()),
        "the reason should name the recorder that went: {reconnecting}"
    );

    let replacement = supervisor.wait_for_a_recorder_other_than(first);
    supervisor.owns_recorder(replacement);
    assert_ne!(
        replacement, first,
        "a replacement is a different process, and the state has to show that"
    );
    assert!(
        is_running(replacement),
        "the replacement should be a recorder that is actually running"
    );
}

#[test]
fn a_recorder_that_will_not_start_is_given_up_on_rather_than_retried_for_ever() {
    // A restart loop is worse than staying down: it burns CPU beside a game,
    // fills a log with one line and never tells the user anything. The policy is
    // bounded, and the state it ends in says what happened.
    let name = unique_name("hopeless");
    let mut supervisor = SupervisorFixture::start(&[
        "--endpoint",
        &name,
        // A "recorder" that exits at once without listening, every time.
        "--recorder",
        &example_binary("never_listens").to_string_lossy(),
        "--restart-attempts",
        "2",
        "--restart-delay-ms",
        "50",
    ]);

    let reported = supervisor.wait_for("recorder=");
    assert!(
        reported.starts_with("error "),
        "a recorder that never listens is not an attachment: {reported}"
    );
    assert!(
        reported.contains("42"),
        "the failure should carry the exit status the recorder produced: {reported}"
    );

    let unavailable = supervisor.wait_for_containing(r#""unavailable""#);
    assert!(
        unavailable.contains("2 attempts"),
        "the state should say how many attempts were made: {unavailable}"
    );

    // Exactly two, and no third. Both halves matter: a policy that stopped after
    // one attempt would pass a test that only checked there was a bound.
    supervisor.drain();
    assert_eq!(
        supervisor.count_containing(r#""reconnecting""#),
        2,
        "two attempts were allowed:\n{}",
        supervisor.transcript()
    );
}

#[test]
fn a_supervisor_pointed_at_no_recorder_at_all_says_so_at_once() {
    // A missing executable does not appear because it was asked for again, so it
    // is not retried: the user is told immediately rather than four backoff
    // delays later (`supervisor::link::worth_trying_again`).
    let name = unique_name("missing");
    let absent = std::env::temp_dir().join("clipped-no-such-recorder.exe");
    let mut supervisor = SupervisorFixture::start(&[
        "--endpoint",
        &name,
        "--recorder",
        &absent.to_string_lossy(),
        // Generous, so that a policy which retried anyway would take far longer
        // than the bound below and fail rather than pass slowly.
        "--restart-attempts",
        "4",
        "--restart-delay-ms",
        "5000",
    ]);

    let started = Instant::now();
    let unavailable = supervisor.wait_for_containing(r#""unavailable""#);
    let waited = started.elapsed();

    assert!(
        unavailable.contains("clipped-no-such-recorder.exe"),
        "the message should name the path it looked in: {unavailable}"
    );
    assert!(
        waited < Duration::from_secs(5),
        "a failure no retry could fix should be reported at once, and took {waited:?}"
    );

    supervisor.drain();
    assert_eq!(
        supervisor.count_containing(r#""reconnecting""#),
        0,
        "nothing should have been retried:\n{}",
        supervisor.transcript()
    );
}

/// The rate the pattern application presents at, matching
/// `tests/record_end_to_end.rs`.
const SOURCE_FPS: u32 = 30;

/// How long a recording runs before the process under test is killed.
const RECORD_FOR: Duration = Duration::from_secs(4);

#[test]
#[ignore = "needs a GPU, an encoder and a desktop session; see the module docs"]
fn killing_the_supervisor_leaves_the_recording_running_and_its_file_playable() {
    // Acceptance criterion 1 in full. A real window, a real recording started
    // over the protocol, and the process that started it ended with
    // `TerminateProcess` while the recording runs. The recording must not
    // notice.
    let Some(_tools) = clipped_media_validation::require_media_tools() else {
        return;
    };

    let directory = clipped_media_validation::TemporaryDirectory::new("supervision-ui-killed");
    let output = directory.file("survived.mkv");
    let pattern = support::PatternApp::start(SOURCE_FPS, 120);

    let name = unique_name("ui-killed");
    let mut supervisor = SupervisorFixture::start(&[
        "--endpoint",
        &name,
        "--recorder",
        &recorder_argument(),
        "--record-pid",
        &pattern.process_id().to_string(),
        "--output",
        &output.to_string_lossy(),
    ]);

    let started = recorder_process_id(&supervisor.wait_for("recorder="));
    supervisor.owns_recorder(started);
    let recording_id = supervisor
        .wait_for("recording=")
        .split_whitespace()
        .next()
        .expect("the recording line names the recording")
        .to_owned();
    supervisor.wait_for("ready");

    thread::sleep(RECORD_FOR / 2);
    supervisor.kill();

    // Still running, and still getting longer: a recorder that had stopped
    // capturing but kept answering would report a frozen elapsed time and would
    // pass a test that only asked "are you recording?".
    let endpoint = Endpoint::named(&name).expect("the generated name is valid");
    let mut client = client(&endpoint);
    let first = elapsed_of(&mut client, &recording_id);
    thread::sleep(RECORD_FOR / 2);
    let second = elapsed_of(&mut client, &recording_id);
    assert!(
        second > first,
        "the recording stopped advancing when its supervisor was killed: {first} then {second} ms"
    );

    let summary = match client
        .call(&IpcCommand::StopRecording(StopRecording {
            recording_id: Some(recording_id),
        }))
        .expect("the recording stops")
    {
        Reply::RecordingStopped { summary } => summary,
        other => panic!("expected a summary, got {other:?}"),
    };

    eprintln!(
        "\n=== a recording that outlived its supervisor ===\n\
         supervisor     : killed {:?} in\n\
         frames encoded : {}\n\
         picture        : {}x{} {}\n\
         duration       : {} ms\n\
         file           : {}\n",
        RECORD_FOR / 2,
        summary.frames_encoded,
        summary.width,
        summary.height,
        summary.codec,
        summary.duration_ms,
        summary.output,
    );

    assert!(
        summary.duration_ms >= RECORD_FOR.as_millis() as u64 / 2,
        "the recording should cover the time after the kill as well: {summary:?}"
    );

    clipped_media_validation::Media::open(&output)
        .unwrap_or_else(|error| panic!("the recording is not usable at all: {error}"))
        .validate()
        .stream_count(1)
        .video_stream_count(1)
        .video(
            clipped_media_validation::VideoStream::codec(&summary.codec)
                .resolution(summary.width, summary.height)
                // The recorder's own count and the decoder's are two
                // independent accounts of the same recording.
                .decoded_frames(summary.frames_encoded),
        )
        .monotonic_timestamps()
        .assert_valid();
}

#[test]
#[ignore = "needs a GPU, an encoder and a desktop session; see the module docs"]
fn killing_the_recorder_leaves_a_playable_file_and_the_supervisor_names_it() {
    // Acceptance criterion 2 in full, and the case that is genuinely different
    // from `tests/record_end_to_end.rs`: a `CTRL_C_EVENT` runs the finalisation
    // hook and `TerminateProcess` does not, so what is under test here is
    // Matroska's own recoverability (ADR 0001, docs/muxing.md) rather than a
    // clean shutdown. And the supervisor has to say what happened rather than
    // showing an idle recorder.
    let Some(_tools) = clipped_media_validation::require_media_tools() else {
        return;
    };

    let directory = clipped_media_validation::TemporaryDirectory::new("supervision-killed");
    let output = directory.file("interrupted.mkv");
    let pattern = support::PatternApp::start(SOURCE_FPS, 120);

    let name = unique_name("recorder-killed");
    let mut supervisor = SupervisorFixture::start(&[
        "--endpoint",
        &name,
        "--recorder",
        &recorder_argument(),
        "--record-pid",
        &pattern.process_id().to_string(),
        "--output",
        &output.to_string_lossy(),
        // No replacement, so what is asserted is what the supervisor said about
        // the loss rather than how quickly it papered over it.
        "--restart-attempts",
        "0",
    ]);

    let started = recorder_process_id(&supervisor.wait_for("recorder="));
    let recorder = supervisor.owns_recorder(started);
    supervisor.wait_for("recording=");
    supervisor.wait_for("ready");
    supervisor.wait_for_containing(r#""state":"recording""#);

    thread::sleep(RECORD_FOR);
    terminate(recorder);

    // The supervisor names the file and says it was interrupted.
    let interrupted = supervisor.wait_for("interrupted=");
    let reported: serde_json::Value =
        serde_json::from_str(&interrupted).expect("the interruption is reported as JSON");
    let named = PathBuf::from(
        reported["output"]
            .as_str()
            .unwrap_or_else(|| panic!("the interruption should name a file: {interrupted}")),
    );
    assert_eq!(
        named, output,
        "the supervisor should name the file the recorder was writing"
    );

    // And it does not claim the recorder is doing anything.
    let unavailable = supervisor.wait_for_containing(r#""unavailable""#);
    assert!(
        unavailable.contains(&recorder.to_string()),
        "the state should say which recorder went: {unavailable}"
    );

    // The file the supervisor named — not one the test knew about — is opened,
    // because "we told the user where it is" is only worth anything if what is
    // there is a recording.
    let media = clipped_media_validation::Media::open(&named).unwrap_or_else(|error| {
        panic!("a recorder killed mid-write must still leave a playable file (ADR 0001): {error}")
    });

    eprintln!(
        "\n=== the file a killed recorder left ===\n{}\n",
        media.inventory()
    );

    media
        .validate()
        .stream_count(1)
        .video_stream_count(1)
        .monotonic_timestamps()
        .assert_valid();
    assert!(
        !media.packets().is_empty(),
        "a file with no packets is not a recording of anything:\n{}",
        media.inventory()
    );
}

/// How long the recorder says the named recording has been running.
fn elapsed_of(client: &mut Client, recording_id: &str) -> u64 {
    match client
        .call(&IpcCommand::GetStatus)
        .expect("status is answered")
    {
        Reply::Status {
            status: RecorderStatus::Recording(active),
        } => {
            assert_eq!(active.recording_id, recording_id);
            active.elapsed_ms
        }
        other => panic!("the recorder should still be recording, not {other:?}"),
    }
}
