# Storage: the database, its schema and its migrations

Clipped keeps recordings as ordinary files and keeps everything it knows *about*
them in one SQLite database. This document covers what is in that database, how
it changes shape without anybody being asked to delete it, who may write to it
and when, and why a failure of any of it cannot cost somebody a recording.

**Status: the database exists and the library index fills part of it.**
`crates/storage` opens it, migrates it and enforces the schema ([#55]). Since
[#402] the recorder reconciles the index against the session sidecars on disk,
so the tables a sitting produces — games, sessions, recordings, events — have a
producer. The rest still do not, and say so by name below.

[#37]: https://github.com/wildware-uk/clipped/issues/37
[#46]: https://github.com/wildware-uk/clipped/issues/46
[#51]: https://github.com/wildware-uk/clipped/issues/51
[#52]: https://github.com/wildware-uk/clipped/issues/52
[#55]: https://github.com/wildware-uk/clipped/issues/55
[#68]: https://github.com/wildware-uk/clipped/issues/68
[#71]: https://github.com/wildware-uk/clipped/issues/71
[#93]: https://github.com/wildware-uk/clipped/issues/93
[#94]: https://github.com/wildware-uk/clipped/issues/94
[#107]: https://github.com/wildware-uk/clipped/issues/107
[#111]: https://github.com/wildware-uk/clipped/issues/111
[#402]: https://github.com/wildware-uk/clipped/issues/402

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

**One process writes, any number of processes read, and inside the writer two
kinds of thread do the writing — neither of them a recording.**

```text
 recorder process                                     desktop process
 ────────────────                                     ───────────────
 capture / encode / recording threads                 library screens
        │                                                    │
        │ cannot reach clipped-storage at all                │ one command,
        │ (tests/integration/tests/workspace_layering.rs)    │ one connection
        │                                                    ▼
        │ index_now(): sets a flag, notifies a         clipped-ipc-connection
        │ condition variable, returns                  threads, one per command
        ▼                                                    │
 clipped-library-index thread                                │ set_favourite
 reconcile · sweep · trash · thumbnails                      │ set_lock
        │                                                    │ restore · empty
        │                                                    ▼
        └──────────────▶  Database::open  ◀──────────────────┘
                                │
                                ▼
                            library.db
                            (WAL mode)
```

The desktop process is on the reading side of that picture and cannot be on the
other: it may link `clipped-ipc` and nothing else of this workspace
(`the_desktop_application_links_nothing_of_this_workspace_but_the_protocol`), so
every question it has about the library is a command the recorder answers, and
so is every change it wants made (ADR 0002,
[#301](https://github.com/wildware-uk/clipped/issues/301)).
`Database::open_read_only` is the connection a second process would read
through; nothing in the product opens one today, for that same reason, and the
tests are what exercise it.

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
a second process meets if it opens the library before the recorder has.
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
which is what the next section is about. In a rollback journal
`NORMAL` buys the same speed by risking the file itself, so a database that fell
back keeps `FULL` — the speed is worth having and the corruption is not.
`durability_is_only_relaxed_where_write_ahead_logging_makes_it_safe` holds both
halves of that.

### Which write runs on which thread

AGENTS.md section 20 says a capture thread must not wait on the database, and
section 18 lists high-frequency database writes among the things to avoid. This
table is how that is kept: every write the product performs, the thread it runs
on, and what is waiting for it.

| Write | Where it is | Thread | What waits for it |
| --- | --- | --- | --- |
| Creating the file and running the migrations | `Database::open` | whichever thread below opened it | that thread, once, on the first use |
| Sessions, recordings, clips and events, from the sidecars | `clipped_library::index::reconcile` | `clipped-library-index` | **nothing** |
| Marking what has gone, and what has come back | `clipped_library::index::reconcile` | `clipped-library-index` | **nothing** |
| Moving recordings to the trash to stay inside a storage limit | `clipped_session::cleanup::sweep`, through `clipped_library::accounting::cleanup` and `clipped_library::trash` | `clipped-library-index` | **nothing** |
| A star put on or taken off | `clipped_library::favourites` | `clipped-ipc-connection` | the one window command that asked for it |
| A padlock put on or taken off | `clipped_library::locks` | `clipped-ipc-connection` | the one window command that asked for it |
| Restoring from the trash, and emptying it | `clipped_library::trash` | `clipped-ipc-connection` | the one window command that asked for it |
| A recovered fragment sent to the trash | `apps/recorder/src/recover.rs` | the main thread of `clipped-recorder recover` | the person who typed the command |
| Nothing, but the file is opened and so may be migrated | `apps/recorder/src/storage.rs` | the main thread of `clipped-recorder storage` | the person who typed the command |

`apps/recorder/src/library.rs` is where the recorder's two halves of that live:
`LibraryIndexer` owns the background thread and the connection it writes
through, and `LibraryReader` owns the connection the window's commands are
answered on. They hold a connection each, deliberately — a page of the library
must not wait behind a reconciliation.

Three things follow, and they are the answers to the question the queue below
used to be the answer to.

**Nothing on a capture or encode path writes, and nothing on one can be made
to.** `clipped-capture`, `clipped-audio`, `clipped-encoder`, `clipped-muxer` and
`clipped-replay` do not depend on `clipped-storage`, directly or through
anything else, so there is no `Database` to reach from a thread taking frames.
Layering does not give that for free — `clipped-storage` is a layer 0 crate, so
any of them *could* name it and still point down the stack — which is why
`no_crate_on_a_capture_or_encoding_path_can_reach_the_database` in
`tests/integration/tests/workspace_layering.rs` asserts it directly.

**Nothing a recording waits for writes either.** The recording thread's one
contact with the index is `RecordingState::index_now` when a sitting ends, which
is `LibraryIndexer::request`: it sets a flag, notifies a condition variable and
returns, and the run it asks for happens on the indexer's thread. The indexer
holds no lock that call needs while it writes.
`asking_for_a_run_does_not_wait_for_the_run_that_is_writing` measures that
against a run of a hundred transactions in flight, and fails if the asking ever
takes as much as a tenth of a second.

**The window's writes are on the connection thread that asked for them, and that
is where they belong.** A star, a padlock and an emptied trash are all things a
person did and is waiting to see the result of; answering "done" before the row
was written would be a window drawing a star it will lose on the next read. The
recorder gives each command its own connection and each connection its own
thread (`crates/ipc/src/server.rs`), so a slow one delays the command that asked
and nothing else.

#### What it costs, and what a collision costs

The measurements are in `docs/library.md`, because
`clipped_library::index` is what does the writing: 2,000 sessions and 3,000
recordings index in **403 ms** across **16 transactions**, and 10,000 sessions in
**1.93 s** across **79**. At the pace the recorder actually uses — small batches
and a real pause between them, because a run cannot know a game is not about to
start — the same 2,000 sessions take **3.54 s** across **125** transactions, no
one of which lasts longer than **13 ms**.

Two writers can still meet: an indexer batch and a window's `set_favourite` want
the same write lock. Both outcomes are bounded and neither loses anything.

- The window's write is a single statement in its own transaction, so SQLite's
  busy handler applies and it waits — at most `busy_timeout`, five seconds — and
  then succeeds.
- The indexer's batch reads before it writes, and SQLite refuses to upgrade a
  read transaction to a write one while another writer holds the lock: it
  answers `SQLITE_BUSY` immediately rather than running the busy handler, which
  is how it avoids a deadlock between two upgraders. The batch is rolled back
  whole, the run ends with a warning, and the next request re-runs it from the
  sidecars — which is what makes an index safe to lose (AGENTS.md section 56).

#### There used to be a queue here

`clipped-storage` shipped a `WriteQueue`: a bounded channel a capture thread
could drop a closure on, drained by a batching `Writer` thread, with benchmarks
in this document showing a submission cost 48 ns. It was written before there
were any callers, on the reasoning that the shape of the writing path decides
whether a capture path can be made to wait on it.

**Nothing ever submitted to it.** By the time the recorder wrote rows at all
([#402](https://github.com/wildware-uk/clipped/issues/402)) every one of them
was written from a thread in the table above, where a stall costs nothing, and
every one of them had a caller that needed the answer — a window waiting to draw
a star, a reconciliation counting what it wrote. A fire-and-forget queue that
drops a write when it is full is the wrong shape for all of them. So it was
removed rather than wired in
([#605](https://github.com/wildware-uk/clipped/issues/605)), and this section
describes what happens instead.

Bring one back for a *producer* that cannot wait and does not need the answer.
There is none today.

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

Version 2. `crates/storage/migrations/0001_initial.sql` is the whole of it and
`0002_manual_session_end_reason.sql` adds one word to one vocabulary; both files
are heavily commented and are the authority. This is the shape of them.

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
| `games` | one row per game actually played, keyed by the catalogue's `game_id` | the recorder's library indexer ([#402]) |
| `sessions` | one sitting with one game, keyed by the recorder's own session identifier | the recorder's library indexer ([#402]) |
| `session_game_candidates` | the games the catalogue could not choose between, for an unattributed session | the recorder's library indexer ([#402]) |
| `recordings` | one media file: path, timings, dimensions, outcome, size, whether it is still there, and where it starts on its session's timeline | the recorder's library indexer ([#402]) |
| `clips` | a clip: the range of a recording it is, why it exists, and its file if it has one yet | a saved replay ([#38](https://github.com/wildware-uk/clipped/issues/38)); a clip with no file is stored by nothing yet ([#56](https://github.com/wildware-uk/clipped/issues/56)) |
| `bookmarks` | a marked moment in a recording: offset, label, colour, duration | the recorder takes them ([#64](https://github.com/wildware-uk/clipped/issues/64)) and writes them to a sidecar beside each recording; nothing indexes them into this table yet (`docs/bookmarks.md`) |
| `session_events` | what happened during a session, in the vocabulary the sidecar already writes | the recorder's library indexer ([#402]) |
| `game_events` | what happened *in the game*: a plugin's event, the moment it happened on the media timeline, and which recording covers that moment | nothing yet — the table exists, and the sidecar write and ingest that fill it are the rest of [#71] |
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
migration, which is the intended cost — and migration 2 is that cost being paid:
a session opened for one recording somebody asked for ends because that recording
did, which is `recording-ended` and was not a reason any game launch could
produce ([#402], `docs/sessions.md`).

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

- **Screenshots.** SPEC.md section 26 designs them; nothing captures one.
- **An interpreted clip edit model.** `clips.edit` holds the document's text
  (migration `0004`), and nothing in this crate parses it: M11 represents cuts,
  audio levels and overlays as metadata over a source recording, that model is
  `clipped-edit`'s, and a second copy of it in SQL columns is what
  AGENTS.md section 55 forbids. What is stored beside the document is only what
  a query has to answer without opening one — which recording it depends on, its
  window, its length and why it exists.
- **Quota and retention policy.** SPEC.md section 27's maximum size, minimum free
  space and maximum age are settings, and they will live in `settings` under keys
  the storage manager defines. Where the *policy* lives is an M12 decision
  ([#93], [#111]) that no crate's remit claims yet.
- **Thumbnails and waveforms.** M8. They are files, so they will be paths.

Every one of those is a column or a table added by a later migration, which is
exactly what the framework below is for.

**Game events used to be on that list, and are not any more.** The reason given
was that the vocabulary did not exist — `clipped-events` was module
documentation with no types in it — and "a table whose `kind` column has no
defined values would be a guess". `clipped-events` is a crate now ([#68]): it
has `EventKind`, `GameEvent`, and a versioned `StoredEvent` envelope that keeps
fields a newer build added. So the guess is gone and `game_events` exists
(migration `0003`, [#71]), shaped the way `docs/highlights.md` argues it.

What has *not* changed is why it is a second table rather than columns on
`session_events`: that table's `at` is RFC 3339 text where an `EventTime` is
signed nanoseconds on the media timeline, it has no `recording_id`, and
`clipped_library::index::ingest` rewrites every one of a session's rows on each
reconciliation — so an event written there would be regenerated rather than
persisted.

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

**6. Two connections may migrate at once, and only one of them does.** A process
holds more than one connection to its library — the recorder answers the window's
questions from one and reconciles the index on another — so on a machine with no
library yet they meet at the moment it is created. Each migration is applied in
an **immediate** transaction and the version is read again *inside* it, so the
second connection waits for the first and then finds there is nothing left to do.
SQLite's deferred default would let both decide to create the same table, and the
loser would fail with `table games already exists` on exactly the first run, for
exactly the users who had never recorded anything.
`two_connections_opening_the_same_new_database_both_get_a_working_schema` opens
four at once and requires every one of them to end up with a usable schema.

**7. The file is claimed.** Clipped stamps `application_id` = `CLPD` into the
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
- **Which thread writes.** Not here — the writes are in `clipped-library` and
  the threads are in `apps/recorder`, so the two properties "Which write runs on
  which thread" claims are asserted where they can be:
  `no_crate_on_a_capture_or_encoding_path_can_reach_the_database` in
  `tests/integration/tests/workspace_layering.rs` reads the dependency graph out
  of `cargo metadata` and fails if anything on a capture or encode path can
  reach this crate at all, and
  `asking_for_a_run_does_not_wait_for_the_run_that_is_writing` in
  `apps/recorder/src/library.rs` times the recording thread's one call into the
  index while a run of a hundred transactions is in flight.
