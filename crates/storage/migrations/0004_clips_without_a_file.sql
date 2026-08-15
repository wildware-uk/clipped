-- A clip no longer has to be a file.
--
-- `0001_initial.sql` modelled `clips` as "a shorter file the user chose to
-- keep", which is what a saved replay is, and required the one column a
-- *virtual* clip does not have: `path TEXT NOT NULL UNIQUE`.
--
-- `clipped_library::virtual_clip` (issue #74) describes the other kind: a range
-- of a recording that behaves like a clip without a file existing, so that a
-- session producing twenty interesting moments costs disk and encoder time for
-- none of them until somebody asks for one (SPEC.md sections 19, 20 and 44).
-- That model is written and tested and cannot be stored, which is issue #269 and
-- what `docs/highlights.md` records under "Persistence, which does not exist
-- yet". It is also why issue #76's highlight generation has nowhere to put what
-- it generates.
--
-- Three columns are added and one requirement is dropped:
--
--   * `path` becomes nullable. A clip with no file is the normal case; a file is
--     what an export (issue #89) adds later, to a clip that already exists.
--   * `edit` holds `clipped_edit::EditDocument::write`'s text, kept without
--     being understood -- exactly as `settings.value` is. The document is what
--     the clip *is*: which parts of which recordings, in what order, at what
--     levels. Parsing it here would be a second copy of a model that lives in
--     `clipped-edit`, and one that this crate has no business interpreting.
--   * `origin` and `origin_detail` mirror the `(kind, detail)` pair
--     `session_events` already uses. `ClipOrigin` serialises as, for example,
--     `{"origin":"highlight","kind":"kill","at":600000000000,"source":"acme-cs2"}`
--     -- so the tag is the column, and the remainder is the detail. The library
--     filters on the tag: "what did Clipped generate" is a different question
--     from "what did I save", and a generated clip must be traceable to the
--     event that caused it.
--
-- SQLite cannot drop a NOT NULL or alter a CHECK, so `clips` is rebuilt. The
-- recipe is `0002_manual_session_end_reason.sql`'s, for the reasons that file
-- gives at length: rows parked in a holding table, the real table dropped and
-- created again rather than renamed, because `ALTER TABLE ... RENAME` re-parses
-- every other table's definition and `clip_tags` names `clips` while it would
-- not exist. The framework runs this with foreign keys off, inside one
-- transaction, and checks every foreign key before that transaction ends -- so a
-- rebuild that lost a row rolls the whole migration back rather than leaving
-- `clip_tags` pointing at clips that have gone (AGENTS.md section 56).

-- Somewhere to keep the rows while `clips` does not exist. No foreign key and no
-- vocabulary: it holds what was already accepted for the length of one
-- transaction, and constraining it again could only reject rows that are already
-- in somebody's library.
CREATE TABLE clips_migrating_to_0004 (
    clip_id              INTEGER PRIMARY KEY,
    session_id           TEXT,
    source_recording_id  INTEGER,
    path                 TEXT,
    title                TEXT,
    created_at           TEXT NOT NULL,
    source_start_seconds REAL,
    source_end_seconds   REAL,
    duration_seconds     REAL,
    size_bytes           INTEGER,
    missing_since        TEXT,
    deleted_at           TEXT,
    deleted_from         TEXT,
    favourited_at        TEXT
) STRICT;

-- Named columns rather than `SELECT *`: a copy relying on column order would put
-- the wrong value in the wrong column the moment either table gained one.
INSERT INTO clips_migrating_to_0004 (
    clip_id, session_id, source_recording_id, path, title, created_at,
    source_start_seconds, source_end_seconds, duration_seconds, size_bytes,
    missing_since, deleted_at, deleted_from, favourited_at
)
SELECT
    clip_id, session_id, source_recording_id, path, title, created_at,
    source_start_seconds, source_end_seconds, duration_seconds, size_bytes,
    missing_since, deleted_at, deleted_from, favourited_at
FROM clips;

DROP TABLE clips;

-- Everything below is `0001_initial.sql`'s `clips` with the three columns above
-- and a nullable path. It is repeated in full, comments included, because this
-- file is now the definition of the table and a reader should not have to hold
-- two of them at once.
--
-- `source_recording_id` is nullable and ON DELETE SET NULL: a replay saved while
-- nothing was being recorded has no source recording, and a clip must outlive
-- the recording it came from -- the whole point of saving it was to keep it
-- after the session is gone (AGENTS.md section 57). It stays a column rather
-- than becoming a fact inside `edit`, because "what depends on this recording?"
-- is asked before every deletion (issue #111, `clipped_library::trash`) and has
-- to be a query rather than a scan that parses every clip's document.
CREATE TABLE clips (
    clip_id              INTEGER PRIMARY KEY,
    session_id           TEXT REFERENCES sessions (session_id) ON DELETE SET NULL,
    source_recording_id  INTEGER REFERENCES recordings (recording_id) ON DELETE SET NULL,

    -- Where the file is, when there is one.
    --
    -- NULL for a clip nothing has exported yet, which is most of them. UNIQUE
    -- still holds: SQLite treats NULLs as distinct from one another, so any
    -- number of clips can have no file while no two can share one.
    path                 TEXT UNIQUE,

    title                TEXT,
    created_at           TEXT NOT NULL,

    -- The window of the source this clip came from, for a clip that is one
    -- window. Kept beside `edit` rather than derived from it for the same reason
    -- `source_recording_id` is: a screen listing clips against a recording's
    -- timeline should not have to parse a document per row.
    source_start_seconds REAL,
    source_end_seconds   REAL,

    -- How long the clip is, on its own timeline.
    --
    -- For a saved replay that is the file's duration; for a virtual clip it is
    -- its document's output length. The two agree once the clip is exported,
    -- because that is what an export writes. It is the number a list shows, and
    -- it is stored rather than computed so that showing a hundred clips does not
    -- mean opening a hundred documents.
    duration_seconds     REAL,

    size_bytes           INTEGER,
    missing_since        TEXT,
    deleted_at           TEXT,
    deleted_from         TEXT,
    favourited_at        TEXT,

    -- What this clip is, as `clipped_edit::EditDocument::write` wrote it.
    --
    -- Held as text and not interpreted here, exactly as `settings.value` is.
    -- NULL for a row written before this migration -- a saved replay, whose
    -- window is the two columns above.
    edit                 TEXT,

    -- Why the clip exists, and the rest of what `ClipOrigin` carried.
    --
    -- A closed vocabulary rather than free text, because the library filters on
    -- it. `origin_detail` is the remainder of the serialised origin: for a
    -- highlight, what happened, when, and which plugin said so, which is what
    -- makes a generated clip traceable to the event that caused it.
    origin               TEXT NOT NULL DEFAULT 'replay-buffer',
    origin_detail        TEXT,

    -- Only that a path, when given, is not empty. A missing path is the new
    -- normal case and is not the same as an empty one.
    CHECK (path IS NULL OR path <> ''),
    CHECK (origin IN ('manual', 'replay-buffer', 'highlight')),
    CHECK (
        source_start_seconds IS NULL
        OR source_end_seconds IS NULL
        OR source_end_seconds >= source_start_seconds
    )
) STRICT;

-- Every row this build could be restoring is a file somebody saved, because a
-- clip with no file could not be stored until now. `replay-buffer` is therefore
-- the truth rather than a default chosen for convenience, and the column's
-- DEFAULT says the same thing for a writer that has not been taught about
-- origins yet.
INSERT INTO clips (
    clip_id, session_id, source_recording_id, path, title, created_at,
    source_start_seconds, source_end_seconds, duration_seconds, size_bytes,
    missing_since, deleted_at, deleted_from, favourited_at, origin
)
SELECT
    clip_id, session_id, source_recording_id, path, title, created_at,
    source_start_seconds, source_end_seconds, duration_seconds, size_bytes,
    missing_since, deleted_at, deleted_from, favourited_at, 'replay-buffer'
FROM clips_migrating_to_0004;

DROP TABLE clips_migrating_to_0004;

CREATE INDEX clips_session ON clips (session_id, created_at);
CREATE INDEX clips_source ON clips (source_recording_id);

-- "What did Clipped generate, newest first" is the highlights screen's query,
-- and it is a different question from "what did I save".
CREATE INDEX clips_origin ON clips (origin, created_at);
