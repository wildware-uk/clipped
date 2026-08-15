//! Finding the recordings a killed recorder left behind, and closing them off.
//!
//! A recorder that is killed — the process ended, the machine lost power, the
//! user pressed reset — leaves two things: a Matroska file that plays as far as
//! the last cluster it closed ([ADR 0001], `docs/muxing.md`), and a session
//! sidecar whose entry for that recording says it began and never says it
//! ended. The file is not lost; nothing knows it is finished.
//!
//! This module is the "nothing knows" half. It reads the sidecars in a
//! recordings directory, finds the entries with no end, and lets the caller
//! close them — which is what stops the same recording being offered on every
//! launch and what lets M6's library index treat it as a finished recording
//! rather than one that is still being written
//! ([issue #56](https://github.com/wildware-uk/clipped/issues/56)).
//!
//! [ADR 0001]: ../../../../docs/adr/0001-mkv-archival-container.md
//!
//! # What "recovery" does and does not mean here
//!
//! It does **not** rewrite the file. A recording without a trailer has no
//! segment length, no duration and no cue index, so it plays from the start and
//! is seekable only by scanning; putting the index back means rewriting the
//! container, which `clipped-muxer` cannot do yet
//! ([issue #283](https://github.com/wildware-uk/clipped/issues/283)). What this
//! does is make the footage *known*: named, sized, attributed to its game and
//! its session, and marked as finished so it is indexed like any other
//! recording.
//!
//! # The one thing it cannot tell apart
//!
//! A recording that is running right now looks exactly like a recording that
//! was interrupted: both have an entry with no end. There is no lock file and
//! no process identifier to check against, and inventing one would be a second
//! source of truth about what is running. So this is documented as a startup
//! question — `clipped-recorder recover` is meant to be asked before a session
//! begins — and nothing here ever acts on a recording without being told to,
//! by name.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::{Map, Value};

use super::clock;

/// The suffix every session sidecar's name ends with.
const SIDECAR_SUFFIX: &str = ".session.json";

/// The outcome an adopted recording is written with.
///
/// A word of its own rather than `failed`: nothing failed. The recording was
/// running and the recorder stopped existing, and a library that filed it under
/// "failed" would tell somebody their footage was no good (`docs/sessions.md`,
/// [issue #278](https://github.com/wildware-uk/clipped/issues/278)).
pub const INTERRUPTED: &str = "interrupted";

/// The outcome a discarded recording is written with.
///
/// The entry stays. AGENTS.md section 56 is about not destroying what a user
/// has, and the *record* that a recording existed and was deliberately thrown
/// away is worth more than a gap: it is the difference between "this session
/// produced one file" and "this session produced two and you deleted one".
pub const DISCARDED: &str = "discarded";

/// A recording that began and was never recorded as having ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterruptedRecording {
    session_id: String,
    game: String,
    index: u32,
    output: PathBuf,
    started_at: String,
    sidecar: PathBuf,
    bytes: Option<u64>,
}

impl InterruptedRecording {
    /// The session it belongs to.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// What to call the game it is of.
    #[must_use]
    pub fn game(&self) -> &str {
        &self.game
    }

    /// Which recording of that session, counting from one.
    #[must_use]
    pub const fn index(&self) -> u32 {
        self.index
    }

    /// The file it was being written to.
    #[must_use]
    pub fn output(&self) -> &Path {
        &self.output
    }

    /// When it started, as the sidecar recorded it.
    #[must_use]
    pub fn started_at(&self) -> &str {
        &self.started_at
    }

    /// The session record it was found in.
    #[must_use]
    pub fn sidecar(&self) -> &Path {
        &self.sidecar
    }

    /// How large the file is, or [`None`] when there is no file.
    ///
    /// [`None`] is a real answer and a different one: the recording was asked
    /// for, the recorder died before the encoder produced a first packet, and
    /// there is nothing to recover. Somebody is better told that than shown a
    /// path to a file that is not there.
    #[must_use]
    pub const fn bytes(&self) -> Option<u64> {
        self.bytes
    }

    /// Whether there is footage behind it.
    #[must_use]
    pub const fn has_footage(&self) -> bool {
        self.bytes.is_some()
    }
}

