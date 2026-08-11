//! `clipped-recorder watch` against a real launched subject, and the files it
//! leaves.
//!
//! These are issue #46's acceptance criteria checked rather than asserted in
//! prose: a game launching produces a finalised session recording with nobody
//! touching the recorder, killing the game's process still leaves a file that
//! **decodes**, and stopping the recorder while a game is still being captured
//! finalises the file *and* finishes the session record that names it.
//!
//! Nothing here is mocked. The recorder is the built binary, run as a child
//! process the way a user runs it. The subject is `test-apps/video-pattern`, a
//! real window on a real display, started in one test through a `cmd.exe`
//! parent, which is the shape a launcher starting a game has. The kill is a
//! real `TerminateProcess`, with no notification and no destructors, which is
//! what a game crashing looks like from outside. The files are read back by the
//! pinned FFmpeg build through `clipped-media-validation`, the workspace's one
//! media validator (AGENTS.md section 22).
//!
//! # What the parent process does and does not prove
//!
//! It proves the recorder records a game it did not start itself and had no
//! handle on. It does **not** reliably exercise the debounce joining a parent
//! and its child into one launch, and saying otherwise would be claiming
//! something these tests do not check. Whether the watcher reports
//! `cmd.exe → video-pattern.exe` as one launch or as two depends on the order
//! WMI happens to deliver the two creation events in, which is not ordered:
//! `crates/game-detection`'s debounce joins a starting process to its parent's
//! launch, so a child reported *before* its parent opens a launch of its own.
//! Both orderings were observed on this machine while writing these tests.
//!
//! The grouped case is pinned deterministically where it can be — against a
//! constructed launch, in `clipped_session::automatic`'s own tests, which
//! assert that a `[launcher.exe, game.exe]` group is recorded by its game and
//! not by its launcher.
//!
//! # How the recorder is made to recognise the subject
//!
//! By its **user overlay**, not by the shipped catalogue. `video-pattern.exe`
//! must never be in `crates/game-detection/data/games.toml`: that file is
//! compiled into every build, and a test application in it would have Clipped
//! recording a test application on somebody's machine. The recorder resolves
//! the overlay under `%LOCALAPPDATA%\Clipped`, so each test points the child
//! process's `LOCALAPPDATA` at a directory of its own and writes the entry
//! there. That also keeps the run's logs out of the maintainer's log directory.
//!
//! # What is asserted
//!
//! That a session was recorded of the game the overlay names; that its sidecar
//! says so and names the file; that the file opens, holds one video stream at
//! the window's size, and **decodes at least as many pictures as the recorder
//! said it encoded**. That last one is the point of using
//! `decoded_frames_at_least` rather than a packet count: a file whose packets
//! all failed to decode satisfies every other assertion here.
//!
//! And, on the one path a user takes most often and the other two do not touch
//! — Ctrl+C with a game still running and a recording still capturing — that
//! the session record is *finished*: an end reason of `recorder-stopping`, and
//! the recording's outcome stored against it. A sidecar saying a recording
//! began and never ended is what M6's indexer would otherwise have to
//! reconcile against a file that is complete and playable.
//!
//! # Why these are `#[ignore]`d
//!
//! They need a GPU, an encoder, a desktop session, WMI, and about a minute of
//! wall-clock each, and they put a window on a display. CI has none of that, so
//! what they would test there is the runner. Being `#[ignore]`d rather than
//! quietly skipped is deliberate: a test that decides for itself that it could
//! not run reads as a pass. The one that checks what is said about a game with
//! *no* window needs neither a GPU nor an encoder — nothing is ever captured —
//! but it still needs WMI and a machine of its own, so it is ignored with the
//! rest rather than being the one that fails on CI.
//!
//! `cargo test --workspace` builds the examples these need; a bare
//! `cargo build --workspace` does not.
//!
//! ```text
//! cargo test -p clipped-recorder --test automatic_sessions -- --ignored --nocapture --test-threads=1
//! ```

#![cfg(windows)]

