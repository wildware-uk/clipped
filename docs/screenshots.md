# Screenshots

A still image of the game, taken on a hotkey, while the game is being played
(SPEC.md section 26). This document is the prose for
[`crates/session/src/screenshot`](../crates/session/src/screenshot) and the frame
grab underneath it in
[`crates/capture/src/still.rs`](../crates/capture/src/still.rs) — where the
picture comes from, what it costs, what it is called, where it goes, and what is
deliberately not built.

Issue: [#67](https://github.com/wildware-uk/clipped/issues/67). Written in
milestone M8.

## The decision everything else follows from

**A screenshot is a frame the recording already had.**

The capture backends deliver every frame of the game to `clipped-session`
dozens of times a second ([capture-pipeline.md](capture-pipeline.md)). Opening a
second capture to photograph the same window would mean a second frame pool, a
second copy of every frame the compositor produces, and — on Desktop
Duplication, which is exclusive per output — competing for a resource Windows
hands to one client at a time. So while a recording is running, a screenshot
costs one texture copy of a frame that had already been captured, and no capture
at all (AGENTS.md sections 18 and 55).

When nothing is being recorded there is no such frame. A screenshot key that
only worked while recording would be a key nobody trusts, so the other path
exists and is the expensive one; its cost is measured below.

## What it costs the capture thread

Measured with `cargo run --release -p clipped-capture --example still_cost`, on
an NVIDIA GeForce RTX 4090, Windows 11, 60 samples per size after a discarded
first one. The source is a Direct3D 11 texture of the size given — the same kind
of resource a capture backend hands over — so the figures are the copy and
nothing else.

| Frame | Bytes | `begin` (queue the copy) | `poll` (map and read) |
| --- | --- | --- | --- |
| 1920x1080 | 8.1 MB | 0.054 ms median, 0.081 worst | 1.05 ms median, 2.13 worst |
| 2560x1440 | 14.4 MB | 0.071 ms median, 0.136 worst | 1.95 ms median, 2.40 worst |
| 3840x2160 | 32.4 MB | 0.070 ms median, 0.194 worst | 4.29 ms median, 8.07 worst |

Two numbers because the copy is in two halves, and that split is the whole
design:

- **`begin`** issues `CopyResource` into a staging texture and flushes. It is
  queued for the GPU and returns; the thread does not wait for it. This is what
  happens on the frame the key was pressed on.
- **`poll`** maps the staging texture with `D3D11_MAP_FLAG_DO_NOT_WAIT` — which
  returns `DXGI_ERROR_WAS_STILL_DRAWING` rather than blocking — and copies the
  rows out. It answers "not yet" until the GPU has caught up, and a recording
  simply asks again on its next turn round the loop.

The naive version does both at once, and then the `Map` waits for the GPU:
milliseconds of a stalled capture thread, which at 144 fps is more than a whole
frame's budget. Split, the frame the key was pressed on pays 0.07 ms and a
later one pays the memory copy — and the later one has 16.7 ms of budget at 60
fps rather than whatever was left of it.

After roughly a quarter of a second of polling
(`SCREENSHOT_POLL_LIMIT` in `crates/session/src/recording.rs`) the loop maps the
blocking way instead. That is for the case where the source stops producing
frames the instant after the key was pressed: without it, the polling stops too
and somebody who asked for a picture would get a timeout instead.

**Encoding and writing happen on another thread entirely** — whichever one the
`take_screenshot` command arrived on. A 4K PNG is tens of milliseconds of
processor and a file write touches a disk, and AGENTS.md section 20 forbids a
capture thread either.

## What the other path costs

Opening a capture just for one picture is everything a recording does before its
first frame, minus the encoder: create a Direct3D 11 device, create a frame pool
or duplicate the output, wait for the source to draw, copy, shut down. On the
machine above that is a couple of hundred milliseconds for a window that is
drawing — two orders of magnitude more than the copy — and the wait is the part
that is not under Clipped's control, because a window that is not redrawing
produces no frames at all. It gives up after two seconds with "nothing produced
a frame to photograph" rather than waiting for ever (AGENTS.md sections 16 and
45).

`clipped_session::screenshot::capture_still` is that path, and
`take_screenshot`'s `window`, `process` and `pid` parameters exist for it. They
are ignored while a recording is running, because the frame that recording
already has is both cheaper and the only way to be sure the picture is of what
is being recorded.

