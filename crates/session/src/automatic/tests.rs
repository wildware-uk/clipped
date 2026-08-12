//! The policy, exercised against constructed launches.
//!
//! Everything the ticket calls a failure case — a crash, a fast restart, a
//! second game, a suspend — is a rule about timing and identity, and every one
//! of them is tested here against process trees the test builds, on a clock the
//! test supplies. No game, no GPU, no window and no waiting: a suspend of eight
//! hours is two `SystemTime` values.
//!
//! What is *not* here is anything about capture. Turning a process into a
//! window and recording it belongs to the driver, and is tested against a real
//! subject in `apps/recorder/tests/automatic_sessions.rs`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use clipped_capture::CaptureMethod;
use clipped_encoder::{Codec, EncoderKind};
use clipped_game_detection::catalogue::{Catalogue, EntrySource};
use clipped_game_detection::{LaunchGroup, LaunchId, ProcessExit, ProcessSnapshot, WatchEvent};

use super::*;
use crate::config::{Preferences, ResolutionSetting, SettingKey, SettingSource};
use crate::report::EndReason;
use crate::settings::{CaptureTargetSettings, RecordingSettings, UnavailableChoice};

/// Three ordinary games, one of which has a known helper, and two more that tie
/// on a shared executable name.
const GAMES: &str = r#"
schema_version = 1

[[game]]
game_id = "test-game"
name = "Test Game"
child_processes = ["test-helper.exe"]
[[game.executables]]
name = "test-game.exe"

[[game]]
game_id = "other-game"
name = "Other Game"
[[game.executables]]
name = "other-game.exe"

[[game]]
game_id = "third-game"
name = "Third Game"
[[game.executables]]
name = "third-game.exe"

[[game]]
game_id = "first-tie"
name = "First Tie"
child_processes = ["first-helper.exe"]
[[game.executables]]
name = "shared.exe"

[[game]]
game_id = "second-tie"
name = "Second Tie"
child_processes = ["second-helper.exe"]
[[game.executables]]
name = "shared.exe"
"#;

/// Settings that keep the tests quick and their arithmetic obvious.
const GRACE: Duration = Duration::from_secs(30);
const RESTART_DELAY: Duration = Duration::from_secs(5);
const SUSPEND_GAP: Duration = Duration::from_secs(600);

/// A directory of this test's own, removed when it is dropped.
///
/// The workspace has no `tempfile` dependency and this is not enough reason to
/// add one; `apps/recorder/src/config.rs` and `crates/logging` build the same
/// thing from `std::env::temp_dir` and the process id (AGENTS.md sections 10
/// and 55).
#[derive(Debug)]
struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "clipped-sessions-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("the temporary directory can be created");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// Every session sidecar in the directory, by file name.
    fn sidecars(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.0)
            .expect("the directory can be listed")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".session.json"))
            .collect();
        names.sort();
        names
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A manager, its settings and somewhere to write.
#[derive(Debug)]
struct Harness {
    manager: SessionManager,
    directory: TestDirectory,
}

impl Harness {
    fn new(label: &str) -> Self {
        Self::with(label, |settings| settings)
    }

    /// A manager resolving every recording's settings from `configuration`.
    fn configured(label: &str, configuration: Configuration) -> Self {
        let mut harness = Self::new(label);
        harness.manager.set_configuration(configuration);
        harness
    }

    fn with(label: &str, adjust: impl FnOnce(AutomaticSettings) -> AutomaticSettings) -> Self {
        let directory = TestDirectory::new(label);
        let settings = adjust(
            AutomaticSettings::new(directory.path().to_path_buf())
                .with_restart_grace(GRACE)
                .with_recording_restart_delay(RESTART_DELAY)
                .with_suspend_gap(SUSPEND_GAP),
        );
        let catalogue =
            Catalogue::parse(GAMES, EntrySource::Seed).expect("the fixture is a valid catalogue");
        Self {
            manager: SessionManager::new(catalogue, settings),
            directory,
        }
    }

    // The four driving methods, wrapped so that a step a test is only setting
    // up can discard what it produced. The manager's own are `#[must_use]`,
    // deliberately — a driver that dropped an action would drop a recording —
    // and `let _ =` at twenty call sites would be noise around the assertions
    // that matter (AGENTS.md section 15).

    fn observe(&mut self, event: &WatchEvent, now: SystemTime) -> Vec<SessionAction> {
        self.manager.observe(event, now)
    }

    fn poll(&mut self, now: SystemTime) -> Vec<SessionAction> {
        self.manager.poll(now)
    }

    fn finished(
        &mut self,
        recording: &RecordingId,
        outcome: RecordingOutcome,
        now: SystemTime,
    ) -> Vec<SessionAction> {
        self.manager.recording_finished(recording, outcome, now)
    }

    fn shut_down(&mut self, now: SystemTime) -> Vec<SessionAction> {
        self.manager.shut_down(now)
    }
}

/// The moment everything is measured from: 2026-08-11T14:32:05Z.
fn t(seconds: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_786_458_725 + seconds)
}

