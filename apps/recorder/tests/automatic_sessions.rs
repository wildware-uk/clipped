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
//! # The resize
//!
//! One of these is [ADR 0012](../../../docs/adr/0012-a-session-follows-a-resize-with-a-new-file.md)
//! checked against a real resize, which is
//! [issue #184](https://github.com/wildware-uk/clipped/issues/184)'s first
//! acceptance criterion and the one thing it rules out by name: *"verified
//! against a real resize rather than a unit test of the branch."*
//! `clipped_session::automatic`'s own tests call the branch — they hand
//! `recording_finished` a constructed outcome and read the delay back out of
//! `restart_delay_after` — and no test in the tree had ever taken a window that
//! a capture backend was looking at and changed its size.
//!
//! This one does. `SetWindowPos` is called on the subject's window from this
//! process, which is a resize a user dragging an edge would make: the compositor
//! sees it, Windows Graphics Capture sees it, and the recording loop finds out
//! the way it finds out about a game changing resolution — as
//! `Acquisition::SizeChanged`, discovered rather than announced. Nothing in the
//! recorder is told the resize happened.
//!
//! What is then asserted is the decision, not the mechanism:
//!
//! - **Two files, one sitting.** A resize does not end the session.
//! - **The first one is finished, not abandoned**: `target-resized`, at the size
//!   the window was, decoding as many pictures as the recorder said it encoded.
//! - **The second one carries on**, at the size the window now is.
//! - **The seam between them is small.** That is what #564 built and the only
//!   part of the decision that a wrong build still produces two files for: the
//!   restart delay is skipped for a resize, because that delay is a wait for an
//!   exit that may be in flight and a resize is proof the window did not go
//!   anywhere. The gap is measured off the session's own timeline and printed.
//!
//! And, on the one path a user takes most often and the other two do not touch
//! — Ctrl+C with a game still running and a recording still capturing — that
//! the session record is *finished*: an end reason of `recorder-stopping`, and
//! the recording's outcome stored against it. A sidecar saying a recording
//! began and never ended is what M6's indexer would otherwise have to
//! reconcile against a file that is complete and playable.
//!
//! # The per-game settings
//!
//! One of these is [issue #61](https://github.com/wildware-uk/clipped/issues/61)'s
//! first acceptance criterion, which no test in the tree had ever made a file
//! for: *"one game records at 1440p60 while another records at 1080p60
//! automatically, verified with ffprobe."* The nearest thing that existed was
//! `watch::tests::a_games_own_settings_reach_the_recording_that_is_started_for_it`,
//! which drives the same resolution through the real manager and the real
//! composition and then hands it to a stand-in engine that records nothing —
//! so it proves what the engine was *asked* for and can say nothing about what
//! came out.
//!
//! This one makes the files. Two subjects, two catalogue entries, one
//! `settings.json` with a `games` section naming both, and the two recordings
//! read back with `ffprobe`. The command line and the global layer are both set
//! to values **neither game asks for**, which is what makes the reading mean
//! something: a build where the per-game layer stopped reaching a recording
//! produces two files at the global layer's frame rate and codec, or at the
//! command line's, and the assertions below name the setting rather than the
//! file.
//!
//! **What the resolution does and does not prove.** This build has no scaler:
//! `RecordingSettings::encode_size` records at the size capture is producing and
//! logs a substitution when a configured size is not that
//! ([docs/recorder-cli.md](../../../docs/recorder-cli.md), "No scaling"). So a
//! per-game `resolution` naming the subject's own window size is *honoured*
//! rather than *causal*, and two files of different sizes on their own would say
//! only that two windows were different sizes. Two things are done about that.
//! The command line asks for a size **neither** window is, with the refusal a
//! command line gets — so a build where the per-game `resolution` did not arrive
//! records nothing at all rather than recording the same thing twice. And the
//! frame rate and the codec, which this build genuinely does apply per game,
//! are asserted alongside it off the same `ffprobe` reading.
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
    client_area, ensure_console, example_binary, read_stderr, ready_line, recorder_binary, resize,
    send_ctrl_c, terminate, video_pattern_binary, wait_for_exit, CREATE_NEW_PROCESS_GROUP,
};

/// The rate the pattern application presents at.
const SOURCE_FPS: u32 = 30;

/// How long the pattern renders for in the test that lets it finish.
const PATTERN_SECONDS: u64 = 30;

/// How long a recording is left running before the subject is killed.
const RECORD_BEFORE_KILL: Duration = Duration::from_secs(6);

/// How long the first file records before the window is resized underneath it.
///
/// Long enough that the file it leaves is unambiguously a recording — several
/// seconds of pictures, past the first keyframe and into a second cluster —
/// rather than a header and a trailer with nothing between them, which is the
/// thing "the first file is complete and playable" has to distinguish itself
/// from.
const RECORD_BEFORE_RESIZE: Duration = Duration::from_secs(5);

/// How long the second file is left recording before the recorder is stopped.
///
/// The same argument as above, applied to the successor: a second file that
/// exists is not the claim ADR 0012 makes. The claim is that the sitting carried
/// on, and a file with pictures in it is what says so.
const RECORD_AFTER_RESIZE: Duration = Duration::from_secs(5);

