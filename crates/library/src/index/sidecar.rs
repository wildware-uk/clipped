//! Reading the session sidecars the recorder writes.
//!
//! The recorder writes one JSON file per session, beside the recordings and
//! named after it — `clipped-<session-id>.session.json` — and rewrites it
//! whenever the session changes (`crates/session/src/automatic/sidecar.rs`,
//! `docs/sessions.md`). That file is **authoritative** for everything about a
//! session that is not an observation of the filesystem: which game it was, when
//! it started and ended, which files it produced and what happened during it.
//!
//! This module is the reader for it, and nothing more: it turns bytes into the
//! shapes in `docs/sessions.md` and makes no decision about what to do with
//! them. What is written into the database is [`super::ingest`]'s business.
//!
//! # Why the shapes are declared again here rather than shared
//!
//! `clipped-session` sits four layers above this crate
//! (`tests/integration/tests/workspace_layering.rs`), so the writer's types are
//! not reachable from here and could not be made so without inverting the
//! dependency. That is the ordinary situation for a file format with a producer
//! and a consumer in different processes, and the answer is the same as for any
//! other: the *file* is the contract, `docs/sessions.md` states it, and both
//! ends are tested against it. `crates/library/tests/sidecars.rs` reads the
//! example printed in that document and a sidecar produced by the real writer,
//! so a change to the format that nobody propagated fails a test here.
//!
//! # Forward compatibility
//!
//! A file carrying a `schema_version` this build does not know is **refused,
//! not half-read**. Deserialisation is otherwise tolerant: unknown fields are
//! ignored rather than fatal within a version this build understands, and the
//! fields an event carries beyond `at` and `event` are kept verbatim, so an
//! event this build has never heard of still reaches the database with
//! everything the recorder wrote about it.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Map, Value};

/// The newest sidecar schema version this build understands.
///
/// `clipped_session::automatic::sidecar::SCHEMA_VERSION` is the writer's copy of
/// the same number. It is duplicated rather than shared for the reason the
/// module documentation gives, and
/// `clipped_session::automatic::sidecar`'s
/// `the_reader_this_build_ships_understands_what_this_build_writes` is what
/// stops the two drifting quietly. That test is why this is `pub`: the writer's
/// crate is the one that can see both numbers.
///
/// Version 2 added `game_events` ([issue
/// #71](https://github.com/wildware-uk/clipped/issues/71)). A version 1 file is
/// still read, and simply has none.
pub const SUPPORTED_SCHEMA_VERSION: u32 = 2;

/// One session, as its sidecar describes it.
#[derive(Debug, Deserialize)]
pub(crate) struct SessionSidecar {
    // `schema_version` is deliberately not a field here. It is read first, on
    // its own, by [`parse`], because a file from a newer build has to be
    // refused before the rest of it is interpreted — and a second copy of it in
    // this struct would be a field nothing reads.
    pub(crate) session_id: String,
    pub(crate) game: SidecarGame,
    pub(crate) started_at: String,
    #[serde(default)]
    pub(crate) ended_at: Option<String>,
    #[serde(default)]
    pub(crate) recordings: Vec<SidecarRecording>,
    /// The clips saved out of this session's recordings.
    ///
    /// Defaulted rather than required, because a sidecar written before
    /// [issue #38](https://github.com/wildware-uk/clipped/issues/38) has the
    /// key but a hand-written one may not, and a session is worth more than the
    /// list.
    #[serde(default)]
    pub(crate) clips: Vec<SidecarClip>,
    #[serde(default)]
    pub(crate) events: Vec<SidecarEvent>,
    /// What plugins reported, each as the whole document the recorder wrote.
    ///
    /// Held as raw JSON rather than parsed into this struct's own shape,
    /// because `clipped_events::schema::read_value` is what interprets one and
    /// it keeps the fields this build has no name for. Parsing them into
    /// borrowed fields here would drop exactly those.
    ///
    /// Defaulted: a version 1 sidecar has no such key, and every sidecar
    /// written before something produces game events has an empty one.
    #[serde(default)]
    pub(crate) game_events: Vec<Value>,
}