mod support;

use std::fs;
use std::io::{BufRead, BufReader};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use clipped_media_validation::{require_media_tools, Media, TemporaryDirectory, VideoStream};
use serde_json::Value;

use support::{
    ensure_console, example_binary, read_stderr, recorder_binary, send_ctrl_c, terminate,
    video_pattern_binary, wait_for_exit, CREATE_NEW_PROCESS_GROUP,
};

/// The rate the pattern application presents at.
const SOURCE_FPS: u32 = 30;

/// How long the pattern renders for in the test that lets it finish.
const PATTERN_SECONDS: u64 = 30;

/// How long a recording is left running before the subject is killed.
const RECORD_BEFORE_KILL: Duration = Duration::from_secs(6);

/// How long any single thing these tests wait for is given.
///
/// Generous on purpose. Detection is deliberately unhurried — the shipped
/// `WatchConfig` costs up to four and a half seconds between a process starting
/// and the launch being reported — and a bound tight enough to trip on a busy
/// machine is a failure nobody can tell from a real one (AGENTS.md section 25).
const PATIENCE: Duration = Duration::from_secs(90);

/// How long the recorder is given to find a window before it gives up.
///
/// Far shorter than the default: the pattern puts its window up immediately, so
/// anything longer only makes a failure slower to arrive.
const WINDOW_TIMEOUT: &str = "15";

/// The overlay entry that makes the recorder treat the pattern as a game.
///
/// A user's own file, exactly as somebody registering an unknown executable
/// would write it (`docs/game-detection.md`).
const OVERLAY: &str = r#"
schema_version = 1

[[game]]
game_id = "clipped-video-pattern"
name = "Clipped Video Pattern"
[[game.executables]]
name = "video-pattern.exe"
"#;

/// An overlay entry naming something that will never put a window on screen.
///
/// `shutdown_fixture` is the recorder's own Ctrl+C fixture: it starts, says
/// `ready`, and waits. That makes it a real process the watcher really reports
/// and really has to give up looking for a window of — which is exactly the
/// case being checked, and one no game can be relied upon to produce on demand.
const WINDOWLESS_OVERLAY: &str = r#"
schema_version = 1

[[game]]
game_id = "clipped-windowless"
name = "Clipped Windowless"
[[game.executables]]
name = "shutdown_fixture.exe"
"#;

#[test]
#[ignore = "needs a GPU, an encoder and a desktop session; see the module docs"]
fn launching_a_game_produces_a_finalised_session_recording() {
    // Acceptance criterion 1, as far as this machine can honestly check it:
    // nobody touches the recorder, and a session recording appears. The launch
    // goes through a `cmd.exe` parent so that the recorder records a process it
    // did not start and holds no handle on. What that does *not* reliably prove
    // is that the launch chain arrives as one group — see the module docs.
    let Some(_tools) = require_media_tools() else {
        return;
    };
    ensure_console();

    let workspace = Workspace::new("watch-launch");
    let mut recorder = workspace.start_recorder();

    recorder.wait_for("Watching for games.");
    // A process that starts between the watcher's baseline snapshot and its
    // subscription is invisible to it for its lifetime, and that window is a
    // few tens of milliseconds (`ProcessWatcher::start`). The line above is
    // printed after the subscription, and this is the cushion.
    thread::sleep(Duration::from_secs(1));

    let mut pattern = LaunchedPattern::through_a_parent(SOURCE_FPS, PATTERN_SECONDS);
    recorder.wait_for("Recording Clipped Video Pattern");

    // Let it run out on its own: the window closes, the recording finalises,
    // and the session stays open for its restart grace until Ctrl+C closes it.
    pattern.wait_for_exit();
    recorder.wait_for("recording file finished");

    let diagnostics = recorder.stop();
    let session = workspace.only_session();

    assert_eq!(
        session["game"]["game_id"],
        Value::from("clipped-video-pattern"),
        "the session should be filed under the overlay's game:\n{diagnostics}"
    );
    let recording = playable_recording(&session, &diagnostics);
    assert_media_decodes(recording, pattern.client_size(), &diagnostics);
}