/// The size the subject's window is dragged to, mid-recording.
///
/// Smaller than the 1280x720 it starts at, so that the two files are told apart
/// by their own dimensions and not by their order, and **even in both
/// dimensions**: an odd client area is a defect of its own
/// ([issue #561](https://github.com/wildware-uk/clipped/issues/561),
/// [ADR 0013](../../../docs/adr/0013-capture-rounds-an-odd-dimension-away.md)),
/// and a resize into one would have this test measuring that instead of this.
const RESIZED_TO: (u32, u32) = (1024, 576);

/// The largest gap between the two files this test will accept.
///
/// The figure that matters is `AutomaticSettings::recording_restart_delay`,
/// which is five seconds by default and which ADR 0012 has the session skip for
/// a resize.
///
/// What a healthy run costs is everything the successor has to do and nothing
/// else: finishing the first file, one pass of `watch`'s loop — which promises
/// only to run once a second — re-resolving the window, starting a capture,
/// waiting for a frame, opening an encoder and creating a file. Measured on this
/// project's development machine on 2026-08-18, from an unoptimised build:
/// **0.288 s** and **1.523 s** over two runs, the larger of them the first run
/// after a build, which pays for the encoder capability probe. A build that
/// waited the delay out measured **6.077 s**.
///
/// This sits between the two, with two seconds of headroom above the slower
/// healthy figure so that a busy machine does not trip it, and two and a half
/// below the broken one so that reinstating the delay fails here rather than
/// somewhere vaguer.
const SEAM_BOUND: Duration = Duration::from_millis(3_500);

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

/// One of the two subjects issue #61's first criterion needs.
///
/// A game is a catalogue entry, an executable and a `games` section of
/// `settings.json`, and the point of the pair is that everything about them is
/// the same except what their own entries ask for.
#[derive(Debug, Clone, Copy)]
struct Subject {
    /// What the recorder calls it on the console and in the session record.
    name: &'static str,
    /// The catalogue's `game_id`, which is also the key its settings are under.
    key: &'static str,
    /// The image name the watcher sees, which is a copy of the pattern
    /// application under a name of its own — two games need two executables, and
    /// the watcher recognises a game by the name of its image.
    executable: &'static str,
    /// The client area its window opens at, and therefore the size capture
    /// produces and the only size this build can encode (see the module docs).
    size: (u32, u32),
    /// The codec its own entry asks for, spelled as `settings.json` spells it
    /// and as `ffprobe` names it — the two agree for both of these.
    codec: &'static str,
}

