//! A stand-in for the desktop application's supervision, for
//! `tests/supervision.rs` to start, kill and outlive.
//!
//! [Issue #106](https://github.com/wildware-uk/clipped/issues/106)'s acceptance
//! criteria are about processes ending: "killing the UI process", "killing the
//! recorder", "a second launch". None of those can be tested in-process — a
//! Rust panic unwinds and runs destructors, and destructors are exactly what a
//! killed process does not get (`crates/muxer/tests/abrupt_termination.rs` makes
//! the same point). So the thing being killed has to be a real process, and this
//! is it.
//!
//! # Why this is not the Tauri binary
//!
//! It should be, and it cannot be. `apps/desktop/src-tauri` is a second Cargo
//! workspace that `cargo test --workspace` does not reach, its build needs
//! `npm run build` to have produced `apps/desktop/dist` first, and running it
//! needs the WebView2 runtime and a desktop session — so a test that started it
//! would run nowhere except one developer's machine.
//!
//! What is duplicated is therefore kept to nothing that matters: this fixture
//! calls [`clipped_ipc::supervisor::claim_instance`],
//! [`clipped_ipc::ensure_recorder`] and [`clipped_ipc::RecorderLink`], which are
//! the same three calls `apps/desktop/src-tauri/src/main.rs` makes, in the same
//! order, with the same settings type. The window is what is missing, and the
//! window is not what these tests are about.
//!
//! It is an example rather than a second binary so that it is built for tests
//! and not shipped with the recorder.
//!
//! ```text
//! supervised_ui_fixture --endpoint NAME --recorder PATH
//!                       [--instance NAME]
//!                       [--record-pid PID --output PATH]
//!                       [--restart-attempts N] [--restart-delay-ms MS]
//! ```
//!
//! Everything it learns goes to standard output, one line at a time, flushed:
//!
//! ```text
//! instance=claimed | instance=already-running
//! recorder=started pid=1234 | recorder=existing pid=1234 | recorder=lost-the-race pid=1234 started=5678
//! recording=r-1 output=D:\clips\x.mkv
//! ready
//! link={"link":"attached","recorder_process_id":1234,"status":{"state":"idle"}}
//! interrupted={"recording_id":"r-1","output":"…","target":"…","elapsed_ms":4200}
//! failed=r-1 the encoder stopped accepting frames
//! ```
//!
//! It then runs until it is killed. That is the point of it.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clipped_ipc::supervisor::{claim_instance, InstanceClaim};
use clipped_ipc::{
    ensure_recorder, AttachmentOrigin, Client, Command, Endpoint, PeerIdentity, RecorderLink,
    RecorderLinkEvent, Reply, StartRecording, SupervisorSettings,
};

/// How this fixture introduces itself in a handshake.
const CLIENT: &str = "clipped-supervised-ui-fixture";

