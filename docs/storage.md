# Storage: the database, its schema and its migrations

Clipped keeps recordings as ordinary files and keeps everything it knows *about*
them in one SQLite database. This document covers what is in that database, how
it changes shape without anybody being asked to delete it, who may write to it
and when, and why a failure of any of it cannot cost somebody a recording.

**Status: the database exists and nothing fills it yet.** `crates/storage` opens
it, migrates it, enforces the schema and offers the write path
([#55]). What writes rows is the library index that reads the session sidecars,
which is M6's remaining work. Nothing here pretends otherwise, and the tables
that have no producer say so by name below.

[#37]: https://github.com/wildware-uk/clipped/issues/37
[#46]: https://github.com/wildware-uk/clipped/issues/46
[#51]: https://github.com/wildware-uk/clipped/issues/51
[#52]: https://github.com/wildware-uk/clipped/issues/52
[#55]: https://github.com/wildware-uk/clipped/issues/55
[#93]: https://github.com/wildware-uk/clipped/issues/93
[#94]: https://github.com/wildware-uk/clipped/issues/94
[#107]: https://github.com/wildware-uk/clipped/issues/107
[#111]: https://github.com/wildware-uk/clipped/issues/111

## The one rule

**No media goes in the database.** A recording is a path and a few facts about
the file at that path; a clip is the same. Nothing in `clipped-storage` opens a
media file, and no column in the schema is a `BLOB`.

That is AGENTS.md sections 31 and 32, and it is what makes the rest of this
document bearable to write. Every failure below — a migration that will not
apply, a file that turns out to belong to another application, a database
deleted by a disk cleaner — costs an *index*. The recordings are still on disk
under names a person can read, still playable in any player, and still described
by the session sidecar written beside them. Losing the database is losing the
answer to "show me every Counter-Strike session from last month", not losing
last month.

`crates/storage/tests/recordings_are_never_touched.rs` puts a recording file in
the same directory as the database and provokes every failure the crate has —
a foreign database, one from a newer build, a corrupt file, a failing write, and
finally deleting the database outright — checking after each that the crate
refused, and that the recording's bytes and modification time are exactly what
they were.

Be exact about what that second check is worth, because it is easy to overstate.
The only files `clipped-storage` opens are the database, the copy taken beside it
before a migration, and the directory those live in; it never opens a media file,
so no change to the crate as it stands can move those bytes. The comparison holds
against every implementation the crate has today, and breaking a refusal fails
the check on the error rather than the comparison. It is a tripwire for the
change that *would* touch a file — a thumbnail cache, a "reclaim space" sweep, a
repair path that renames a recording it cannot find — and a tripwire nothing has
trodden on is doing its job. What has teeth today is the second test, which walks
`PRAGMA table_info` over every table and fails if any column is a `BLOB`.

## Where the file is

Wherever the caller says. `clipped-storage` is a layer 0 crate that reads no
environment and resolves no paths, so `Database::open` takes one:

```rust
let database = Database::open(application_directory.join("library.db"))?;
```

The convention is `%LOCALAPPDATA%\Clipped\library.db`, beside the logs, the
encoder's capability cache and the user's game catalogue overlay — that
directory is `clipped_logging::application_directory`, which is the one function
that resolves it (issue #228). Storage does not call it because they are both
layer 0 and neither may depend on the other; the process that opens the database
calls it instead.

## The concurrency model

**One writer, any number of readers, across processes.**

```text
 recorder process                          desktop process
 ────────────────                          ───────────────
 capture / encode threads                  library screens
        │ never touch the database                │
        ▼                                         │
 WriteQueue  ── bounded channel ──▶ Writer thread │
                                        │          ▼
                                        ▼   Database::open_read_only
                                  Database::open        (query_only)
                                        │              │
                                        └──▶  library.db  ◀──┘
                                              (WAL mode)
```

The database is opened in write-ahead logging mode. That is the whole mechanism:
a reader in WAL mode sees a consistent snapshot from before the write in
progress rather than waiting for it, and the writer never waits for a reader.
`a_reader_can_read_while_a_writer_holds_a_transaction_open` is that property as
a test — a reader counts the rows while an uncommitted insert is open and sees
the committed state, then sees the new row after the commit.

A reader is `Database::open_read_only`, which sets `PRAGMA query_only` so the
connection refuses every statement that would write. It opens the *file* for
writing even so, deliberately: an `SQLITE_OPEN_READONLY` connection to a
WAL-mode database cannot create the shared-memory index it needs, so it fails
outright when the `-shm` file is not already there — which is exactly the state
the desktop application meets if it opens the library before the recorder has.
`query_only` gives the guarantee the flag was wanted for without that failure.

A database that will not take the mode is a warning and not a failure, both when
SQLite answers with a different mode and when the statement fails outright — a
filesystem that cannot map the shared memory WAL needs, usually a network share,
is met either way. Readers then wait for writes, which for a metadata index is
slow rather than broken, and refusing to open would cost somebody their library
over where they chose to keep it.

`synchronous` is `NORMAL` rather than `FULL`, **but only when the mode was
actually granted**. A commit then does not wait for the disk to acknowledge it;
a power cut can lose the most recent transactions but cannot corrupt the file,
and losing the last few metadata writes costs a re-index from the sidecars.
`FULL` would cost a disk flush on every commit made while a game is running,
which is the thing the next section exists to avoid. In a rollback journal
`NORMAL` buys the same speed by risking the file itself, so a database that fell
back keeps `FULL` — the speed is worth having and the corruption is not.
`durability_is_only_relaxed_where_write_ahead_logging_makes_it_safe` holds both
halves of that.

### Nothing on a recording path waits for the database

AGENTS.md section 20 says capture threads must not wait on the database and
section 18 lists high-frequency database writes among the things to avoid. So
the writing path is a queue:

- `WriteQueue::submit` puts a closure on a **bounded channel** and returns. It
  takes no lock on the database, performs no disk I/O, and never blocks. If the
  queue is full it returns `SubmitError::Full` rather than waiting — the video is
  what cannot be made again, and metadata can be rebuilt from the sidecars.
- The `Writer` thread drains the queue and commits what it finds in **one
  transaction per batch**, up to 256 writes or 50 milliseconds, whichever comes
  first. A hundred rows cost one disk flush rather than a hundred.
- Each write runs inside its own savepoint, so one that fails is rolled back
  alone and the rest of the batch still commits.

Measured on the maintainer's machine (`cargo test -p clipped-storage -- --nocapture`):

| Measurement | Result |
| --- | --- |
| 5,000 submissions while the writer was held inside a transaction for 1s | **243 µs total, 48 ns each** |
| 10,000 queued writes | **40 transactions** (about 250 writes each) |

The second number is counted by SQLite's own commit hook rather than by the
writer's bookkeeping, so the measurement does not come from the code being
measured. The first is the one that matters for a capture thread: the database
was deliberately busy for a full second and submitting was not delayed by it at
all.

**Nothing calls any of this yet.** The recorder writes no rows in this build.
The mechanism exists now because the shape of the writing path is what decides
whether a capture path can be made to wait on it, and that decision is much
cheaper to make before there are callers than after.

## How this relates to the session sidecars

The recorder already writes **one JSON sidecar per session**, beside the
recordings, atomically, and has since automatic recording arrived ([#46],
`docs/sessions.md`). That is not a second database and this is not a replacement
for it. **The database ingests the sidecars.**

| | Sidecar | Database |
| --- | --- | --- |
| Written by | the recorder, as the session happens | the library index, afterwards |
| Covers | one session | every session |
| Lives | beside the recordings it describes | in the per-user data directory |
| Survives | uninstalling Clipped | nothing; it is derived |
| Answers | "what is this file?" | "which of my thousand files?" |

They exist for different reasons. The sidecar is crash-safe local knowledge
written by the process that made the files, and it is what keeps a recording
legible to somebody who has stopped using Clipped (AGENTS.md section 32). The
database is an *index*: it answers questions across all of them — every session
of one game, the clips from last week, what is using 40 GB — which a directory
of JSON files cannot answer without reading all of them.

So the sidecar stays authoritative for one session's facts, the database is
derived, and **the database can always be rebuilt from the sidecars**. That is
what makes it safe for a database failure to be a recoverable inconvenience
rather than a data loss, and it is why `sessions.sidecar_path` is a column: the
route back to the source of a row is part of the row.

Ingestion itself belongs to `clipped-library`, whose documented remit is
reconciling the index against what is on disk. It is not in `clipped-storage`,
which owns the file and the schema and deliberately not their meaning.

## The schema

Version 1, in `crates/storage/migrations/0001_initial.sql`. The file is heavily
commented and is the authority; this is the shape of it.

```text
games ──┬── sessions ──┬── recordings ──┬── bookmarks
        │              │                ├── recording_tags ──┐
        │              ├── session_events                    ├── tags
        │              ├── session_game_candidates           │
        │              └── clips ──────── clip_tags ─────────┘
        └── game_settings                    settings
```

| Table | What it holds | Producer today |
| --- | --- | --- |
| `games` | one row per game actually played, keyed by the catalogue's `game_id` | none |
| `sessions` | one sitting with one game, keyed by the recorder's own session identifier | none |
| `session_game_candidates` | the games the catalogue could not choose between, for an unattributed session | none |
| `recordings` | one media file: path, timings, dimensions, outcome, size, whether it is still there | none |
| `clips` | a shorter file the user kept, and the window of the source it came from | nothing can create one |
| `bookmarks` | a marked moment in a recording: offset, label, colour, duration | nothing can create one |
| `session_events` | what happened during a session, in the vocabulary the sidecar already writes | none |
| `tags`, `recording_tags`, `clip_tags` | free-form labels, and what they are on | none |
| `settings`, `game_settings` | global settings and per-game overrides, as JSON values under opaque keys | none |
| `schema_migrations` | which migrations have run, and the checksum of each | the framework |

Some decisions worth stating, because the shape of this is what four screens
([#51], [#52], [#94], [#107]) will be built against.

**Every table is `STRICT`.** SQLite's default type affinity lets a mistyped
binding put the text `"60000"` into an integer column, and nothing notices until
a duration sorts alphabetically eighteen months later. `STRICT` needs SQLite
3.37, which is one of the reasons the SQLite amalgamation is compiled into the
binary rather than the `winsqlite3.dll` that happens to be on the machine.

**Timestamps are RFC 3339 text with an offset**, exactly as the sidecars write
them. That makes ingesting a sidecar a copy rather than a conversion, sorts
correctly within one offset, and is readable by a person with the `sqlite3`
command line — which is the whole point of a local-first application storing
things in a format its user can open.

**Vocabularies are `CHECK` constraints.** A recording's `outcome` may only be
`recorded`, `no-window` or `failed`, and its `end_reason` only `stopped`,
`target-lost` or `target-resized`, because those are the Rust enumerations
`clipped-session` already writes. A typo in a future writer is then a failed
insert rather than a row no filter will ever match. Adding a token is a
migration, which is the intended cost.

**Favourites are a nullable `favourited_at` column** on `sessions`, `recordings`
and `clips` rather than a table of their own. SPEC.md section 29 says any of
those can be favourited; a single polymorphic favourites table would need an
`item_kind` column and could therefore carry no foreign key, which is how you end
up with favourites for rows that no longer exist.

**Deleting is two columns, not a row disappearing.** `deleted_at` and
`deleted_from` are the trash (SPEC.md section 28): footage is moved rather than
unlinked, and restoring it needs to know where it was. A row is never removed
when its file goes to the trash — that is what makes restore possible at all.

**Settings are opaque key/value JSON.** The keys and their meanings belong to
configuration, and per-game configuration is M7. A column per setting today would
be inventing the shape of settings nobody has designed and would make every new
setting a migration. Two tables with the same shape is what AGENTS.md section 30's
inheritance rule looks like in SQL: the game's row if there is one, otherwise the
global row.

### What is deliberately absent

A table that is wrong is worse than a table that is missing, so:

- **Game events.** A kill, a round starting, a highlight. These are a different
  vocabulary from session events entirely, they come from plugins, and that
  vocabulary does not exist — `clipped-events` is module documentation with no
  types in it and the plugin API is M9. A table whose `kind` column has no
  defined values would be a guess. `session_events` covers what is produced
  today; the migration that adds game events belongs to the milestone that
  defines them.
- **Screenshots.** SPEC.md section 26 designs them; nothing captures one.
- **The clip edit model.** M11 represents cuts, audio levels and overlays as
  metadata over a source recording. `clips` models the single window a saved
  replay came from and nothing more.
- **Quota and retention policy.** SPEC.md section 27's maximum size, minimum free
  space and maximum age are settings, and they will live in `settings` under keys
  the storage manager defines. Where the *policy* lives is an M12 decision
  ([#93], [#111]) that no crate's remit claims yet.
- **Thumbnails and waveforms.** M8. They are files, so they will be paths.

Every one of those is a column or a table added by a later migration, which is
exactly what the framework below is for.

## Migrations

The rules, in the order they matter.

**1. Append-only.** A migration that has shipped is never edited; the next change
is the next file. `CHECKSUMS` in `crates/storage/src/migrations.rs` pins the
bytes of every released migration and a test fails if one moves, because an
edited migration means two users at the same schema version with different
schemas — a difference nobody notices until a query works on one machine and not
another. The test prints the checksum to paste when a new migration is added.

**2. All or nothing.** Each migration runs inside its own transaction. A failure
rolls that migration back and stops, leaving the database at the last version
some build of Clipped understood. There is no half-applied state, and the
upgrade can simply be attempted again by a build carrying the fix.

**3. A copy is taken first.** Before the first migration is applied to a database
that already holds a schema, the whole thing is copied beside itself as
`library.pre-v<from>.db`. The copy is made with `VACUUM INTO`, which asks SQLite
for a consistent snapshot *as a database* — the write-ahead log is folded in and
the result is a single file that opens — rather than copying bytes and hoping.
**If the copy cannot be written, nothing is migrated.**

**4. A newer database is refused, not touched.** A build that does not understand
a schema does not write to it. `StorageError::FromNewerVersion` names both
versions and tells the user to update Clipped. This is the same rule the game
catalogue's overlay follows, for the same reason: an old build rewriting a file
from a new one is how somebody loses the library they built on the machine that
was up to date.

**5. Foreign keys off during, checked after.** Rebuilding a table in SQLite means
creating, copying, dropping and renaming, which is impossible with enforcement
on. So it is off, and `PRAGMA foreign_key_check` runs *inside the same
transaction* afterwards. A rebuild that dropped rows out from under their
children is rolled back rather than committed.

**6. The file is claimed.** Clipped stamps `application_id` = `CLPD` into the
SQLite header. A file that already holds tables and does not carry that stamp is
somebody else's database, and opening it returns
`StorageError::NotAClippedDatabase` rather than writing a schema over it. The
most likely way to arrive there is a mistyped path.

There is exactly one record of which version a database is at — the
`schema_migrations` table — because a second one, `PRAGMA user_version` for
instance, is a second thing that can disagree with the first.

### Writing one

Add `crates/storage/migrations/000N_<name>.sql`, add it to `MIGRATIONS`, run the
tests, and paste the checksum the failing test prints into `CHECKSUMS`. The SQL
must not contain `BEGIN`, `COMMIT` or `PRAGMA foreign_keys`: the framework owns
the transaction and the pragma, and a migration that took either into its own
hands would defeat rules 2 and 5. A test asserts that too.

Migrations must preserve data. Adding a column, a table or an index is easy;
anything that moves data is a rebuild, and a rebuild is written to carry every
row across and is then checked by rule 5. Nothing may drop a user's rows to make
a schema tidy (AGENTS.md section 56).

## How to test it

```text
cargo test -p clipped-storage
```

Thirty-one tests, all of which run anywhere — a database is a file, not a GPU.
They cover, among other things:

- **Upgrading from every prior version.** `every_shipped_version_migrates_forwards_to_the_current_schema`
  builds a database at each version in turn and migrates it to the newest, then
  asserts that its schema is *identical to a database created today* — comparing
  the whole of `sqlite_master`, not a list of table names somebody remembered to
  update. It loops over the versions rather than naming them, so it covers each
  new migration on the day it is added. `migrating_from_every_earlier_version_reaches_the_same_schema`
  does the same against a three-version fixture, so the property is exercised
  across more versions than the shipped schema has yet.
- **A failure part-way through a migration.** A fixture migration creates a
  table, inserts a row and then references a table that does not exist. The test
  asserts the version is left at the one before, that the failed migration's
  table is absent and that its insert is gone — and then that a build carrying
  the corrected migration finishes the upgrade.
- **A database from a newer build.** Refused by both `open` and
  `open_read_only`, with the file unchanged.
- **A database belonging to something else.** Refused, and its own table and rows
  are still there afterwards.
- **The backup.** Asserted to be a database, at the old version, containing a row
  written before the upgrade — a file that exists and does not open is not a
  backup. And a backup that cannot be written stops the migration before it
  starts.
- **The schema enforcing what it declares.** Foreign keys are off by default in
  SQLite and per connection, so a schema full of `REFERENCES` clauses enforces
  nothing unless the connection asks. The test writes an orphaned session, an
  outcome outside the vocabulary and text into an integer column, and expects
  three refusals. A second test does the same against a database that needed no
  migration, because those are two different code paths and only the second one
  depends on `configure` — migrating turns enforcement back on at the end, so a
  newly created database has it either way. That test exists because a mutation
  found the gap.
- **The write path.** That submitting never waits, that a burst commits in few
  transactions, that a failed write does not take its batch with it, that a full
  queue refuses rather than blocks, and that stopping the writer writes
  everything already queued.
