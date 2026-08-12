-- Migration 1: the library.
--
-- This file is released. Once a build carrying it has opened a user's database
-- it must never be edited again — `migrations::CHECKSUMS` pins its bytes and a
-- test fails if they move (docs/storage.md, "Migrations are append-only").
-- Everything after this arrives as 0002, 0003, … even when the change is a
-- single column.
--
-- Every table is STRICT, so a column declared INTEGER cannot quietly hold the
-- text "12". SQLite's default type affinity turns a mistyped binding into data
-- that reads back as the wrong type months later, which for a library of
-- recordings means a duration that sorts as a string.
--
-- Timestamps are TEXT holding RFC 3339 with an offset, exactly as the session
-- sidecars already write them (docs/sessions.md). That is what makes ingesting
-- a sidecar a copy rather than a conversion, it sorts lexicographically within
-- one offset, and it survives being read by a human with `sqlite3`.
--
-- Media never lives here. Every file is a path and a handful of facts about it.

--------------------------------------------------------------------------------
-- games
--------------------------------------------------------------------------------
--
-- One row per game the user has actually played, keyed by the catalogue's own
-- identifier (`clipped-game-detection`, docs/game-detection.md). It is not a
-- copy of the catalogue: the catalogue is data shipped with the application
-- plus the user's overlay, and it answers "which game is this process?". This
-- table answers "which games are in my library?", which is a smaller set that
-- only grows when something is recorded.
--
-- `name` is denormalised from the catalogue on purpose. A catalogue entry can
-- be renamed or removed by an update, and a library that then showed a
-- recording under a blank name would have lost the only record of what the user
-- played.
CREATE TABLE games (
    game_id       TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    first_seen_at TEXT NOT NULL,
    last_played_at TEXT,
    -- SPEC.md section 29: a recording, session, clip or screenshot can be
    -- favourited. A game cannot, so there is no column here for it.
    CHECK (game_id <> ''),
    CHECK (name <> '')
) STRICT;

--------------------------------------------------------------------------------
-- sessions
--------------------------------------------------------------------------------
--
-- One sitting with one game (docs/sessions.md). `session_id` is the identifier
-- the recorder already generates — `<game_id>-<yyyymmdd>-<hhmmss>` — so a
-- session's row, its sidecar and the files it produced all carry the same name
-- and an ingest is idempotent without inventing a second identity for it.
--
-- `game_id` is nullable because a session can be recorded without being
-- attributed: when the catalogue reports a tie, the recording is made and filed
-- under no game rather than under a guess. The candidates it could not choose
-- between go in `session_game_candidates`.
--
-- ON DELETE RESTRICT rather than CASCADE: removing a game must not silently
-- take a user's sessions with it (AGENTS.md section 56).
CREATE TABLE sessions (
    session_id    TEXT PRIMARY KEY,
    game_id       TEXT REFERENCES games (game_id) ON DELETE RESTRICT,
    started_at    TEXT NOT NULL,
    ended_at      TEXT,
    -- The vocabulary `clipped_session::automatic::SessionEndReason` writes into
    -- the sidecar. Constrained rather than free text so that a typo in a writer
    -- is a failed insert instead of a session that no filter ever matches;
    -- adding a reason is a migration, which is the intended cost.
    end_reason    TEXT CHECK (
        end_reason IN ('game-exited', 'system-resumed', 'recorder-stopping')
    ),
    -- Where the session's JSON record is, when it has one. The sidecar is the
    -- recorder's crash-safe write; this column is what lets the library go back
    -- to it, and what makes re-ingesting a session possible after the database
    -- has been restored from a backup.
    sidecar_path  TEXT,
    favourited_at TEXT,
    CHECK (session_id <> '')
) STRICT;

CREATE INDEX sessions_started_at ON sessions (started_at);
CREATE INDEX sessions_game ON sessions (game_id, started_at);

-- The games the catalogue could not choose between for an unattributed session.
--
-- No foreign key to `games`: a candidate is a catalogue identifier for a game
-- that has, by definition, never been recorded, so requiring a row in `games`
-- would mean inventing library entries for games the user has not played.
CREATE TABLE session_game_candidates (
    session_id TEXT NOT NULL REFERENCES sessions (session_id) ON DELETE CASCADE,
    game_id    TEXT NOT NULL,
    PRIMARY KEY (session_id, game_id)
) STRICT;