/// Which game the session was of, and how sure the catalogue was.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum SidecarGame {
    /// The catalogue matched exactly one entry.
    Known { game_id: String, name: String },
    /// The catalogue found several equally good answers and refused to choose,
    /// so the session was recorded and left unattributed (`docs/sessions.md`).
    Ambiguous {
        #[serde(default)]
        candidates: Vec<String>,
    },
    /// Nothing asked the catalogue. The session is one somebody started by
    /// pointing at a window, so it is unattributed with nothing to attribute it
    /// from (`clipped_session::automatic::ManualSession`).
    Unidentified,
    /// A kind this build has never heard of.
    ///
    /// Indexed as unattributed and reported, rather than refusing the session:
    /// the recording, when it started and which files it produced are all still
    /// legible, and losing the whole sitting over one word would be losing far
    /// more than could not be read. This variant is also what makes
    /// `game.kind` an open vocabulary that a new writer can add to without a
    /// schema version — see `crates/session/src/automatic/sidecar.rs`.
    #[serde(other)]
    Unrecognised,
}

/// One media file the session produced.
#[derive(Debug, Deserialize)]
pub(crate) struct SidecarRecording {
    /// The recording's ordinal within its session, counting from one.
    pub(crate) index: u32,
    /// The file, as the recorder named it.
    pub(crate) output: String,
    pub(crate) started_at: String,
    #[serde(default)]
    pub(crate) ended_at: Option<String>,
    #[serde(default)]
    pub(crate) outcome: Option<String>,
    #[serde(default)]
    pub(crate) end_reason: Option<String>,
    #[serde(default)]
    pub(crate) duration_seconds: Option<f64>,
    /// Where this file starts on the session's timeline, in nanoseconds.
    ///
    /// With `duration_seconds` it is the span the file covers, which is what
    /// places a game event in one recording rather than merely on a session
    /// ([issue #71](https://github.com/wildware-uk/clipped/issues/71)).
    /// Defaulted: a sidecar written before the key existed has none, and a
    /// recording that produced no frame never had one.
    #[serde(default)]
    pub(crate) starts_at_nanos: Option<i64>,
    #[serde(default)]
    pub(crate) frames_encoded: Option<u64>,
    #[serde(default)]
    pub(crate) width: Option<u32>,
    #[serde(default)]
    pub(crate) height: Option<u32>,
}

/// One shorter file the session produced: today, a save from a recording's
/// replay buffer.
///
/// `source_start_seconds` and `source_end_seconds` are offsets into the
/// recording `source_recording` names, on that recording's own timeline, which
/// is what the `clips` table stores and what survives the files being moved
/// (`crates/session/src/automatic/sidecar.rs`, `docs/sessions.md`).
///
/// Everything but the path is optional here, and deliberately. A clip whose
/// provenance a hand-edited file left out is still a clip the user has, and
/// filing it with the columns that *are* legible beats refusing to index a file
/// that exists (AGENTS.md section 16).
#[derive(Debug, Deserialize)]
pub(crate) struct SidecarClip {
    /// The file, as the recorder named it.
    pub(crate) path: String,
    #[serde(default)]
    pub(crate) created_at: Option<String>,
    /// Which recording of the session it was cut from.
    #[serde(default)]
    pub(crate) source_recording: Option<u32>,
    #[serde(default)]
    pub(crate) source_start_seconds: Option<f64>,
    #[serde(default)]
    pub(crate) source_end_seconds: Option<f64>,
    #[serde(default)]
    pub(crate) duration_seconds: Option<f64>,
    /// What the user called it, when anything did. Nothing names a replay clip
    /// today; the column exists for the clips M11 creates.
    #[serde(default)]
    pub(crate) title: Option<String>,
}

/// One thing that happened during the session.
///
/// `at` and `event` are the two fields every event has. The rest differ per kind
/// — a process identifier for `session-started`, an output path for
/// `recording-started`, a gap in seconds for `system-resumed` — and are captured
/// as they were written rather than as named fields, because that is what lets
/// an event kind added by a later recorder survive a round trip through a build
/// that predates it.
#[derive(Debug, Deserialize)]
pub(crate) struct SidecarEvent {
    pub(crate) at: String,
    pub(crate) event: String,
    /// Everything else the event carried, in the order a JSON object sorts.
    ///
    /// `BTreeMap` rather than `serde_json::Map` so that the text written into
    /// `session_events.detail` is the same for the same event however the file
    /// happened to order its keys — a row that changes on every re-index for no
    /// reason is a diff nobody can read.
    #[serde(flatten)]
    pub(crate) detail: BTreeMap<String, Value>,
}

impl SidecarEvent {
    /// The event's remaining fields as the JSON object to store, or `None` when
    /// it carried none.
    pub(crate) fn detail_json(&self) -> Option<String> {
        if self.detail.is_empty() {
            return None;
        }
        let object: Map<String, Value> = self
            .detail
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        serde_json::to_string(&Value::Object(object)).ok()
    }
}