## Format

**PNG by default**, with JPEG and lossless WebP available. All three are
encoded by the FFmpeg build Clipped already links (`png`, `mjpeg` and `libwebp`
respectively), so none of them is a new dependency — the same reasoning
[thumbnails.md](thumbnails.md) gives for using FFmpeg to write a JPEG.

PNG is the default for the opposite reason a thumbnail is a JPEG. A thumbnail is
640 pixels wide, there are tens of thousands of them, and nobody keeps one. A
screenshot is full resolution, there are a handful, and it is kept, cropped,
annotated and posted — and every one of those is done to the *file*, not to the
frame it came from. A lossy default trades bytes nobody is short of against
quality that cannot be recovered, and it is a trade a user only notices after
the frame is gone.

"Lossless WebP where practical" in SPEC.md section 26 is doing real work.
libwebp's default is *lossy*, so the encoder is explicitly told `lossless=1`
and a build whose libwebp will not take the option refuses the format by name
rather than writing a lossy file under it (AGENTS.md section 54). A build with
no WebP encoder at all reports the same thing.
`ScreenshotFormat::is_available` is the question a settings screen has to ask
before offering the option, because the answer depends on how FFmpeg was built.

Colour is handled explicitly in both halves. A captured frame is BGRA8 with
full-range samples. PNG takes RGB24 — the alpha channel is *dropped*, because a
captured frame's alpha is whatever the compositor left there and is routinely
zero across the whole picture, which produces a screenshot that looks correct in
an image viewer and is entirely transparent in anything that respects it.
Lossless WebP takes BGRA unchanged, which is what makes it lossless. JPEG
becomes 4:2:0, and both swscale and the encoder are told the destination is full
range; the two disagreeing produces a slightly grey picture with nothing
anywhere reporting a problem, the same silent failure
[thumbnails.md](thumbnails.md) and `crates/encoder`'s converter document.

An HDR capture is 10 or 16 bits a channel and is **refused**, naming
[#99](https://github.com/wildware-uk/clipped/issues/99). Reading those samples
as though they were 8-bit produces a picture, and the picture is wrong. SPEC.md
section 26 puts HDR-aware screenshots in a later milestone and this is where
that boundary is enforced.

## Where they go, and what they are called

```text
%USERPROFILE%\Pictures\Clipped\
    clipped-counter-strike-2-20260811-143205.png
    clipped-counter-strike-2-20260811-143205-2.png
    clipped-unattributed-20260811-150118.png
```

**`Pictures\Clipped`, not the recordings folder.** Two reasons, and the second
is the one that would bite later:

- Windows files images in Pictures. Explorer, the Photos app and every upload
  dialog start there, and a screenshot is something a person goes and finds,
  unlike a recording, which the application shows them.
- Storage accounting requires that **no root contains another**
  ([storage-management.md](storage-management.md)), and `Screenshots` and
  `Recordings` are two of its categories. A screenshots folder nested inside the
  recordings folder would have its bytes counted under whichever root won.

The name is `clipped-<game>-<yyyymmdd>-<hhmmss>`, which is deliberately the stem
a session's own files use (`clipped-<game>-<yyyymmdd>-<hhmmss>.mkv`,
`clipped_session::automatic::SessionId`). It sorts chronologically, it is legible
in a directory listing, it contains no character Windows forbids in a file name,
and a person looking at a screenshot and a recording of the same evening can tell
they belong together without opening either.

A screenshot whose game nobody identified is filed under `unattributed`, which is
the word a *session* with no attributable game already uses. It is not "unknown"
and not an empty gap where the name should be.

**Two screenshots in the same second are two files.** Names are stamped to the
second, so the second press takes `-2`, the third `-3`, up to 99. Without that
rule the second press silently replaces a picture the user took and can never
take again (AGENTS.md section 56). The file is written to a temporary name in the
same folder and renamed into place, so a recorder killed mid-write leaves no
truncated image where a screenshot should be — the same rule
[bookmarks.md](bookmarks.md) follows for the same reason.

## The rendezvous

Somebody presses the key. The press arrives on a connection thread inside the
recorder ([ipc.md](ipc.md)); the frames are on the capture thread, where they are
borrowed from the backend, may not outlive the acquisition that produced them and
may not leave the thread at all. The two cannot be introduced directly, so
`ScreenshotRequests` is the meeting point:

