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
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use clipped_ipc::frame::{read_message, write_message};
use clipped_ipc::transport::connect;
use clipped_ipc::{
    features, Client, ClientError, ClientMessage, Command as IpcCommand, ConnectionRole, Endpoint,
    ErrorCode, ErrorDetail, Event, EventClient, EventStream, Hello, PeerIdentity, RecorderStatus,
    Reply, ServerMessage, StartRecording, StopRecording, MAX_CONCURRENT_CONNECTIONS,
    PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS, UNBUILT_COMMANDS,
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
        ensure_console();

        let name = unique_endpoint_name(label);
        let mut child = Command::new(recorder_binary())
            .args(["serve", "--endpoint", &name])
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
/// client has gone at its own pace. Anything other than `too_many_connections`
/// fails the test rather than being retried.
fn connect_once_a_slot_is_free(recorder: &ServedRecorder) -> Client {
    let deadline = std::time::Instant::now() + PATIENCE;
    loop {
        match Client::connect(recorder.endpoint(), CLIENT_NAME, "0.0.0", PATIENCE) {
            Ok(client) => return client,
            Err(ClientError::Refused(refusal))
                if refusal.code == ErrorCode::TooManyConnections
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("the freed slot was never usable again: {error}"),
        }
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