#[test]
#[ignore = "needs a GPU, an encoder and a desktop session; see the module docs"]
fn killing_the_game_process_finalises_the_recording_into_a_playable_file() {
    // Acceptance criterion 2, and the failure case the ticket cares most about:
    // the process vanishes with no notification, no destructors and no flush,
    // and the recording still has to become a file that decodes.
    let Some(_tools) = require_media_tools() else {
        return;
    };
    ensure_console();

    let workspace = Workspace::new("watch-kill");
    let mut recorder = workspace.start_recorder();

    recorder.wait_for("Watching for games.");
    thread::sleep(Duration::from_secs(1));

    let pattern = LaunchedPattern::directly(SOURCE_FPS, 300);
    recorder.wait_for("Recording Clipped Video Pattern");
    thread::sleep(RECORD_BEFORE_KILL);

    // `TerminateProcess`, which is what `crates/muxer/tests/abrupt_termination.rs`
    // uses to mean "killed": the process is ended where it stands.
    terminate(pattern.process_id());
    recorder.wait_for("recording file finished");

    let diagnostics = recorder.stop();
    let session = workspace.only_session();
    let recording = playable_recording(&session, &diagnostics);

    assert_eq!(
        recording["end_reason"],
        Value::from("target-lost"),
        "a killed game takes its window with it:\n{diagnostics}"
    );
    assert_media_decodes(recording, pattern.client_size(), &diagnostics);

    let seconds = recording["duration_seconds"]
        .as_f64()
        .expect("a finished recording has a duration");
    assert!(
        seconds >= 2.0,
        "the recorder ran for about {:.0}s before the kill and the file holds {seconds:.2}s; \
         a fraction of a second is what a recording finalised without its last cluster looks \
         like:\n{diagnostics}",
        RECORD_BEFORE_KILL.as_secs_f64()
    );
}

#[test]
#[ignore = "needs a GPU, an encoder and a desktop session; see the module docs"]
fn stopping_the_recorder_mid_recording_finalises_it_and_finishes_the_session() {
    // The other two tests only ever press Ctrl+C after the recording has ended
    // by itself, so neither of them touches the path a user actually takes:
    // stopping the recorder while a game is still being captured. Everything on
    // that path has to survive being interrupted — the file has to be
    // finalised, the outcome has to reach the session it belongs to, and the
    // session has to say a stopping recorder ended it. A session record saying
    // a recording began and never ended is exactly what M6's indexer would
    // later have to reconcile against a file that is sitting there, playable.
    let Some(_tools) = require_media_tools() else {
        return;
    };
    ensure_console();

    let workspace = Workspace::new("watch-ctrl-c");
    let mut recorder = workspace.start_recorder();

    recorder.wait_for("Watching for games.");
    thread::sleep(Duration::from_secs(1));

    // Long enough that it is unambiguously still rendering when the recorder is
    // stopped: the pattern outliving the recorder is the point of this test.
    let mut pattern = LaunchedPattern::directly(SOURCE_FPS, 300);
    recorder.wait_for("Recording Clipped Video Pattern");
    thread::sleep(RECORD_BEFORE_KILL);

    let diagnostics = recorder.stop();
    assert!(
        pattern.is_running(),
        "the game must still have been running when the recorder was stopped, or this test \
         proves nothing the others do not already prove:\n{diagnostics}"
    );

    let session = workspace.only_session();
    assert!(
        !session["ended_at"].is_null(),
        "a session the recorder stopped is a finished session:\n{session:#}\n{diagnostics}"
    );
    assert!(
        session["events"].as_array().is_some_and(|events| events
            .iter()
            .any(
                |event| event["event"] == "session-ended" && event["reason"] == "recorder-stopping"
            )),
        "the session should record why it ended:\n{session:#}\n{diagnostics}"
    );

    let recording = playable_recording(&session, &diagnostics);
    assert_eq!(
        recording["end_reason"],
        Value::from("stopped"),
        "a recording ended by Ctrl+C was stopped by request, not lost:\n{diagnostics}"
    );
    assert_media_decodes(recording, pattern.client_size(), &diagnostics);

    let seconds = recording["duration_seconds"]
        .as_f64()
        .expect("a finished recording has a duration");
    assert!(
        seconds >= 2.0,
        "the recorder ran for about {:.0}s before Ctrl+C and the file holds {seconds:.2}s; a \
         fraction of a second is what a recording finalised without its last cluster looks \
         like:\n{diagnostics}",
        RECORD_BEFORE_KILL.as_secs_f64()
    );
}

