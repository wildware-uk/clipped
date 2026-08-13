//! The session's form on disk.
//!
//! One JSON file per session, written beside its recordings and named after it:
//! `clipped-<session-id>.session.json`. It is a documented sidecar rather than
//! a database, which is what AGENTS.md section 32 allows for
//! application-specific metadata and what section 55 requires given that
//! **M6's [issue #55](https://github.com/wildware-uk/clipped/issues/55) owns
//! the real store**. When the SQLite library index arrives it becomes the index
//! of these files; nothing here is a second attempt at it.
//!
//! # Why a file at all, before there is a database
//!
//! Because the answer to "which game was this, and which of these three files
//! belong together" exists only in the memory of a process that is expected to
//! be killed. AGENTS.md section 17 says not to keep irreplaceable state only in
//! memory, and this is the cheapest way not to: the file is rewritten whenever
//! the session changes, and a session that has produced two recordings has that
//! written down before the third starts.
//!
//! # The schema
//!
//! ```json
//! {
//!   "schema_version": 1,
//!   "session_id": "counter-strike-2-20260811-143205",
//!   "game": { "kind": "known", "game_id": "counter-strike-2", "name": "Counter-Strike 2" },
//!   "started_at": "2026-08-11T14:32:05+01:00",
//!   "ended_at": null,
//!   "recordings": [ { …, "settings": { … } } ],
//!   "clips": [],
//!   "bookmarks": [],
//!   "events": [ … ]
//! }
//! ```
//!
//! Each recording carries the settings it was made with, and where each of them
//! came from:
//!
//! ```json
//! "settings": {
//!   "resolution": { "value": "2560x1440", "source": "game" },
//!   "framerate":  { "value": "60",        "source": "global" }
//! }
//! ```
//!
//! It is per recording rather than per session because that is where the answer
//! can differ — a session that spans a settings change holds one recording made
//! at the old settings and one at the new — and it is kept at all because
//! "why is this game's file 1440p when the global settings say 1080p" is a
//! question a log that has rotated away can no longer answer
//! ([issue #61](https://github.com/wildware-uk/clipped/issues/61)).
//!
//! The key was added after the schema shipped, and the version is deliberately
//! unchanged: a reader of version 1 that does not know the key ignores it, and
//! every other field means exactly what it did. `docs/sessions.md` says the
//! same, and says that a file written by an older build has no `settings` on
//! its recordings — which is not the same as a recording made at the defaults.
//!
//! # `game.kind` is an open vocabulary
//!
//! `known`, `ambiguous` and — since a recording could be started over the
//! protocol ([issue #402](https://github.com/wildware-uk/clipped/issues/402)) —
//! `unidentified`. The version is unchanged for that addition as well, and that
//! is a promise the *reader* keeps rather than a hope: `clipped-library`'s
//! reader files a `kind` it has never met as unattributed and says so, instead
//! of refusing the session (`crates/library/src/index/sidecar.rs`). A session
//! is worth more than the one field nobody could interpret, so adding a kind is
//! an addition to the file rather than a change to its shape.
//!
//! `clips` and `bookmarks` are reserved and are **always empty in this build**.
//! Nothing here can create either, and for two different reasons now. A clip
//! needs a recording running a replay buffer to save from, which is
//! [issue #38](https://github.com/wildware-uk/clipped/issues/38). Bookmarks
//! *exist* ([issue #64](https://github.com/wildware-uk/clipped/issues/64)) and
//! are not kept here: a bookmark is an offset into one recording rather than a
//! moment in a session, so it lives in that recording's own sidecar beside it
//! (`crate::bookmarks`, `docs/bookmarks.md`) — which is also the shape
//! `clipped-storage`'s `bookmarks` table has. What no build has is a way to
//! *take* one during an automatic session: `watch` serves no protocol, so
//! nothing can reach it with an `add_bookmark`, and joining the two is
//! [issue #232](https://github.com/wildware-uk/clipped/issues/232).
//!
//! Both keys are named here so that filling them later is an addition to the
//! file rather than a change to its shape (AGENTS.md section 43). A reader must
//! not infer from their presence that a session has none — for bookmarks, the
//! answer is in the recordings' own files; `docs/sessions.md` says so in the
//! same words.
//!
//! # Writing it safely
//!
//! To a temporary file, then renamed over the real one. A sidecar is rewritten
//! on every change, and a recorder killed halfway through a write would
//! otherwise leave a truncated file where the session's own record used to be.
//! `std::fs::rename` replaces the destination on Windows, which is the whole
//! reason this is two steps rather than one.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::ser::SerializeSeq;
use serde::{Serialize, Serializer};

