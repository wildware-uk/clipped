# 0013. Capture rounds an odd dimension away, and crops the frame to match

- Status: Accepted
- Date: 2026-08-17
- Issue: [#561](https://github.com/wildware-uk/clipped/issues/561)

## Context

H.264, HEVC and AV1 all sample 4:2:0 chroma at half resolution in both
directions, so a picture with an odd width or an odd height has no
representation in any of them. All four backends in `clipped-encoder` refuse one
at `open`, deliberately and by name, with the same sentence: *"has an odd
dimension, and 4:2:0 chroma needs both to be even"*.

Nothing between the capture backends and the encoder did anything about it.
`RecordingSettings::encode_size` returns the captured size unchanged, so a
capture that produced an odd shape reached `clipped_session::encoding::open`,
every ranked encoder refused it, and the recording failed before its first frame
with `encoder-unavailable`.

**That shape is ordinary.** A bordered window sized to 1000x600 has a client
area of **986x593** on Windows 11 at 96 DPI, and Windows Graphics Capture
reports exactly that. Measured on 2026-08-17 with `clipped-recorder watch`
against `test-apps/video-pattern`, a window taken from 1280x720 to 1000x600
mid-recording produced one playable file and then nothing at all: three
consecutive `encoder-unavailable` failures, five seconds apart, until the game
exited 24 seconds later.

[ADR 0012](0012-a-session-follows-a-resize-with-a-new-file.md) is what turned
that from a recording-ending fault into a session-ending one — it names this
defect in its consequences — but the fault does not need a resize. A window that
is *already* 986x593 could not be recorded either.

Two constraints shape the answer.

- **A track declares the size of the pictures in it.** Matroska puts a track's
  dimensions in the header when the file is created (ADR 0001), the encoder
  session is configured for one resolution, and every submitted frame states its
  own. AGENTS.md section 22 is about exactly the gap between what generated
  media claims and what it contains: a recording that says 986x593 while
  carrying 986x592 pictures is a lie no later stage can detect.
- **Nothing in this build can scale a frame.** There is no `ID3D11VideoContext`
  in the workspace, and ADR 0012 refused to add a per-frame blit to the capture
  path to keep a track's dimensions constant. Whatever is done about an odd
  dimension must not be a resample.

Not in scope: `--resolution`, which already refuses an odd value with a message
naming 4:2:0 (`apps/recorder/src/options.rs`), and scaling in general
([#182](https://github.com/wildware-uk/clipped/issues/182)).

## Decision

**`clipped-capture` never reports an odd dimension. Every backend reports
`FrameSize::rounded_down_to_even` of what its target measures, and hands over a
texture that is genuinely that size.**

Concretely:

- `FrameFormat` is documented as always even, and it is the whole of what the
  session, the encoder and the Matroska track are configured from — so no other
  crate changes at all.
- The row and the column that go are the **bottom** one and the **right-hand**
  one. The crop is anchored at the top-left corner, because that is where both
  Windows capture APIs anchor a picture, so what is recorded is the same picture
  with its last row missing rather than the same picture moved.
- Desktop Duplication capturing a window pays nothing: it already copies the
  window out of the desktop image into a destination texture of the size it was
  given, and that texture is now created one row or column smaller.
- Windows Graphics Capture, and Desktop Duplication capturing a display whose
  *mode* has an odd dimension, pay one `CopySubresourceRegion` per frame into a
  texture the backend owns (`crates/capture/src/windows/crop.rs`). It is a
  GPU-to-GPU copy on the capture thread — no `Map`, no CPU wait, nothing that
  leaves video memory.
- The Windows Graphics Capture frame pool keeps the content's **own** size, odd
  or not. The pool is what the compositor composes into, and a frame whose
  `ContentSize` differs from the pool's is the entire mechanism by which that
  backend recognises a resize.
- A target one pixel wide or one pixel high has no even picture inside it and is
  refused with `CaptureError::UnsupportedTarget`, naming 4:2:0.

`docs/capture-pipeline.md` states the rule, the table of what each backend pays,
and the two behavioural consequences.

## Alternatives

### Round in the session, and submit a frame that declares less than the texture holds

The cheapest possible change, and the one the issue names second: round in
`RecordingSettings::encode_size`, configure the encoder for 986x592, and go on
handing the encoder the capture backend's 986x593 texture with 986x592 stamped
on it. Three lines, no new texture, no copy, no change to `clipped-capture` at
all — and `SourceFrame`'s own documentation says the resolution is "asserted by
the caller and not checked against the resource", which reads like permission.

It lost on what the four encoder backends actually do with such a frame, which
is not one answer but four.

The software encoder refuses it outright and says so: `Readback::map` reads the
source texture's description and compares it against the session's resolution,
because `CopyResource` copies whole resources and returns `void`, so a mismatch
there would encode whatever the staging texture happened to hold — *"a 986x593
texture arrived for a session reading 986x592 frames"*. That refusal is not an
accident to be relaxed; it is the guard that exists so a size mismatch cannot
become a silently black or stale picture.

The three hardware backends do no such check, which is worse rather than better.
AMF builds a surface from the native texture, and the surface's size comes from
the texture; NVENC registers the resource with dimensions the caller supplies;
Quick Sync allocates through its own surface allocator. What each of their
drivers does when handed a surface a row taller than the session it was opened
for is undocumented, unequal between vendors, and untestable on the machine
making the change — and the failure mode, if it is wrong, is a picture that is
subtly sheared or offset rather than an error. Choosing an approach whose
correctness rests on three vendors' undocumented behaviour, to save one GPU copy
on the minority of windows that need it, is the wrong trade.

It is also the shape AGENTS.md section 22 warns about. The size travelling with
a frame would stop being a description of the frame and become an instruction to
the encoder, and every later reader of `SourceFrame::resolution` — including a
fifth backend nobody has written yet — would have to know that.

### Ask Windows Graphics Capture for a frame pool one row shorter than the content

The other half of the issue's first option, and it needs no copy at all: create
the pool at the even size and let the compositor put the content in it.

It lost twice. First, the pool's size is the reference the backend compares each
frame's `ContentSize` against, and that comparison is the whole of how a resize
is recognised — a pool deliberately smaller than the content reports
`Acquisition::SizeChanged` for every frame, for ever, and under ADR 0012 a
session answers each one with a new file. The comparison could be taught to
round, and that is genuinely the smaller change.

What it could not be taught is the second problem: **what the compositor does
with the row that does not fit is undocumented.** A crop and a rescale are
indistinguishable through the API — the same call, the same texture, the same
`ContentSize` — and one of them silently resamples every frame of the recording,
which is the thing ADR 0012 refused to do. Building on it would mean either
measuring one Windows build's behaviour and depending on it, or shipping a
recording whose honesty is a guess. One `CopySubresourceRegion` with a known
answer is worth more than a copy saved.

If Microsoft ever documents the behaviour as a top-left crop, this becomes the
better implementation for that backend and the copy can go.

### Pad the odd dimension up to even instead of cropping down

Symmetric, and it loses nothing of the window: record 986x594 with a row of
black at the bottom.

It lost because the black row is *in the picture*. Cropping loses one row of a
window nobody can see the bottom pixel of; padding adds a row that was never
there, to every frame of the file, for the life of the recording — visible in an
editor, in a thumbnail, and on any player that does not letterbox it away. It
also costs strictly more than cropping in every backend: Desktop Duplication's
window path, which pays nothing today, would have to clear the pad, and the
copy would still be needed everywhere else.

Padding is what an encoder does internally to reach a macroblock boundary, and
it is right there because the padding is discarded on decode. Here it would not
be.

### Refuse the target, clearly, and tell the user

Honest and tiny: report "this window cannot be recorded because its client area
is 986x593" and stop.

It lost against AGENTS.md section 56. The user cannot act on it — they cannot
resize a game's client area to an even number by dragging, and the shape is
reached by accident about half the time — so it converts an ordinary window into
one Clipped simply refuses to record, and under ADR 0012 it refuses the rest of
the sitting too. One lost row is not a loss a user would choose to trade a
recording for.

It survives in one place: a target a single pixel wide or high, where there is
no even picture at all, is refused rather than cropped to nothing.

## Consequences

- **A recording can be one pixel narrower or shorter than the window.** That is
  the cost, it is paid by any target with an odd dimension, and it is stated in
  `docs/capture-pipeline.md` rather than left to be noticed. Nothing tells the
  *user* about it: a line per recording saying a row was dropped would be noise
  on the overwhelming majority of recordings that lose nothing, and the log line
  the backend writes when it builds a crop is where a diagnosis starts.
- **Windows Graphics Capture pays a GPU copy per frame for such a target**, and
  loses its zero-copy property there. It is a full-frame copy in video memory —
  about 2.3 MB at 986x592, issued and not waited on — against an alternative of
  not recording at all. Even targets are untouched and still zero-copy, and the
  cost is bounded by the fact that no capture has more than one crop.
- **A frame's declared size and its texture's size are now the same thing, and
  must stay so.** Two places currently take `min(declared, texture)` defensively
  — `windows/pixel_sample.rs` and `windows/still.rs` — and they are now belt
  over braces rather than live cases. A future backend that reports a size it
  does not crop to would reintroduce exactly the defect this record refuses, and
  "Adding a backend" in `docs/capture-pipeline.md` says so.
- **Desktop Duplication and Windows Graphics Capture now disagree about what
  counts as a resize.** Desktop Duplication compares the window against the
  frame as recorded, so a 986x593 window taken to 987x593 keeps its file;
  Windows Graphics Capture must match its pool to the compositor exactly and
  reports the change. Both are honest for their API, the divergence is only
  ever about a single pixel, and the Desktop Duplication behaviour is the better
  one — fewer file boundaries, which is the thing ADR 0012's consequences say to
  watch.
- **`--resolution` gains a case that used to be impossible.** A user recording a
  986x593 window can now ask for 986x592 and be given it, because that is what
  capture produces; before, every even size was refused as needing a scaler and
  every odd one was refused by the encoder.
- **What to watch**: recordings whose size is one less than the window in the
  sidecar, and the "this target has an odd dimension" log line. Together they
  say how often the copy is being paid. If it turns out to be most windows
  rather than a minority, the Windows Graphics Capture alternative above is
  worth re-examining with a measurement of what the compositor actually does.