fn main() -> ExitCode {
    let arguments = match Arguments::parse() {
        Ok(arguments) => arguments,
        Err(message) => {
            eprintln!("supervised_ui_fixture: {message}");
            return ExitCode::FAILURE;
        }
    };

    // First, because a second launch that has already started a recorder has
    // failed the criterion whatever it does afterwards.
    if let Some(name) = &arguments.instance {
        match claim_instance(name) {
            Ok(InstanceClaim::Claimed(claim)) => {
                say(&format!("instance=claimed name={}", claim.name()));
                // Deliberately leaked rather than dropped: the claim must last
                // as long as this process, and the value is not needed again.
                std::mem::forget(claim);
            }
            Ok(InstanceClaim::AlreadyRunning) => {
                say("instance=already-running");
                // Nothing else at all. Not a probe, not an attach, and above
                // all not a recorder.
                return ExitCode::SUCCESS;
            }
            Err(error) => {
                eprintln!("supervised_ui_fixture: {error}");
                return ExitCode::FAILURE;
            }
        }
    }

    let settings = match arguments.settings() {
        Ok(settings) => settings,
        Err(message) => {
            eprintln!("supervised_ui_fixture: {message}");
            return ExitCode::FAILURE;
        }
    };

    match ensure_recorder(&settings) {
        Ok(attachment) => {
            let origin = match attachment.origin {
                AttachmentOrigin::Existing => "existing".to_owned(),
                AttachmentOrigin::Started => "started".to_owned(),
                AttachmentOrigin::LostTheRace { started_process_id } => {
                    format!("lost-the-race started={started_process_id}")
                }
            };
            say(&format!(
                "recorder={origin} pid={}",
                attachment.recorder_process_id
            ));
        }
        Err(error) => {
            // Printed rather than exited on, so that a test can assert what the
            // supervisor said about a recorder it could not start.
            say(&format!("recorder=error {error}"));
        }
    }

    if let (Some(pid), Some(output)) = (arguments.record_pid, &arguments.output) {
        if let Err(message) = start_recording(&settings, pid, output) {
            eprintln!("supervised_ui_fixture: {message}");
            return ExitCode::FAILURE;
        }
    }

    let (link, events) = RecorderLink::start(settings);
    say("ready");

    // Until this process is killed, which is what every test here does to it.
    // The receiver ends only when the link's thread does, which happens when a
    // stop is asked for — and nothing here asks.
    while let Ok(event) = events.recv() {
        match event {
            RecorderLinkEvent::State(state) => say(&format!(
                "link={}",
                serde_json::to_string(&state)
                    .unwrap_or_else(|error| format!("{{\"error\":\"{error}\"}}"))
            )),
            RecorderLinkEvent::RecordingInterrupted(active) => say(&format!(
                "interrupted={}",
                serde_json::to_string(&active)
                    .unwrap_or_else(|error| format!("{{\"error\":\"{error}\"}}"))
            )),
            RecorderLinkEvent::RecordingFailed {
                recording_id,
                error,
            } => say(&format!("failed={recording_id} {error}")),
            // Reported once the link attaches, when Windows would not give the
            // recorder a combination it asked for (issue #417). Said here as the
            // actions rather than the whole rows: this fixture's output is read
            // line by line by a test, and what a test would assert on is which
            // hotkeys were refused.
            RecorderLinkEvent::HotkeysUnavailable { conflicts } => say(&format!(
                "hotkeys_unavailable={}",
                conflicts
                    .iter()
                    .map(|binding| binding.action.as_str())
                    .collect::<Vec<&str>>()
                    .join(",")
            )),
            // How far a running export has got (issue #446). The fraction and
            // the file, rather than the whole event: a test reading this line
            // by line asserts that the figure advances, and the destination is
            // what says which export it belongs to.
            RecorderLinkEvent::ExportProgress(progress) => say(&format!(
                "export_progress={} {}",
                progress.fraction().map_or_else(
                    || "unmeasured".to_owned(),
                    |fraction| format!("{:.0}%", fraction * 100.0)
                ),
                progress.destination
            )),
        }
    }

    drop(link);
    ExitCode::SUCCESS
}

/// Starts a recording over the protocol, the way a window's record button
/// would.
fn start_recording(
    settings: &SupervisorSettings,
    pid: u32,
    output: &std::path::Path,
) -> Result<(), String> {
    let mut client = Client::connect(
        &settings.endpoint,
        CLIENT,
        env!("CARGO_PKG_VERSION"),
        Duration::from_secs(10),
    )
    .map_err(|error| format!("the recorder would not accept a control connection: {error}"))?;

    let reply = client
        .call(&Command::StartRecording(StartRecording {
            pid: Some(pid),
            output: Some(output.to_string_lossy().into_owned()),
            overwrite: true,
            // Asked for explicitly: a fixture should produce the same file
            // on a machine with audio devices and one without.
            microphone: Some("none".to_owned()),
            system_audio: Some("none".to_owned()),
            ..StartRecording::default()
        }))
        .map_err(|error| format!("the recording did not start: {error}"))?;

    match reply {
        Reply::RecordingStarted {
            recording_id,
            output,
        } => {
            say(&format!("recording={recording_id} output={output}"));
            Ok(())
        }
        other => Err(format!("expected a started recording, got {other:?}")),
    }
}