/// Every recording in `directory` that began and was never recorded as ended.
///
/// Oldest session first, and within a session by index, so that a list printed
/// from this reads in the order the recordings were made.
///
/// A sidecar that cannot be read is skipped and logged rather than failing the
/// scan: one damaged file must not hide every other session's recoverable
/// footage (AGENTS.md section 17).
///
/// # Errors
///
/// Only what stopped the *directory* being listed. A directory that is not
/// there is not an error — it is a machine that has never recorded — and
/// answers with nothing.
pub fn interrupted_recordings(directory: &Path) -> io::Result<Vec<InterruptedRecording>> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };

    let mut found = Vec::new();
    let mut sidecars: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| is_sidecar(path))
        .collect();
    // The session identifier is the game and the moment it started, so the file
    // name sorts chronologically within a game. Sorting the paths is what makes
    // the output stable between runs on a filesystem that does not promise an
    // order (AGENTS.md section 25).
    sidecars.sort();

    for path in sidecars {
        match read_sidecar(&path) {
            Ok(file) => collect_open_recordings(&file, &path, &mut found),
            Err(error) => tracing::warn!(
                sidecar = %clipped_logging::RedactedPath::new(&path),
                %error,
                "a session record could not be read while looking for interrupted recordings; \
                 the other sessions were still checked"
            ),
        }
    }

    Ok(found)
}

/// Records `recording` as having been interrupted, keeping the file.
///
/// The file is not touched. What changes is the session's record: the entry
/// gains an end time and the [`INTERRUPTED`] outcome, and a `recording-ended`
/// event is appended, so the recording is indexed like any other and is not
/// offered again.
///
/// # Errors
///
/// Whatever stopped the sidecar being read or rewritten.
pub fn adopt(recording: &InterruptedRecording, at: SystemTime) -> io::Result<()> {
    close_recording(recording, INTERRUPTED, at)
}

/// Deletes `recording`'s file and records that it was discarded.
///
/// Deliberately not something that can be done to a whole directory at once:
/// this is footage that cannot be made again, so the caller has to have named
/// one recording (AGENTS.md section 56). The sidecar entry stays, with the
/// [`DISCARDED`] outcome, because the record that a file existed and was thrown
/// away is worth more than a gap.
///
/// # Errors
///
/// Whatever stopped the file being deleted or the sidecar being rewritten. A
/// file that is already gone is not an error — the outcome is recorded and the
/// caller is told nothing was deleted.
pub fn discard(recording: &InterruptedRecording, at: SystemTime) -> io::Result<Discarded> {
    let deleted = match fs::remove_file(recording.output()) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error),
    };

    // The record is written after the deletion and not before, so a rewrite
    // that fails leaves a sidecar saying the recording is still open — which
    // offers it again — rather than one saying a file that is still there was
    // discarded.
    close_recording(recording, DISCARDED, at)?;
    Ok(Discarded {
        bytes: recording.bytes.filter(|_| deleted),
    })
}

/// What discarding a recording actually did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Discarded {
    bytes: Option<u64>,
}

impl Discarded {
    /// How many bytes were freed, or [`None`] if there was no file to delete.
    #[must_use]
    pub const fn bytes_freed(&self) -> Option<u64> {
        self.bytes
    }
}

/// Whether a path is a session sidecar.
fn is_sidecar(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("clipped-") && name.ends_with(SIDECAR_SUFFIX))
}

/// A sidecar, as JSON, with nothing thrown away.
///
/// Deliberately a [`Value`] rather than a second `#[derive(Deserialize)]` copy
/// of the schema. Two reasons, and the second is the important one. The schema
/// is written a few lines away in [`super::sidecar`], so a mirror of it here
/// would be the same shape stated twice. And this file is *rewritten*: a typed
/// view would silently drop every field it did not know about, so a sidecar
/// written by a newer Clipped and recovered by an older one would come back
/// with the newer fields gone (AGENTS.md sections 43 and 56).
fn read_sidecar(path: &Path) -> io::Result<Map<String, Value>> {
    let text = fs::read_to_string(path)?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    match value {
        Value::Object(map) => Ok(map),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a session record must be an object, and this one is {other}"),
        )),
    }
}

