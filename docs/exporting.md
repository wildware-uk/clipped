# Exporting

How an edit document becomes a file, what that costs, and what the resulting
file is guaranteed to contain. The implementation is `crates/export`
(`clipped-export`); the document being rendered is
[docs/editing.md](editing.md); the container being written is
[docs/muxing.md](muxing.md).

Spec: SPEC.md sections 19 and 20. AGENTS.md sections 55, 56 and 57.

## What an export is

An edit is metadata. `EditDocument` says which recordings to play, which parts
of them, in which order, how loud each audio track is and what text to draw over
the picture — and nothing in it is a picture. An export is the function that
turns that into something a person can send somebody, and it is the only part of
editing that produces a file.

It never touches the recordings it drew on. That is not a policy this code
follows carefully; it is a property of how it opens them. `avformat_open_input`
opens for reading, nothing in `crates/export/src/media.rs` opens a source any
other way, and `crates/export/tests/a_source_recording_is_never_touched.rs`
asserts both — the recording's bytes before and after every path an export can
take, and the absence of any file-writing call from the module that reads them.

## Two ways to make one, and which is used

| | Stream copy | Re-encode |
| --- | --- | --- |
| What runs | a demuxer and a muxer | a decoder, filters and an encoder |
| Picture quality | the recording's own coded frames, bit for bit | one more generation |
| Speed | about as fast as reading the file | many times slower |
| Can change the picture | no | yes |

The decision is `ExportPlan::of`, it is made before anything is created, and it
is answered from the document plus one pass over each recording that demuxes
without decoding. A copy is used when **every** one of these holds:

