-- A session can now end because the recording it was opened for finished.
--
-- Recording from the window opens a session of exactly one recording
-- (`clipped_session::automatic::ManualSession`, issue #402). It has no game
-- whose exit could end it and no restart grace to wait through, so none of the
-- three reasons this column accepted describes it: it ends when its recording
-- does. `0001_initial.sql` says that adding a reason is a migration, and this
-- is that cost being paid.
--
-- SQLite cannot alter a `CHECK` constraint, so `sessions` is rebuilt. The
-- framework runs this with foreign keys off, inside one transaction, and checks
-- every foreign key before that transaction ends (`crates/storage/src/
-- migrations.rs`) — which is what makes a rebuild safe here: the children of
-- `sessions` (`recordings`, `session_events`, `session_game_candidates` and
-- `clips`) must still point at rows that exist afterwards, and a rebuild that
-- lost one rolls the whole migration back rather than leaving an index that has
-- quietly forgotten somebody's recordings (AGENTS.md section 56).
--
-- The rows are parked in a holding table and the real one is dropped and
-- created again, rather than the new table being renamed into place. `ALTER
-- TABLE ... RENAME` re-parses every other table's definition, and the children
-- above name `sessions` while it does not exist, so the rename is the one step
-- of the usual recipe that would fail here. Nothing is renamed, so nothing
-- re-parses.

-- Somewhere to keep the rows while `sessions` does not exist. No foreign key
-- and no vocabulary: it holds what was already accepted for the length of one
-- transaction, and constraining it again would only be able to reject rows that
-- are already in the user's library.
CREATE TABLE sessions_migrating_to_0002 (
    session_id    TEXT PRIMARY KEY,
    game_id       TEXT,
    started_at    TEXT NOT NULL,
    ended_at      TEXT,
    end_reason    TEXT,
    sidecar_path  TEXT,
    favourited_at TEXT
) STRICT;

-- Named columns rather than `SELECT *`: a copy that relied on column order
-- would put the wrong value in the wrong column the moment either table gained
-- one.
INSERT INTO sessions_migrating_to_0002 (
    session_id, game_id, started_at, ended_at, end_reason, sidecar_path, favourited_at
)
SELECT
    session_id, game_id, started_at, ended_at, end_reason, sidecar_path, favourited_at
FROM sessions;

DROP TABLE sessions;

-- Everything below is `0001_initial.sql`'s `sessions` with one word added to
-- the vocabulary. It is repeated in full, comments included, because this file
-- is now the definition of the table and a reader should not have to hold two
-- of them at once.
--
-- `game_id` is nullable because a session can be recorded without being
-- attributed: the catalogue reported a tie, or — since issue #402 — nothing
-- asked it at all, because the user pointed at a window. The recording is made
-- and filed under no game rather than under a guess. The candidates it could
-- not choose between go in `session_game_candidates`.
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
        end_reason IN (
            'game-exited',
            'system-resumed',
            'recorder-stopping',
            'recording-ended'
        )
    ),
    -- Where the session's JSON record is, when it has one. The sidecar is the
    -- recorder's crash-safe write; this column is what lets the library go back
    -- to it, and what makes re-ingesting a session possible after the database
    -- has been restored from a backup.
    sidecar_path  TEXT,
    favourited_at TEXT,
    CHECK (session_id <> '')
) STRICT;

INSERT INTO sessions (
    session_id, game_id, started_at, ended_at, end_reason, sidecar_path, favourited_at
)
SELECT
    session_id, game_id, started_at, ended_at, end_reason, sidecar_path, favourited_at
FROM sessions_migrating_to_0002;

DROP TABLE sessions_migrating_to_0002;

-- Dropped with the old table, so they are made again. Same names and same
-- columns: a query planner that was choosing `sessions_game` before this
-- migration chooses it afterwards.
CREATE INDEX sessions_started_at ON sessions (started_at);
CREATE INDEX sessions_game ON sessions (game_id, started_at);