/// Why a sidecar could not be turned into rows.
///
/// Every variant is per-file: one unreadable sidecar is reported and skipped,
/// and the rest of the library still indexes (AGENTS.md section 16).
#[derive(Debug)]
pub(crate) enum SidecarError {
    /// The file could not be read at all.
    Unreadable(io::Error),
    /// The file is not the JSON this build expects.
    Malformed(serde_json::Error),
    /// The file announces a schema this build does not know.
    UnsupportedSchema {
        /// The version the file carries.
        found: u32,
    },
    /// The file is JSON of the right shape but says nothing this crate can key
    /// a row on.
    Incomplete {
        /// What is wrong, for a log a person has to act on.
        detail: &'static str,
    },
}

/// Reads and validates the sidecar at `path`.
pub(crate) fn read(path: &Path) -> Result<SessionSidecar, SidecarError> {
    let text = fs::read_to_string(path).map_err(SidecarError::Unreadable)?;
    parse(&text)
}

/// The same, from text that has already been read.
pub(crate) fn parse(text: &str) -> Result<SessionSidecar, SidecarError> {
    // The version is read before the rest of the file is interpreted, so that a
    // file from a newer build is refused on its own terms rather than failing
    // as a shape mismatch — "update Clipped" and "this file is corrupt" are
    // different things to tell somebody (AGENTS.md section 15).
    #[derive(Deserialize)]
    struct Versioned {
        schema_version: u32,
    }

    let versioned: Versioned = serde_json::from_str(text).map_err(SidecarError::Malformed)?;
    if versioned.schema_version > SUPPORTED_SCHEMA_VERSION {
        return Err(SidecarError::UnsupportedSchema {
            found: versioned.schema_version,
        });
    }

    let sidecar: SessionSidecar = serde_json::from_str(text).map_err(SidecarError::Malformed)?;
    if sidecar.session_id.is_empty() {
        return Err(SidecarError::Incomplete {
            detail: "the session has no identifier",
        });
    }
    if sidecar.started_at.is_empty() {
        return Err(SidecarError::Incomplete {
            detail: "the session has no start time",
        });
    }
    Ok(sidecar)
}

/// The suffix a session sidecar's file name ends in.
pub(crate) const SIDECAR_SUFFIX: &str = ".session.json";

/// Whether `path` names a session sidecar.
pub(crate) fn is_sidecar(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(SIDECAR_SUFFIX))
}