impl Subject {
    /// The settings this game's own entry writes, spelled as `settings.json`
    /// spells them and as the recorder writes them back out.
    ///
    /// One list, read by three assertions — the log line the recording starts
    /// with, the session record beside the files, and the messages naming what
    /// went wrong — so that adding a setting to the fixture cannot leave one of
    /// the three behind (AGENTS.md section 55).
    fn configured(self) -> [(&'static str, String); 3] {
        [
            ("resolution", format!("{}x{}", self.size.0, self.size.1)),
            ("framerate", PER_GAME_FRAMERATE.to_string()),
            ("codec", self.codec.to_owned()),
        ]
    }
}

/// The 1440p subject.
const ALPHA: Subject = Subject {
    name: "Clipped Pattern Alpha",
    key: "clipped-pattern-alpha",
    executable: "pattern-alpha.exe",
    size: (2560, 1440),
    codec: "h264",
};

/// The 1080p subject.
const BETA: Subject = Subject {
    name: "Clipped Pattern Beta",
    key: "clipped-pattern-beta",
    executable: "pattern-beta.exe",
    size: (1920, 1080),
    codec: "hevc",
};

/// The frame rate both games' own entries ask for.
///
/// The same for both, because the criterion asks for 1440p60 and 1080p60 — what
/// differs between the two subjects is the size and the codec. It is still a
/// per-game assertion: neither the command line nor the global layer says 60,
/// so a file at 60 can only have come from the game's own entry.
const PER_GAME_FRAMERATE: u32 = 60;

/// What the global layer says about the frame rate.
///
/// A value neither game asks for, so that a build which resolved the global
/// layer and dropped the per-game one is a distinct, readable failure rather
/// than an accidental pass (AGENTS.md section 30's worked example, inverted).
const GLOBAL_FRAMERATE: u32 = 15;

/// What the global layer says about the codec, for the same reason.
///
/// `auto` would have been the tempting value and is the one that could not be
/// read: `auto` on this project's machine resolves to AV1, which is a real
/// codec name in the file, and a global layer naming it outright is the same
/// answer said out loud.
const GLOBAL_CODEC: &str = "av1";

/// The frame rate the recorder's command line asks for.
///
/// Third value, third layer. A file at 24 says the settings file did not reach
/// the recording at all; a file at [`GLOBAL_FRAMERATE`] says the per-game layer
/// did not.
///
/// Below what capture delivers, as [`GLOBAL_FRAMERATE`] is, so that both wrong
/// answers are *produced* rather than merely asked for. A rate above what the
/// compositor hands over comes out of the file as whatever capture managed,
/// which on this project's machine is the 60 a healthy run also produces.
const COMMAND_LINE_FRAMERATE: &str = "24";

/// The size the recorder's command line asks for.
///
/// Neither subject's window, deliberately. A command line's choice is refused
/// rather than substituted when the machine cannot honour it
/// ([docs/configuration.md](../../../docs/configuration.md), "What a stale
/// setting does"), so a build where the per-game `resolution` did not reach the
/// recording fails both recordings with `ScalingNotSupported` instead of quietly
/// recording both windows at their own size — which is the only way this build,
/// which cannot scale, can make a resolution setting causal.
const COMMAND_LINE_RESOLUTION: &str = "1280x720";

/// How long each subject is recorded for before it is killed.
const RECORD_PER_SUBJECT: Duration = Duration::from_secs(6);

/// The overlay naming both subjects.
fn per_game_overlay() -> String {
    format!(
        r#"schema_version = 1

[[game]]
game_id = "{alpha_key}"
name = "{alpha_name}"
[[game.executables]]
name = "{alpha_exe}"

[[game]]
game_id = "{beta_key}"
name = "{beta_name}"
[[game.executables]]
name = "{beta_exe}"
"#,
        alpha_key = ALPHA.key,
        alpha_name = ALPHA.name,
        alpha_exe = ALPHA.executable,
        beta_key = BETA.key,
        beta_name = BETA.name,
        beta_exe = BETA.executable,
    )
}

/// A user's own `settings.json`: a global layer, and a layer for each game.
///
/// Written as text rather than built through `clipped_session::config`, for the
/// reason [`OVERLAY`] is: this is the file a person edits, and a test that wrote
/// it through the same API that reads it would not notice the two disagreeing.
fn per_game_settings() -> String {
    format!(
        r#"{{
  "version": 1,
  "global": {{
    "framerate": {GLOBAL_FRAMERATE},
    "codec": "{GLOBAL_CODEC}"
  }},
  "games": {{
    "{alpha_key}": {{
      "resolution": "{alpha_width}x{alpha_height}",
      "framerate": {PER_GAME_FRAMERATE},
      "codec": "{alpha_codec}"
    }},
    "{beta_key}": {{
      "resolution": "{beta_width}x{beta_height}",
      "framerate": {PER_GAME_FRAMERATE},
      "codec": "{beta_codec}"
    }}
  }}
}}
"#,
        alpha_key = ALPHA.key,
        alpha_width = ALPHA.size.0,
        alpha_height = ALPHA.size.1,
        alpha_codec = ALPHA.codec,
        beta_key = BETA.key,
        beta_width = BETA.size.0,
        beta_height = BETA.size.1,
        beta_codec = BETA.codec,
    )
}

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
#[ignore = "needs a GPU, an encoder and a desktop session; see the module docs"]
fn a_window_resized_mid_recording_is_followed_by_a_second_file() {
    // Issue #184's first acceptance criterion, and ADR 0012's decision, against
    // a resize this test really makes rather than one it tells the recorder
    // about. Nothing here touches `EndReason`, `restart_delay_after` or any
    // other branch by name: the window changes size, and everything below is
    // read out of the session the recorder wrote.
    let Some(_tools) = require_media_tools() else {
        return;
    };
    ensure_console();

    let workspace = Workspace::new("watch-resize");
    let mut recorder = workspace.start_recorder();

    recorder.wait_for("Watching for games.");
    thread::sleep(Duration::from_secs(1));

    // Long enough to outlive both files and the seam between them, so that
    // nothing here ends because the subject ran out.
    let mut pattern = LaunchedPattern::directly(SOURCE_FPS, 300);
    let before = pattern.client_size();
    assert_ne!(
        before, RESIZED_TO,
        "the subject already has the size this test resizes it to, so the resize would change \
         nothing and the two files could not be told apart"
    );

    recorder.wait_for("Recording Clipped Video Pattern to");
    thread::sleep(RECORD_BEFORE_RESIZE);

    // The resize. A real `SetWindowPos` on the subject's real window, from
    // outside the process that owns it — which is what a user dragging an edge
    // is. The recorder is not told; it finds out through capture.
    resize(pattern.window(), RESIZED_TO);
    assert_eq!(
        client_area(pattern.window()),
        RESIZED_TO,
        "the window did not actually change size, so nothing below would be about a resize"
    );

    // That capture saw it, and not merely that Windows did. Without this line
    // the test could pass on a run where the resize landed after the recording
    // had already ended for some other reason, and `wait_for` reads forwards,
    // so it also fixes the order of the two lines below it.
    recorder.wait_for("the recorded window changed size");
    // The successor, which is the decision. This is the *second* time this line
    // appears in the stream: `wait_for` never rewinds.
    recorder.wait_for("Recording Clipped Video Pattern to");
    thread::sleep(RECORD_AFTER_RESIZE);

    let diagnostics = recorder.stop();
    assert!(
        pattern.is_running(),
        "the subject must still have been running at the end, or a file boundary here says \
         nothing about a resize:\n{diagnostics}"
    );

    let session = workspace.only_session();
    let files = recorded_files(&session, &diagnostics);

    assert_eq!(
        files.len(),
        2,
        "a resize finishes the file and the session starts the next one (ADR 0012); this \
         sitting has {} file(s) in it:\n{session:#}\n{diagnostics}",
        files.len()
    );

    // The first file: finished on purpose, at the size the window was, and
    // playable — which is the whole reason for following a resize with a new
    // file rather than carrying on into a track that cannot hold two sizes.
    assert_eq!(
        files[0]["end_reason"],
        Value::from("target-resized"),
        "the first file should have ended because its target changed size:\n{diagnostics}"
    );
    assert_media_decodes(files[0], before, &diagnostics);
    assert_at_least_seconds(files[0], 2.0, "the file the resize finished", &diagnostics);

    // The second: the sitting carrying on, at the size the window now is. It
    // ends because the recorder was stopped, which is this test doing the
    // stopping and not the resize ending the session.
    assert_eq!(
        files[1]["end_reason"],
        Value::from("stopped"),
        "the second file should have run until the recorder was stopped:\n{diagnostics}"
    );
    assert_media_decodes(files[1], RESIZED_TO, &diagnostics);
    assert_at_least_seconds(
        files[1],
        2.0,
        "the file that followed the resize",
        &diagnostics,
    );

    // And the seam, off the session's own timeline: where the first file's
    // pictures stop, and where the second file's start.
    let seam = seam_between(files[0], files[1], &diagnostics);
    eprintln!(
        "\n=== the seam a resize costs ===\n\
         first file  : {}x{} for {}s, starting at {}ns on the session's timeline\n\
         second file : {}x{} for {}s, starting at {}ns\n\
         seam        : {:.3}s\n",
        files[0]["width"],
        files[0]["height"],
        files[0]["duration_seconds"],
        files[0]["starts_at_nanos"],
        files[1]["width"],
        files[1]["height"],
        files[1]["duration_seconds"],
        files[1]["starts_at_nanos"],
        seam.as_secs_f64(),
    );

    assert!(
        seam <= SEAM_BOUND,
        "the two files are {:.3}s apart on the session's timeline, and this run will accept \
         {:.3}s. The restart delay a recording ordinarily waits out is a wait for a process \
         exit that may still be in flight, and a resize is proof the window did not go \
         anywhere — so ADR 0012 has the session start the successor of a resized recording at \
         once. A seam this size is that skip not happening, and it costs seconds of a game \
         somebody is still playing every time they drag an edge.\n{diagnostics}",
        seam.as_secs_f64(),
        SEAM_BOUND.as_secs_f64(),
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

/// How fast each subject is asked to draw.
///
/// Faster than any of the three layers asks for, so that what limits a file's
/// frame rate is the rate resolved for that game rather than the rate the
/// subject could draw at. What actually arrives is capped by the compositor —
/// 60 a second on this project's machine, measured on 2026-08-19 — which is why
/// no layer here asks for more than that.
const SUBJECT_FPS: u32 = 120;

/// How far from the rate a game's entry asks for its recording may read.
///
/// Wide enough that a busy machine losing a picture or two does not trip it, and
/// far narrower than the gap to either wrong answer: the nearest is
/// [`COMMAND_LINE_FRAMERATE`], which is 36 below.
const FRAMERATE_TOLERANCE: f64 = 6.0;

#[test]
#[ignore = "needs a GPU, an encoder and a desktop session; see the module docs"]
fn each_game_records_at_the_settings_its_own_entry_asks_for() {
    // Issue #61's first acceptance criterion, with the files to show for it.
    // Two games, one settings file, and `ffprobe` on what came out. Nothing
    // here reaches into the configuration: the recorder is the built binary, it
    // reads the settings file off disk the way it does on a real machine, and
    // what is asserted is the picture in each file.
    let Some(_tools) = require_media_tools() else {
        return;
    };
    ensure_console();

    let workspace =
        Workspace::with_settings("watch-per-game", &per_game_overlay(), &per_game_settings());
    let alpha = workspace.subject_binary(ALPHA);
    let beta = workspace.subject_binary(BETA);

    // The command line is the third layer, and it asks for something neither
    // game does — see the constants above.
    let mut recorder = workspace.start_recorder_asking_for(&[
        "--framerate",
        COMMAND_LINE_FRAMERATE,
        "--resolution",
        COMMAND_LINE_RESOLUTION,
    ]);
    recorder.wait_for("Watching for games.");
    thread::sleep(Duration::from_secs(1));

    // One at a time: this process records one thing at a time whoever asked for
    // it, and two games produce two sittings either way.
    record_one_subject(&mut recorder, &alpha, ALPHA);
    record_one_subject(&mut recorder, &beta, BETA);

    let diagnostics = recorder.stop();
    let sessions = workspace.sessions();
    assert_eq!(
        sessions.len(),
        2,
        "two games were launched and each should have a sitting of its own:\n{diagnostics}"
    );

    let alpha_record = playable_recording(session_of(&sessions, ALPHA, &diagnostics), &diagnostics);
    let beta_record = playable_recording(session_of(&sessions, BETA, &diagnostics), &diagnostics);
    let alpha_file = probe_recording(alpha_record, ALPHA);
    let beta_file = probe_recording(beta_record, BETA);

    eprintln!(
        "\n=== what each game's own settings produced ===\n\
         command line : --resolution {COMMAND_LINE_RESOLUTION} --framerate \
         {COMMAND_LINE_FRAMERATE}\n\
         global layer : framerate {GLOBAL_FRAMERATE}, codec {GLOBAL_CODEC}\n\
         {}\n{}\n",
        alpha_file.describe(),
        beta_file.describe(),
    );

    assert_the_settings_reached_the_file(&alpha_file, ALPHA, &diagnostics);
    assert_the_settings_reached_the_file(&beta_file, BETA, &diagnostics);

    // The third acceptance criterion's other half, off the record the recorder
    // wrote next to the files rather than out of a unit test of the writer:
    // each recording carries the settings it was made with, and where each one
    // came from.
    assert_the_settings_are_in_the_session_record(alpha_record, ALPHA, &diagnostics);
    assert_the_settings_are_in_the_session_record(beta_record, BETA, &diagnostics);

    // And that the two are not one answer written twice. Every assertion above
    // is about one game against its own entry; these two are the criterion as
    // it is written — one game recorded at 1440p60 *while another* recorded at
    // 1080p60 — and they are what a build resolving every game against one
    // layer fails.
    assert_ne!(
        (alpha_file.width, alpha_file.height),
        (beta_file.width, beta_file.height),
        "both games recorded at the same size:\n{}\n{}\n{diagnostics}",
        alpha_file.describe(),
        beta_file.describe(),
    );
    assert_ne!(
        alpha_file.codec, beta_file.codec,
        "both games recorded with the same codec, and `{}`'s entry asks for {} while `{}`'s \
         asks for {}. One codec for two games is what a build that resolved every game \
         against the global layer produces, and the global layer here says \
         {GLOBAL_CODEC}:\n{diagnostics}",
        ALPHA.key, ALPHA.codec, BETA.key, BETA.codec,
    );
}

/// Launches one subject, records it, and kills it.
///
/// The sitting is waited out rather than left to overlap the next one: a
/// recording ends when the window goes and the sitting stays open for its
/// restart grace after that, and two overlapping sittings would make the two
/// session records harder to attribute than the thing they are here to show.
fn record_one_subject(recorder: &mut RecorderProcess, binary: &Path, subject: Subject) {
    let pattern = LaunchedPattern::as_subject(binary, subject, SUBJECT_FPS, 300);
    assert_the_settings_resolved_for_the_game(recorder, subject);
    recorder.wait_for(&format!("Recording {} to", subject.name));
    thread::sleep(RECORD_PER_SUBJECT);

    terminate(pattern.process_id());
    recorder.wait_for("recording file finished");
    // The sitting closing, so the next launch cannot land inside this one.
    recorder.wait_for(&format!("of {}:", subject.name));
    drop(pattern);
}

/// The settings the recorder resolved for this game, before it records it.
///
/// This is the tripwire, and it is here because of what the failure looks like
/// without it. A build where the per-game layer stopped reaching a recording
/// resolves this game against the global layer, whose `resolution` says nothing,
/// so the command line's is left standing; it names a size neither window is,
/// and a command line's choice is refused rather than substituted
/// (`docs/configuration.md`). The recording therefore fails, no file is written,
/// and every assertion below this point is waiting for something that will never
/// come. Left to itself the test then fails as a ninety-second timeout, which is
/// true and says nothing.
///
/// So the settings are read where the recorder announces them: `clipped_session`
/// writes every setting and the layer that supplied it at the moment a recording
/// starts, which is the line that answers "why was this session recorded like
/// that" months later (AGENTS.md section 35). Each one is checked separately, so
/// a failure names the setting and the layer it came from instead.
fn assert_the_settings_resolved_for_the_game(recorder: &mut RecorderProcess, subject: Subject) {
    let resolved = recorder.wait_for_line("with the settings that apply to this game");

    for (setting, value) in subject.configured() {
        let expected = format!("{setting}={value} (game)");
        assert!(
            resolved.contains(&expected),
            "`{key}`'s own entry asks for {setting} {value}, and the recorder resolved \
             something else for it. The layer in brackets is where each answer came from: \
             `global` is the per-game layer not being read, and `default` is the settings \
             file not being read at all.\n  expected to contain: {expected}\n  resolved: \
             {resolved}",
            key = subject.key,
        );
    }
}

/// The session record of one subject.
fn session_of<'a>(sessions: &'a [Value], subject: Subject, diagnostics: &str) -> &'a Value {
    sessions
        .iter()
        .find(|session| session["game"]["game_id"].as_str() == Some(subject.key))
        .unwrap_or_else(|| {
            panic!(
                "no sitting was filed under `{}`, which is the catalogue identifier this \
                 game's settings are keyed by. A sitting filed under anything else resolves \
                 against the global layer instead of the game's own \
                 layer (docs/configuration.md):\n{sessions:#?}\n{diagnostics}",
                subject.key
            )
        })
}

