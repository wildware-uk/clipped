-- An event now says which recording it landed in, and where.
--
-- `0001_initial.sql` created `session_events` with a session, a wall-clock `at`,
-- a `kind` and a `detail`, and said the migration giving `kind` its values
-- "belongs to the milestone that defines them". `clipped-events` defines them
-- now (issue #68), and issue #71 asks for something the original table cannot
-- hold: events stored "with their session **and recording**", positioned
-- correctly "when a session spans multiple recording segments or the recording
-- started after the game".
--
-- A wall-clock stamp cannot answer that on its own. One sitting can produce
-- several recordings -- a window destroyed and recreated, a machine that slept,
-- a game restarted inside the grace period (docs/sessions.md) -- and each has
-- its own timeline starting at its own moment. Drawing a kill on the right
-- second of the right file needs the file and the offset into it, so both are
-- stored rather than derived later from timestamps that may not survive a move
-- to another machine.
--
-- Both are NULLABLE, and that is not laxness:
--
--   * An event can arrive before any recording has started. The game is
--     running, the plugin is attached, and the session has not begun a file
--     yet -- or never will, because it is keeping a replay buffer and nothing
--     else. Issue #71's own second acceptance criterion is about exactly that
--     case, so the schema has to be able to represent it rather than force a
--     recording that does not exist.
--   * `ON DELETE SET NULL` rather than CASCADE, for the reason the rest of the
--     schema uses it: deleting a recording must not silently take the record of
--     what happened during it. The event still happened, it is still on the
--     session, and it stops claiming a position in a file that has gone.
--
-- `ALTER TABLE ... ADD COLUMN` rather than a rebuild. Neither column has a
-- CHECK constraint or a NOT NULL, which is what forced `sessions` to be rebuilt
-- in 0002; SQLite adds a nullable column with a foreign key in place, and doing
-- less to a table that holds somebody's data is the whole argument.

ALTER TABLE session_events ADD COLUMN recording_id INTEGER
    REFERENCES recordings (recording_id) ON DELETE SET NULL;

-- Seconds from the start of that recording, which is the number a timeline
-- draws with. REAL because an event is not on a frame boundary and rounding one
-- to a second would put a kill up to half a second from where it happened.
ALTER TABLE session_events ADD COLUMN offset_seconds REAL;

-- The timeline's query: every event of one recording, in order.
CREATE INDEX session_events_recording
    ON session_events (recording_id, offset_seconds);