/// A launch of one process.
///
/// The launch identifier is [`LaunchId::ALREADY_RUNNING`] because it is the
/// only one this crate can construct and nothing in the policy reads it — a
/// launch is identified to the manager by the processes in it.
fn launch(processes: &[(u32, &str)]) -> WatchEvent {
    WatchEvent::Launched(LaunchGroup {
        id: LaunchId::ALREADY_RUNNING,
        processes: processes
            .iter()
            .map(|(pid, name)| ProcessSnapshot::new(*pid, 4, None, name))
            .collect(),
    })
}

/// One process exiting.
fn exit(pid: u32, name: &str) -> WatchEvent {
    WatchEvent::Exited(ProcessExit {
        process: ProcessSnapshot::new(pid, 4, None, name),
        launch: LaunchId::ALREADY_RUNNING,
    })
}

/// A finished recording, as the driver would report it.
fn recorded(output: &Path, end_reason: EndReason) -> RecordingOutcome {
    RecordingOutcome::Recorded(Box::new(RecordingReport {
        output: output.to_path_buf(),
        capture_method: CaptureMethod::WindowsGraphicsCapture,
        encoder: EncoderKind::Nvenc,
        codec: Codec::H264,
        width: 1280,
        height: 720,
        requested_framerate: 60,
        frames_captured: 200,
        frames_encoded: 181,
        frames_skipped_for_rate: 19,
        frames_dropped_writer_behind: 0,
        frames_missed_by_source: 0,
        times_target_minimised: 0,
        packets_written: 181,
        timestamps_corrected: 0,
        duration: Duration::from_secs(6),
        end_reason,
        audio_tracks: Vec::new(),
    }))
}

/// The one start in `actions`, or a panic naming what was there instead.
fn one_start(actions: &[SessionAction]) -> &RecordingRequest {
    let starts: Vec<&RecordingRequest> = actions
        .iter()
        .filter_map(|action| match action {
            SessionAction::StartRecording(request) => Some(request),
            _ => None,
        })
        .collect();
    assert_eq!(
        starts.len(),
        1,
        "expected exactly one recording to be started, got {actions:?}"
    );
    starts[0]
}

/// Whether anything in `actions` starts a recording.
fn starts_a_recording(actions: &[SessionAction]) -> bool {
    actions
        .iter()
        .any(|action| matches!(action, SessionAction::StartRecording(_)))
}

/// The stop in `actions`, if there is one.
fn stop_cause(actions: &[SessionAction]) -> Option<StopCause> {
    actions.iter().find_map(|action| match action {
        SessionAction::StopRecording { cause, .. } => Some(*cause),
        _ => None,
    })
}

/// The session `actions` ended, if one did.
fn ended_session(actions: &[SessionAction]) -> Option<&Session> {
    actions.iter().find_map(|action| match action {
        SessionAction::SessionEnded(session) => Some(session.as_ref()),
        _ => None,
    })
}

/// Starts a session for `test-game` at `pid`, and returns what it asked for.
fn started(harness: &mut Harness, pid: u32, now: SystemTime) -> RecordingId {
    let actions = harness.observe(&launch(&[(pid, "test-game.exe")]), now);
    one_start(&actions).recording.clone()
}

#[test]
fn a_launch_the_catalogue_does_not_claim_is_not_a_game() {
    // The overwhelming majority of process launches on a Windows machine.
    let mut harness = Harness::new("unclaimed");
    let actions = harness
        .manager
        .observe(&launch(&[(100, "notepad.exe")]), t(0));

    assert!(actions.is_empty(), "{actions:?}");
    assert!(harness.manager.active_session().is_none());
    assert!(harness.directory.sidecars().is_empty());
}

#[test]
fn a_recognised_launch_starts_a_session_and_records_the_game() {
    // The whole point of the ticket: nobody pressed anything.
    let mut harness = Harness::new("recognised");
    let actions = harness.manager.observe(
        &launch(&[(10, "launcher.exe"), (11, "test-game.exe")]),
        t(0),
    );

    let request = one_start(&actions);
    assert_eq!(
        request.process_id, 11,
        "the game was recorded, not the launcher that started it"
    );
    assert_eq!(request.game.slug(), "test-game");
    assert_eq!(
        request.output,
        harness
            .manager
            .active_session()
            .expect("a session is open")
            .recording_path(harness.directory.path(), 1)
    );
    assert_eq!(
        harness.directory.sidecars().len(),
        1,
        "the session should be on disk before its first frame is captured"
    );
}

#[test]
fn the_game_vanishing_stops_the_recording() {
    // A crash and a quit are the same event here: the process is gone. What
    // must not happen is a recording left running against a window that no
    // longer exists.
    let mut harness = Harness::new("crash");
    let recording = started(&mut harness, 11, t(0));

    let actions = harness.observe(&exit(11, "test-game.exe"), t(10));

    assert_eq!(stop_cause(&actions), Some(StopCause::GameExited));
    assert!(
        actions.contains(&SessionAction::StopRecording {
            recording,
            cause: StopCause::GameExited
        }),
        "the stop must name the recording that is running: {actions:?}"
    );
}

