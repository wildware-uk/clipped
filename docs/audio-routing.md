# Audio routing

**Status: stub. Audio capture and routing do not exist yet.** Basic capture
arrives in M1 and true source separation in M2 (SPEC.md section 42).
`crates/audio` currently contains module documentation and no code, so there is
nothing implemented to document, and describing routing behaviour before it is
built would produce a page that is wrong from the day it is committed
(AGENTS.md section 7).

Until this document is written, the intended behaviour is specified in SPEC.md
sections 11 to 14, the constraints are AGENTS.md section 21, the decision that
shapes the whole subsystem is
[ADR 0003](adr/0003-process-specific-audio-capture.md), and the crate's remit is
stated in `crates/audio/src/lib.rs`.

## What this document will cover

Written during M2, alongside the code:

- The track model as actually implemented, and how a configured set of tracks
  becomes a set of capture streams.
- Process-scoped loopback capture in both directions: including a game's
  process tree, and excluding it to obtain everything else
  ([issue #26](https://github.com/wildware-uk/clipped/issues/26),
  [issue #27](https://github.com/wildware-uk/clipped/issues/27)).
- How a game's process tree is resolved and kept current as children start and
  exit ([issue #25](https://github.com/wildware-uk/clipped/issues/25)), and what
  happens to audio that cannot be attributed to a tree.
- Microphone capture, and the optional preservation of a raw pre-processing
  microphone track (SPEC.md section 14).
- Application-to-track routing configuration, how it is persisted, and how it
  behaves when a routed application is not running
  ([issue #33](https://github.com/wildware-uk/clipped/issues/33)).
- The compatibility mix: what is mixed into it, at what point, and how muting a
  source interacts with it
  ([issue #29](https://github.com/wildware-uk/clipped/issues/29)).
- Clock drift and sample-rate handling between independent capture clients, and
  how audio stays aligned with video over a multi-hour session
  ([issue #30](https://github.com/wildware-uk/clipped/issues/30)).
- Device changes during a recording: the default endpoint changing, a
  microphone being unplugged, a device appearing mid-session.
- Per-source processing — gain, mute, noise suppression, gate, compressor,
  limiter — and where in the chain each sits
  ([issue #31](https://github.com/wildware-uk/clipped/issues/31)).
- How to verify isolation: the tone-generator system tests in `tests/audio`
  ([issue #34](https://github.com/wildware-uk/clipped/issues/34)), which assert
  by frequency rather than by ear.
- The Windows version requirements the subsystem depends on, and how it behaves
  on a machine that does not meet them.
