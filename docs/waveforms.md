# Audio waveforms

The timeline draws a waveform under a recording (SPEC.md section 18) and the
clip editor shows one row per audio track (SPEC.md section 19). This is where
the numbers behind those drawings come from: `crates/waveform` reads a finished
recording, decodes each of its audio tracks and reduces them to peaks.

```text
VIDEO        █████████████████

GAME         ▃▅▆▃▇▆▄▅▄▄▅▇▅▄

MIC          ▁▁▃▇▅▁▁▃▆▁▁▁▅▁

DISCORD      ▂▂▁▅▁▁▇▂▁▅▃▁▁▁
```

Nothing here opens an audio device, a capture session or an encoder. It reads
files that have already been written, which is what makes it safe to run at all
while a game is being recorded — subject to priority, which is
["Where it runs"](#where-it-runs) below.

## What a waveform is

For each audio track, and for each slice of time, how far the signal went in
each direction:

| Field | Type | Meaning |
| --- | --- | --- |
| `minimum` | `i8` | the lowest sample in the slice, scaled to ±127 |
| `maximum` | `i8` | the highest sample in the slice, scaled to ±127 |

Minimum *and* maximum rather than one magnitude, because asymmetric audio is a
real thing and drawing it as a mirror image is a lie about the recording.

Quantising to eight bits is a decision about the **drawing**, not about the
audio. The editor mock-up in SPEC.md section 19 gives an audio row on the order
of a hundred pixels of height, and at 128 steps per direction the quantisation
error is under half a pixel at that size. Rounding is **outwards** — minima down,
maxima up — so a drawn waveform is never smaller than the audio it came from,
which matters when somebody is hunting for the quiet start of a sound to cut on.

### Resolution

The base resolution is **10 milliseconds** per bucket. Above it sits a pyramid:
each level merges two buckets of the level below, down to a level of at most 128
buckets.

```text
level 0    10 ms     6,000 buckets per minute
level 1    20 ms     3,000
level 2    40 ms     1,500
 ...
           total   < 12,000 buckets per minute per track
```

**Why 10 ms.** A waveform is drawn at some pixel width. At 1920 pixels, a
10 millisecond bucket covers 19.2 seconds of audio at one bucket per pixel, which
is past the working zoom of a trim editor; and it is finer than one frame of 60
fps video, so a cut aligned to a video frame can always be placed against a
bucket boundary. Zooming closer than that stretches each bucket over more than
one pixel rather than showing more detail. Halving the bucket would double
everything in the next section; the base bucket is written into each cache entry,
so changing it later is a format version rather than a cache that silently means
something else.

**Why a pyramid rather than one resolution.** Storing one resolution makes
zooming out either wrong or slow: a 200-pixel overview of a three-hour recording
means reading and reducing 1.08 million base buckets every time the view moves.
Merging minima and maxima is exact — the maximum of two maxima *is* the maximum
of the union — so a coarse level is not an approximation, it is the same answer
on a coarser grid. A geometric series costs exactly double and no more.

### What it costs in bytes

Two bytes per bucket, so **under 24 kB per minute per audio track**, of which
12 kB is level 0.

| Recording | Tracks | Cache |
| --- | --- | --- |
| 1 minute | 1 | 24 kB |
| 1 hour | 1 | 1.4 MB |
| 1 hour | 3 (game, microphone, other system audio) | 4.2 MB |
| 100 hours | 3 | 420 MB |

The default cache budget is 512 MB, which is roughly 120 hours of three-track
recording.

## Multi-track

A recording has several audio tracks by design — game, other system audio,
microphone and voice chat are separate tracks (SPEC.md section 11, issue #28) —
so a waveform is a **list** of tracks and nothing assumes how many. Each carries
its own peaks, its own sample rate and channel count, its container stream index,
and its name from the container's `title` tag (or its language, when a file from
elsewhere carries only that).

Channels within a track are **merged**, not averaged: a sound panned hard to one
side is as visible in the waveform as one in the middle. Averaging would halve
it, and reading only the first channel would lose it entirely.

Zero tracks is a supported answer. Recordings Clipped writes today have no audio
at all — multi-track audio is issue #180 — and such a file produces a waveform
with no tracks rather than an error.

## Missing data is not an error

`WaveformState` has three cases and every one of them is something a timeline can
draw:

| State | Means | Reached when | The timeline shows |
| --- | --- | --- | --- |
| `Ready` | the peaks are here | an entry matches the recording | the waveform |
| `Pending` | not generated yet; it has been requested | there is no entry, or it belongs to an older version of the file | the track with no waveform in it |
| `Unavailable` | there will not be one, and why | the recording cannot be stat-ed, or an earlier analysis read it and got nothing | the same, plus a diagnostic in the log |

`tracks()` is empty for all but `Ready`, so a caller that draws a row per track
needs no branch at all. "Not generated yet" is the ordinary state of a recording
that has just been written; treating it as an error would put a banner over every
new recording.

The second route to `Unavailable` is the one that matters for load. A recording
that cannot be decoded — truncated by a crash, in an audio codec this build has
no decoder for, or longer than the 8-hour bound — would otherwise miss the cache
on every lookup, and every miss asks for the whole file to be read and demuxed
again. So a failed analysis is **written down**, as an entry that holds the
reason instead of peaks. It is invalidated like any other entry: repair or
replace the recording and it is analysed again.

The same applies inside a file. A track in a sample format this build cannot read,
or whose codec has no decoder in the pinned FFmpeg, is left out with a warning
naming the codec — not included as a flat line, which would be indistinguishable
from a silent track.

## The cache

Peaks are derived data. They can always be recomputed from the recording, so
losing them costs time and nothing else, and nothing in the cache is treated as
precious: an entry that will not read is deleted and regenerated, an entry whose
recording has changed is overwritten, and the whole directory can be deleted
while Clipped is running.

### Where

```text
%LOCALAPPDATA%\Clipped\waveforms\<digest>.cwf
```

`<digest>` is a 64-bit digest of the recording's path (lower-cased, because two
spellings of a Windows path name the same file). It is not cryptographic and does
not need to be: the entry carries the whole source identity and a lookup checks
it, so a digest collision costs a recomputation rather than showing the wrong
waveform.