#[test]
fn a_session_ends_once_the_game_has_been_gone_for_the_grace_period() {
    let mut harness = Harness::new("grace");
    let recording = started(&mut harness, 11, t(0));

    harness.observe(&exit(11, "test-game.exe"), t(10));
    let finished = harness.finished(
        &recording,
        recorded(Path::new("one.mkv"), EndReason::TargetLost),
        t(11),
    );
    assert!(
        ended_session(&finished).is_none(),
        "a session must stay open for the game to come back: {finished:?}"
    );

    // One second before the grace expires, and one second after.
    let waiting = harness.poll(t(40));
    assert!(ended_session(&waiting).is_none(), "{waiting:?}");

    let closed = harness.poll(t(42));
    let session = ended_session(&closed).expect("the session ends when the grace expires");
    assert_eq!(session.recordings().len(), 1);
    assert!(harness.manager.active_session().is_none());
}

#[test]
fn a_fast_restart_is_one_session_with_two_recordings_in_it() {
    // Alt-F4 and straight back in. Two files, because one container cannot
    // span the gap; one session, because a library fragmented by every restart
    // is the failure on the other side of this decision.
    let mut harness = Harness::new("fast-restart");
    let first = started(&mut harness, 11, t(0));
    let session_id = first.session.clone();

    harness.observe(&exit(11, "test-game.exe"), t(10));
    harness.finished(
        &first,
        recorded(Path::new("one.mkv"), EndReason::TargetLost),
        t(11),
    );

    let relaunched = harness
        .manager
        .observe(&launch(&[(12, "test-game.exe")]), t(15));
    let second = one_start(&relaunched);

    assert_eq!(
        second.recording.session, session_id,
        "a relaunch inside the grace belongs to the session it left"
    );
    assert_eq!(second.recording.index, 2);
    assert_eq!(second.process_id, 12);
    assert_ne!(
        second.output,
        harness.manager.active_session().expect("open").recordings()[0]
            .output()
            .to_path_buf(),
        "the second recording must not be written over the first"
    );
    assert_eq!(
        harness.directory.sidecars().len(),
        1,
        "one sitting is one sidecar: {:?}",
        harness.directory.sidecars()
    );
}

#[test]
fn a_relaunch_after_the_grace_is_a_session_of_its_own() {
    let mut harness = Harness::new("slow-restart");
    let first = started(&mut harness, 11, t(0));
    let first_session = first.session.clone();

    harness.observe(&exit(11, "test-game.exe"), t(10));
    harness.finished(
        &first,
        recorded(Path::new("one.mkv"), EndReason::TargetLost),
        t(11),
    );
    harness.poll(t(45));

    let relaunched = harness
        .manager
        .observe(&launch(&[(12, "test-game.exe")]), t(90));
    let second = one_start(&relaunched);

    assert_ne!(
        second.recording.session, first_session,
        "a game relaunched a minute later is a second sitting"
    );
    assert_eq!(second.recording.index, 1);
    assert_eq!(
        harness.directory.sidecars().len(),
        2,
        "two sittings are two sidecars: {:?}",
        harness.directory.sidecars()
    );
}

#[test]
fn a_known_child_process_holds_the_session_open_and_is_never_recorded() {
    // The catalogue carries a game's helpers and deliberately does not match
    // on them. Here they mean one thing and one only: the session is not over.
    // Recording one instead would be recording somebody's crash reporter.
    let mut harness = Harness::new("child");
    let started_actions = harness.observe(
        &launch(&[(11, "test-game.exe"), (12, "test-helper.exe")]),
        t(0),
    );
    let recording = one_start(&started_actions).recording.clone();

    harness.observe(&exit(11, "test-game.exe"), t(10));
    harness.finished(
        &recording,
        recorded(Path::new("one.mkv"), EndReason::TargetLost),
        t(11),
    );

    // Far past the grace period. The helper is still running, so the session
    // has not started counting yet — and nothing is recording it.
    let waiting = harness.poll(t(200));
    assert!(ended_session(&waiting).is_none(), "{waiting:?}");
    assert!(
        !starts_a_recording(&waiting),
        "a helper is not a capture target: {waiting:?}"
    );
    assert!(!harness.manager.is_recording());

    harness.observe(&exit(12, "test-helper.exe"), t(210));
    assert!(ended_session(&harness.poll(t(215))).is_none());
    let closed = harness.poll(t(245));
    assert!(
        ended_session(&closed).is_some(),
        "the grace runs from the last of the game's processes going: {closed:?}"
    );
}

#[test]
fn a_second_game_during_a_session_is_noted_and_recorded_when_the_first_ends() {
    // There is one encoder and one capture target. The recording in progress
    // keeps them; the second game is not lost, it waits.
    let mut harness = Harness::new("second-game");
    let first = started(&mut harness, 11, t(0));

    let intruding = harness
        .manager
        .observe(&launch(&[(20, "other-game.exe")]), t(30));
    assert!(
        intruding.is_empty(),
        "the recording in progress must not be pre-empted: {intruding:?}"
    );
    assert!(harness
        .manager
        .active_session()
        .expect("open")
        .events()
        .iter()
        .any(|event| matches!(
            event.kind(),
            SessionEventKind::AnotherGameStarted { game_id, pid: 20 } if game_id == "other-game"
        )));

    harness.observe(&exit(11, "test-game.exe"), t(60));
    harness.finished(
        &first,
        recorded(Path::new("one.mkv"), EndReason::TargetLost),
        t(61),
    );
    let closed = harness.poll(t(95));

    assert!(ended_session(&closed).is_some(), "{closed:?}");
    let promoted = one_start(&closed);
    assert_eq!(promoted.game.slug(), "other-game");
    assert_eq!(promoted.process_id, 20);
    assert_eq!(promoted.recording.index, 1);
}

