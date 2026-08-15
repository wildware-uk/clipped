-- Where a game event happened, and in which file.
--
-- `0001_initial.sql` created `session_events` and reserved it, in its own
-- comment, for the session's vocabulary: the things the session manager did.
-- A kill or a round starting is a different vocabulary, it comes from a plugin,
-- and `docs/highlights.md` ("The table, which does not exist yet") gives three
-- reasons why writing one into `session_events` would be wrong rather than
-- merely untidy:
--
--   * its `at` is RFC 3339 text, where an `EventTime` is signed nanoseconds on
--     the media timeline;
--   * it has no `recording_id`, so a moment cannot be tied to the file it
--     landed in; and
--   * `clipped_library::index::ingest` rewrites every one of a session's rows
--     on each reconciliation, so a game event put there would be regenerated
--     rather than persisted.
--
-- So this is the migration `docs/storage.md` records as owed, in the shape
-- `docs/highlights.md` argues, and it is issue #71's persistence half.

CREATE TABLE game_events (
    game_event_id INTEGER PRIMARY KEY,
    session_id    TEXT NOT NULL REFERENCES sessions (session_id) ON DELETE CASCADE,

    -- Nullable because an event can belong to a session and to no file: one
    -- heard before the first recording started, one in the gap after a window
    -- was destroyed and recreated, or one from a replay-buffer-only session
    -- that never wrote anything -- which is issue #71's own second acceptance
    -- criterion, so the schema has to be able to say it.
    --
    -- ON DELETE SET NULL rather than CASCADE: deleting a recording must not
    -- silently take the record of what happened during it (AGENTS.md section
    -- 56). The event still happened and is still the session's; it stops
    -- claiming a position in a file that has gone.
    recording_id  INTEGER REFERENCES recordings (recording_id) ON DELETE SET NULL,

    -- Signed nanoseconds on the session's media timeline, which is what
    -- `clipped_events::EventTime` is. Signed because an event can precede the
    -- origin, and INTEGER rather than text because this column is arithmetic:
    -- `clipped_library::events` subtracts a `RecordedSpan`'s start from it to
    -- get the offset into one file. The offset itself is deliberately not
    -- stored -- it is derived from this and the recording's span, and a stored
    -- copy would be a second truth to keep in step.
    at_nanos      INTEGER NOT NULL,

    kind          TEXT NOT NULL,
    source        TEXT NOT NULL,

    -- The whole `StoredEvent` JSON, and the authority for this row.
    --
    -- This is what makes issue #68's forward compatibility survive storage: a
    -- field a newer build added is inside this text, `ReadEvent::to_json` puts
    -- it back, and an older build that re-indexes a library does not delete
    -- what it could not name (AGENTS.md section 56). The columns beside it are
    -- indexes into this document rather than a second copy of the model --
    -- spreading the envelope across columns would lose everything that did not
    -- fit one.
    document      TEXT NOT NULL,

    -- Only that a kind was given, never which. The vocabulary is open by
    -- design -- a plugin's namespaced `Custom`, or an `Unrecognised` kind this
    -- build has never met, must still be drawn on the timeline -- so a
    -- constraint listing the permitted values would refuse exactly the events
    -- that this design exists to keep.
    CHECK (kind <> '')
) STRICT;

-- Every event of one session, in the order things happened. This also gives the
-- ON DELETE CASCADE above an index to delete through, the way
-- `session_events_session` does for its table.
CREATE INDEX game_events_session ON game_events (session_id, at_nanos);

-- The timeline's query: every event of one recording, in order.
CREATE INDEX game_events_recording ON game_events (recording_id, at_nanos);
