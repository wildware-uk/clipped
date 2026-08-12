# Manual bookmarks

A bookmark marks a moment in a recording while the recording is being made. The
user presses a key or clicks a menu item, nothing is interrupted, and the moment
is written down where it can be found again — including after the recorder has
been killed.

This document covers what exists today
([issue #64](https://github.com/wildware-uk/clipped/issues/64)): the bookmark
store in `crates/session/src/bookmarks.rs`, the `add_bookmark` command the
recorder answers, the file it writes, and the two decisions that make the
feature either useful or annoying — *when* a bookmark is, and *how accurate*
that is.

**What does not exist yet.** Drawing bookmarks on a timeline and jumping to one
is [issue #65](https://github.com/wildware-uk/clipped/issues/65). Indexing them
into the SQLite library is M6's job and the `bookmarks` table is already there
waiting for it (`docs/storage.md`). And `Ctrl`+`F9` does not yet reach the
recorder: `clipped-hotkeys` registers the combination but nothing in any process
plugs a handler in, which is
[issue #232](https://github.com/wildware-uk/clipped/issues/232) — until it
lands, the tray's **Add Bookmark** item is the way to take one.

## When is a bookmark?

**Five seconds before the key press.**

A person presses the bookmark key *after* the thing they wanted to mark. They
had to watch it happen, decide it was worth keeping, and move a hand to a
function key. A bookmark stamped at the moment of the press is therefore
reliably late — never occasionally, always — and a marker that is always late is
one you scrub backwards from every single time you use it.

So the recorder subtracts a **lead** from the recording's position at the moment
the request arrives:

```text
 the thing happens        the key is pressed
        │                         │
        ▼                         ▼
 ───────●─────────────────────────●──────────▶  the recording
        └───────── lead ──────────┘
     bookmark lands here
```

Five seconds is the default. It is a reaction-time allowance and not a clip
length: the interesting part almost always started before the moment that made
it obvious, and landing a little early costs a few seconds of scrubbing forwards
where landing late costs the thing being marked.

Both figures are kept. The bookmark carries `at_seconds` (where it landed) and
`lead_seconds` (how far back it was moved), so the press is always recoverable
and a timeline can draw both. Nothing has to guess afterwards what the lead was
when a particular bookmark was taken.

**A bookmark taken in the first five seconds of a recording lands at zero.** The
offset is clamped rather than refused, because the moment being marked genuinely
is the beginning of the file. The lead is still recorded, so where the press was
is still known.

**A caller with no human reaction to allow for sends `lead_seconds: 0`.** That
is what a plugin marking an event it detected itself should do
(`docs/plugin-api.md`), and it is why the lead is a per-request parameter rather
than a constant baked into the recorder.

### Why it is not a setting yet

It should become one, and it is deliberately not one today. The recorder does
not read the configuration API at all: `clipped_session::config` resolves
settings and per-game overrides
([issue #108](https://github.com/wildware-uk/clipped/issues/108)), but reading
them at the moment a recording starts is
[issue #61](https://github.com/wildware-uk/clipped/issues/61) and no build does
it. A `bookmark_lead` preference added now would be a control the user could
change with no effect, which is exactly what AGENTS.md section 27 rules out.

So the lead is a documented default with a per-request override, and promoting
it to a stored preference is
[issue #271](https://github.com/wildware-uk/clipped/issues/271), which is a
small change once #61 has landed.

## How accurate is it?

A bookmark's offset is the media timestamp of the **last frame that reached the
file** when the request was handled. Not a wall clock, and this matters more
than it sounds.

`clipped_session::record` selects a capture backend, initialises it, waits for a
frame in order to find which device the textures live on, opens an encoder
against that device, and creates the file. Only *then* does the first frame that
reaches the container fix the recording's epoch (`docs/av-sync.md`). Anything
timing the recording from outside — `Instant::now()` when `record` was called,
say — is ahead of the file by all of that, which is hundreds of milliseconds on
a warm machine and can be seconds on a cold one. A bookmark a second out points
at the wrong thing.

So the recording publishes where it is: `clipped_session::RecordingProgress` is
one `u64`, stored with `Ordering::Relaxed` once per encoded frame, and read by
whoever is taking a bookmark.

**The tolerance, stated as a sum:**

| Contribution | Size |
| --- | --- |
| The published position is at most one frame interval stale | 16.7 ms at 60 fps, 8.3 ms at 120 fps |
| Named-pipe round trip from the client to the recorder | sub-millisecond on a local pipe |
| The lead the bookmark was taken with | 5 s by default, and *recorded*, so it is a known quantity rather than an error |

Discounting the deliberate lead, **a bookmark lands within one frame interval of
the moment the request reached the recorder** — under 17 ms at 60 fps. The
`lead_seconds` on the bookmark is what turns that into "and here is where the key
was pressed".

A frame that was skipped to hold the requested frame rate, or dropped because
the writer was behind, does not move the position. That is on purpose: a
bookmark has to name a moment that is actually in the file.

## Nothing about this touches capture

AGENTS.md section 20 says a capture thread may not wait on the filesystem, and a
bookmark ends in a file write. The two are kept apart by construction rather
than by care:

```text
 capture / encode thread                    connection thread
 ───────────────────────                    ─────────────────
 acquire, submit, drain                     read add_bookmark
 progress.reached(t)  ── relaxed store ──▶  progress.position()
        │                                   append to the log
        ▼                                   write the sidecar
   the recording                            reply
```

- The capture thread's entire involvement is **one relaxed atomic store per
  encoded frame**. No lock, no allocation, nothing to wait on.
- `clipped_session::record_into` is handed the `RecordingProgress` and **never
  the bookmark log**. It could not reach a bookmark if it tried; there is no
  field, no argument and no global.
- The recorder's `add_bookmark` takes what it needs out of the recording-state
  mutex — the log and the progress handle, both `Arc`s — and **releases it
  before writing anything**. The recording thread touches that mutex exactly
  twice in its life, when it is stored and when its outcome is, and never per
  frame.

The cost of a bookmark, then, is one small file write on a connection thread. If
that write is slow because the disk is busy, the reply is slow; the recording is
not.

## The file

One JSON sidecar per recording, beside it and named after it:

```text
D:\clips\clipped-cs2-20260811-143205.mkv
D:\clips\clipped-cs2-20260811-143205.bookmarks.json
```

Beside the recording rather than in the session file, because a bookmark belongs
to *one recording* — it is an offset into that file, and it has to survive the
file being moved, copied or opened on another machine. That is also the shape
`clipped-storage`'s `bookmarks` table has: a row references a `recording_id`,
not a session (`docs/storage.md`). A session sidecar's `bookmarks` key stays
reserved and empty; `docs/sessions.md` says so.

```json
{
  "schema_version": 1,
  "recording": "clipped-cs2-20260811-143205.mkv",
  "bookmarks": [
    {
      "at_seconds": 115.0,
      "lead_seconds": 5.0,
      "label": "triple kill on mid",
      "colour": "#ffcc00",
      "duration_seconds": 12.5,
      "created_at": "2026-08-11T14:34:05+01:00"
    }
  ]
}
```

The four fields SPEC.md section 25 asks for — timestamp, label, colour, duration
— under the names `clipped-storage`'s columns use, so that indexing these files
later is a copy rather than a translation. `label`, `colour` and
`duration_seconds` are omitted when they were not given; a bare hotkey press
produces a bookmark with only `at_seconds`, `lead_seconds` and `created_at`.

A colour is stored exactly as the interface wrote it. Nothing in the recorder
interprets one.

### Rules for anything that reads it

- **Ignore keys you do not recognise.** A later Clipped may add a field, and a
  reader that refused one would make every recording taken by a newer build
  unreadable by an older one.
- **Do not assume the file exists.** A recording with no bookmarks has no
  bookmark file at all.
- **Tolerate nonsense figures.** These are text files a user can edit. A
  negative or unrepresentable `at_seconds` is read as zero rather than being
  allowed to bring anything down.

### Surviving a recorder that is killed

The file is rewritten in full after **every** bookmark: to a `.tmp` beside it,
then renamed over the real one, which is the same technique the session sidecar
uses and the reason a half-written file is impossible (`std::fs::rename`
replaces the destination on Windows).

The write happens *before* the reply is sent. So a recorder that dies — a crash,
a power cut, Task Manager — has already written down every bookmark it
acknowledged. Nothing is batched and nothing waits for the recording to end.

If a write fails, the caller is told: the reply is an `internal` refusal naming
the file, because a full or disconnected drive is something only the user can do
anything about (AGENTS.md section 45). The bookmark stays in memory and the next
successful write carries it.

## The command

```json
{"id": 7, "command": "add_bookmark", "params": {}}
```

Every parameter is optional, because the request a hotkey or a tray item sends
carries none of them — pressing the key is the whole interaction and there is
nowhere to type.

| Parameter | Meaning |
| --- | --- |
| `recording_id` | Which recording to mark. Absent means "whatever is being recorded". |
| `label` | What to call it. At most 200 characters, no control characters. |
| `colour` | Any notation the interface likes, at most 64 characters. |
| `duration_seconds` | How long the marked moment lasts, up to an hour. Absent means it is a moment rather than a span. |
| `lead_seconds` | How far before this request to stamp it. Absent means the default above; at most 120 seconds. |

The reply says where the bookmark **landed**, which is not where the request was
made:

```json
{"id": 7, "outcome": {"ok": {"reply": "bookmark_added", "bookmark": {
  "recording_id": "r-1",
  "at_seconds": 115.0,
  "pressed_at_seconds": 120.0,
  "lead_seconds": 5.0,
  "bookmarks_file": "D:\\clips\\clipped-cs2-20260811-143205.bookmarks.json",
  "bookmarks_in_recording": 3
}}}}
```

An interface that reported the press rather than `at_seconds` would be showing a
moment that is not the one in the file.

### When it is refused

| Situation | Refusal |
| --- | --- |
| Nothing is being recorded | `not_recording` — "there is no recording to mark a moment in" |
| A `recording_id` that is not the one running | `not_recording`, naming it |
| The recording has not captured its first frame | `not_recording` — there is no moment to mark, and marking zero would put the bookmark somewhere the user was not looking |
| A label, colour, duration or lead the recorder will not store | `invalid_parameters`, naming the field |
| The bookmark was taken and could not be saved | `internal`, naming the file |

A recorder built before this feature has no `add_bookmark` command and answers
`unknown_command`. An interface should not reach that state: the recorder
advertises `bookmarks` in its welcome, and the point of the feature list is that
a control whose command would be refused is never offered (`docs/ipc.md`).

## The replay buffer case

A recording that also fills a replay buffer is **one encoder and one timeline**
(`docs/replay-buffer.md`). So a bookmark taken during one is the same bookmark
in the same place: the position it is placed against is the media timestamp of
the frame that went to both the file and the buffer, and the sidecar belongs to
the file.

A recording that fills a buffer and writes *no* file has nothing for a bookmark
to be an offset into, and no build makes one — starting a buffered recording at
all is [issue #38](https://github.com/wildware-uk/clipped/issues/38). What a
bookmark should mean in that mode is a decision for the ticket that builds it,
and the honest answer today is that this build cannot be in that state.

## Where the interface says it

- **Tray.** **Add Bookmark**, live while something is being recorded and
  disabled with "nothing is being recorded" otherwise. It sends no label and no
  colour — a notification-area menu is one click and has nowhere to type — so it
  takes the same bare mark a hotkey would. What happened is reported only when
  it failed; a mark that succeeded changes nothing on screen worth interrupting
  a game for.
- **Hotkey.** `Ctrl`+`F9` is bound by default (`docs/hotkeys.md`) and no process
  handles it yet — [issue #232](https://github.com/wildware-uk/clipped/issues/232).
  A press is reported as unhandled rather than silently swallowed.
- **Overlay and toast.** SPEC.md section 25 asks for feedback that does not
  interrupt gameplay. The overlay is M5,
  [issue #53](https://github.com/wildware-uk/clipped/issues/53); a "bookmark
  added" notification is in
  [issue #110](https://github.com/wildware-uk/clipped/issues/110)'s scope and is
  deliberately not a toast today — `apps/desktop/src-tauri/src/notification_policy.rs`
  reserves toasts for failures, on the grounds that a toast per successful
  bookmark is the nuisance rather than the feature.
