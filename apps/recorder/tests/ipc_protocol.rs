//! The IPC protocol, driven against a real `clipped-recorder serve` process.
//!
//! [Issue #49](https://github.com/wildware-uk/clipped/issues/49)'s second
//! acceptance criterion is "an integration test drives a real recorder process
//! over the protocol", and the word doing the work is *real*. Everything here
//! starts the built binary as a child process, opens the actual named pipe,
//! exchanges actual frames and stops it with an actual `CTRL_C_EVENT`. Nothing
//! is mocked, no transport is simulated in-process, and the client is
//! `clipped-ipc`'s own — the one the desktop application will use — rather than
//! a second implementation written to agree with the server.
//!
//! `clipped-ipc`'s unit tests cover the same handshake and dispatch against a
//! byte buffer, which is where the exhaustive cases live. What these add is
//! everything a buffer cannot show: that the pipe is really created and really
//! reachable, that the endpoint is exclusive, that a client which vanishes
//! leaves the recorder serving, and that Ctrl+C ends the process cleanly.
//!
//! # Why these are not `#[ignore]`d
//!
//! They need no GPU, no display and no encoder — a named pipe and a child
//! process are all a recorder needs to answer `ping` — so they run in CI, which
//! is where a protocol regression should be caught. The one test that does need
//! hardware is marked, at the bottom of the file.
//!
//! # Endpoints
//!
//! The pipe namespace is machine-wide, so every test names an endpoint of its
//! own. A test that used the default would be talking to whatever recorder the
//! person at the keyboard is running, which is exactly why `serve --endpoint`
//! exists.

#![cfg(windows)]

mod support;

use std::io::{BufRead, BufReader};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use clipped_ipc::frame::{read_message, write_message};
use clipped_ipc::transport::connect;
use clipped_ipc::{
    features, Client, ClientError, ClientMessage, Command as IpcCommand, ConnectionRole, Endpoint,
    ErrorCode, ErrorDetail, Event, EventClient, EventStream, Hello, HotkeyBinding, PeerIdentity,
    RecorderStatus, Reply, ServerMessage, StartRecording, StopRecording,
    MAX_CONCURRENT_CONNECTIONS, PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS, UNBUILT_COMMANDS,
};

use support::{
    collected_stderr, ensure_console, read_stderr, recorder_binary, send_ctrl_c, wait_for_exit,
    CREATE_NEW_PROCESS_GROUP,
};

/// How long a client waits for a recorder that is starting.
const PATIENCE: Duration = Duration::from_secs(10);

/// This test binary's name for itself in a handshake.
const CLIENT_NAME: &str = "clipped-ipc-integration-test";

/// A running `clipped-recorder serve`, and the endpoint it announced.
///
/// Started in a process group of its own so that a `CTRL_C_EVENT` reaches it
/// and not `cargo test`, exactly as `tests/ctrl_c.rs` does — the recorder asks
/// Ctrl+C back on for itself, which is what makes stopping it a real signal
/// rather than a kill.
struct ServedRecorder {
    child: Child,
    endpoint: Endpoint,
    diagnostics: Receiver<String>,
}

impl ServedRecorder {
    /// Starts a recorder and waits until it says it is listening.
    fn start(label: &str) -> Self {
        Self::start_under(label, None)
    }

    /// The same, with the recorder's idea of this user's directories pointed
    /// somewhere of the test's own.
    ///
    /// `%LOCALAPPDATA%` is where the library index, the logs and the game
    /// overlay live, and `%USERPROFILE%` is what the recordings folder hangs
    /// off. A test that indexes anything has to move both, or it would walk the
    /// recordings of whoever is running it and write to their library
    /// (AGENTS.md section 25).
    fn start_under(label: &str, home: Option<&Path>) -> Self {
        ensure_console();

        let name = unique_endpoint_name(label);
        let mut command = Command::new(recorder_binary());
        command.args(["serve", "--endpoint", &name]);
        if let Some(home) = home {
            command
                .env("USERPROFILE", home)
                .env("LOCALAPPDATA", home.join("AppData").join("Local"));
        }
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NEW_PROCESS_GROUP)
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

    /// Where it is listening.
    fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// A control connection, handshaken.
    fn client(&self) -> Client {
        Client::connect(&self.endpoint, CLIENT_NAME, "0.0.0", PATIENCE)
            .expect("the recorder that just said it was listening accepts a connection")
    }

    /// Stops the recorder with a real Ctrl+C and asserts it exited cleanly.
    fn stop(mut self) -> String {
        send_ctrl_c(&self.child);
        let status = wait_for_exit(&mut self.child, "the recorder");
        let diagnostics = collected_stderr(&self.diagnostics);

        assert!(
            status.success(),
            "Ctrl+C should stop a serving recorder cleanly, not kill it; exit status was \
             {status}.\n{diagnostics}"
        );
        diagnostics
    }
}

