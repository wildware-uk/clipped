# 0001. MKV is the archival recording container

- Status: Accepted
- Date: 2026-08-10
- Issue: [#6](https://github.com/wildware-uk/clipped/issues/6)

## Context

A recording is irreplaceable. The gameplay it captured is not going to happen
again, so the container it is written into is chosen for what happens when
things go wrong, not for what happens when they go right.

Two requirements set the constraints.

**Recordings must survive an abrupt termination.** AGENTS.md section 17
requires incremental writing, recoverable containers and tolerance of the
process being killed. This is not a rare case: the recorder is running while a
game is running, and games take machines down with them. A GPU driver reset, a
hard power loss, a full disk, or the user holding the power button because the
game locked up all end the process without a chance to finalise anything. A
session may be hours long, so "the file is unreadable" means hours lost.

**Recordings must stay usable outside Clipped.** AGENTS.md section 32 requires
that a user who stops using this application keeps their recordings, which
rules out anything proprietary or custom and pushes towards a widely
implemented standard container.

Two further requirements narrow the field. The output carries several
independent audio tracks with meaningful names — game, other system audio,
microphone, per-application tracks (SPEC.md section 11), plus the compatibility
mix (SPEC.md section 13) and optionally an unprocessed microphone track
(SPEC.md section 14) — and how many there are is decided at runtime from the
user's routing rather than fixed at three (SPEC.md section 44). The video codec
likewise varies with the machine: H.264, HEVC or AV1 from whichever hardware
encoder is present, or the software fallback (SPEC.md section 9). The audio
codec is not settled anywhere yet; it is decided by the M2 muxing work
([issue #28](https://github.com/wildware-uk/clipped/issues/28)).

## Decision

Recordings are written into **Matroska (MKV)**, incrementally, through the
FFmpeg libraries. MKV is the format the capture pipeline is designed around and
the format an archived session is stored in.

In practice:

- The muxer writes clusters as encoded packets arrive; nothing important is
  buffered until the end of a session.
- Every audio track keeps its name and language tag in the container, so the
  track layout survives into an editor without a sidecar.
- MP4 remains available as a user-selected recording format (SPEC.md section
  15), and the setting presents MKV as recommended. Choosing MP4 is choosing to
  give up crash resilience, and the UI should say so rather than presenting the
  two as equivalent.
- MP4 copies for sharing are produced by remuxing without re-encoding, as a
  separate step after recording, not by recording into MP4.

## Alternatives

### Fragmented MP4

Write ISO base media format with movie fragments, so that each fragment is
self-describing and the file does not depend on an index written at the end.

This is the strongest alternative and it is genuinely close. It solves the
crash problem: a truncated fragmented MP4 loses the fragment being written and
remains readable up to that point, which is the same failure profile as a
truncated MKV losing its last cluster. It is the format streaming
infrastructure is built on, browsers play it, and it would remove the need for
a separate remux step for anything that accepts fragmented input.

It lost on the other requirements rather than on resilience. ISO BMFF stores
only what has a registered mapping into it, so the choice of audio codec — a
choice this project has not made yet — would be constrained by the container
before it is made on its own merits, and a later codec change becomes a
question of what the format permits rather than a question of muxing. Matroska
imposes no such limit. Multi-track audio with per-track naming is handled
inconsistently by consumer players and editors, which is exactly the surface
this project cares about. And in practice a fragmented file is often still
remuxed to a plain MP4 before it is accepted by an upload target, so the cost of
fragmenting would be paid without removing the remux step it was meant to avoid.

What would make it win later: if measurement showed that editors handle
fragmented MP4's multi-track audio as well as they handle MKV's, the argument
for MKV would be mostly gone, because the remux step in
[issue #92](https://github.com/wildware-uk/clipped/issues/92) would become
unnecessary for most users.

### Plain MP4, written directly

Record into ordinary non-fragmented MP4.

Its case is strong and it is what most users would choose if asked: MP4 is
accepted everywhere, by every upload target, phone, chat client and editor, and
choosing it would delete a whole category of work from the project — no remux,
no explaining to users why their recording will not upload.

It was rejected because the index is written when the file is closed. A
recording interrupted before that point is not a shorter recording, it is an
unplayable file that needs third-party recovery tooling, and telling a user to
run an untrusted repair tool over footage they cannot recreate is not an
acceptable failure mode. That is a direct conflict with AGENTS.md section 17,
and it is the failure that happens precisely when the machine has just crashed
during something worth recording.

### A custom segmented format with a manifest

Write encoded packets into an application-defined segment format with a
sidecar manifest, and assemble a standard container on demand.

It would fit the replay buffer's segment model (SPEC.md section 16) neatly and
give complete freedom over what metadata is stored. It was rejected outright
under AGENTS.md section 32: it makes recordings depend on this application to
be readable at all, which is the specific outcome that section exists to
prevent. Segmenting still happens for the replay buffer, but the segments are
standard containers.

## Consequences

- **MKV is not universally accepted.** Several upload targets and chat clients
  reject it, and some editors do not import it. Producing a shareable MP4 is
  therefore compulsory rather than optional, and is tracked as
  [issue #92](https://github.com/wildware-uk/clipped/issues/92) in M11. Until
  that exists, sharing a recording means the user converts it themselves.
- **Remux is not always lossless in metadata terms.** Copying streams into MP4
  preserves the media, but track naming, some tags and, depending on the codec
  chosen, some audio streams do not survive. The remux work has to decide, and
  document, what it does when a recorded audio codec has no MP4 mapping.
- **Editor support has to be measured, not assumed.** SPEC.md section 45 makes
  the MVP conditional on opening a recording in a real editor and adjusting the
  tracks independently. Support for multi-track MKV differs between editors and
  between versions of the same editor. This is the largest open risk in this
  decision, and it is not one an automated test can retire: the isolation tests
  ([issue #34](https://github.com/wildware-uk/clipped/issues/34)) prove the
  tracks are separate in the file, not that DaVinci Resolve or Premiere will
  import them. It is retired only by a person opening real output in those
  editors. If the answer turns out to be no, this is the decision to revisit.
- **The muxer is tied to FFmpeg's Matroska implementation**, and so to whatever
  the FFmpeg dependency strategy turns out to be
  ([issue #7](https://github.com/wildware-uk/clipped/issues/7)).
- **Validation gets easier.** Because MKV carries named tracks, media tests can
  assert the expected stream layout directly from the file with `ffprobe`
  rather than inferring it (AGENTS.md section 22).
- **A recording killed mid-write is expected to be playable.** That is the
  claim this decision rests on, so it is a thing to test rather than assume:
  terminate the recorder during a session and assert the output still opens and
  has a plausible duration.
