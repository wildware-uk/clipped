# Storage management

Clipped fills disks. A session of 1080p60 is around 8 GB an hour, an evening of
play is a few of those, and the whole point of an automatic recorder is that
nobody is deciding, per session, whether to keep it. So SPEC.md section 27 makes
storage a product feature: a user configures a maximum size, a minimum amount of
free disk space and a maximum recording age, and expects the figures they are
shown to be the truth about their own disk.

This document covers the part of that which exists today: **storage
accounting** — measuring what the library occupies, attributing it, and saying
whether the configured limits are met. It lives in
[`crates/library/src/accounting`](../crates/library/src/accounting)
([issue #93](https://github.com/wildware-uk/clipped/issues/93)).

**Nothing described here deletes anything.** That is deliberate and it is the
line the module is built around. Acting on a breached limit is
[issue #111](https://github.com/wildware-uk/clipped/issues/111), the trash and
its retention are [issue #94](https://github.com/wildware-uk/clipped/issues/94),
and the screen that shows all of it is
[issue #95](https://github.com/wildware-uk/clipped/issues/95). Recordings are
irreplaceable (AGENTS.md section 56), so the code that measures them and the code
that removes them are different modules in different tickets, and the measuring
one has no capability to delete.

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

## How it is tested

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
