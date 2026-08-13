//! Turning one session sidecar into rows.
//!
//! This is the ingestion `docs/storage.md` says belongs to this crate: the
//! database is *derived* from the sidecars, `sessions.sidecar_path` is the route
//! back to the source of a row, and a library that was lost can be rebuilt by
//! reading the files again.
//!
//! # The two authorities
//!
//! | Question | Answered by |
//! | --- | --- |
//! | Which game, when, which files, what happened | the sidecar |
//! | Whether a file is there and how large it is | the filesystem |
//! | Favourites, tags, bookmarks, what is in the trash | the user |
//!
//! Ingestion writes the first two and **never touches the third**. That is not
//! a stylistic rule: an upsert that wrote every column would silently
//! unfavourite a session on the next reconciliation, and the user would have no
//! way to tell what had happened (AGENTS.md section 56).
//! `crates/library/tests/reconciliation.rs` re-indexes a favourited, tagged,
//! bookmarked session and checks all three survive.
//!
//! # Idempotence
//!
//! Re-indexing the same sidecar produces the same rows. Sessions, games and
//! recordings are upserted on their natural keys — the session identifier the
//! recorder generates, and a recording's ordinal within its session — and the
//! two tables with no natural key, `session_events` and
//! `session_game_candidates`, are rewritten wholesale for the session being
//! ingested. Both are wholly derived from the file, so replacing them loses
//! nothing.
//!
//! # Costs
//!
//! Ingestion is pure SQLite: every file the session names has already been
//! looked at by [`prepare`], which runs outside the transaction. Nothing here
//! waits on a disk it does not have to, because the write lock is held while it
//! runs (AGENTS.md section 20).

use std::path::{Path, PathBuf};

use clipped_storage::rusqlite::{params, OptionalExtension, Savepoint};
use tracing::debug;

use super::error::IndexProblem;
use super::moment;
use super::presence::{self, FileFacts};
use super::sidecar::{self, SessionSidecar, SidecarError, SidecarGame};

/// The words `recordings.outcome` may hold.
const RECORDING_OUTCOMES: &[&str] = &["recorded", "no-window", "failed"];
/// The words `recordings.end_reason` may hold.
const RECORDING_END_REASONS: &[&str] = &["stopped", "target-lost", "target-resized"];
/// The words `sessions.end_reason` may hold.
const SESSION_END_REASONS: &[&str] = &[
    "game-exited",
    "system-resumed",
    "recorder-stopping",
    // A session opened for one recording somebody asked for, which ends when
    // that recording does (`clipped_session::automatic::ManualSession`).
    "recording-ended",
];

/// A sidecar that has been read, with the filesystem already consulted about
/// every file it names.
///
/// The split exists so that the transaction is opened knowing everything it
/// needs: reading and `stat`ing happen here, writing happens later, and the
/// database's single writer is never held waiting on a disk.
#[derive(Debug)]
pub(crate) struct PreparedSession {
    /// The session as its sidecar describes it.
    pub(crate) sidecar: SessionSidecar,
    /// Where that sidecar is.
    pub(crate) sidecar_path: PathBuf,
    /// Each recording's file, in the same order as `sidecar.recordings`.
    pub(crate) files: Vec<PreparedFile>,
    /// Each clip's file, in the same order as `sidecar.clips`.
    pub(crate) clip_files: Vec<PreparedFile>,
}

/// One recording's file, as the filesystem describes it.
#[derive(Debug)]
pub(crate) struct PreparedFile {
    /// The absolute path, resolved against the sidecar's own directory.
    pub(crate) path: PathBuf,
    /// Whether it is there, and how large.
    pub(crate) facts: FileFacts,
}

/// What writing one session changed.
#[derive(Debug, Default)]
pub(crate) struct SessionWrite {
    /// The identifiers of the recordings written, so that the pass over rows
    /// nothing claimed does not judge them a second time.
    pub(crate) recording_ids: Vec<i64>,
    /// Recordings written.
    pub(crate) recordings: usize,
    /// Recordings whose file was found to have gone.
    pub(crate) newly_missing: usize,
    /// Recordings whose file has come back.
    pub(crate) returned: usize,
}

