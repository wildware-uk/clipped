# Encoder capability detection

Clipped picks an encoder for the user (SPEC.md section 9, "Automatic"), so it
has to know what the machine can do. This document is about **how it finds
out**, because the method decides what the answers are worth.

Everything here is implemented in `crates/encoder` and reported by
`clipped-recorder capabilities` ([recorder-cli.md](recorder-cli.md)).

## The problem in one sentence

The two obvious ways of answering "does this GPU encode AV1?" both fail, in
opposite directions.

| Method | What it gets right | How it fails |
| --- | --- | --- |
| A table keyed on the GPU model | Instant, and works with no driver interaction | Wrong the moment a driver changes. AV1 encoding arrived on hardware that had already shipped; so did the bugs that took features away again. A table cannot know which driver is installed. |
| Opening an encoder session and asking it | The truth, and the only way to learn bitrate ranges and real limits | Allocates GPU memory and takes an encode session slot — from a game that may be mid-match. Slow enough to be felt at start-up. |

A user who selects AV1 because a table said so, and then loses a recording
because the driver disagrees, has been lied to. A user whose game stutters
because Clipped opened three encoder sessions to draw a settings screen has been
robbed. Neither is acceptable, so this crate does neither by default — and when
it does open a session, it is because the user asked it to.

## What Clipped does instead

**Measure what can break a recording. Infer only what shapes a warning. Say
which is which, everywhere.**

Three measurements, none of which opens an encoder session:

| Question | How it is answered | Where |
| --- | --- | --- |
| Which adapters are present, from which vendor, on which driver version? | DXGI: `IDXGIFactory1::EnumAdapters1`, and `CheckInterfaceSupport` for the user-mode driver version | `src/windows/dxgi.rs` |
| Will each vendor's encoder runtime load? | `LoadLibraryEx` with `LOAD_LIBRARY_SEARCH_SYSTEM32`, then released immediately | `src/windows/runtime.rs` |
| Which codecs does the installed display driver register a hardware encoder for? | Media Foundation: `MFTEnumEx` over `MFT_CATEGORY_VIDEO_ENCODER`, filtered to hardware, asked once per output codec | `src/windows/media_foundation.rs` |