#[test]
fn a_second_game_that_has_since_exited_is_not_recorded_afterwards() {
    let mut harness = Harness::new("second-game-gone");
    let first = started(&mut harness, 11, t(0));

    harness.observe(&launch(&[(20, "other-game.exe")]), t(30));
    harness.observe(&exit(20, "other-game.exe"), t(40));

    harness.observe(&exit(11, "test-game.exe"), t(60));
    harness.finished(
        &first,
        recorded(Path::new("one.mkv"), EndReason::TargetLost),
        t(61),
    );
    let closed = harness.poll(t(95));

    assert!(ended_session(&closed).is_some(), "{closed:?}");
    assert!(
        !starts_a_recording(&closed),
        "a game that quit while it waited must not produce a recording of nothing: {closed:?}"
    );
}

#[test]
fn a_deferred_games_helper_that_exits_while_it_waits_is_still_accounted_for() {
    // The worst thing this manager can do. A process is reported as exiting
    // exactly once, so a deferred game's helper that is not accounted for while
    // it waits can never be accounted for afterwards: it is promoted with the
    // game as a live child that is already dead. Its session then never runs
    // out of live processes, never starts its grace period and never ends — and
    // a session that never ends defers every game launched after it. The
    // recorder goes on running, says nothing is wrong, and records nothing ever
    // again.
    let mut harness = Harness::new("deferred-child-exit");

    // A session of a game with no helpers of its own, so that the only children
    // in play belong to the game that is made to wait.
    let first = one_start(&harness.observe(&launch(&[(20, "other-game.exe")]), t(0)))
        .recording
        .clone();

    harness.observe(
        &launch(&[(11, "test-game.exe"), (12, "test-helper.exe")]),
        t(30),
    );
    // The helper quits while its game is still waiting its turn. This is the
    // only time the watcher will ever mention pid 12.
    harness.observe(&exit(12, "test-helper.exe"), t(40));

    harness.observe(&exit(20, "other-game.exe"), t(60));
    harness.finished(
        &first,
        recorded(Path::new("one.mkv"), EndReason::TargetLost),
        t(61),
    );

    let promoted = harness.poll(t(95));
    assert!(ended_session(&promoted).is_some(), "{promoted:?}");
    assert_eq!(one_start(&promoted).process_id, 11);
    let second = one_start(&promoted).recording.clone();

    // The promoted game quits, and nothing it was launched with is left.
    harness.observe(&exit(11, "test-game.exe"), t(120));
    harness.finished(
        &second,
        recorded(Path::new("two.mkv"), EndReason::TargetLost),
        t(121),
    );

    let closed = harness.poll(t(200));
    assert!(
        ended_session(&closed).is_some(),
        "the promoted session must end: its game has exited and the helper it was launched with \
         exited before it was promoted, so nothing of it is running: {closed:?}"
    );
    assert!(harness.manager.active_session().is_none());

    // And the consequence that makes this worse than a crash, asserted rather
    // than reasoned about: a session that never ends silently swallows every
    // game after it.
    let afterwards = harness.observe(&launch(&[(30, "other-game.exe")]), t(210));
    assert_eq!(
        one_start(&afterwards).process_id,
        30,
        "the next game to launch must still be recorded: {afterwards:?}"
    );
}

#[test]
fn a_tie_in_the_catalogue_is_recorded_and_left_unattributed() {
    // The catalogue refuses to guess between two entries that claim the same
    // executable equally well. Refusing to record would lose footage; guessing
    // would file it under the wrong game. It is recorded, and the candidates
    // are written down.
    let mut harness = Harness::new("ambiguous");
    let actions = harness.observe(&launch(&[(30, "shared.exe")]), t(0));

    let request = one_start(&actions);
    assert_eq!(
        request.game,
        GameIdentity::Ambiguous {
            candidates: vec!["first-tie".to_owned(), "second-tie".to_owned()]
        }
    );
    assert_eq!(request.game.slug(), UNATTRIBUTED);
    assert!(
        request.output.to_string_lossy().contains(UNATTRIBUTED),
        "the file must not be named after one of the candidates: {}",
        request.output.display()
    );

    let sidecar = harness
        .directory
        .path()
        .join(&harness.directory.sidecars()[0]);
    let text = fs::read_to_string(sidecar).expect("the sidecar was written");
    assert!(
        text.contains("first-tie") && text.contains("second-tie"),
        "both candidates belong in the session's own record: {text}"
    );
}