#[test]
#[ignore = "needs a desktop session and WMI; see the module docs"]
fn a_game_that_never_shows_a_window_is_said_so_and_never_claimed_as_a_recording() {
    // A launch is noticed seconds before there is anything to capture, and the
    // search for a window can run for `--window-timeout` and then fail. The
    // console must not announce a recording at the moment the game was noticed,
    // because that is a claim about something that may never happen — and when
    // the search does give up it must say so, or a user whose game was never
    // captured has nothing to read but a summary saying zero recordings
    // (AGENTS.md section 27).
    ensure_console();

    let workspace = Workspace::with_overlay("watch-no-window", WINDOWLESS_OVERLAY);
    let mut recorder = workspace.start_recorder();

    recorder.wait_for("Watching for games.");
    thread::sleep(Duration::from_secs(1));

    let subject = WindowlessSubject::start(&workspace.path().join("marker"));

    // In order, which is the whole point: the launch is noticed first, and that
    // is all that has happened. `wait_for` reads the stream forwards, so a
    // console that announced a recording up front would never reach this line.
    recorder.wait_for("Clipped Windowless started. Looking for its window.");
    recorder.wait_for("Nothing was recorded of Clipped Windowless");

    let diagnostics = recorder.stop();
    drop(subject);

    assert!(
        !diagnostics.contains("Recording Clipped Windowless to"),
        "nothing ever had a window, so nothing was ever recorded, and the console must not have \
         said otherwise:\n{diagnostics}"
    );

    let session = workspace.only_session();
    let recordings = session["recordings"]
        .as_array()
        .unwrap_or_else(|| panic!("the session record has no recordings:\n{session:#}"));
    assert!(
        !recordings.is_empty()
            && recordings
                .iter()
                .all(|recording| recording["outcome"] == "no-window"),
        "every attempt should be recorded as having found no window:\n{session:#}\n{diagnostics}"
    );
}

/// A temporary directory holding a run's clips, its session records and the
/// user data the recorder is pointed at.
#[derive(Debug)]
struct Workspace {
    directory: TemporaryDirectory,
}

impl Workspace {
    fn new(label: &str) -> Self {
        Self::with_overlay(label, OVERLAY)
    }

    fn with_overlay(label: &str, overlay: &str) -> Self {
        let directory = TemporaryDirectory::new(label);
        let workspace = Self { directory };

        fs::create_dir_all(workspace.clips()).expect("the clips directory can be created");
        let application = workspace.local_app_data().join("Clipped");
        fs::create_dir_all(&application).expect("the data directory can be created");
        fs::write(application.join("games.toml"), overlay).expect("the overlay can be written");

        workspace
    }

    fn clips(&self) -> PathBuf {
        self.directory.path().join("clips")
    }

    fn path(&self) -> &Path {
        self.directory.path()
    }

    fn local_app_data(&self) -> PathBuf {
        self.directory.path().join("appdata")
    }