/// Reads the sidecar at `path` and looks at every file it names.
///
/// All of the I/O of ingesting a session happens here, deliberately, and none
/// of it inside a transaction.
pub(crate) fn prepare(path: &Path) -> Result<PreparedSession, SidecarError> {
    let sidecar = sidecar::read(path)?;
    let directory = path.parent().unwrap_or(Path::new("."));

    let files = sidecar
        .recordings
        .iter()
        .map(|recording| {
            let path = sidecar::recording_path(directory, &recording.output);
            let facts = presence::look_at(&path);
            PreparedFile { path, facts }
        })
        .collect();
    // Through the same resolution a recording's path goes through, so that a
    // folder moved to another drive carries its clips as well as its
    // recordings.
    let clip_files = sidecar
        .clips
        .iter()
        .map(|clip| {
            let path = sidecar::recording_path(directory, &clip.path);
            let facts = presence::look_at(&path);
            PreparedFile { path, facts }
        })
        .collect();

    Ok(PreparedSession {
        sidecar,
        sidecar_path: path.to_path_buf(),
        files,
        clip_files,
    })
}

/// Writes one prepared session into the database.
///
/// `observed_at` is the moment this reconciliation ran, and is what a newly
/// missing file is marked with. Problems that do not stop the session being
/// indexed are pushed onto `problems`; a failure that does is returned, and the
/// caller rolls the savepoint back.
pub(crate) fn write(
    savepoint: &mut Savepoint<'_>,
    prepared: &PreparedSession,
    observed_at: &str,
    problems: &mut Vec<IndexProblem>,
) -> Result<SessionWrite, clipped_storage::rusqlite::Error> {
    let session = &prepared.sidecar;
    let game_id = write_game(savepoint, prepared, problems)?;

    savepoint.prepare(
        "INSERT INTO sessions (session_id, game_id, started_at, ended_at, end_reason, sidecar_path) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
         ON CONFLICT (session_id) DO UPDATE SET \
             game_id = excluded.game_id, \
             started_at = excluded.started_at, \
             ended_at = excluded.ended_at, \
             end_reason = excluded.end_reason, \
             sidecar_path = excluded.sidecar_path",
    )?
    .execute(params![
        session.session_id,
        game_id,
        session.started_at,
        session.ended_at,
        end_reason(session, problems),
        prepared.sidecar_path.display().to_string(),
    ])?;

    write_candidates(savepoint, session)?;
    write_events(savepoint, session)?;
    let written = write_recordings(savepoint, prepared, observed_at, problems)?;
    // After the recordings, because a clip points at the one it was cut from
    // and the row it points at has to exist.
    write_clips(savepoint, prepared, problems)?;
    Ok(written)
}