/// Adds every open recording in one sidecar to `found`.
fn collect_open_recordings(
    file: &Map<String, Value>,
    sidecar: &Path,
    found: &mut Vec<InterruptedRecording>,
) {
    let Some(session_id) = file.get("session_id").and_then(Value::as_str) else {
        return;
    };
    let game = game_name(file);
    let directory = sidecar.parent().unwrap_or_else(|| Path::new(""));

    let recordings = file
        .get("recordings")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();

    for recording in recordings {
        let Some(recording) = recording.as_object() else {
            continue;
        };
        // Either, and not both. `ended_at` is when it stopped and `outcome` is
        // what it turned out to be;
        // `crate::automatic::Session::end_recording` writes them together, so
        // an entry carrying one and not the other cannot come from this build
        // at all. Requiring both to be absent would mean that if one ever did
        // — a half-written record, a build that stopped writing one of them —
        // the footage would simply stop being offered, silently. A recovery
        // tool should read "not fully closed" as "offer it": the worst that
        // costs is a recording listed that did not need to be.
        let open = recording.get("ended_at").is_none_or(Value::is_null)
            || recording.get("outcome").is_none_or(Value::is_null);
        if !open {
            continue;
        }

        let Some(index) = recording.get("index").and_then(Value::as_u64) else {
            continue;
        };
        let Some(output) = recording.get("output").and_then(Value::as_str) else {
            continue;
        };

        // The path as written, but resolved against the directory the sidecar
        // was found in when it is relative — a recordings folder that has been
        // moved to another drive is exactly the case somebody is recovering
        // from, and an absolute path from the old drive would name nothing.
        let output = {
            let written = PathBuf::from(output);
            if written.is_absolute() {
                written
            } else {
                directory.join(written)
            }
        };

        found.push(InterruptedRecording {
            session_id: session_id.to_owned(),
            game: game.clone(),
            index: u32::try_from(index).unwrap_or(u32::MAX),
            bytes: fs::metadata(&output).ok().map(|data| data.len()),
            output,
            started_at: recording
                .get("started_at")
                .and_then(Value::as_str)
                .unwrap_or("an unknown time")
                .to_owned(),
            sidecar: sidecar.to_path_buf(),
        });
    }
}

/// What to call the game a sidecar is of.
fn game_name(file: &Map<String, Value>) -> String {
    let Some(game) = file.get("game").and_then(Value::as_object) else {
        return "an unknown game".to_owned();
    };
    game.get("name")
        .and_then(Value::as_str)
        .or_else(|| game.get("game_id").and_then(Value::as_str))
        .unwrap_or("an unattributed game")
        .to_owned()
}

/// Writes an end time and an outcome onto one recording, and says so in the
/// session's history.
fn close_recording(
    recording: &InterruptedRecording,
    outcome: &str,
    at: SystemTime,
) -> io::Result<()> {
    let mut file = read_sidecar(recording.sidecar())?;
    let stamp = clock::rfc3339(at);

    let entries = file
        .get_mut("recordings")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "the session record has no recordings to close",
            )
        })?;

    let entry = entries
        .iter_mut()
        .filter_map(Value::as_object_mut)
        .find(|entry| {
            entry.get("index").and_then(Value::as_u64) == Some(u64::from(recording.index()))
        })
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "the session record no longer holds recording {} of {}",
                    recording.index(),
                    recording.session_id()
                ),
            )
        })?;

    entry.insert("ended_at".to_owned(), Value::String(stamp.clone()));
    entry.insert("outcome".to_owned(), Value::String(outcome.to_owned()));

    // The event list is the session's own history, and it is what answers "why
    // does this session have two files in it?" afterwards (`super::session`).
    // A recording that ends without one leaves a hole in that history.
    let mut event = Map::new();
    event.insert("at".to_owned(), Value::String(stamp));
    event.insert(
        "event".to_owned(),
        Value::String("recording-ended".to_owned()),
    );
    event.insert("index".to_owned(), Value::from(recording.index()));
    event.insert("outcome".to_owned(), Value::String(outcome.to_owned()));
    file.entry("events")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "the session record's events are not a list",
            )
        })?
        .push(Value::Object(event));

    write_atomically(recording.sidecar(), &Value::Object(file))
}