#[test]
fn a_suspend_ends_the_recording_and_starts_another_in_the_same_session() {
    // A file whose timestamps span eight hours of nothing is not a recording
    // anybody can use.
    let mut harness = Harness::new("suspend");
    let first = started(&mut harness, 11, t(0));
    let session_id = first.session.clone();

    let resumed = harness.poll(t(8 * 60 * 60));
    assert_eq!(stop_cause(&resumed), Some(StopCause::SystemResumed));

    let after = harness.finished(
        &first,
        recorded(Path::new("one.mkv"), EndReason::Stopped),
        t(8 * 60 * 60 + 1),
    );
    assert!(!starts_a_recording(&after), "the restart waits: {after:?}");

    let restarted = harness.poll(t(8 * 60 * 60 + 10));
    let second = one_start(&restarted);
    assert_eq!(
        second.recording.session, session_id,
        "the sitting did not end because the machine slept"
    );
    assert_eq!(second.recording.index, 2);
}

#[test]
fn a_suspend_while_a_session_waits_for_a_restart_ends_it_rather_than_extending_it() {
    // The grace period measures the seconds between alt-F4 and coming back. If
    // eight hours went by, the player did not come back — and a game launched
    // after a resume must not be joined to yesterday's session.
    let mut harness = Harness::new("suspend-grace");
    let recording = started(&mut harness, 11, t(0));
    harness.observe(&exit(11, "test-game.exe"), t(10));
    harness.finished(
        &recording,
        recorded(Path::new("one.mkv"), EndReason::TargetLost),
        t(11),
    );

    let resumed = harness.poll(t(8 * 60 * 60));
    let session = ended_session(&resumed).expect("the session ends on the resume");
    assert!(session
        .events()
        .iter()
        .any(|event| matches!(event.kind(), SessionEventKind::SystemResumed { .. })));
    // Ended *by the resume*, not by its grace period happening to have run out
    // in the meantime. The two are indistinguishable in wall-clock terms once
    // eight hours have passed, and they are not the same answer: a session
    // record that says `game-exited` describes a player who stopped playing,
    // and this one describes a machine that was closed.
    assert!(
        matches!(
            session.events().last().map(SessionEvent::kind),
            Some(SessionEventKind::Ended {
                reason: SessionEndReason::SystemResumed
            })
        ),
        "the session should say the resume ended it: {:?}",
        session.events()
    );
    assert!(harness.manager.active_session().is_none());

    let relaunched = harness.observe(&launch(&[(12, "test-game.exe")]), t(8 * 60 * 60 + 5));
    assert_eq!(
        one_start(&relaunched).recording.index,
        1,
        "the game coming back after a resume is a new sitting"
    );
}

#[test]
fn a_recording_that_ends_while_the_game_runs_is_followed_by_another() {
    // A window destroyed and recreated on a resolution change, which is what
    // `EndReason::TargetResized` is for: the file has to end, the sitting has
    // not.
    let mut harness = Harness::new("resized");
    let first = started(&mut harness, 11, t(0));

    let finished = harness.finished(
        &first,
        recorded(Path::new("one.mkv"), EndReason::TargetResized),
        t(20),
    );
    assert!(
        !starts_a_recording(&finished),
        "the restart waits long enough for an exit to arrive: {finished:?}"
    );

    let waiting = harness.poll(t(22));
    assert!(!starts_a_recording(&waiting), "{waiting:?}");

    let restarted = harness.poll(t(26));
    let second = one_start(&restarted);
    assert_eq!(second.recording.session, first.session);
    assert_eq!(second.recording.index, 2);
}

#[test]
fn a_recording_that_found_no_window_is_not_tried_again_for_the_same_process() {
    // Otherwise a process that matched the catalogue and never opens a window
    // would have the driver searching the desktop for it, over and over, for
    // as long as it ran.
    let mut harness = Harness::new("no-window");
    let first = started(&mut harness, 11, t(0));

    harness.finished(
        &first,
        RecordingOutcome::NoWindow {
            detail: "no window matches process 11".to_owned(),
        },
        t(70),
    );

    for second in [80, 120, 300] {
        let actions = harness.poll(t(second));
        assert!(
            !starts_a_recording(&actions),
            "nothing should be retried at t+{second}: {actions:?}"
        );
    }
    assert!(
        harness.manager.active_session().is_some(),
        "the session stays open until the game exits, so its record is complete"
    );
}

#[test]
fn a_session_stops_starting_recordings_once_it_reaches_its_cap() {
    // The loop guard. A game that somehow ended every recording immediately
    // would otherwise fill a disk with three-frame files.
    let mut harness = Harness::with("cap", |settings| {
        settings.with_max_recordings_per_session(2)
    });
    let first = started(&mut harness, 11, t(0));

    harness.finished(
        &first,
        recorded(Path::new("one.mkv"), EndReason::TargetResized),
        t(1),
    );
    let second = one_start(&harness.poll(t(10))).recording.clone();

    harness.finished(
        &second,
        recorded(Path::new("two.mkv"), EndReason::TargetResized),
        t(11),
    );
    let capped = harness.poll(t(20));

    assert!(!starts_a_recording(&capped), "{capped:?}");
    assert!(harness
        .manager
        .active_session()
        .expect("the session stays open")
        .events()
        .iter()
        .any(|event| matches!(
            event.kind(),
            SessionEventKind::RecordingLimitReached { limit: 2 }
        )));
}