    /// Starts `clipped-recorder watch` against this workspace.
    fn start_recorder(&self) -> RecorderProcess {
        let mut child = Command::new(recorder_binary())
            .args([
                "watch",
                "--output-directory",
                &self.clips().to_string_lossy(),
                "--window-timeout",
                WINDOW_TIMEOUT,
                // The session cannot record audio yet and would warn on every
                // recording; a test should ask for what it expects to get.
                "--microphone",
                "none",
                "--system-audio",
                "none",
            ])
            // The overlay and the logs both hang off this, so the run touches
            // nothing of the machine's own (see the module documentation).
            .env("LOCALAPPDATA", self.local_app_data())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A group of its own, so the Ctrl+C reaches the recorder and not
            // `cargo test`.
            .creation_flags(CREATE_NEW_PROCESS_GROUP)
            .spawn()
            .expect("the recorder binary can be started");

        let lines = read_stderr(&mut child);
        RecorderProcess {
            child: Some(child),
            lines,
            collected: String::new(),
        }
    }

    /// The one session record the run produced, as JSON.
    fn only_session(&self) -> Value {
        let mut sidecars: Vec<PathBuf> = fs::read_dir(self.clips())
            .expect("the clips directory can be listed")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.to_string_lossy()
                    .to_lowercase()
                    .ends_with(".session.json")
            })
            .collect();
        sidecars.sort();

        assert_eq!(
            sidecars.len(),
            1,
            "expected exactly one session record in {}, found {sidecars:?}",
            self.clips().display()
        );
        let text = fs::read_to_string(&sidecars[0]).expect("the session record can be read");
        serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("the session record is not JSON: {error}\n{text}"))
    }
}

/// The recorder, and everything it has said so far.
#[derive(Debug)]
struct RecorderProcess {
    child: Option<Child>,
    lines: Receiver<String>,
    collected: String,
}

impl RecorderProcess {
    /// Waits for a line containing `needle`, keeping everything read on the way.
    fn wait_for(&mut self, needle: &str) {
        let deadline = Instant::now() + PATIENCE;
        loop {
            match self
                .lines
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(line) => {
                    let found = line.contains(needle);
                    self.collected.push_str(&line);
                    self.collected.push('\n');
                    if found {
                        return;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => panic!(
                    "the recorder stopped without ever saying `{needle}`:\n{}",
                    self.collected
                ),
                Err(RecvTimeoutError::Timeout) => panic!(
                    "the recorder did not say `{needle}` within {PATIENCE:?}:\n{}",
                    self.collected
                ),
            }
        }
    }

    /// Ctrl+C, then everything it said on the way out.
    fn stop(&mut self) -> String {
        let mut child = self
            .child
            .take()
            .expect("the recorder has not been stopped");
        send_ctrl_c(&child);
        let status = wait_for_exit(&mut child, "the recorder");

        while let Ok(line) = self.lines.recv_timeout(Duration::from_secs(5)) {
            self.collected.push_str(&line);
            self.collected.push('\n');
        }

        assert!(
            status.success(),
            "Ctrl+C should stop `watch` cleanly, not kill it; exit status was {status}.\n{}",
            self.collected
        );
        assert!(
            self.collected.contains("Session "),
            "the recorder should have reported the session it finished:\n{}",
            self.collected
        );
        self.collected.clone()
    }
}

impl Drop for RecorderProcess {
    /// A leaked `watch` would keep recording whatever launches next
    /// ([issue #220](https://github.com/wildware-uk/clipped/issues/220)), so a
    /// panicking test must not leave one behind.
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// A running `video-pattern`, started in one of the two ways these tests need.
///
/// # Why this is not `support::PatternApp`
///
/// It nearly is, and the shared one cannot do the part that matters here:
/// starting the pattern *through a parent process*, so that the launch the
/// watcher reports is a chain. Everything else — the arguments, reading the one
/// `ready` line, making sure nothing is left rendering on a monitor — is the
/// same, and the pattern itself is not touched (`docs/testing.md`).
#[derive(Debug)]
struct LaunchedPattern {
    child: Child,
    process_id: u32,
    client: (u32, u32),
}

impl LaunchedPattern {
    /// Started by this test process, so its process identifier is known here.
    fn directly(fps: u32, seconds: u64) -> Self {
        let binary = pattern_binary();
        let mut command = Command::new(&binary);
        command.args(pattern_arguments(fps, seconds));
        let mut child = spawn(command, &binary);
        let process_id = child.id();
        let client = read_ready_line(&mut child);
        Self {
            child,
            process_id,
            client,
        }
    }