/// Replaces a sidecar, via a temporary file.
///
/// The same two steps [`super::sidecar::write`] takes, and for the same reason:
/// a recorder killed halfway through a write would otherwise leave a truncated
/// file where the session's own record used to be — which, in a module whose
/// whole job is recovering from a killed recorder, would be an unusually
/// pointed bug.
fn write_atomically(path: &Path, file: &Value) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(file)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    clipped_logging::write_atomically(path, |temporary| io::Write::write_all(temporary, &json))
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicU64, Ordering};
    use core::time::Duration;

    use super::*;

    /// A directory under the system temporary directory, removed when dropped.
    #[derive(Debug)]
    struct TemporaryDirectory(PathBuf);

    impl TemporaryDirectory {
        fn new(purpose: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "clipped-recovery-{purpose}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).expect("a temporary directory can be created");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TemporaryDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    /// Writes a sidecar whose one recording never ended, and the file behind
    /// it.
    ///
    /// Exactly the shape `super::sidecar` writes, minus the fields
    /// `end_recording` would have added — which is what a killed recorder
    /// leaves.
    fn interrupted_session(directory: &Path, session_id: &str, bytes: usize) -> PathBuf {
        let recording = directory.join(format!("clipped-{session_id}.mkv"));
        fs::write(&recording, vec![0u8; bytes]).expect("the recording can be written");

        let sidecar = directory.join(format!("clipped-{session_id}{SIDECAR_SUFFIX}"));
        let file = serde_json::json!({
            "schema_version": 1,
            "session_id": session_id,
            "game": { "kind": "known", "game_id": "counter-strike-2", "name": "Counter-Strike 2" },
            "started_at": "2026-08-11T14:32:05+01:00",
            "ended_at": null,
            "recordings": [{
                "index": 1,
                "output": recording.display().to_string(),
                "started_at": "2026-08-11T14:32:05+01:00",
                "ended_at": null,
                "outcome": null
            }],
            "clips": [],
            "bookmarks": [],
            "events": [
                { "at": "2026-08-11T14:32:05+01:00", "event": "session-started", "pid": 4242 },
                { "at": "2026-08-11T14:32:05+01:00", "event": "recording-started", "index": 1 }
            ]
        });
        fs::write(
            &sidecar,
            serde_json::to_vec_pretty(&file).expect("the shape encodes"),
        )
        .expect("the sidecar can be written");
        sidecar
    }

    fn read(path: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(path).expect("the sidecar can be read"))
            .expect("the sidecar is JSON")
    }

    #[test]
    fn a_recording_that_began_and_never_ended_is_found_with_its_file() {
        let directory = TemporaryDirectory::new("found");
        interrupted_session(directory.path(), "counter-strike-2-20260811-143205", 4096);

        let found = interrupted_recordings(directory.path()).expect("the directory can be listed");

        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].session_id(), "counter-strike-2-20260811-143205");
        assert_eq!(found[0].game(), "Counter-Strike 2");
        assert_eq!(found[0].index(), 1);
        assert_eq!(found[0].bytes(), Some(4096));
        assert!(found[0].has_footage());
        assert!(found[0].output().exists());
    }

    #[test]
    fn a_recording_that_ended_normally_is_not_offered_for_recovery() {
        // The other half, and the half that would make the check useless if it
        // were wrong: every finished recording in a library has a sidecar entry
        // too, and offering all of them would make the feature noise.
        let directory = TemporaryDirectory::new("finished");
        let sidecar = interrupted_session(directory.path(), "cs2-20260811-143205", 4096);

        let mut file = read(&sidecar);
        file["recordings"][0]["ended_at"] = Value::from("2026-08-11T14:38:05+01:00");
        file["recordings"][0]["outcome"] = Value::from("recorded");
        fs::write(&sidecar, serde_json::to_vec_pretty(&file).expect("encodes"))
            .expect("the sidecar can be rewritten");

        let found = interrupted_recordings(directory.path()).expect("the directory can be listed");
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn a_recording_stamped_with_an_end_but_no_outcome_is_still_treated_as_open() {
        // The window between the two writes. They are written together today,
        // and a check that read only `ended_at` would silently stop finding
        // anything if that ever changed.
        let directory = TemporaryDirectory::new("half-closed");
        let sidecar = interrupted_session(directory.path(), "cs2-20260811-143205", 4096);

        let mut file = read(&sidecar);
        file["recordings"][0]["ended_at"] = Value::from("2026-08-11T14:38:05+01:00");
        fs::write(&sidecar, serde_json::to_vec_pretty(&file).expect("encodes"))
            .expect("the sidecar can be rewritten");

        assert_eq!(
            interrupted_recordings(directory.path())
                .expect("the directory can be listed")
                .len(),
            1
        );
    }

    #[test]
    fn a_recording_whose_file_never_appeared_is_reported_as_having_no_footage() {
        // A recorder killed before the encoder produced its first packet. There
        // is nothing to recover, and saying so is better than printing a path
        // to a file that is not there (AGENTS.md section 27).
        let directory = TemporaryDirectory::new("no-file");
        let sidecar = interrupted_session(directory.path(), "cs2-20260811-143205", 0);
        let file = read(&sidecar);
        let output = PathBuf::from(
            file["recordings"][0]["output"]
                .as_str()
                .expect("the output is a string"),
        );
        fs::remove_file(&output).expect("the file can be removed");

        let found = interrupted_recordings(directory.path()).expect("the directory can be listed");
        assert_eq!(found[0].bytes(), None);
        assert!(!found[0].has_footage());
    }

    #[test]
    fn adopting_a_recording_closes_its_record_and_leaves_the_file_alone() {
        let directory = TemporaryDirectory::new("adopt");
        let sidecar = interrupted_session(directory.path(), "cs2-20260811-143205", 4096);
        let found = interrupted_recordings(directory.path()).expect("listed");

        adopt(&found[0], at(1_786_459_085)).expect("the record can be closed");

        assert!(
            found[0].output().exists(),
            "adopting must never touch the footage"
        );
        let file = read(&sidecar);
        assert_eq!(file["recordings"][0]["outcome"], Value::from(INTERRUPTED));
        assert!(
            file["recordings"][0]["ended_at"].is_string(),
            "the entry should have an end time: {file}"
        );
        assert_eq!(
            interrupted_recordings(directory.path())
                .expect("listed")
                .len(),
            0,
            "an adopted recording must not be offered again"
        );
    }

    #[test]
    fn adopting_a_recording_leaves_the_sessions_history_explaining_what_happened() {
        let directory = TemporaryDirectory::new("adopt-events");
        let sidecar = interrupted_session(directory.path(), "cs2-20260811-143205", 4096);
        let found = interrupted_recordings(directory.path()).expect("listed");

        adopt(&found[0], at(1_786_459_085)).expect("the record can be closed");

        let file = read(&sidecar);
        let events = file["events"].as_array().expect("the events are a list");
        let last = events.last().expect("an event was appended");
        assert_eq!(last["event"], Value::from("recording-ended"));
        assert_eq!(last["outcome"], Value::from(INTERRUPTED));
        assert_eq!(last["index"], Value::from(1));
    }

    #[test]
    fn a_field_this_build_does_not_know_about_survives_being_recovered() {
        // The reason the sidecar is rewritten as JSON rather than through a
        // typed mirror of the schema. A recording made by a newer Clipped and
        // recovered by an older one must come back with the newer build's
        // fields intact, or recovering somebody's footage would quietly cost
        // them the metadata attached to it (AGENTS.md sections 43 and 56).
        let directory = TemporaryDirectory::new("forward");
        let sidecar = interrupted_session(directory.path(), "cs2-20260811-143205", 4096);

        let mut file = read(&sidecar);
        file["recordings"][0]["audio_tracks"] = serde_json::json!(["game", "microphone"]);
        file["highlights"] = serde_json::json!([{ "at": 12.5, "kind": "kill" }]);
        fs::write(&sidecar, serde_json::to_vec_pretty(&file).expect("encodes"))
            .expect("the sidecar can be rewritten");

        let found = interrupted_recordings(directory.path()).expect("listed");
        adopt(&found[0], at(1_786_459_085)).expect("the record can be closed");

        let after = read(&sidecar);
        assert_eq!(
            after["recordings"][0]["audio_tracks"],
            serde_json::json!(["game", "microphone"]),
            "a field on the recording was dropped: {after}"
        );
        assert_eq!(
            after["highlights"],
            serde_json::json!([{ "at": 12.5, "kind": "kill" }]),
            "a field on the session was dropped: {after}"
        );
        assert_eq!(
            after["schema_version"],
            Value::from(1),
            "the schema version was dropped: {after}"
        );
    }

    #[test]
    fn discarding_a_recording_deletes_the_file_and_says_so_in_the_record() {
        let directory = TemporaryDirectory::new("discard");
        let sidecar = interrupted_session(directory.path(), "cs2-20260811-143205", 4096);
        let found = interrupted_recordings(directory.path()).expect("listed");

        let discarded = discard(&found[0], at(1_786_459_085)).expect("the file can be discarded");

        assert_eq!(discarded.bytes_freed(), Some(4096));
        assert!(!found[0].output().exists(), "the file should be gone");
        let file = read(&sidecar);
        assert_eq!(file["recordings"][0]["outcome"], Value::from(DISCARDED));
        assert_eq!(
            file["recordings"][0]["output"],
            Value::from(found[0].output().display().to_string()),
            "the record of which file it was must stay: a gap is not a record"
        );
    }

    #[test]
    fn discarding_a_recording_whose_file_is_already_gone_still_closes_the_record() {
        let directory = TemporaryDirectory::new("discard-missing");
        interrupted_session(directory.path(), "cs2-20260811-143205", 0);
        let found = interrupted_recordings(directory.path()).expect("listed");
        fs::remove_file(found[0].output()).expect("the file can be removed");

        let discarded = discard(&found[0], at(1_786_459_085)).expect(
            "nothing to delete is not a \
                                                                     failure",
        );

        assert_eq!(discarded.bytes_freed(), None);
        assert!(interrupted_recordings(directory.path())
            .expect("listed")
            .is_empty());
    }

    #[test]
    fn a_damaged_session_record_does_not_hide_the_recoverable_footage_beside_it() {
        // The failure this rules out is the expensive one: one unreadable file
        // making every other session's footage invisible, on the launch where
        // somebody is trying to get their recording back.
        let directory = TemporaryDirectory::new("damaged");
        fs::write(
            directory.path().join("clipped-broken.session.json"),
            b"{ this is not json",
        )
        .expect("the damaged file can be written");
        interrupted_session(directory.path(), "cs2-20260811-143205", 4096);

        let found = interrupted_recordings(directory.path()).expect("the directory can be listed");

        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].session_id(), "cs2-20260811-143205");
    }

    #[test]
    fn a_directory_that_has_never_been_recorded_into_answers_with_nothing() {
        let directory = TemporaryDirectory::new("empty");
        let never = directory.path().join("no-such-folder");

        assert!(interrupted_recordings(&never)
            .expect("a directory that is not there is not an error")
            .is_empty());
    }

    #[test]
    fn sessions_are_listed_in_a_stable_order_rather_than_the_filesystems() {
        let directory = TemporaryDirectory::new("order");
        interrupted_session(directory.path(), "cs2-20260811-150000", 16);
        interrupted_session(directory.path(), "cs2-20260811-143205", 16);

        let found = interrupted_recordings(directory.path()).expect("listed");
        let ids: Vec<&str> = found.iter().map(InterruptedRecording::session_id).collect();
        assert_eq!(ids, ["cs2-20260811-143205", "cs2-20260811-150000"]);
    }

    #[test]
    fn a_file_that_is_not_a_session_record_is_not_read_as_one() {
        assert!(is_sidecar(Path::new(
            r"D:\clips\clipped-cs2-20260811-143205.session.json"
        )));
        assert!(!is_sidecar(Path::new(r"D:\clips\clipped-cs2.mkv")));
        assert!(!is_sidecar(Path::new(r"D:\clips\notes.session.json")));
        assert!(!is_sidecar(Path::new(
            r"D:\clips\clipped-cs2.bookmarks.json"
        )));
    }
}