use super::clock;
use super::session::{
    GameIdentity, RecordingOutcomeSummary, Session, SessionEvent, SessionEventKind,
};
use crate::config::{ResolvedSettings, SettingKey};

/// The version of the sidecar schema.
///
/// The file's, not Clipped's: it changes when the shape changes and at no other
/// time. It exists so that whatever reads these files — M6's library index
/// first — can tell a file it understands from one it does not, rather than
/// half-understanding it.
pub const SCHEMA_VERSION: u32 = 1;

/// Writes `session`'s sidecar into `directory`, replacing any previous one.
///
/// # Errors
///
/// Whatever the filesystem said. A caller must not fail a recording over it:
/// AGENTS.md section 17 puts the video above the metadata, and the manager logs
/// this and carries on.
pub(crate) fn write(directory: &Path, session: &Session) -> io::Result<PathBuf> {
    fs::create_dir_all(directory)?;

    let path = session.sidecar_path(directory);
    let temporary = path.with_extension("tmp");
    let json = serde_json::to_vec_pretty(&SidecarFile::of(session))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

    fs::write(&temporary, &json)?;
    fs::rename(&temporary, &path)?;
    Ok(path)
}

/// The whole file.
///
/// A shape of its own rather than `Serialize` on [`Session`], so that the file
/// format is visible in one place and is not something a change to a public
/// type alters by accident (AGENTS.md section 43).
#[derive(Debug, Serialize)]
struct SidecarFile<'a> {
    schema_version: u32,
    session_id: &'a str,
    game: SidecarGame<'a>,
    started_at: String,
    ended_at: Option<String>,
    recordings: Vec<SidecarRecording<'a>>,
    /// Always empty; see the module documentation.
    clips: Reserved,
    /// Always empty; see the module documentation.
    bookmarks: Reserved,
    events: Vec<SidecarEvent>,
}

impl<'a> SidecarFile<'a> {
    fn of(session: &'a Session) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            session_id: session.id().as_str(),
            game: SidecarGame::of(session.game()),
            started_at: clock::rfc3339(session.started_at()),
            ended_at: session.ended_at().map(clock::rfc3339),
            recordings: session
                .recordings()
                .iter()
                .map(SidecarRecording::of)
                .collect(),
            clips: Reserved,
            bookmarks: Reserved,
            events: session.events().iter().map(SidecarEvent::of).collect(),
        }
    }
}

/// A list this build always writes empty.
///
/// A type rather than an empty `Vec` of something, because nothing in an
/// automatic session fills either list: no clip can be made at all, and a
/// bookmark belongs to a recording's own sidecar rather than to this file. See
/// the module documentation.
#[derive(Debug)]
struct Reserved;

impl Serialize for Reserved {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_seq(Some(0))?.end()
    }
}

/// Which game, and how sure the catalogue was.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum SidecarGame<'a> {
    Known {
        game_id: &'a str,
        name: &'a str,
    },
    /// The catalogue found several equally good answers and did not choose.
    Ambiguous {
        candidates: &'a [String],
    },
    /// Nothing asked the catalogue, because the recording was of a window
    /// somebody chose (`super::ManualSession`). Written as
    /// `{ "kind": "unidentified" }` and carrying nothing else, because there is
    /// nothing else to say.
    Unidentified,
}

impl<'a> SidecarGame<'a> {
    fn of(game: &'a GameIdentity) -> Self {
        match game {
            GameIdentity::Known { game_id, name } => Self::Known { game_id, name },
            GameIdentity::Ambiguous { candidates } => Self::Ambiguous { candidates },
            GameIdentity::Unidentified => Self::Unidentified,
        }
    }
}

/// One recording of the session.
#[derive(Debug, Serialize)]
struct SidecarRecording<'a> {
    index: u32,
    output: String,
    started_at: String,
    ended_at: Option<String>,
    outcome: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    frames_encoded: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<&'a str>,
    /// What this recording was made with, and which layer each answer came
    /// from. See [`SidecarSetting`].
    settings: BTreeMap<&'static str, SidecarSetting>,
}