    /// Started by a `cmd.exe` that this test process started.
    ///
    /// The game is then two processes deep and nothing in the test has a handle
    /// on it, which is the shape a launcher starting a game has. `cmd.exe`
    /// matches nothing in the catalogue either way. What this does *not*
    /// reliably produce is one launch containing both — see the module
    /// documentation, which says why and where that case is pinned instead.
    ///
    /// The identifier is the shell's; the pattern's own is not needed by the
    /// test that uses this, and asking Windows for it would be a second way of
    /// finding a process for no benefit.
    fn through_a_parent(fps: u32, seconds: u64) -> Self {
        let binary = pattern_binary();
        let mut command = Command::new("cmd.exe");
        command
            .arg("/c")
            .arg(&binary)
            .args(pattern_arguments(fps, seconds));
        let mut child = spawn(command, Path::new("cmd.exe"));
        let process_id = child.id();
        let client = read_ready_line(&mut child);
        Self {
            child,
            process_id,
            client,
        }
    }

    fn process_id(&self) -> u32 {
        self.process_id
    }

    fn client_size(&self) -> (u32, u32) {
        self.client
    }

    /// Whether the subject is still rendering.
    ///
    /// The premise of the test that stops the recorder mid-recording: a pattern
    /// that had already finished would make it a repeat of the one above it.
    fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn wait_for_exit(&mut self) {
        wait_for_exit(&mut self.child, "the pattern application");
    }
}

impl Drop for LaunchedPattern {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => thread::sleep(Duration::from_millis(20)),
                Err(_) => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A running process that the overlay calls a game and that never draws.
///
/// It is killed rather than asked to stop: what it is here for is to exist
/// while the recorder looks for a window it does not have, and a fixture left
/// running would sit in the watcher's view of the machine for the next test.
#[derive(Debug)]
struct WindowlessSubject {
    child: Child,
}

impl WindowlessSubject {
    fn start(marker: &Path) -> Self {
        let mut child = Command::new(example_binary("shutdown_fixture"))
            .arg(marker)
            .stdout(Stdio::piped())
            // Its own group, so that the Ctrl+C aimed at the recorder cannot
            // reach it and end this test's subject early.
            .creation_flags(CREATE_NEW_PROCESS_GROUP)
            .spawn()
            .expect("the shutdown fixture can be started");

        // It says `ready` once it is up, which is what makes the wait below a
        // wait on the recorder rather than on this process starting.
        let stdout = child.stdout.take().expect("stdout was piped");
        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .expect("the fixture announces itself");
        assert!(
            line.trim() == "ready",
            "the fixture should have said it was ready, and said {line:?}"
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

/// Where the pattern application was built.
fn pattern_binary() -> PathBuf {
    let binary = video_pattern_binary();
    assert!(
        binary.exists(),
        "{} has not been built. Run `cargo build --workspace` before this test.",
        binary.display()
    );
    binary
}

/// The pattern's arguments: off the primary display, topmost, never focused.
fn pattern_arguments(fps: u32, seconds: u64) -> Vec<String> {
    vec![
        "--mode".to_owned(),
        "borderless".to_owned(),
        "--fps".to_owned(),
        fps.to_string(),
        "--seconds".to_owned(),
        seconds.to_string(),
        "--monitor".to_owned(),
        "auto".to_owned(),
    ]
}

/// Starts a command with the pipes these tests need.
fn spawn(mut command: Command, what: &Path) -> Child {
    command
        // Standard input is a pipe so that dropping it stops the application,
        // which is what stops it outliving the test.
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("{} could not be started: {error}", what.display()))
}

/// The `client=WIDTHxHEIGHT` the pattern announces before it renders.
fn read_ready_line(child: &mut Child) -> (u32, u32) {
    let stdout = child.stdout.take().expect("stdout was piped");
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .expect("the pattern application announces itself before rendering");

    let field = line
        .split_whitespace()
        .find_map(|field| field.strip_prefix("client="))
        .unwrap_or_else(|| panic!("the ready line has no client size: {line}"));
    let (width, height) = field
        .split_once('x')
        .unwrap_or_else(|| panic!("the client size is not WIDTHxHEIGHT: {field}"));
    (
        width.parse().expect("the client width is a number"),
        height.parse().expect("the client height is a number"),
    )
}

/// The one recording of the session that produced a file.
///
/// A session may hold more than one: a recording ends when the game's window
/// goes, and the watcher can take a couple of seconds longer to report the
/// process itself exiting, so the manager can start one more before it learns
/// the game has gone. That one finds no window and writes nothing, which is
/// what is asserted about the others here.
fn playable_recording<'a>(session: &'a Value, diagnostics: &str) -> &'a Value {
    let recordings = session["recordings"]
        .as_array()
        .unwrap_or_else(|| panic!("the session record has no recordings:\n{session:#}"));

