# The library index: reconciling the database against the disk

Clipped keeps recordings as ordinary files and keeps an index of them in SQLite.
This document covers the thing that fills that index and keeps it honest: what
it reads, what it does when the database and the filesystem disagree, what it
costs on a large library, and why it cannot get in the way of a recording.

**Status: this is built, and something calls it.** `clipped-library`'s `index`
module reconciles a real database against real folders ([#56]), with tests that
run it against real files on a real disk. Since [#301] the recorder answers
`library_sessions` and `library_games` from it, so the desktop window draws real
rows ([ipc.md](ipc.md), [desktop-ui.md](desktop-ui.md)). Since [#402] the
recorder *fills* it: `clipped-recorder serve` reconciles at start-up and again
whenever a recording finishes, on a thread and a connection of its own
([#385]) — see [Where it runs](#where-it-runs-and-why-it-cannot-compete-with-a-recording).

`clipped-recorder watch` does not index. It writes session sidecars and nothing
else; the run `serve` makes at start-up is what picks them up, so a machine that
only ever runs `watch` has a library that catches up the next time the
application is opened.

[#37]: https://github.com/wildware-uk/clipped/issues/37
[#46]: https://github.com/wildware-uk/clipped/issues/46
[#51]: https://github.com/wildware-uk/clipped/issues/51
[#55]: https://github.com/wildware-uk/clipped/issues/55
[#56]: https://github.com/wildware-uk/clipped/issues/56
[#57]: https://github.com/wildware-uk/clipped/issues/57
[#59]: https://github.com/wildware-uk/clipped/issues/59
[#93]: https://github.com/wildware-uk/clipped/issues/93
[#94]: https://github.com/wildware-uk/clipped/issues/94
[#272]: https://github.com/wildware-uk/clipped/issues/272
[#301]: https://github.com/wildware-uk/clipped/issues/301
[#385]: https://github.com/wildware-uk/clipped/issues/385
[#402]: https://github.com/wildware-uk/clipped/issues/402
[#449]: https://github.com/wildware-uk/clipped/issues/449

## The shape of it

```text
 the recorder                     the library index
 ────────────                     ─────────────────
 D:\clips\
   clipped-cs2-…session.json ──▶  read the sidecar   ─┐
   clipped-cs2-….mkv         ──▶  look at the file   ─┤
   clipped-cs2-…-2.mkv       ──▶  look at the file   ─┤   one transaction
                                                      ├──  per batch  ──▶ library.db
 rows nothing on disk claimed ─▶  look at the file   ─┤
 files no session claims      ─▶  count and report   ─┘
```

Three passes, in that order, and each one exists because the one before it
cannot see what it sees.

## The database is derived

`docs/storage.md` states the rule this module implements: **the sidecars are the
source and the database is an index of them.** The recorder writes one JSON file
per session beside the recordings, atomically, as the session happens
([#46], `docs/sessions.md`). This crate reads those files and writes rows.

That direction is what makes a database failure survivable. Delete `library.db`
and the next reconciliation rebuilds it from the files;
`the_index_can_be_rebuilt_from_the_sidecars_alone` deletes the database outright
and compares every row of the rebuilt one against the original.

| Question | Answered by |
| --- | --- |
| Which game, when it started and ended, which files, what happened | the sidecar |
| Whether a file is there, and how many bytes it is | the filesystem |
| Favourites, tags, bookmarks, what is in the trash | the user |

Ingestion writes the first two and **never the third**. An upsert that wrote
every column would silently unfavourite a session on the next run, and nothing
would tell the user it had happened.
`re_indexing_does_not_lose_a_favourite_a_tag_or_a_bookmark` favourites, tags and
bookmarks a session and re-indexes it.

### What a sidecar becomes

| In the file | In the database |
| --- | --- |
| `game.kind = "known"` | a `games` row, and `sessions.game_id` |
| `game.kind = "ambiguous"` | `sessions.game_id` is NULL, and every candidate goes in `session_game_candidates` |
| `session_id`, `started_at`, `ended_at` | the `sessions` row, with `sidecar_path` pointing back at the file |
| the `session-ended` event's `reason` | `sessions.end_reason` |
| each `recordings[]` entry | a `recordings` row: path, timings, outcome, end reason, duration, frames, width, height |
| each `events[]` entry | a `session_events` row; everything beyond `at` and `event` is kept verbatim as JSON in `detail` |
| — | `size_bytes` and `missing_since`, which are observations of the file rather than claims from the file |

A game's `name` is taken from its **most recently started session**, because the
catalogue can rename a game between two sittings and sidecars are read in
whatever order the walk meets them. `first_seen_at` only ever moves backwards.

## Reconciliation is the substance

Users move, rename and delete files behind the application's back. So:

| What a run finds | What it does |
| --- | --- |
| A sidecar the index does not have | Indexes the session and its recordings |
| A sidecar the index already has | Updates it; running again changes nothing |
| A row whose file has gone | Sets `missing_since`, and **never deletes the row** |
| A row whose file has come back | Clears `missing_since` and measures the file again |
| A row under a root that was not walked | Leaves it alone — nobody looked there |
| A row in the trash (`deleted_at`) | Leaves it alone — the trash owns it ([#94]) |
| A media file no session claims | Counts it, samples a few paths, invents nothing |

**Nothing in this module deletes a file or a row.** That is AGENTS.md
section 56, and it is not squeamishness: a recording that has gone is very often
on a drive somebody is about to plug back in, and a row deleted to tidy up takes
the favourite, the tags and the bookmarks on it as well.

### Missing, and the difference between "gone" and "not looked at"

`missing_since` holds the moment a file was **first** found to be absent, and
does not move while it stays absent — "missing since Tuesday" is the useful
fact, and a mark re-stamped on every run says "missing since a second ago"
forever.

The distinction that matters more is between a file that has gone and a file
nobody looked for:

- A root that cannot be read — the external drive that is not plugged in — is
  reported as an `UnavailableRoot` and **nothing under it is judged**.
  `nothing_under_a_root_that_could_not_be_reached_is_marked_missing` removes a
  whole root and checks that not one recording is marked.
- A row whose path is not under any root that was walked is skipped for the same
  reason.
- A file that cannot be measured for a reason other than "it is not there" — a
  permission failure, a drive that answered badly — is assumed to still be
  there. The one thing that must not happen is a file that exists being marked
  because Windows was busy.

A file named by a sidecar the run has just read is judged from that sidecar's
own evidence, whichever root the file itself is under. The sidecar is written
beside its recordings, so the two are separated only by somebody moving files by
hand — and a mark is reversible the moment the file is found again.

### Media with no session record

Counted and reported, never invented. A file with no sidecar cannot be
attributed to a game without guessing, and a wrong guess files somebody's
footage under a game they were not playing, silently (AGENTS.md section 27).
`IndexReport` carries the count and a bounded, sorted sample of the paths, and
the recorder logs both at every run.

**This is the state an upgrading user is in.** A build before [#402] recorded
from the window and wrote no session record at all, so those `.mkv` files have no
sidecar and nothing can say what they are. They are left exactly where they are —
never adopted under a guessed game, never moved, never renamed, never deleted
(AGENTS.md section 56) — and reported at every run. Giving the user a deliberate
way to claim them is [#272].

## Where it runs, and why it cannot compete with a recording

`reconcile` is a **synchronous function with no threads of its own**. The caller
owns the thread, and it must be a thread of its own: never a capture, encoder or
UI thread (AGENTS.md section 20). It belongs in the process that holds the
database's writing connection — `docs/storage.md`'s one-writer model — and it
can be stopped at any moment with `IndexControl::cancel`.

Four properties keep it out of the way:

1. **Every transaction is short.** Sessions are written `IndexPace::batch` at a
   time and rows are reconciled `IndexPace::page` at a time, each in its own
   transaction. Measured below: **the longest transaction on a library of
   10,000 sessions was 25 ms.** Anything else with a row to write waits for one
   batch, not for the run.
2. **No file is touched while a transaction is open.** Sidecars are read and
   files are looked at *before* the transaction is opened, so the write lock is
   never held waiting on a disk.
3. **It rests between batches.** `IndexPace::background` — the default — pauses
   25 ms between transactions and assumes a game may be recording.
   `IndexPace::foreground` does not pause and is for when a person has asked for
   a rescan and is watching.
4. **Readers are never blocked at all.** The database is in write-ahead logging
   mode, so a library screen reads a consistent snapshot while a reconciliation
   writes (`docs/storage.md`).
   `a_library_screen_can_read_while_a_reconciliation_writes` runs a
   reconciliation of 300 sessions on one thread while another queries the
   library in a loop, and asserts that every query was answered.

Cancellation is checked between files and between transactions. What a cancelled
run had already committed stays committed, so the next run carries on rather
than starting again.

### The caller

`apps/recorder`'s `LibraryIndexer` (`apps/recorder/src/library.rs`). It owns a
thread and a database connection of its own, and it runs at exactly two moments:

| When | Why that moment |
| --- | --- |
| `serve` start-up, after the ready line | Catches up on everything produced while nothing was indexing: sittings `watch` recorded, files copied onto the machine, a `library.db` the user deleted. It is after the ready line so a window connecting never waits for a walk. |
| A recording finished, from the recording state | The session's record is final at that moment, and this is what puts a recording made from the window into the Library screen without a restart. |

Requests **coalesce**: asking while a run is in flight schedules exactly one
more, so a burst of recordings cannot queue a run each.

There is deliberately **no protocol command** that asks for a rescan. Nothing in
`clipped-ipc` offers one, adding one would be a protocol change, and the two
moments above already keep the index correct for everything this build can
produce. A rescan a person asks for belongs with claiming files the library has
no record of, which is [#272].

The indexer's connection is **its own**, not the reader's. A reconciliation holds
its connection for the length of the run; a library screen that had to wait
behind it would be exactly the stall this section is about. Two connections in
one process is what write-ahead logging is for — and it is why migrating a
database that does not exist yet has to be safe against two connections doing it
at once (`docs/storage.md`).

### Demonstrated, not only argued

Everything above is a design argument, and [#385]'s second acceptance criterion
asked for it to be shown. `an_index_run_in_flight_neither_delays_nor_interrupts_a_recording`
in `apps/recorder/tests/ipc_protocol.rs` is that demonstration: it leaves 8,000
earlier sittings in the recordings folder, starts the real recorder, and records
a real window through the real encoder while the start-up walk is going.

On the development machine (RTX 4090, NVMe) the run was still walking — 2,160
sittings in, cancelled by shutdown rather than finished — across the whole of a
recording that ran from 6 ms to 4,035 ms after the walk began, and that recording
came back with 115 frames of AV1 that `ffprobe` validates.

The test is `#[ignore]`d, because it needs a GPU, an encoder and a desktop
session. Two assertions stop it passing for the wrong reason: the run must have
indexed enough sessions to have genuinely been walking the library, and it must
still have been in flight when the recording *stopped*. Both matter — the first
draft wrote its sittings from `SystemTime::now()`, they collapsed onto eight
identifiers because a session's id is its game and the second it started, and
the "demonstration" was a recording made alongside a 46 ms walk of eight
sessions.

Which folders it walks is the recordings folder `record` and `watch` write into
by default. A recording written somewhere else — an `output` a `start_recording`
named — has its session record beside it, outside that root, and is not indexed.
Letting somebody say which folders make up their library is [#272]'s.

### The walk is bounded

By **depth** (eight directories below a root by default), and by refusing to
follow symbolic links and Windows junctions at all — together, a directory tree
that refers to itself cannot spin. Directories reached through two overlapping
roots are visited once.

It is deliberately *not* bounded by a file count or a time budget: a walk that
stopped early would leave the caller unable to tell "these recordings are gone"
from "I did not look", and that difference is the whole of the second pass.

## What it costs

Measured on the maintainer's machine — Windows 11, NVMe system drive, warm
filesystem cache — with `cargo run --release -p clipped-library --example
index_cost`. Both libraries are one folder of empty files; the size of a
recording changes nothing, because nothing here opens one.

**2,000 sessions, 3,000 recordings** (five years of recording every evening):

| Run | Time | Transactions | Longest transaction |
| --- | --- | --- | --- |
| First, everything to index | **403 ms** | 16 | 24 ms |
| Second, nothing changed | **310 ms** | 16 | 26 ms |
| After deleting 200 files | **275 ms** | 16 | 10 ms |
| After putting them back | **292 ms** | 16 | 23 ms |
| Again, at the background pace | 3.54 s | 125 | 13 ms |

**10,000 sessions, 15,000 recordings**:

| Run | Time | Transactions | Longest transaction |
| --- | --- | --- | --- |
| First, everything to index | **1.93 s** | 79 | 24 ms |
| Second, nothing changed | **1.29 s** | 79 | 22 ms |
| After deleting 1,000 files | **1.27 s** | 79 | 25 ms |
| After putting them back | **1.28 s** | 79 | 22 ms |
| Again, at the background pace | 18.1 s | 625 | 25 ms |

The last row of each table is the pace that ships, and the difference is not
overhead — it is the time the run spends deliberately out of the way. Eighteen
seconds to reconcile ten thousand sessions in the background, in transactions of
25 ms, is the trade this module is shaped around.

Nothing is re-ingested conditionally: every sidecar found is read and written on
every run. A "has this changed?" shortcut would need a column that nothing
writes and would make the index blind to a class of change; at 1.3 seconds for
ten thousand sessions it buys nothing worth that.

Memory is bounded by what is on disk rather than by the library: the walk holds
one path per file it found, and the pass over rows reads them a page at a time.

## Failure, one file at a time

Indexing meets bad input constantly, and almost none of it is a reason to stop.
`IndexProblem` is per item — the item is skipped, the run carries on, and the
report lists what could not be used:

| Problem | What happens |
| --- | --- |
| A sidecar that cannot be read or is not JSON | Reported by path; the rest of the library still indexes |
| A sidecar from a newer build | Left alone entirely, so a later build can read it properly |
| A folder that cannot be listed | Reported; anything in it is not indexed |
| A word outside the schema's vocabulary | That column is left empty and the rest of the session is indexed |
| A session the database refuses | Rolled back on its own inside a savepoint |
| A recording the database refuses — two sessions claiming one file | Rolled back on its own; the session keeps its other recordings |

The only failure that ends a run is the database refusing outright, and even
then every batch already committed stays committed.

## Reading it back

`index::browse::list_sessions` is the other direction: one page of sittings,
newest first, with the recordings and clips each produced. It is what the
desktop window asks for over the control protocol, because the window may
neither open `library.db` nor link this crate
([ADR 0002](adr/0002-separate-recorder-process.md), [ipc.md](ipc.md)).

**Everything is a page.** There is no function here that returns the whole
library. A `SessionListing` takes a limit — defaulting to 50, clamped to 200 —
and a cursor, and answers a `SessionPage` carrying `next` when a further session
was actually found. The cursor is offered only when there is one, so a caller
stops on `next: None` rather than on an empty page.

That limit bounds how much is **read**, and deliberately not how large the answer
is, because a count of sessions cannot bound that: a session holds any number of
recordings and clips, so two hundred of them is 135 KB with one recording each
and over 3 MB with thirty. Whatever carries a page across a process boundary has
to bound its own payload in bytes, and `apps/recorder/src/library.rs` does —
against `clipped_ipc::MAX_FRAME_BYTES`, cutting a page short and moving the
cursor back to the last session it carried. This crate knows nothing about
frames, which is why the bound is not here.

**The cursor is a keyset, not an offset.** It names the last session on the page
— `started_at|session_id` — and the next page is everything ordered after it.
An offset would make the tenth page ten times the cost of the first, and would
skip or repeat rows when a reconciliation inserted a session between two
requests. The order is `started_at DESC, session_id DESC`: the second key is not
decoration, because two sessions can share a start moment and an order that is
not total cannot be paged through without losing or repeating one.

**A missing file is listed, never omitted.** `missing_since` crosses the
boundary because a screen has to *say* a file has gone rather than draw a broken
tile (AGENTS.md section 27). Filtering those rows out here would leave the
window unable to tell "you deleted this" from "this was never recorded". A row
in the trash (`deleted_at`) is a different thing and is left out — it is deleted
as far as the library is concerned, and the trash has a screen of its own
([#94]).

**A search is compiled to SQL, and checked against the matcher.** A query
([#59]) becomes a `WHERE` fragment (`search::sql`) that the statement reading a
page carries, so the sessions a search reads are the ones it returns.

It used to run the matcher instead: build the `search::Row` each session
projects — four further statements per session, for its recordings, its clips,
their tags and its events — and ask `Query::matches`. The reason was that
compiling is a second implementation of what a match means and two of those
disagreeing is a bug nobody can see. The reason was right; the cost was 316 ms
over ten thousand sittings on every keystroke ([#449]).

So the reason is answered rather than dropped. The matcher is still the
definition, `browse::row_of` still builds the row it is defined over, and
`the_database_and_the_matcher_select_the_same_sessions` runs both over one
library and fails if they part company. The folding is not duplicated either:
the SQL calls `search::fold` itself, registered on the connection, rather than
SQLite's ASCII-only `lower()` — a stored folded column was tried and dropped,
because a writer that forgot to fill it would silently stop returning matches
rather than fail.

### What a page costs

The same machine and the same command as the reconciliation figures above, on
the same two libraries. A page is 25 sessions, which is what the Library screen
asks for.

| Read | 2,000 sessions | 10,000 sessions |
| --- | --- | --- |
| First page of 25 | **1.2 ms** | **4.6 ms** |
| The 21st page of 25 | **1.0 ms** | **4.1 ms** |
| A search matching one game in twenty | **1.0 ms** | **4.3 ms** |
| A search matching nothing at all | **0.5 ms** | **3.8 ms** |
| The games view, every game at once | 1.1 ms | 8.3 ms |

The second row is the whole claim keyset paging makes, and it is the row to
watch: **page twenty-one costs what page one costs.** An offset would show a
curve there.

The two search rows are the point of [#449], and what they now say is that
**a search costs what browsing costs.** Before it was compiled, the same two
rows read 23 ms and 37 ms at two thousand sessions and 188 ms and 316 ms at ten
thousand — because a search that matched nothing hydrated every session in the
library to reject it, which is the worst case and the one a search box hits on
every keystroke. A search matching nothing is now the *cheapest* of the four
reads, because it is the one that carries no sessions back.

The games view is not a page — it is every game at once, which is what SPEC.md
section 17 draws, and it is counted by SQLite rather than by walking rows.

These are the cost of the read itself, which is what this crate is answerable
for. Carrying a page to the desktop application adds a serialisation of it, once
to size the page against the frame budget and once to send it
(`apps/recorder/src/library.rs`); that is not measured here, and it is a pass
over a hundred-odd kilobytes rather than a query.

Memory is bounded by the page rather than by the library, and more simply than it
used to be: the statement carries the query and a `LIMIT`, so the sessions read
are the sessions returned. A search no longer reads batches of rows it will
discard, because there are none.

## The games view

`game_summaries` is SPEC.md section 17's list: per game, the sessions,
recordings, clips, favourites and bytes, plus how many files could not be found.
A missing file contributes **nothing** to the size — the space it is not
occupying is not being used — and is counted separately; its row keeps the size
it had when it was last seen, so a drive coming back needs no re-measurement.
Sessions the catalogue could not attribute get a row with no identifier and no
name, because what to call that group on screen is the screen's decision.

## How the format stays in step

`clipped-session` writes sidecars and `clipped-library` reads them, and the
first sits four layers above the second in the workspace
(`tests/integration/tests/workspace_layering.rs`), so the compiler cannot hold
the two together. Two tests stand in for it:

- `the_documented_session_record_is_the_one_this_build_indexes` **reads
  `docs/sessions.md`**, indexes the example printed in it, and checks every
  field arrives in the right column. Changing the format means changing that
  document (AGENTS.md section 7), and this test fails until the reader is
  changed with it.
- `a_session_record_the_recorder_wrote_indexes_without_a_single_problem` indexes
  `crates/library/tests/fixtures/written-by-the-recorder.session.json`, captured
  from the real writer running in `clipped-session`'s own tests and committed
  verbatim but for the temporary directory in its paths, which was replaced with
  `D:\clips` so that no machine path is committed.

## How to test it

```text
cargo test -p clipped-library
```

Every one of them runs anywhere: a library is a directory and a database, not a
GPU. The integration tests build real folders under the platform's temporary
directory, provoke every disagreement between the index and the disk described
above, and check the outcome against a real SQLite database. The paging and
search tests in `index::browse` build a real library and page through it, so a
cursor that skipped or repeated a session fails rather than being reasoned
about.

## What this is not

- **Search.** The query language is [#59] and this crate's `search` module is
  where it lives. `index::browse` is a *caller* of it — it builds the row a
  session projects and asks the matcher — and defines none of the language.
- **Thumbnails and waveforms.** Thumbnails are [#57] and landed as
  `clipped_library::thumbnail` ([thumbnails.md](thumbnails.md)); waveforms are
  #66 and live in `clipped-waveform`. Nothing in *this* module — indexing —
  opens a media file, and the thumbnail module never writes to one.
- **Quotas, retention and the trash.** [#93] and [#94]. Nothing here deletes
  anything, and nothing here can.
- **A second walk of the disk.** Storage accounting ([#93]) walks the same
  folders to measure them. Both were built in the same milestone against
  different questions; sharing one walk is worth doing once both have landed,
  and is not worth guessing at before then.