### What reaches the log

Nothing that names a directory. Recording paths and cache paths are logged as
`RedactedPath` — the final component and a digest of the whole path — so neither
the account name in `%LOCALAPPDATA%` nor the folders somebody chose for their
library reach a log file that gets attached to a bug report (AGENTS.md section
13, [logging.md](logging.md), "Privacy").

That applies to error messages too, because they are logged: `WaveformError`
holds no `Path`, and the one free-text field it has is a `&'static str`, so a
path cannot be formatted into it. `crates/waveform/tests/privacy.rs` renders the
crate's own log lines through a real subscriber and fails if a directory
component appears in one — `crates/logging`'s own privacy tests explicitly do not
cover hand-written call sites, which is where this went wrong first.

### Why not SQLite

AGENTS.md section 32 allows application metadata in SQLite, in a documented
sidecar format, or in the container. This is a documented sidecar in a cache
directory, for three reasons in order of weight:

1. Section 31 says not to store large media blobs in SQLite, and a three-track
   hour is about 4 MB of peaks.
2. The database holds things that cannot be recovered — bookmarks, favourites,
   per-game settings. Mixing throwaway data into it means every backup, migration
   and integrity check carries the throwaway data too.
3. It does not need the database to exist. The schema is issue #55, in progress
   as this is written.

**How it would move.** If peaks ever belong in SQLite, the *index* moves and the
payload does not: a `waveform` table keyed on recording id, holding the source
identity and the entry's file name, replaces "find the file named after the digest
of the path". The bytes stay a file, because of section 31. Nothing else in the
crate changes, which is why the identity is written into the entry rather than
being implied by where the entry is.

### Invalidation

An entry records the recording it was computed from: **path, length and
modification time**. A lookup compares that against the file on disk now, so a
recording that was trimmed, re-encoded or replaced does not show its previous
waveform. There is no separate invalidation step to forget to run.

Not a content hash: hashing a two-gigabyte recording to decide whether to redraw
a waveform would cost more than generating the waveform did, and the failure this
has to catch is a file replaced or rewritten in place, which changes at least one
of the three.

### Cleanup

`WaveformCache::prune` does three things, in this order:

1. Deletes entries whose recording no longer exists.
2. Deletes temporaries an interrupted store left behind.
3. Deletes the least recently **written** entries until the directory is inside
   its byte budget.

Least recently written rather than least recently *used*, because recording a use
means writing to the entry every time a waveform is drawn, and a timeline that
scrolls would do that many times a second. The cost of getting the order slightly
wrong is regenerating a waveform.

Pruning is not automatic. It is a call the host process makes when it has time,
because deleting files is not something a lookup should do behind a caller's
back.

### The `.cwf` format

Little-endian throughout. `crates/waveform/src/format.rs` is the implementation
and the two are changed together.