```text
capture thread                          the connection thread
──────────────                          ─────────────────────
                                        take() registers a request
acquire a frame                              │
submit it to the encoder                     │  (waiting)
a request is waiting? ◀──────────────────────┘
  claim it, begin the GPU copy            (waiting)
... carry on recording ...                (waiting)
poll the copy                             (waiting)
  ready: hand over owned pixels ─────────▶ encode
                                           write the file
                                           reply with the path
```

The order inside the loop is load-bearing: the frame reaches the encoder
**first**, and the screenshot is considered afterwards. The recording is what
must not be delayed, so a screenshot waits one more frame rather than being the
reason a frame was late (AGENTS.md section 17).

Nothing in that diagram can fail a recording. A copy that fails is reported to
whoever asked and the loop carries on. A recording that ends with a request
outstanding answers it — "the recording ended before a frame could be copied" —
rather than leaving a thread to time out, and a waiter that gives up withdraws
its request so that no later frame spends a texture copy on a screenshot nobody
will collect.

## What is not built

- **The library and the timeline.** A screenshot is a file with a name that
  sorts beside the recordings of the same game, and nothing indexes it. There is
  no `screenshots` table in `clipped-storage` ([storage.md](storage.md) says so),
  so a screenshot cannot yet be favourited, cannot appear in the library grid and
  leaves no timeline marker. The recorder does return `at_seconds` — the
  recording's own media position of the frame — so the marker has somewhere to go
  the day the table exists. That is
  [#334](https://github.com/wildware-uk/clipped/issues/334).
- **Attribution to a game.** A screenshot taken by `serve` is filed
  `unattributed`. `serve` does open a session for a recording somebody asked for
  ([sessions.md](sessions.md), [#402](https://github.com/wildware-uk/clipped/issues/402)),
  but that session's game is `unidentified` — nothing asked the catalogue about
  the window the user picked — so there is still no game to file a screenshot
  under, and filing it under one nobody identified would be invented data
  (AGENTS.md section 27). Same issue; identifying the game a manual session is of
  is [#403](https://github.com/wildware-uk/clipped/issues/403).
- **Settings.** The format, the JPEG quality and the folder are typed, validated
  and defaulted in `ScreenshotSettings`, and nothing reads them from the
  configuration file: the recorder does not read the settings API at the moment a
  command arrives, which is
  [#61](https://github.com/wildware-uk/clipped/issues/61).
- **A default combination.** The recorder registers a `take_screenshot` hotkey
  and a press sends the same command the window does
  ([#232](https://github.com/wildware-uk/clipped/issues/232),
  `docs/hotkeys.md`) — but nothing is bound to it out of the box, because
  SPEC.md names only two defaults and taking a third combination from every
  other application on the machine is not this ticket's to do. Binding one means
  editing the settings file until
  [#54](https://github.com/wildware-uk/clipped/issues/54) draws the screen.
- **HDR.** [#99](https://github.com/wildware-uk/clipped/issues/99), above.

## How it is tested

| What | Where | Needs |
| --- | --- | --- |
| A frame copied off a real GPU is the picture, pixel for pixel | `crates/capture/src/windows/still.rs` | A Direct3D device. No window. |
| Polling produces the identical image to waiting | same | same |
| An HDR or non-Direct3D frame is refused by name | same | nothing |
| A padded buffer reads back one row at a time | `crates/capture/src/still.rs` | nothing |
| PNG and lossless WebP decode back to the exact captured pixels | `crates/session/src/screenshot/tests.rs` | `ffmpeg` from the pinned build |
| Every format is the codec and size it claims | same | `ffprobe` |
| The JPEG quality setting reaches the encoder | same | nothing |
| Two screenshots in one second are two files | same | nothing |
| A refused screenshot leaves no file, and no temporary one | same | nothing |
| The rendezvous answers every waiter exactly once | same | nothing |
| A screenshot during a real capture of a real window | `tests/capture/screenshot.rs` | `CLIPPED_REQUIRE_CAPTURE`, a desktop |

The last row is the only one that opens a window, and it is behind
`CLIPPED_REQUIRE_CAPTURE` and `#[ignore]` for the reason
[testing.md](testing.md) gives.