| Condition | Why not otherwise |
| --- | --- |
| The edit draws on one recording | two recordings are two sets of stream parameters and one container header |
| Every segment is untransformed | a speed, a crop or a rotation is a new picture |
| There are no overlays | text over the picture is a new picture |
| The document's aspect ratio matches the recording's | a different shape is a new picture |
| The codecs are ones `clipped-muxer` can describe | a track it cannot describe cannot be declared |
| The video stream is not reordered | see [Reordered streams](#reordered-streams) |
| Every segment begins on a keyframe | a decoder cannot start anywhere else |
| Every audio track is one recorded stream at its recorded level | anything else is a mix, which has to be produced |

`ExportPlan::blockers` returns **all** the reasons rather than the first, because
a caller offering to re-encode is explaining a decision and one of three reasons
is a worse explanation than three. Every one of them prints as a sentence.

`plan_export` answers all of this without writing anything, so an export dialog
can say what somebody is about to wait for before they wait for it (AGENTS.md
section 45).

## Frames, cuts and keyframes

[docs/editing.md](editing.md#frames-and-what-happens-at-a-keyframe) fixes the
rules and this is the implementation of them.

**Snap outwards from the document, never inwards.** A segment's first exported
picture is the first whose presentation time is at or after `span.start`, and
its last is the last strictly before `span.end`. Ranges are half-open, so a
picture belongs to exactly one side of a cut: none is duplicated at a join and
none is dropped. `VideoFrameIndex::first_at_or_after` and
`VideoFrameIndex::frames_in` are the two halves of that rule, and the writing
loop applies the same arithmetic.

**A keyframe is a re-encode decision, not a timing one.** A segment whose first
picture is not a keyframe cannot be copied, because a decoder cannot start
there. A copy would have to begin at the previous keyframe instead — up to a
whole group of pictures of material the user deleted — which is a visible
difference from what the editor showed. So the cut is *never* moved. The
blocker names the cut, the keyframe a copy would have had to move back to, and
how much material that is:

```text
segment 0 starts at 1.500s, which is not a picture a decoder can start at;
a copy would have had to begin 0.500s earlier and show material the cut removed
```

This is deliberately different from a replay clip, which *does* begin at the
keyframe at or before the requested start and reports the slack
([docs/replay-buffer.md](replay-buffer.md)). A replay has no editor behind it
and no preview to disagree with; an export has both.

### The one conversion

Every packet a copy writes goes through one line of arithmetic:

```text
output = segment.output_start + (source − segment.span.start)
```

It lives in `PlannedSegment::output_of` so that it is testable on its own, and
it is the whole of "the exported file matches the timeline".

### Reordered streams

A copy trims the end of a segment in presentation order, which for a stream
that stores its pictures out of the order they are shown in could drop a
picture that a kept picture references. Rather than guess at the reordering
depth, an export refuses to copy such a stream at all: the index pass notices
that some packet's decode time differs from its presentation time and the plan
reports `ReorderedStream`. Nothing Clipped's own encoders produce is reordered.

## Audio

An output audio track is copied when it is **one recorded stream played at the
level it was recorded at**. Everything else is a mix — it has to be produced
sample by sample, which needs a decoder and an encoder:

| Track | What happens |
| --- | --- |
| One input, `gain_db` of 0, not muted, not silenced by another track's solo, no fades | copied |
| Two or more inputs | mix (`SeveralInputs`) |
| Any other level | mix (`Level`) |
| Muted, or silent because something else is soloed | mix (`Silenced`) |
| Fading in or out | mix (`Fades`) |

Mute and solo are resolved by `EditDocument::track_output`, so the export cannot
disagree with the editor about which tracks are audible
([docs/editing.md](editing.md#audio)). A silenced track is a *mix* and not a
missing track: the clip has that track, it is simply silent, and dropping it
would write a file with fewer tracks than the clip has.

**A document that declares no audio tracks at all carries the recording's audio
as it was recorded** — every stream, in the container's order, with the name the
recording gave it. That is what an edit which says nothing about audio means. An
instant clip (`EditDocument::from_recording`) declares one source, one segment
and no mix, and a clip of a match that arrived silent would be worse than one
that sounds like the recording.

Audio is cut on packet boundaries, so each end of a segment is accurate to one
audio packet — 20 ms for the packet sizes Clipped records at, and less for
uncompressed audio written in smaller blocks.

## What the export is measured against

`crates/export/tests/an_export_matches_the_timeline.rs` is the measurement, and
it is deliberately not marking its own homework: the expected picture times are
read out of the **source recording** with `ffprobe` and moved onto the output
timeline by the rule above, and the exported file's picture times are read out
of the export the same way. Neither list comes from `clipped-export`.

The measured agreement, on the fixtures those tests build:

- **No picture is lost, none is duplicated, and none moves by a frame.** The two
  lists are the same length and are compared one for one.
- **Timestamps agree to within one millisecond**, which is the resolution
  Matroska stores them at rather than slack the exporter needs. A picture at
  1.2345 s is stored as 1.234 s or 1.235 s and no writer can do better.
- **The coded bytes are the recording's own.** Each packet's payload is hashed
  in both files and compared, which is what distinguishes a copy from a
  re-encode that happened to produce the same number of frames.

That is the tolerance
[issue #84](https://github.com/wildware-uk/clipped/issues/84) asks for.

## Progress, cancellation and what is left behind

`ExportOptions` carries a progress callback and a `Cancellation`. Progress is
reported at most once per `ExportOptions::every` of *output written* — a
quarter of a second by default, so a few reports a second for a copy rather
than one per frame — and the callback runs on the exporting thread.
`Cancellation` is an atomic flag any thread may set; the export reads it between
packets, which bounds how long a cancel takes at one packet's write.

**Nothing partial is ever left on disk.** A cancellation, a read failure and a
write failure all drop the writer and remove the file. That matters twice: half
a clip is not a clip, and `MkvWriter::create` refuses a destination that already
exists rather than truncating it (AGENTS.md section 56) — so a partial file left
behind would make that name permanently unusable.

Nothing here overwrites anything. A destination that is taken is refused, and
the file that was there is untouched.

## Where an export runs, and what it must not disturb

An export must not degrade a recording that is in progress. Three things carry
that today:

- **It is not on the capture path.** `export` blocks for as long as the copy
  takes and is documented as something the caller runs on a thread that is not
  capturing (AGENTS.md section 20). It shares no state with a session: it reads
  a finished recording, which nothing is writing, and writes a new file nothing
  else knows about.
- **A copy does no encoding at all.** It runs no decoder and no encoder, so it
  competes with a recording for disk bandwidth and for nothing else — in
  particular not for the GPU's encoder, which is the resource a recording cannot
  spare.
- **It holds no lock a recording needs**, and the recording it reads is a
  different file from the one a session is writing.

What is **not** in place yet is a thread priority. `clipped-waveform` lowers its
worker to background priority for exactly this reason — on Windows
`THREAD_MODE_BACKGROUND_BEGIN` lowers I/O priority as well as CPU priority, and
this work is as much disk as CPU — and that helper is private to that crate. It
belongs somewhere both can reach it. Until then the caller owns the decision,
and the claim that an export does not cost a recording frames is **unmeasured**;
see "What is not built yet".

## What is not built yet

**Re-encoding.** An edit that cannot be copied is refused with
`ExportError::ReencodeRequired`, which names every blocker. Nothing is written.
That is the honest form of the gap (AGENTS.md section 54): a cut between
keyframes, a speed change, a crop, an overlay, a joined recording and any audio
mix are all currently refusals rather than slow exports. Re-encoding needs a
decoder, a scaler, an audio mixer and the encoder settings
[issue #90](https://github.com/wildware-uk/clipped/issues/90) owns, and it is
where the target resolution, framerate, codec and quality of SPEC.md section 19
will be applied.

**Export settings.** Resolution, framerate, codec and quality are
[issue #90](https://github.com/wildware-uk/clipped/issues/90). A copy has no
settings by definition — it writes what the recording holds — so the only
setting that means anything today is the destination path.

**The MP4 destination.** An export writes Matroska. `clipped_muxer::remux_to_mp4`
turns one into an MP4 without re-encoding, and wiring the two together is a
caller's decision rather than something this crate does on its own.

**A measurement of an export during a recording.** The reasoning above says why
it should not cost frames; nothing has measured it, and a performance claim
without a measurement is not a claim (AGENTS.md section 19).