/// What `ffprobe` says about one game's recording.
#[derive(Debug)]
struct ProbedRecording {
    subject: Subject,
    media: Media,
    path: PathBuf,
    codec: String,
    /// The size this game's entry asks for, which is also its window's size.
    /// What the file says is asserted against it through the validator.
    width: u32,
    height: u32,
    decoded_frames: f64,
    seconds: f64,
}

impl ProbedRecording {
    /// The observed frame rate: pictures a decoder produced, over the span the
    /// file covers.
    ///
    /// **Not `avg_frame_rate` and not `r_frame_rate`.** Both were measured on
    /// this project's machine on 2026-08-19 and neither reads back what the
    /// recording was configured for: a 2560x1440 H.264 file encoded at 60 from
    /// a source drawing 30 reports `r_frame_rate=120/1` and
    /// `avg_frame_rate=125/2`, while an AV1 file encoded at 20 reports `20/1`
    /// for both. Those are what the container and the bitstream declare, by a
    /// different route per codec. What the frame rate setting actually decides
    /// is which captured frames are encoded (`clipped_session::pacing`), and
    /// counting the pictures is the reading of that — which is also a reading
    /// no file whose packets fail to decode can satisfy.
    fn observed_framerate(&self) -> f64 {
        self.decoded_frames / self.seconds
    }

