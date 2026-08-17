-- A recovered recording can end as `interrupted` or `discarded`, not only
-- `recorded`, `no-window` or `failed`.
--
-- `clipped-recorder recover` (issue #103) writes those two words into a
-- session's sidecar when somebody keeps or throws away the footage a killed
-- recorder left (`clipped_session::automatic::recovery::{INTERRUPTED,
-- DISCARDED}`), and until now this table's CHECK constraint did not know
-- either one: `crates/library/src/index/ingest.rs`'s `RECORDING_OUTCOMES`
-- degraded them to NULL plus an `IndexProblem`, which is honest but is the
-- gap issue #278 tracks. Issue #451 gives `recover --discard` a reason to
-- close that gap for these two words specifically -- it indexes a recovered
-- fragment before sending it to `clipped_library`'s trash, so the row it
-- writes has to survive being reconciled again later, once its sidecar says
-- `discarded`, without the database refusing the very word `clipped-session`
-- wrote.
--
-- SQLite cannot alter a CHECK constraint in place, so `recordings` is
-- rebuilt. The recipe is `0002_manual_session_end_reason.sql`'s and
-- `0004_clips_without_a_file.sql`'s: rows parked in a holding table with no
-- constraints, the real table dropped and created again -- never renamed, for
-- the reason those files give -- and refilled. Every other column and both
-- indexes are restated exactly as `0001_initial.sql` and
-- `0005_recording_spans.sql` left them; only the one CHECK changes.

-- Somewhere to keep the rows while `recordings` does not exist. No
-- constraints and no foreign key: it holds what was already accepted for the
-- length of one transaction, and constraining it again could only reject a
-- row that is already in somebody's library.
CREATE TABLE recordings_migrating_to_0006 (
    recording_id     INTEGER PRIMARY KEY,
    session_id       TEXT,
    session_index    INTEGER,
    path             TEXT,
    started_at       TEXT,
    ended_at         TEXT,
    outcome          TEXT,
    end_reason       TEXT,
    duration_seconds REAL,
    frames_encoded   INTEGER,
    width            INTEGER,
    height           INTEGER,
    size_bytes       INTEGER,
    missing_since    TEXT,
    deleted_at       TEXT,
    deleted_from     TEXT,
    favourited_at    TEXT,
    starts_at_nanos  INTEGER
);

-- Named columns rather than `SELECT *`: a copy relying on column order would
-- put the wrong value in the wrong column the moment either table gained one.
INSERT INTO recordings_migrating_to_0006 (
    recording_id, session_id, session_index, path, started_at, ended_at,
    outcome, end_reason, duration_seconds, frames_encoded, width, height,
    size_bytes, missing_since, deleted_at, deleted_from, favourited_at,
    starts_at_nanos
)
SELECT
    recording_id, session_id, session_index, path, started_at, ended_at,
    outcome, end_reason, duration_seconds, frames_encoded, width, height,
    size_bytes, missing_since, deleted_at, deleted_from, favourited_at,
    starts_at_nanos
FROM recordings;

DROP TABLE recordings;

-- Everything below is `0001_initial.sql`'s `recordings` plus
-- `0005_recording_spans.sql`'s `starts_at_nanos`, with two words added to the
-- one vocabulary this migration exists to widen. It is repeated in full,
-- comments included, because this file is now the definition of the table and
-- a reader should not have to hold three of them at once.
CREATE TABLE recordings (
    recording_id     INTEGER PRIMARY KEY,
    session_id       TEXT NOT NULL REFERENCES sessions (session_id) ON DELETE CASCADE,
    -- The recording's ordinal within its session, as written in the sidecar.
    session_index    INTEGER NOT NULL,
    path             TEXT NOT NULL UNIQUE,
    started_at       TEXT NOT NULL,
    ended_at         TEXT,
    -- `clipped_session::automatic::RecordingOutcomeSummary`, plus the two
    -- words `clipped_session::automatic::recovery` mints when a killed
    -- recorder's footage is recovered: `interrupted` (kept, unrewritten) and
    -- `discarded` (sent to the trash). NULL means the recording was still
    -- running when this row was last written.
    outcome          TEXT CHECK (
        outcome IN ('recorded', 'no-window', 'failed', 'interrupted', 'discarded')
    ),
    -- `clipped_session::report::EndReason`, and only meaningful for 'recorded'.
    end_reason       TEXT CHECK (end_reason IN ('stopped', 'target-lost', 'target-resized')),
    -- What the sidecar records about a finished recording. Every one of them is
    -- nullable because a recording that failed produced none of them.
    duration_seconds REAL,
    frames_encoded   INTEGER,
    width            INTEGER,
    height           INTEGER,
    -- Storage metadata: the file's size on disk, and when reconciliation first
    -- found it gone. Both are observations, not claims -- neither is written at
    -- record time.
    size_bytes       INTEGER,
    missing_since    TEXT,
    deleted_at       TEXT,
    deleted_from     TEXT,
    favourited_at    TEXT,
    -- Where a recording starts on its session's timeline
    -- (`0005_recording_spans.sql`). Nullable for the reason that file gives.
    starts_at_nanos  INTEGER,
    UNIQUE (session_id, session_index),
    CHECK (path <> ''),
    CHECK (session_index >= 1),
    CHECK (duration_seconds IS NULL OR duration_seconds >= 0.0),
    CHECK (size_bytes IS NULL OR size_bytes >= 0)
) STRICT;

INSERT INTO recordings (
    recording_id, session_id, session_index, path, started_at, ended_at,
    outcome, end_reason, duration_seconds, frames_encoded, width, height,
    size_bytes, missing_since, deleted_at, deleted_from, favourited_at,
    starts_at_nanos
)
SELECT
    recording_id, session_id, session_index, path, started_at, ended_at,
    outcome, end_reason, duration_seconds, frames_encoded, width, height,
    size_bytes, missing_since, deleted_at, deleted_from, favourited_at,
    starts_at_nanos
FROM recordings_migrating_to_0006;

DROP TABLE recordings_migrating_to_0006;

-- Dropped with the old table, so they are made again. Same names and same
-- columns: a query planner that was choosing `recordings_session` before this
-- migration chooses it afterwards.
CREATE INDEX recordings_session ON recordings (session_id, session_index);
CREATE INDEX recordings_started_at ON recordings (started_at);