--------------------------------------------------------------------------------
-- recordings
--------------------------------------------------------------------------------
--
-- One media file. A session has one recording in the ordinary case and more
-- when a window was destroyed and recreated, the machine slept, or the game was
-- restarted inside the session's grace period (docs/sessions.md).
--
-- `path` is the absolute path of a file this database does not own. Nothing in
-- this crate opens it. `size_bytes` and `missing_since` are the two facts
-- reconciliation against the filesystem produces — the library is a view over
-- what is actually on disk, and a user who moves or deletes a file behind the
-- application's back must not be shown a broken row with no explanation.
--
-- `deleted_at` and `deleted_from` are the trash (SPEC.md section 28): deleting
-- footage moves the file rather than unlinking it, so restoring it needs to know
-- where it was. A row is never removed when its file goes to the trash — that is
-- what makes restore possible.
CREATE TABLE recordings (
    recording_id     INTEGER PRIMARY KEY,
    session_id       TEXT NOT NULL REFERENCES sessions (session_id) ON DELETE CASCADE,
    -- The recording's ordinal within its session, as written in the sidecar.
    session_index    INTEGER NOT NULL,
    path             TEXT NOT NULL UNIQUE,
    started_at       TEXT NOT NULL,
    ended_at         TEXT,
    -- `clipped_session::automatic::RecordingOutcomeSummary`. NULL means the
    -- recording was still running when this row was last written.
    outcome          TEXT CHECK (outcome IN ('recorded', 'no-window', 'failed')),
    -- `clipped_session::report::EndReason`, and only meaningful for 'recorded'.
    end_reason       TEXT CHECK (end_reason IN ('stopped', 'target-lost', 'target-resized')),
    -- What the sidecar records about a finished recording. Every one of them is
    -- nullable because a recording that failed produced none of them.
    duration_seconds REAL,
    frames_encoded   INTEGER,
    width            INTEGER,
    height           INTEGER,
    -- Storage metadata: the file's size on disk, and when reconciliation first
    -- found it gone. Both are observations, not claims — neither is written at
    -- record time.
    size_bytes       INTEGER,
    missing_since    TEXT,
    deleted_at       TEXT,
    deleted_from     TEXT,
    favourited_at    TEXT,
    UNIQUE (session_id, session_index),
    CHECK (path <> ''),
    CHECK (session_index >= 1),
    CHECK (duration_seconds IS NULL OR duration_seconds >= 0.0),
    CHECK (size_bytes IS NULL OR size_bytes >= 0)
) STRICT;

CREATE INDEX recordings_session ON recordings (session_id, session_index);
CREATE INDEX recordings_started_at ON recordings (started_at);

