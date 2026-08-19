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
    ConnectionRole, Diagnostics, Endpoint, ErrorCode, ErrorDetail, Event, EventClient, EventStream,
    Hello, HotkeyBinding, PeerIdentity, RecorderStatus, Reply, SaveReplay, ServerMessage,
    SettingEntry, SettingsView, StartRecording, StopRecording, MAX_CONCURRENT_CONNECTIONS,
    PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS,
};

use clipped_recorder::settings::RECORDING_DIRECTORY;

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

    /// Waits for a line containing `needle` on standard error.
    ///
    /// The ready line is not enough for everything. It is written as soon as
    /// the protocol is being served, which is *before* the watcher thread has
    /// built the session manager that automatic recordings are resolved
    /// through — and that manager takes its copy of the settings when it is
    /// built. A test that saved a setting on the strength of the ready line
    /// alone would be racing that copy, and would pass whether or not anything
    /// ever replaced it (`crate::watch::announce`, issue #51).
    ///
    /// Lines read on the way are returned rather than dropped, so that a test
    /// can put them in an assertion message; `stop` still collects everything
    /// written after this point.
    fn wait_for(&self, needle: &str) -> String {
        let deadline = std::time::Instant::now() + PATIENCE;
        let mut read = String::new();
        loop {
            match self
                .diagnostics
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            {
                Ok(line) => {
                    let found = line.contains(needle);
                    read.push_str(&line);
                    read.push('\n');
                    if found {
                        return read;
                    }
                }
                Err(error) => panic!("the recorder never said `{needle}` ({error}):\n{read}"),
            }
        }
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
    ApplySettings { game: None, values }
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
            .call(&IpcCommand::GetSettings(clipped_ipc::GetSettings::default()))
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
            .call(&IpcCommand::GetSettings(clipped_ipc::GetSettings::default()))
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
fn a_notification_switched_off_in_the_window_is_in_the_one_settings_file() {
    // Issue #252's first two acceptance criteria, against a real recorder over a
    // real pipe. The switches were `notifications.json` in the *window's* own
    // configuration directory — a second store with a second version field and a
    // second reader — and they are settings now: the window sends the same
    // `apply_settings` it sends for a frame rate, and what comes back is what a
    // window opening afterwards is told.
    //
    // Nothing in this recorder reads them. That is the point: the process that
    // acts on them may link one crate of this workspace and so cannot open the
    // file, so it asks.
    let home = scratch_home("notifications");
    let recorder = ServedRecorder::start_under("notifications", Some(&home));
    let mut client = recorder.client();

    let before = settings_of(
        client
            .call(&IpcCommand::GetSettings(clipped_ipc::GetSettings::default()))
            .expect("a recorder that is serving can be asked for its settings"),
    );
    for key in [
        "recording_failed",
        "recording_interrupted",
        "recorder_unavailable",
        "hotkey_unavailable",
    ] {
        let switch = setting(&before, key);
        assert_eq!(switch.value, "true", "{key} should default to on");
        assert!(!switch.overridden);
        assert!(
            switch.applies,
            "the window acts on {key}, so it must not be drawn as a dead control",
        );
        assert_eq!(
            switch.choices,
            vec!["true".to_owned(), "false".to_owned()],
            "a switch is a closed set of two, which is what makes a window draw one",
        );
    }

    let after = settings_of(
        client
            .call(&IpcCommand::ApplySettings(change(
                "recorder_unavailable",
                Some("false"),
            )))
            .expect("a switch takes true or false"),
    );
    assert_eq!(setting(&after, "recorder_unavailable").value, "false");
    assert!(setting(&after, "recorder_unavailable").overridden);
    assert_eq!(
        setting(&after, "recording_failed").value,
        "true",
        "switching one category off must leave the others alone",
    );

    // One file, which is the whole of the first acceptance criterion: it is the
    // settings file the recording settings are in, not a second one beside it.
    let file = std::path::PathBuf::from(&after.file);
    assert!(file.ends_with("settings.json"), "{}", file.display());
    let written = std::fs::read_to_string(&file).expect("the settings file was written");
    assert!(
        written.contains("\"notifications\"") && written.contains("\"recorder_unavailable\""),
        "the switch did not reach the settings file: {written}",
    );

    // And a window opening afterwards is told the same thing, which is how a
    // switch survives a restart now that nothing keeps a copy of it.
    let mut second = recorder.client();
    let again = settings_of(
        second
            .call(&IpcCommand::GetSettings(clipped_ipc::GetSettings::default()))
            .expect("the settings can be read again"),
    );
    assert_eq!(setting(&again, "recorder_unavailable").value, "false");

    drop(second);
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
            .call(&IpcCommand::GetSettings(clipped_ipc::GetSettings::default()))
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
fn the_recorder_says_whether_it_starts_at_login_without_changing_whether_it_does() {
    // **A read only.** `set_start_at_login` is deliberately not exercised from
    // here: the only entry a served recorder can be asked about is the real one
    // under this account, and a test that turned it on would arrange for the
    // machine running the suite to start a recorder at every sign-in afterwards
    // (AGENTS.md section 25). What the write does is
    // `clipped_recorder::start_at_login`'s own tests, against a scratch key.
    //
    // What this proves is the half those cannot: that a `serve` really answers
    // the command, so a window asking it gets an arrangement or a reason rather
    // than `unknown_command`.
    let recorder = ServedRecorder::start("start-at-login");
    let mut client = recorder.client();

    let before = client.call(&IpcCommand::GetStartAtLogin);
    match &before {
        Ok(Reply::StartAtLogin { start_at_login }) => {
            assert!(
                start_at_login.location.contains(r"CurrentVersion\Run"),
                "the window is told where the entry is, and it is the key Windows reads: {:?}",
                start_at_login.location,
            );
            assert_eq!(
                start_at_login.enabled,
                start_at_login.command.is_some(),
                "on and no command, or off and a command, is a state that cannot be drawn: {start_at_login:?}",
            );
        }
        Err(ClientError::Refused(refusal)) => {
            assert!(
                !refusal.message.trim().is_empty(),
                "a recorder that cannot read the entry has to say why, so a window can say it \
                 too rather than drawing the switch off",
            );
        }
        other => panic!("expected an arrangement or a reason, got {other:?}"),
    }

    // And asking twice changes nothing, which is the property that makes this
    // safe to run on somebody's machine: reading is not repairing.
    let again = client.call(&IpcCommand::GetStartAtLogin);
    match (&before, &again) {
        (
            Ok(Reply::StartAtLogin {
                start_at_login: first,
            }),
            Ok(Reply::StartAtLogin {
                start_at_login: second,
            }),
        ) => {
            assert_eq!(first, second, "reading the arrangement changed it");
        }
        (Err(_), Err(_)) => {}
        (first, second) => {
            panic!("the same question was answered two ways: {first:?} then {second:?}")
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

/// What the recorder says about capture and encoding.
fn diagnostics(client: &mut Client) -> Diagnostics {
    match client
        .call(&IpcCommand::GetDiagnostics)
        .expect("a recorder can say how it is capturing and what it can encode")
    {
        Reply::Diagnostics { diagnostics } => diagnostics,
        other => panic!("`get_diagnostics` was answered with {other:?}"),
    }
}

#[test]
fn a_recorder_reports_what_this_machine_can_encode_without_a_terminal() {
    // Issue #302's second acceptance criterion, against a real recorder over a
    // real pipe: what `clipped-recorder capabilities` prints, obtained by a
    // client that has no terminal to read.
    //
    // It needs no GPU. A machine with no hardware encoder at all still has
    // adapters — Windows always presents at least the Basic Render Driver — and
    // still has the software encoder, and "Clipped found no NVIDIA card" is
    // exactly the report this exists to deliver.
    let recorder = ServedRecorder::start("diagnostics-encoders");
    let mut client = recorder.client();

    let diagnostics = diagnostics(&mut client);
    let encoders = &diagnostics.encoders;

    assert!(
        !encoders.encoders.is_empty(),
        "every build knows the four encoder families of SPEC.md section 9, present or not:          {encoders:?}"
    );
    for family in ["nvenc", "amf", "quick_sync", "software"] {
        let summary = encoders
            .encoders
            .iter()
            .find(|summary| summary.encoder == family)
            .unwrap_or_else(|| panic!("`{family}` should be reported: {encoders:?}"));
        assert!(
            !summary.label.is_empty(),
            "an encoder with no label is a row a window cannot draw: {summary:?}"
        );
        // The half a report of bare ticks would hide: what the machine can do,
        // and what *this build* can do with it, are different questions
        // (AGENTS.md sections 27 and 54).
        assert_eq!(
            summary.implemented,
            family != "quick_sync",
            "the encoders this build has a proven backend for are the ones              `EncoderKind::is_implemented` names: {summary:?}"
        );
        assert_eq!(
            summary.available,
            summary.unavailable.is_none(),
            "an encoder that cannot be used says why, and one that can does not: {summary:?}"
        );
        // An encoder that names an adapter names one that is in the list beside
        // it. The two halves are mapped separately — encoders carry a per-boot
        // identifier and adapters carry a model name — so a mismatch here is a
        // window drawing "NVENC, on an adapter this report does not mention".
        if let Some(adapter) = &summary.adapter {
            assert!(
                encoders
                    .adapters
                    .iter()
                    .any(|candidate| &candidate.description == adapter),
                "`{adapter}` is not one of the adapters this report lists: {encoders:?}"
            );
        }
    }

    assert!(
        !encoders.adapters.is_empty(),
        "Windows always presents at least the Basic Render Driver, so an empty adapter list          means the report was never taken: {encoders:?}"
    );
    assert!(
        encoders
            .adapters
            .iter()
            .filter(|adapter| adapter.captures)
            .count()
            <= 1,
        "a recording creates its graphics device on one adapter, so at most one is marked:          {encoders:?}"
    );

    // Nothing is being recorded, so there is no capture backend running and the
    // account is absent rather than a guess at which one would be chosen.
    assert_eq!(
        diagnostics.capture, None,
        "an idle recorder has no capture backend to name: {diagnostics:?}"
    );

    drop(client);
    recorder.stop();
}

#[test]
fn a_recorder_that_can_report_diagnostics_says_so_in_its_welcome() {
    // The check a window makes before it draws either row. A recorder built
    // before issue #302 refuses `get_diagnostics` with `unknown_command`, and
    // "this machine has no encoder" and "nobody asked" are the two readings a
    // screen must never confuse — which is the whole reason the capability is
    // named rather than inferred from an empty answer.
    let recorder = ServedRecorder::start("diagnostics-welcome");
    let mut client = recorder.client();

    assert!(
        client
            .welcome()
            .features
            .iter()
            .any(|feature| feature == features::DIAGNOSTICS),
        "a recorder that performs `get_diagnostics` advertises it: {:?}",
        client.welcome().features
    );
    // And it does perform it, which is the half a feature list cannot promise
    // on its own: `replay` was advertised by a build that refused `save_replay`
    // with `not_implemented` for two milestones.
    let _ = diagnostics(&mut client);

    drop(client);
    recorder.stop();
}

#[test]
fn a_recorder_carries_no_path_into_its_diagnostics() {
    // Issue #302's fourth acceptance criterion, and the one worth a test of its
    // own. The capability cache lives at
    // `%LOCALAPPDATA%\Clipped\encoder-capabilities.json`, the terminal report
    // prints where it is, and this reply must not: the path runs through the
    // user's account name (AGENTS.md section 13, `docs/logging.md`).
    //
    // Asserted over the **bytes of the frame**, not over a parsed reply. A path
    // in a field this build does not define would be invisible to a
    // `Diagnostics` and would still be in what somebody pastes into a bug
    // report, which is the difference this test exists to make.
    let recorder = ServedRecorder::start("diagnostics-no-paths");

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
    let _welcome: ServerMessage = read_message(&mut connection).expect("the recorder welcomes");

    let request = IpcCommand::GetDiagnostics
        .to_request(1)
        .expect("the command has no parameters to fail on");
    write_message(&mut connection, &ClientMessage::Request(request)).expect("the request is sent");
    let reply: ServerMessage = read_message(&mut connection).expect("the recorder answers");
    let json = serde_json::to_string(&reply).expect("the frame serialises");

    // A Windows path in JSON is `C:\Users\...`, so the account name and the
    // directories are what to look for rather than a separator.
    for leak in ["Users", "AppData", "Clipped\\\\", "encoder-capabilities"] {
        assert!(
            !json.contains(leak),
            "`{leak}` reached a diagnostics reply, which is a path leaving this machine inside \
             something a user is asked to paste into a bug report: {json}"
        );
    }

    drop(connection);
    recorder.stop();
}

/// One action's row, or a failure naming the action that is missing.
fn row<'a>(hotkeys: &'a [HotkeyBinding], action: &str) -> &'a HotkeyBinding {
    hotkeys
        .iter()
        .find(|row| row.action == action)
        .unwrap_or_else(|| panic!("`{action}` should be in the report: {hotkeys:?}"))
}

/// Turns "this machine could not register a hotkey" from a pass into a failure,
/// exactly as `crates/hotkeys/tests/windows_hotkeys.rs` does and through the
/// same variable, so that one machine's answer covers both suites.
const REQUIRE_HOTKEYS: &str = "CLIPPED_REQUIRE_HOTKEYS";

/// How many spare function keys there are to choose from: `F13` to `F24`, which
/// no keyboard has and nothing binds.
const SPARE_FUNCTION_KEYS: u32 = 12;

/// The two combinations the rebind test moves a binding between.
///
/// **This is how the test avoids the recorder the person at the keyboard is
/// running.** That recorder holds `Ctrl`+`F10` and `Ctrl`+`F9` — the shipped
/// defaults — and a test that started a recorder on the defaults would ask
/// Windows for combinations it has already given away, so every row would come
/// back a conflict and the rebind would prove nothing. The recorder this test
/// starts is given a settings file naming these instead, and its other actions
/// are unbound, so it asks for nothing anybody else has.
///
/// Derived from this process's identifier for the reason
/// `crates/hotkeys/tests/windows_hotkeys.rs` derives its own that way: two
/// checkouts running the suite at once must not fight over one registration.
fn combinations_for_this_process() -> (String, String) {
    let first = std::process::id() % SPARE_FUNCTION_KEYS;
    let second = (first + 1) % SPARE_FUNCTION_KEYS;
    (
        format!("Ctrl+Alt+Shift+F{}", first + 13),
        format!("Ctrl+Alt+Shift+F{}", second + 13),
    )
}

/// Writes the settings file a recorder started under `home` will read, with
/// `save_replay` on `combination` and every other action bound to nothing.
fn write_hotkeys_file(home: &Path, combination: &str) {
    let directory = home.join("AppData").join("Local").join("Clipped");
    std::fs::create_dir_all(&directory).expect("the recorder's directory can be made");
    std::fs::write(
        directory.join("settings.json"),
        // `null` is a deliberate unbinding rather than an absent key, which is
        // what keeps `add_bookmark` off the `Ctrl`+`F9` it holds by default
        // (`docs/configuration.md`).
        format!(
            "{{\n  \"version\": 1,\n  \"hotkeys\": {{\n    \"save_replay\": \"{combination}\",\n \
             \"add_bookmark\": null\n  }}\n}}\n"
        ),
    )
    .expect("the settings file can be written");
}

/// Whether something on this machine holds `combination`, asked by trying to
/// register it here.
///
/// This is the question the whole rebind test turns on, and it is not one the
/// recorder can be asked: the recorder's report says what it *asked for*, and a
/// report is what a recorder that changed nothing but its own bookkeeping would
/// also produce. `RegisterHotKey` is the only witness, so this process becomes
/// the second application competing for the combination and reports what
/// Windows said.
///
/// [`None`] when no combination could be registered here at all, which is a
/// machine this test cannot run on rather than an answer.
fn is_held_by_something(combination: &str) -> Option<bool> {
    let hotkey: clipped_hotkeys::Hotkey = combination
        .parse()
        .expect("a combination this test writes down");
    let mut bindings = clipped_hotkeys::Bindings::empty();
    bindings
        .bind(clipped_hotkeys::HotkeyAction::SaveReplay, hotkey)
        .expect("one binding cannot collide with itself");

    let (service, events) =
        clipped_hotkeys::HotkeyService::start(&bindings, clipped_hotkeys::Handlers::new()).ok()?;
    drop(events);
    let state = service
        .registration()
        .statuses()
        .iter()
        .find(|status| status.action() == clipped_hotkeys::HotkeyAction::SaveReplay)
        .expect("every action has a status")
        .state()
        .clone();
    // Before the answer is used, so that this process is never left holding a
    // combination the recorder is about to be asked for.
    service.stop();

    match state {
        clipped_hotkeys::BindingState::Bound => Some(false),
        clipped_hotkeys::BindingState::Conflict(_) => Some(true),
        // Unreachable: the binding above is not `None`.
        clipped_hotkeys::BindingState::Unbound => None,
    }
}

/// Reports that this machine cannot run the hotkey half of a test, and fails
/// under [`REQUIRE_HOTKEYS`].
fn cannot_register(reason: &str) {
    assert!(
        std::env::var_os(REQUIRE_HOTKEYS).is_none_or(|value| value.is_empty()),
        "{REQUIRE_HOTKEYS} is set, so this must not be skipped: {reason}"
    );
    // Through `stderr()` rather than `eprintln!`, which libtest captures: a skip
    // nobody sees is the failure this exists to prevent.
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr(), "SKIPPED (hotkeys): {reason}");
}

/// Issue #233: a binding changed from the window takes effect on this recorder,
/// without restarting it.
///
/// Everything before this issue made that impossible by construction rather
/// than by omission. `serve` read the configuration once, registered from it
/// before the ready line, and published the report into a `OnceLock` — so the
/// only way to change a combination was to edit `settings.json` and start
/// Clipped again, and the settings screen could not offer the control at all
/// because `hotkeys` was not a settable key.
///
/// **The proof is `RegisterHotKey`, not the report.** A recorder that saved the
/// setting, redrew its own report and left Windows pointing the old combination
/// at itself would satisfy every assertion made against `get_hotkeys` alone;
/// that recorder is exactly what this repository shipped until now. So this
/// test competes for both combinations from its own process, before and after,
/// and the two answers have to swap over.
#[test]
fn a_hotkey_changed_over_the_protocol_is_registered_without_restarting_the_recorder() {
    let (first, second) = combinations_for_this_process();
    let home = scratch_home("rebind");
    write_hotkeys_file(&home, &first);

    let recorder = ServedRecorder::start_under("rebind", Some(&home));
    // Registration happens before the ready line — it has to, or a window
    // connecting the instant it appears races it (`crate::serve`) — so this
    // line is already in the stream by the time the client connects, and
    // waiting for it is not a race with the thing under test. It is here to
    // fail loudly on the machine where nothing registered at all, rather than
    // leaving that to look like a rebind that did not happen.
    recorder.wait_for("the global hotkeys were registered");

    let mut client = recorder.client();
    let before = row(&hotkey_report(&mut client), "save_replay").clone();
    assert_eq!(
        before.hotkey.as_deref(),
        Some(first.as_str()),
        "the recorder should have registered what its settings file names: {before:?}",
    );

    match is_held_by_something(&first) {
        None => {
            cannot_register("no combination can be registered in this session at all");
            drop(client);
            recorder.stop();
            return;
        }
        Some(false) => {
            cannot_register(&format!(
                "the recorder did not get {first} — something else on this machine holds it, so \
                 there is no registration to move: {before:?}"
            ));
            drop(client);
            recorder.stop();
            return;
        }
        Some(true) => {}
    }

    // The change, exactly as the settings screen sends one.
    let saved = settings_of(
        client
            .call(&IpcCommand::ApplySettings(change(
                "hotkey_save_replay",
                Some(&second),
            )))
            .expect("a combination is a value the settings file can hold"),
    );
    assert_eq!(
        setting(&saved, "hotkey_save_replay").value,
        second,
        "the reply should carry what was saved",
    );

    // What the recorder says, which is read out of the live registration rather
    // than out of the file (`crate::hotkeys::RegisteredHotkeys::report`).
    let after = row(&hotkey_report(&mut client), "save_replay").clone();
    assert_eq!(
        after.hotkey.as_deref(),
        Some(second.as_str()),
        "the recorder still reports the old combination, so nothing rebound it: {after:?}",
    );

    // And what Windows says, which is the half a report cannot fake. Both
    // directions: holding the new one is not enough on its own, because a
    // recorder that registered the new combination and never released the old
    // one would leave the user with a key they thought they had moved.
    assert_eq!(
        is_held_by_something(&second),
        Some(true),
        "the recorder does not hold {second}, so the change reached its settings file and its \
         report and stopped there — which is what issue #233 is: {after:?}",
    );
    assert_eq!(
        is_held_by_something(&first),
        Some(false),
        "the recorder is still holding {first} after being moved off it, so the combination the \
         user gave up is one nothing can have until Clipped is restarted",
    );

    drop(client);
    let diagnostics = recorder.stop();
    assert!(
        diagnostics.contains("a hotkey was rebound"),
        "a rebind is a thing that happened to the user's keyboard and belongs in the log \
         (AGENTS.md section 15):\n{diagnostics}",
    );
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

    // Issue #302's first acceptance criterion, and the only place it can be
    // checked: a capture backend exists only while something is being captured,
    // and it is chosen on the capture thread by a `CaptureFallback` that is
    // dropped as soon as the frame loop starts. A unit test of the publisher
    // could not show that the reading reaches a client at all.
    let while_recording = diagnostics(&mut client);
    let capture = while_recording
        .capture
        .as_ref()
        .expect("a recording in progress is capturing with something, and says which");

    assert_eq!(
        capture.setting, "Automatic",
        "a recording nobody pinned a method for asked for whichever one works: {capture:?}"
    );
    assert!(
        !capture.current.is_empty(),
        "a capture backend with no name is a row a window cannot draw: {capture:?}"
    );
    assert_eq!(
        capture.current, capture.started_with,
        "nothing replaced the backend during this recording, so the two agree — and an empty          change list is what says so rather than an absence: {capture:?}"
    );
    for change in &capture.changes {
        // Whatever is here is a real fall-through from start-up, so every field
        // of it has to be worth drawing.
        assert!(
            !change.reason.is_empty() && !change.trigger.is_empty(),
            "a capture change that explains nothing is a row with no purpose: {change:?}"
        );
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

    // And the capture account goes with it. A recorder that went on reporting
    // the last backend it used would be answering "what is capturing" with a
    // reading of what was.
    assert_eq!(
        diagnostics(&mut client).capture,
        None,
        "nothing is being recorded, so there is no capture backend to name"
    );

    drop(client);
    recorder.stop();
}

#[test]
#[ignore = "needs a GPU, an encoder and a desktop session; see tests/record_end_to_end.rs"]
fn a_watching_recorder_moves_through_all_three_states_over_the_protocol() {
    // The third state, against the recorder the product actually runs: one that
    // watches for games *and* serves the protocol. The two states that need no
    // hardware are asserted by
    // `a_recorder_watching_for_games_is_told_apart_from_one_that_is_not`, which
    // runs in CI; this is the one that needs an encoder and a desktop.
    //
    // The assertion at the end is the one only this shape can make: a recorder
    // that has just finished a recording goes back to **watching**, not to
    // idle. Nothing else in the suite exercises a status that has to be worked
    // out from two things at once.
    let home = scratch_home("three-states");
    let output = home.join("over-ipc.mkv");
    let pattern = support::PatternApp::start(SOURCE_FPS, 120);

    let recorder =
        ServedRecorder::started_with("three-states", Some(&home), &["--watch-for-games"]);
    let mut client = recorder.client();

    let watching = RecorderStatus::Watching(clipped_ipc::Watching { session: None });
    assert_eq!(
        status_of(&mut client),
        watching,
        "nothing has launched and nothing was asked for",
    );

    let started = client
        .call(&IpcCommand::StartRecording(StartRecording {
            pid: Some(pattern.process_id()),
            output: Some(output.to_string_lossy().into_owned()),
            overwrite: true,
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

    match status_of(&mut client) {
        RecorderStatus::Recording(active) => assert_eq!(
            active.recording_id, recording_id,
            "a recorder that is watching and recording is recording: that is the thing a window \
             has to be able to see and stop",
        ),
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
    assert!(
        summary.frames_encoded > 0,
        "a recording of no frames is not a recording: {summary:?}"
    );

    assert_eq!(
        status_of(&mut client),
        watching,
        "and back to watching rather than to idle: this recorder will still record the next game \
         that launches, and a window told `idle` would draw a recorder that had stopped watching",
    );

    drop(client);
    recorder.stop();
    let _ = std::fs::remove_dir_all(&home);
}

/// What the recorder says it is doing.
fn status_of(client: &mut Client) -> RecorderStatus {
    match client
        .call(&IpcCommand::GetStatus)
        .expect("status is answered")
    {
        Reply::Status { status } => status,
        other => panic!("expected a status, got {other:?}"),
    }
}

/// The overlay entry that makes a recorder treat the pattern application as a
/// game.
///
/// A user's own file, exactly as somebody registering an unknown executable
/// would write it (`docs/game-detection.md`). The shipped catalogue must never
/// name it: that file is compiled into every build, and a test application in it
/// would have Clipped recording a test application on somebody's machine
/// (`tests/automatic_sessions.rs`).
const PATTERN_OVERLAY: &str = r#"
schema_version = 1

[[game]]
game_id = "clipped-video-pattern"
name = "Clipped Video Pattern"
[[game.executables]]
name = "video-pattern.exe"
"#;

/// Writes that overlay into a scratch home, where the recorder reads one.
fn overlay_naming_the_pattern(home: &Path) {
    let application_directory = home.join("AppData").join("Local").join("Clipped");
    std::fs::create_dir_all(&application_directory).expect("the data directory can be made");
    std::fs::write(application_directory.join("games.toml"), PATTERN_OVERLAY)
        .expect("the games file can be written");
}

/// Collects every event a subscription delivers until the recorder stops.
///
/// Reading on a thread rather than in the test, for the reason
/// `a_client_that_does_not_ask_for_export_progress_is_not_sent_any` does it:
/// the assertion is about what did and did not arrive over a whole run, and
/// `recorder.stop()` is what ends the loop.
fn collecting_events(recorder: &ServedRecorder) -> std::thread::JoinHandle<Vec<Event>> {
    let events = EventClient::subscribe(
        recorder.endpoint(),
        CLIENT_NAME,
        "0.0.0",
        vec![EventStream::Status],
        PATIENCE,
    )
    .expect("the status stream is delivered");

    std::thread::spawn(move || {
        let mut events = events;
        let mut seen = Vec::new();
        while let Ok(event) = events.next_event() {
            seen.push(event);
        }
        seen
    })
}

/// The sitting a `session_ended` event carried, or a failure naming what did
/// arrive.
fn the_sitting_that_ended(seen: &[Event], diagnostics: &str) -> clipped_ipc::SessionSummary {
    seen.iter()
        .find_map(|event| match event {
            Event::SessionEnded { session } => Some(session.clone()),
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!(
                "no `session_ended` event was sent for a sitting that ended: {seen:#?}\n\
                 {diagnostics}"
            )
        })
}

#[test]
#[ignore = "needs a GPU, an encoder and a desktop session; see tests/record_end_to_end.rs"]
fn a_recording_names_the_game_it_is_of_and_its_sitting_is_announced_when_it_ends() {
    // Both of issue #241's remaining acceptance criteria, over a real pipe and
    // against a real recording — which is the only place they can be checked,
    // because both are producers. `ActiveRecording::session` was `None` for
    // every recording this recorder made and `Event::SessionEnded` was sent by
    // nothing, and a unit test of either producer is exactly the test that
    // would have passed while they were missing.
    //
    // The overlay is what makes the game name real: without it the pattern is a
    // window the catalogue claims nothing about, the sitting is unattributed,
    // and "a recording that can name its game" is not something this test could
    // tell from a recorder that always says nothing.
    let Some(_tools) = clipped_media_validation::require_media_tools() else {
        return;
    };

    let home = scratch_home("recording-sitting");
    overlay_naming_the_pattern(&home);
    let output = home.join("over-ipc.mkv");
    let pattern = support::PatternApp::start(SOURCE_FPS, 120);

    let recorder = ServedRecorder::start_under("recording-sitting", Some(&home));
    let reader = collecting_events(&recorder);
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
        .expect("the recording starts");
    let recording_id = match started {
        Reply::RecordingStarted { recording_id, .. } => recording_id,
        other => panic!("expected a started recording, got {other:?}"),
    };

    std::thread::sleep(RECORD_FOR);

    let session = match status_of(&mut client) {
        RecorderStatus::Recording(active) => {
            assert_eq!(active.recording_id, recording_id);
            assert!(
                active.target.contains(&pattern.process_id().to_string()),
                "`target` is the selector the user gave, which is what makes the game name \
                 necessary: {}",
                active.target,
            );
            *active
                .session
                .expect("every recording this recorder makes belongs to a sitting")
        }
        other => panic!("the recorder should be recording, not {other:?}"),
    };
    assert_eq!(
        session.game_name.as_deref(),
        Some("Clipped Video Pattern"),
        "a window cannot turn a capture selector into a game name without the catalogue, which \
         is why the sitting is on the status",
    );
    assert_eq!(
        session.game_id.as_deref(),
        Some("clipped-video-pattern"),
        "and the identifier the library will file it under, so a window can find it again",
    );
    assert_eq!(session.recordings.len(), 1, "{:?}", session.recordings);
    assert_eq!(session.recordings[0].output, output.to_string_lossy());
    assert_eq!(
        session.recordings[0].outcome, None,
        "the file is still being written, which is what an absent outcome means",
    );

    // And as it actually goes down the pipe. `ActiveRecording::session` is
    // `skip_serializing_if`, so an absent sitting and an empty one are the same
    // thing to a parsed reply and different bytes on the wire — and the wire is
    // what a window reads.
    let frame = status_on_the_wire(&recorder);
    assert!(
        frame.contains(r#""session":{"session_id":"clipped-video-pattern-"#),
        "the recording on the wire carries no sitting: {frame}",
    );
    assert!(
        frame.contains(r#""game_name":"Clipped Video Pattern""#),
        "the sitting on the wire cannot name the game: {frame}",
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
    assert!(
        summary.frames_encoded > 0,
        "a recording of no frames is not a recording: {summary:?}"
    );

    drop(client);
    drop(pattern);
    let diagnostics = recorder.stop();
    let seen = reader.join().expect("the events thread does not panic");
    let ended = the_sitting_that_ended(&seen, &diagnostics);

    assert_eq!(
        ended.session_id, session.session_id,
        "the sitting that ended is the one the status was carrying",
    );
    assert!(
        ended.ended_at.is_some(),
        "what makes a sitting over is `ended_at`: {ended:?}",
    );
    assert_eq!(
        ended.end_reason.as_deref(),
        Some("recording-ended"),
        "a sitting somebody's recording was the whole of ends when that recording does",
    );
    assert_eq!(
        ended.recordings.len(),
        1,
        "the event carries the files rather than an identifier to look up, because the library \
         may not have indexed one of them yet: {:?}",
        ended.recordings,
    );
    assert_eq!(ended.recordings[0].output, output.to_string_lossy());
    assert_eq!(ended.recordings[0].outcome.as_deref(), Some("recorded"));
    assert!(
        ended.recordings[0]
            .duration_ms
            .is_some_and(|duration| duration > 0),
        "and how long it runs for: {:?}",
        ended.recordings[0],
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// The size the subject's window is dragged to, mid-recording.
///
/// The same figure `tests/automatic_sessions.rs` and
/// `tests/record_end_to_end.rs` use, and for the same two reasons: it differs
/// from the size the subject starts at, so the file is told apart by its own
/// dimensions, and it is **even in both dimensions** because an odd client area
/// is a defect of its own
/// ([issue #561](https://github.com/wildware-uk/clipped/issues/561),
/// [ADR 0013](../../../docs/adr/0013-capture-rounds-an-odd-dimension-away.md)).
const RESIZED_TO: (u32, u32) = (1024, 576);

/// How long a recording that is expected to end by itself is given to do so.
///
/// Generous on purpose: the capture has to see a frame of the new size, discard
/// it and report the change, and the recording then has to flush an encoder and
/// write a Matroska trailer. A bound tight enough to trip on a busy machine is a
/// failure nobody can tell from a real one (AGENTS.md section 25).
const ENDING_PATIENCE: Duration = Duration::from_secs(30);

#[test]
#[ignore = "needs a GPU, an encoder and a desktop session; see tests/record_end_to_end.rs"]
fn a_resize_ends_a_recording_the_desktop_asked_for_and_the_sitting_says_why() {
    // [Issue #625](https://github.com/wildware-uk/clipped/issues/625)'s second
    // path, against a resize this test really makes.
    //
    // ADR 0012 has an automatic session follow a size change with a second file;
    // a recording somebody asked for over this protocol keeps stopping, because
    // `ManualSession` is one recording by construction and the file it produces
    // is the one path the request named. The decision this checks is that the
    // stopping is no longer *silent*: the recorder announces the sitting, and
    // the announcement now carries **why each file ended**, so a window can say
    // "a size change finished this, and here is the file" rather than going from
    // "Recording" to "not recording" with nothing in between.
    //
    // The load-bearing assertion is `end_reason`. Every other one below — that
    // the event arrives, that it names the file, that the outcome is `recorded`,
    // that the recorder went idle, that the media decodes — passes unchanged on
    // a build that never puts the reason on the wire, which is exactly the shape
    // issue #624 warned about: the structure being right is not evidence the
    // decision is implemented.
    let Some(_tools) = clipped_media_validation::require_media_tools() else {
        return;
    };

    let home = scratch_home("resized-sitting");
    overlay_naming_the_pattern(&home);
    let output = home.join("until-resized.mkv");
    let mut pattern = support::PatternApp::start(SOURCE_FPS, 300);
    let before = pattern.client_size();
    assert_ne!(
        before, RESIZED_TO,
        "the subject already has the size this test resizes it to, so the resize would change \
         nothing and the recording would not end"
    );

    let recorder = ServedRecorder::start_under("resized-sitting", Some(&home));
    let reader = collecting_events(&recorder);
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
        .expect("the recording starts");
    let recording_id = match started {
        Reply::RecordingStarted { recording_id, .. } => recording_id,
        other => panic!("expected a started recording, got {other:?}"),
    };

    std::thread::sleep(RECORD_FOR);
    match status_of(&mut client) {
        RecorderStatus::Recording(active) => assert_eq!(
            active.recording_id, recording_id,
            "some other recording was running when the window was resized",
        ),
        other => panic!("the recording should still have been running, not {other:?}"),
    }

    // A real `SetWindowPos` on the subject's real window, from outside the
    // process that owns it — which is what a user dragging an edge is. Nothing
    // tells the recorder; it finds out through capture.
    support::resize(pattern.window(), RESIZED_TO);
    assert_eq!(
        support::client_area(pattern.window()),
        RESIZED_TO,
        "the window did not actually change size, so nothing below would be about a resize"
    );

    // The recording ends **by itself**, which is the case with no reply to carry
    // a reason: nothing here asks it to stop.
    let deadline = std::time::Instant::now() + ENDING_PATIENCE;
    while status_of(&mut client).is_recording() {
        assert!(
            std::time::Instant::now() < deadline,
            "the recording was still running {:.0}s after the window changed size; ADR 0012 has \
             a size change finish the file where it is",
            ENDING_PATIENCE.as_secs_f64()
        );
        std::thread::sleep(Duration::from_millis(200));
    }

    assert!(
        pattern.is_running(),
        "the subject must still have been running when the recording ended, or it ended because \
         its window went rather than because it changed size"
    );

    drop(client);
    drop(pattern);
    let diagnostics = recorder.stop();
    let seen = reader.join().expect("the events thread does not panic");
    let ended = the_sitting_that_ended(&seen, &diagnostics);

    assert_eq!(
        ended.recordings.len(),
        1,
        "a recording somebody asked for is the whole of its sitting, and a resize does not give \
         it a second file: {:?}\n{diagnostics}",
        ended.recordings,
    );
    assert_eq!(
        ended.recordings[0].output,
        output.to_string_lossy(),
        "the file the sitting names is not the one that was asked for:\n{diagnostics}"
    );
    assert_eq!(
        ended.recordings[0].outcome.as_deref(),
        Some("recorded"),
        "the file was finished on purpose rather than abandoned:\n{diagnostics}"
    );

    // **The assertion this test exists for.** Everything above is true of a
    // build that says nothing at all about why the recording stopped.
    assert_eq!(
        ended.recordings[0].end_reason.as_deref(),
        Some("target-resized"),
        "the sitting says nothing about why its file ended, so a window watching this recording \
         finish can name the file and cannot say that a size change is what finished it — which \
         leaves a recording cut short by somebody dragging a window's edge looking exactly like \
         one that ran to the end: {:?}\n{diagnostics}",
        ended.recordings[0],
    );

    // And the file itself, at the size the window was: finished rather than
    // abandoned, which is what makes ending here an honest answer rather than a
    // loss.
    clipped_media_validation::Media::open(&output)
        .unwrap_or_else(|error| {
            panic!("the recording is not usable at all: {error}\n{diagnostics}")
        })
        .validate()
        .stream_count(1)
        .video_stream_count(1)
        .video(
            clipped_media_validation::VideoStream::default()
                .resolution(before.0, before.1)
                // Pictures out of a decoder, not packets in a container: a file
                // that lists packets, declares one video stream and has
                // monotonic timestamps can still decode to nothing at all, and
                // every other assertion here passes on that file. The floor is
                // low because the exact count depends on when the resize
                // landed.
                .decoded_frames_at_least(30),
        )
        .monotonic_timestamps()
        .assert_valid();

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
#[ignore = "needs a GPU, an encoder, a desktop session and process detection; see the module docs"]
fn a_recording_the_watcher_started_names_the_game_over_the_protocol() {
    // The same criterion for the recording issue #241 is actually about: one
    // nobody asked for. It is the harder half, because the sitting is not the
    // recording's own — it belongs to the session manager on the watcher's
    // thread, and reaching a window with it crosses the seam issue #421 built.
    //
    // Nothing here starts a recording: the recorder is told to watch, the
    // pattern application launches, and everything after that is the product
    // doing what it does. What is asserted is what a window would draw.
    let Some(_tools) = clipped_media_validation::require_media_tools() else {
        return;
    };

    let home = scratch_home("watcher-sitting");
    overlay_naming_the_pattern(&home);

    let recorder =
        ServedRecorder::started_with("watcher-sitting", Some(&home), &["--watch-for-games"]);
    let mut client = recorder.client();
    assert_eq!(
        status_of(&mut client),
        RecorderStatus::Watching(clipped_ipc::Watching { session: None }),
        "nothing has launched yet, so this recorder is watching and in no sitting",
    );

    // A process that starts between the watcher's baseline snapshot and its
    // subscription is invisible to it for its lifetime, and that window is a
    // few tens of milliseconds (`tests/automatic_sessions.rs` says the same).
    std::thread::sleep(Duration::from_secs(1));
    let pattern = support::PatternApp::start(SOURCE_FPS, 120);

    let session = the_sitting_being_recorded(&mut client, DETECTION_PATIENCE);
    assert_eq!(
        session.game_name.as_deref(),
        Some("Clipped Video Pattern"),
        "this is the sentence the issue asks for: not \"recording, process 4242\" but a \
         recording that names the game it is of",
    );
    assert!(
        session.session_id.starts_with("clipped-video-pattern-"),
        "and the sitting it belongs to, which is what makes the second file of one sitting \
         recognisable as such: {}",
        session.session_id,
    );
    assert_eq!(
        session.recordings.len(),
        1,
        "the file being written is part of the sitting while it is being written: {:?}",
        session.recordings,
    );

    let frame = status_on_the_wire(&recorder);
    assert!(
        frame.contains(r#""game_name":"Clipped Video Pattern""#),
        "the recording on the wire cannot name the game: {frame}",
    );

    match client
        .call(&IpcCommand::StopRecording(StopRecording {
            recording_id: None,
        }))
        .expect("the recording stops")
    {
        Reply::RecordingStopped { summary } => assert!(
            summary.frames_encoded > 0,
            "a recording of no frames is not a recording: {summary:?}"
        ),
        other => panic!("expected a summary, got {other:?}"),
    }

    // And the sitting outlives the recording, which is the flicker
    // `Watching::session` exists to prevent: the game is still running and its
    // sitting is still open.
    match status_of(&mut client) {
        RecorderStatus::Watching(watching) => assert_eq!(
            watching
                .session
                .expect("the sitting is still open, and the recorder is still in it")
                .session_id,
            session.session_id,
        ),
        other => panic!("the recorder should be watching again, not {other:?}"),
    }

    drop(client);
    drop(pattern);
    recorder.stop();
    let _ = std::fs::remove_dir_all(&home);
}

/// The microphone this test saves from the Settings screen.
///
/// Nothing on any machine is called this, and it never has to be: a name is
/// stored as it was written and resolved against the machine only when a
/// recording opens a device, which this one never reaches. What matters is that
/// it is not the default, so that a session record saying `default` is
/// unambiguously the answer the recorder booted with rather than a coincidence.
const CHOSEN_MICROPHONE: &str = "name:Issue 51 Microphone";

/// An overlay entry naming something that will never put a window on screen.
///
/// `shutdown_fixture` is the recorder's own Ctrl+C fixture: it starts, says
/// `ready`, and waits. That makes it a real process the watcher really reports
/// and really starts a recording of, on a machine with no GPU, no encoder and
/// no display — the recording ends as `no-window`, and the settings it was made
/// with are on disk long before that, because they are written when it *starts*
/// (`clipped_session::automatic::SessionManager::begin_recording`).
/// `tests/automatic_sessions.rs` uses the same fixture for the same reason.
const WINDOWLESS_OVERLAY: &str = r#"
schema_version = 1

[[game]]
game_id = "clipped-windowless"
name = "Clipped Windowless"
[[game.executables]]
name = "shutdown_fixture.exe"
"#;

/// Writes that overlay into a scratch home, where the recorder reads one.
fn overlay_naming_the_windowless_fixture(home: &Path) {
    let application_directory = home.join("AppData").join("Local").join("Clipped");
    std::fs::create_dir_all(&application_directory).expect("the data directory can be made");
    std::fs::write(application_directory.join("games.toml"), WINDOWLESS_OVERLAY)
        .expect("the games file can be written");
}

/// A process the watcher will report, which never shows a window.
///
/// In a process group of its own, so that the `CTRL_C_EVENT` that stops the
/// recorder cannot reach it and end this test's subject early.
#[derive(Debug)]
struct WindowlessSubject {
    child: Child,
}

impl WindowlessSubject {
    fn start(marker: &Path) -> Self {
        let mut child = Command::new(support::fixture_binary())
            .arg(marker)
            .stdout(Stdio::piped())
            .creation_flags(CREATE_NEW_PROCESS_GROUP)
            .spawn()
            .expect("the shutdown fixture can be started");

        // It says `ready` once it is up, which is what makes the wait that
        // follows a wait on the recorder noticing it rather than on this
        // process starting.
        let stdout = child.stdout.take().expect("stdout was piped");
        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .expect("the fixture announces itself");
        assert_eq!(
            line.trim(),
            "ready",
            "the fixture should have said it was ready",
        );

        Self { child }
    }
}

impl Drop for WindowlessSubject {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The first recording of the one sitting under `home`, once there is one.
///
/// Polled, because the session record appears in two steps: it is written when
/// the game is noticed, and the recording — carrying the settings it was
/// resolved at — is added to it when the recording starts. A reader that took
/// the first file it saw would read a sitting with nothing in it, which is not
/// the state this is about.
fn the_recording_the_watcher_started(home: &Path, within: Duration) -> serde_json::Value {
    let clips = home.join("Videos").join("Clipped");
    the_sitting_with_recordings_in(&[&clips], 1, within).1["recordings"][0].clone()
}

/// The one sitting under any of `folders`, once it holds `count` recordings, and
/// which folder it turned up in.
///
/// Polled, because a sitting appears on disk when the game is noticed and grows
/// a recording each time one starts, so a reader that took the first file it saw
/// would read a state that is not the one the test is about.
///
/// **Several folders, and the answer says which.** A test about where recordings
/// go has two ways to fail and they are different defects: the folder moved when
/// it should not have, and detection never started anything at all. Watching one
/// folder cannot tell them apart — both are a poller that times out — so this
/// watches every folder the recording could be in and hands back the one it
/// found, leaving the test to assert which. A guard that fails on "nothing ever
/// happened" is one nobody has seen fail for its own reason.
fn the_sitting_with_recordings_in(
    folders: &[&Path],
    count: usize,
    within: Duration,
) -> (std::path::PathBuf, serde_json::Value) {
    let deadline = std::time::Instant::now() + within;
    loop {
        let mut why = Vec::new();
        for folder in folders {
            match the_sitting_in(folder) {
                Ok(session) => {
                    let so_far = session["recordings"].as_array().map_or(0, Vec::len);
                    if so_far >= count {
                        return ((*folder).to_path_buf(), session);
                    }
                    why.push(format!(
                        "{} holds a sitting with {so_far} recording(s):\n{session:#}",
                        folder.display()
                    ));
                }
                Err(reason) => why.push(reason),
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no folder held a sitting with {count} recording(s) within {within:?}: {}",
            why.join("; "),
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Every session record in `clips`, by file name.
///
/// Empty for a folder that is not there, which is the honest answer for a
/// directory nothing has recorded into: a test asserting that a folder holds no
/// sitting must not be satisfied by a folder that could not be listed for some
/// other reason, and must not fail because nothing created it.
fn sittings_in(clips: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(clips) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.to_lowercase().ends_with(".session.json"))
        .collect();
    names.sort();
    names
}

/// The one session record in `clips`, or why there is not one yet.
fn the_sitting_in(clips: &Path) -> Result<serde_json::Value, String> {
    let entries = std::fs::read_dir(clips)
        .map_err(|error| format!("{} cannot be listed: {error}", clips.display()))?;
    let mut sidecars: Vec<std::path::PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.to_string_lossy()
                .to_lowercase()
                .ends_with(".session.json")
        })
        .collect();
    sidecars.sort();

    let [sidecar] = sidecars.as_slice() else {
        return Err(format!(
            "expected one session record in {}, found {sidecars:?}",
            clips.display()
        ));
    };
    let text = std::fs::read_to_string(sidecar)
        .map_err(|error| format!("the session record cannot be read: {error}"))?;
    let session: serde_json::Value = serde_json::from_str(&text)
        .map_err(|error| format!("the session record is not JSON: {error}\n{text}"))?;

    Ok(session)
}

#[test]
#[ignore = "needs a desktop session and process detection; see the module docs"]
fn a_microphone_saved_over_the_protocol_reaches_the_next_automatic_recording() {
    // SPEC.md section 45, step 3: a fresh user selects a microphone. Step 15:
    // they do the whole thing again without reconfiguring. And the sentence
    // that ends the section — "if any of those steps require ... restarting the
    // recorder, the MVP is not finished".
    //
    // Nothing here restarts anything. One recorder is started and told to
    // watch, and it is asked over the protocol to save a microphone exactly as
    // the Settings screen asks. Then a game launches and *detection* starts a
    // recording of it — nobody asks for that recording, which is the half of
    // the path that was broken: a recording the window asks for reads the
    // settings state at the moment it starts, but the session manager on the
    // watcher's thread owns a copy of the configuration, and until issue #51
    // nothing replaced that copy.
    //
    // What is asserted is the value the recording was actually made with, read
    // out of the session record the recorder itself wrote. Not a getter, and
    // not the reply to the save: `SessionManager::set_configuration` has always
    // passed a test that set a configuration and read it back, and that test
    // went on passing for as long as nothing in the product called it.
    let home = scratch_home("settings-reach-detection");
    overlay_naming_the_windowless_fixture(&home);

    let recorder = ServedRecorder::started_with(
        "settings-reach-detection",
        Some(&home),
        &["--watch-for-games"],
    );
    // Before the save, and this is the line the test turns on. `announce` is
    // written after the watcher thread has built its session manager, and the
    // manager takes its copy of the configuration when it is built. Waiting for
    // it is what makes the save below land *after* that copy was taken — which
    // is the only arrangement in which the assertion at the end means anything.
    // On the ready line alone this test passed with the propagation removed.
    let watching = recorder.wait_for("Watching for games.");
    let mut client = recorder.client();

    // What that copy holds is `default`, because this scratch home had no
    // settings file in it at all.
    let saved = settings_of(
        client
            .call(&IpcCommand::ApplySettings(change(
                "microphone",
                Some(CHOSEN_MICROPHONE),
            )))
            .expect("a device name is a value the settings file can hold"),
    );
    assert_eq!(
        setting(&saved, "microphone").value,
        CHOSEN_MICROPHONE,
        "the save itself has to land before anything can be said about what reads it",
    );

    // A process that starts between the watcher's baseline snapshot and its
    // subscription is invisible to it for its lifetime, and that window is a
    // few tens of milliseconds (`tests/automatic_sessions.rs` says the same).
    std::thread::sleep(Duration::from_secs(1));
    let subject = WindowlessSubject::start(&home.join("marker"));

    let recording = the_recording_the_watcher_started(&home, DETECTION_PATIENCE);
    let microphone = &recording["settings"]["microphone"];

    assert_eq!(
        microphone["value"].as_str(),
        Some(CHOSEN_MICROPHONE),
        "the recording detection started was made with the microphone this recorder held when it \
         booted rather than the one saved from the Settings screen a moment earlier, so the \
         Settings screen is a control that does nothing until the recorder is restarted — which \
         is what SPEC.md section 45 rules out.\n\nThe recording:\n{recording:#}\n\nWhat the \
         recorder said before the save:\n{watching}",
    );
    assert_eq!(
        microphone["source"].as_str(),
        Some("global"),
        "and it should be recorded as having come from the user's own settings rather than from \
         what Clipped ships with:\n{recording:#}",
    );

    drop(client);
    drop(subject);
    recorder.stop();
    // On the way out only. A run that failed keeps the home directory, and the
    // session record in it, for whoever has to read the assertion above.
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
#[ignore = "needs a desktop session and process detection; see the module docs"]
fn a_recording_directory_saved_over_the_protocol_reaches_the_next_automatic_recording() {
    // SPEC.md section 45 step 3 names two things — "select microphone **and
    // recording directory**" — and the section ends "if any of those steps
    // require ... restarting the recorder, the MVP is not finished". PR #608
    // fixed the microphone half and could not reach this one: the directory is
    // not in the `Configuration` that `set_configuration` replaces, it is frozen
    // into the session manager's `AutomaticSettings` before the watcher thread
    // starts.
    //
    // Nothing here restarts anything. One recorder is started and told to watch,
    // it is asked over the protocol to save a folder exactly as the Settings
    // screen asks, and then a game launches and *detection* starts a recording
    // of it. What is asserted is where the recorder itself wrote the session
    // record and the file it names — not a getter, and not the reply to the
    // save.
    let home = scratch_home("directory-reaches-detection");
    overlay_naming_the_windowless_fixture(&home);
    let chosen = home.join("Chosen Clips");

    let recorder = ServedRecorder::started_with(
        "directory-reaches-detection",
        Some(&home),
        &["--watch-for-games"],
    );
    // Not the ready line. `announce` is written after the watcher thread has
    // built its session manager, and that manager takes the directory when it is
    // built; a save on the strength of the ready line alone would race the very
    // snapshot this test exists to defeat, and would pass with the fix removed.
    let watching = recorder.wait_for("Watching for games.");
    let mut client = recorder.client();

    let saved = settings_of(
        client
            .call(&IpcCommand::ApplySettings(change(
                RECORDING_DIRECTORY,
                Some(&chosen.to_string_lossy()),
            )))
            .expect("an absolute folder is a value the settings file can hold"),
    );
    assert_eq!(
        setting(&saved, RECORDING_DIRECTORY).value,
        chosen.to_string_lossy(),
        "the save itself has to land before anything can be said about what reads it",
    );

    // No game is running, so there is no sitting to keep together and the change
    // is in force on the watcher's next pass — about a second. This is also the
    // answer to "what if they change it and then do not play for a week": the
    // change is not pending at all, and the week has nothing to do with it.
    std::thread::sleep(Duration::from_secs(2));
    let now = settings_of(
        client
            .call(&IpcCommand::GetSettings(clipped_ipc::GetSettings::default()))
            .expect("the settings can be read"),
    );
    assert_eq!(
        setting(&now, RECORDING_DIRECTORY).not_yet_in_force,
        None,
        "with nothing being recorded the folder is simply in force, and a screen that said it \
         was still waiting on something would be describing a delay that is not there:\n{now:#?}",
    );

    let subject = WindowlessSubject::start(&home.join("marker"));

    // Both folders are watched, so that a failure says *where the recording
    // went* rather than only that nothing turned up in the one this test wants.
    let was = home.join("Videos").join("Clipped");
    let (folder, sitting) = the_sitting_with_recordings_in(&[&chosen, &was], 1, DETECTION_PATIENCE);

    assert_eq!(
        folder, chosen,
        "the recording detection started was written to the folder this recorder booted with \
         rather than the one saved from the Settings screen a moment earlier, so choosing a \
         folder is a control that does nothing until the recorder is restarted — which is what \
         SPEC.md section 45 rules out.\n\nThe sitting:\n{sitting:#}\n\nWhat the recorder said \
         before the save:\n{watching}",
    );
    assert!(
        sitting["recordings"][0]["output"]
            .as_str()
            .is_some_and(|output| Path::new(output).starts_with(&chosen)),
        "and the file it names has to be in it, not merely the record of it:\n{sitting:#}",
    );
    assert!(
        sittings_in(&was).is_empty(),
        "and nothing should have been written to the folder that was left behind: {:?}",
        sittings_in(&was),
    );

    drop(client);
    drop(subject);
    recorder.stop();
    // On the way out only. A run that failed keeps the home directory, and both
    // folders in it, for whoever has to read the assertion above.
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
#[ignore = "needs a desktop session and process detection; see the module docs"]
fn a_recording_directory_saved_during_a_sitting_does_not_move_that_sitting() {
    // The guard the whole design is chosen for, and the one that fails silently
    // if it is wrong. A sitting is a sequence of recordings held together by one
    // session record, and `SessionManager::begin_recording` writes that record
    // *next to the files it names*. Move the directory half way through and the
    // record lists files that are no longer beside it — and nothing errors,
    // nothing is logged as lost and every file is still on disk, so the only
    // thing that has gone is the ability to say which sitting they belonged to
    // (AGENTS.md section 56).
    //
    // So: a sitting is opened, the folder is changed while it is open, and then
    // the same sitting is made to ask for a second file. Both files and the
    // record naming them have to be in the folder the sitting started in.
    let home = scratch_home("directory-mid-sitting");
    overlay_naming_the_windowless_fixture(&home);
    let clips = home.join("Videos").join("Clipped");
    let chosen = home.join("Chosen Clips");

    let recorder =
        ServedRecorder::started_with("directory-mid-sitting", Some(&home), &["--watch-for-games"]);
    recorder.wait_for("Watching for games.");
    let mut client = recorder.client();

    // A process that starts between the watcher's baseline snapshot and its
    // subscription is invisible to it for its lifetime, and that window is a few
    // tens of milliseconds (`tests/automatic_sessions.rs` says the same).
    std::thread::sleep(Duration::from_secs(1));
    let mut subject = Some(WindowlessSubject::start(&home.join("marker")));
    let (_, opened) = the_sitting_with_recordings_in(&[&clips], 1, DETECTION_PATIENCE);

    // Saved with the sitting open, which is the only state in which this
    // setting waits for anything.
    let saved = settings_of(
        client
            .call(&IpcCommand::ApplySettings(change(
                RECORDING_DIRECTORY,
                Some(&chosen.to_string_lossy()),
            )))
            .expect("an absolute folder is a value the settings file can hold"),
    );
    let entry = setting(&saved, RECORDING_DIRECTORY);
    assert_eq!(
        entry.value,
        chosen.to_string_lossy(),
        "the folder is saved either way; what differs is when it is used",
    );
    assert!(
        entry
            .not_yet_in_force
            .as_deref()
            .is_some_and(|sentence| sentence.contains(&*clips.to_string_lossy())),
        "a saved value that is not yet the one being used has to say so, and say where the \
         footage is going in the meantime — otherwise the folder picker is a control whose \
         effect is invisible (AGENTS.md section 27):\n{entry:#?}",
    );

    // The game goes and comes back inside the restart grace, which is one
    // sitting with two files in it (`docs/sessions.md`). The second file is what
    // makes this a test rather than an assertion about a getter: it is asked for
    // *after* the save, and it is the one that would land in the wrong folder.
    drop(subject.take());
    std::thread::sleep(Duration::from_secs(5));
    let subject = WindowlessSubject::start(&home.join("marker"));

    // Both folders again: a build that moved the directory mid-sitting would put
    // the second file — and the record naming both of them — in the new one,
    // and this is what says so instead of timing out on the old one.
    let (folder, sitting) =
        the_sitting_with_recordings_in(&[&clips, &chosen], 2, DETECTION_PATIENCE);
    let recordings = sitting["recordings"]
        .as_array()
        .unwrap_or_else(|| panic!("the sitting has no recordings:\n{sitting:#}"));
    assert_eq!(
        folder, clips,
        "the record of the sitting that was open moved to the folder saved during it, so it is \
         no longer beside the recordings it names — and every file is still on disk, so nothing \
         fails and nothing is left able to say which sitting they belonged to (AGENTS.md \
         section 56):\n{sitting:#}",
    );
    assert_eq!(
        sitting["session_id"], opened["session_id"],
        "the second file has to belong to the sitting that was already open, or this test is \
         about two sittings and proves nothing:\n{sitting:#}\n\nthe sitting that was open when \
         the folder was saved:\n{opened:#}",
    );
    assert!(
        recordings.iter().all(|recording| recording["output"]
            .as_str()
            .is_some_and(|output| Path::new(output).starts_with(&clips))),
        "every file of the sitting that was open must stay beside the session record that names \
         them. One of them moved with the setting, which leaves the record pointing at files \
         that are not there — and leaves nothing able to say which sitting they \
         belonged to (AGENTS.md section 56):\n{sitting:#}",
    );
    assert!(
        sittings_in(&chosen).is_empty(),
        "and no part of the open sitting — least of all its record — may be written into the \
         new folder while it is open: {:?}",
        sittings_in(&chosen),
    );

    // Still open, so the screen still says the folder is waiting on it.
    let now = settings_of(
        client
            .call(&IpcCommand::GetSettings(clipped_ipc::GetSettings::default()))
            .expect("the settings can be read"),
    );
    assert!(
        setting(&now, RECORDING_DIRECTORY)
            .not_yet_in_force
            .is_some(),
        "the sitting has not ended, so the answer has not changed:\n{now:#?}",
    );

    drop(client);
    drop(subject);
    recorder.stop();
    let _ = std::fs::remove_dir_all(&home);
}

/// How long the watcher is given to notice a launch and reach a window.
///
/// Generous on purpose: detection is deliberately unhurried — up to four and a
/// half seconds between a process starting and the launch being reported — and
/// a bound tight enough to trip on a busy machine is a failure nobody can tell
/// from a real one (AGENTS.md section 25).
const DETECTION_PATIENCE: Duration = Duration::from_secs(60);

/// The sitting of the recording this recorder is making, once it is making one.
///
/// Polled through `get_status` rather than waited for on the event stream, so
/// that a build which never records anything fails on this deadline instead of
/// blocking on a pipe that will never carry another frame.
fn the_sitting_being_recorded(
    client: &mut Client,
    within: Duration,
) -> clipped_ipc::SessionSummary {
    let deadline = std::time::Instant::now() + within;
    let mut last = None;
    while std::time::Instant::now() < deadline {
        match status_of(client) {
            RecorderStatus::Recording(active) => {
                if let Some(session) = active.session {
                    return *session;
                }
                last = Some(RecorderStatus::Recording(active));
            }
            other => last = Some(other),
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    panic!("no recording carrying a sitting within {within:?}; the last status was {last:?}")
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
        // Nothing has launched, so nothing is being recorded — and this
        // recorder will record the next game that does, which `idle` cannot
        // say. It answered `idle` here until issue #584, which is what
        // `a_recorder_watching_for_games_is_told_apart_from_one_that_is_not`
        // below is about.
        Reply::Status { status } => assert_eq!(
            status,
            RecorderStatus::Watching(clipped_ipc::Watching { session: None }),
        ),
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

#[test]
fn a_recorder_watching_for_games_is_told_apart_from_one_that_is_not() {
    // Issue #584. `RecorderStatus::Watching` was put on the wire by issue #241,
    // refused by name in `hotkeys.rs` by issue #421 and drawn by the tray — and
    // no recorder could enter it, so a recorder waiting for a game to launch
    // answered `idle`: the same word as one that will never record anything.
    // Issue #241 exists to stop exactly that.
    //
    // Both halves are here, in one test, because the distinction is the point.
    // A build that answered `watching` to everything would pass the first half
    // perfectly well and would tell every `serve` somebody started at a
    // terminal that it was about to record a game.
    let watching_home = scratch_home("watching-status");
    let watching = ServedRecorder::started_with(
        "watching-status",
        Some(&watching_home),
        &["--watch-for-games"],
    );

    let mut client = watching.client();
    match client
        .call(&IpcCommand::GetStatus)
        .expect("status is answered")
    {
        Reply::Status { status } => assert_eq!(
            status,
            RecorderStatus::Watching(clipped_ipc::Watching { session: None }),
            "a recorder that will record the next game to launch says so, and carries no sitting \
             until it is in one",
        ),
        other => panic!("expected a status, got {other:?}"),
    }

    // And as it actually goes down the pipe. `Watching::session` is
    // `skip_serializing_if`, so a recorder watching for anything at all is
    // `{"state":"watching"}` and nothing more — an empty sitting invented to
    // fill the field would be a game name with nothing behind it, and a parsed
    // reply cannot show the difference.
    let frame = status_on_the_wire(&watching);
    assert!(
        frame.contains(r#""status":{"state":"watching"}"#),
        "the status on the wire is not the watching state with no sitting: {frame}",
    );

    // The tray reads this rather than asking: a status subscription opens with
    // the state the recorder is in, which for a recorder started at login is
    // the only status event there will be for hours.
    let mut events = EventClient::subscribe(
        watching.endpoint(),
        CLIENT_NAME,
        "0.0.0",
        vec![EventStream::Status],
        PATIENCE,
    )
    .expect("the status stream is delivered");
    match events.next_event().expect("an event arrives") {
        Event::StatusChanged { status } => assert_eq!(
            status,
            RecorderStatus::Watching(clipped_ipc::Watching { session: None }),
            "a window that attaches to a watching recorder is told what it is, or it draws \
             \"not recording\" over a recorder that is about to record",
        ),
        other => panic!("expected an opening status event, got {other:?}"),
    }

    drop(events);
    drop(client);
    watching.stop();
    let _ = std::fs::remove_dir_all(&watching_home);

    // The other direction, and what makes the half above mean anything.
    let idle_home = scratch_home("idle-status");
    let idle = ServedRecorder::start_under("idle-status", Some(&idle_home));
    let mut client = idle.client();
    match client
        .call(&IpcCommand::GetStatus)
        .expect("status is answered")
    {
        Reply::Status { status } => assert_eq!(
            status,
            RecorderStatus::Idle,
            "nothing asked this recorder to watch for games, so it will record nothing until \
             something asks it to",
        ),
        other => panic!("expected a status, got {other:?}"),
    }

    drop(client);
    idle.stop();
    let _ = std::fs::remove_dir_all(&idle_home);
}

#[test]
fn only_a_recorder_that_records_games_by_itself_advertises_that_it_does() {
    // Issue #587. `features::AUTOMATIC` was defined, was documented as the
    // switch a window reads before it draws a screen saying whether games are
    // being recorded, and **no recorder advertised it** — so the one question
    // it exists to answer had no answer at all.
    //
    // It is a fact about this recorder rather than about the build. Since issue
    // #421 both kinds are the same binary: a plain `serve` records only what it
    // is asked to and reports `idle` for ever, and a `serve --watch-for-games`
    // records the next game to launch. The desktop application starts the
    // second (`SupervisorSettings::watch_for_games`), so a window that could
    // not tell them apart was describing the wrong one.
    //
    // Both directions are here, in one test, because that is the whole of it: a
    // build that advertised the feature unconditionally would pass the first
    // half perfectly well, and would tell somebody who started `serve` at a
    // terminal that Clipped was about to record whatever game they had open.
    let watching_home = scratch_home("automatic-feature");
    let watching = ServedRecorder::started_with(
        "automatic-feature",
        Some(&watching_home),
        &["--watch-for-games"],
    );
    let mut client = watching.client();

    let advertised = client.welcome().features.clone();
    assert!(
        advertised.iter().any(|name| name == features::AUTOMATIC),
        "a recorder that will record the next game to launch has to say so in its welcome, or a \
         window has no way to ask: {advertised:?}",
    );
    // The status agrees, because the feature is answered from the same claim
    // rather than from a second flag beside it: a recorder that advertised
    // `automatic` and then reported `idle` would be telling a window two
    // opposite things about itself in the same second.
    assert!(
        matches!(status_of(&mut client), RecorderStatus::Watching(_)),
        "the capability and the status are the same claim, so a recorder advertising `automatic` \
         cannot report that it will never record anything",
    );

    drop(client);
    watching.stop();
    let _ = std::fs::remove_dir_all(&watching_home);

    // The other direction, and what makes the half above mean anything.
    let idle_home = scratch_home("automatic-feature-off");
    let idle = ServedRecorder::start_under("automatic-feature-off", Some(&idle_home));
    let mut client = idle.client();

    let advertised = client.welcome().features.clone();
    assert!(
        !advertised.iter().any(|name| name == features::AUTOMATIC),
        "nothing asked this recorder to watch for games, so nothing may tell a window that it \
         will record one: {advertised:?}",
    );
    // And it still advertises what it can do. A recorder that had lost its
    // whole feature list would pass the assertion above for a reason that has
    // nothing to do with this issue.
    assert!(
        advertised.iter().any(|name| name == features::RECORDING),
        "a recorder that records what it is asked to still says so: {advertised:?}",
    );
    assert_eq!(
        status_of(&mut client),
        RecorderStatus::Idle,
        "and the status agrees in this direction too",
    );

    drop(client);
    idle.stop();
    let _ = std::fs::remove_dir_all(&idle_home);
}

/// The `get_status` reply exactly as it appears on the pipe.
///
/// Every other test here uses `clipped-ipc`'s own client, which is the right
/// tool for almost everything. This exists because one of the watching state's
/// properties is about what is *not* in the JSON, and a parsed reply cannot show
/// an absent field.
fn status_on_the_wire(recorder: &ServedRecorder) -> String {
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
    match read_message::<_, ServerMessage>(&mut connection).expect("the handshake is answered") {
        ServerMessage::Welcome(_) => {}
        other => panic!("expected a welcome, got {other:?}"),
    }

    write_message(
        &mut connection,
        &ClientMessage::Request(
            IpcCommand::GetStatus
                .to_request(1)
                .expect("a status request can be built"),
        ),
    )
    .expect("the request is sent");

    let mut length = [0_u8; 4];
    std::io::Read::read_exact(&mut connection, &mut length).expect("a reply arrives");
    let mut payload = vec![0_u8; u32::from_le_bytes(length) as usize];
    std::io::Read::read_exact(&mut connection, &mut payload).expect("the whole reply arrives");
    String::from_utf8(payload).expect("this protocol is JSON, which is UTF-8")
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
fn a_preview_of_a_recording_that_is_not_there_is_an_answer_and_not_a_refusal() {
    // Issue #448 over a real recorder, a real pipe and a real handshake, which
    // is the hop that says the command is *served* rather than merely defined:
    // a build that advertised `previews` and never dispatched the command would
    // pass every unit test in `crates/ipc` and in `apps/recorder/src/preview`.
    //
    // It asks about a recording that does not exist, deliberately. A path that
    // did exist would be one belonging to whoever is running the tests, and
    // asking about it would put it in the recorder's generation queue and write
    // a picture into their cache (AGENTS.md section 25). A path that is not
    // there is answered from a `stat` that fails, so nothing is queued and
    // nothing is written.
    let recorder = ServedRecorder::start("preview-missing");
    let mut client = recorder.client();

    assert!(
        client
            .welcome()
            .features
            .iter()
            .any(|feature| feature == features::PREVIEWS),
        "a recorder that answers `open_preview` has to say so, or a window will not ask: {:?}",
        client.welcome().features
    );

    let reply = client
        .call(&IpcCommand::OpenPreview(clipped_ipc::OpenPreview {
            source: r"D:\clips recording nobody has\match.mkv".to_owned(),
            kind: clipped_ipc::PreviewKind::Thumbnail,
            buckets: None,
        }))
        .expect("a recording that is not there is still an answer about a picture");

    match reply {
        Reply::PreviewOpened { preview } => {
            assert_eq!(
                preview.state,
                clipped_ipc::PreviewState::Unavailable,
                "a recording that cannot be read has no picture coming"
            );
            assert!(
                preview.reason.is_some(),
                "an unavailable preview says why: {preview:?}"
            );
            assert!(
                preview.picture.is_none() && preview.tracks.is_empty(),
                "nothing is drawn for it: {preview:?}"
            );
        }
        other => panic!("expected a preview, got {other:?}"),
    }

    // And a waveform of the same recording comes back the same way, over the
    // same command — which is the whole of #448's third criterion. A second
    // mechanism would be a second thing to keep in step with this one.
    let reply = client
        .call(&IpcCommand::OpenPreview(clipped_ipc::OpenPreview {
            source: r"D:\clips recording nobody has\match.mkv".to_owned(),
            kind: clipped_ipc::PreviewKind::Waveform,
            buckets: Some(256),
        }))
        .expect("the peaks are asked for the same way");

    match reply {
        Reply::PreviewOpened { preview } => {
            assert_eq!(preview.kind, clipped_ipc::PreviewKind::Waveform);
            assert_eq!(preview.state, clipped_ipc::PreviewState::Unavailable);
        }
        other => panic!("expected a preview, got {other:?}"),
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

#[test]
fn an_export_says_how_far_it_has_got_while_it_runs_and_ends_where_the_reply_does() {
    // Issue #446, over a real recorder process, a real named pipe and a real
    // file. The reply to `export_recording` arrives when the MP4's index has
    // been written, so anything a window can draw during the copy has to have
    // come down the events connection while the control connection was blocked
    // — which is what this drives both halves of.
    let Some(tools) = clipped_media_validation::require_media_tools() else {
        return;
    };

    let directory = clipped_media_validation::TemporaryDirectory::new("recorder-export-progress");
    let source = directory.file("match.mkv");
    let destination = directory.file("match.mp4");
    recording_to_export(tools.ffmpeg(), &source);

    let recorder = ServedRecorder::start("export-progress");

    // The recorder has to *say* it can do this before a window may ask, because
    // asking for a stream a recorder does not have is refused by name and the
    // refusal takes the whole events connection with it. A recorder that
    // published progress without advertising it would leave every window
    // choosing between no progress and no status.
    let mut control = recorder.client();
    assert!(
        control
            .welcome()
            .features
            .iter()
            .any(|feature| feature == clipped_ipc::features::EXPORT_PROGRESS),
        "this recorder publishes export progress and does not advertise it, so no window could \
         safely subscribe: {:?}",
        control.welcome().features
    );

    let events = EventClient::subscribe(
        recorder.endpoint(),
        CLIENT_NAME,
        "0.0.0",
        vec![EventStream::Exports],
        PATIENCE,
    )
    .expect("the exports stream is delivered");
    assert_eq!(events.streams(), [EventStream::Exports]);

    // Read on a thread of its own, because the export below blocks this one
    // until the copy has finished. That is the whole point: if progress only
    // arrived with the reply there would be nothing for this thread to have
    // missed. The loop ends when the recorder closes the connection at
    // shutdown.
    let reader = std::thread::spawn(move || {
        let mut events = events;
        let mut seen = Vec::new();
        while let Ok(event) = events.next_event() {
            match event {
                Event::ExportProgress { export } => seen.push(export),
                other => panic!("the exports stream carried something else: {other:?}"),
            }
        }
        seen
    });

    let summary = match export(&mut control, &source, &destination) {
        Reply::RecordingExported { export } => export,
        other => panic!("expected an export, got {other:?}"),
    };

    drop(control);
    recorder.stop();
    let seen = reader.join().expect("the events thread does not panic");

    assert!(
        seen.len() >= 2,
        "a copy of a {EXPORT_FIXTURE_SECONDS}-second recording produced {} progress events; one \
         is a bar that never moves, and none is the silence this ticket is about",
        seen.len()
    );

    // Every event names the export it belongs to, because nothing else does:
    // there is no request identifier on the event path, and a window matches
    // these against the files it asked for.
    for progress in &seen {
        assert_eq!(progress.source, summary.source, "{progress:?}");
        assert_eq!(progress.destination, summary.destination, "{progress:?}");
    }

    // It advances. A recorder that published the same figure repeatedly, or one
    // that published a single event, would satisfy "an event arrived" and fail
    // here.
    for pair in seen.windows(2) {
        assert!(
            pair[1].written_ms > pair[0].written_ms && pair[1].packets > pair[0].packets,
            "export progress went backwards or stood still: {:?} then {:?}",
            pair[0],
            pair[1]
        );
    }

    // And the last thing said during the copy agrees with the reply that ended
    // it. A bar that stopped at 80 % and was then replaced by "exported" leaves
    // somebody wondering what happened to the other fifth.
    let last = seen.last().expect("there is a progress event");
    assert_eq!(
        (last.written_ms, last.packets, last.bytes),
        (summary.duration_ms, summary.packets, summary.bytes),
        "the last progress event and the reply disagree about what was copied"
    );
    assert_eq!(
        last.fraction()
            .map(|fraction| (fraction * 100.0).round() as u32),
        Some(100),
        "the copy finished and the last event did not read as finished: {last:?}"
    );

    let _ = std::fs::remove_dir_all(directory.path());
}

#[test]
fn a_client_that_does_not_ask_for_export_progress_is_not_sent_any() {
    // The other half of the compatibility claim. A window that subscribes to
    // `status` — which is every window built before issue #446 — must not have
    // its subscription changed by this feature existing, and must not receive
    // events it has no case for.
    let Some(tools) = clipped_media_validation::require_media_tools() else {
        return;
    };

    let directory = clipped_media_validation::TemporaryDirectory::new("recorder-export-unasked");
    let source = directory.file("match.mkv");
    let destination = directory.file("match.mp4");
    recording_to_export(tools.ffmpeg(), &source);

    let recorder = ServedRecorder::start("export-unasked");

    let events = EventClient::subscribe(
        recorder.endpoint(),
        CLIENT_NAME,
        "0.0.0",
        vec![EventStream::Status],
        PATIENCE,
    )
    .expect("the status stream is delivered");

    let reader = std::thread::spawn(move || {
        let mut events = events;
        let mut seen = Vec::new();
        while let Ok(event) = events.next_event() {
            seen.push(event);
        }
        seen
    });

    let mut control = recorder.client();
    match export(&mut control, &source, &destination) {
        Reply::RecordingExported { .. } => {}
        other => panic!("expected an export, got {other:?}"),
    }

    drop(control);
    recorder.stop();
    let seen = reader.join().expect("the events thread does not panic");

    assert!(
        !seen
            .iter()
            .any(|event| matches!(event, Event::ExportProgress { .. })),
        "a `status` subscriber was sent export progress it never asked for: {seen:?}"
    );
    // And it still got what it did ask for, so the subscription was not broken
    // in the course of not being sent the other thing.
    assert!(
        seen.iter()
            .any(|event| matches!(event, Event::StatusChanged { .. })),
        "a `status` subscriber received nothing at all: {seen:?}"
    );

    let _ = std::fs::remove_dir_all(directory.path());
}
