# 0003. Process-specific audio capture is the basis for track separation

- Status: Accepted
- Date: 2026-08-10
- Issue: [#6](https://github.com/wildware-uk/clipped/issues/6)

## Context

SPEC.md section 11 calls true multi-track audio a core architectural
requirement, and SPEC.md sections 44 and 46 make it the reason to use this
application rather than one of the several mature alternatives. The promise is
that a finished recording contains the game, the rest of the system, the
microphone and optionally individual applications as independent tracks, with
no virtual cables and no manual routing.

That promise has a specific consequence: separation has to happen at capture
time. AGENTS.md section 21 forbids silently combining sources the user expects
to remain isolated, and SPEC.md section 44's retrospective editing — noticing
hours later that Discord was too loud and fixing it in the edit — is only
possible if Discord was never mixed into anything in the first place.

Windows provides a mechanism for this. Application loopback capture can be
scoped to a process and its child process tree through
`ActivateAudioInterfaceAsync`, and it supports the inverse: capture everything
*except* a given process tree (SPEC.md section 11). Those two modes are exactly
the shape of the two hardest tracks — "the game" and "everything but the game".

The scoping is by process *tree* rather than by process because games are not a
single process. They start under a launcher, spawn helpers, and sometimes the
process that produces audio is not the one whose window is being captured.

## Decision

Audio tracks are captured **separately at the source, using Windows
process-scoped loopback capture**, and never separated after the fact.

The track model that follows from this:

| Track | Captured as |
| --- | --- |
| Game | Process-scoped loopback, including the game's process tree |
| Other system audio | Process-scoped loopback, excluding the same tree |
| Microphone | Its own capture client on the selected input device |
| Per-application (Discord, music, browser) | A further include-scoped stream per configured application |
| Compatibility mix | Mixed from the above at record time |

No component ever mixes two sources into one track except the compatibility mix
track, which exists solely so that a casual player sounds correct
(SPEC.md section 13) and is additional to the isolated tracks, never a
replacement for them.

## Alternatives

### Mix at the endpoint, separate afterwards

Capture the ordinary desktop loopback — the single mixed stream every recorder
can get — and recover the individual sources later, whether by source
separation, by correlating against per-application peak meters, or by a learned
model.

Its case is not trivial. It is one capture stream, so it works on every version
of Windows, needs no special API, has no clock drift between tracks because
there is only one clock, and costs the least CPU of any option here. It would
also work for audio that cannot be attributed to a process at all.

It was rejected because mixing is lossy and not invertible. Once gunfire and a
voice are summed into the same samples, no amount of processing returns the
original voice at a quality anyone would put into an edit; the best available
result is an estimate with artefacts. Shipping that as "separate tracks" would
be a claim the output does not support, and every downstream feature that
assumes isolation — per-track volume in the editor, muting a source, the
retrospective editing story — would inherit the estimate's errors. It fails
AGENTS.md section 21 on its plain meaning.

### Virtual audio device drivers

Install virtual audio endpoints, have the user route each application to one,
and capture each endpoint separately. This is how the problem was solved before
Windows offered process loopback, and it demonstrably works: several shipping
products are built on it.

Its case is real. It works on older Windows, it captures anything that can be
pointed at an output device, and the separation is genuinely perfect because
the sources were never mixed.

It was rejected on cost and on risk to the user's machine. A virtual audio
device is a driver package: it needs driver signing, an elevated installer that
modifies the system's audio configuration, and it introduces a class of failure
where the user's default playback device is left pointing at a virtual cable —
a state in which their computer has no sound and the cause is not obvious.
Uninstalling has to unpick all of it correctly, on machines that may have been
through several Windows updates since. That is a heavy commitment for an
open-source utility, and it is only defensible if there is no alternative. It
also requires per-application routing to be configured by hand, which
contradicts the zero-configuration principle in SPEC.md sections 2 and 44: the
product's claim is that it works without virtual cables, so building it on
virtual cables would be self-defeating.

### Hooking the game's audio API

Intercept the game's own audio calls in-process to obtain its output directly.

Rejected without much deliberation, and recorded here so that nobody proposes
it again: it requires injecting into a running game, which is indistinguishable
from cheating to an anti-cheat system. AGENTS.md section 34 puts a user's game
account above richer functionality, and this would risk it.

## Consequences

- **The product's central feature is probably Windows 11 only.** Microsoft
  documents process-scoped loopback as available from build 20348 onwards. No
  shipping consumer Windows 10 release reaches that number: Windows 10 22H2 is
  build 19045, and 20348 is Windows Server 2022, whose client-side equivalent is
  Windows 11 (22000 and above). If 20348 is the true floor, then true
  multi-track audio — which SPEC.md section 11 calls a core architectural
  requirement and SPEC.md sections 44 and 46 make the reason to use this
  application at all — is unavailable on **every** Windows 10 machine, and
  Windows 10 users get the degraded single-track mode below. That materially
  narrows the SPEC.md section 3 target of "Windows 11 / modern Windows 10"
  rather than formalising it, and it is a cost this decision imposes on a
  population of users, not a technicality. The floor is stated from
  documentation and has not yet been confirmed on real hardware; confirming it
  — including whether any Windows 10 servicing update backported the API — is
  part of the M2 audio work, and the answer belongs in
  [prerequisites.md](../prerequisites.md) with the version numbers spelled out.
  If it is confirmed, the limitation has to be stated plainly wherever a Windows
  10 user might otherwise expect separate tracks, rather than being discovered
  after the fact by someone whose tracks all came out identical.
- **Behaviour below that floor must degrade, not fail.** On a machine without
  the API — on current information, any Windows 10 machine — the honest outcome
  is a single system-audio track and an explicit statement that separation is
  unavailable, not silently mixing everything and labelling it "Game"
  (AGENTS.md section 27). That fallback is therefore a supported mode with its
  own tests, not an error path.
- **A compatibility mix track is required, not optional.** Some players pick
  one arbitrary track from a multi-track file, so a recording whose first track
  is the isolated game audio would sound wrong to a user who simply
  double-clicked it. The mix therefore has to be produced during recording
  ([issue #29](https://github.com/wildware-uk/clipped/issues/29)), which puts a
  real-time mixing stage in the capture path — a cost this decision creates and
  that the endpoint-mixing alternative would not have.
- **Several capture clients means several clocks.** Each stream has its own
  sample clock and device period, so resampling and drift correction are
  compulsory rather than a refinement, and A/V synchronisation has to be
  defined against a single capture clock
  ([issue #22](https://github.com/wildware-uk/clipped/issues/22),
  [issue #30](https://github.com/wildware-uk/clipped/issues/30)).
- **Process tree resolution becomes a subsystem of its own.** The tree is not
  known once at start: children appear and exit during a session, so membership
  is re-evaluated as processes come and go
  ([issue #25](https://github.com/wildware-uk/clipped/issues/25)).
- **Getting the tree wrong is a silent correctness bug.** "Other system audio"
  is defined as the complement of the game tree, so a missed child process puts
  game audio into the system track, and nobody notices until they open the file
  in an editor days later. This is the main reason the audio tests generate
  known tones and assert isolation by frequency
  ([issue #34](https://github.com/wildware-uk/clipped/issues/34)) instead of
  relying on listening.
- **Double capture has to be prevented explicitly.** An application given its
  own track must be excluded from the "other system audio" complement, or it
  appears twice and is doubled in the compatibility mix.
- **More concurrent capture clients cost CPU and memory** in the process that
  has a 3% budget (SPEC.md section 38), and the cost scales with the number of
  configured tracks. It has to be measured with a realistic track count rather
  than with the minimum.
- **Some audio has no obvious owner.** System notification sounds and audio
  produced by service processes may not attribute to a capturable tree. Where
  each ends up is a question to answer with observation and then document in
  [audio-routing.md](../audio-routing.md).