/// One effective setting, as a session's record keeps it.
///
/// The value *and* its source, because the two answer different questions. The
/// value is what the recording was made with, which is what somebody comparing
/// two files of the same game wants. The source is why — "this game overrode
/// it" against "it followed the global settings" — which is what makes a
/// surprising recording explicable months later without the log that has since
/// rotated away (`docs/logging.md`).
#[derive(Debug, Serialize)]
struct SidecarSetting {
    value: String,
    source: &'static str,
}

impl<'a> SidecarRecording<'a> {
    fn of(recording: &'a super::session::SessionRecording) -> Self {
        let mut written = Self {
            index: recording.index(),
            output: recording.output().display().to_string(),
            started_at: clock::rfc3339(recording.started_at()),
            ended_at: recording.ended_at().map(clock::rfc3339),
            outcome: recording.outcome().map(RecordingOutcomeSummary::token),
            frames_encoded: None,
            duration_seconds: None,
            width: None,
            height: None,
            end_reason: None,
            detail: None,
            settings: settings_of(recording.settings()),
        };

        match recording.outcome() {
            None => {}
            Some(RecordingOutcomeSummary::Recorded {
                frames_encoded,
                duration,
                size,
                end_reason,
            }) => {
                written.frames_encoded = Some(*frames_encoded);
                written.duration_seconds = Some(duration.as_secs_f64());
                written.width = Some(size.0);
                written.height = Some(size.1);
                written.end_reason = Some(end_reason_token(*end_reason));
            }
            Some(
                RecordingOutcomeSummary::NoWindow { detail }
                | RecordingOutcomeSummary::Failed { detail },
            ) => written.detail = Some(detail),
        }

        written
    }
}

/// Every setting a recording was made with, keyed as the settings file keys
/// them.
///
/// The same names `settings.json` uses, and the same spelling of each value
/// (`crate::config::ResolvedSettings::written_value`), so that a session's
/// record can be read against the file that produced it without translation.
fn settings_of(settings: &ResolvedSettings) -> BTreeMap<&'static str, SidecarSetting> {
    SettingKey::ALL
        .into_iter()
        .map(|key| {
            (
                key.name(),
                SidecarSetting {
                    value: settings.written_value(key),
                    source: settings.source_of(key).token(),
                },
            )
        })
        .collect()
}

/// The token a recording's end reason is written as.
///
/// [`EndReason::token`](crate::report::EndReason::token) is where the words
/// live, so that the sidecar, the IPC protocol and the log all name a reason
/// the same way and a reason added later cannot be given a second spelling
/// here (AGENTS.md section 55).
fn end_reason_token(reason: crate::report::EndReason) -> &'static str {
    reason.token()
}

/// One thing that happened during the session.
///
/// Flat rather than a tagged union of payloads: the fields a session event
/// carries are few and mostly shared, and a reader looking for "when did the
/// game exit" should not have to know which shape that answer arrives in.
/// Absent fields are omitted rather than written as null.
#[derive(Debug, Serialize)]
struct SidecarEvent {
    at: String,
    event: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    index: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    outcome: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    game_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gap_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
}

impl SidecarEvent {
    fn of(event: &SessionEvent) -> Self {
        let mut written = Self {
            at: clock::rfc3339(event.at()),
            event: token(event.kind()),
            pid: None,
            image_name: None,
            index: None,
            output: None,
            outcome: None,
            game_id: None,
            gap_seconds: None,
            limit: None,
            reason: None,
        };

        match event.kind() {
            SessionEventKind::Started { pid, image_name } => {
                written.pid = Some(*pid);
                written.image_name = Some(image_name.clone());
            }
            SessionEventKind::RecordingStarted { index, output } => {
                written.index = Some(*index);
                written.output = Some(output.display().to_string());
            }
            SessionEventKind::RecordingEnded { index, outcome } => {
                written.index = Some(*index);
                written.outcome = Some(outcome.clone());
            }
            SessionEventKind::GameExited { pid } | SessionEventKind::GameRelaunched { pid } => {
                written.pid = Some(*pid);
            }
            SessionEventKind::AnotherGameStarted { game_id, pid } => {
                written.game_id = Some(game_id.clone());
                written.pid = Some(*pid);
            }
            SessionEventKind::SystemResumed { gap } => {
                written.gap_seconds = Some(gap.as_secs());
            }
            SessionEventKind::RecordingLimitReached { limit } => {
                written.limit = Some(*limit);
            }
            SessionEventKind::Ended { reason } => {
                written.reason = Some(reason.token());
            }
        }

        written
    }
}