And a fourth that does, on request only: **asking the encoder itself**, which is
the only way to learn the numeric limits and is described in
[its own section](#asking-the-encoder-itself) below.

The third is the important one for **which codecs**. Every hardware encoder on
Windows registers a Media Foundation transform per codec it produces, and
enumerating them asks the driver that is installed *right now* — which is
exactly the property a table keyed on a GPU model cannot have. It does not
activate the transforms, so nothing is allocated on the GPU and no session slot
is taken.

The second is the one that decides **whether the encoder is available at all**,
and the two questions are kept apart deliberately:

> **The vendor runtime decides availability. The transforms decide codecs.**

Clipped encodes through `nvEncodeAPI64.dll`, `amfrt64.dll` and Intel's media
runtime (issues #15 to #17), not through the Media Foundation transforms. So a
library that will not load is an encoder that will not work, whatever else the
system says. A driver that ships its media transforms without its encode SDK
runtime — which happens, and is what a partly installed driver looks like — is
reported as *unavailable, the adapter is present but its encoder runtime is
not*, with the transforms still listed underneath as the evidence for why that
answer is surprising. Calling it available would be reporting a capability the
recording would not have, which is the failure this whole crate exists to
prevent. A library that is present and will not load is reported differently
again, because a broken driver and an absent one need different fixes.

### Which adapter an encoder runs on is a guess

Nothing measured here says it. `MFT_ENUM_ADAPTER_LUID` is documented as an input
filter for `MFTEnum2` — "use this attribute when calling MFTEnum2 to enumerate
MFTs associated with a specific adapter" — not as an attribute stored on an
activation object, and it is absent from all seven transforms on the machine
this was developed on. So an encoder is attributed to its vendor's adapter,
preferring the one with the most video memory of its own. That is right whenever
a machine has one card per vendor; a machine with two cards from one vendor and
only one of them encoding is beyond what this can tell, and the report prints
which adapter it picked rather than implying it knew.

## Asking the encoder itself

Maximum resolution, the framerate ceiling, B-frame support and 10-bit (HDR)
support cannot be answered without an encoder session. Since
[issue #133](https://github.com/wildware-uk/clipped/issues/133) Clipped can open
one and ask, and `clipped-recorder capabilities --refresh` is what does it.

**Nothing else opens a session.** A plain `capabilities`, and anything that
detects at start-up, ask only the three cheap questions and report the published
limits, marked. The choice is a parameter — `Probing::WithoutSessions` or
`Probing::WithSessions` (`src/probe.rs`) — passed by the caller rather than
decided inside the crate, because the caller is the only one that knows whether
a user asked. There is exactly one caller that passes `WithSessions`, and it is
a subcommand that records nothing, which is how the promise that this never
happens during a recording is kept.

The answers are then cached like any other part of the report, so the machine
pays for a session once per refresh rather than once per question.

### What each vendor answers

| | NVENC | AMF | Quick Sync |
| --- | --- | --- | --- |
| Codec support | `nvEncGetEncodeGUIDs` — measured, including a measured **no** | whether the encoder component can be created — measured, including a measured no | not measured |
| Maximum resolution | `NV_ENC_CAPS_WIDTH_MAX` / `HEIGHT_MAX` | `AMFCaps::GetInputCaps` → `GetWidthRange` / `GetHeightRange` | not measured |
| Framerate ceiling | **deliberately not measured** (below) | `MaxThroughput` / `HevcMaxThroughput`, in macroblocks a second | not measured |
| B-frames | `NV_ENC_CAPS_NUM_MAX_BFRAMES` | `BFrames` (H.264 only; AMF's HEVC encoder has no such capability) | not measured |
| 10-bit | `NV_ENC_CAPS_SUPPORT_10BIT_ENCODE` | `HevcMaxProfile` ≥ `Main10` (HEVC only; below) | not measured |

The queries are made on a session that is never *initialised*: NVENC's
capability queries answer from an open session, and `AMFComponent::GetCaps`
answers before `Init`, so nothing is configured, no surface is allocated and no
picture is coded. Each vendor is asked on the adapter the report attributes its
encoder to, through a Direct3D 11 device created for the purpose and destroyed
with it (`src/windows/device.rs`).

Two gaps are deliberate:

- **AMF and 10-bit H.264.** `AMF_VIDEO_ENCODER_PROFILE_ENUM` contains no 10-bit
  profile at all — it ends at Constrained High — so no comparison against
  `MaxProfile` can answer the question, and the published `no` stands. HEVC's
  enumeration is `Main` then `Main10`, so there the comparison means what it
  looks like it means.
- **AMF and AV1.** This backend creates no AV1 component
  ([#165](https://github.com/wildware-uk/clipped/issues/165)), so AMF is never
  asked about AV1 and the row stays `unknown` rather than becoming a measured
  no.

**Quick Sync is not measured at all, and that is not an oversight.** oneVPL
describes an implementation's limits and this project cannot run a line of that
code: there is no Intel GPU on the machine Clipped is developed on, so a
measurement path written for it could never have been executed, and a path
nobody has seen return a value is a claim rather than a measurement. Every Quick
Sync limit therefore remains inferred and marked.

### Why NVENC's framerate ceiling is not published

NVENC answers `NV_ENC_CAPS_MB_PER_SEC_MAX`, and the answer is not a ceiling.

| | |
| --- | --- |
| What the driver reports | 983,040 macroblocks a second — **121 frames a second at 1080p** |
| What the hardware does | 1280x720 at **1,034 frames a second**, which is 3.7 million macroblocks a second |
| Measured on | GeForce RTX 4090, driver 32.0.16.1074, through this project's own NVENC backend, synchronously, release build |

Publishing the driver's figure as a *measurement* would tell a user wanting to
record at 240 frames a second that their encoder cannot, in a cell with no `(i)`
beside it to warn them — the exact failure this crate exists to prevent, with
the evidence pointing the other way. So the codec level's ceiling stays in its
place, marked inferred, and is at least honest about being an upper bound.

AMF's `MaxThroughput` is published, because it survives the same check: it
reports 2.8 million macroblocks a second for H.264 on the integrated Radeon
here, and the same measurement reaches 2.1 million — under the ceiling, where a
ceiling belongs.

### What is still inferred

Everything the vendors do not answer, everything on a machine nobody has run
`--refresh` on, and every Quick Sync row. Those come from `src/reference.rs`:
vendor documentation for the resolution and feature limits, and the codec
standards' own level limits for the framerate ceiling — H.264 Level 6.2 (ITU-T
H.264 Table A-1), HEVC Level 6.2 High tier (ITU-T H.265 Table A.8), AV1 Level
6.3 (AOMedia AV1, Annex A).

The rule where the two meet is one sentence, and `src/detection.rs` has one
function for it: **a measurement replaces the published limit and the absence of
one leaves it alone.** A session that opened and answered two of five questions
does not turn the other three into measurements.

Every inferred number cites a source, and where there is no source there is no
number: the software encoder's row states only that it encodes H.264, and leaves
its maximum resolution, B-frames and 10-bit `Unknown`, because there is no
vendor and no published limit to infer from.

Two honest caveats, which the report repeats:

- An **inferred** framerate ceiling is what the codec permits, not what the
  silicon can sustain. HEVC Level 6.2 allows over two thousand frames a second
  at 1080p; no encoder does that. A **measured** one is the encoder's own answer
  about its throughput, which is a different and better claim — and still not a
  promise that a recording will keep up, because the capture, the disk and the
  machine have ceilings this knows nothing about.
- A resolution ceiling is an upper bound whether measured or inferred. A picture
  inside it is not guaranteed to be accepted; only opening a session at that
  size settles that.

"HDR" here means 10-bit encoding, which is the necessary condition for it. The
colour signalling that makes a 10-bit stream an HDR one belongs to the muxer,
which does not write it yet
([issue #146](https://github.com/wildware-uk/clipped/issues/146)).

### The rule that keeps the table honest

**No entry in the reference table claims HEVC or AV1 support.** Both are
`Unknown` there, and become `true` only when Windows reports a hardware encoder
for them. H.264 is the single exception, inferred as supported for every encoder
family, because every generation of NVENC, AMF and Quick Sync silicon their
runtimes will load on has it.

So the failure mode this whole design is shaped around cannot happen: a user
cannot pick AV1 on the strength of a table entry, because there is no table
entry that says yes to AV1. `no_table_entry_claims_hevc_or_av1_support` in
`src/reference.rs` fails the build if somebody adds one.

### Measured, inferred, unknown

Every capability is a `Claim<T>`, which is `Measured(value)`, `Inferred(value)`
or `Unknown`. The evidence travels with the value rather than in a comment
beside it, so printing a claim prints its qualifier:

```text
codec  supported   max size         max fps at 1920x1080  B-frames   10-bit
AV1    unknown     —                —                     —          —
HEVC   yes         8192x4352        300                   no (i)     yes
H.264  yes         4096x2160 (i)    522 (i)               unknown    no (i)
```

`yes` was measured. `(i)` was inferred. `unknown` means nobody here knows —
deliberately not collapsed into "no", because "we did not measure this" and
"your GPU cannot do this" are different answers and one of them is a lie.

The two rows above are the AMD encoder before and after a `--refresh`: the HEVC
limits came from the encoder itself and lost their markers, and the one beside
`no` for B-frames stayed, because AMF's HEVC encoder has no B-frame capability
to ask about.

A row the encoder will not produce prints no limits at all, whether that is
`unknown` or a measured `no`. The limits are inferred from the encoder family's
documentation, so putting `8192x4352 (i)` and `yes (i)` for 10-bit next to a
codec that may not exist on this machine invites exactly the reading the whole
design is trying to prevent.

Anything that acts on a capability, rather than printing it, should ask
`Claim::is_measured_true`. The ranking in `src/recommendation.rs` does: it will
choose a measured H.264 over an inferred AV1 every time.

## What "Automatic" chooses

`recommend` returns every available encoder, best first, ordered by:

1. An encoder this build can open before one it cannot. Detection reports what
   the machine has; `EncoderKind::is_implemented` says which of those Clipped
   has a backend proven to drive, and a family it rejects ranks below the
   software fallback rather than above it. It is still listed, marked and with
   its issue named — a user whose GPU has an encoder Clipped cannot yet use
   should be told — but nothing opens it, and `Recommendation::for_opening`
   cannot return it. See
   [encoder-pipeline.md](encoder-pipeline.md#choosing-an-encoder-to-open).
2. Hardware before software. The recorder runs alongside a game and CPU time is
   the scarcest resource on the machine (AGENTS.md section 18).
3. An adapter with video memory of its own before one that shares system memory.
4. Then the most video memory — the tie-break between two adapters that both
   have some.
5. Then the order SPEC.md section 9 lists: NVIDIA, AMD, Intel.

Within an encoder, the codec is the most efficient one whose support was
measured, falling back to H.264.

Rules 3 and 4 exist instead of "prefer the discrete GPU" because DXGI cannot
tell you which adapter is discrete. It reports how much video memory an adapter
has of its own, and an AMD APU with a BIOS carve-out reports gigabytes of it —
the machine this was developed on does exactly that. So the ranking uses the
number DXGI actually gives, and `AdapterKind` says "own video memory" or "shared
system memory" rather than a word it would have to guess.

The list is never empty. The software encoder is available on every machine,
which is what makes "Automatic" a setting that always has an answer.

## The cache

| | |
| --- | --- |
| Where | `%LOCALAPPDATA%\Clipped\encoder-capabilities.json` |
| Key | Every adapter's identifier, vendor, device and **driver version**, plus the **detection revision** |
| Format | JSON, with a `format` number that this build must recognise |
| Written | Atomically: to a temporary file named for the writing process, then renamed into place |

Detection splits into a cheap half and an expensive half for the cache's sake.
Enumerating adapters is a DXGI call; finding encoders means starting Media
Foundation and loading vendor runtimes. The cheap half runs every time and
produces the key, so a cache hit skips the expensive half without ever trusting
the cache about what hardware is present.

The driver version is in the key because a driver update is the event most
likely to change the answer. Adding, removing or swapping a card changes the
rest of it.

`DETECTION_REVISION` is in the key because the machine is not the only thing
that can make a stored report wrong. The reference table and the rules that read
it are Clipped's own content: correcting a published limit, or tightening an
availability rule as this one was, changes the report for hardware that has not
changed at all. Without that number the one thing guaranteed to change would be
the one thing that could never invalidate the file, and an installation would
serve the old answer until its GPU was replaced. **Any change that would make
detection answer differently for the same machine must increment it.**

The temporary file is named for the process that writes it. A fixed name is
shared by every process writing the same cache, and two of them interleaving
write and rename can leave a truncated file where a finished one should be —
which is not the atomicity this table claims.

**When it is stale, corrupt, missing or unwritable, nothing fails.** All four
mean the same thing: the cache does not answer, the machine is asked again, and
the file is overwritten. Each is logged with its reason, so a machine that never
caches says so in the diagnostics instead of just being slow. A recorder that
refused to report your GPU because a file in `%LOCALAPPDATA%` was truncated by a
power cut would be choosing its own bookkeeping over the user (AGENTS.md section
17).

`clipped-recorder capabilities --refresh` ignores what is stored and replaces it.

### Why measuring the limits did not change the key

The stored report now depends on something outside the key — whether the run
that wrote it opened an encoder session. That does not make the key wrong. A
measured report and an inferred one are both true of the same machine; one
simply says more. Everything that could make a *measurement* stale is already in
the key, because an encoder's own limits change when the adapter or its driver
changes and at no other time.

One consequence is worth stating plainly, because it surprises: a plain
`capabilities` run after a driver update finds the cache stale, re-probes
without opening a session, and replaces measured limits with published ones.
That is correct — the measurements described the previous driver — and another
`--refresh` takes them again. The report says so itself: while an available
hardware encoder has never been asked, the footer names the command that would
ask it. It stops saying so once every encoder has been asked, even though some
limits are still marked — NVENC's framerate ceiling always will be, and a
suggestion that would produce the same page again is a session slot spent for
nothing.

`DETECTION_REVISION` went to 3 for this change, because a machine that had
already cached would otherwise go on showing inferred numbers this build would
have measured.

## Diagnostics

Detection logs one line per adapter and one per encoder at `info`, and one per
codec at `debug`. The `encoder` field carries the standard vocabulary word —
`nvenc`, `amd_amf`, `intel_quicksync`, `software_h264` — so a search for
`encoder=nvenc` finds these lines and a recording session's alike
([logging.md](logging.md), "Standard fields").

```text
INFO clipped_encoder::detection: encoder detected encoder=nvenc available=true
  availability=available adapter="0000000000013516" measured_codecs="av1,hevc,h264"
DEBUG clipped_encoder::detection: encoder codec capability encoder=nvenc codec="av1"
  supported=true max_resolution=8192x8192 max_framerate_1080p=2269 (inferred)
  b_frames=true hdr=true
```

A run that opened sessions says what that cost, at `info`, because the argument
for caching it is an argument about a number:

```text
INFO clipped_encoder::detection: asked the encoder sessions for their own limits
  elapsed_ms=192 codecs=5
```

Adapter model names are logged. A GPU model is hardware, not user content, and
it is the single most useful thing in a bug report about encoding. The cache
path is redacted, because it runs through the user's account name (AGENTS.md
section 14).

## Testing this without the hardware

The reasoning is a pure function from `SystemFacts` to a report, and the
platform calls are behind `SystemProbe`. So the interesting machines — no
adapter at all, only the Basic Render Driver, an integrated GPU with a carve-out,
two vendors at once, a driver that registers media transforms but has no
runtime, a runtime that is installed and will not load — are all tested by
handing detection the facts those machines would report.

That is a deliberate limitation as well as a technique, and it is worth saying
plainly: **the no-hardware path has been tested by injection, not on bare metal.**
The machine this was developed on has an NVIDIA RTX 4090 and an integrated AMD
part, and neither can be removed.

The half that cannot be tested by injection is the session probe, because its
whole point is what a real driver says. Those tests live beside the backends —
`the_capability_queries_describe_every_codec_this_card_encodes` and its AMF
counterpart — assert properties rather than numbers, since a 4090 is not every
card, and skip on a machine with no GPU unless `CLIPPED_REQUIRE_ENCODER=1` makes
the skip a failure. `windows/mod.rs` holds the two that compare the probes
against each other: that an ordinary probe opens no session at all, and that
every claim a session did not answer is byte-for-byte the claim the cheap probe
produced.

The same goes for **Quick Sync, which is unverified on real hardware**: there is
no Intel GPU here, so `libvpl.dll`, `libmfx64.dll` and `libmfxhw64.dll` — the
list the backend itself loads from, shared with detection so the two cannot
drift — have only ever been observed as absent. The code path is the same one
NVENC and AMF take, and that is an argument, not a measurement.

## What this deliberately cannot tell you

- Whether a given resolution, framerate and bitrate will actually be accepted.
  The maximum size is now measured on request, and a size inside it is still not
  a promise: only opening a session *at that size and bitrate* settles it, and
  that belongs to the encoder backends.
- How fast an encoder really is. AMF's own throughput figure is reported where
  it gives one, and nothing here runs a frame through an encoder to time it —
  the numbers in "Why NVENC's framerate ceiling is not published" were measured
  by hand for that decision, not by anything that ships.
- Which codecs an encoder supports that it neither registers a Media Foundation
  transform for nor enumerates when asked. Without a `--refresh` those come back
  as `Unknown`, never as "not supported", because a driver that under-reports
  and a part that genuinely cannot encode look identical from here. The AMD row
  on the development machine says `unknown` for AV1 for a second reason too: the
  backend has no AV1 component to create, so AMF is never asked. In that
  particular case the conservative answer and the true one coincide — the part
  is the integrated RDNA 2 GPU in a Ryzen desktop processor (PCI `1002:13C0`),
  which decodes AV1 and does not encode it — and nothing in the report knows
  that, which is the point.
- Whether an available encoder can actually open a session, on a run that did
  not open one. Availability without `--refresh` means the vendor runtime
  loaded, which is a necessary condition and not a sufficient one. After a
  `--refresh` the answer is stronger for the encoders that were asked, because a
  session did open.
- Anything Quick Sync, beyond whether its runtime loads. No Intel GPU here.
- Anything at all on a machine that is not Windows. The crate builds and its
  reasoning is tested there; `capabilities` reports that it cannot ask.

## Where the code is

| File | What it holds |
| --- | --- |
| `src/claim.rs` | `Claim`, `Evidence` — the measured/inferred/unknown distinction |
| `src/codec.rs` | Codecs, encoder families, vendors, resolutions |
| `src/adapter.rs` | Adapters, driver versions, and what DXGI can honestly say about them |
| `src/probe.rs` | `SystemProbe`, `SystemFacts` — the seam between asking and reasoning |
| `src/reference.rs` | The table of published limits, and the rule that keeps it honest |
| `src/detection.rs` | `detect`, the report types, and the diagnostics |
| `src/recommendation.rs` | The ranking behind "Automatic" |
| `src/cache.rs` | The cache and its invalidation |
| `src/windows/` | The only code that calls a platform API |
| `src/windows/device.rs` | The Direct3D device a capability query is asked through |
| `src/windows/nvenc/mod.rs`, `src/windows/amf/mod.rs` | `measure_limits`, where an encoder is asked about itself |