#[test]
fn shutting_down_stops_the_recording_and_closes_the_session_when_it_reports() {
    let mut harness = Harness::new("shutdown");
    let recording = started(&mut harness, 11, t(0));

    let asked = harness.shut_down(t(30));
    assert_eq!(stop_cause(&asked), Some(StopCause::RecorderStopping));
    assert!(
        ended_session(&asked).is_none(),
        "the session is not closed until its file is finished: {asked:?}"
    );

    let closed = harness.finished(
        &recording,
        recorded(Path::new("one.mkv"), EndReason::Stopped),
        t(31),
    );
    let session = ended_session(&closed).expect("the session closes once the recording reports");
    assert_eq!(session.recordings().len(), 1);
    assert!(session.recordings()[0]
        .outcome()
        .expect("the outcome was stored")
        .produced_a_file());

    let afterwards = harness
        .manager
        .observe(&launch(&[(12, "test-game.exe")]), t(40));
    assert!(
        afterwards.is_empty(),
        "a stopping recorder must not start another session: {afterwards:?}"
    );
}

#[test]
fn shutting_down_with_nothing_recording_closes_the_session_at_once() {
    let mut harness = Harness::new("shutdown-idle");
    let recording = started(&mut harness, 11, t(0));
    harness.finished(
        &recording,
        RecordingOutcome::Failed {
            detail: "the encoder went".to_owned(),
        },
        t(5),
    );

    let closed = harness.shut_down(t(6));
    assert!(ended_session(&closed).is_some(), "{closed:?}");
    assert!(stop_cause(&closed).is_none(), "{closed:?}");
}

#[test]
fn shutting_down_while_a_stop_is_already_pending_still_waits_for_the_outcome() {
    // Ctrl+C a moment after the game quit. The recording is already being
    // stopped and is already finalising its file, and its outcome is still
    // coming. Closing the session here would throw that outcome away: the
    // sidecar would say a recording began and never ended, and the summary
    // would tell the user there were no recordings — about a file sitting on
    // disk, complete and playable.
    let mut harness = Harness::new("shutdown-mid-stop");
    let recording = started(&mut harness, 11, t(0));

    let exited = harness.observe(&exit(11, "test-game.exe"), t(10));
    assert_eq!(stop_cause(&exited), Some(StopCause::GameExited));

    let asked = harness.shut_down(t(11));
    assert!(
        ended_session(&asked).is_none(),
        "the session must not close while a recording still owes it an outcome: {asked:?}"
    );
    assert!(
        stop_cause(&asked).is_none(),
        "the recording is already stopping; a second request would be noise: {asked:?}"
    );

    let closed = harness.finished(
        &recording,
        recorded(Path::new("one.mkv"), EndReason::TargetLost),
        t(12),
    );
    let session = ended_session(&closed).expect("the outcome closes the session");
    assert_eq!(session.recordings().len(), 1);
    assert!(
        session.recordings()[0]
            .outcome()
            .expect("the recording's outcome reached the session it belongs to")
            .produced_a_file(),
        "the session must know about the file that was made: {:?}",
        session.recordings()
    );
}

#[test]
fn an_outcome_for_a_recording_the_session_has_moved_past_is_ignored() {
    // The driver reports outcomes asynchronously, so a late one must not be
    // applied to the recording that replaced it.
    let mut harness = Harness::new("late-outcome");
    let first = started(&mut harness, 11, t(0));
    harness.finished(
        &first,
        recorded(Path::new("one.mkv"), EndReason::TargetResized),
        t(1),
    );
    let second = one_start(&harness.poll(t(10))).recording.clone();

    let late = harness.finished(
        &first,
        RecordingOutcome::Failed {
            detail: "arriving twice".to_owned(),
        },
        t(11),
    );

    assert!(late.is_empty(), "{late:?}");
    assert!(
        harness.manager.is_recording(),
        "the recording that is running must not have been ended by somebody else's outcome"
    );
    let session = harness.manager.active_session().expect("open");
    assert_eq!(session.recordings().len(), 2);
    assert!(
        session.recordings()[0]
            .outcome()
            .expect("the first outcome is the one it was given")
            .produced_a_file(),
        "the late failure must not have overwritten it"
    );
    assert_eq!(session.recordings()[1].outcome(), None);
    assert_eq!(second.index, 2);
}

#[test]
fn a_game_already_running_when_the_recorder_starts_is_reported_rather_than_recorded() {
    // Full Session means "start when the game starts". Joining halfway would
    // produce a session that began at an arbitrary moment — but saying nothing
    // would leave the user wondering why nothing is being recorded.
    let harness = Harness::new("already-running");
    let running = [
        ProcessSnapshot::new(11, 4, None, "test-game.exe"),
        ProcessSnapshot::new(12, 4, None, "notepad.exe"),
    ];

    let found = harness.manager.already_running_games(&running);

    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].slug(), "test-game");
    assert!(harness.manager.active_session().is_none());
}