/// Writes the session's game, and answers which game the session is of.
///
/// `None` for a session the catalogue could not attribute, which is a state the
/// schema models rather than a failure (`docs/sessions.md`).
fn write_game(
    savepoint: &Savepoint<'_>,
    prepared: &PreparedSession,
    problems: &mut Vec<IndexProblem>,
) -> Result<Option<String>, clipped_storage::rusqlite::Error> {
    let session = &prepared.sidecar;
    let SidecarGame::Known { game_id, name } = &session.game else {
        if matches!(session.game, SidecarGame::Unrecognised) {
            // Reported rather than silently unattributed: it means this build
            // is older than the recorder that wrote the file, and a library
            // quietly filing sessions under nothing is exactly the kind of
            // "working" that hides an upgrade nobody performed (AGENTS.md
            // section 54). The sitting is still indexed.
            problems.push(IndexProblem::Unattributable {
                session_id: session.session_id.clone(),
                detail: "it names a kind of game this build does not know, so it is indexed \
                         without one",
            });
        }
        return Ok(None);
    };

    if game_id.is_empty() || name.is_empty() {
        // The schema forbids both, and a session is worth more than the
        // attribution: it is indexed as unattributed rather than dropped.
        problems.push(IndexProblem::Unattributable {
            session_id: session.session_id.clone(),
            detail: "the game it names has no identifier or no name",
        });
        return Ok(None);
    }

    // "Last played" is when the sitting finished, or when it started if it is
    // still going.
    let played_at = session.ended_at.as_deref().unwrap_or(&session.started_at);

    let existing: Option<(String, String, Option<String>)> = savepoint
        .prepare("SELECT name, first_seen_at, last_played_at FROM games WHERE game_id = ?1")?
        .query_row(params![game_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .optional()?;

    match existing {
        None => {
            savepoint
                .prepare(
                    "INSERT INTO games (game_id, name, first_seen_at, last_played_at) \
                     VALUES (?1, ?2, ?3, ?4)",
                )?
                .execute(params![game_id, name, session.started_at, played_at])?;
        }
        Some((stored_name, first_seen_at, last_played_at)) => {
            // The catalogue can rename a game between two sessions, and the
            // newer name is the one to show — but only if this session really
            // is the newer one, because sidecars are ingested in whatever order
            // the walk met them.
            let name = if moment::at_or_after(played_at, last_played_at.as_deref()) {
                name.as_str()
            } else {
                stored_name.as_str()
            };
            savepoint
                .prepare(
                    "UPDATE games SET name = ?2, first_seen_at = ?3, last_played_at = ?4 \
                     WHERE game_id = ?1",
                )?
                .execute(params![
                    game_id,
                    name,
                    moment::earlier(&first_seen_at, &session.started_at),
                    moment::later(last_played_at.as_deref(), played_at),
                ])?;
        }
    }

    Ok(Some(game_id.clone()))
}

/// Replaces the games an unattributed session could have been.
fn write_candidates(
    savepoint: &Savepoint<'_>,
    session: &SessionSidecar,
) -> Result<(), clipped_storage::rusqlite::Error> {
    savepoint
        .prepare("DELETE FROM session_game_candidates WHERE session_id = ?1")?
        .execute(params![session.session_id])?;

    let SidecarGame::Ambiguous { candidates } = &session.game else {
        return Ok(());
    };
    let mut insert = savepoint.prepare(
        "INSERT OR IGNORE INTO session_game_candidates (session_id, game_id) VALUES (?1, ?2)",
    )?;
    for candidate in candidates.iter().filter(|candidate| !candidate.is_empty()) {
        insert.execute(params![session.session_id, candidate])?;
    }
    Ok(())
}

/// Replaces the session's events.
///
/// Wholesale, because an event has no natural key and is wholly derived from
/// the file: there is nothing in one of these rows a user can have changed, so
/// nothing is lost by rewriting them.
fn write_events(
    savepoint: &Savepoint<'_>,
    session: &SessionSidecar,
) -> Result<(), clipped_storage::rusqlite::Error> {
    savepoint
        .prepare("DELETE FROM session_events WHERE session_id = ?1")?
        .execute(params![session.session_id])?;

    let mut insert = savepoint.prepare(
        "INSERT INTO session_events (session_id, at, kind, detail) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for event in &session.events {
        if event.event.is_empty() {
            debug!(
                session = %session.session_id,
                "an event with no name was skipped"
            );
            continue;
        }
        insert.execute(params![
            session.session_id,
            event.at,
            event.event,
            event.detail_json(),
        ])?;
    }
    Ok(())
}

/// Writes the session's recordings, with what the filesystem said about each.
fn write_recordings(
    savepoint: &mut Savepoint<'_>,
    prepared: &PreparedSession,
    observed_at: &str,
    problems: &mut Vec<IndexProblem>,
) -> Result<SessionWrite, clipped_storage::rusqlite::Error> {
    let session = &prepared.sidecar;
    let mut written = SessionWrite::default();

    for (recording, file) in session.recordings.iter().zip(&prepared.files) {
        if recording.index < 1 {
            problems.push(IndexProblem::UnknownToken {
                session_id: session.session_id.clone(),
                field: "recording index",
                value: recording.index.to_string(),
            });
            continue;
        }

        // One recording that the database refuses — two sessions naming the
        // same file is the way that happens — must not cost the session the
        // rest of its recordings.
        let inner = savepoint.savepoint()?;
        match write_recording(&inner, prepared, recording, file, observed_at, problems) {
            Ok(outcome) => {
                inner.commit()?;
                written.recording_ids.push(outcome.recording_id);
                written.recordings += 1;
                written.newly_missing += usize::from(outcome.newly_missing);
                written.returned += usize::from(outcome.returned);
            }
            Err(error) => {
                drop(inner);
                problems.push(IndexProblem::RecordingRefused {
                    session_id: session.session_id.clone(),
                    session_index: recording.index,
                    path: file.path.clone(),
                    error,
                });
            }
        }
    }

    Ok(written)
}

/// The row a recording already has, if it has one.
///
/// The three columns beyond its identifier are the ones ingestion must read
/// before it writes: two of them belong to the user or to the trash and must
/// survive, and the third is the size of a file that may no longer be there to
/// measure.
struct ExistingRecording {
    recording_id: i64,
    /// Where the row says the file is, which is **not** where the sidecar says
    /// it is once the recording has been deleted (`crate::trash`).
    path: String,
    missing_since: Option<String>,
    deleted_at: Option<String>,
    size_bytes: Option<i64>,
}

/// What writing one recording changed.
struct RecordingWrite {
    recording_id: i64,
    newly_missing: bool,
    returned: bool,
}

fn write_recording(
    savepoint: &Savepoint<'_>,
    prepared: &PreparedSession,
    recording: &sidecar::SidecarRecording,
    file: &PreparedFile,
    observed_at: &str,
    problems: &mut Vec<IndexProblem>,
) -> Result<RecordingWrite, clipped_storage::rusqlite::Error> {
    let session_id = &prepared.sidecar.session_id;
    let path = file.path.display().to_string();

    let existing: Option<ExistingRecording> = savepoint
        .prepare(
            "SELECT recording_id, path, missing_since, deleted_at, size_bytes FROM recordings \
             WHERE session_id = ?1 AND session_index = ?2",
        )?
        .query_row(params![session_id, recording.index], |row| {
            Ok(ExistingRecording {
                recording_id: row.get(0)?,
                path: row.get(1)?,
                missing_since: row.get(2)?,
                deleted_at: row.get(3)?,
                size_bytes: row.get(4)?,
            })
        })
        .optional()?;

    // A recording in the trash has been moved on purpose, and its row's `path`
    // is where it was moved *to* — the sidecar still names the location it came
    // from, which is in `deleted_from`. Writing the sidecar's path here would
    // lose the only record of where the file actually is and make restoring it
    // impossible, so the trash keeps its column exactly as ingestion keeps a
    // favourite (see this module's two-authorities table).
    let path = match &existing {
        Some(row) if row.deleted_at.is_some() => row.path.clone(),
        _ => path,
    };

    let judged = presence::judge(
        file.facts.present,
        existing.as_ref().and_then(|row| row.deleted_at.as_deref()),
        existing
            .as_ref()
            .and_then(|row| row.missing_since.as_deref()),
        observed_at,
    );
    let previous_size = existing.as_ref().and_then(|row| row.size_bytes);
    // A file that is not there keeps the size it had when it was, because that
    // is the only record of how much space it took; a consumer showing what a
    // library occupies filters on `missing_since` rather than reading a NULL as
    // a zero (`super::summary`).
    let size_bytes = if file.facts.present {
        file.facts.size_bytes
    } else {
        previous_size
    };

    let outcome = token(
        recording.outcome.as_deref(),
        RECORDING_OUTCOMES,
        session_id,
        "recording outcome",
        problems,
    );
    let end_reason = token(
        recording.end_reason.as_deref(),
        RECORDING_END_REASONS,
        session_id,
        "recording end reason",
        problems,
    );

    let recording_id = match existing {
        Some(ExistingRecording { recording_id, .. }) => {
            savepoint
                .prepare(
                    "UPDATE recordings SET path = ?2, started_at = ?3, ended_at = ?4, \
                         outcome = ?5, end_reason = ?6, duration_seconds = ?7, \
                         frames_encoded = ?8, width = ?9, height = ?10, size_bytes = ?11, \
                         missing_since = ?12 \
                     WHERE recording_id = ?1",
                )?
                .execute(params![
                    recording_id,
                    path,
                    recording.started_at,
                    recording.ended_at,
                    outcome,
                    end_reason,
                    recording.duration_seconds,
                    recording
                        .frames_encoded
                        .and_then(|frames| i64::try_from(frames).ok()),
                    recording.width,
                    recording.height,
                    size_bytes,
                    judged.missing_since,
                ])?;
            recording_id
        }
        None => {
            savepoint
                .prepare(
                    "INSERT INTO recordings (session_id, session_index, path, started_at, \
                         ended_at, outcome, end_reason, duration_seconds, frames_encoded, \
                         width, height, size_bytes, missing_since) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                )?
                .execute(params![
                    session_id,
                    recording.index,
                    path,
                    recording.started_at,
                    recording.ended_at,
                    outcome,
                    end_reason,
                    recording.duration_seconds,
                    recording
                        .frames_encoded
                        .and_then(|frames| i64::try_from(frames).ok()),
                    recording.width,
                    recording.height,
                    size_bytes,
                    judged.missing_since,
                ])?;
            savepoint.last_insert_rowid()
        }
    };

    Ok(RecordingWrite {
        recording_id,
        newly_missing: judged.newly_missing,
        returned: judged.returned,
    })
}

/// Writes the clips the session saved out of its recordings.
///
/// A clip's natural key is its **path**, because a clip has no ordinal within
/// its session the way a recording does: they are saved when somebody presses a
/// key, and the file is the thing. `clips.path` is `UNIQUE`, so re-indexing the
/// same sidecar updates the row it wrote last time rather than adding another.
///
/// A row the trash has moved is found by where it *came from*
/// (`clips.deleted_from`) and keeps its own `path`, which is where the file
/// actually is now — exactly as [`write_recording`] keeps a trashed
/// recording's. Writing the sidecar's path over it would lose the only record
/// of where the file went and make restoring it impossible (AGENTS.md
/// section 56).
///
/// Presence is deliberately **not** judged here. Every clip row is looked at by
/// the pass over rows nothing claimed, in the same run
/// (`super::reconcile_rows`), so a clip whose file has gone is marked there
/// with the same rule and counted in the same figures as every other one.
fn write_clips(
    savepoint: &Savepoint<'_>,
    prepared: &PreparedSession,
    problems: &mut Vec<IndexProblem>,
) -> Result<(), clipped_storage::rusqlite::Error> {
    let session = &prepared.sidecar;

    for (clip, file) in session.clips.iter().zip(&prepared.clip_files) {
        if clip.path.trim().is_empty() {
            problems.push(IndexProblem::UnknownToken {
                session_id: session.session_id.clone(),
                field: "clip path",
                value: String::new(),
            });
            continue;
        }

        let path = file.path.display().to_string();
        let source_recording_id = match clip.source_recording {
            None => None,
            Some(index) => savepoint
                .prepare(
                    "SELECT recording_id FROM recordings WHERE session_id = ?1 \
                     AND session_index = ?2",
                )?
                .query_row(params![session.session_id, index], |row| {
                    row.get::<_, i64>(0)
                })
                .optional()?,
        };

        let existing: Option<(i64, String, bool)> = savepoint
            .prepare(
                "SELECT clip_id, path, deleted_at IS NOT NULL FROM clips \
                 WHERE path = ?1 OR deleted_from = ?1",
            )?
            .query_row(params![path], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .optional()?;

        // The size of the file as it is now, or nothing when it is not there —
        // the row keeps whatever it had, for the reason a recording's does.
        let size_bytes = file
            .facts
            .present
            .then_some(file.facts.size_bytes)
            .flatten();

        match existing {
            Some((clip_id, row_path, deleted)) => {
                let path = if deleted { row_path } else { path };
                savepoint
                    .prepare(
                        "UPDATE clips SET session_id = ?2, source_recording_id = ?3, path = ?4, \
                             title = COALESCE(?5, title), created_at = COALESCE(?6, created_at), \
                             source_start_seconds = ?7, source_end_seconds = ?8, \
                             duration_seconds = ?9, size_bytes = COALESCE(?10, size_bytes) \
                         WHERE clip_id = ?1",
                    )?
                    .execute(params![
                        clip_id,
                        session.session_id,
                        source_recording_id,
                        path,
                        clip.title,
                        clip.created_at,
                        clip.source_start_seconds,
                        clip.source_end_seconds,
                        clip.duration_seconds,
                        size_bytes,
                    ])?;
            }
            None => {
                savepoint
                    .prepare(
                        "INSERT INTO clips (session_id, source_recording_id, path, title, \
                             created_at, source_start_seconds, source_end_seconds, \
                             duration_seconds, size_bytes) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    )?
                    .execute(params![
                        session.session_id,
                        source_recording_id,
                        path,
                        clip.title,
                        // `created_at` is NOT NULL in the schema. A clip whose
                        // sidecar entry does not say when it was saved is filed
                        // at the moment the session started, which is the
                        // nearest true thing this row can say and keeps it
                        // sortable beside its session.
                        clip.created_at.as_deref().unwrap_or(&session.started_at),
                        clip.source_start_seconds,
                        clip.source_end_seconds,
                        clip.duration_seconds,
                        size_bytes,
                    ])?;
            }
        }
    }

    Ok(())
}

/// The reason the session ended, from the event that says so.
///
/// The sidecar carries it as the `reason` of its `session-ended` event rather
/// than as a field of its own, so this is where the column comes from.
fn end_reason<'a>(
    session: &'a SessionSidecar,
    problems: &mut Vec<IndexProblem>,
) -> Option<&'a str> {
    let written = session
        .events
        .iter()
        .rev()
        .find(|event| event.event == "session-ended")
        .and_then(|event| event.detail.get("reason"))
        .and_then(serde_json::Value::as_str)?;

    token(
        Some(written),
        SESSION_END_REASONS,
        &session.session_id,
        "session end reason",
        problems,
    )
}

/// A word, if the schema's vocabulary contains it.
///
/// A word outside it is reported and the column left empty. The vocabularies
/// are `CHECK` constraints, so a recorder that gains a new one ships with the
/// migration that adds it; until then, losing one column of a session is much
/// better than losing the session.
fn token<'a>(
    written: Option<&'a str>,
    vocabulary: &[&str],
    session_id: &str,
    field: &'static str,
    problems: &mut Vec<IndexProblem>,
) -> Option<&'a str> {
    let written = written?;
    if vocabulary.contains(&written) {
        return Some(written);
    }
    problems.push(IndexProblem::UnknownToken {
        session_id: session_id.to_owned(),
        field,
        value: written.to_owned(),
    });
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sidecar_with_events(events: &str) -> SessionSidecar {
        let text = format!(
            r#"{{
                "schema_version": 1,
                "session_id": "counter-strike-2-20260811-143205",
                "game": {{ "kind": "known", "game_id": "cs2", "name": "CS2" }},
                "started_at": "2026-08-11T14:32:05+01:00",
                "events": [{events}]
            }}"#
        );
        sidecar::parse(&text).expect("the sidecar parses")
    }

    #[test]
    fn a_sessions_end_reason_comes_from_the_event_that_ended_it() {
        let session = sidecar_with_events(
            r#"{ "at": "2026-08-11T15:31:21+01:00", "event": "session-ended",
                 "reason": "game-exited" }"#,
        );
        let mut problems = Vec::new();

        assert_eq!(end_reason(&session, &mut problems), Some("game-exited"));
        assert!(problems.is_empty());
    }

    #[test]
    fn a_session_that_has_not_ended_has_no_end_reason() {
        let session = sidecar_with_events(
            r#"{ "at": "2026-08-11T14:32:05+01:00", "event": "session-started", "pid": 42 }"#,
        );
        let mut problems = Vec::new();

        assert_eq!(end_reason(&session, &mut problems), None);
        assert!(problems.is_empty());
    }

    #[test]
    fn an_end_reason_the_schema_does_not_know_is_reported_rather_than_written() {
        // The schema's vocabularies are CHECK constraints, so writing an
        // unknown word would refuse the whole session. A newer recorder that
        // grows a reason must not cost this build a sitting.
        let session = sidecar_with_events(
            r#"{ "at": "2026-08-11T15:31:21+01:00", "event": "session-ended",
                 "reason": "abducted-by-aliens" }"#,
        );
        let mut problems = Vec::new();

        assert_eq!(end_reason(&session, &mut problems), None);
        match problems.as_slice() {
            [IndexProblem::UnknownToken { field, value, .. }] => {
                assert_eq!(*field, "session end reason");
                assert_eq!(value, "abducted-by-aliens");
            }
            other => panic!("expected one unknown token, got {other:?}"),
        }
    }
}