    fn describe(&self) -> String {
        format!(
            "{name:<22}: {width}x{height} {codec}, {frames:.0} decoded pictures over \
             {seconds:.2}s = {rate:.1} fps  ({file})",
            name = self.subject.name,
            width = self.width,
            height = self.height,
            codec = self.codec,
            frames = self.decoded_frames,
            seconds = self.seconds,
            rate = self.observed_framerate(),
            file = self.path.display(),
        )
    }
}

/// Reads one recording with `ffprobe`.
fn probe_recording(recording: &Value, subject: Subject) -> ProbedRecording {
    let path = PathBuf::from(
        recording["output"]
            .as_str()
            .expect("a recording names its file"),
    );
    let media = Media::open(&path).unwrap_or_else(|error| {
        panic!("{}'s recording is not usable at all: {error}", subject.name)
    });

    let (codec, decoded_frames) = {
        let stream = *media.video_streams().first().unwrap_or_else(|| {
            panic!(
                "{}'s recording has no video stream:\n{}",
                subject.name,
                media.inventory()
            )
        });
        let codec = stream
            .field("codec_name")
            .unwrap_or_else(|| {
                panic!(
                    "{}'s recording has no codec:\n{}",
                    subject.name,
                    media.inventory()
                )
            })
            .to_owned();
        let frames = stream
            .field("nb_read_frames")
            .and_then(|value| value.parse::<f64>().ok())
            .unwrap_or_else(|| {
                panic!(
                    "{}'s recording has no decoded picture count:\n{}",
                    subject.name,
                    media.inventory()
                )
            });
        (codec, frames)
    };

    let seconds = media.duration_seconds().unwrap_or_else(|| {
        panic!(
            "{}'s recording has no duration:\n{}",
            subject.name,
            media.inventory()
        )
    });

    ProbedRecording {
        subject,
        media,
        path,
        codec,
        width: subject.size.0,
        height: subject.size.1,
        decoded_frames,
        seconds,
    }
}