/// Writes one line and flushes it.
///
/// Flushed every time because whatever started this fixture is very likely
/// blocked reading that line, and standard output to a pipe is buffered.
fn say(line: &str) {
    println!("{line}");
    let _ = std::io::stdout().flush();
}

/// What the fixture was asked to do.
#[derive(Debug, Default)]
struct Arguments {
    endpoint: Option<String>,
    recorder: Option<PathBuf>,
    instance: Option<String>,
    record_pid: Option<u32>,
    output: Option<PathBuf>,
    restart_attempts: Option<u32>,
    restart_delay_ms: Option<u64>,
}

impl Arguments {
    /// Reads the command line, by hand.
    ///
    /// By hand rather than with clap because this is a test fixture with seven
    /// options and no help text to get right, and because adding a second clap
    /// command line to this package would invite somebody to keep it in step
    /// with the real one.
    fn parse() -> Result<Self, String> {
        let mut arguments = Self::default();
        let mut argv = std::env::args().skip(1);

        while let Some(flag) = argv.next() {
            let value = argv.next().ok_or_else(|| format!("{flag} needs a value"))?;

            match flag.as_str() {
                "--endpoint" => arguments.endpoint = Some(value),
                "--recorder" => arguments.recorder = Some(PathBuf::from(value)),
                "--instance" => arguments.instance = Some(value),
                "--record-pid" => {
                    arguments.record_pid = Some(value.parse().map_err(|_| {
                        format!("--record-pid takes a number, not `{flag}`'s `{value}`")
                    })?);
                }
                "--output" => arguments.output = Some(PathBuf::from(value)),
                "--restart-attempts" => {
                    arguments.restart_attempts = Some(
                        value
                            .parse()
                            .map_err(|_| "--restart-attempts takes a number".to_owned())?,
                    );
                }
                "--restart-delay-ms" => {
                    arguments.restart_delay_ms = Some(
                        value
                            .parse()
                            .map_err(|_| "--restart-delay-ms takes a number".to_owned())?,
                    );
                }
                other => return Err(format!("unknown argument {other}")),
            }
        }

        Ok(arguments)
    }

    /// The supervisor settings they describe.
    fn settings(&self) -> Result<SupervisorSettings, String> {
        let name = self
            .endpoint
            .as_deref()
            .ok_or_else(|| "--endpoint is required".to_owned())?;
        let endpoint = Endpoint::named(name).map_err(|error| error.to_string())?;
        let recorder = self
            .recorder
            .clone()
            .ok_or_else(|| "--recorder is required".to_owned())?;

        let mut settings = SupervisorSettings::new(
            endpoint,
            recorder,
            PeerIdentity {
                name: CLIENT.to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
            },
        );

        // A fixture starts a real recorder on whatever machine is running the
        // suite. One that watched for games would create that person's
        // recordings folder and could start recording a game they had open,
        // which is what AGENTS.md section 25 rules out — and what these tests
        // are about is the supervisor, not what the recorder records.
        settings.watch_for_games = false;

        if let Some(attempts) = self.restart_attempts {
            settings.restart.attempts = attempts;
        }
        if let Some(delay) = self.restart_delay_ms {
            settings.restart.first_delay = Duration::from_millis(delay);
            settings.restart.maximum_delay = Duration::from_millis(delay);
        }
        // A fixture is killed within seconds, so a recorder that stays up for
        // one is a recorder that started: the default minute would mean the
        // attempt counter never reset inside a test's lifetime.
        settings.restart.settled_after = Duration::from_secs(1);

        Ok(settings)
    }
}
