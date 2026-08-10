# Plugin API

**Status: stub. The plugin API does not exist yet.** It is built in the M9
milestone (SPEC.md section 42). `crates/plugins` and `crates/events` currently
contain module documentation and no code, and there is no plugin contract to
document.

This stub matters more than the others, because a plugin API is a compatibility
surface: once it is published, third-party plugins depend on it and it cannot
be changed casually (AGENTS.md section 43). Publishing a speculative
description of it now would be actively harmful — contributors would build
against a shape that has not been designed.

Until this document is written, the intended shape is SPEC.md sections 21 to 23
(the universal event model and the `HighlightProvider` sketch), the hard
constraint is AGENTS.md section 34, and the crate remits are in
`crates/plugins/src/lib.rs` and `crates/events/src/lib.rs`.

## What this document will cover

Written during M9, alongside the code, and treated as the reference a
third-party plugin author works from:

- The `HighlightProvider` contract as implemented: its operations, its
  lifecycle, and what a provider may assume about the session it is attached to
  ([issue #69](https://github.com/wildware-uk/clipped/issues/69)).
- The universal event model, and how a game's native events are translated into
  it ([issue #68](https://github.com/wildware-uk/clipped/issues/68)).
- Event timing: how an event's timestamp relates to the recording it belongs
  to, and how much latency is tolerable before an event lands in the wrong
  place on the timeline.
- Discovery, loading, supervision and isolation: where plugins live, what
  happens when one crashes, hangs or floods the event channel, and why that
  must not affect a recording.
- The permitted integration techniques — official APIs, local telemetry, logs,
  Game State Integration, replay files — and the explicitly forbidden ones.
  Nothing that resembles injection or memory inspection is acceptable,
  regardless of what it would enable (AGENTS.md section 34).
- Network access by plugins, which must be visible and documented rather than
  incidental (SPEC.md section 39).
- Versioning and compatibility: how the contract changes, and what a plugin
  built against an older version can expect.
- How to write and test a plugin, using the Counter-Strike 2 integration
  ([issue #70](https://github.com/wildware-uk/clipped/issues/70)) as the worked
  reference.