impl Drop for ServedRecorder {
    /// A recorder must not outlive the test that started it, whether that test
    /// passed, failed or panicked part way through.
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// An endpoint name no other test, and no recorder anybody is using, will have.
fn unique_endpoint_name(label: &str) -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!(
        "clipped-recorder-test.{label}.{}.{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    )
}

/// Reads the one line `serve` writes to standard output.
fn read_ready_line(stdout: ChildStdout, expected_name: &str) -> Endpoint {
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

#[test]
fn a_client_handshakes_with_a_real_recorder_and_gets_answers_to_real_commands() {
    let recorder = ServedRecorder::start("handshake");
    let mut client = recorder.client();

    let welcome = client.welcome().clone();
    assert_eq!(welcome.protocol_version, PROTOCOL_VERSION);
    assert_eq!(welcome.recorder.name, "clipped-recorder");
    assert_eq!(welcome.role, ConnectionRole::Control);
    for feature in [features::RECORDING, features::STATUS_EVENTS] {
        assert!(
            welcome.features.iter().any(|have| have == feature),
            "a build that can record should say so: {welcome:?}"
        );
    }

    assert_eq!(
        client.call(&IpcCommand::Ping).expect("ping is answered"),
        Reply::Pong
    );

    match client
        .call(&IpcCommand::GetStatus)
        .expect("status is answered")
    {
        Reply::Status { status } => assert_eq!(
            status,
            RecorderStatus::Idle,
            "a recorder that was just started is not recording"
        ),
        other => panic!("expected a status, got {other:?}"),
    }

    drop(client);
    recorder.stop();
}

#[test]
fn a_version_the_recorder_does_not_speak_is_refused_with_the_ones_it_does() {
    // Acceptance criterion 3, against a real process rather than a buffer: not
    // undefined behaviour, not silence, and not a deserialisation failure three
    // messages later.
    let recorder = ServedRecorder::start("version");

    let mut connection =
        connect(recorder.endpoint(), PATIENCE).expect("the recorder accepts a connection");
    write_message(
        &mut connection,
        &ClientMessage::Hello(Hello {
            protocol_version: 9_999,
            client: PeerIdentity {
                name: CLIENT_NAME.to_owned(),
                version: "0.0.0".to_owned(),
            },
            role: ConnectionRole::Control,
            streams: Vec::new(),
        }),
    )
    .expect("the handshake is written");

    match read_message::<_, ServerMessage>(&mut connection).expect("the recorder answers") {
        ServerMessage::Refused(error) => {
            assert_eq!(error.code, ErrorCode::UnsupportedProtocolVersion);
            match error.detail {
                Some(ErrorDetail::UnsupportedProtocolVersion {
                    requested,
                    supported,
                    recorder_version,
                }) => {
                    assert_eq!(requested, 9_999);
                    assert_eq!(supported, SUPPORTED_PROTOCOL_VERSIONS.to_vec());
                    assert!(
                        !recorder_version.is_empty(),
                        "the user has to be told which side to update"
                    );
                }
                other => panic!("the refusal should say what the recorder speaks: {other:?}"),
            }
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    drop(connection);

    assert_still_serving(&recorder);
    recorder.stop();
}

#[test]
fn a_frame_that_is_not_a_message_is_refused_and_the_recorder_keeps_serving() {
    let recorder = ServedRecorder::start("malformed");

    let mut connection =
        connect(recorder.endpoint(), PATIENCE).expect("the recorder accepts a connection");
    let payload = br#"{"type":"hello","protocol_version":"#;
    let mut frame = (payload.len() as u32).to_le_bytes().to_vec();
    frame.extend_from_slice(payload);
    std::io::Write::write_all(&mut connection, &frame).expect("the bytes are written");

    match read_message::<_, ServerMessage>(&mut connection).expect("the recorder answers") {
        ServerMessage::Refused(error) => assert_eq!(error.code, ErrorCode::MalformedFrame),
        other => panic!("expected a refusal, got {other:?}"),
    }
    drop(connection);

    assert_still_serving(&recorder);
    recorder.stop();
}

#[test]
fn a_length_prefix_that_would_allocate_the_machine_is_refused() {
    // The recorder is the process that must not fall over (AGENTS.md section
    // 17), and its endpoint is reachable by anything running as the user. A
    // four-gigabyte length prefix must cost it nothing.
    let recorder = ServedRecorder::start("oversized");

    let mut connection =
        connect(recorder.endpoint(), PATIENCE).expect("the recorder accepts a connection");
    let mut frame = u32::MAX.to_le_bytes().to_vec();
    frame.extend_from_slice(b"and now four gigabytes of nothing");
    std::io::Write::write_all(&mut connection, &frame).expect("the bytes are written");

    match read_message::<_, ServerMessage>(&mut connection).expect("the recorder answers") {
        ServerMessage::Refused(error) => {
            assert_eq!(error.code, ErrorCode::MalformedFrame);
            assert!(
                error.message.contains("4294967295"),
                "the refusal should say what was announced: {}",
                error.message
            );
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    drop(connection);

    assert_still_serving(&recorder);
    recorder.stop();
}

#[test]
fn a_client_that_disappears_mid_request_leaves_the_recorder_serving() {
    // The desktop application is closed by the user at any moment, including
    // the moment between a command and its reply. That must cost the recorder
    // one connection and nothing else (ADR 0002).
    let recorder = ServedRecorder::start("vanishing");

    let mut deserter = recorder.client();
    // Written straight onto the connection rather than through `call`, because
    // `call` waits for the reply and the point is to leave without it.
    let request = IpcCommand::GetStatus
        .to_request(1)
        .expect("a status request can be built");
    let mut connection =
        connect(recorder.endpoint(), PATIENCE).expect("the recorder accepts a connection");
    write_message(
        &mut connection,
        &ClientMessage::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            client: PeerIdentity {
                name: CLIENT_NAME.to_owned(),
                version: "0.0.0".to_owned(),
            },
            role: ConnectionRole::Control,
            streams: Vec::new(),
        }),
    )
    .expect("the handshake is written");
    write_message(&mut connection, &ClientMessage::Request(request)).expect("the request is sent");
    drop(connection);

    assert_eq!(
        deserter.call(&IpcCommand::Ping).expect("ping is answered"),
        Reply::Pong,
        "another client's connection dying must not disturb this one"
    );

    assert_still_serving(&recorder);
    recorder.stop();
}

#[test]
fn every_command_whose_subsystem_is_not_built_is_refused_with_where_it_is_being_built() {
    // AGENTS.md sections 27 and 54: not silence, and not a success it did not
    // perform. The UI has to be able to say "not in this build" and point
    // somewhere.
    let recorder = ServedRecorder::start("unbuilt");
    let mut client = recorder.client();

    for unbuilt in UNBUILT_COMMANDS {
        let error = client
            .call_raw(unbuilt.name(), serde_json::json!({}))
            .expect_err("this build cannot do that");

        match error {
            ClientError::Refused(refusal) => {
                assert_eq!(
                    refusal.code,
                    ErrorCode::NotImplemented,
                    "{}",
                    unbuilt.name()
                );
                match refusal.detail {
                    Some(ErrorDetail::NotImplemented {
                        subsystem,
                        milestone,
                        tracking_issue,
                    }) => {
                        assert!(!subsystem.is_empty());
                        assert!(!milestone.is_empty());
                        assert_eq!(tracking_issue, unbuilt.tracking_issue());
                    }
                    other => panic!("{} should say where it is built: {other:?}", unbuilt.name()),
                }
            }
            other => panic!("{} should be refused, not {other}", unbuilt.name()),
        }
    }

    drop(client);
    recorder.stop();
}

#[test]
fn a_command_this_recorder_has_never_heard_of_is_refused_by_name() {
    let recorder = ServedRecorder::start("unknown-command");
    let mut client = recorder.client();

    match client.call_raw("summon_a_demon", serde_json::json!({})) {
        Err(ClientError::Refused(error)) => {
            assert_eq!(error.code, ErrorCode::UnknownCommand);
            assert!(
                error.message.contains("summon_a_demon"),
                "the refusal should name the command: {}",
                error.message
            );
        }
        other => panic!("expected a refusal, got {other:?}"),
    }

    // The connection survives an unknown command: it is the client being newer
    // than the recorder, not a broken peer.
    assert_eq!(
        client.call(&IpcCommand::Ping).expect("ping is answered"),
        Reply::Pong
    );

    drop(client);
    recorder.stop();
}

#[test]
fn a_status_subscription_opens_with_the_state_the_recorder_is_in() {
    let recorder = ServedRecorder::start("events");

    let mut events = EventClient::subscribe(
        recorder.endpoint(),
        CLIENT_NAME,
        "0.0.0",
        vec![EventStream::Status],
        PATIENCE,
    )
    .expect("the status stream is delivered");
    assert_eq!(events.streams(), [EventStream::Status]);

    match events.next_event().expect("an event arrives") {
        Event::StatusChanged { status } => assert_eq!(status, RecorderStatus::Idle),
        other => panic!("expected an opening status event, got {other:?}"),
    }

    drop(events);
    recorder.stop();
}

#[test]
fn the_metrics_stream_is_refused_rather_than_accepted_and_left_silent() {
    // Nothing measures live metrics during a recording yet. A subscription that
    // was accepted and then never delivered anything would be a UI showing an
    // empty graph with no explanation (AGENTS.md section 27).
    let recorder = ServedRecorder::start("metrics");

    let error = EventClient::subscribe(
        recorder.endpoint(),
        CLIENT_NAME,
        "0.0.0",
        vec![EventStream::Metrics],
        PATIENCE,
    )
    .expect_err("this build has no metrics to stream");

    match error {
        ClientError::Refused(refusal) => {
            assert_eq!(refusal.code, ErrorCode::NotImplemented);
            assert!(
                matches!(refusal.detail, Some(ErrorDetail::NotImplemented { .. })),
                "the refusal should say where metrics are being built: {refusal:?}"
            );
        }
        other => panic!("expected a refusal, got {other}"),
    }

    recorder.stop();
}

#[test]
fn the_connection_after_the_last_one_the_recorder_will_serve_is_refused_and_the_slot_comes_back() {
    // The cap exists because the endpoint is reachable by anything running as
    // the user, and an unbounded accept loop is an unbounded thread count
    // inside the process that must not fall over (AGENTS.md section 17). It is
    // the one refusal in the protocol whose whole point is a resource bound, so
    // both halves are asserted: that the cap holds, and that a connection
    // ending gives its place back rather than costing the recorder a slot for
    // the rest of its life.
    let recorder = ServedRecorder::start("connection-cap");

    let mut held = Vec::with_capacity(MAX_CONCURRENT_CONNECTIONS);
    for _ in 0..MAX_CONCURRENT_CONNECTIONS {
        let mut client = recorder.client();
        assert_eq!(
            client.call(&IpcCommand::Ping).expect("ping is answered"),
            Reply::Pong,
            "every connection up to the cap is served, not merely accepted"
        );
        held.push(client);
    }

    // Deliberately a bare connection rather than `Client::connect`: the cap is
    // applied at the accept, *before* a handshake is read, so the refusal is
    // already there for a client that has sent nothing at all. Reading the
    // frame directly is what asserts that, and it is also what keeps the
    // assertion honest — through `Client::connect` a refusal and a write that
    // lost the race to the closing pipe are the same failure.
    match beyond_the_cap(&recorder) {
        ServerMessage::Refused(error) => {
            assert_eq!(error.code, ErrorCode::TooManyConnections);
            assert!(
                error
                    .message
                    .contains(&MAX_CONCURRENT_CONNECTIONS.to_string()),
                "the refusal should say how many connections that is: {}",
                error.message
            );
        }
        other => panic!("expected a refusal, got {other:?}"),
    }

    // The ones already being served are untouched by somebody else being turned
    // away.
    assert_eq!(
        held[0].call(&IpcCommand::Ping).expect("ping is answered"),
        Reply::Pong,
        "a refused connection must not disturb the ones that were accepted"
    );

    // And a slot released is a slot reusable. The recorder notices the client
    // has gone on its own thread, so this is the one place the test has to
    // wait for something rather than assert it at once.
    held.pop();
    let mut replacement = connect_once_a_slot_is_free(&recorder);
    assert_eq!(
        replacement
            .call(&IpcCommand::Ping)
            .expect("ping is answered"),
        Reply::Pong,
        "a connection that ended should have given its place back"
    );

    drop(replacement);
    drop(held);
    recorder.stop();
}

/// Opens one connection more than the recorder will serve, and reads what it
/// says about it.
///
/// The read happens on a thread of its own because reads on this transport have
/// no deadline (`docs/ipc.md`): a recorder that accepted this connection instead
/// of refusing it would leave the read blocked for ever, and a test that hangs
/// says nothing to whoever broke the cap. This one fails with a sentence.
fn beyond_the_cap(recorder: &ServedRecorder) -> ServerMessage {
    let mut connection =
        connect(recorder.endpoint(), PATIENCE).expect("the pipe itself still accepts a connection");

    let (answers, answer) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = answers.send(
            read_message::<_, ServerMessage>(&mut connection).map_err(|error| error.to_string()),
        );
    });

    answer
        .recv_timeout(PATIENCE)
        .expect(
            "a connection past the cap should be refused at once; nothing arrived, so it was \
             accepted and left waiting",
        )
        .expect("the refusal should be a readable frame")
}

/// Connects, waiting while the recorder is still at its connection cap.
///
/// A connection's slot is released by the thread serving it, which notices the
/// client has gone at its own pace, so an attempt made immediately after one is
/// closed may still meet the cap. It gets [`PATIENCE`] to stop doing so, and
/// then the failure is the test's.
fn connect_once_a_slot_is_free(recorder: &ServedRecorder) -> Client {
    let deadline = std::time::Instant::now() + PATIENCE;
    loop {
        match Client::connect(recorder.endpoint(), CLIENT_NAME, "0.0.0", PATIENCE) {
            Ok(client) => return client,
            Err(error) if still_at_the_cap(&error) && std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("the freed slot was never usable again: {error}"),
        }
    }
}

/// Whether a failed handshake was the recorder saying it is full.
///
/// A capped connection is refused and closed at the accept, before the
/// handshake is read, so a client can lose the race between writing its `hello`
/// and the pipe closing under it — in which case it sees its own write fail
/// rather than the refusal. Both mean the same thing here.
fn still_at_the_cap(error: &ClientError) -> bool {
    match error {
        ClientError::Refused(refusal) => refusal.code == ErrorCode::TooManyConnections,
        ClientError::Frame(_) => true,
        _ => false,
    }
}

#[test]
fn a_second_recorder_on_the_same_endpoint_refuses_to_compete_with_the_first() {
    // Two recorders on one endpoint would be two processes racing to own the
    // same recording. The full single-instance behaviour is
    // [issue #106](https://github.com/wildware-uk/clipped/issues/106); what is
    // asserted here is only that the transport refuses rather than sharing.
    let recorder = ServedRecorder::start("exclusive");

    let second = Command::new(recorder_binary())
        .args(["serve", "--endpoint", recorder.endpoint().name()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("a second recorder can be started");

    assert!(
        !second.status.success(),
        "a second recorder on a taken endpoint should fail rather than serve"
    );
    let complaint = String::from_utf8_lossy(&second.stderr);
    assert!(
        complaint.contains("already listening"),
        "the second recorder should say why it stopped: {complaint}"
    );

    assert_still_serving(&recorder);
    recorder.stop();
}

/// The question issue #232 exists to answer: does a shipped recorder register
/// the global hotkeys at all.
///
/// `crates/hotkeys` had a full test suite and no production caller for two
/// milestones, and every one of those tests passed the whole time. So this one
/// deliberately asks nothing of the hotkey service and everything of the
/// recorder somebody actually runs: a real `serve`, over a real pipe, answering
/// the question the window asks. Deleting the registration from `serve::run`
/// leaves every unit test in this repository green and fails here.
#[test]
fn a_real_recorder_registers_the_global_hotkeys_and_says_where_each_one_stands() {
    let recorder = ServedRecorder::start("hotkeys");
    let mut client = recorder.client();

    assert!(
        client
            .welcome()
            .features
            .iter()
            .any(|have| have == features::HOTKEYS),
        "a recorder that registers hotkeys has to say so, or a window cannot tell it from one \
         built before it did: {:?}",
        client.welcome().features,
    );

    let hotkeys = hotkey_report(&mut client);

    // Every action, not only the bound ones: a screen sent a subset could not
    // offer the rest.
    let names: Vec<&str> = hotkeys.iter().map(|row| row.action.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "save_replay",
            "add_bookmark",
            "take_screenshot",
            "toggle_recording",
            "mute_microphone",
            "toggle_microphone",
            "open_overlay",
        ],
        "the report is the whole of SPEC.md section 34, in the order a screen lists it",
    );

    // Which combination each action has depends on the settings file of whoever
    // is running this, so nothing here asserts one. What does not depend on that
    // is which actions this build can perform — and that is the half a user
    // otherwise finds out about by pressing a key and watching nothing happen.
    let bookmark = row(&hotkeys, "add_bookmark");
    assert!(
        bookmark.handled && bookmark.unavailable.is_none(),
        "this recorder answers `add_bookmark`, so its row must not read as unavailable: \
         {bookmark:?}",
    );

    let save = row(&hotkeys, "save_replay");
    assert!(!save.handled, "no build saves a replay yet: {save:?}");
    let reason = save
        .unavailable
        .as_deref()
        .expect("an action nothing performs has to say why");
    assert!(
        reason.contains("Save replay") && reason.contains("M3") && reason.contains("#38"),
        "the refusal has to name the action, the milestone and the issue: {reason}",
    );

    drop(client);
    recorder.stop();
}

/// The third acceptance criterion of issue #232: a second copy of Clipped must
/// not silently take the user's hotkeys away.
///
/// It cannot, and the reason is an ordering rather than a lock of its own.
/// `serve` binds the endpoint before it registers anything, so a second recorder
/// in the same session has already exited by the time it could have asked
/// Windows for a combination — which is what makes the endpoint's exclusivity
/// (ADR 0005, ADR 0006) the hotkeys' exclusivity too.
///
/// The evidence is that the first recorder's report is the same afterwards as
/// before. A second recorder that had reached `RegisterHotKey` would have turned
/// the first's rows into conflicts or its own, and either would show here.
#[test]
fn a_second_recorder_never_reaches_the_first_ones_hotkeys() {
    let recorder = // Not "hotkeys": the label becomes the pipe's name, the name appears in
    // the second recorder's complaint, and the assertion below is about what
    // that complaint does *not* mention.
    ServedRecorder::start("second-copy");
    let mut client = recorder.client();
    let before = hotkey_report(&mut client);

    let second = Command::new(recorder_binary())
        .args(["serve", "--endpoint", recorder.endpoint().name()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("a second recorder can be started");

    assert!(
        !second.status.success(),
        "a second recorder on a taken endpoint should fail rather than serve"
    );
    let complaint = String::from_utf8_lossy(&second.stderr);
    assert!(
        complaint.contains("already listening"),
        "the second recorder should say why it stopped: {complaint}"
    );
    assert!(
        !complaint.contains("hotkey"),
        "a recorder that exited because the endpoint was taken should never have got as far as a \
         hotkey: {complaint}",
    );

    assert_eq!(
        hotkey_report(&mut client),
        before,
        "the second recorder cost the first one its hotkeys",
    );

    drop(client);
    recorder.stop();
}

/// Where every hotkey stands, as the recorder reports it.
fn hotkey_report(client: &mut Client) -> Vec<HotkeyBinding> {
    match client
        .call(&IpcCommand::GetHotkeys)
        .expect("a recorder that registered its hotkeys can say where they stand")
    {
        Reply::Hotkeys { hotkeys } => hotkeys,
        other => panic!("`get_hotkeys` was answered with {other:?}"),
    }
}

/// One action's row, or a failure naming the action that is missing.
fn row<'a>(hotkeys: &'a [HotkeyBinding], action: &str) -> &'a HotkeyBinding {
    hotkeys
        .iter()
        .find(|row| row.action == action)
        .unwrap_or_else(|| panic!("`{action}` should be in the report: {hotkeys:?}"))
}

#[test]
fn starting_a_recording_of_something_that_is_not_there_is_refused_by_the_real_handler() {
    // No GPU is needed to prove that `start_recording` reaches the recorder's
    // own target resolution rather than a stub: a process identifier nothing
    // owns is refused with `target_not_found`, and the recorder is still idle
    // afterwards.
    let recorder = ServedRecorder::start("no-such-target");
    let mut client = recorder.client();

    let error = client
        .call(&IpcCommand::StartRecording(StartRecording {
            // Chosen to be a process identifier no machine will have: Windows
            // allocates them as multiples of four and well below this.
            pid: Some(u32::MAX - 3),
            microphone: Some("none".to_owned()),
            system_audio: Some("none".to_owned()),
            ..StartRecording::default()
        }))
        .expect_err("there is no such window");

    match error {
        ClientError::Refused(refusal) => assert_eq!(refusal.code, ErrorCode::TargetNotFound),
        other => panic!("expected a refusal, got {other}"),
    }

    match client
        .call(&IpcCommand::GetStatus)
        .expect("status is answered")
    {
        Reply::Status { status } => assert_eq!(
            status,
            RecorderStatus::Idle,
            "a recording that never started must not leave the recorder claiming to record"
        ),
        other => panic!("expected a status, got {other:?}"),
    }

    // And stopping what was never started says so rather than pretending.
    match client.call(&IpcCommand::StopRecording(StopRecording::default())) {
        Err(ClientError::Refused(refusal)) => {
            assert_eq!(refusal.code, ErrorCode::NotRecording);
        }
        other => panic!("expected a refusal, got {other:?}"),
    }

    drop(client);
    recorder.stop();
}

/// How long the recording the export tests copy runs for.
const EXPORT_FIXTURE_SECONDS: &str = "3";

/// Builds a recording for the export tests with the pinned build's own
/// `ffmpeg`, and returns it.
///
/// Not recorded, because a recording needs a window, a GPU and an encoder, and
/// what an export does is copy coded packets between containers — which cares
/// about neither the picture nor how it got there. What it does care about is
/// the **shape** of a Clipped recording, and that is what this reproduces: one
/// picture track and one track of **uncompressed** sound, which is what Clipped
/// writes and what MP4 only gained a mapping for in FFmpeg 8 (`ipcm`,
/// `clipped_muxer::remux`). A fixture with AAC in it would exercise the easy
/// half and leave the interesting one untested.
///
/// This is the same program `clipped-media-validation` inspects with, used the
/// same way: as a test tool. Nothing in the recorder shells out to FFmpeg
/// (`docs/ffmpeg.md`).
fn recording_to_export(ffmpeg: &std::path::Path, into: &std::path::Path) {
    let output = Command::new(ffmpeg)
        .arg("-nostdin")
        .args([
            "-hide_banner",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=30",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            EXPORT_FIXTURE_SECONDS,
            "-c:v",
            "mpeg4",
            "-c:a",
            "pcm_s16le",
        ])
        .arg(into)
        .output()
        .expect("the pinned ffmpeg can be run");

    assert!(
        output.status.success(),
        "the recording to export could not be built: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Sends `export_recording` and returns what the recorder answered with.
fn export(client: &mut Client, source: &std::path::Path, destination: &std::path::Path) -> Reply {
    client
        .call(&IpcCommand::ExportRecording(clipped_ipc::ExportRecording {
            source: source.to_string_lossy().into_owned(),
            destination: destination.to_string_lossy().into_owned(),
        }))
        .unwrap_or_else(|error| panic!("the export was refused: {error}"))
}

#[test]
fn a_recording_exported_over_the_protocol_decodes_from_first_frame_to_last_and_keeps_its_sound() {
    // Issue #399's third and fourth acceptance criteria, over a real recorder
    // process and a real file. Everything here is asserted against the
    // **source**, measured from the same file in the same run, rather than
    // against numbers written down: an export that dropped every other frame
    // would satisfy a hard-coded count chosen to match it and cannot satisfy
    // the source's.
    //
    // `decoded_frames`, never a packet count on its own: a container can list
    // ninety packets, have monotonic timestamps and one video stream, and still
    // decode to nothing at all (`tests/media`).
    let Some(tools) = clipped_media_validation::require_media_tools() else {
        return;
    };

    let directory = clipped_media_validation::TemporaryDirectory::new("recorder-export");
    let source = directory.file("match.mkv");
    let destination = directory.file("match.mp4");
    recording_to_export(tools.ffmpeg(), &source);

    let recorded = clipped_media_validation::Media::open(&source).expect("the recording opens");
    let recorded_video = recorded.video_streams();
    let recorded_video = recorded_video.first().expect("the fixture has a picture");
    let video_codec = recorded_video
        .field("codec_name")
        .expect("the fixture's picture has a codec")
        .to_owned();
    let frames = recorded_video
        .number("nb_read_frames")
        .expect("the fixture's pictures can be counted") as u64;
    let seconds = recorded
        .duration_seconds()
        .expect("a finished recording records its duration");

    let recorder = ServedRecorder::start("export");
    let mut client = recorder.client();

    let summary = match export(&mut client, &source, &destination) {
        Reply::RecordingExported { export } => export,
        other => panic!("expected an export, got {other:?}"),
    };

    assert_eq!(summary.source, source.to_string_lossy());
    assert_eq!(summary.destination, destination.to_string_lossy());
    assert!(
        summary.packets > 0 && summary.bytes > 0,
        "an export that copied nothing is not an export: {summary:?}"
    );
    assert!(
        summary.lossless && summary.losses.is_empty(),
        "a recording of one picture and one uncompressed sound track fits in MP4 whole: \
         {summary:?}"
    );

    clipped_media_validation::Media::open(&destination)
        .unwrap_or_else(|error| panic!("the export is not usable at all: {error}"))
        .validate()
        .stream_count(2)
        .video_stream_count(1)
        .video(
            clipped_media_validation::VideoStream::codec(&video_codec)
                .resolution(320, 240)
                // The whole of "decodes cleanly from first frame to last": the
                // decoder is run over the MP4 and has to produce exactly the
                // pictures the recording holds, not merely some of them.
                .decoded_frames(frames),
        )
        .audio_stream_count(1)
        .audio(
            0,
            clipped_media_validation::AudioStream::codec("pcm_s16le")
                .sample_rate(48_000)
                .channels(1),
        )
        .duration_seconds(seconds, 0.2)
        .monotonic_timestamps()
        .assert_valid();

    // And the recording it was made from is untouched, which is the promise a
    // refusal makes and a success has to keep as well (AGENTS.md section 56).
    clipped_media_validation::Media::open(&source)
        .expect("the recording still opens")
        .validate()
        .stream_count(2)
        .video(clipped_media_validation::VideoStream::codec(&video_codec).decoded_frames(frames))
        .assert_valid();

    drop(client);
    recorder.stop();
}

#[test]
fn an_export_over_the_protocol_copies_the_recordings_coded_bytes_rather_than_re_encoding_them() {
    // Issue #399's fourth acceptance criterion — "a stream copy, not a
    // re-encode, verified rather than assumed" — and the only assertion that
    // can carry it. A duration, a frame count and a stream layout are all
    // satisfied by a file that went through an encoder and came out looking
    // worse; identical coded bytes are not.
    //
    // Separate from the test above deliberately: that one proves the MP4 is
    // playable media, this one proves it is the *same* media. A regression that
    // put an encoder in the path would leave the first test green.
    let Some(tools) = clipped_media_validation::require_media_tools() else {
        return;
    };

    let directory = clipped_media_validation::TemporaryDirectory::new("recorder-export-lossless");
    let source = directory.file("match.mkv");
    let destination = directory.file("match.mp4");
    recording_to_export(tools.ffmpeg(), &source);

    let recorded = clipped_media_validation::Media::open(&source)
        .expect("the recording opens")
        .packet_payloads_by_stream();

    let recorder = ServedRecorder::start("export-lossless");
    let mut client = recorder.client();
    export(&mut client, &source, &destination);

    let exported = clipped_media_validation::Media::open(&destination)
        .expect("the export opens")
        .packet_payloads_by_stream();

    assert_eq!(
        exported.len(),
        recorded.len(),
        "the MP4 has a different number of streams to the recording"
    );
    for (stream, expected) in recorded.iter().enumerate() {
        assert_eq!(
            &exported[stream], expected,
            "stream {stream} of the MP4 does not hold the recording's coded bytes; the media was \
             re-encoded or reordered rather than copied"
        );
    }

    drop(client);
    recorder.stop();
}

#[test]
fn an_export_onto_a_file_that_is_already_there_is_refused_and_that_file_is_left_alone() {
    // Issue #399's fifth acceptance criterion, over the wire. The refusal has
    // to arrive as `destination_exists` rather than as a general failure,
    // because that is the code the window offers "choose another name" on — and
    // the file that was there has to still hold what it held, because it is
    // somebody's footage (AGENTS.md section 56).
    //
    // No media tools are needed: the refusal happens before anything is read,
    // which is itself part of the claim.
    let directory = support::unique_path("export-taken");
    std::fs::create_dir_all(&directory).expect("a scratch directory can be made");
    let source = directory.join("match.mkv");
    let destination = directory.join("match.mp4");
    std::fs::write(&source, b"a recording").expect("the source is written");
    std::fs::write(&destination, b"somebody else's footage").expect("the file is written");

    let recorder = ServedRecorder::start("export-taken");
    let mut client = recorder.client();

    let error = client
        .call(&IpcCommand::ExportRecording(clipped_ipc::ExportRecording {
            source: source.to_string_lossy().into_owned(),
            destination: destination.to_string_lossy().into_owned(),
        }))
        .expect_err("a destination that already exists is refused");

    match error {
        ClientError::Refused(refusal) => {
            assert_eq!(refusal.code, ErrorCode::DestinationExists);
            assert!(
                refusal.message.contains("match.mp4")
                    && refusal.message.contains("choose another name"),
                "the refusal has to name the file and the one thing to do about it, or there is                  nothing to act on: {}",
                refusal.message
            );
        }
        other => panic!("expected a refusal, got {other}"),
    }

    assert_eq!(
        std::fs::read(&destination).expect("the file is still there"),
        b"somebody else's footage",
        "a refused export must not have written over what was already there"
    );

    drop(client);
    recorder.stop();
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn an_export_of_something_that_is_not_a_recording_says_so_in_the_muxers_own_words() {
    // Issue #399's sixth acceptance criterion: a refusal from the muxer reaches
    // the window with its own wording. A recorder that mapped every export
    // failure onto one sentence would leave somebody unable to tell a file that
    // has been moved from a recording MP4 cannot hold — two problems with two
    // different answers (AGENTS.md sections 15 and 45).
    let directory = support::unique_path("export-unreadable");
    std::fs::create_dir_all(&directory).expect("a scratch directory can be made");
    let source = directory.join("not-a-recording.mkv");
    std::fs::write(&source, b"this is not media").expect("the source is written");

    let recorder = ServedRecorder::start("export-unreadable");
    let mut client = recorder.client();

    let error = client
        .call(&IpcCommand::ExportRecording(clipped_ipc::ExportRecording {
            source: source.to_string_lossy().into_owned(),
            destination: directory
                .join("not-a-recording.mp4")
                .to_string_lossy()
                .into_owned(),
        }))
        .expect_err("a file that is not media cannot be exported");

    match error {
        ClientError::Refused(refusal) => {
            assert_eq!(refusal.code, ErrorCode::ExportFailed);
            assert!(
                refusal.message.contains("not-a-recording.mkv")
                    && refusal.message.contains("could not be read"),
                "the muxer's own sentence has to survive the whole round trip: {}",
                refusal.message
            );
        }
        other => panic!("expected a refusal, got {other}"),
    }

    assert!(
        !directory.join("not-a-recording.mp4").exists(),
        "a refused export must not leave a stub behind"
    );

    drop(client);
    recorder.stop();
    let _ = std::fs::remove_dir_all(&directory);
}

/// The rate the pattern application presents at, matching
/// `tests/record_end_to_end.rs`.
const SOURCE_FPS: u32 = 30;

/// How long the recording driven over the protocol runs for.
const RECORD_FOR: Duration = Duration::from_secs(4);

#[test]
#[ignore = "needs a GPU, an encoder and a desktop session; see tests/record_end_to_end.rs"]
fn a_recording_driven_entirely_over_the_protocol_produces_a_playable_file() {
    // The other end of the same claim `tests/record_end_to_end.rs` makes for the
    // command line: a recording started, observed and stopped over IPC is the
    // same recording, and its file is real. Without this, `start_recording` and
    // `stop_recording` would be two commands that have only ever been refused.
    let Some(_tools) = clipped_media_validation::require_media_tools() else {
        return;
    };

    let directory = clipped_media_validation::TemporaryDirectory::new("recorder-ipc");
    let output = directory.file("over-ipc.mkv");
    let pattern = support::PatternApp::start(SOURCE_FPS, 120);

    let recorder = ServedRecorder::start("recording");
    let mut client = recorder.client();

    let started = client
        .call(&IpcCommand::StartRecording(StartRecording {
            pid: Some(pattern.process_id()),
            output: Some(output.to_string_lossy().into_owned()),
            overwrite: true,
            // The session cannot record audio yet and would warn on every run;
            // a test should ask for what it expects to get.
            microphone: Some("none".to_owned()),
            system_audio: Some("none".to_owned()),
            ..StartRecording::default()
        }))
        .expect("the recording starts");

    let recording_id = match started {
        Reply::RecordingStarted { recording_id, .. } => recording_id,
        other => panic!("expected a started recording, got {other:?}"),
    };

    std::thread::sleep(RECORD_FOR);

    match client
        .call(&IpcCommand::GetStatus)
        .expect("status is answered")
    {
        Reply::Status {
            status: RecorderStatus::Recording(active),
        } => {
            assert_eq!(active.recording_id, recording_id);
            assert!(
                active.elapsed_ms >= 1_000,
                "a recording that has been running for seconds should say so: {active:?}"
            );
        }
        other => panic!("the recorder should say it is recording, not {other:?}"),
    }

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
        "\n=== recording over IPC ===\n\
         frames encoded : {}\n\
         picture        : {}x{} {}\n\
         encoder        : {}\n\
         duration       : {} ms\n\
         file           : {}\n",
        summary.frames_encoded,
        summary.width,
        summary.height,
        summary.codec,
        summary.encoder,
        summary.duration_ms,
        summary.output,
    );

    assert!(
        summary.frames_encoded > 0,
        "a recording of no frames is not a recording: {summary:?}"
    );
    assert_eq!(summary.end_reason, clipped_ipc::EndReason::Stopped);
    assert_eq!(
        (summary.width, summary.height),
        pattern.client_size(),
        "a borderless window's capture is exactly its client area"
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

    // And the recorder is idle again, which is what the UI would be told.
    match client
        .call(&IpcCommand::GetStatus)
        .expect("status is answered")
    {
        Reply::Status { status } => assert_eq!(status, RecorderStatus::Idle),
        other => panic!("expected a status, got {other:?}"),
    }

    drop(client);
    recorder.stop();
}

#[test]
fn a_real_recorder_indexes_the_recordings_folder_at_start_up_and_answers_from_it() {
    // Issue #402's second half, against the real process rather than against a
    // service built in a test: `serve` has to *call* the thing that fills the
    // library index, and before this ticket nothing anywhere in the product
    // did. A session record sitting in the recordings folder — the state a
    // machine that has run `watch` is in — must be findable through
    // `library_sessions` without anybody running a tool by hand.
    //
    // No GPU, no window and no encoder: the sitting is written by
    // `clipped-session`'s own writer, which is what a real recording would have
    // written, and what is under test is everything after that.
    let home = scratch_home("start-up-index");
    let recordings = home.join("Videos").join("Clipped");
    std::fs::create_dir_all(&recordings).expect("the recordings folder can be made");

    let output = recordings.join("clipped-earlier-sitting.mkv");
    std::fs::write(&output, [0u8; 4096]).expect("the recording can be written");
    let session = clipped_session::automatic::ManualSession::start(
        &recordings,
        output.clone(),
        &clipped_session::config::Configuration::defaults(),
        // Deliberately empty. What is under test is indexing, and a catalogue
        // is the one input here that would otherwise come from the machine
        // running the test rather than from the test (AGENTS.md section 25).
        &clipped_game_detection::catalogue::Catalogue::default(),
        clipped_session::automatic::RecordedProcess::new(4_242, "cs2.exe"),
        std::time::SystemTime::now(),
    );
    let identifier = session.id().as_str().to_owned();
    let _ = session.finish(
        &clipped_session::automatic::RecordingOutcome::Failed {
            detail: "recorded before this recorder started".to_owned(),
        },
        std::time::SystemTime::now(),
    );

    let recorder = ServedRecorder::start_under("start-up-index", Some(&home));
    let mut client = recorder.client();
    assert!(
        client
            .welcome()
            .features
            .iter()
            .any(|feature| feature == features::LIBRARY),
        "a recorder that indexes and answers has to advertise the library"
    );

    // Indexing runs on a thread of its own and nothing waits for it, so the
    // window's own behaviour is what this does: ask again.
    let deadline = std::time::Instant::now() + PATIENCE;
    let page = loop {
        let Reply::LibrarySessions { page } = client
            .call(&IpcCommand::LibrarySessions(
                clipped_ipc::LibrarySessions::default(),
            ))
            .expect("the library is readable")
        else {
            panic!("`library_sessions` was answered with something else");
        };
        if !page.sessions.is_empty() || std::time::Instant::now() >= deadline {
            break page;
        }
        std::thread::sleep(Duration::from_millis(100));
    };

    assert_eq!(
        page.sessions.len(),
        1,
        "the recorder never indexed the sitting sitting in its own recordings folder"
    );
    assert_eq!(page.sessions[0].session_id, identifier);
    assert_eq!(
        page.sessions[0].recordings[0].path,
        output.to_string_lossy()
    );
    assert_eq!(
        page.sessions[0].end_reason.as_deref(),
        Some("recording-ended"),
        "a session ended by its recording finishing has to survive the index's vocabulary"
    );

    drop(client);
    recorder.stop();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_recorder_whose_games_file_cannot_be_read_still_serves_and_says_so() {
    // Issue #403's third acceptance criterion, against the real process. The
    // catalogue is read at start-up now, because a recording started from the
    // window is filed under the game it belongs to — and a user who has broken
    // their own games file must lose the attribution and nothing else. `watch`
    // refuses to start over this same file, correctly, because it has nothing
    // to do without a catalogue; a `serve` that did the same would take the
    // window, the tray, every recording and the library with it.
    //
    // The entry below is refused by the catalogue for a reason nothing can
    // change later: a game with no executable can never match anything, so it
    // is rejected rather than skipped (`clipped_game_detection::catalogue`).
    let home = scratch_home("unreadable-catalogue");
    let application_directory = home.join("AppData").join("Local").join("Clipped");
    std::fs::create_dir_all(&application_directory).expect("the data directory can be made");
    std::fs::write(
        application_directory.join("games.toml"),
        "schema_version = 1\n\n[[game]]\ngame_id = \"broken\"\nname = \"Broken\"\n",
    )
    .expect("the games file can be written");

    // Starting at all is half the assertion: `start_under` waits for the ready
    // line and fails if the recorder exits instead of announcing an endpoint.
    let recorder = ServedRecorder::start_under("unreadable-catalogue", Some(&home));
    let mut client = recorder.client();
    assert_eq!(
        client.call(&IpcCommand::Ping).expect("ping is answered"),
        Reply::Pong
    );
    match client
        .call(&IpcCommand::GetStatus)
        .expect("status is answered")
    {
        Reply::Status { status } => assert_eq!(status, RecorderStatus::Idle),
        other => panic!("expected a status, got {other:?}"),
    }

    drop(client);
    let diagnostics = recorder.stop();
    assert!(
        diagnostics.contains("games.toml"),
        "somebody whose games file is unreadable has to be told which file to fix, or the \
         recordings quietly stop being attributed:\n{diagnostics}"
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// A home directory of this test's own, for a recorder that must not touch the
/// library or the recordings of whoever is running the tests.
fn scratch_home(label: &str) -> std::path::PathBuf {
    let home =
        std::env::temp_dir().join(format!("clipped-ipc-home-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("a scratch home can be made");
    home
}

/// Asserts the recorder is still answering, on a connection of its own.
///
/// Every rejection test ends with this. The interesting half of "a bad client
/// is refused" is that the *next* client is not: a recorder that closed its
/// listener because somebody sent it rubbish would pass the first half of each
/// of those tests perfectly well.
fn assert_still_serving(recorder: &ServedRecorder) {
    let mut client = recorder.client();
    assert_eq!(
        client.call(&IpcCommand::Ping).expect("ping is answered"),
        Reply::Pong,
        "the recorder should still be serving after refusing a connection"
    );
}