/// The effective settings, as the session record beside the files carries them.
fn assert_the_settings_are_in_the_session_record(
    recording: &Value,
    subject: Subject,
    diagnostics: &str,
) {
    for (setting, value) in subject.configured() {
        let recorded = &recording["settings"][setting.to_owned()];
        assert_eq!(
            (&recorded["value"], &recorded["source"]),
            (&Value::from(value.clone()), &Value::from("game")),
            "the session record should say this recording was made at {setting} {value} \
             because `{key}`'s own entry asked for it. A `source` of `global` or `default` \
             here is a file recorded at one answer and filed under \
             another.\n{recording:#}\n{diagnostics}",
            key = subject.key,
        );
    }
}

/// Every per-game setting this build applies, read back off the file.
///
/// Each assertion names the setting and the layer a wrong answer would have come
/// from, because "the two files differ" is a claim about two windows, and "this
/// file is at the rate this game's entry asks for, and not at the rate of the
/// layer beneath it" is a claim about the settings.
fn assert_the_settings_reached_the_file(
    probed: &ProbedRecording,
    subject: Subject,
    diagnostics: &str,
) {
    // The frame rate, which is the load-bearing one: nothing but this game's
    // own entry says 60.
    let observed = probed.observed_framerate();
    assert!(
        (observed - f64::from(PER_GAME_FRAMERATE)).abs() <= FRAMERATE_TOLERANCE,
        "`{key}`'s entry asks for framerate {PER_GAME_FRAMERATE} and its recording holds \
         {observed:.1} pictures a second. The global layer of the same settings file asks \
         for {GLOBAL_FRAMERATE} and the recorder's command line asks for \
         {COMMAND_LINE_FRAMERATE}, so a reading near either of those is the per-game \
         `framerate` not reaching the recording.\n{described}\n{diagnostics}",
        key = subject.key,
        described = probed.describe(),
    );

    // The codec, which is the setting the two games differ on.
    assert_eq!(
        probed.codec,
        subject.codec,
        "`{key}`'s entry asks for codec {wanted} and its recording holds {found}. The global \
         layer asks for {GLOBAL_CODEC}, so that is what the per-game `codec` not reaching \
         the recording reads as.\n{described}\n{diagnostics}",
        key = subject.key,
        wanted = subject.codec,
        found = probed.codec,
        described = probed.describe(),
    );

    // And the size, off `ffprobe`'s own fields rather than the recorder's
    // account of them. This build cannot scale, so a per-game `resolution` can
    // only ever name the size capture is producing; what makes it causal here is
    // the command line, which names a size neither window is and which is
    // refused rather than substituted — so a recording that did not receive this
    // game's own `resolution` is a recording that does not exist (module docs).
    if let Err(report) = probed
        .media
        .validate()
        .video_stream_count(1)
        .video(
            VideoStream::codec(&probed.codec)
                .resolution(probed.width, probed.height)
                // Decoded, not demuxed: a file whose packets all fail to decode
                // still reports a stream, a duration and monotonic timestamps.
                .decoded_frames_at_least(1),
        )
        .monotonic_timestamps()
        .check()
    {
        panic!(
            "`{key}`'s entry asks for resolution {width}x{height}, which is the size its \
             window opens at and the only size this build can encode it \
             at.\n{report}\n{diagnostics}",
            key = subject.key,
            width = probed.width,
            height = probed.height,
        );
    }
}

/// A temporary directory holding a run's clips, its session records and the
/// user data the recorder is pointed at.
#[derive(Debug)]
struct Workspace {
    /// [`Option`] only so that [`Drop`] can decline to remove it. See there.
    directory: Option<TemporaryDirectory>,
}

impl Workspace {
    fn new(label: &str) -> Self {
        Self::with_overlay(label, OVERLAY)
    }