/// The token an event kind is written as.
fn token(kind: &SessionEventKind) -> &'static str {
    match kind {
        SessionEventKind::Started { .. } => "session-started",
        SessionEventKind::RecordingStarted { .. } => "recording-started",
        SessionEventKind::RecordingEnded { .. } => "recording-ended",
        SessionEventKind::GameExited { .. } => "game-exited",
        SessionEventKind::GameRelaunched { .. } => "game-relaunched",
        SessionEventKind::AnotherGameStarted { .. } => "another-game-started",
        SessionEventKind::SystemResumed { .. } => "system-resumed",
        SessionEventKind::RecordingLimitReached { .. } => "recording-limit-reached",
        SessionEventKind::Ended { .. } => "session-ended",
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use serde_json::Value;

    use super::*;
    use crate::automatic::session::SessionEndReason;
    use crate::config::{Configuration, GameKey, Preferences, ResolutionSetting};
    use crate::report::EndReason;

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    /// Counter-Strike 2 at 1440p, on a machine whose global frame rate is 144.
    ///
    /// Two layers rather than one, so that what is written can be shown to
    /// carry *where* each answer came from and not only what it was.
    fn resolved() -> ResolvedSettings {
        let mut configuration = Configuration::defaults();

        let mut global = Preferences::none();
        global
            .set_framerate(Some(144))
            .expect("144 is an acceptable frame rate");
        configuration.set_global(global);

        let mut game = Preferences::none();
        game.set_resolution(Some(ResolutionSetting::Fixed {
            width: 2560,
            height: 1440,
        }))
        .expect("1440p is an acceptable size");
        configuration.set_game(
            GameKey::parse("counter-strike-2").expect("a valid identifier"),
            game,
        );

        configuration.resolve_for(&GameKey::parse("counter-strike-2").expect("a valid identifier"))
    }

    fn session() -> Session {
        let mut session = Session::new(
            GameIdentity::Known {
                game_id: "counter-strike-2".to_owned(),
                name: "Counter-Strike 2".to_owned(),
            },
            at(1_786_458_725),
        );
        session.record(
            at(1_786_458_725),
            SessionEventKind::Started {
                pid: 4242,
                image_name: "cs2.exe".to_owned(),
            },
        );
        session.begin_recording(
            1,
            PathBuf::from(r"D:\clips\clipped-a.mkv"),
            resolved(),
            at(1_786_458_725),
        );
        session.end_recording(
            1,
            RecordingOutcomeSummary::Recorded {
                frames_encoded: 181,
                duration: Duration::from_secs(6),
                size: (2560, 1440),
                end_reason: EndReason::TargetLost,
            },
            at(1_786_458_731),
        );
        session.end(SessionEndReason::GameExited, at(1_786_458_791));
        session
    }

    fn written() -> Value {
        let json = serde_json::to_string(&SidecarFile::of(&session())).expect("the shape encodes");
        serde_json::from_str(&json).expect("what serde wrote is JSON")
    }

    #[test]
    fn the_file_carries_its_schema_version_and_the_game_it_is_of() {
        let file = written();
        assert_eq!(file["schema_version"], Value::from(SCHEMA_VERSION));
        assert_eq!(file["game"]["kind"], Value::from("known"));
        assert_eq!(file["game"]["game_id"], Value::from("counter-strike-2"));
    }

    #[test]
    fn a_recordings_figures_are_written_beside_the_file_it_produced() {
        let recording = &written()["recordings"][0];
        assert_eq!(recording["index"], Value::from(1));
        assert_eq!(recording["outcome"], Value::from("recorded"));
        assert_eq!(recording["frames_encoded"], Value::from(181));
        assert_eq!(recording["end_reason"], Value::from("target-lost"));
        assert_eq!(recording["width"], Value::from(2560));
        assert!(
            recording["output"]
                .as_str()
                .expect("the output is a string")
                .ends_with("clipped-a.mkv"),
            "{recording}"
        );
    }

    #[test]
    fn a_recording_carries_the_settings_it_was_made_with_and_where_each_came_from() {
        // The question this answers months later is "why is this game's file
        // 1440p when my settings say source?" — and the answer is only useful
        // if the *source* of each value is there too, because "the game
        // overrode it" and "it followed the global settings" are different
        // things to go and change.
        let settings = &written()["recordings"][0]["settings"];

        assert_eq!(settings["resolution"]["value"], Value::from("2560x1440"));
        assert_eq!(settings["resolution"]["source"], Value::from("game"));
        assert_eq!(settings["framerate"]["value"], Value::from("144"));
        assert_eq!(settings["framerate"]["source"], Value::from("global"));
        assert_eq!(settings["codec"]["value"], Value::from("auto"));
        assert_eq!(settings["codec"]["source"], Value::from("default"));

        // Every setting, not the interesting ones: a recording whose record
        // says nothing about the encoder it was configured for is one nobody
        // can explain afterwards.
        for key in SettingKey::ALL {
            assert!(
                settings[key.name()]["value"].is_string(),
                "{} is missing from a recording's settings: {settings}",
                key.name()
            );
        }
    }

    #[test]
    fn clips_and_bookmarks_are_written_as_empty_lists_in_a_session_file() {
        // The point of the assertion is not that they are empty today but that
        // they are *reserved*: a reader must be able to tell "no clips" from "a
        // file that predates clips", and a later milestone must not have to
        // change the shape to add one. Bookmarks are empty here because they
        // live beside the recording they are in rather than in the session —
        // see `crate::bookmarks` and docs/bookmarks.md.
        let file = written();
        assert_eq!(file["clips"], Value::Array(vec![]));
        assert_eq!(file["bookmarks"], Value::Array(vec![]));
    }

    #[test]
    fn an_ambiguous_session_writes_every_candidate_rather_than_a_choice() {
        let session = Session::new(
            GameIdentity::Ambiguous {
                candidates: vec!["first-game".to_owned(), "second-game".to_owned()],
            },
            at(1_786_458_725),
        );
        let json = serde_json::to_string(&SidecarFile::of(&session)).expect("the shape encodes");
        let file: Value = serde_json::from_str(&json).expect("what serde wrote is JSON");

        assert_eq!(file["game"]["kind"], Value::from("ambiguous"));
        assert_eq!(
            file["game"]["candidates"],
            Value::from(vec!["first-game", "second-game"])
        );
        assert!(
            file["game"]["game_id"].is_null(),
            "an ambiguous session must not name one of the candidates as the answer: {file}"
        );
    }

    #[test]
    fn the_events_are_the_sessions_history_in_order() {
        let file = written();
        let events: Vec<&str> = file["events"]
            .as_array()
            .expect("the events are a list")
            .iter()
            .map(|event| event["event"].as_str().expect("each event is named"))
            .collect();

        assert_eq!(
            events,
            [
                "session-started",
                "recording-started",
                "recording-ended",
                "session-ended"
            ]
        );
        assert_eq!(file["events"][3]["reason"], Value::from("game-exited"));
    }

    #[test]
    fn writing_a_sidecar_replaces_the_previous_one_rather_than_appending_to_it() {
        let directory = std::env::temp_dir().join(format!(
            "clipped-sidecar-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&directory);

        let mut session = session();
        let first = write(&directory, &session).expect("the sidecar can be written");
        session.record(
            at(1_786_458_800),
            SessionEventKind::GameExited { pid: 4242 },
        );
        let second = write(&directory, &session).expect("the sidecar can be rewritten");

        assert_eq!(
            first, second,
            "a session has one sidecar, not one per write"
        );
        let text = fs::read_to_string(&second).expect("the sidecar can be read");
        let file: Value = serde_json::from_str(&text).expect("the sidecar is JSON");
        assert_eq!(
            file["events"].as_array().expect("a list").len(),
            5,
            "the rewrite should hold the whole session: {text}"
        );
        let left: Vec<_> = fs::read_dir(&directory)
            .expect("the directory can be listed")
            .map(|entry| entry.expect("an entry").file_name())
            .collect();
        assert_eq!(
            left.len(),
            1,
            "the temporary file should have been renamed, not left behind: {left:?}"
        );

        let _ = fs::remove_dir_all(&directory);
    }
}
