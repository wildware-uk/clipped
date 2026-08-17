# 0016. A recording's thumbnail and its waveform cross the control protocol, and the window gains no file-system reach

- Status: Accepted
- Date: 2026-08-17
- Issue: [#448](https://github.com/wildware-uk/clipped/issues/448)

## Context

Two subsystems generate something a screen has to draw, and both were finished,
tested and reachable by nothing at all:

- `crates/library/src/thumbnail` chooses a frame and stores a 640-pixel JPEG,
  about 20 kB, under `%LOCALAPPDATA%\Clipped\thumbnails`
  ([thumbnails.md](../thumbnails.md));
- `crates/waveform` reduces each sound track to minima and maxima and stores a
  pyramid of them in a `.cwf` binary sidecar under
  `%LOCALAPPDATA%\Clipped\waveforms` ([waveforms.md](../waveforms.md)).

`docs/thumbnails.md` said it plainly: *"The generator, the cache and the
background worker exist and are tested. Nothing draws the result."* Three routes
into the window were closed, and two of them deliberately:

1. **The window cannot read a file.** `capabilities/default.json` grants three
   `core:` permissions and two dialog permissions, none of which reaches the
   file system, and Tauri denies what is not listed. The content security policy
   names no origin a picture could come from either.
2. **The Tauri host may not link the crates that made them.**
   `tests/integration/tests/workspace_layering.rs` permits
   `apps/desktop/src-tauri` exactly one member of the workspace, `clipped-ipc`,
   which is what keeps capture and encoding out of the window's process
   ([ADR 0002](0002-separate-recorder-process.md)).
3. **No command served one.** `library_sessions` and `library_games` carry rows.

What has to remain true afterwards: (1) and (2) above; that a recording with no
picture yet stays distinguishable from one that will never have a picture
(AGENTS.md section 27); and that whatever is chosen carries the *peaks* as well
as the picture, because two mechanisms for two halves of the same problem is two
things to keep in step (AGENTS.md section 55).

Out of scope: a **live** preview of what is being captured. That is a stream
rather than a cached artefact and still gets its own transport decision when
something needs one ([ipc.md](../ipc.md), "Serialisation").

## Decision

**The recorder answers with the data itself, on the existing control protocol,
one recording at a time.**

`open_preview` names a recording and a kind — `thumbnail` or `waveform` — and is
answered by `preview_opened` carrying a state and, when there is one, the
picture as base64 or the peaks as numbers. The window draws the picture from a
`data:` URI and the peaks from the array.

Three things this fixes the boundary of:

- **The window gains nothing.** `img-src 'self' data:` was already in the policy,
  so `capabilities/default.json`, `tauri.conf.json` and
  `apps/desktop/src/playbackReach.test.ts` are all unchanged. The Tauri host
  registers nothing and serves nothing; the `clip` scheme
  ([ADR 0011](0011-what-the-webview-plays.md)) still serves recordings and only
  recordings.
- **One recording per request, never a page.** A page of the library is 25
  sittings ([library.md](../library.md)); 25 pictures is 25 frames of about
  27 kB, not one frame of 670 kB against a 1 MiB limit. A waveform is answered
  at the number of buckets the caller says it can draw, which is what keeps an
  hour-long recording's 360,000 base buckets off the wire.
- **Three states on the wire**, mirroring `ThumbnailState` and `WaveformState`
  exactly: `pending`, `ready`, `unavailable` with a reason.

## Alternatives

### A narrow Tauri file-system scope over the cache directories

The obvious answer, and the one the issue was written expecting. Grant the
window read access to `%LOCALAPPDATA%\Clipped\thumbnails` and let the webview
load each picture as a file. The bytes never touch the control protocol, the
webview's own image cache does the caching, and scrolling a library costs no
round trips at all — which is a real advantage and the reason this was close.

It lost on the half nobody had checked: **it cannot carry the peaks.** A
waveform entry is a `.cwf`, a binary format `crates/waveform/src/format.rs`
defines. The Tauri host may not link that crate (constraint 2 above) and the
webview certainly cannot, so serving the file would mean a second implementation
of the format in TypeScript — of the one surface where the two halves disagreeing
is a waveform that is quietly wrong rather than obviously broken. A scope would
therefore have served the thumbnail and left the waveform needing a transport of
its own, which is exactly the outcome #448's third criterion forbids.

Two smaller costs, which would not have been decisive alone: it is a
file-system permission the window has never had, and it would have to be scoped
to a directory the window has no other way to learn the location of.

**What would make it win later.** A screen that draws hundreds of pictures at
once, or one that redraws them fast enough for the round trips to be felt. The
figure to watch is in [thumbnails.md](../thumbnails.md); the measurement that
would settle it is the one this record does not have, which is how long a page
of twenty-five takes on a real machine.

### Putting the thumbnail on the library row

`library_sessions` already carries a row per recording, so a `thumbnail` field
on it would cost no extra round trip at all. Rejected on arithmetic: a page is
25 sittings and 15,000 recordings share 10,000 sittings in the library
[library.md](../library.md) measures, so a page carries more than 25 recordings,
at 27 kB of base64 each, against `MAX_FRAME_BYTES` of 1 MiB. The page would have
had to be cut short to fit — the bound `docs/library.md` already describes
having to apply once — and cutting a *listing* short because its pictures are
large is the tail wagging the dog. It would also make every library read pay for
a `stat` per recording whether or not anything was drawing pictures.

### A second URI scheme beside `clip`

Register a `preview` scheme in the Tauri host and have the recorder vouch for
cache files the way it vouches for recordings, exactly as
[ADR 0011](0011-what-the-webview-plays.md) arranged for playback. This is the
most consistent-looking option and it was the first design.

It fails on the same point as the scope, one step further along: the host can
serve a `.cwf` as bytes, but nothing at the far end can read them. Making it
work would mean either the TypeScript `.cwf` reader above, or the host
transcoding — which needs the crate it may not link. It also costs an origin in
`img-src` and one in `connect-src`, where the chosen answer costs none.

### Waiting for generation, rather than answering `pending`

Answer the request by generating the thumbnail if it is missing, so the caller
always gets a picture. Rejected: a thumbnail is tens of milliseconds and a
waveform is seconds ([waveforms.md](../waveforms.md)), a connection thread would
be parked for each, and a screen of rows would multiply it. Answering `pending`
immediately *and queuing the work* gets the picture made just as surely, without
a screen that does not draw.

## Consequences

**What becomes easy.** Any screen can draw either, with no permission, no policy
change and no new origin — the Library screen and the playback screen's poster
frame both did in the change that made this decision. Adding a third kind of
derived picture is a variant, not a transport. A window asking for a preview is
also what *queues* the work, so the generator's queue now follows what somebody
is looking at rather than the order the index happened to be in.

**What becomes hard.** Bulk. Every picture is a round trip, and
`RecorderLink::call` opens a connection per call against a recorder that serves
8 at once — so a screen drawing many rows must bound how many requests it has in
flight, or it will exhaust the cap and take whatever else the window was asking
for down with it. `apps/desktop/src/preview.ts` is where that bound lives, and
it is a cost this decision creates that the scope alternative would not have.

**What has to be watched.** The size of one picture, which is why
`apps/recorder/src/preview/tests.rs` asserts a bound rather than trusting the
generator's 640-pixel default; and the round-trip cost of a page, which is the
measurement this record does not have and which is the number that would reopen
the scope alternative.

**What this does not decide.** A live capture preview, above. And whether
`clipped-waveform`'s peaks should one day be drawn from a stream rather than a
reply — issue [#65](https://github.com/wildware-uk/clipped/issues/65)'s timeline
scrolls, and a timeline that re-asked the recorder on every scroll would be a
different question from this one.