    fn with_overlay(label: &str, overlay: &str) -> Self {
        let directory = Some(TemporaryDirectory::new(label));
        let workspace = Self { directory };

        fs::create_dir_all(workspace.clips()).expect("the clips directory can be created");
        let application = workspace.local_app_data().join("Clipped");
        fs::create_dir_all(&application).expect("the data directory can be created");
        fs::write(application.join("games.toml"), overlay).expect("the overlay can be written");

        workspace
    }

    /// The same, with a `settings.json` beside the overlay.
    ///
    /// The same directory, because that is where the recorder looks for both
    /// (`ConfigurationStore::default_path`), and the reason the child process
    /// gets a `LOCALAPPDATA` of its own is the reason this can be written at
    /// all: nothing here touches the settings of whoever is running the tests.
    fn with_settings(label: &str, overlay: &str, settings: &str) -> Self {
        let workspace = Self::with_overlay(label, overlay);
        fs::write(
            workspace
                .local_app_data()
                .join("Clipped")
                .join("settings.json"),
            settings,
        )
        .expect("the settings file can be written");
        workspace
    }

    /// A copy of the pattern application under a name of the caller's choosing.
    ///
    /// Two games need two executables, because the watcher recognises a game by
    /// the name of its image and one binary can only ever be one game. A copy
    /// rather than a second test application: what is being recorded is not the
    /// point of this test, and a second renderer would be a second thing to keep
    /// working (AGENTS.md section 55).
    fn subject_binary(&self, subject: Subject) -> PathBuf {
        let directory = self.path().join("games");
        fs::create_dir_all(&directory).expect("the subjects' directory can be created");
        let copy = directory.join(subject.executable);
        fs::copy(pattern_binary(), &copy).unwrap_or_else(|error| {
            panic!(
                "the pattern application could not be copied to {}: {error}",
                copy.display()
            )
        });
        copy
    }

    fn clips(&self) -> PathBuf {
        self.path().join("clips")
    }

    fn path(&self) -> &Path {
        self.directory
            .as_ref()
            .expect("the workspace outlives everything that reads it")
            .path()
    }

    fn local_app_data(&self) -> PathBuf {
        self.path().join("appdata")
    }

    /// Starts `clipped-recorder watch` against this workspace.
    fn start_recorder(&self) -> RecorderProcess {
        self.start_recorder_asking_for(&[])
    }