#[test]
fn the_sessions_record_is_on_disk_before_anything_needs_it() {
    // AGENTS.md section 17: irreplaceable state does not live only in the
    // memory of a process that is expected to be killed. Which game a file is
    // of, and which files belong together, is exactly that.
    let mut harness = Harness::new("sidecar");
    let recording = started(&mut harness, 11, t(0));

    let path = harness
        .directory
        .path()
        .join(&harness.directory.sidecars()[0]);
    let while_recording = fs::read_to_string(&path).expect("written when the session started");
    assert!(while_recording.contains("test-game"), "{while_recording}");
    assert!(
        while_recording.contains("recording-started"),
        "{while_recording}"
    );

    harness.observe(&exit(11, "test-game.exe"), t(10));
    harness.finished(
        &recording,
        recorded(Path::new("one.mkv"), EndReason::TargetLost),
        t(11),
    );
    harness.poll(t(45));

    let afterwards = fs::read_to_string(&path).expect("rewritten as the session changed");
    assert!(afterwards.contains("session-ended"), "{afterwards}");
    assert!(
        afterwards.contains("\"frames_encoded\": 181"),
        "{afterwards}"
    );
    assert_eq!(
        harness.directory.sidecars().len(),
        1,
        "a session has one file, rewritten, not one per change"
    );
}

/// A configuration in which one game records at 1440p and another at 1080p,
/// over a global frame rate of 60.
///
/// The shape of the ticket's first acceptance criterion, and of AGENTS.md
/// section 30's worked example: two games that differ, and the games that were
/// never mentioned, which follow the global settings.
fn two_games_configured() -> Configuration {
    let mut configuration = Configuration::defaults();

    let mut global = Preferences::none();
    global
        .set_framerate(Some(60))
        .expect("60 is an acceptable frame rate");
    configuration.set_global(global);

    let mut first = Preferences::none();
    first
        .set_resolution(Some(ResolutionSetting::Fixed {
            width: 2560,
            height: 1440,
        }))
        .expect("1440p is an acceptable size");
    configuration.set_game(
        GameKey::parse("test-game").expect("a valid identifier"),
        first,
    );

    let mut second = Preferences::none();
    second
        .set_resolution(Some(ResolutionSetting::Fixed {
            width: 1920,
            height: 1080,
        }))
        .expect("1080p is an acceptable size");
    configuration.set_game(
        GameKey::parse("other-game").expect("a valid identifier"),
        second,
    );

    configuration
}

#[test]
fn each_game_is_recorded_at_its_own_settings_over_the_global_ones() {
    // The ticket, in one test: two games launch, and each is recorded at the
    // size its own settings name while both follow the global frame rate.
    let mut harness = Harness::configured("per-game", two_games_configured());

    let actions = harness.observe(&launch(&[(11, "test-game.exe")]), t(0));
    let recording = one_start(&actions).recording.clone();
    let first = one_start(&actions).settings.clone();

    // The first session has to end before the second game is recorded: there
    // is one encoder and one capture target.
    harness.observe(&exit(11, "test-game.exe"), t(10));
    harness.finished(
        &recording,
        recorded(Path::new("one.mkv"), EndReason::TargetLost),
        t(11),
    );
    harness.poll(t(45));
    let second = harness.observe(&launch(&[(21, "other-game.exe")]), t(46));
    let second = one_start(&second).settings.clone();

    assert_eq!(
        first.resolution().get(),
        ResolutionSetting::Fixed {
            width: 2560,
            height: 1440
        }
    );
    assert_eq!(first.resolution().source(), SettingSource::Game);
    assert_eq!(
        second.resolution().get(),
        ResolutionSetting::Fixed {
            width: 1920,
            height: 1080
        }
    );
    assert_eq!(second.resolution().source(), SettingSource::Game);

    // And the setting neither of them overrode is the same for both, from the
    // layer they share.
    assert_eq!(first.framerate().get(), 60);
    assert_eq!(second.framerate().get(), 60);
    assert_eq!(first.framerate().source(), SettingSource::Global);
    assert_eq!(second.framerate().source(), SettingSource::Global);
}

#[test]
fn a_game_nobody_configured_records_exactly_as_the_global_settings_say() {
    // The half of inheritance that is easy to break: a game with no section of
    // its own must be unaffected by the sections other games have. Every
    // setting, and its source, has to match the global answer exactly —
    // "inherited 60" and "set to 60" are different answers and only one of
    // them is right here.
    let configuration = two_games_configured();
    let global = configuration.resolve_global();
    let mut harness = Harness::configured("inherited", configuration);

    let actions = harness.observe(&launch(&[(31, "third-game.exe")]), t(0));
    let request = one_start(&actions);
    assert_eq!(request.game.slug(), "third-game");

    for key in SettingKey::ALL {
        assert_eq!(
            request.settings.written_value(key),
            global.written_value(key),
            "{key} differs from the global settings"
        );
        assert_eq!(
            request.settings.source_of(key),
            global.source_of(key),
            "{key} came from a different layer than the global settings"
        );
    }
}

#[test]
fn a_manager_given_no_configuration_records_every_game_at_the_shipped_defaults() {
    // What a caller that has no settings file to offer gets, and the state
    // every existing driver is in until it hands one over: nothing overridden,
    // every answer the built-in default.
    let mut harness = Harness::new("defaults");

    let actions = harness.observe(&launch(&[(11, "test-game.exe")]), t(0));
    let settings = &one_start(&actions).settings;

    for key in SettingKey::ALL {
        assert_eq!(
            settings.source_of(key),
            SettingSource::Default,
            "{key} came from somewhere, and nothing was configured"
        );
    }
    assert_eq!(settings.framerate().get(), crate::DEFAULT_FRAMERATE);
    assert_eq!(settings.resolution().get(), ResolutionSetting::Source);
}

