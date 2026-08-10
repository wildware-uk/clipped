# Encoder pipeline

**Status: the interface exists, and one backend does.** `crates/encoder` defines
the video encoder trait, the vocabulary a stream is described in, and — since
[issue #15](https://github.com/wildware-uk/clipped/issues/15) — an NVENC backend
that implements all of it for H.264, HEVC and AV1. A Windows build on NVIDIA
hardware can turn captured GPU textures into coded packets today.

What is missing is everything around it. AMF
([#16](https://github.com/wildware-uk/clipped/issues/16)), Quick Sync
([#17](https://github.com/wildware-uk/clipped/issues/17)) and the software
fallback ([#18](https://github.com/wildware-uk/clipped/issues/18)) are not
written, so a machine without an NVIDIA GPU can be told what it could encode
(see [encoder-capabilities.md](encoder-capabilities.md)) and cannot encode
anything. `clipped-muxer` does not mux yet
([#21](https://github.com/wildware-uk/clipped/issues/21)), and no session wires
capture to encoding ([#19](https://github.com/wildware-uk/clipped/issues/19),
[#20](https://github.com/wildware-uk/clipped/issues/20)), so `recorder record`
still reports that the capture engine is not implemented. Packets go into a
`Vec` in a test rather than into a file a user can play.

This document describes the interface, the rules a backend has to obey, and the
one backend that obeys them. Where it describes something that does not exist it
says so, because a document that quietly describes intentions as facts is worse
than a short one (AGENTS.md section 7).

## What it does

Takes a GPU texture that a capture backend owns, and produces coded packets with
timestamps, without the picture ever entering system memory.

```text
clipped-capture                 clipped-encoder                 clipped-muxer
───────────────                 ───────────────                 ─────────────
ID3D11Texture2D  ──submit──→  register / map / encode  ──→  EncodedPacket  ──→  file
   (14 MB, on the GPU)          (nothing is copied)          (~80 kB, in RAM)
```

## Why this shape

**A frame must not be copied.** At 2560x1440 and 60 frames a second, one
readback per frame is roughly 850 MB/s of memory traffic and a GPU/CPU
synchronisation in the middle of a game's frame. It would not fail; it would
just cost the user frames per second, silently. So the interface carries handles
rather than pixels, and the NVENC backend registers the capture backend's own
texture with the encoder (AGENTS.md section 18).

**A leaked encoder session is worse than a crash.** Consumer NVIDIA cards cap
how many encoding sessions may exist at once, across every application on the
machine. A recorder that leaks one takes it away from OBS, from Discord and from
its own next recording, until the process exits. So the session is owned by one
type, released in `Drop` as well as in `shut_down`, and the release path is
exercised by a test that opens and closes sixteen of them (AGENTS.md section
58).

**Timestamps decide whether the recording is watchable.** A frame's presentation
time comes from the capture timestamp and is carried through the encoder
untouched; an encoder that read a clock would record its own scheduling jitter
into the file. Timestamps that go backwards are refused rather than passed on
(AGENTS.md section 22).

**The vocabulary has to fit four encoders.** Everything in `EncoderConfig` is
something AMF, Quick Sync and a software encoder can also be told. Where a
vendor offers a knob nobody else has, the backend picks a defensible value and
documents it, rather than growing the configuration into the union of four
vendors' options.

## The interface

| Type | What it is |
| --- | --- |
| `EncoderConfig` | What to produce: codec, resolution, frame rate, `RateControl`, `KeyframeInterval`, `ColourSpace`, `SurfaceFormat`, `EncodePreset`. |
| `GraphicsDevice` | The device the frames live on, as an opaque handle. An encoder session belongs to one. |
| `SourceFrame` | One frame going in: a borrowed `SourceTexture`, its format and size, and a presentation time. |
| `VideoEncoder` | A live session: `submit`, `next_packet`, `finish`, `shut_down`. |
| `EncodedPacket` | One coded picture: bytes borrowed from the encoder, a presentation and a decode timestamp, and a `PictureKind`. |
| `EncodeError` | A failure, which always names the encoder, the codec and the resolution. |
| `NvencEncoder` | The NVENC implementation. Windows only. |

### Lifecycle

```text
NvencEncoder::open(device, config)
         ↓
    submit(frame) ──→ next_packet() ⇄ next_packet() → None
         ↑                                             │
         └─────────────────────────────────────────────┘
         ↓
     finish() ──→ next_packet() ⇄ ... → None
         ↓
    shut_down()
```

Submit a frame, drain every packet it produced, repeat. At the end call
`finish`, drain again, and shut down.

Draining before the next submission is part of the contract. An encoder holds a
bounded pool of output buffers, and a caller that submits without draining runs
it dry — at which point the encoder reports `OutputBuffersExhausted` rather than
blocking, because a capture thread that blocks is a capture thread dropping
frames (AGENTS.md section 20).

A submission does not always produce a packet. An encoder that reorders pictures
holds frames back until it has enough of them, so `None` is ordinary; the packets
appear after a later submission or after `finish`. The NVENC backend is
configured so that it cannot do that — no B-frames and no lookahead, because a
picture the encoder holds is a texture the caller cannot release — so each
submission produces exactly one packet and every packet's decode timestamp equals
its presentation timestamp.

### There is no factory

`clipped-capture` has a factory trait because a pure function picks a backend
from declarations without touching hardware. Encoding does not need one:
`recommend` already ranks encoder families from the capability report, and
opening a session needs a platform device handle no abstract factory could
supply. Each backend is constructed by its own function, and a dispatcher over
`EncoderKind` belongs in the change that has more than one backend to dispatch
to (AGENTS.md section 1).

## Ownership: who owns what, and for how long

1. **The encoder owns its session and everything allocated from it** — output
   buffers, resource registrations, the loaded runtime — from `open` until
   `shut_down` or `Drop`, whichever comes first. Both do the same work, so an
   unwind on the encoding thread cannot leak a session.
2. **The encoder owns nothing it is given.** The graphics device belongs to the
   caller and the texture belongs to the capture backend. A `SourceFrame`
   borrows the texture, and everything an encoder does with it happens inside
   `submit`: when `submit` returns, by any path including a failure, nothing
   derived from the handle is still held, and the caller may recycle the frame.

   This is an obligation on a backend, not an observation about one. A hardware
   encoder has to hand the driver something derived from the texture — NVENC
   registers and maps it — and releasing that may mean waiting inside `submit`
   for the picture to be coded. That is the price, and it is worth paying: a
   capture backend recycles frame pool surfaces, so an encoder still reading one
   after `submit` returns produces corruption that nothing in any log explains.
3. **A packet's bytes belong to the encoder** until the next `next_packet`,
   which is when the bitstream buffer is unlocked. The borrow of `&mut self`
   makes the compiler enforce it — the same contract `clipped-capture` uses for
   a captured frame, for the same reason.

## Threading

One encoder belongs to one thread. `VideoEncoder` is `Send`, so a session can be
opened on one thread and moved to the encoding thread, and deliberately not
`Sync`.

That thread is normally the capture thread, because a frame texture may not
outlive the capture that produced it. Packets are what cross to another thread:
they are bytes, and a muxer or a replay buffer can take as long as it likes with
a copy of them.

## The NVENC backend

### How it reaches NVENC, and why

Through NVIDIA's own interface, `nvEncodeAPI.h`, loaded from the driver at run
time with `LoadLibraryEx` and `GetProcAddress`. There is no SDK to install and
nothing to link against: `nvEncodeAPI64.dll` ships with the display driver, and
a machine without one simply fails to load it — an ordinary error, not a
build-time dependency.

Two other routes were considered:

| Route | Why not |
| --- | --- |
| Media Foundation's NVIDIA encoder transform | Reaches the same silicon through an operating system abstraction, which is attractive until the input format is considered: the hardware transforms take NV12, and captured frames are BGRA. Every frame would need a colour conversion pass — a shader or a video processor — before it reached the encoder. NVENC's own interface takes a BGRA surface directly and converts inside the encoder: one GPU pass per frame against none. |
| A binding crate from crates.io | The maintained ones wrap NVENC's CUDA path rather than its DirectX path, so they would pull in a CUDA runtime in order to avoid a copy the DirectX path never makes. |

### The bindings, and their licence

`crates/encoder/src/windows/nvenc/sys.rs` is generated by
[bindgen](https://github.com/rust-lang/rust-bindgen) from `nvEncodeAPI.h`. The
header is NVIDIA's, published under the **MIT licence** through FFmpeg's
[nv-codec-headers](https://github.com/FFmpeg/nv-codec-headers), which is what
makes it redistributable at all: the copy inside the Video Codec SDK carries
NVIDIA's SDK agreement instead, and that is not a licence `deny.toml` accepts
(AGENTS.md sections 11 and 12).

The generated file is a derivative of that header, so it carries NVIDIA's
copyright and the MIT permission notice at the top of itself, and the same
notice is recorded in
[THIRD-PARTY-NOTICES.md](../THIRD-PARTY-NOTICES.md) with the tag and checksum it
came from. `cargo deny` does not cover this: it inspects the Cargo graph, not
source committed into the tree.

The generated file is committed rather than produced by a build script, so that
building Clipped needs neither libclang nor a network fetch of a header, and so
that a change in the FFI is a reviewable diff rather than a silent difference
between two machines. Regenerating it is a deliberate act:

```bash
curl -sLO https://raw.githubusercontent.com/FFmpeg/nv-codec-headers/n12.0.16.2/include/ffnvcodec/nvEncodeAPI.h
# sha256: 808db2a21232839ee8a6057601f1964e90b20385f6dbf080f3cd33f6470c66c4
bindgen nvEncodeAPI.h \
  --no-layout-tests --no-doc-comments --use-core --no-prepend-enum-name \
  --allowlist-type "NV_ENCODE_API_FUNCTION_LIST|NV_ENC_.*|NVENCSTATUS|GUID" \
  --allowlist-var "NV_ENC_.*|NVENC_.*" \
  --blocklist-item "NV_ENC.*_GUID" \
  --blocklist-function "NvEncodeAPI.*" \
  -o crates/encoder/src/windows/nvenc/sys.rs \
  -- -target x86_64-pc-windows-msvc
```

Two things bindgen cannot generate, and which are therefore written by hand:

- **The codec, preset and profile identifiers.** They are `static const GUID`
  values in the header, which have no symbol to link against; they are
  transcribed into `settings.rs` beside the comment naming each one.
- **The structure version stamps.** `NV_ENC_CONFIG_VER` and its relatives are
  macro invocations, not constants. `api.rs` reproduces the macro and the
  revision numbers, and a unit test checks the arithmetic against hand-computed
  values — a wrong one produces `NV_ENC_ERR_INVALID_VERSION` from a call several
  steps away from the mistake.

### Which interface version, and which driver

This build compiles against interface **12.0** and asks the driver, before
anything else, what it supports; an older driver gets an error naming the
release to install (522.25 or newer) rather than a status code. Newer drivers
are fine, because NVENC is backwards compatible — the machine this was developed
on runs driver 610.74 and accepts the 12.0 structures.

Pinning to an older interface than the newest available is the trade: a few
features this project does not use, in exchange for working on every driver back
to October 2022.

### What is configured, and what is left to NVIDIA

NVENC's configuration structure has upwards of two hundred fields. The backend
starts from the preset configuration the driver returns — NVIDIA's own tuning
for the chosen preset — and overwrites only what Clipped has an opinion about:

| Setting | What the backend does |
| --- | --- |
| Preset | `EncodePreset::Speed`, `Balanced`, `Quality` map to NVENC's P1, P4 and P7, always with the high-quality tuning: Clipped writes to a local disk, so the low-latency tunings buy nothing. |
| Rate control | `RateControl::Bitrate` with no peak becomes constant bit rate with a one-second buffer, rather than NVENC's single-frame default, which holds the rate precisely and costs visible quality whenever the picture changes. With a peak it becomes variable bit rate. `RateControl::Quality` becomes variable bit rate with a target quality and no average to aim at. |
| Keyframes | The interval is written to `gopLength` *and* to the codec-specific IDR period. Setting only the first produces intra frames that are not cut points, which looks fine in a player and breaks every clip the replay buffer tries to save. |
| Parameter sets | Repeated at every keyframe. A clip cut from the middle of a recording begins at a keyframe, and a decoder handed a keyframe with no parameter sets in front of it cannot start (SPEC.md section 7). |
| Colour | The primaries, transfer function, matrix and range are written into the stream's VUI (or AV1's equivalent fields) as ITU-T H.273 code points. |
| B-frames | Off. A recorder gains a few per cent of compression and pays with reordered output, which means a decode timestamp that differs from the presentation timestamp and a muxer that has to reconstruct it. Revisit when there is a muxer that does. |

Everything else is NVIDIA's default, which is better tuned than a guess here and
maintained by the people who make the silicon.

### Colour

Captured frames are BGRA and coded pictures are 4:2:0 YUV, so something has to
convert. NVENC does it inside the encoder, as part of encoding — which is what
lets the BGRA texture go straight in.

The conversion and the tag have to agree, and a stream that is tagged one way and
converted another plays back with visibly wrong colour and nothing in any log to
say so. That is checked end to end rather than reasoned about: a test encodes
frames of pure red, green and blue, decodes them with FFmpeg, and asserts the
pixels come back the colours they went in as. Tagging BT.709 limited range —
the default — passes on the hardware this was developed on.

**What that test does not prove.** Tagging BT.601 in the VUI and re-running it
was measured *not* to fail: NVENC appears to follow the configured matrix for its
own conversion, so the tag and the conversion moved together and red stayed red.
The round trip therefore shows that the description in the stream agrees with the
conversion performed — which is the failure that ruins a recording — and not
which matrix was chosen. Proving that needs a decode with the matrix forced,
compared against a reference conversion, and there is no test for it yet
([#147](https://github.com/wildware-uk/clipped/issues/147)).

HDR is not supported. A 10-bit surface is refused by name, with the format that
would work, rather than encoded into something wrong
([#99](https://github.com/wildware-uk/clipped/issues/99)).

### Resource registration, and why `submit` waits

One `submit` does the whole of a frame:

```text
nvEncRegisterResource → nvEncMapInputResource → nvEncEncodePicture
        → nvEncLockBitstream → nvEncUnmapInputResource → nvEncUnregisterResource
```

Locking is what waits: `doNotWait` is off, so `nvEncLockBitstream` returns once
the hardware has finished the picture. That is also the earliest the header
permits the input to be released — "the client must unmap the buffer after
`NvEncLockBitstream()` API returns successfully for encode work submitted using
the mapped input buffer" — and releasing it there is what lets `submit` promise
that nothing derived from the texture outlives the call. The coded bitstream
stays locked afterwards, which is what `next_packet` hands over and unlocks.

Two settings exist to keep that promise true rather than usually true. B-frames
are off, and lookahead is switched off whatever the preset returned, because the
header is explicit that with lookahead "input frames must remain available to the
encoder until encode completion". With both off, NVENC codes every picture on the
submission that carries it. If it ever answers `NV_ENC_ERR_NEED_MORE_INPUT`
anyway, `submit` flushes that picture out, releases the texture and reports the
failure, rather than quietly holding a registration on a surface the caller has
been told it may reuse.

Registering per frame rather than caching a registration per texture is the
simple, obviously correct version: a cache keyed on a texture pointer is only
sound while the capture backend keeps that texture alive, and nothing in the
interface promises it does.

The cost of both decisions is inside the measurements below — the submit-to-packet
figure covers registration, mapping, encoding, locking and release — and if a
later profile shows it matters, a registration cache belongs in a change that
also gives the interface a way to say when a texture pool has been recycled.

## Errors

Every failure names the encoder, the codec and the resolution before it says
anything else, because "encoder failed" arrives in a bug report with no way to
reproduce it:

```text
NVIDIA NVENC could not encode 7680x4320 H.264: the largest picture this encoder
accepts is 4096x4096
NVIDIA NVENC could not encode 2560x1440 HEVC: every encoding session this
hardware allows is already in use, possibly by another application
NVIDIA NVENC could not encode 1920x1080 H.264: its runtime nvEncodeAPI64.dll
could not be loaded (the specified module could not be found); the graphics
driver is missing or damaged
```

The context is carried by `EncodeError` itself rather than repeated in each
variant, so a new failure mode cannot forget it, and a test asserts the sentence
for every variant.

`EncodeErrorKind` is platform-neutral: a caller that wants to react to "there is
no session slot left" should not have to know whose status code said so. Vendor
detail survives in `EncodeErrorKind::Api`, which keeps NVIDIA's status name and
whatever the runtime said about it.

Everything that can be checked is checked when the session opens — that the
runtime loads, that the driver is new enough, that this GPU encodes this codec,
that the resolution is inside what it reports — so a recording that cannot work
fails before the user believes it started.

## Measurements

Measured on the machine described below by the tests in
`crates/encoder/src/windows/nvenc/tests.rs`, which encode a moving pattern and
report submit-to-packet latency. That figure covers the whole path: registering
the texture, mapping it, encoding, locking the bitstream, and releasing the
texture again. The 2560x1440 runs are the same tests with `TEST_SIZE` and
`TEST_FRAMES` raised for the measurement.

| | |
| --- | --- |
| Hardware | GeForce RTX 4090, driver 610.74, Windows 11 build 26200 |
| Workload | A synthetic moving pattern uploaded into a fresh BGRA texture per frame |
| Rate control | 20 Mbit/s constant, one-second buffer, balanced preset (P4, high-quality tuning) |
| Build | `--release` |

**2560x1440, 900 frames (15 seconds of 60 fps video):**

| Codec | Mean | Median | p95 | Worst | Bitstream |
| --- | --- | --- | --- | --- | --- |
| H.264 | 4.23 ms | 3.91 ms | 7.26 ms | 9.24 ms | 36.9 MB |
| HEVC | 4.76 ms | 4.29 ms | 7.92 ms | 9.84 ms | 37.0 MB |
| AV1 | 3.66 ms | 3.32 ms | 6.77 ms | 9.30 ms | 37.3 MB |

**1280x720, 90 frames** (what the test suite runs by default): mean 1.35 ms
(H.264), 1.44 ms (HEVC), 1.28 ms (AV1).

At 2560x1440 and 60 frames a second, a mean of 4.23 ms is about a quarter of the
16.7 ms frame budget — so the encoder alone could sustain roughly 236 frames a
second at that resolution, and encoding a 60 fps recording occupies it about 25%
of the time.

**Encoder utilisation**, sampled with `nvidia-smi --query-gpu=utilization.encoder`
every 250 ms across a 50-second window covering the 2560x1440 runs: 200 samples,
peak 25%. The 75 samples taken while a run was actually encoding average 20%; the
other 125, between runs, sit at 1-6%. The test harness, not the encoder, is the
limit here — it builds and uploads a new 14 MB texture per frame from the CPU,
which a real capture backend does not do — so this is a floor on what the
hardware can take, not a ceiling.

## Verification

What the tests in this crate actually check, and where (AGENTS.md sections 22
and 53):

- **The stream is real.** For each of H.264, HEVC and AV1: 90 frames are
  encoded, the bitstream is parsed in-process (Annex B NAL units, or AV1 OBUs)
  for a sequence header and a keyframe, and `ffprobe` is asked what it sees. It
  reports the codec, 1280x720, `yuv420p` and 90 frames.
- **Colour survives.** Red, green and blue frames are encoded, decoded with
  FFmpeg and compared with what went in. What that does and does not prove is
  under [Colour](#colour) above.
- **A texture can be reused the moment `submit` returns.** One surface is
  overwritten immediately after each `submit`, the way a frame pool recycles
  one, and the decoded pictures still have to be the colours that were
  submitted. It is a guard rather than a reproduction: run against the previous
  shape of this backend, which held the registration past `submit`, it passed
  too — on driver 610.74 the encode finishes before a CPU write can land on the
  surface. The code no longer depends on that timing.
- **Keyframes land where they were asked to.** A one-second interval at 60 fps
  puts keyframes at frames 0 and 60 and nowhere else; a frame that asks to be a
  keyframe becomes one even when the interval says otherwise.
- **Sessions are released.** Sixteen sessions are opened and dropped in turn.
  Removing the `nvEncDestroyEncoder` call makes it fail on the thirteenth, which
  is the concurrent session limit of the card it was run on.
- **A full session table is survivable.** Sessions are opened until the card
  refuses one, which has to arrive as `SessionLimitReached` rather than as a
  status code; they are released and the same thing is done again, which has to
  get at least as far. A failed open that kept driver-side state would show up
  as a second pass that stops earlier.
- **Bad input is refused, not encoded.** An odd picture size, a frame of the
  wrong size, a 10-bit surface, a timestamp that goes backwards, and use after
  shutdown each produce an error naming what was wrong.

`ffprobe` and `ffmpeg` are development tools here and nothing else: nothing in
the recorder shells out to FFmpeg. The tests find them beside the pinned FFmpeg
that `scripts/fetch-ffmpeg.ps1` installs, and then on the path.

The hardware tests skip on a machine with no NVIDIA GPU — which is every hosted
CI runner — and setting `CLIPPED_REQUIRE_ENCODER=1` turns that skip into a
failure, so "the encoder tests passed" cannot quietly mean "the encoder tests did
nothing". The lever covers a missing FFmpeg too, because what `ffprobe` reports
*is* the acceptance criterion; the one skip it leaves alone is a codec the card
cannot encode, which is a fact about the silicon rather than about the checkout.

## Not written yet

- AMF ([#16](https://github.com/wildware-uk/clipped/issues/16)), Quick Sync
  ([#17](https://github.com/wildware-uk/clipped/issues/17)) and the software
  fallback ([#18](https://github.com/wildware-uk/clipped/issues/18)).
- Anything that writes a packet to a container
  ([#21](https://github.com/wildware-uk/clipped/issues/21)) or connects capture
  to encoding ([#19](https://github.com/wildware-uk/clipped/issues/19),
  [#20](https://github.com/wildware-uk/clipped/issues/20)).
- Reconfiguring a running session when the captured target changes size. Today
  a frame of a different size is refused, and the caller has to open a new
  encoder.
- Recovering from a driver reset. A lost device is reported as
  `EncodeErrorKind::DeviceLost` and marked transient, and nothing yet acts on
  it: there is no session loop to recover into
  ([#148](https://github.com/wildware-uk/clipped/issues/148)). A full session
  table, the other half of that scope bullet, is handled and tested.
- B-frames, 10-bit and HDR, and lookahead.
