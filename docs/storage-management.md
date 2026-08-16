# Storage management

Clipped fills disks. A session of 1080p60 is around 8 GB an hour, an evening of
play is a few of those, and the whole point of an automatic recorder is that
nobody is deciding, per session, whether to keep it. So SPEC.md section 27 makes
storage a product feature: a user configures a maximum size, a minimum amount of
free disk space and a maximum recording age, and expects the figures they are
shown to be the truth about their own disk.

Two parts of that exist today, in two modules that deliberately know nothing
about each other:

- **Storage accounting** — measuring what the library occupies, attributing it,
  and saying whether the configured limits are met. It lives in
  [`crates/library/src/accounting`](../crates/library/src/accounting)
  ([issue #93](https://github.com/wildware-uk/clipped/issues/93)) and is the
  first half of this document.
- **The trash** — what deleting a recording actually does, how long it is
  recoverable for, and what restoring it puts back. It lives in
  [`crates/library/src/trash`](../crates/library/src/trash)
  ([issue #94](https://github.com/wildware-uk/clipped/issues/94)) and is
  ["The trash"](#the-trash) below.

**Nothing in accounting deletes anything**, and that is the line that module is
built around. Acting on a breached limit is
[issue #111](https://github.com/wildware-uk/clipped/issues/111) and the screen
that shows all of it is
[issue #95](https://github.com/wildware-uk/clipped/issues/95). Recordings are
irreplaceable (AGENTS.md section 56), so the code that measures them and the code
that removes them are different modules in different tickets, and the measuring
one has no capability to delete.

The trash is what makes #111 defensible. Automatic cleanup deletes recordings on
the user's behalf, on a schedule nobody watches, and that is only an acceptable
thing to build if there is a way back from it.

## Where it lives, and why not in `clipped-storage`

`clipped-storage` owns the persistence *mechanism*: the SQLite schema and the
on-disk layout ([issue #55](https://github.com/wildware-uk/clipped/issues/55)).
`clipped-library` owns the *view over what is actually on disk*, which is what
its crate documentation has claimed from the start, and accounting is that view —
it walks the filesystem, reconciles the result against the index, and answers
questions about games and sessions, which are library concepts. Putting policy
in `clipped-library` also keeps `clipped-storage` free of it, which is the split
[architecture.md](architecture.md) now records against the storage manager: the
mechanism there, the measurement and the limits here.

It is a module rather than a new crate: it depends on nothing but the standard
library and one Windows call, and a crate whose whole content is one module is a
layer in the dependency graph that buys nothing.

Accounting does not read the database and does not depend on `clipped-storage`.
The index is passed in as data (`IndexedItem` values), which keeps accounting a
pure function of "what is on disk" and "what the index says".

## What counts

A quota that omits half of what is on disk is worse than no quota, because the
user believes it. Every category below is counted towards the total, and the
caller declares a directory — a *root* — for each one it has.

| Category | What it holds | Ever a cleanup candidate? |
| --- | --- | --- |
| `Recordings` | Captured sessions | Yes, oldest first, subject to #111's protection rules |
| `Clips` | Clips cut from recordings | Yes, same rules |
| `Screenshots` | SPEC.md section 26 screenshots | Yes, same rules |
| `Thumbnails` | Generated preview images | Regenerable, so cheap to lose |
| `Waveforms` | Generated timeline audio data | Regenerable |
| `ReplayBuffer` | The replay buffer's disk backing ([#36](https://github.com/wildware-uk/clipped/issues/36)) | **No** — it belongs to a recording in progress |
| `Trash` | Deleted media awaiting retention ([#94](https://github.com/wildware-uk/clipped/issues/94)) | Yes, and by its own retention period as well |
| `Logs` | Diagnostics ([logging.md](logging.md)) | **No** — already bounded by rotation |
| `Metadata` | The database and sidecar files | **No** |

Two of those are worth stating plainly because they are the ones a naive
implementation misses. **Trash counts**: footage in the trash still occupies the
disk, and a user who deletes 40 GB and sees no change in free space is owed an
explanation rather than a surprise. **The replay buffer's disk backing counts**:
it is real bytes on a real disk even though nothing has been saved yet.

The last column is not prose. `StorageCategory::is_cleanup_candidate` is the
code that says it, and the two places a limit could otherwise reach the wrong
file both go through it: `StorageInventory::cleanup_candidates_older_than`,
which is what the maximum recording age is judged from, and
`StorageInventory::cleanup_candidates_oldest_first`, which is the order
[#111](https://github.com/wildware-uk/clipped/issues/111) will work in. So the
database is never counted as over-age footage and is never handed to a deleter
as a first candidate, while still being counted towards the total. Everything is
counted; only *selection* is narrowed.

No root may contain another. A trash directory inside the recordings directory
would be walked twice and the total would be wrong in the direction that makes a
cleanup delete more than it needed to, so `StorageRoots` refuses an overlap when
the roots are declared, rather than producing a plausible wrong number later.

## Where the figures come from

Two sources, and they will disagree. Users move recordings, delete them in
Explorer, restore backups, and drop files of their own into the recordings
folder. The rule accounting applies is:

> **The filesystem is the authority for bytes. The index is the authority for
> meaning.**

| Situation | What accounting reports |
| --- | --- |
| Indexed and on disk, sizes agree | Counted, attributed to its game and session |
| Indexed and on disk, sizes differ | Counted **at the size on disk**; the disagreement is listed for the indexer |
| Indexed, not on disk | Counted as **nothing**; listed as missing |
| On disk, not indexed | **Counted**, attributed to nothing |

Both failure modes matter. Trusting a stale row would have a quota delete real
recordings to make room that already exists; ignoring untracked files would
report 40 GB while the disk filled. `Reconciliation` never changes the total — it
only moves bytes between columns — and never changes the index; healing that is
the indexer's job, and the reconciliation is the evidence it works from.

## Accuracy, and the tolerance

The reported figure is the **sum of logical file lengths**: what the files
contain, as `std::fs::Metadata::len()` reports it. It is exact in that sense and
was checked against an independent enumeration. Building a library and counting
it twice:

```text
cargo run --release -p clipped-library --example scan_cost -- --files 4000 --keep --path %TEMP%\xcheck
Get-ChildItem -Recurse -File $env:TEMP\xcheck | Measure-Object -Property Length -Sum
```

The harness wrote 8,000 files (4,000 recordings of 1,024 bytes and a 64-byte
thumbnail each) totalling **4,352,000 bytes**; the scan reported that number, the
harness's own record of what it wrote was that number, and PowerShell's
enumeration of the same tree returned `Count: 8000, Sum: 4352000`. Three
independent counts, agreeing to the byte.

It is deliberately **not** the number of bytes the volume allocates. That
difference used to be stated here as arithmetic; it is now measured. The same
harness sums `FILE_STANDARD_INFO.AllocationSize` — what NTFS has actually
reserved for each file, read through `GetFileInformationByHandleEx`, which needs
no elevation — over the library it built:

| Library | Logical | Allocated | Difference | Bound (one cluster per file) |
| --- | --- | --- | --- | --- |
| 20,000 files | 10.9 MB | 41.6 MB | 30,720,000 B (1,536 B/file) | 81,920,000 B — held |
| 100,000 files | 54.4 MB | 208.0 MB | 153,600,000 B (1,536 B/file) | 409,600,000 B — held |

The cluster size on that volume is 4,096 bytes, read with `GetDiskFreeSpaceW`
rather than assumed. The measured average is *below* one cluster per file because
NTFS keeps a small enough file's data inside its MFT record and allocates it
nothing at all — every 64-byte thumbnail in that library allocates zero, and
every 1,024-byte recording allocates a whole 4,096-byte cluster. Note the shape
of the test library exaggerates this enormously: its files are 1 KB, where a real
recording is gigabytes and rounds by at most 4 KB.

So the difference has this documented shape:

- **Cluster rounding, upwards, at most one cluster per file** — measured above,
  and bounded by `files × cluster size`. For a library of 5,000 real recordings
  that is at most 20 MB, 0.008% of a 250 GB quota, and proportionally smaller the
  larger the files are.
- **Filesystem overhead, upwards, and not counted at all.** MFT records,
  directory indexes and the change journal occupy the volume and belong to no
  file. This is why the minimum-free-space limit is judged from the volume's own
  free space rather than by subtracting the library's total from the disk size.
- **Compression, sparse files, deduplication and hard links, downwards.**
  Clipped writes none of these, but a user may enable NTFS compression on their
  recordings folder, in which case the volume consumes *less* than the figure
  here.

So: the figure is exact as a measure of the user's data, within one cluster per
file of what the volume spends holding it, and the settings screen should present
it as the size of the library rather than as free space arithmetic.

Two things are excluded from the figure by design rather than by oversight.
**Links are not followed**: a symbolic link, junction or other reparse point is
counted as nothing and not descended into, because following one counts a file
twice through two paths and a link to an ancestor walks for ever. The scan
reports how many it skipped, which is what explains a total that looks short.
**Directories themselves weigh nothing**, which is the same convention Explorer
uses.

## What it costs, and where it runs

A scan is one directory enumeration per directory and one length per file. No
file's contents are read. Measured with
`cargo run --release -p clipped-library --example scan_cost [-- --files 50000]`,
on a synthetic library:

| Library | Files | Directories | First pass | Second pass |
| --- | --- | --- | --- | --- |
| 10,000 recordings + thumbnails | 20,000 | 1,285 | 0.124 s | 0.108 s |
| 50,000 recordings + thumbnails | 100,000 | 6,410 | 0.647 s | 0.570 s |

**Hardware and conditions.** AMD Ryzen 9 9950X3D (16 cores, 32 threads), 64 GB
RAM, Crucial P5 Plus 2 TB NVMe SSD (PCIe 4.0), NTFS with 4,096-byte clusters,
Windows 11 Pro 26200, release profile. The machine was **not** idle — several
Rust builds were running throughout — so these are figures from a loaded
machine rather than a best case. Repeating each row immediately gave 0.116 s /
0.105 s and 0.763 s / 0.638 s, which is the spread to expect: about ±15%, and
the reason no figure here is quoted to more precision than it deserves.

Roughly 130,000–190,000 files a second, linear in the number of files, with the
second pass faster because the filesystem cache is warm. A library of 50,000
recordings is far larger than a year of heavy use produces: 100,000 files is
well under a second.

**Holding the result** costs about 280 bytes per file for that library — 88
bytes of structures and 193 bytes of path text — so its 100,000-entry inventory
is around 28 MB. Both halves of that are worth understanding before the number
is reused:

- The paths are counted **twice**, because they are stored twice. The inventory
  is a `BTreeMap<PathBuf, FileEntry>` and the entry keeps its own copy of its
  path, so each file owns two heap allocations of the same text. That is the
  price of being able to look a file up by path *and* hand out entries that know
  where they are.
- It is therefore a **floor**: the harness's estimate excludes the B-tree's node
  overhead, the allocator's rounding of each allocation up to a size class, and
  any spare capacity a `PathBuf` holds.
- It **scales with path length**, not just file count. The 193 bytes above are
  the paths that library happens to have; a library nested more deeply costs
  more per file, in exactly that proportion.

That is the price of being able to answer "which files would be removed first"
at all.

**Where it may run.** Nothing in accounting spawns a thread; the caller chooses
one, and two rules follow from AGENTS.md section 20:

- **Never on a capture path.** A recording thread must not wait on the
  filesystem. The recorder checks limits *before* a recording starts, against the
  last completed inventory.
- **Never on the thread drawing the interface.** The desktop application scans on
  a background thread and shows the previous figures until it finishes.

**How it is bounded.** `ScanOptions::with_time_budget` stops the walk once it has
spent long enough, and `scan_until` stops it when the caller's closure says so —
a user navigating away, or a process shutting down. The budget is checked before
each directory and every 256 entries within one, so a single enormous directory
cannot outrun it either. A scan that stops early produces a *partial* inventory
that says it is partial; it is never a truncated total that looks complete.

**What is incremental.** The walk is not: it is a full enumeration every time.
Incrementality comes from the other end — `StorageInventory::record_added` and
`record_removed` maintain the figures between walks, so finishing a recording
costs an insertion rather than a rescan, and the walk becomes the periodic
reconciliation that catches what happened behind the application's back. A
sensible schedule for that walk is on application start and occasionally
thereafter, not on a timer measured in seconds.

## The limits

Three, all optional, all independent, and unset means no limit rather than a
default this module invented:

| Setting | Type | Judged from |
| --- | --- | --- |
| Maximum usage | bytes | The library's own total |
| Minimum free space | bytes | The volume's free space, which everything else on the disk affects |
| Maximum recording age | duration | Each file's modification time |

Validation happens twice, deliberately. Some values are impossible on their own
and are refused by the constructors:

- A **maximum usage of zero** — or anything below 1 GB — is refused. It would put
  every library over quota from the first session, and with issue #111 behind it
  that setting empties a library. "No limit" is expressed by not setting one.
- A **maximum age of zero**, or anything under a day, is refused for the same
  reason: it would mark footage recorded this afternoon as over-age.
- A **minimum free space of zero** is accepted. It says the disk may be filled,
  which is a defensible thing to want on a drive kept for nothing else, and
  unlike a quota of zero it cannot cause a deletion on its own.

The rest are impossible only against a particular disk, which is not known when
the value is typed and may be a different disk by the time it is used. So
`StorageLimits::validate_for` is a second pass, run when the limits meet a
volume: a **minimum free space larger than the disk** can never be satisfied, and
a **maximum usage larger than the disk** can never bite, so a user who set one
believes they have a quota and does not. Both are refused with a message naming
both numbers.

Free space and volume size come from `GetDiskFreeSpaceExW`, asked of the nearest
existing ancestor of the recording location — a recording directory that has not
been created yet is an ordinary first-run state, and the question is really about
the drive.

## Breached, satisfied, or unknown

`StorageStatus::evaluate` answers what a recording about to start asks. It is
three-valued on purpose, because a limit whose state cannot be established must
not be reported as satisfied:

- **Breached** — with the numbers: what is used against the quota, what is free
  against the minimum, how many files and how many bytes are over-age.
- **Satisfied** — nothing to report.
- **Unknown**, with the reason: the measurement of the library did not finish
  (`AccountingIncomplete`), or the drive could not be read (`VolumeUnreadable`).

One asymmetry makes partial measurements more useful than they sound:
**incomplete evidence can prove a breach, but not the absence of one.** Usage
only grows as more of the library is seen, so a cancelled scan that has already
passed the quota has settled the question; the same scan finding nothing proves
nothing. Accounting reports it that way rather than throwing the finding away.

Limits are judged independently, so a drive that cannot be read makes the
free-space limit unknown while the quota is still judged from the library.

`StorageStatus` carries the measurement's completeness as well as its figures
(`StorageStatus::measurement`), and the two questions are different ones.
`is_certain` asks whether every configured *limit* could be judged; `measurement`
asks whether the *number* is a total or a floor. A scan cancelled with no limits
configured leaves nothing unknown and still under-reports usage, so a screen
rendering `used_bytes` reads the second one before deciding between "212 GB" and
"at least 212 GB".

### When the recording drive is not there

A disconnected drive is a normal state, not a bug (AGENTS.md section 16), and the
dangerous response to it is to measure zero. A root whose *volume* cannot be
reached is reported as an `UnavailableRoot`, naming the path, the category and
what the operating system said, and the inventory is marked partial. A root whose
volume is fine but whose directory does not exist yet is worth zero bytes and
leaves the measurement complete — the difference between "the drive is gone" and
"nothing has been recorded yet" is the difference between a library that must not
be acted on and one that is simply empty.

A library spread over two drives with one disconnected still measures the other
one. One unreadable root does not abandon the rest.

## What a cleanup will need, and what this deliberately does not do

Issue #111 has to answer "which files would be removed first". It cannot if
accounting keeps only a total, so the inventory keeps one entry per file — path,
category, size, modification time — and
`StorageInventory::cleanup_candidates_oldest_first` orders them the way SPEC.md
section 27 describes deletion happening, with files of unknown age *last* so
that a missing timestamp never makes something the first candidate, and with the
database, the logs and the replay buffer's disk backing left out altogether (the
category table above). `StorageInventory::cleanup_candidates_older_than` reports
how much footage is over-age without anything being deleted, which is the "review
large recordings" path one ticket early.

That is where this module stops. The protection rules — favourites, locked
recordings, recordings being edited, sources referenced by clips — are facts
about the index rather than about the disk, and they belong with the code that
deletes. Nothing here decides that a file may go, and nothing here can remove
one.

## Where the settings will live

`StorageLimits` is a validated in-memory model, not a settings file. The
configuration API with global-to-per-game inheritance is
[issue #108](https://github.com/wildware-uk/clipped/issues/108), and this type is
shaped to move into it: three independent optional limits, no invented defaults,
validation in the constructors, and a second validation pass against the volume.
When #108 exists it deserialises into these constructors, which is what keeps a
hand-edited settings file from installing a limit this module would have refused
(AGENTS.md section 30).

## What reaches the log

A storage root is `C:\Users\<account>\Videos\Clipped\Recordings`, so every path
accounting could record names the account and the folders somebody chose — the
shape [logging.md](logging.md) lists in its forbidden set. All three places a
scan logs a path (an unreadable root, a root that has not been created yet, a
directory that could not be read) record `RedactedPath`: the final component
plus a digest of the whole path, so `root=Recordings#<digest>` rather than the
path itself. Equal digests mean the same root, so a sequence of lines about one
drive can still be followed.

The full path is not thrown away — it is kept in `UnavailableRoot` inside the
inventory, where the settings screen can write "your D: drive is not connected"
from it. A path a user can see about their own machine is not a path a diagnostic
bundle should carry.

## How accounting is tested

`cargo test -p clipped-library` — no window, no GPU, no audio device, and no
installed game. The filesystem tests build a small library of their own under the
system temporary directory, named for the test, the process and the thread so
that parallel runs cannot share one, and remove it when they finish.

Two of them skip rather than fail on a machine that cannot host them, and say so
on stderr. The disconnected-drive tests need a drive letter with no volume
mounted on it. The link test needs a directory link: it tries a symbolic link
first, which needs Developer Mode or an elevated shell, and falls back to a
junction, which needs no privilege at all and is what a user who moved their
recordings to another drive is most likely to have made.

`tests/accounting_privacy.rs` covers the section above: it drives a real scan
into a real subscriber and asserts on the bytes that would have reached a log
file. Each of the three cases first asserts that the scan actually reached the
state whose log line it is checking — an unavailable root, an absent root, a
directory removed mid-walk — so that none of them can pass by the line never
being written.

The cost harness is an executable that writes thousands of files and then
removes what it wrote, so it **refuses a `--path` that already exists**. Pointed
at a real library it would otherwise have deleted it, and recordings are
irreplaceable (AGENTS.md section 56).

## The trash

SPEC.md section 28: deleting footage moves it into an application trash, a
configurable retention decides how long it stays, and it can be restored until
then. [`crates/library/src/trash`](../crates/library/src/trash) is that
([issue #94](https://github.com/wildware-uk/clipped/issues/94)).

It is the load-bearing half of M12. Everything else in this milestone either
measures the disk or removes things from it on the user's behalf, and the second
of those is only defensible because of this.

### What "in the trash" means physically

**The file is moved, on the same volume, with a rename.** It goes to
`<trash>\<when it was deleted>\<its own name>`; its row keeps existing, and
`path`, `deleted_at` and `deleted_from` say where it is now, when it went and
where it came from. That schema was written for this in
[#55](https://github.com/wildware-uk/clipped/issues/55) and reconciliation has
respected it since [#56](https://github.com/wildware-uk/clipped/issues/56): a row
in the trash is left alone, because its file being absent from where it used to
be is the expected outcome of a deletion rather than a discovery.

The decision matters because of *when* the code runs. Automatic cleanup runs on a
disk that is nearly full, which rules two of the three plausible answers out:

| Answer | Consequence on a nearly-full drive |
| --- | --- |
| Marked in place, file untouched | **Nothing is reclaimed.** A user who deletes 40 GB to make room and sees no change in free space has been lied to, and #111 deletes *to make room* |
| Copied into the trash, then unlinked | **Needs as much free space as the file**, on the one occasion when there is none, and holds two copies of a recording while a machine already in trouble decides what to do |
| **Moved with a rename** | Costs no space and no time whatever the file's size: a rename within a volume rewrites a directory entry and never touches the data |

The rename is also what makes "restored byte for byte" a property of the
filesystem rather than a promise this code makes. The bytes are never read,
copied or rewritten at any point between a delete and a restore.

Its one limitation is that a rename cannot cross a volume, so a library spread
over two drives needs a trash on each. That is refused explicitly, with a message
naming both drives, rather than silently becoming a copy. Two paths can also
share a drive letter and still be on two volumes — a directory can be a mount
point — and only the rename finds that out, so the operating system's own
`ERROR_NOT_SAME_DEVICE` is translated into the same message rather than surfacing
as "the system cannot move the file to a different disk drive".

**Each file gets a directory of its own** inside the trash, named for the moment
it was deleted (`20260812-091500`, with a counter when a second already has one).
The alternative — renaming the file itself to something unique — would lose the
name the user recognises, which is the name the trash screen has to show and the
name a restore has to put back.

The trash directory must not sit inside the recordings directory: `StorageRoots`
refuses that overlap, because a trash inside a root it measures would be walked
twice and the total would be wrong in the direction that makes a cleanup delete
more than it needed to. It also **counts towards the quota** — the category table
above says so, and the reasoning is the same one: footage in the trash still
occupies the disk.

### Retention, and what happens when it expires

The four values SPEC.md section 28 names, and no fifth:

| Setting | Kept for |
| --- | --- |
| Immediately | Expires the moment it is deleted |
| 3 days | 3 days |
| 7 days | 7 days — the default |
| 30 days | 30 days |

Seven days is the default because SPEC.md names none, and an unset setting must
not be the one that destroys. A stored number outside the four is refused rather
than rounded to the nearest, so a hand-edited settings file cannot install a
retention this code would not have offered (AGENTS.md section 30). Persisting the
choice belongs to the configuration API,
[issue #108](https://github.com/wildware-uk/clipped/issues/108), and
`Retention::from_days` is the route back in.

When retention expires, a sweep destroys the item: the file is unlinked and **the
row is deleted**. That is the only place in `clipped-library` that removes a row.
It is deliberate — an entry that can never be restored, played or acted on is not
a record of anything — and the schema's `ON DELETE` rules were written for this
moment: a clip outlives the recording it came from (`source_recording_id` becomes
`NULL`), and the session is never touched.

Three things bound it, and they are what make an automatic sweep something to
build rather than something to fear:

- **A sweep is always an explicit call.** Nothing here runs on a timer, so the
  moment footage is destroyed is a moment the application chose.
- **It reaches only rows that are already in the trash.** The list it works from
  is `WHERE deleted_at IS NOT NULL`.
- **A `deleted_at` that cannot be read never expires.** A row that was
  hand-edited or restored from a corrupt backup would otherwise be destroyed on
  the strength of a value nothing understands.

"Immediately" means *expires the instant it is deleted*, not *unlinked by the
delete itself*. There is one code path rather than two, the file is still
recoverable until the next sweep, and a user who chose the setting that keeps
nothing still gets the few minutes in which they realise.

### What restore does

| The original location | What happens |
| --- | --- |
| Free | The file goes back to it exactly, and the row with it |
| Its folder has been deleted | The folder is recreated, and the file goes back to it |
| **Occupied by another file** | The file goes back *beside* it as `name (restored).mkv`, and the outcome says it was diverted |
| On a drive that is not there | The move fails, nothing changes, and the item is still in the trash |
| The trash file has gone (emptied in Explorer) | The row is restored and reports that no file came with it |

Overwriting is never an option. Whatever is at the original location is a file
the user did not ask to lose — most often the same recording put back from a
backup — and destroying it to make room for a restore is exactly the deletion
nobody asked for. The free name is *claimed* by creating an empty file at it
rather than by asking whether one is there, because `MoveFileExW` replaces an
existing destination and a check followed by a rename would overwrite a file that
appeared in between.

Everything the user put on the item comes back with it — its favourite, its tags,
its bookmarks, the clips cut from it — because the row was never removed. That is
the whole reason deleting marks a row instead of dropping one.

One interaction is easy to get wrong and is tested for that reason. A session
sidecar still names the location a recording came from, and reconciliation
rewrites every column it is authoritative for; `path`, which now points into the
trash, has to be one it leaves alone. Ingestion therefore keeps the path of a row
in the trash exactly as it keeps a favourite (`crates/library/src/index/ingest.rs`
lists the three authorities). Without that, re-indexing would lose the only
record of where the deleted file is and no restore would work afterwards.

### The Windows Recycle Bin's role: none

Asked on [issue #103](https://github.com/wildware-uk/clipped/issues/103), where
`clipped-recorder recover --discard` faced the same question, and answered here.
Clipped never sends anything to the Recycle Bin, for reasons that get worse the
larger the file is:

- **It silently destroys large files.** The Recycle Bin has a per-volume size cap,
  around 5% of the volume by default. A file larger than the cap is *permanently
  deleted* rather than recycled. A recording is the largest file on most machines,
  so the case the Recycle Bin would be there to protect is the case it does not.
- **It evicts silently too.** Recycling one large recording can push older items
  out of the bin to stay under the cap, so deleting one thing would destroy
  another.
- **It is not everywhere.** Network shares and some removable media have no
  Recycle Bin, so a library kept on one would need this trash anyway — and two
  mechanisms that both mean "thrown away" is worse than one that is explicit.
- **Its retention is not the user's.** SPEC.md section 28 offers 3, 7 and 30 days;
  the Recycle Bin offers whatever Windows decides. A restore made from Explorer
  would also put a file back with the index still saying it was deleted.
- **It needs `IFileOperation`, a COM surface with thread affinity**, for a
  behaviour that is worse than the one it would replace.

What the Recycle Bin does keep is its place as the *user's* tool. Recordings are
ordinary files in an ordinary folder (AGENTS.md section 32), so somebody who
deletes one in Explorer gets Windows' behaviour, and Clipped's reconciliation
notices the file has gone and marks the row rather than removing it.

### Never delete anything a user did not ask to delete

Four interlocks, each in one place so that each can be reviewed:

- **Only the trash's own files can be unlinked.** One function in the crate calls
  `remove_file` on media, and it refuses a path that is not inside the trash
  directory — the path is compared component by component and case-insensitively,
  the same comparison `StorageRoots` uses, so `C:\Videos2` is not treated as
  living inside `C:\Videos`.
- **Only something already in the trash can be destroyed.** Permanent deletion
  refuses an item whose `deleted_at` is unset, so no single call can reach a
  recording the library still holds.
- **Emptying the trash is confirmed against what the user was shown.** The
  confirmation carries the count and the total size from the listing the
  dialogue quoted, and emptying refuses if the trash has changed since. A boolean
  would satisfy "requires explicit confirmation" literally and mean nothing: the
  interesting failure is not that the code forgot to ask but that the user agreed
  to destroy the twelve things they were looking at and something else arrived
  before they clicked.
- **The file wins over the row.** If the move succeeds and the database then
  refuses the change, the file is moved back before the error is returned. An
  index can be rebuilt from the session sidecars beside the recordings; a
  recording cannot be rebuilt from anything. The one state that needs a person —
  two filesystem failures in a row, so the file could not be put back either — is
  reported as an error naming both paths rather than logged and forgotten.

### A file with no row: `clipped-recorder recover --discard`

Everything above assumes a row to key off — that is what lets an item be
listed, restored and swept by retention. `recover --discard`
([issue #451](https://github.com/wildware-uk/clipped/issues/451)) hands the
trash a fragment an interrupted recorder left, and that has no row: the library
only indexes a recording once its session record says it is finished, which is
the thing discarding is in the middle of doing. Giving it one purely so it
could be trashed would mean a delete command doing the library's indexing —
not this crate's job, and not free, since the sidecar outcome recovery writes
is not yet a word the indexer recognises
([issue #278](https://github.com/wildware-uk/clipped/issues/278)) — a separate
gap this does not paper over.

So `Trash::stow_untracked` does only the physical half: the same rename, into
the same trash directory, under the same cross-volume refusal as an ordinary
delete. What it does not do is give the file a row, and that is a real cost
stated plainly rather than hidden — it will not appear on the trash screen, is
not counted towards what emptying the trash would reclaim, and is not swept by
retention, because there is no `deleted_at` for retention to be judged from.
The file is on disk, in the trash directory, byte for byte, which is the one
guarantee that makes a mistaken `--discard` recoverable at all; getting the
rest of the bookkeeping is future work, not something this quietly promises.
[recorder-cli.md](recorder-cli.md#recover) is what a person running the command
is told about the difference.

### Where the trash runs

Synchronously, on a thread the caller owns, and **never a capture thread**: a
rename is a filesystem call (AGENTS.md section 20). Every database statement is a
single one, so each is its own transaction and the database's one writer is never
held for longer than a row update — a sweep of a hundred expired items is a
hundred short writes rather than one long one, and a recorder with something to
write waits for at most a row.

Every path the trash logs goes through `RedactedPath`, for the reason
[logging.md](logging.md) gives: a recording's path names the account and the
folders somebody chose.

### How the trash is tested

`cargo test -p clipped-library` — no window, no GPU, no audio device, no
installed game and no waiting. The libraries are built under the system temporary
directory by the **real indexer** from a real session sidecar, so the rows the
operations act on are the rows a running Clipped would have.

Three properties are worth naming because they are what stop these tests being
decorative:

- **Retention is tested with controlled timestamps.** Both the moment of deletion
  and "now" are values the test chooses, so a seven-day retention is exercised at
  six days and at eight without a second of waiting (AGENTS.md section 25).
- **"Byte for byte" is a real comparison.** The recordings are written with
  pseudo-random content rather than zeroes, so two files of the same length do
  not compare equal by accident.
- **"It still plays" is `ffprobe`'s answer, not this code's.** One test writes a
  real Matroska file with the pinned FFmpeg build, deletes it, restores it, and
  hands the result to `clipped-media-validation` (AGENTS.md section 22). It skips
  cleanly on a checkout with no `ffmpeg.exe`, and `CLIPPED_REQUIRE_MEDIA` turns
  that skip into a failure, which is how CI is configured.

The compensating move — the file being put back when the index refuses the change
— is provoked rather than described: the test opens the database read-only, which
answers the query and refuses the update, and then asserts that the recording is
back where it was with the same bytes in it.
