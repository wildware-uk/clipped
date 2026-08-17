# 0011. A session follows a mid-recording size change with a new file, and Clipped does not scale

- Status: Accepted
- Date: 2026-08-17
- Issue: [#184](https://github.com/wildware-uk/clipped/issues/184)

## Context

A capture target can change size while it is being recorded. A window is dragged
by its edge, a game changes resolution, a borderless window is toggled to
fullscreen, a display's mode changes underneath a duplication. `clipped-capture`
reports every one of them the same way: it discards the frame that revealed the
change, reports `Acquisition::SizeChanged(new_size)`, and goes idle until the
caller calls `CaptureBackend::resize` (`docs/capture-pipeline.md`).

What the caller does next is the decision this record makes, and the reason it is
a decision rather than a fix is that **one file cannot hold two sizes**. Three
separate parts of the pipeline fix a resolution and none of them can change it:

- **The container.** Matroska puts a track's dimensions in the header, which is
  written when the file is created ([ADR 0001](0001-mkv-archival-container.md),
  `crates/muxer/src/writer.rs`). A track cannot appear later and cannot change.
- **The encoder session.** `VideoEncoder` has no reconfigure and no reset — the
  lifecycle is `open → (submit ⇄ next_packet)* → finish → shut_down`, with no arrow
  back to `open` — and the resolution is baked into the session at `open` by every
  one of the four backends. All four refuse a `SourceFrame` whose resolution is
  not the session's, with the same sentence: *"reconfigure the encoder when the
  capture changes size"*, a thing the trait does not offer.
- **The recording itself.** `RecordingSettings::encode_size` produces one pair of
  numbers, once, from the first frame, and that pair is simultaneously the
  encoder's configuration, the Matroska track's dimensions and the resolution
  stamped on every submitted frame (`crates/session/src/recording.rs`).

There is also **no scaler anywhere between capture and the encoder**. The one
`sws_scale` in the encoder crate converts pixel format at a fixed size; the one
`CopySubresourceRegion` in Desktop Duplication crops and cannot resample; there
is no `ID3D11VideoContext` in the workspace at all. `docs/configuration.md` and
`docs/recorder-cli.md` already say so, and it is why `--resolution` may only name
the size the capture is already producing.

Against that, a session is **already** a thing that holds several files.
`docs/sessions.md` describes a sitting whose window was destroyed and recreated
as one session with two recordings, and SPEC.md section 35 asks for exactly this
shape: *"prefer recoverable segmented recordings over giant fragile files."*

## Decision

**A size change finishes the current file and the session starts the next one.
Clipped does not scale, crop or letterbox to keep a track's dimensions constant.**

Concretely:

- `clipped_session::record` ends the recording at `Acquisition::SizeChanged`, with
  `EndReason::TargetResized`, having flushed the encoder and written the trailer.
  Everything captured before the change is in a file that plays and seeks.
- The **session** is what continues. `clipped_session::automatic::SessionManager`
  already starts the next recording of a session whenever one ends while the game
  is still running; a recording that ended in a resize starts its successor
  **immediately** rather than waiting out the restart delay, because that delay
  exists to let a not-yet-reported process exit arrive and a resize is proof that
  the window did not go anywhere.
- `clipped-recorder record` is unchanged and single-file by design: it writes to
  the one path the user named, and a resize ends it. `docs/recorder-cli.md` says
  so.

## Alternatives

### Scale the new size back to the committed one in the capture path

The option the issue names first, and the only one that keeps a sitting in one
file. A `VideoProcessorBlt` (or a shader pass) between `CapturedFrame` and
`SourceFrame` would resample every frame to the size the track was created with,
so the encoder and the container would never see a change.

It has a real case. One file per sitting is what a user expects, it needs nothing
of the library or the protocol, and the same piece would let `--resolution` mean
something other than "the size you are already capturing"
([#182](https://github.com/wildware-uk/clipped/issues/182)).

It lost on three counts. It is **a new GPU stage on the capture path**, which is
the one path this project holds to a per-frame budget (AGENTS.md sections 18 and
20) — a blit per frame, an intermediate texture pool per recording, and the end
of the zero-copy handoff the encoder crate was built around. It is **lossy in a
way the file cannot admit**: a window enlarged from 1280x720 to 2560x1440 would
be recorded as an upscale of the smaller picture, in a track that declares 720p
and gives no hint that half the session is a resample. And it is **the wrong
default even when it works**: somebody who changes their game to a higher
resolution mid-session wants the rest of the sitting at that resolution, not a
downscale to whatever it was when they pressed record.

Nothing here rules the blit out as a *feature*. It is what #182 needs, and if it
is built, a user-chosen "keep one file" mode could use it. What this record
refuses is making it the silent answer to a size change.

### Keep ending the recording, and document it

The status quo, and the issue's third option. It is honest — the file is
finalised, the reason is recorded, and no footage is lost — and it costs no code.

It lost because ending is only honest at the level of the *file*. Measured
against a real resize on 2026-08-17 (`clipped-recorder record`, a window taken
from 1280x720 to 1000x600 by `SetWindowPos`), the recording stopped at the
resize and nothing followed it: the sitting was over because somebody dragged a
window. SPEC.md section 35's "recoverable segmented recordings" is a request for
the opposite, and the model that satisfies it already exists.

### Reconfigure the encoder and start a new Matroska *segment* in the same file

Matroska supports linked segments, and a second segment could declare a second
track with new dimensions. It would give one file per sitting without any
resampling.

It lost on tool support and on `clipped-muxer`. Segment linking is poorly handled
by editors — DaVinci Resolve and Premiere are the two the MVP is defined
against — and a file that opens showing only its first segment is worse than two
files that both open. `clipped-muxer` writes through `libavformat`, whose
Matroska muxer writes one segment and finalises once.

### Segment inside `clipped_session::record` rather than in the session layer

The tightest possible seam: `record` would keep the capture backend, call
`CaptureBackend::resize`, open a new encoder and a new file, and carry on — a gap
of tens of milliseconds instead of a restart.

It lost on ownership. A session's *files* are the session layer's to name, number,
place on the session timeline and write into the sidecar the library indexes
(`docs/sessions.md`). A `record` that produced several files would either have to
report them all — changing the shape every driver, the sidecar and the protocol
are built on — or produce files that the session's own record does not know
about, which is worse than the bug. `SessionManager` already segments a sitting
correctly; a second implementation of it under `record` is what AGENTS.md section
55 exists to prevent.

The cost of losing this option is the size of the seam, and it is the consequence
below that is worth watching.

## Consequences

- **A resize costs a file boundary, and the library has to present it.** Two
  files, one sitting, joined by the session record — `starts_at_nanos` and
  `duration_seconds` already place each file on the session's timeline, which is
  what lets a moment be drawn on the right second of the right recording
  ([#71](https://github.com/wildware-uk/clipped/issues/71)).
- **The seam is not free.** The next recording re-resolves the window, starts a
  capture, waits for a frame, opens an encoder and creates a file. Removing the
  restart delay takes the gap from about six seconds to about half of one; it
  does not take it to zero, and it cannot without the option above.
- **Every resize consumes one of a session's hundred recordings.** A window
  dragged repeatedly can reach
  `AutomaticSettings::max_recordings_per_session`, after which the sitting stops
  recording and says so. The cap is a loop guard and this decision is the thing
  most likely to make it bite; if it does, the answer is to coalesce a run of
  resizes rather than to raise the number.
- **`clipped-recorder record` and `serve` still end.** The CLI records to one
  named path, which is the contract of that subcommand. A recording started from
  the desktop window ends too, because `ManualSession` holds exactly one
  recording by construction (`crates/session/src/automatic/manual.rs`) — so a
  user who drags the edge of a window they are recording from the desktop gets
  the file they had and no successor. That is a gap this decision creates the
  obligation to close, and it belongs with
  [#241](https://github.com/wildware-uk/clipped/issues/241), which is giving the
  protocol the words for a sitting with more than one file in it.
- **`CaptureFallback` can relax its committed-format rule.** `crates/capture`'s
  fallback refuses a replacement backend whose frames are a different size,
  explicitly deferring to this issue. Under this decision the rule stands for a
  *replacement mid-file* and relaxes only in that a caller which followed a resize
  tells the fallback through `CaptureFallback::resize`. That is
  [#285](https://github.com/wildware-uk/clipped/issues/285)'s to wire up.
- **A display change is the same answer.** Ultrawide and display-change handling
  ([#98](https://github.com/wildware-uk/clipped/issues/98)) reaches the recording
  loop as a size change and is followed the same way, rather than needing a rule
  of its own.
- **This decision makes an unrelated defect load-bearing.** A window resized to a
  client area with an odd dimension cannot be encoded at all — 4:2:0 chroma has no
  representation for one, and all four encoders refuse it by name. Before this
  decision that failed one recording; under it, the recording that *follows* every
  such resize fails to open, so the sitting records nothing more. Measured on
  2026-08-17: a 1280x720 window taken to 1000x600 produced a 986x593 client area
  and four consecutive `encoder-unavailable` failures until the game exited. It is
  filed as its own issue and is not solved here; the note is here because this
  record is what makes it a session-ending fault rather than a recording-ending
  one.
- **What to watch**: sessions with many short recordings in them. That is the
  signature of both the cap being reached and the odd-dimension failure, and both
  are visible in the sidecar without a log.
