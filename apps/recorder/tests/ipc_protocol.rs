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
    features, ApplySettings, Client, ClientError, ClientMessage, Command as IpcCommand,
    ConnectionRole, Endpoint, ErrorCode, ErrorDetail, Event, EventClient, EventStream, Hello,
    HotkeyBinding, PeerIdentity, RecorderStatus, Reply, SaveReplay, ServerMessage, SettingEntry,
    SettingsView, StartRecording, StopRecording, MAX_CONCURRENT_CONNECTIONS, PROTOCOL_VERSION,
    SUPPORTED_PROTOCOL_VERSIONS,
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
        Self::started_with(label, home, &[])
    }

    /// The same, with further arguments after `--endpoint`.
    fn started_with(label: &str, home: Option<&Path>, extra: &[&str]) -> Self {
        ensure_console();

        let name = unique_endpoint_name(label);
        let mut command = Command::new(recorder_binary());
        command.args(["serve", "--endpoint", &name]);
        command.args(extra);
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
fn save_replay_is_a_command_this_recorder_performs_rather_than_one_it_refuses_by_name() {
    // Issue #38 turned `save_replay` from a command that was parsed only so it
    // could be refused into a real one, and this is the difference against a
    // real process: the refusal a recorder
    // with nothing recording gives is `not_recording` — a fact about right now,
    // which changes when a recording starts — and **not** `not_implemented`,
    // which is a fact about the build and never changes.
    //
    // The mistake it guards against is the one `add_bookmark` and the library
    // commands already record: a command still refused as unbuilt after its
    // subsystem landed answers every request with a plausible sentence about a
    // milestone, and nobody questions it. `apply_settings` was the last of
    // them, until issue #51.
    let recorder = ServedRecorder::start("save-replay");
    let mut client = recorder.client();

    assert!(
        client
            .welcome()
            .features
            .iter()
            .any(|feature| feature == features::REPLAY),
        "a build that can save a replay has to say so, or the window never offers the \
         control: {:?}",
        client.welcome()
    );

    match client
        .call(&IpcCommand::SaveReplay(SaveReplay::default()))
        .expect_err("nothing is being recorded")
    {
        ClientError::Refused(refusal) => {
            assert_eq!(
                refusal.code,
                ErrorCode::NotRecording,
                "a recorder with nothing recording has nothing to save from: {refusal:?}"
            );
            assert_ne!(
                refusal.code,
                ErrorCode::NotImplemented,
                "`save_replay` is built; refusing it by milestone would be a lie the UI                  would repeat"
            );
            assert!(
                refusal.detail.is_none(),
                "and it carries no milestone to point at: {refusal:?}"
            );
            assert!(
                refusal.message.contains("replay buffer"),
                "the refusal has to say what was missing: {}",
                refusal.message
            );
        }
        other => panic!("expected a refusal, got {other:?}"),
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

/// One setting out of a view, by the key the settings file holds it under.
fn setting(view: &SettingsView, key: &str) -> SettingEntry {
    view.settings
        .iter()
        .find(|entry| entry.key == key)
        .unwrap_or_else(|| {
            panic!(
                "the recorder sent no `{key}` setting: {:?}",
                view.settings
                    .iter()
                    .map(|entry| &entry.key)
                    .collect::<Vec<_>>()
            )
        })
        .clone()
}

/// The settings a `get_settings` or an `apply_settings` answered with.
fn settings_of(reply: Reply) -> SettingsView {
    match reply {
        Reply::Settings { settings } => settings,
        other => panic!("expected the settings, got {other:?}"),
    }
}

/// One change, as a settings screen sends it.
fn change(key: &str, value: Option<&str>) -> ApplySettings {
    let mut values = std::collections::BTreeMap::new();
    values.insert(key.to_owned(), value.map(str::to_owned));
    ApplySettings { values }
}

#[test]
fn a_microphone_chosen_in_the_window_reaches_the_settings_file_the_recorder_records_by() {
    // Step 3 of SPEC.md section 45's MVP, against a real recorder over a real
    // pipe: pick a microphone, and it is in the file the recorder reads and in
    // the answer the next window gets. Until issue #51 the window could not
    // read or write a setting at all — `apply_settings` was refused by every
    // build with `not_implemented`.
    let home = scratch_home("settings");
    let recorder = ServedRecorder::start_under("settings", Some(&home));
    let mut client = recorder.client();

    assert!(
        client
            .welcome()
            .features
            .iter()
            .any(|feature| feature == features::SETTINGS),
        "a build that can change settings has to say so, or the window never draws the \
         controls: {:?}",
        client.welcome()
    );

    let before = settings_of(
        client
            .call(&IpcCommand::GetSettings)
            .expect("a recorder that is serving can be asked for its settings"),
    );
    let microphone = setting(&before, "microphone");
    assert_eq!(microphone.value, "default");
    assert!(
        !microphone.overridden,
        "a machine whose settings file does not exist has configured nothing",
    );
    assert!(
        microphone.applies,
        "the microphone is read when a recording starts, so it is offered as a control",
    );

    let after = settings_of(
        client
            .call(&IpcCommand::ApplySettings(change(
                "microphone",
                Some("name:Shure MV7"),
            )))
            .expect("a device name is a value the settings file can hold"),
    );
    assert_eq!(setting(&after, "microphone").value, "name:Shure MV7");
    assert!(setting(&after, "microphone").overridden);

    // The file the recorder owns, not this process's idea of it: what makes
    // "close the window and recording works from then on" true is that the
    // choice is on disk (SPEC.md section 45).
    let file = std::path::PathBuf::from(&after.file);
    assert!(
        file.starts_with(&home),
        "the recorder saved to {} rather than under the home this test gave it",
        file.display(),
    );
    let written = std::fs::read_to_string(&file).expect("the settings file was written");
    assert!(
        written.contains("Shure MV7"),
        "the microphone did not reach the file: {written}",
    );

    // And a window opening afterwards is told the same thing.
    let mut second = recorder.client();
    let again = settings_of(
        second
            .call(&IpcCommand::GetSettings)
            .expect("the settings can be read again"),
    );
    assert_eq!(setting(&again, "microphone").value, "name:Shure MV7");

    drop(second);
    drop(client);
    recorder.stop();
}

#[test]
fn a_recorder_that_can_listen_to_a_microphone_says_so_and_answers_about_one() {
    // The first run's meter, over the wire (issue #109). Two things are checked
    // and neither needs a microphone plugged into the machine running the test:
    //
    // - the capability is advertised **separately** from `settings`, because a
    //   window that cannot get a level should still draw the device chooser
    //   rather than refuse the whole screen;
    // - `none` is refused rather than answered with a reading of zero. That is
    //   the distinction the meter exists for: a setting somebody chose and a
    //   microphone that heard nothing must not arrive looking the same
    //   (AGENTS.md section 27).
    //
    // What is *not* checked here is the number, because that needs an endpoint.
    // `crates/session/src/audio/tests.rs` holds the reduction of a buffer of
    // samples to a peak, which is the part of it that is arithmetic.
    let home = scratch_home("microphone-level");
    let recorder = ServedRecorder::start_under("microphone-level", Some(&home));
    let mut client = recorder.client();

    assert!(
        client
            .welcome()
            .features
            .iter()
            .any(|feature| feature == features::MICROPHONE_LEVEL),
        "a build that can measure a microphone has to say so, or the window draws a meter that          will never move: {:?}",
        client.welcome()
    );

    match client.call(&IpcCommand::GetMicrophoneLevel(
        clipped_ipc::MicrophoneLevelRequest {
            microphone: "none".to_owned(),
        },
    )) {
        Err(ClientError::Refused(refusal)) => {
            assert_eq!(refusal.code, ErrorCode::InvalidParameters);
            assert!(
                refusal.message.contains("no microphone"),
                "the refusal has to say why there is no level: {}",
                refusal.message,
            );
        }
        other => panic!("`none` has no level to report, got {other:?}"),
    }

    drop(client);
    recorder.stop();
}

#[test]
fn a_setting_the_file_would_refuse_is_refused_with_what_would_have_been_accepted() {
    // AGENTS.md section 45, over the wire: not "invalid", but the value, the
    // range and the setting — the same sentence the file's own reader gives,
    // because it is the same validation (`clipped_session::config`).
    let home = scratch_home("settings-refused");
    let recorder = ServedRecorder::start_under("settings-refused", Some(&home));
    let mut client = recorder.client();

    match client.call(&IpcCommand::ApplySettings(change("framerate", Some("900")))) {
        Err(ClientError::Refused(refusal)) => {
            assert_eq!(refusal.code, ErrorCode::InvalidParameters);
            assert!(
                refusal.message.contains("900") && refusal.message.contains("480"),
                "the refusal should name the value and the range: {}",
                refusal.message,
            );
        }
        other => panic!("900 frames per second should be refused, got {other:?}"),
    }

    // And nothing was saved: a refused change leaves the settings alone.
    let view = settings_of(
        client
            .call(&IpcCommand::GetSettings)
            .expect("the settings can still be read"),
    );
    assert_eq!(setting(&view, "framerate").value, "60");
    assert!(!setting(&view, "framerate").overridden);

    drop(client);
    recorder.stop();
}

#[test]
fn the_recorder_lists_the_microphones_it_would_record_from_or_says_why_it_cannot() {
    // AGENTS.md section 27: an empty list drawn as though the machine had been
    // looked at is the failure this reply exists to prevent, so either there is
    // a list or there is a reason. Which of the two this machine gives depends
    // on the machine, and both are answers.
    let recorder = ServedRecorder::start("audio-devices");
    let mut client = recorder.client();

    match client.call(&IpcCommand::GetAudioDevices) {
        Ok(Reply::AudioDevices { devices }) => {
            for device in &devices.microphones {
                assert!(
                    !device.name.trim().is_empty(),
                    "a device somebody is asked to choose has to have a name: {devices:?}",
                );
            }
        }
        Err(ClientError::Refused(refusal)) => {
            assert!(
                !refusal.message.trim().is_empty(),
                "a recorder that cannot list the devices has to say why",
            );
        }
        other => panic!("expected a device list or a reason, got {other:?}"),
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

    // The row this ticket turned over. `save_replay` was the example of an
    // action nothing performed, naming M3 and issue #38 — and issue #38 built
    // it, so the recorder answers `save_replay` now (`crate::hotkeys`,
    // `docs/hotkeys.md`). A row still reading as unavailable here would be the
    // product telling a user a shipped feature is unbuilt, which is the failure
    // AGENTS.md sections 27 and 54 name and the one nobody questions, because
    // the sentence still reads plausibly.
    let save = row(&hotkeys, "save_replay");
    assert!(
        save.handled && save.unavailable.is_none(),
        "this recorder answers `save_replay`, so its row must not read as unavailable: {save:?}",
    );

    // And the half that has not changed: an action nothing performs still says
    // which milestone and issue would build it.
    let overlay = row(&hotkeys, "open_overlay");
    assert!(!overlay.handled, "no build opens an overlay: {overlay:?}");
    let reason = overlay
        .unavailable
        .as_deref()
        .expect("an action nothing performs has to say why");
    assert!(
        reason.contains("Open overlay") && reason.contains("M5") && reason.contains("#53"),
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
            // Asked for explicitly, so the file is the same whatever audio
            // devices the machine has;
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
        &clipped_game_detection::launcher::Launchers::none(),
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

/// How many earlier sittings the overlap test leaves for the indexer to walk.
///
/// Enough that the walk cannot finish inside the four seconds of recording it
/// has to overlap. Measured rather than guessed: on the development machine
/// (RTX 4090, NVMe) a run gets through about 2,160 sittings in 4.1 s at the
/// background pace, so this is roughly four times what the recording's length
/// allows for — and the run is expected to be cancelled by shutdown well before
/// it reaches the end.
///
/// The test does not trust that headroom. It reads how far the run really got
/// and how long it really lasted out of the recorder's own report, and fails
/// with an honest "demonstrates nothing, raise this constant" if a faster
/// machine finishes the walk before the recording ends — rather than passing
/// against an idle recorder.
const SITTINGS_TO_INDEX: usize = 8_000;

/// Writes `count` finished session records into `recordings`, each with a file
/// beside it, which is the state a machine that has recorded before is in.
///
/// **A second apart, and that is not decoration.** A session's identifier is its
/// game's slug and the second it started
/// (`clipped_session::automatic::SessionId::new`), and the record is persisted
/// under that identifier — so sittings written in a loop from
/// `SystemTime::now()` share an identifier and overwrite each other. Writing
/// 4,000 that way leaves however many whole seconds the loop happened to take,
/// which was eight, and an index run with eight sessions in it finishes before
/// a recording can start.
fn earlier_sittings(recordings: &Path, count: usize) {
    let now = std::time::SystemTime::now();

    for index in 0..count {
        // Backwards, so that every sitting is one this recorder could plausibly
        // have made before it started, and every one gets a second of its own.
        let started = now - Duration::from_secs(index as u64 + 1);
        let output = recordings.join(format!("clipped-earlier-{index:05}.mkv"));
        std::fs::write(&output, [0u8; 1024]).expect("the recording can be written");
        let session = clipped_session::automatic::ManualSession::start(
            recordings,
            output,
            &clipped_session::config::Configuration::defaults(),
            // Deliberately empty, for the reason the start-up test gives: a
            // catalogue is the one input here that would otherwise come from
            // the machine running the test (AGENTS.md section 25).
            &clipped_game_detection::catalogue::Catalogue::default(),
            &clipped_game_detection::launcher::Launchers::none(),
            clipped_session::automatic::RecordedProcess::new(4_242, "cs2.exe"),
            started,
        );
        let _ = session.finish(
            &clipped_session::automatic::RecordingOutcome::Failed {
                detail: "recorded before this recorder started".to_owned(),
            },
            started,
        );
    }
}

/// How long the first index run lasted, according to the recorder's own report.
///
/// Read out of the log rather than measured from outside, because the question
/// is when *the run* ended and only the recorder knows that. A run cancelled by
/// shutdown logs too, with the time it had been going for, which is a lower
/// bound on how long it was in flight — and a lower bound is all the assertion
/// below needs.
fn first_index_run(diagnostics: &str) -> &str {
    const REPORT: &str = "the library index was reconciled against the recording folders";

    diagnostics
        .lines()
        .find(|line| line.contains(REPORT))
        .unwrap_or_else(|| {
            panic!(
                "the recorder never reported reconciling its index, so there was no run to \
                 overlap with:\n{diagnostics}"
            )
        })
}

/// One `name=value` field of a log line, as a number.
fn field(line: &str, name: &str) -> u64 {
    let prefix = format!("{name}=");
    let value = line
        .split_whitespace()
        .find_map(|field| field.strip_prefix(prefix.as_str()))
        .unwrap_or_else(|| panic!("an index report should carry `{name}`: {line}"));

    value
        .trim_matches(|character: char| !character.is_ascii_digit())
        .parse()
        .unwrap_or_else(|error| panic!("`{value}` is not a number: {error}"))
}

#[test]
#[ignore = "needs a GPU, an encoder and a desktop session, and writes thousands of session records"]
fn an_index_run_in_flight_neither_delays_nor_interrupts_a_recording() {
    // Issue #385's second acceptance criterion, which asks for this to be
    // *demonstrated* rather than asserted.
    //
    // The design argues it already: the indexer owns a thread and a database
    // connection of its own, runs at the background pace, and nothing on a
    // recording's path takes either of its locks — which is why `finish` hands
    // the work to `indexer.request()` instead of walking the folder on the
    // recording thread. An argument is not a demonstration, though, and the way
    // to tell is to make a run genuinely be in flight while a real recording
    // starts, runs and stops, and then to check afterwards that it really was.
    //
    // What would fail here: an indexer that shared the reader's connection, took
    // a lock the recording path also takes, or ran on the thread that answers
    // commands. The recording would stall, come back short of frames, or not
    // start until the walk had finished.
    let Some(_tools) = clipped_media_validation::require_media_tools() else {
        return;
    };

    let home = scratch_home("index-overlap");
    let recordings = home.join("Videos").join("Clipped");
    std::fs::create_dir_all(&recordings).expect("the recordings folder can be made");
    earlier_sittings(&recordings, SITTINGS_TO_INDEX);

    // Started before the recorder, so that the recording can begin in the first
    // moments of the index run rather than after the window has been waited for.
    let directory = clipped_media_validation::TemporaryDirectory::new("recorder-index-overlap");
    let output = directory.file("while-indexing.mkv");
    let pattern = support::PatternApp::start(SOURCE_FPS, 120);

    let recorder = ServedRecorder::start_under("index-overlap", Some(&home));
    // The ready line is printed immediately before `start_indexing`, so this is
    // the moment the run began, give or take the thread starting.
    let indexing_began = std::time::Instant::now();
    let mut client = recorder.client();

    let started = client
        .call(&IpcCommand::StartRecording(StartRecording {
            pid: Some(pattern.process_id()),
            output: Some(output.to_string_lossy().into_owned()),
            overwrite: true,
            microphone: Some("none".to_owned()),
            system_audio: Some("none".to_owned()),
            ..StartRecording::default()
        }))
        .expect("a recorder that is indexing still starts a recording");
    let recording_began = std::time::Instant::now();

    let recording_id = match started {
        Reply::RecordingStarted { recording_id, .. } => recording_id,
        other => panic!("expected a started recording, got {other:?}"),
    };

    std::thread::sleep(RECORD_FOR);

    let summary = match client
        .call(&IpcCommand::StopRecording(StopRecording {
            recording_id: Some(recording_id),
        }))
        .expect("a recorder that is indexing still stops a recording")
    {
        Reply::RecordingStopped { summary } => summary,
        other => panic!("expected a summary, got {other:?}"),
    };
    let recording_ended = std::time::Instant::now();

    // The recording is whole: an indexer that interrupted one would show up
    // here as a short file, a failed end reason, or no frames at all.
    assert!(
        summary.frames_encoded > 0,
        "a recording made while the index was being walked encoded nothing: {summary:?}"
    );
    assert_eq!(summary.end_reason, clipped_ipc::EndReason::Stopped);

    clipped_media_validation::Media::open(&output)
        .unwrap_or_else(|error| panic!("the recording is not usable at all: {error}"))
        .validate()
        .video_stream_count(1)
        .video(
            clipped_media_validation::VideoStream::codec(&summary.codec)
                .resolution(summary.width, summary.height)
                .decoded_frames(summary.frames_encoded),
        )
        .monotonic_timestamps()
        .assert_valid();

    drop(client);
    let diagnostics = recorder.stop();
    let report = first_index_run(&diagnostics);
    let indexing_lasted = Duration::from_millis(field(report, "duration_ms"));
    let sessions_indexed = field(report, "sessions");
    let recording_started_after = recording_began.duration_since(indexing_began);
    let recording_ended_after = recording_ended.duration_since(indexing_began);

    eprintln!(
        "\n=== a recording made while the library index was being walked ===\n\
         sittings written  : {SITTINGS_TO_INDEX}\n\
         sessions indexed  : {sessions_indexed}\n\
         index run lasted  : {} ms\n\
         recording ran     : {} ms to {} ms after the run began\n\
         frames encoded    : {}\n\
         picture           : {}x{} {}\n\
         report            : {report}\n",
        indexing_lasted.as_millis(),
        recording_started_after.as_millis(),
        recording_ended_after.as_millis(),
        summary.frames_encoded,
        summary.width,
        summary.height,
        summary.codec,
    );

    // Two checks stop this passing for the wrong reason, and both are about the
    // run rather than the recording.
    //
    // The run has to have done real work. A walk that finds nothing finishes in
    // microseconds, and a recording made alongside *that* demonstrates nothing —
    // which is not hypothetical: the first draft of this test wrote its 4,000
    // sittings from `SystemTime::now()`, they collapsed onto eight identifiers,
    // and the run it overlapped was a 46 ms walk of eight sessions.
    //
    // Not an equality against what was written, because the run is expected not
    // to finish: the recorder is stopped while the walk is still going, which
    // cancels it. How far it got is the machine's business; that it was
    // genuinely walking this library is not.
    assert!(
        sessions_indexed >= 500,
        "the run indexed only {sessions_indexed} sessions of the {SITTINGS_TO_INDEX} written, \
         which is too few to have been a walk of this library: {report}"
    );

    // And it has to have been in flight for the *whole* recording, not merely
    // to have started before it. A run that had finished by the time the
    // recording began would make everything above a recording made against an
    // idle recorder.
    //
    // `cancelled=true` in the report is the ordinary outcome and the strongest
    // form of this: it means the walk had still not finished when the recorder
    // was stopped, which is after the recording ended.
    assert!(
        indexing_lasted >= recording_ended_after,
        "the index run finished {} ms in, and the recording ran until {} ms, so the walk was \
         not in flight for all of it and this run demonstrates less than it claims. Raise \
         SITTINGS_TO_INDEX: {report}",
        indexing_lasted.as_millis(),
        recording_ended_after.as_millis(),
    );

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

#[test]
fn a_recorder_watching_for_games_serves_the_protocol_and_stops_cleanly() {
    // Issue #421 joined the two halves of the recorder: the process that serves
    // the protocol and owns the hotkeys is now also the one that records games
    // as they launch. Everything about that is in one process, so the things
    // most likely to go wrong with it are the ones only a real process shows —
    // a watcher thread that stops `serve` from ever announcing its endpoint,
    // and a shutdown that waits for a thread nobody asked to stop.
    //
    // The home is redirected because this recorder really does create its
    // recordings folder and really does watch this machine for launches
    // (AGENTS.md section 25).
    let home = scratch_home("watching");

    let recorder = ServedRecorder::started_with("watching", Some(&home), &["--watch-for-games"]);
    let mut client = recorder.client();

    assert_eq!(
        client.call(&IpcCommand::Ping).expect("ping is answered"),
        Reply::Pong,
        "a recorder that watches for games still serves the protocol",
    );
    match client
        .call(&IpcCommand::GetStatus)
        .expect("status is answered")
    {
        // Nothing has launched, so nothing is being recorded. A recorder that
        // reported otherwise would be claiming a recording it is not making.
        Reply::Status { status } => assert_eq!(status, RecorderStatus::Idle),
        other => panic!("expected a status, got {other:?}"),
    }

    drop(client);
    // `stop` asserts the exit was clean rather than a kill, which is the half
    // that catches a shutdown waiting on the watcher for ever.
    let diagnostics = recorder.stop();
    assert!(
        diagnostics.contains("Watching for games"),
        "a recorder asked to watch has to say so, or nobody can tell it from one that was \
         not:\n{diagnostics}"
    );
    assert!(
        home.join("Videos").join("Clipped").is_dir(),
        "recordings go where the settings say and to the videos folder when they say nothing, \
         and the folder is made at start-up rather than when a game launches",
    );

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_recorder_that_was_not_asked_to_watch_for_games_does_not() {
    // The other direction, and what makes the test above mean anything: a build
    // that watched regardless would pass it just as well, and every `serve`
    // somebody started at a terminal would begin recording whatever game they
    // had open.
    let home = scratch_home("not-watching");

    let recorder = ServedRecorder::start_under("not-watching", Some(&home));
    let mut client = recorder.client();
    assert_eq!(
        client.call(&IpcCommand::Ping).expect("ping is answered"),
        Reply::Pong
    );

    drop(client);
    let diagnostics = recorder.stop();
    assert!(
        !diagnostics.contains("Watching for games"),
        "nothing asked this recorder to watch for games:\n{diagnostics}"
    );
    assert!(
        !home.join("Videos").join("Clipped").exists(),
        "and it must not have made a recordings folder for a user who did not ask it to record",
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

/// The frequency each sound track of the playback fixture carries.
///
/// The same three AGENTS.md section 26 uses, so that "which track is this" is
/// answered by listening to the file rather than by trusting its metadata.
const PLAYBACK_MIX: f64 = 440.0;
const PLAYBACK_GAME: f64 = 880.0;
const PLAYBACK_MICROPHONE: f64 = 1320.0;

/// Builds a recording shaped like a Clipped one — a picture and three named
/// sound tracks, the first of them flagged as the default — and returns it.
///
/// Built with the pinned build's own `ffmpeg` for the reason
/// [`recording_to_export`] is, and with **uncompressed** sound for the same
/// reason: that is what Clipped writes, and it is the half that would go
/// untested if the fixture were convenient instead.
fn recording_with_three_sound_tracks(ffmpeg: &std::path::Path, into: &std::path::Path) {
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
        ])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000"])
        .args(["-f", "lavfi", "-i", "sine=frequency=880:sample_rate=48000"])
        .args(["-f", "lavfi", "-i", "sine=frequency=1320:sample_rate=48000"])
        .args(["-t", EXPORT_FIXTURE_SECONDS])
        .args(["-map", "0:v", "-map", "1:a", "-map", "2:a", "-map", "3:a"])
        .args(["-c:v", "mpeg4", "-c:a", "pcm_s16le"])
        .args(["-metadata:s:a:0", "title=Compatibility Mix"])
        .args(["-metadata:s:a:1", "title=Game"])
        .args(["-metadata:s:a:2", "title=Microphone"])
        .args(["-disposition:a:0", "default"])
        .args(["-disposition:a:1", "0"])
        .args(["-disposition:a:2", "0"])
        .arg(into)
        .output()
        .expect("the pinned ffmpeg can be run");

    assert!(
        output.status.success(),
        "the recording to play could not be built: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Sends `open_playback` and returns what the recorder answered with.
fn open_playback(
    client: &mut Client,
    source: &std::path::Path,
    audio_track: Option<usize>,
) -> clipped_ipc::PlaybackStream {
    match client
        .call(&IpcCommand::OpenPlayback(clipped_ipc::OpenPlayback {
            source: source.to_string_lossy().into_owned(),
            audio_track,
        }))
        .unwrap_or_else(|error| panic!("playback was refused: {error}"))
    {
        Reply::PlaybackOpened { playback } => playback,
        other => panic!("expected a playback stream, got {other:?}"),
    }
}

#[test]
fn a_recording_opened_for_playback_is_served_whole_and_costs_nothing_to_open() {
    // Issue #304's first criterion, from the recorder's side: what the window
    // is told to play is the recording itself, because a WebView2 plays it
    // (`docs/adr/0011-what-the-webview-plays.md`). A regression that started
    // remuxing every recording somebody watched would leave the player working
    // and cost a pass over the file every time, so `prepared` is asserted as
    // hard as the path is.
    let Some(tools) = clipped_media_validation::require_media_tools() else {
        return;
    };

    let directory = clipped_media_validation::TemporaryDirectory::new("recorder-playback");
    let source = directory.file("match.mkv");
    recording_with_three_sound_tracks(tools.ffmpeg(), &source);
    let recording_before = std::fs::read(&source).expect("the recording can be read");

    let recorder = ServedRecorder::start("playback");
    let mut client = recorder.client();

    let playback = open_playback(&mut client, &source, None);

    assert_eq!(
        playback.path,
        source.to_string_lossy(),
        "the recording itself is what plays when nothing has to be prepared"
    );
    assert!(
        !playback.prepared,
        "opening a recording on its own default track must not copy it: {playback:?}"
    );
    // Stream 1: the picture is stream 0, and the index is the container's own
    // rather than an ordinal among the sound tracks.
    assert_eq!(playback.audio_track, Some(1));

    let names: Vec<Option<&str>> = playback
        .audio_tracks
        .iter()
        .map(|track| track.name.as_deref())
        .collect();
    assert_eq!(
        names,
        vec![Some("Compatibility Mix"), Some("Game"), Some("Microphone")],
        "the window needs the recording's own track names to draw a selector"
    );
    assert_eq!(
        playback
            .audio_tracks
            .iter()
            .map(|track| track.index)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(
        playback
            .audio_tracks
            .iter()
            .map(|track| track.default)
            .collect::<Vec<_>>(),
        vec![true, false, false]
    );

    assert!(
        std::fs::read(&source).expect("the recording can be read again") == recording_before,
        "opening a recording for playback changed it"
    );

    drop(client);
    recorder.stop();
}

#[test]
fn a_track_chosen_for_playback_is_the_one_that_can_be_heard_in_what_is_served() {
    // Issue #304's second criterion, and the reason any of this exists: a media
    // element cannot choose an audio track, so choosing one means being handed a
    // file that holds it. The assertion is on what is *audible* in that file — a
    // selection that took the wrong stream would still produce a one-track MP4
    // of the right length with the right codec.
    let Some(tools) = clipped_media_validation::require_media_tools() else {
        return;
    };

    let directory = clipped_media_validation::TemporaryDirectory::new("recorder-playback-track");
    let source = directory.file("match.mkv");
    recording_with_three_sound_tracks(tools.ffmpeg(), &source);

    let recorder = ServedRecorder::start("playback-track");
    let mut client = recorder.client();

    for (stream, tone, others) in [
        (2, PLAYBACK_GAME, [PLAYBACK_MIX, PLAYBACK_MICROPHONE]),
        (3, PLAYBACK_MICROPHONE, [PLAYBACK_MIX, PLAYBACK_GAME]),
    ] {
        let playback = open_playback(&mut client, &source, Some(stream));

        assert_eq!(playback.audio_track, Some(stream));
        assert!(
            playback.prepared,
            "a track a media element cannot reach has to be prepared: {playback:?}"
        );
        assert_ne!(
            playback.path,
            source.to_string_lossy(),
            "a prepared copy must never be the recording itself"
        );
        assert_eq!(
            playback.audio_tracks.len(),
            3,
            "the tracks offered are the recording's, not the copy's: {playback:?}"
        );

        clipped_media_validation::Media::open(std::path::Path::new(&playback.path))
            .unwrap_or_else(|error| panic!("what was served does not open: {error}"))
            .validate()
            .audio_stream_count(1)
            .audio_tone(
                0,
                clipped_media_validation::Tone::at(tone)
                    .isolated_from(others[0])
                    .isolated_from(others[1]),
            )
            .video_stream_count(1)
            .assert_valid();
    }

    // And asking for a track the recording has not got says so, rather than
    // quietly playing the default: a window that asked for the microphone and
    // was handed the mix would look exactly as though it had worked.
    let error = client
        .call(&IpcCommand::OpenPlayback(clipped_ipc::OpenPlayback {
            source: source.to_string_lossy().into_owned(),
            audio_track: Some(9),
        }))
        .expect_err("a track that is not there is refused");
    match error {
        ClientError::Refused(refusal) => {
            assert_eq!(refusal.code, ErrorCode::PlaybackFailed);
            assert!(
                refusal.message.contains('9'),
                "the refusal should name the track: {}",
                refusal.message
            );
        }
        other => panic!("expected a refusal, got {other}"),
    }

    drop(client);
    recorder.stop();
}

#[test]
fn a_recording_whose_file_has_gone_is_refused_with_something_a_person_can_act_on() {
    // Issue #304's fourth criterion. The window draws this sentence instead of a
    // player, so it has to name the file and say what probably happened to it
    // rather than being "playback failed" (AGENTS.md sections 15, 27 and 45).
    let recorder = ServedRecorder::start("playback-missing");
    let mut client = recorder.client();

    let error = client
        .call(&IpcCommand::OpenPlayback(clipped_ipc::OpenPlayback {
            source: r"D:\clips\a recording nobody has\match.mkv".to_owned(),
            audio_track: None,
        }))
        .expect_err("a recording that is not there cannot be played");

    match error {
        ClientError::Refused(refusal) => {
            assert_eq!(refusal.code, ErrorCode::PlaybackFailed);
            assert!(
                refusal.message.contains("match.mkv")
                    && refusal.message.contains("not there any more"),
                "the refusal should name the file and say it has gone: {}",
                refusal.message
            );
        }
        other => panic!("expected a refusal, got {other}"),
    }

    drop(client);
    recorder.stop();
}
