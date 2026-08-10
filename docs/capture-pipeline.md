# Capture pipeline

**Status: stub. The capture pipeline does not exist yet.** It is built in the
M1 milestone (SPEC.md section 42). `crates/capture`, `crates/encoder` and
`crates/muxer` currently contain module documentation and no code, so there is
no behaviour to describe here, and inventing some would be worse than leaving
the page short (AGENTS.md section 7).

Until this document is written, the authoritative statements about the pipeline
are the crate-level documentation in `crates/capture/src/lib.rs`,
`crates/encoder/src/lib.rs` and `crates/muxer/src/lib.rs`, which say what each
crate is and is not responsible for, plus SPEC.md sections 8 to 10 for the
intended behaviour.

## What this document will cover

Written during M1, alongside the code, and answering the questions in
AGENTS.md section 47 for this subsystem:

- The path a frame takes from the capture API to a packet in the container, and
  which thread owns each stage.
- The capture backends — game capture, Windows Graphics Capture, Desktop
  Duplication — how one is selected automatically, and what happens when the
  selected backend fails mid-session (SPEC.md section 8,
  [issue #11](https://github.com/wildware-uk/clipped/issues/11)).
- Target selection: how a window or monitor is chosen and what happens when the
  target moves, resizes, changes resolution or disappears.
- The capture clock, timestamping, and the model that keeps audio and video in
  sync ([issue #22](https://github.com/wildware-uk/clipped/issues/22)).
- Encoder selection across NVENC, AMF, Quick Sync and the software fallback,
  and how capability detection decides
  ([issue #14](https://github.com/wildware-uk/clipped/issues/14)).
- Buffer ownership and back-pressure: what happens when the encoder cannot keep
  up, and which frames are dropped when they must be.
- How to run a capture from the command line, and how to test one without a
  game — the controlled test applications in `tests/capture`
  ([issue #23](https://github.com/wildware-uk/clipped/issues/23)) and the media
  validation harness ([issue #24](https://github.com/wildware-uk/clipped/issues/24)).
- The assumptions it makes about GPU, display and driver behaviour.

Related decisions: [ADR 0001](adr/0001-mkv-archival-container.md) for the
container the pipeline writes into.