/// The absolute form of a recording's `output`, resolved against the directory
/// its sidecar was found in.
///
/// The recorder writes absolute paths, and this is what happens when a user
/// moves the whole folder to another drive: the paths in the file point at
/// where it used to be, and the directory the sidecar was found in is the only
/// evidence of where it is now. A relative `output` — which no build writes and
/// a hand-edited file might — is resolved rather than refused.
pub(crate) fn recording_path(directory: &Path, output: &str) -> PathBuf {
    let written = Path::new(output);
    if written.is_absolute() {
        written.to_path_buf()
    } else {
        directory.join(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"{
        "schema_version": 1,
        "session_id": "counter-strike-2-20260811-143205",
        "game": { "kind": "known", "game_id": "counter-strike-2", "name": "Counter-Strike 2" },
        "started_at": "2026-08-11T14:32:05+01:00",
        "ended_at": null,
        "recordings": [],
        "clips": [],
        "bookmarks": [],
        "events": []
    }"#;

    #[test]
    fn a_sidecar_from_a_newer_build_is_refused_rather_than_half_read() {
        // One past whatever this build supports, so that the test keeps
        // testing a refusal rather than becoming a test of the current version
        // the next time the schema grows a field.
        let newer = SUPPORTED_SCHEMA_VERSION + 1;
        let text = MINIMAL.replace(
            "\"schema_version\": 1",
            &format!("\"schema_version\": {newer}"),
        );

        match parse(&text) {
            Err(SidecarError::UnsupportedSchema { found }) => assert_eq!(found, newer),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn fields_this_build_has_never_heard_of_do_not_stop_it_reading_the_ones_it_knows() {
        // Within a version this build understands, an unknown field is a field
        // added by a writer that still claims compatibility. Refusing the file
        // would lose a whole session over a field nobody needed.
        // `clips` used to be this test's second example, because it was a key
        // the recorder wrote and this reader ignored. It is read now
        // ([issue #38](https://github.com/wildware-uk/clipped/issues/38)), so a
        // key that is still nobody's is used instead — and `bookmarks`, which
        // the recorder writes and this build has no column for, stands in for
        // the case that mattered: a field the *writer* has and the reader does
        // not.
        let text = MINIMAL.replace("\"events\": []", "\"events\": [], \"weather\": \"raining\"");

        let sidecar = parse(&text).expect("an unknown field is not fatal");

        assert_eq!(sidecar.session_id, "counter-strike-2-20260811-143205");
        assert!(
            sidecar.clips.is_empty(),
            "a session that saved no clips reads as one that saved none"
        );
    }

    #[test]
    fn an_events_unknown_fields_are_kept_rather_than_dropped() {
        // The detail column exists so that an event kind from a later recorder
        // still arrives with everything it carried. A reader that named the
        // fields it knew would silently discard the rest.
        let text = MINIMAL.replace(
            "\"events\": []",
            r#""events": [
                { "at": "2026-08-11T14:32:05+01:00", "event": "match-started",
                  "map": "de_dust2", "round": 1 }
            ]"#,
        );

        let sidecar = parse(&text).expect("the sidecar parses");
        let event = &sidecar.events[0];

        assert_eq!(event.event, "match-started");
        let detail: Value = serde_json::from_str(
            &event
                .detail_json()
                .expect("an event with fields has a detail"),
        )
        .expect("the detail is JSON");
        assert_eq!(detail["map"], Value::from("de_dust2"));
        assert_eq!(detail["round"], Value::from(1));
        assert!(
            detail.get("at").is_none() && detail.get("event").is_none(),
            "the two named fields should not be repeated in the detail: {detail}"
        );
    }

    #[test]
    fn an_event_with_nothing_but_a_time_and_a_name_has_no_detail() {
        let text = MINIMAL.replace(
            "\"events\": []",
            r#""events": [{ "at": "2026-08-11T14:32:05+01:00", "event": "session-ended" }]"#,
        );

        let sidecar = parse(&text).expect("the sidecar parses");

        assert_eq!(sidecar.events[0].detail_json(), None);
    }

    #[test]
    fn a_detail_is_written_in_a_stable_order_however_the_file_ordered_it() {
        // Two files saying the same thing must produce the same row, or every
        // re-index rewrites rows that did not change.
        let one = MINIMAL.replace(
            "\"events\": []",
            r#""events": [{ "at": "t", "event": "e", "pid": 1, "image_name": "a.exe" }]"#,
        );
        let other = MINIMAL.replace(
            "\"events\": []",
            r#""events": [{ "at": "t", "event": "e", "image_name": "a.exe", "pid": 1 }]"#,
        );

        assert_eq!(
            parse(&one).expect("parses").events[0].detail_json(),
            parse(&other).expect("parses").events[0].detail_json()
        );
    }

    #[test]
    fn an_ambiguous_session_reads_as_its_candidates_and_not_as_a_choice() {
        let text = MINIMAL.replace(
            r#""game": { "kind": "known", "game_id": "counter-strike-2", "name": "Counter-Strike 2" }"#,
            r#""game": { "kind": "ambiguous", "candidates": ["half-life-2", "team-fortress-2"] }"#,
        );

        match parse(&text).expect("the sidecar parses").game {
            SidecarGame::Ambiguous { candidates } => {
                assert_eq!(candidates, ["half-life-2", "team-fortress-2"]);
            }
            other => panic!("expected an ambiguous game, got {other:?}"),
        }
    }

    #[test]
    fn a_truncated_file_is_reported_as_malformed_rather_than_panicking() {
        match parse(&MINIMAL[..MINIMAL.len() / 2]) {
            Err(SidecarError::Malformed(_)) => {}
            other => panic!("expected a parse failure, got {other:?}"),
        }
    }

    #[test]
    fn a_session_with_no_identifier_is_refused() {
        let text = MINIMAL.replace("counter-strike-2-20260811-143205", "");

        match parse(&text) {
            Err(SidecarError::Incomplete { .. }) => {}
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_recordings_output_is_resolved_against_the_directory_the_sidecar_was_found_in() {
        let directory = Path::new(r"E:\moved\clips");

        assert_eq!(
            recording_path(directory, r"D:\clips\clipped-a.mkv"),
            PathBuf::from(r"D:\clips\clipped-a.mkv"),
            "an absolute path is what the recorder wrote and is taken as it stands"
        );
        assert_eq!(
            recording_path(directory, "clipped-a.mkv"),
            directory.join("clipped-a.mkv")
        );
    }

    #[test]
    fn only_files_named_as_sidecars_are_read_as_sidecars() {
        assert!(is_sidecar(Path::new(r"D:\clips\clipped-a.session.json")));
        assert!(!is_sidecar(Path::new(r"D:\clips\clipped-a.mkv")));
        assert!(
            !is_sidecar(Path::new(r"D:\clips\clipped-a.session.json.tmp")),
            "the half-written file the recorder renames from is not a sidecar"
        );
    }
}
