# Replay buffer

**Status: stub. The replay buffer does not exist yet.** It is built in the M3
milestone (SPEC.md section 42). No buffering code exists in the workspace, so
this page states what the document will contain rather than describing
behaviour that has not been written (AGENTS.md section 7).

Until this document is written, the intended design is SPEC.md section 16: keep
a rolling window of continuously encoded media segments rather than raw frames,
retain the segments a saved clip needs, and continue capturing without
interruption. `crates/session/src/lib.rs` and `crates/muxer/src/lib.rs` state
which crate owns which part.

## What this document will cover

Written during M3, alongside the code:

- The segment model: how long a segment is, what a segment contains, and why
  encoded segments rather than raw frames
  ([issue #35](https://github.com/wildware-uk/clipped/issues/35)).
- Retention: how the configured window maps to segments held, and when a
  segment is discarded.
- Where the buffer lives — memory, disk, or both — and how long durations are
  supported without the footprint scaling with resolution
  ([issue #36](https://github.com/wildware-uk/clipped/issues/36)).
- Saving a clip: identifying the segments that cover the requested range,
  retaining them against the cleanup that is running concurrently, assembling
  the output, and doing all of it without a gap in capture
  ([issue #37](https://github.com/wildware-uk/clipped/issues/37)).
- Keyframe alignment, and what the boundaries of a saved clip are when the
  requested range does not land on one.
- How every audio track survives into a saved replay
  ([issue #40](https://github.com/wildware-uk/clipped/issues/40)).
- Interaction with a full-session recording running at the same time, and with
  automatic highlight clipping in M10.
- Failure behaviour: what a save does when the disk is full, when the buffer is
  shorter than the requested duration, and when the hotkey is pressed twice in
  quick succession.
- How to exercise it: the `recorder replay` command
  ([issue #38](https://github.com/wildware-uk/clipped/issues/38)) and the global
  hotkey service ([issue #39](https://github.com/wildware-uk/clipped/issues/39)).

Related decisions: [ADR 0001](adr/0001-mkv-archival-container.md), because
segments are standard containers rather than an application-specific format.