```text
offset  size  field
0       8     magic, "CLIPWAVE"
8       2     format version (currently 1)
10      2     flags; bit 0 is "no waveform" (below), the rest are unused
              and a reader ignores them
12      2     track count
14      4     length of the recording's path, in bytes
18      8     the recording's length in bytes when it was analysed
26      8     its modification time, nanoseconds since the Unix epoch,
              signed; i64::MIN means the filesystem reported none
34      ...   the recording's path, UTF-8

then, when flag bit 0 is set, and nothing else:
        2     length of the reason, in bytes (at most 1,024)
        ...   the reason, UTF-8

then, per track:
        4     container stream index
        4     sample rate, Hz
        2     channels
        2     length of the track's name, in bytes
        ...   the track's name, UTF-8 (empty when the container gave none)
        8     the track's duration in nanoseconds
        2     level count

then, per level:
        8     bucket duration in nanoseconds
        4     bucket count
        ...   bucket count × (minimum: i8, maximum: i8)
```

Flag bit 0 is why one entry covers both "here are the peaks" and "there are
none, and this is why": everything above the flag is identical for the two, so
invalidation, pruning and the byte budget work on either without knowing which
they found. A "no waveform" entry declares zero tracks and carries a reason
instead of levels; a recording that genuinely has no audio declares zero tracks
with the flag clear, and the two are therefore never confused.

A reader refuses a version it does not know, refuses counts larger than it will
allocate for, and refuses a file that ends part way through any field. All three
are a cache miss and a recomputation, never an error a user sees.

Entries are written to `<digest>.cwf.writing` in the same directory and renamed
over `<digest>.cwf`, so a process that dies mid-write leaves either the previous
entry or none — never a half-written one a lookup has to detect. A process
killed between the write and the rename does leave the temporary behind, and
`prune` sweeps those: nothing else can, because no lookup will ever open a file
that is not named `.cwf`.

## Where it runs

On **one** thread that `WaveformService` creates and owns.

- One, not a pool. The work is bounded by disk at least as much as by processor,
  and a second thread would double the disk queue depth for a job whose whole
  purpose is to stay out of the way.
- Created by the service rather than borrowed, so the priority below applies to a
  thread this crate owns and never to a caller's (AGENTS.md section 20).

