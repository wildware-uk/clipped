# Thumbnails

Every screen that lists a recording needs a picture for it: the library grid,
the game page, the clip list and search results (SPEC.md sections 22 and 30).
This document is the prose for `crates/library/src/thumbnail` — which frame is
chosen and why, what is stored and where, how a stale or missing thumbnail
behaves, and what generating one actually costs.

Issue: [#57](https://github.com/wildware-uk/clipped/issues/57). Written in
milestone M6.

## What exists today

The generator, the cache and the background worker exist and are tested.
**Nothing draws the result.** The library screen is
[#52](https://github.com/wildware-uk/clipped/issues/52) and no process hosts the
worker yet, so on a real machine no thumbnail is generated until one of those
lands. This document describes behaviour that is written and covered by
`crates/library/tests/thumbnails.rs`, and says so where it does not.

## Which frame

**Not the first one.** The first frame of a Clipped recording is the moment
capture attached to the game's window: a black fade, a loading screen, an
anti-cheat splash or a publisher logo far more often than it is the game. A
library of black tiles is a library nobody can scan visually, which is the whole
job a thumbnail has.

The rule, in `src/thumbnail/choose.rs`:

1. Look at up to three places in the recording — 10%, 35% and 60% of its
   duration. Never the first moment, never past the last tenth.
2. At each, seek **backwards** to the nearest keyframe and decode the frame
   there. Take up to four frames at one candidate if the first is blank.
3. Score each frame by the **variety** in its luma: one minus the share of
   sampled pixels that fall in the largest of sixteen brightness bins.
4. Keep the highest-scoring frame. Stop early once one scores 0.35 or better,
   which any ordinary frame of a game does.

A black loading screen puts every sample in one bin and scores 0. A fade to
white does the same. A publisher logo on black scores near 0 because 99% of the
frame is still one colour. A night scene in a game scores well, because the rule
is variety and not brightness — a rule that preferred bright frames would take a
grey menu over a dark firefight.

### What that costs, and what was rejected

The cost is up to three seeks and, in the ordinary case, **one decoded frame**;
the bound is twelve. That is what makes it affordable to do this for a whole
library. Measured figures are below.

Rejected, and why:

| Alternative | Why not |
| --- | --- |
| The first frame | The failure above. It is free and it is usually wrong. |
| A fixed offset, e.g. 30 seconds in | A 20-second replay clip has no 30-second mark, and a game with a two-minute loading screen still gets the loading screen. Fractions of the duration scale to both. |
| Scene-change detection, motion analysis | Needs continuous decoding of a large part of the file, which is the cost this design exists to avoid — hundreds of frames rather than one. |
| Face or object detection | A model, a dependency and an order of magnitude more processor, to choose between frames that all cost 20 kB. |
| Letting the user pick | Worth having later as an override (no issue yet), but a library only becomes browsable if the default is good without being asked. |

Sampling is over a fixed grid of about 4,096 pixels rather than the whole plane,
so scoring a 4K frame costs the same as scoring a 720p one.

The frame is scaled to thumbnail size **before** it is scored. That costs about
a millisecond a frame and removes a branch per pixel format — a recording may
decode to 8-bit 4:2:0, 10-bit 4:2:0 for an HDR capture, or RGB for a file from
elsewhere — and it has the useful property that what is judged is exactly the
picture that becomes the thumbnail.

## Size and format

**One stored size: 640 pixels wide**, height from the recording's own aspect
ratio, both rounded to even numbers because 4:2:0 chroma is shared between pairs
of pixels. A recording narrower than 640 is never enlarged.

A library tile is drawn at about 320 logical pixels, which is 640 device pixels
on the 200%-scaled displays most gaming machines run, so 640 is the smallest
width that is sharp where a thumbnail is actually shown.

**Is one size enough?** Measured on the recording in the table below, at the
default quality:

| Width | Height | Bytes | Per 10,000 recordings |
| --- | --- | --- | --- |
| 320 | 180 | 6,698 | 64 MB |
| **640** | **360** | **20,073** | **191 MB** |
| 1,280 | 720 | 126,108 | 1.2 GB |

A second stored size would cost almost nothing in *time* — the frame is decoded
once, and a second scale and encode is a few milliseconds — so the decision is
about bytes and about cleanup, not about speed. 640 alone serves the one place a
thumbnail is drawn at size; 320 as well would save nothing worth a second file
per recording to invalidate and prune, and 1,280 is six times the disk for a
picture no screen shows at that size. If a screen later needs a large hero image,
`ThumbnailOptions::with_width` already makes one, and the entry records the size
it was made at.

**JPEG**, at MJPEG quantiser scale 4 (the scale runs 1–31, lower is better).
A frame of a game is photographic: PNG is roughly ten times the bytes for a
difference nobody can see at this size. JPEG is the one format every webview,
file manager and image viewer on Windows opens without being asked, which matters
because these are ordinary files in a user's own directory.

**WebP is smaller and is available**, which this page used to deny
([#453](https://github.com/wildware-uk/clipped/issues/453)): the pinned build
carries `libwebp`, screenshot capture already writes it, and
`tests/capture/screenshot.rs` decodes the pattern back out of a 280-byte lossless
WebP against JPEG's 5,680 for the same frame. So the reason for JPEG is the one
above — universal support — and not availability. Whether that still wins for a
thumbnail nobody opens outside Clipped is a fair question; it should be argued
against the real options. **AVIF genuinely is unavailable**: the pinned build
lists no AVIF encoder.

Colour is handled explicitly in both halves: `swscale` is told the source's range
and matrix and asked for full-range output, and the encoder is told the picture
is full range. The two disagreeing produces thumbnails that are slightly grey
with nothing anywhere reporting a problem — the same silent failure
[muxing.md](muxing.md) and `crates/encoder`'s converter document.

## Where they live

A directory of files under Clipped's per-user directory, **not** in the database:

```text
%LOCALAPPDATA%\Clipped\thumbnails\
    <key>.jpg     the picture
    <key>.json    what it is, and which recording it came from
```

`<key>` is a 16-character hexadecimal digest of the recording's path,
lower-cased on Windows because two spellings of a path name one file. It is a
digest of the path and not of the contents, so a lookup costs one `stat` and one
small file read rather than hashing a two-gigabyte recording.

### Why not the database

AGENTS.md section 31 forbids media blobs in SQLite and
[#55](https://github.com/wildware-uk/clipped/issues/55)'s schema deliberately has
no BLOB column, so the picture is a file whatever else happens. That leaves where
the *bookkeeping* goes, and it is the sidecar rather than a `thumbnail_path`
column on `recordings`:

- A column needs a migration in `clipped-storage`, whose migrations are
  append-only and released ([storage.md](storage.md)), for data that is derived
  and disposable.
- The cache is keyed on the recording's own path, which the index already holds
  ([library.md](library.md)). Nothing has to be joined, and a library rebuilt
  from nothing — which `clipped_library::index` can do — still finds every
  thumbnail it had.
- A user who deletes the database keeps their thumbnails; a user who deletes the
  thumbnails keeps their library. Neither half can damage the other.

That makes this a documented sidecar format, which is what AGENTS.md section 32
asks of application metadata that is not in SQLite.

Thumbnails are already the `Thumbnails` category in storage accounting
([storage-management.md](storage-management.md)), which is marked *regenerable*:
losing one costs the tens of milliseconds below and nothing else.

### The sidecar

```json
{
  "version": 1,
  "recording": "D:\\clips\\Counter-Strike 2\\cs2-20260812-193000-1.mkv",
  "size_bytes": 1288490188,
  "modified_nanos": 1786467000000000000,
  "image": {
    "file": "3f2a91c40be15d77.jpg",
    "width": 640,
    "height": 360,
    "at_seconds": 184.5,
    "blank": false
  }
}
```

An entry may instead record that a recording produced no thumbnail:

```json
{
  "version": 1,
  "recording": "D:\\clips\\Counter-Strike 2\\truncated.mkv",
  "size_bytes": 25,
  "modified_nanos": 1786467000000000000,
  "failure": "could not decode a frame of <redacted> for a thumbnail: the container could not be opened"
}
```

That second kind matters as much as the first. Without it, a recording that
cannot be decoded misses on every lookup, every miss asks for another attempt,
and a library screen redraw becomes a seek and a decode per broken tile, for
ever.

Fields are added at the end and read back as optional, so a sidecar written by an
earlier build of the same version still reads (AGENTS.md section 43). A sidecar
whose `version` is not 1 is treated as a **miss**, not as an error: the thumbnail
is made again, which costs milliseconds, and nothing guesses at a format it does
not know.

### Invalidation

The sidecar names the recording it was made from — path, length and modification
time. A lookup compares that against the file on disk *now*, so a recording that
was trimmed, re-encoded or replaced never shows its previous picture. There is no
separate invalidation step to forget to run, and a remembered failure stops
applying the moment the file changes, so a repaired recording is attempted again.

Not a content hash: hashing a two-gigabyte recording to decide whether to redraw
a 20 kB picture would cost far more than making the picture did, and the failure
this has to catch — a file replaced or rewritten — changes at least one of the
three fields.

### Cleanup

`ThumbnailCache::prune` deletes, in this order:

1. Entries whose recording no longer exists, and the pictures beside them. A
   library where the user deletes clips would otherwise accumulate pictures of
   files that are gone.
2. Half-written files an interrupted store left behind. Nothing else ever
   deletes one: a lookup only opens `<key>.json` and `<key>.jpg`, so an
   abandoned `<key>.jpg.writing` is invisible to every other path in the module.
3. Pictures with no sidecar to say what they are.
4. The least recently *written* entries, until the directory is inside its byte
   budget — 256 MB by default, which at 20 kB a picture is about 13,000
   recordings.

"Least recently written" rather than least recently used: recording a use would
mean writing to the directory every time a library screen was drawn. The cost of
getting the order slightly wrong is regenerating a thumbnail, which is the
cheapest mistake in this module.

Pruning is **not** automatic. It is a call the host makes when it has time — the
same place it decides to index the library — because deleting files is not
something a lookup should do behind a caller's back.
`ThumbnailCache::forget(path)` is the immediate answer for a caller that already
knows a recording has gone.

A store writes each file through a temporary and a rename, and finishes the
picture **before** the sidecar that describes it. A process killed mid-store
therefore leaves either the previous entry or a picture no lookup will read and
pruning will collect — never a sidecar pointing at a picture that is not there.

## Missing data is not an error

Issue #57's third acceptance criterion: a failure to generate a thumbnail leaves
the recording usable. `ThumbnailState` has exactly three answers, and none of
them is an error a screen has to handle:

| State | What a screen does |
| --- | --- |
| `Pending` | Draw the tile with no picture. One is being made. |
| `Ready(Thumbnail)` | Draw `image_path()`. |
| `Unavailable(error)` | Draw the tile with no picture, and it may say why. |

Every one of these is reached by a real case:

| What happened | Answer |
| --- | --- |
| Never made | `Pending` |
| The recording was trimmed or replaced | `Pending` — the entry no longer matches |
| Somebody deleted the picture but not the sidecar | `Pending` |
| The sidecar is corrupt, or from another build | `Pending` — and a corrupt one is deleted on the spot |
| The recording is gone from the disk | `Unavailable` — it cannot be stat-ed |
| The recording holds no video stream | `Unavailable` — an audio-only file has no frame to show |
| The container cannot be opened at all | `Unavailable`, and remembered so it is not retried per tile |

A recording whose every candidate frame is a flat colour still gets a picture,
marked `is_blank()`. That is what the recording looks like; a screen may show it
or fall back to its no-picture tile, and both are honest. Inventing something
else would be fake data (AGENTS.md section 27).

Nothing in this module ever writes to a recording. A failed thumbnail leaves the
file byte-for-byte as it was, which
`a_recording_that_cannot_be_decoded_leaves_the_recording_usable` asserts rather
than assumes.

## Where it runs

On **one** thread that `ThumbnailService` creates and owns, and nowhere else.

- One, not a pool. The work is a seek and a handful of decoded frames; a second
  thread would double the disk queue depth of a job whose whole purpose is to
  stay out of the way.
- Created by the service rather than borrowed from the caller, so the priority
  below applies to a thread this crate owns (AGENTS.md section 20).

The intended host is the recorder process, which is the process that already
knows when a recording is running and can therefore suspend generation
truthfully. Nothing hosts it yet.

### What bounds it

| Bound | Value | Why |
| --- | --- | --- |
| Threads | 1 | above |
| Queue | 128 paths | a library scan must not become an unbounded allocation; the **oldest** waiting request is dropped when it is full, because the newest is what somebody just scrolled to |
| Work per recording | 3 seeks, at most 12 decoded frames, 1 JPEG | `src/thumbnail/choose.rs` |
| Packets per candidate | 512 | a container whose index is wrong must not turn one candidate into a full read of the file |
| Thread priority | `THREAD_PRIORITY_LOWEST` | run only when nothing else wants the processor |
| I/O priority | `THREAD_MODE_BACKGROUND_BEGIN` | reads must not take disk bandwidth from a recording |
| Suspension | while a recording is running | below |

### Suspension is the "deferred" half

Priority is not the whole answer, because a background-priority thread still runs
when a game is waiting on something other than the processor. So the service can
also be **suspended**: while it is, the worker stops between packets and does not
resume until it is told to. A host calls `suspend_for_recording()` when a
recording starts and `resume()` when it ends.

Suspension is a real stop, not a hint. The worker blocks on a condition variable,
`is_suspended()` reports it, and shutting down while suspended breaks the wait
rather than deadlocking behind it — which is the case a host hits every time it
quits during a recording.

### Proving the priority took

`WorkerPriority` is read back from `GetThreadPriority` rather than inferred from
the fact that `SetThreadPriority` was called, because "we asked for background
priority" and "the thread is running at background priority" are different
statements and only the second is worth anything. A control that silently does
nothing is worse than no control (AGENTS.md section 27).

`the_worker_runs_at_the_lowest_priority_windows_will_give_it` asserts it: the
scheduling priority took, background I/O mode was entered, and the priority
Windows reports afterwards is at or below `THREAD_PRIORITY_LOWEST` (−4 on this
build, because background mode is lower still).

## Measurements

Taken by `crates/library/tests/thumbnails.rs`, which runs on every `cargo test`
and prints the figures with `--nocapture`. Measured on:

| | |
| --- | --- |
| Machine | AMD Ryzen 9 9950X3D, 16 cores, 62 GB, Crucial P5 Plus NVMe SSD |
| Operating system | Windows 11 Pro, 10.0.26200 |
| File cache | warm |
| Date | 2026-08-12 |

### The ordinary case

| | |
| --- | --- |
| Workload | 12 s of 1920×1080 30 fps H.264 at 20,000 kb/s in Matroska, 29.4 MB |
| Content | a test pattern with noise over it, so nothing compresses away |
| Outcome | a 640×360 JPEG of 20,073 bytes, from the frame at 1.1 s |

| Build | Per thumbnail |
| --- | --- |
| `--release` | 45–112 ms (5 runs, median ~50 ms) |
| debug | 44–47 ms (4 runs) |

The two builds are the same because essentially all of this time is inside
FFmpeg's own libraries, which are the pinned prebuilt ones either way; the Rust
in this module is a histogram over 4,096 bytes. The spread in the release column
is a machine running other builds at the same time, not variance in the work.

The first candidate satisfied the rule here — the noise pattern has variety
everywhere — so this figure is one seek and one decoded frame, which is what a
recording of a game normally costs.

### When every early candidate is blank

| | |
| --- | --- |
| Workload | 12 s of 1280×720 30 fps H.264, black for its first 6 s and a test pattern after |
| Outcome | the frame at 7.0 s, from the third candidate |

| Build | Per thumbnail |
| --- | --- |
| `--release` | 21–26 ms (2 runs) |
| debug | 11–13 ms (3 runs) |

Three seeks and several decoded frames, and still cheaper than the row above —
because this file is 720p at a fraction of the bitrate, and the cost of a
thumbnail is set by the size of the pictures being decoded and the bytes between
the seek and them, not by how many candidates were looked at.

### What that means for a library

At roughly 50 ms a recording on this hardware, a library of 1,000 recordings is
under a minute of a background thread, and a library of 10,000 is about eight
minutes — spread over as long as the user's games leave the machine idle, since
the worker is suspended for every recording in the meantime. Contrast the same
job at whole-file speed: generating a waveform for those same recordings is
seconds each ([waveforms.md](waveforms.md)), because a waveform has to read the
whole file and a thumbnail seeks.

The figures are with the file in the operating system's cache. A cold library is
bounded by the disk instead, which is exactly what background I/O priority is
there for.

**Not measured here:** the effect on a game's frame times while generation runs.
That needs a game, a GPU and a machine to itself, so it is a manual measurement
rather than a test. What is asserted instead is the second half of #57's
criterion — that generation is *deferred*: the worker is suspended while a
recording is running, and
`suspending_generation_stops_the_worker_and_resuming_lets_it_finish` fails if
suspension is a flag nobody reads.

## Where the code is

| | |
| --- | --- |
| Module | `crates/library/src/thumbnail` |
| Choosing a frame | `choose.rs` |
| Decoding, scaling and encoding | `render.rs` |
| The cache | `cache.rs` |
| The background worker | `service.rs` |
| Thread priority | `windows/priority.rs` |
| Tests | `crates/library/tests/thumbnails.rs`, and unit tests in each module |

```text
cargo test -p clipped-library
```

The end-to-end tests need `ffmpeg.exe` to write their subject recordings, and
skip cleanly without it (`scripts/fetch-ffmpeg.ps1` installs the pinned build).
`CLIPPED_REQUIRE_MEDIA` turns that skip into a failure, which is how CI is
configured.

### Why this lives in `clipped-library`

A thumbnail is a property of a library item, and this crate already owns what one
costs on disk. `clipped-library` is layer 1, so it may name `rusty_ffmpeg`
directly: `clipped-muxer` owns the workspace's safe wrappers over the container
API and sits at layer 2, *above* this crate, so depending on it would invert the
direction `tests/integration/tests/workspace_layering.rs` asserts.
[ADR 0004](adr/0004-ffmpeg-dependency-strategy.md) permits exactly that case, and
`clipped-encoder` and `clipped-waveform` are the other two crates that use the
allowance.

It has a cost worth stating plainly: **every dependent of `clipped-library` now
links FFmpeg**, and imports `avformat`, `avcodec`, `avutil` and `swscale` before
`main` runs. Nothing depends on this crate today, and the recorder already links
all four through the muxer and the encoder, so nothing new is shipped — but a
future crate that wants only the search parser pays for a decoder it will not
call.

The alternative is a `clipped-thumbnails` crate beside `clipped-waveform`, which
is the shape [#66](https://github.com/wildware-uk/clipped/issues/66) chose for the
same kind of work. That was not done here because creating a crate means editing
the layer table, the layering test and the architecture document — shared files
that several M6 tickets were touching in the same week — and because it is worth
deciding together with the duplication it would fix:
[#293](https://github.com/wildware-uk/clipped/issues/293) covers extracting the
source identity, the background worker and the thread-priority calls that this
module and `clipped-waveform` now each have their own copy of, and the crate
placement is a question on it.