    /// The same, with more of `watch`'s own command line.
    ///
    /// What a command line asks for is one of the three layers a recording is
    /// resolved from, so a test about the other two has to be able to set it to
    /// something it can recognise (`docs/configuration.md`).
    fn start_recorder_asking_for(&self, extra: &[&str]) -> RecorderProcess {
        let mut child = Command::new(recorder_binary())
            .args([
                "watch",
                "--output-directory",
                &self.clips().to_string_lossy(),
                "--window-timeout",
                WINDOW_TIMEOUT,
                // Asked for explicitly, so this test asserts what it
                // configured rather than what the machine happened to have: a
                // CI runner with no audio device and one with two produce
                // different files otherwise, and the assertion below is exact.
                "--microphone",
                "none",
                "--system-audio",
                "none",
            ])
            .args(extra)
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

    /// Every session record the run produced, oldest first.
    fn sessions(&self) -> Vec<Value> {
        let mut sidecars: Vec<PathBuf> = fs::read_dir(self.clips())
            .expect("the clips directory can be listed")
            .map(|entry| entry.expect("the clips directory can be read").path())
            .filter(|path| {
                path.to_string_lossy()
                    .to_lowercase()
                    .ends_with(".session.json")
            })
            .collect();
        // Named after the moment the sitting started, so this is chronological.
        sidecars.sort();

        sidecars
            .iter()
            .map(|path| {
                let text = fs::read_to_string(path).expect("the session record can be read");
                serde_json::from_str(&text).unwrap_or_else(|error| {
                    panic!(
                        "the session record is not JSON: {error}
{text}"
                    )
                })
            })
            .collect()
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

impl Drop for Workspace {
    /// Removes the run's recordings, unless the test failed.
    ///
    /// `TemporaryDirectory` removes itself unconditionally, and [`Drop`] runs
    /// while a panicking thread unwinds — so a failed assertion here would take
    /// the recordings, the session record and the recorder's overlay with it,
    /// which are the only evidence of what the run saw. Asking
    /// [`std::thread::panicking`] separates the two cases, the same way
    /// `crates/library/tests/support`'s `Scratch` does: a passing run's
    /// workspace is worth nothing and goes, a failing run's stays, with the path
    /// printed so whoever reads the failure knows where to look.
    ///
    /// Forgetting the `TemporaryDirectory` is what declines the removal; there
    /// is nothing else in it to run.
    fn drop(&mut self) {
        let Some(directory) = self.directory.take() else {
            return;
        };
        if std::thread::panicking() {
            eprintln!(
                "workspace kept for diagnosis: {}",
                directory.path().display()
            );
            std::mem::forget(directory);
        }
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
        self.wait_for_line(needle);
    }

    /// The same, handing back the line that matched.
    ///
    /// For the one caller that has to read a line rather than merely reach it:
    /// the recorder writes the settings it resolved for a game at the moment it
    /// starts recording it, and that line is the earliest place a per-game
    /// setting that did not arrive can be named. Without it the first thing to
    /// go wrong is a wait that times out, and a timeout says nothing about
    /// which setting was missing.
    fn wait_for_line(&mut self, needle: &str) -> String {
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
                        return line;
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
    window: usize,
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
        let (window, client) = read_ready_line(&mut child);
        Self {
            child,
            process_id,
            window,
            client,
        }
    }

    /// Started from a copy of the pattern application, at a size of its own.
    ///
    /// The two subjects of the per-game settings test: the same renderer under
    /// two image names, so that the catalogue has two games to tell apart and
    /// each opens a window the size its own settings entry names.
    fn as_subject(binary: &Path, subject: Subject, fps: u32, seconds: u64) -> Self {
        let mut command = Command::new(binary);
        command.args(pattern_arguments(fps, seconds)).args([
            "--size".to_owned(),
            format!("{}x{}", subject.size.0, subject.size.1),
        ]);
        let mut child = spawn(command, binary);
        let process_id = child.id();
        let (window, client) = read_ready_line(&mut child);
        assert_eq!(
            client, subject.size,
            "{} did not open the window its settings entry is written for, so nothing below              would be about that entry",
            subject.name
        );
        Self {
            child,
            process_id,
            window,
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
        let (window, client) = read_ready_line(&mut child);
        Self {
            child,
            process_id,
            window,
            client,
        }
    }

    fn process_id(&self) -> u32 {
        self.process_id
    }

    fn client_size(&self) -> (u32, u32) {
        self.client
    }

    /// The window handle the subject announced.
    ///
    /// Taken from the application's own `ready` line rather than searched for on
    /// the desktop: the resize test has to act on the window the recorder is
    /// recording, and a search by title would find whichever one it found.
    fn window(&self) -> usize {
        self.window
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

/// The `hwnd=0x…` and `client=WIDTHxHEIGHT` the pattern announces before it
/// renders.
///
/// The parsing is [`ready_line`]'s, which `support::PatternApp` uses for the
/// same line: what is here is only the reading of it, because this file starts
/// the subject itself rather than through that type.
fn read_ready_line(child: &mut Child) -> (usize, (u32, u32)) {
    let stdout = child.stdout.take().expect("stdout was piped");
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .expect("the pattern application announces itself before rendering");

    ready_line(&line)
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

/// Every recording of the session that produced a file, in the order the
/// session made them.
///
/// The sibling of [`playable_recording`] for a sitting that is *supposed* to
/// hold more than one. The same rule about the others applies: a recording that
/// found no window is an ordinary tail on a session whose game has gone, and a
/// recording that **failed** is not — a failure here would mean the successor
/// of the resize never opened, which is the shape issue #561 had.
fn recorded_files<'a>(session: &'a Value, diagnostics: &str) -> Vec<&'a Value> {
    let recordings = session["recordings"]
        .as_array()
        .unwrap_or_else(|| panic!("the session record has no recordings:\n{session:#}"));

    for recording in recordings {
        let outcome = recording["outcome"].as_str().unwrap_or("none");
        assert!(
            matches!(outcome, "recorded" | "no-window"),
            "a recording of this session failed rather than simply finding nothing to \
             record: {recording:#}\n{diagnostics}"
        );
    }

    recordings
        .iter()
        .filter(|recording| recording["outcome"] == "recorded")
        .collect()
}

/// Asserts a file holds at least this many seconds of pictures.
///
/// A recording finalised without its last cluster, or one that opened and was
/// closed again, satisfies every assertion about dimensions and end reasons and
/// holds a fraction of a second.
fn assert_at_least_seconds(recording: &Value, floor: f64, what: &str, diagnostics: &str) {
    let seconds = recording["duration_seconds"]
        .as_f64()
        .unwrap_or_else(|| panic!("{what} has no duration:\n{recording:#}\n{diagnostics}"));
    assert!(
        seconds >= floor,
        "{what} holds {seconds:.2}s, which is below the floor of {floor:.2}s; a fraction of a \
         second is what a file finalised with nothing in it looks like:\n{diagnostics}"
    );
}

/// The gap between where one file's pictures stop and the next one's start, on
/// the session's own timeline.
///
/// `starts_at_nanos` is where a recording begins on that timeline and
/// `duration_seconds` is how much of it the file covers, which is the pair the
/// library already uses to put a moment on the right second of the right
/// recording ([issue #71](https://github.com/wildware-uk/clipped/issues/71)).
/// Their difference is therefore the seam, measured out of the record the
/// recorder wrote rather than off a clock in this process.
///
/// A negative gap — the second file starting before the first one's last picture
/// — is reported as zero rather than as an error: it would mean the two spans
/// overlap, which is not what this test is guarding against and is not a thing
/// the pipeline can currently produce.
fn seam_between(first: &Value, second: &Value, diagnostics: &str) -> Duration {
    let nanos = |recording: &Value| {
        recording["starts_at_nanos"].as_i64().unwrap_or_else(|| {
            panic!(
                "a recorded file has no place on the session's timeline, so the seam cannot be \
                 measured:\n{recording:#}\n{diagnostics}"
            )
        })
    };
    let seconds = |recording: &Value| {
        recording["duration_seconds"].as_f64().unwrap_or_else(|| {
            panic!("a recorded file has no duration:\n{recording:#}\n{diagnostics}")
        })
    };

    let ends = nanos(first) as f64 + seconds(first) * 1e9;
    Duration::from_secs_f64(((nanos(second) as f64 - ends) / 1e9).max(0.0))
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
        // One video stream and nothing else, because this recording asked for
        // no audio. "At least one video stream" would not notice a track
        // appearing half-wired, which is the failure worth catching here.
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