The intended host is the recorder process, which is the process that already
knows when a recording is running and can therefore suspend generation
truthfully, and **it does host it**: `LibraryIndexer::for_this_user` starts the
service beside the index and asks for a waveform per recording after each
reconciliation (issue #293). What it does not yet do is *suspend* it — nothing
in `apps/recorder` calls `suspend_for_recording` or `resume` — so the half of
the promise that is kept today is the thread and I/O priority below.

### What bounds it

| Bound | Value | Why |
| --- | --- | --- |
| Threads | 1 | above |
| Queue | 64 recordings | a library scan must not become an unbounded allocation |
| Thread priority | `THREAD_PRIORITY_LOWEST` | run only when nothing else wants the processor |
| I/O priority | `THREAD_MODE_BACKGROUND_BEGIN` | reads must not take disk bandwidth from a recording |
| Memory | one accumulator set | 8 bytes per 10 ms of audio while a file is being read, freed when it finishes |
| Audio length | 8 hours | bounds the above; longer is refused with a reason |
| Suspension | while a recording is running | below |

When the queue is full the **oldest** waiting request is dropped and the caller is
told which one: the newest is the recording somebody just looked at, and a
dropped request costs nothing, because asking again is one call and the peaks
were never there to lose.

`THREAD_MODE_BACKGROUND_BEGIN` is the important half. Summarising a recording
means reading the whole file — including its video packets, because audio is
interleaved with video and there is no way to reach the last second of one
without reading past the last second of the other — and a recording in progress
is writing to the same disk. A low-priority thread issuing normal-priority reads
would still take disk bandwidth from the recorder. Background I/O priority is
what Windows gives its own indexer, for the same reason.

### Suspension

Priority is not the whole answer, because a background-priority thread still runs
when a game is waiting on something other than the processor. So the service can
also be **suspended**: while it is, the worker stops between packets — within 64
container packets, a few milliseconds of audio — and does not resume until it is
told to. A file part way through is paused, not abandoned.

A host suspends when a recording starts and resumes when it ends. This is a real
stop: the worker blocks on a condition variable, `is_suspended` reports it, and
`crates/waveform/tests/background.rs` asserts that nothing is produced while it
holds.

### Proving the priority took

`WaveformService::worker_priority` reports what `GetThreadPriority` says, read
back from Windows rather than inferred from the fact that `SetThreadPriority` was
called. "We asked for background priority" and "the thread is running at
background priority" are different statements and only the second is worth
anything (AGENTS.md section 27).

## Measurements

Taken by `crates/waveform/tests/cost.rs`, which runs on every `cargo test` and
prints the figures with `--nocapture`. Both were measured on:

| | |
| --- | --- |
| Machine | AMD Ryzen 9 9950X3D, 16 cores, 62 GB |
| File cache | warm |
| Date | 2026-08-12 |

### Audio decode alone

| | |
| --- | --- |
| Workload | 60 s of 48 kHz 16-bit stereo PCM in a WAV container, 11.0 MB |
| Content | a tone at 0.9 full scale, silence, a tone at 0.3, 20 s each |

| Build | Per minute of audio | Faster than real time by |
| --- | --- | --- |
| `--release` | 25–26 ms (5 runs) | ~2,400× |
| debug | 210–219 ms (5 runs) | ~280× |

This is the cost of decoding audio and accumulating peaks and **nothing else**.
No video, and no compression: raw PCM is copied out of the container rather than
decoded. It is not what summarising a recording costs, and the figure must not
be extrapolated to one — the row below is what that costs.

### A container shaped like a recording

| | |
| --- | --- |
| Workload | 10 s of 1280×720 30 fps H.264 at 20,000 kb/s, plus 3 AAC tracks at 160 kb/s, in Matroska, 24.4 MB |
| Content | noise (which does not compress away), and a tone per track |

| Build | Per minute of recording | Container throughput | Faster than real time by |
| --- | --- | --- | --- |
| `--release` | 212–237 ms (4 runs) | 617–690 MB/s | ~270× |
| debug | 449–465 ms (4 runs) | 315–326 MB/s | ~130× |

Nine times the audio-only figure per minute, for the same amount of audio,
because ~40 of every 41 bytes read here are video packets the analyser demuxes
and discards, and because AAC has to be decoded rather than copied.

**The throughput column is the one to extrapolate with, not the per-minute
column.** What this work costs is set by how many bytes there are to read, and a
real recording is bigger per minute than this file: 1440p60 at 50 Mb/s is about
2.5× the bitrate above, so about 2.5× the per-minute cost. On that basis a
30-minute recording is on the order of half a minute of a background thread in a
release build.

Both figures are with the file in the operating system's cache. A cold library
is bounded by the disk rather than by any of this — 617 MB/s is faster than most
drives read — which is exactly what background I/O priority is there for, and
why the honest summary is that this is a disk job with a decoder attached.

**Not measured here:** the effect on a game's frame times while generation runs.
That needs a game, a GPU and a machine to itself, so it is a manual measurement
rather than a test — see the verification notes on issue #66.

## How they reach the window

The window cannot open a `.cwf` and never will. It has no file-system
permission, and `tests/integration/tests/workspace_layering.rs` permits the
Tauri host exactly one crate of this workspace, `clipped-ipc` — so neither the
host nor the webview links a reader for this format, and giving the webview the
file would mean a second implementation of the layout above in TypeScript, of
the one surface where the two halves disagreeing is a waveform that is quietly
wrong.

So the peaks cross as **numbers**, on the same `open_preview` command that
carries a thumbnail ([ipc.md](ipc.md),
[ADR 0016](adr/0016-derived-pictures-cross-the-control-protocol.md),
[#448](https://github.com/wildware-uk/clipped/issues/448)). One command, one
reply and one set of three states for both, because a picture of a recording and
a picture of its sound are the same problem — derived, cached outside the index,
and unreachable from the window — and two mechanisms would have been two things
to keep in step.

A request names **how many buckets the caller can draw**, which in practice is
the pixel width of the row. That is what the pyramid is for and it is not an
approximation: merging is exact, so answering on the caller's own grid is the
same answer rather than a resampling of somebody else's. It is also what keeps
the reply small — the base resolution of an hour is 360,000 buckets, which no
frame holds — and the request is clamped to 4,096, past the width of any display
this runs on.

The wire shape is two numbers per bucket, minimum then maximum, interleaved in
one array: the two halves of a bucket cannot then arrive at different lengths,
and it is a quarter of the bytes a list of objects would cost. Zero tracks is a
successful answer and not a failure, which is what every recording Clipped
writes today produces.

## What is not built

- **Nothing draws these numbers on a timeline yet.** The playback screen draws
  the peaks of the recording it is playing — the first time any of these numbers
  has been on a screen — and issue #65 has since put the recording's *marks* on a
  strip below them, but the two are not one picture: there is no playhead over the
  peaks and nothing to scrub, which is issue #66. The clip editor's own lanes are
  issue #83, and that screen still cannot open a clip at all (issue #306), so they
  still say "No waveform".
- Nothing **suspends** the service when a recording starts. The recorder hosts
  it — `LibraryIndexer::for_this_user` starts it beside the indexer — but
  nothing calls `suspend_for_recording` or `resume`, so what protects a
  recording today is entirely the worker's thread and I/O priority. The
  mechanism is here and tested; the caller is not.
- The effect on an active game is stated as a design property and measured only
  as far as thread and I/O priority; the frame-time measurement is deferred.