--------------------------------------------------------------------------------
-- clips
--------------------------------------------------------------------------------
--
-- A shorter file the user chose to keep: the replay buffer saved to disk
-- (issue #37) or, later, an exported edit. Nothing in this build can create
-- one, and this table is empty in every database this build writes.
--
-- It is modelled now because clip playback is a screen waiting on this schema
-- (issue #52) and because the sidecar already reserves the word, so the
-- alternative is two shapes for one concept.
--
-- `source_recording_id` is nullable and ON DELETE SET NULL: a replay saved
-- while nothing was being recorded has no source recording, and a clip must
-- outlive the recording it came from — the whole point of saving it was to keep
-- it after the session is gone (AGENTS.md section 57).
--
-- What is deliberately absent: the edit model. M11's clip editor represents cuts,
-- audio levels and overlays as metadata over a source, and none of that is
-- designed yet. `source_start_seconds`/`source_end_seconds` describe the single
-- window a saved replay came from and nothing more.
CREATE TABLE clips (
    clip_id              INTEGER PRIMARY KEY,
    session_id           TEXT REFERENCES sessions (session_id) ON DELETE SET NULL,
    source_recording_id  INTEGER REFERENCES recordings (recording_id) ON DELETE SET NULL,
    path                 TEXT NOT NULL UNIQUE,
    title                TEXT,
    created_at           TEXT NOT NULL,
    source_start_seconds REAL,
    source_end_seconds   REAL,
    duration_seconds     REAL,
    size_bytes           INTEGER,
    missing_since        TEXT,
    deleted_at           TEXT,
    deleted_from         TEXT,
    favourited_at        TEXT,
    CHECK (path <> ''),
    CHECK (
        source_start_seconds IS NULL
        OR source_end_seconds IS NULL
        OR source_end_seconds >= source_start_seconds
    )
) STRICT;

CREATE INDEX clips_session ON clips (session_id, created_at);
CREATE INDEX clips_source ON clips (source_recording_id);

--------------------------------------------------------------------------------
-- bookmarks
--------------------------------------------------------------------------------
--
-- A marked moment inside a recording, with the four fields SPEC.md section 25
-- specifies: timestamp, label, colour and duration. The timestamp is an offset
-- into the recording rather than a wall-clock time, because a bookmark is a
-- place in a file and has to survive the file being moved, copied or opened on
-- another machine.
--
-- Bookmarks are M8. Nothing in this build creates one.
CREATE TABLE bookmarks (
    bookmark_id      INTEGER PRIMARY KEY,
    recording_id     INTEGER NOT NULL REFERENCES recordings (recording_id) ON DELETE CASCADE,
    at_seconds       REAL NOT NULL,
    label            TEXT,
    -- A colour as the user picked it, in whatever notation the UI writes; this
    -- crate does not interpret it.
    colour           TEXT,
    duration_seconds REAL,
    created_at       TEXT NOT NULL,
    CHECK (at_seconds >= 0.0),
    CHECK (duration_seconds IS NULL OR duration_seconds >= 0.0)
) STRICT;

CREATE INDEX bookmarks_recording ON bookmarks (recording_id, at_seconds);

--------------------------------------------------------------------------------
-- session_events
--------------------------------------------------------------------------------
--
-- What happened during a session: it started, a recording began, the game
-- exited, the machine woke up. This is the vocabulary the sidecar already
-- writes and `clipped_session::automatic` already produces, so it is a table
-- with a producer today.
--
-- `detail` is the event's remaining fields as the JSON object the sidecar wrote,
-- because those fields differ per event kind — a process identifier for
-- `session-started`, an output path for `recording-started`, a gap in seconds
-- for `system-resumed`. Columns for the union of them would be fifteen mostly
-- NULL columns, and a column per kind is a table per kind.
--
-- Game events — a kill, a round starting — are deliberately *not* here. They
-- are a different vocabulary entirely, they come from plugins, and that
-- vocabulary does not exist yet: `clipped-events` is module documentation with
-- no types in it and the plugin API is M9. A table whose `kind` column has no
-- defined values would be a table that is wrong, and the migration that adds it
-- belongs to the milestone that defines them (docs/storage.md).
CREATE TABLE session_events (
    event_id   INTEGER PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions (session_id) ON DELETE CASCADE,
    at         TEXT NOT NULL,
    kind       TEXT NOT NULL,
    detail     TEXT,
    CHECK (kind <> '')
) STRICT;

CREATE INDEX session_events_session ON session_events (session_id, at);

--------------------------------------------------------------------------------
-- tags
--------------------------------------------------------------------------------
--
-- Free-form labels the user applies, and one of the things search filters on
-- (SPEC.md section 30). A tag is a row rather than a string on each item so
-- that renaming one is one update and so that listing the tags in a library
-- does not mean scanning every recording.
--
-- Two join tables rather than one polymorphic table with an `item_kind` column:
-- a polymorphic key cannot carry a foreign key, and losing referential
-- integrity here would mean tags surviving the thing they were on.
CREATE TABLE tags (
    tag_id INTEGER PRIMARY KEY,
    name   TEXT NOT NULL UNIQUE,
    CHECK (name <> '')
) STRICT;

CREATE TABLE recording_tags (
    recording_id INTEGER NOT NULL REFERENCES recordings (recording_id) ON DELETE CASCADE,
    tag_id       INTEGER NOT NULL REFERENCES tags (tag_id) ON DELETE CASCADE,
    PRIMARY KEY (recording_id, tag_id)
) STRICT;

CREATE INDEX recording_tags_tag ON recording_tags (tag_id);

CREATE TABLE clip_tags (
    clip_id INTEGER NOT NULL REFERENCES clips (clip_id) ON DELETE CASCADE,
    tag_id  INTEGER NOT NULL REFERENCES tags (tag_id) ON DELETE CASCADE,
    PRIMARY KEY (clip_id, tag_id)
) STRICT;

CREATE INDEX clip_tags_tag ON clip_tags (tag_id);

--------------------------------------------------------------------------------
-- settings and game_settings
--------------------------------------------------------------------------------
--
-- Recording settings, and the storage policy that SPEC.md section 27 configures
-- (maximum size, minimum free space, maximum age): global values, and per-game
-- overrides that inherit from them where absent. That inheritance is AGENTS.md
-- section 30's rule and it is why these are two tables with the same shape —
-- resolving a setting is "the game's row if there is one, otherwise the global
-- row", which is one query and no NULL-keyed primary key.
--
-- `value` is JSON text and this crate does not interpret it. The set of keys and
-- the meaning of each is configuration's business, not storage's, and per-game
-- configuration is M7 (SPEC.md section 31): writing a column per setting now
-- would be inventing the shape of settings nobody has designed, and would make
-- every new setting a migration for no gain.
CREATE TABLE settings (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK (key <> '')
) STRICT;

CREATE TABLE game_settings (
    game_id    TEXT NOT NULL REFERENCES games (game_id) ON DELETE CASCADE,
    key        TEXT NOT NULL,
    value      TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (game_id, key),
    CHECK (key <> '')
) STRICT;