    let recorded: Vec<&Value> = recordings
        .iter()
        .filter(|recording| recording["outcome"] == "recorded")
        .collect();

    assert_eq!(
        recorded.len(),
        1,
        "expected exactly one recording that produced a file, got {recordings:#?}\n{diagnostics}"
    );
    for recording in recordings {
        let outcome = recording["outcome"].as_str().unwrap_or("none");
        assert!(
            matches!(outcome, "recorded" | "no-window"),
            "a recording of this session failed rather than simply finding nothing to \
             record: {recording:#}\n{diagnostics}"
        );
    }

    recorded[0]
}

/// Asserts the file the session names is one that plays.
fn assert_media_decodes(recording: &Value, client: (u32, u32), diagnostics: &str) {
    let output = PathBuf::from(
        recording["output"]
            .as_str()
            .expect("a recording names its file"),
    );
    let frames = recording["frames_encoded"]
        .as_u64()
        .expect("a finished recording says how many frames it encoded");
    let width = recording["width"].as_u64().expect("a width") as u32;
    let height = recording["height"].as_u64().expect("a height") as u32;

    eprintln!(
        "\n=== automatic session recording ===\n\
         file           : {}\n\
         frames encoded : {frames}\n\
         picture        : {width}x{height}\n\
         duration       : {}s\n",
        output.display(),
        recording["duration_seconds"]
    );

    assert!(
        frames > 0,
        "a recording of no frames is not a recording:\n{diagnostics}"
    );
    assert_eq!(
        (width, height),
        client,
        "a borderless window's capture is exactly its client area:\n{diagnostics}"
    );

    let media = Media::open(&output)
        .unwrap_or_else(|error| panic!("the recording is not usable at all: {error}"));

    media
        .validate()
        // One video stream and nothing else: there is no audio track yet, and
        // "at least one video stream" would not notice one appearing
        // half-wired.
        .stream_count(1)
        .video_stream_count(1)
        .video(
            VideoStream::codec(&codec_of(&media))
                .resolution(width, height)
                // **Decoded**, not demuxed. A file whose packets all failed to
                // decode opens, reports a stream, reports a duration and has
                // monotonic timestamps; the only assertion that catches it is
                // one that runs a decoder over it.
                .decoded_frames_at_least(frames),
        )
        .monotonic_timestamps()
        .assert_valid();
}

/// What `ffprobe` calls the codec in the file.
///
/// Read from the file rather than asserted, because `watch` was not told which
/// codec to use: it takes whatever this machine's encoder was measured to
/// support, and pinning one here would make the test a test of the GPU.
fn codec_of(media: &Media) -> String {
    media
        .video_streams()
        .first()
        .and_then(|stream| stream.field("codec_name"))
        .unwrap_or_else(|| panic!("the recording has no video codec:\n{}", media.inventory()))
        .to_owned()
}