#[test]
fn a_tie_between_games_is_recorded_at_the_global_settings_rather_than_a_candidates() {
    // The catalogue would not choose between the entries, and neither does
    // this: picking one candidate's settings would be the same guess the
    // catalogue refused to make, and it would be invisible afterwards.
    let mut configuration = two_games_configured();
    let mut tied = Preferences::none();
    tied.set_framerate(Some(240))
        .expect("240 is an acceptable frame rate");
    configuration.set_game(
        GameKey::parse("first-tie").expect("a valid identifier"),
        tied,
    );
    let mut harness = Harness::configured("ambiguous", configuration);

    let actions = harness.observe(&launch(&[(41, "shared.exe")]), t(0));
    let request = one_start(&actions);

    assert!(matches!(request.game, GameIdentity::Ambiguous { .. }));
    assert_eq!(request.settings.scope(), &crate::config::Scope::Global);
    assert_eq!(
        request.settings.framerate().get(),
        60,
        "a tied candidate's frame rate must not have been applied"
    );
}

#[test]
fn a_recordings_settings_are_the_ones_it_started_with_however_they_change_after() {
    // The rule the module states, and this is what makes it true: a setting
    // changing under a running encoder is a different feature. What a user who
    // edits their settings mid-game gets is the *next* recording.
    let mut harness = Harness::configured("resolved-once", two_games_configured());
    let recording = started(&mut harness, 11, t(0));
    let started_with = harness
        .manager
        .active_session()
        .expect("a session is open")
        .recordings()[0]
        .settings()
        .clone();

    let mut changed = Preferences::none();
    changed
        .set_framerate(Some(240))
        .expect("240 is an acceptable frame rate");
    let mut configuration = two_games_configured();
    configuration.set_game(
        GameKey::parse("test-game").expect("a valid identifier"),
        changed,
    );
    harness.manager.set_configuration(configuration);
    let nothing = harness.poll(t(5));

    assert!(
        !starts_a_recording(&nothing),
        "changing the settings must not start or restart anything: {nothing:?}"
    );
    assert_eq!(
        harness
            .manager
            .active_session()
            .expect("a session is open")
            .recordings()[0]
            .settings(),
        &started_with,
        "the running recording's settings were re-read after it started"
    );
    assert_eq!(started_with.framerate().get(), 60);

    // The next recording of the same session — the window went and came back —
    // is where the change arrives.
    let next = harness.finished(
        &recording,
        recorded(Path::new("one.mkv"), EndReason::TargetResized),
        t(6),
    );
    assert!(
        !starts_a_recording(&next),
        "the restart delay has not passed: {next:?}"
    );
    let restarted = harness.poll(t(20));
    assert_eq!(
        one_start(&restarted).settings.framerate().get(),
        240,
        "the settings that were changed should apply to the recording that follows"
    );
}

#[test]
fn the_settings_a_recording_started_with_are_in_the_sessions_own_record() {
    // Not only in the log: a log rotates, and "why is this file 1440p" is
    // asked of a library months later (`docs/sessions.md`).
    let mut harness = Harness::configured("recorded-settings", two_games_configured());
    started(&mut harness, 11, t(0));

    let path = harness
        .directory
        .path()
        .join(&harness.directory.sidecars()[0]);
    let written = fs::read_to_string(&path).expect("written when the session started");
    let file: serde_json::Value = serde_json::from_str(&written).expect("the sidecar is JSON");
    let settings = &file["recordings"][0]["settings"];

    assert_eq!(
        settings["resolution"]["value"],
        serde_json::Value::from("2560x1440"),
        "{written}"
    );
    assert_eq!(
        settings["resolution"]["source"],
        serde_json::Value::from("game")
    );
    assert_eq!(
        settings["framerate"]["source"],
        serde_json::Value::from("global")
    );
}

#[test]
fn what_a_game_was_configured_for_reaches_the_recording_it_is_made_with() {
    // The last link: the manager resolves, the driver applies. A setting that
    // is resolved and then cannot reach a recording is a control that silently
    // does nothing (AGENTS.md section 27).
    let mut harness = Harness::configured("applied", two_games_configured());
    let actions = harness.observe(&launch(&[(11, "test-game.exe")]), t(0));

    let recording = one_start(&actions)
        .settings
        .apply_to(RecordingSettings::new(
            CaptureTargetSettings::window(0x1234, 2560, 1440),
            PathBuf::from("out.mkv"),
        ));

    assert_eq!(
        recording.resolution(),
        ResolutionSetting::Fixed {
            width: 2560,
            height: 1440
        }
    );
    assert_eq!(recording.framerate(), 60);
    assert_eq!(
        recording.unavailable_choice(),
        UnavailableChoice::Substitute,
        "a configured recording must not be refused over a setting that has gone stale"
    );
}
