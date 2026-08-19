-- The copy kept when a clip's edit document is replaced by a converted one.
--
-- Issue #306. `clips.edit` (migration 0004) holds a clip's document as
-- `clipped_edit::EditDocument::write` wrote it. Reading one written by an older
-- build converts it in memory, and `docs/editing.md` is explicit about what the
-- caller owes in return:
--
--     Migration is the caller's write, not this crate's. Reading a document
--     written by an older build converts it in memory and reports that it did;
--     the caller decides whether to store the result, and must keep the
--     original when it does.
--
-- Before this there was nowhere to keep it. The recorder is now that caller --
-- it serves a document to the editor and takes an edited one back -- so the
-- obligation became real, and an obligation with no column behind it is a
-- comment.
--
-- WHY A COLUMN AND NOT A BACKUP FILE
--
-- A document is a value, not a file (migration 0004 says why, and AGENTS.md
-- section 32 puts application metadata in the database). Writing the original
-- beside the recordings would invent a second place a clip lives, with a
-- directory and a locking story of its own, for text that is a few kilobytes.
-- It also would not survive the database being copied to another machine,
-- which is exactly the case this is for: the user who edited a clip on the
-- build that was up to date and opened it on the one that was not.
--
-- WHY IT IS WRITTEN ONCE AND NEVER AGAIN
--
-- This holds the only copy of a document *this build could not have produced*.
-- Overwriting it on the second save would replace it with text this build wrote
-- and destroy the thing it exists to keep, so the write is conditional --
-- `WHERE edit_superseded IS NULL` -- and the second save leaves it alone. That
-- is AGENTS.md section 56: the older text is user data, and a save is not a
-- reason to lose it.
--
-- NULL therefore means "nothing has ever been converted for this clip", which
-- is the state of every clip made by this build. It is not "the original was
-- the same": a save of a document that was already at the current version
-- writes nothing here at all, because there is nothing older to keep.
--
-- WHY THE VERSION IS STORED BESIDE IT
--
-- The text carries its own `schema_version`, so this is redundant in the strict
-- sense. It is here because the question somebody asks of this column -- "which
-- clips are still holding a format 1 original?" -- is a query, and the
-- alternative is parsing every document in the table to answer it. That is the
-- same argument `source_recording_id` and `duration_seconds` are columns rather
-- than facts inside `edit` (migration 0004).

ALTER TABLE clips ADD COLUMN edit_superseded TEXT;

ALTER TABLE clips ADD COLUMN edit_superseded_at TEXT;

ALTER TABLE clips ADD COLUMN edit_superseded_version INTEGER;
