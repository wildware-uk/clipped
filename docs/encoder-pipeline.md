# Encoder pipeline

**Status: the interface exists, and three backends do.** `crates/encoder`
defines the video encoder trait, the vocabulary a stream is described in, and
three implementations of it:

- **NVENC**, since [issue #15](https://github.com/wildware-uk/clipped/issues/15),
  for H.264, HEVC and AV1 on NVIDIA hardware.
- **AMF**, since [issue #16](https://github.com/wildware-uk/clipped/issues/16),
  for H.264 and HEVC on AMD hardware.
- **The software fallback**, since
  [issue #18](https://github.com/wildware-uk/clipped/issues/18), for H.264 on the
  CPU — what a machine with no usable encoding hardware records with, and never
  what a machine with one does.

What is still missing is everything around them. Quick Sync
([#17](https://github.com/wildware-uk/clipped/issues/17)) is not written, so an
Intel GPU is detected and reported (see
[encoder-capabilities.md](encoder-capabilities.md)) and its encoder is not used:
such a machine falls back to the CPU, which works and costs it frames. And no
session wires capture to encoding
([#19](https://github.com/wildware-uk/clipped/issues/19),
[#20](https://github.com/wildware-uk/clipped/issues/20)), so `recorder record`
still reports that the capture engine is not implemented, and packets go into a
`Vec` in a test rather than through `clipped-muxer` into a file a user can play.

This document describes the interface, the rules a backend has to obey, and the
three backends that obey them. Where it describes something that does not exist
it says so, because a document that quietly describes intentions as facts is
worse than a short one (AGENTS.md section 7).

## What it does

Takes a GPU texture that a capture backend owns, and produces coded packets with
timestamps. On the hardware path the picture never enters system memory:

```text
clipped-capture                 clipped-encoder                 clipped-muxer
───────────────                 ───────────────                 ─────────────
ID3D11Texture2D  ──submit──→  register / map / encode  ──→  EncodedPacket  ──→  file
   (14 MB, on the GPU)          (nothing is copied)          (~80 kB, in RAM)
```

On the software path it has to, because a CPU encoder cannot read video memory,
and that copy is the largest single reason the fallback is a fallback. It is
described and measured under [the software fallback](#the-software-fallback).

## Why this shape

**A frame must not be copied.** At 2560x1440 and 60 frames a second, one
readback per frame is roughly 850 MB/s of memory traffic and a GPU/CPU
synchronisation in the middle of a game's frame. It would not fail; it would
just cost the user frames per second, silently. So the interface carries handles
rather than pixels, and the NVENC backend registers the capture backend's own
texture with the encoder (AGENTS.md section 18).

The software backend is the exception that shows why: it has to make exactly
that copy, because a CPU encoder cannot read video memory, and the measurements
below put a number on what it costs.

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
| `AmfEncoder` | The AMF implementation. Windows only. |
| `SoftwareEncoder` | The CPU implementation, `libopenh264` through libavcodec. Windows only. |

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

The software backend adds one requirement to that, because it is the only one
that calls Direct3D: it copies each frame through the graphics device's
*immediate context*, which has no thread affinity but is not internally
synchronised. So it belongs on the thread that drives that device — the capture
thread — or the caller has to serialise the two. Nothing here can check it.

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

## The AMF backend

`AmfEncoder`, in `crates/encoder/src/windows/amf/`. Same trait, same lifecycle,
same ownership rules as NVENC; H.264 and HEVC, and **not AV1** — see below.

### How it reaches AMF, and why

Through AMD's own interface, the Advanced Media Framework, loaded from the
driver at run time with `LoadLibraryEx` and `GetProcAddress`. `amfrt64.dll`
ships with the display driver, and a machine without one simply fails to load it.
The two alternatives NVENC rejected were rejected again for the same reasons:
Media Foundation's AMD transforms take NV12, so every BGRA frame would need a
colour conversion pass first, and the binding crates on crates.io do not cover
AMF's DirectX path.

### The AMF bindings, and their licence

`crates/encoder/src/windows/amf/sys.rs` is generated by
[bindgen](https://github.com/rust-lang/rust-bindgen) from the public headers of
AMD's [AMF SDK](https://github.com/GPUOpen-LibrariesAndSDKs/AMF), which are
**MIT**. The notices are carried at the top of the generated file and recorded in
[THIRD-PARTY-NOTICES.md](../THIRD-PARTY-NOTICES.md) with the tag and commit they
came from, exactly as the NVENC bindings are.

**Generated as C, not C++.** Every AMF interface is declared twice in those
headers: as an abstract C++ class, and — when `__cplusplus` is not defined — as a
plain structure holding a pointer to a table of function pointers. The second is
an ABI a Rust binding can describe exactly, and it is what makes this backend
possible without a C++ shim or a hand-written vtable whose entries nothing
checks. A call is `(*(*object).pVtbl).Method.unwrap()(object, ...)`.

Interfaces inherit singly — `AMFComponent` is an `AMFPropertyStorageEx` is an
`AMFPropertyStorage` is an `AMFInterface` — so every table begins with the same
entries in the same order, which is what lets one `set_property` helper take a
component, a context and a surface alike. AMD's own C interface relies on the
same thing, in the `AMFPropertyStorage*` parameters of `AddTo` and `CopyTo`.

Regenerating it is a deliberate act:

```bash
curl -sLO https://github.com/GPUOpen-LibrariesAndSDKs/AMF/archive/refs/tags/v1.4.30.tar.gz
tar -xzf v1.4.30.tar.gz AMF-1.4.30/amf/public/include
# commit a118570647cfa579af8875c3955a314c3ddd7058

cat > amf.h <<'HEADER'
#include <core/Factory.h>
#include <core/Surface.h>
#include <core/Buffer.h>
#include <components/VideoEncoderVCE.h>
#include <components/VideoEncoderHEVC.h>
#include <components/ColorSpace.h>
#include <components/VideoConverter.h>
HEADER

bindgen amf.h \
  --no-layout-tests --no-doc-comments --use-core --no-prepend-enum-name \
  --allowlist-type "AMF.*" --allowlist-var "AMF_.*" \
  -o crates/encoder/src/windows/amf/sys.rs \
  -- -x c -I AMF-1.4.30/amf/public/include -target x86_64-pc-windows-msvc
```

Three things bindgen cannot generate, written by hand beside a comment naming
each source:

- **Property and component names.** They are `#define X L"Name"` — wide string
  literals — so `settings.rs` builds them at compile time with a `wide!` macro
  and a unit test checks the conversion.
- **Interface identifiers.** `AMF_DECLARE_IID` expands to a static inline
  function, which has no symbol; `AMFBuffer`'s is transcribed into `api.rs`.
- **The version packing.** `AMF_MAKE_FULL_VERSION` is a macro; `api.rs`
  reproduces it and a unit test checks the arithmetic against a hand-computed
  value.

### Which AMF version, and which driver

This build asks for AMF **1.4.30** and queries the installed runtime first; an
older one gets an error naming the driver release to install (Adrenalin 23.5.2 or
newer, which is the row AMD's own release notes give for 1.4.30) rather than a
status code from a later call. Pinning to an older version than the newest
available is the same trade NVENC makes: the core interfaces and the H.264 and
HEVC property sets have been stable for years, and asking for an old version is
what makes the backend work on a driver installed two years ago. The machine this
was developed on runs AMF 1.4.37.

### What is configured, and what is left to AMD

AMF has no configuration structure: an encoder is configured by setting named
properties one at a time. `settings.rs` therefore produces a *list* of them,
which is also what makes the whole configuration of an encoder one value a test
can assert on. `Usage` is set first, because AMF documents it as configuring a
whole parameter set.

| Setting | What the backend does |
| --- | --- |
| Usage | `TRANSCODING`, AMD's general-purpose tuning. The low-latency usages trade picture quality for a shorter pipeline, which buys nothing for a recorder writing to a local disk. |
| Profile | H.264 High; HEVC Main, main tier. 8-bit 4:2:0 is all this build produces. |
| Preset | `Speed`, `Balanced`, `Quality` map to AMF's own three. The two codecs number them completely differently — balanced is 0 for H.264 and 5 for HEVC — which a unit test pins. |
| Rate control | `RateControl::Bitrate` with no peak becomes CBR with a one-second VBV buffer; with a peak, peak-constrained VBR. `RateControl::Quality` becomes QVBR, whose quality level is the same 1–51 scale `QualityTarget` already uses. HEVC's rate-control enumeration is not H.264's with a prefix: CBR is 1 in one and 3 in the other. |
| Keyframes | H.264 sets `IDRPeriod`; HEVC sets `HevcGOPSize` with one IDR per group of pictures. Setting only a GOP length produces intra frames that are not cut points, which looks fine in a player and breaks every clip the replay buffer tries to save. The *first* picture of a stream is forced to be an IDR whatever the interval says — see [The first picture is forced](#the-first-picture-is-forced). |
| Parameter sets | Repeated at every keyframe — `HeaderInsertionSpacing` for H.264, IDR-aligned header insertion for HEVC — and inserted explicitly on a frame that asks to be a keyframe out of turn. |
| Colour | Written as AMF's combined colour profile (matrix and range together) plus the ITU-T H.273 transfer and primaries code points, and for HEVC the nominal range as well. |
| B-frames | Off, for the same reason as NVENC: reordered output means a decode timestamp that differs from the presentation timestamp. |
| `QueryTimeout` | 5 ms, so `QueryOutput` waits inside the driver instead of returning `AMF_REPEAT` to a poll loop. |

Everything else is AMD's default.

### Why `submit` waits, and how it knows when to stop

AMF's encoder is asynchronous by design: `SubmitInput` queues a frame and
`QueryOutput` collects the result. This backend still does the whole of a frame
inside one `submit`, because the alternative breaks the contract in
[Ownership](#ownership-who-owns-what-and-for-how-long) above:

```text
CreateSurfaceFromDX11Native → SetPts → SubmitInput
        → QueryOutput (waiting) → wait for AMF's reference → Release
```

The last step is the interesting one, and it is where AMF is *better* than NVENC
at answering the question. AMF wraps the caller's texture in a reference-counted
`AMFSurface` and takes its own reference to that wrapper while the frame is in
the encoder. So "has the hardware finished with the caller's texture?" has an
exact answer: acquire and release the wrapper as a probe, and when the count is
back to one, the only reference left is this session's. AMD's API reference
describes the same thing from the other side — input samples are tracked after
submission through the surface's observer, which fires when the last reference
goes.

Measured on Adrenalin 32.0.21043.5001, the count is *already* one every time
`QueryOutput` returns the picture: over 90 frames the probe never once had to
wait. The wait is therefore a guard rather than something that fires here, and if
it ever does not clear, the encoder is flushed — which is what releases a queued
input — and the failure is logged rather than the caller being told it may
recycle a texture AMF is still reading.

### The first picture is forced

`Session::submit` asks for an IDR on the first picture of every stream, as well
as on any frame the caller marked. That is not belt and braces: with HEVC and
`KeyframeInterval::Never` the backend sets `HevcGOPSPerIDR = 0`, and AMD's "0
means no IDR will be inserted" turns out to be literal. Dumping the NAL headers
of such a stream gives `[35, 32, 33, 34, 21, …]` — access unit delimiter, VPS,
SPS, PPS, then type 21, `CRA_NUT`, for frame 0. AMF reports that through
`HevcOutputDataType` as intra rather than as an IDR, so the first packet of the
recording was not flagged as a keyframe, and a replay buffer or a muxer looking
for the first decodable point would not find one at the recording's own
beginning (SPEC.md section 7). H.264 does not behave that way: `IDRPeriod = 0`
still emits a real IDR (NAL type 5) for the first picture.

Forcing it makes `KeyframeInterval::Never`'s documented promise — "only the first
frame is a keyframe" — true for both codecs, and costs nothing on H.264, where
the first picture is an IDR either way. The test that pins it reads the NAL unit
types out of the bitstream rather than AMF's own report of them, so it fails on
exactly this.

### Timestamps lose 100 nanoseconds

The one place a frame's position is not carried through exactly. AMF's `amf_pts`
counts hundred-nanosecond ticks, so a capture timestamp is rounded down to the
tick below and comes back that way: 116.666666 ms in, 116.6666 ms out. The error
is bounded by 100 ns per frame and does not accumulate, because every timestamp
is converted from the original rather than from the previous one — a
ten-thousandth of a frame at 60 frames a second. The tests assert the exact
quantised timeline rather than allowing a tolerance, so a drift that *does* grow
with the recording still fails.

### AV1 is not implemented

`recorder capabilities` reports AV1 as `unknown` for the AMD part this was
developed on — an integrated RDNA 2 GPU, which decodes AV1 and does not encode
it — and Windows lists only `AMDh264Encoder` and `AMDh265Encoder` for it. AMF has
an AV1 encoder component and a path to it could have been written here and never
executed once. It was not, because a backend nobody has seen produce a frame is a
claim rather than support (AGENTS.md section 54).

`AmfEncoder::open` therefore refuses `Codec::Av1` with a message that says it is
Clipped's limitation rather than the hardware's, and links to
[#165](https://github.com/wildware-uk/clipped/issues/165), which is blocked on
AMD hardware that encodes AV1.

### AMF measurements

Measured by the tests in `crates/encoder/src/windows/amf/tests.rs`, which report
submit-to-packet latency — the whole path: wrapping the texture, submitting it,
waiting for the coded picture, and releasing the wrapper.

| | |
| --- | --- |
| Hardware | AMD Radeon(TM) Graphics (integrated, RDNA 2, device 0x13C0), driver 32.0.21043.5001, AMF runtime 1.4.37, Windows 11 build 26200 |
| Rate control | 20 Mbit/s constant, one-second buffer, balanced preset |
| Build | `--release` |

**1280x720, 90 frames** (what the suite runs by default): mean 5.20 ms (H.264),
5.35 ms (HEVC).

**2560x1440, 900 frames**, with the two workloads that bracket a real capture:

| Workload | H.264 mean | HEVC mean | Bitstream |
| --- | --- | --- | --- |
| A moving pattern uploaded into a fresh texture per frame | 26.84 ms | 18.92 ms | 34.8 MB / 39.9 MB |
| One static texture, submitted 900 times | 7.32 ms | 7.36 ms | 3.9 MB / 7.8 MB |

Neither row is the answer, and quoting either alone would be misleading. The
first includes a 14 MB upload from the CPU per frame, which a capture backend
never does and which is far more expensive on an integrated GPU sharing system
memory than on a discrete card; the second encodes an unchanging picture, which
is much less work than a game. A real 2560x1440 60 fps capture — no CPU upload, a
changing picture — sits between them, and the lower row says the silicon has the
headroom for it. There is no measurement of the real thing yet because there is
no session that connects capture to encoding
([#19](https://github.com/wildware-uk/clipped/issues/19)).

### What the AMF tests check

- **The stream is real.** For each of H.264 and HEVC: 90 frames encoded, the
  bitstream parsed in-process for a sequence parameter set and a keyframe, and
  `ffprobe` asked what it sees. It reports the codec, 1280x720, `yuv420p` and 90
  frames.
- **The timestamps are the ones that went in**, frame by frame, quantised to
  AMF's tick. Removing the conversion to hundred-nanosecond units makes it fail.
- **Keyframes land where they were asked to, in the bytes rather than in the
  report.** A one-second interval at 60 fps puts keyframes at frames 0 and 60 and
  nowhere else; a frame that asks to be a keyframe becomes one even when the
  interval says otherwise; and the first picture of a stream is one whatever the
  interval says. Both codecs are run. Every keyframe assertion is made twice —
  against `PictureKind`, which comes from an AMF property, and against the NAL
  unit types in the coded bytes — so an encoder that mis-reports its own picture
  types cannot pass. Asking for one IDR every *two* groups of pictures makes it
  fail, and so does letting HEVC open a `KeyframeInterval::Never` stream with the
  `CRA_NUT` AMF produces by default.
- **Colour survives.** Red, green and blue frames are encoded, decoded with
  FFmpeg and compared with what went in. As with NVENC, this shows that the
  description in the stream agrees with the conversion performed rather than
  which profile was chosen: tagging full range while asking for limited was
  measured *not* to fail it, while telling AMF the BGRA texture is RGBA decodes
  frame 0 as `[0, 0, 251]` instead of `[255, 0, 0]`.
- **A texture can be reused the moment `submit` returns.** One surface is
  overwritten immediately after each `submit`. Run against a deliberately broken
  backend whose `submit` returned before collecting the picture, it passed too —
  on this driver the encoder is finished with a 1280x720 frame before a CPU write
  can land on it — so it is a guard for the contract rather than a reproduction
  of corruption.
- **Teardown gives everything back.** A test takes a reference of its own to the
  context and the component, drops the encoder, and requires the counts to reach
  zero. Removing the component release makes it fail with a count of 1. Sixteen
  sessions are also opened and dropped in turn; that one, unlike NVENC's, does
  *not* detect a leak — AMD has no small cap on concurrent sessions — which is
  why the reference-count test exists.
- **Bad input is refused, not encoded.** An odd picture size, a frame of the
  wrong size, a 10-bit surface, a timestamp that goes backwards, use after
  shutdown, and AV1 each produce an error naming what was wrong.

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

## The software fallback

`SoftwareEncoder`, in `crates/encoder/src/software`. It produces H.264 on the
CPU and exists for one situation: a machine where no hardware encoder is
available. `recommend` ranks it behind every hardware encoder that is (see
`crates/encoder/src/recommendation.rs`), so it is never chosen ahead of one, and
it is always available, which is what makes "Automatic" a setting that always
has an answer.

### Which encoder, and the licence

**`libopenh264`** — Cisco's H.264 encoder, **BSD-2-Clause** — inside the pinned
LGPL FFmpeg build, reached through libavcodec.

**Not x264, and this is the load-bearing part.** x264 is the software H.264
encoder everyone reaches for, and it is GPL: linking it would require every
binary Clipped distributes to be GPL as well. That is a decision about what this
project is, not a choice of encoder, and
[ADR 0004](adr/0004-ffmpeg-dependency-strategy.md) took it in the other
direction — the pinned build is configured `--disable-libx264 --disable-libx265`,
and `crates/muxer/tests/ffmpeg_linkage.rs` asserts that `libopenh264` is present
so that a pin which dropped it fails a test rather than this backend. Issue #18
said "x264 or equivalent"; the answer is "equivalent".

What that costs: `libopenh264` compresses less well than x264 at the same bit
rate, has no B-frames, and offers a smaller set of rate control modes. For a
fallback nobody should be using unless their machine has no encoder in it, none
of that is worth relicensing the project for.

**Patent licensing is a separate question** and is untouched by any of this:
distributing something that encodes H.264 has patent-pool implications whoever
wrote the encoder. ADR 0004 says so, and it is out of scope here.

No new dependency was added: `rusty_ffmpeg` is the binding already pinned in
`[workspace.dependencies]`, and `crates/encoder` now names it too. ADR 0004
records `crates/muxer` as the only crate that does, which is no longer true —
the muxer's wrappers are out of reach from a layer below it, and
[#155](https://github.com/wildware-uk/clipped/issues/155) tracks correcting the
record.

### What it costs, and where

A hardware encoder reads the capture backend's texture where it already lives. A
CPU encoder cannot, so every frame makes a round trip that the hardware path
never makes:

```text
ID3D11Texture2D (BGRA, video memory)
        │  CopyResource + Map          readback.rs   14 MB per frame at 1440p,
  BGRA bytes (system memory)                         and the CPU waits for the GPU
        │  swscale                     convert.rs    reads 14 MB, writes 5.5 MB
  YUV 4:2:0 planes
        │  libopenh264                 avcodec.rs    the CPU cost proper
  coded packets
```

Each stage keeps its own clock, and closing the session logs all three, so a
support log from a machine where recording made the game stutter says which
stage the time went into rather than only that it was slow.

| | |
| --- | --- |
| Hardware | Ryzen 9 9950X3D (16 cores), GeForce RTX 4090, Windows 11 build 26200 |
| Workload | A scrolling chequerboard with a moving bright band, uploaded into a fresh BGRA texture per frame |
| Rate control | 20 Mbit/s constant, balanced preset (CABAC, High profile), four slices |
| Build | `--release`, `crates/encoder/src/software/tests.rs` with `TEST_SIZE` and `TEST_FRAMES` raised for the 1440p runs |

**2560x1440, 300 frames**, four consecutive runs:

| Run | Readback | Colour conversion | `libopenh264` | Submit to packet | Sustained |
| --- | --- | --- | --- | --- | --- |
| 1 | 1.62 ms | 3.89 ms | 12.91 ms | 18.43 ms | 54 fps |
| 2 | 1.58 ms | 3.98 ms | 14.37 ms | 19.94 ms | 50 fps |
| 3 | 1.53 ms | 3.65 ms | 11.86 ms | 17.05 ms | 59 fps |
| 4 | 1.65 ms | 3.81 ms | 12.65 ms | 18.11 ms | 55 fps |

**1280x720, 90 frames**, three consecutive runs: 4.09 ms, 3.96 ms and 3.96 ms
per frame — about 250 frames a second — of which readback 0.6 ms, colour
conversion 0.95 ms and `libopenh264` 2.4 ms.

Read those numbers as a ceiling, not as a promise:

- **1440p60 is marginal on this machine, with nothing else running.** 17 to 20 ms
  a frame against a 16.7 ms budget means a 16-core CPU and a 4090 doing the
  readback cannot quite keep up with 60 frames a second — and the machines this
  backend exists for have neither. The comparison is on the same page: NVENC
  encodes the same picture size in a mean of 4.23 ms while using about a quarter
  of one piece of fixed-function silicon.
- **The wall-clock figures understate the CPU cost.** They are time on the
  submitting thread, and `libopenh264` is encoding on up to four threads
  underneath. Total processor time was not measured separately.
- **The readback wait is not stable.** Over the runs above it sat at 1.5-1.7 ms,
  but earlier runs of the same test on the same machine measured it at
  5.3-5.9 ms a frame, which took the total to 21-22 ms and the rate to 46 fps.
  The copy itself is 3 µs to queue; what varies is how long `Map` waits for the
  GPU to retire it, which depends on what else the GPU is doing and on its power
  state. On a machine that is actually running a game, expect the higher figure.
- **The colour conversion is not free and cannot be avoided here.** It is
  roughly a fifth of the frame, and it is work the hardware path does inside the
  encoder for nothing.

[#156](https://github.com/wildware-uk/clipped/issues/156) is the readback
pipelining that would hide the wait, and why it needs an interface change rather
than a bigger pool.

### What is configured

| Setting | What the backend does |
| --- | --- |
| Threads | At most four, and never more than half the machine's. `libopenh264` parallelises by cutting the picture into slices, which costs compression efficiency and takes cores from the game. A recorder that takes every core in order to record well has made the game unplayable. |
| Rate control | `RateControl::Bitrate` becomes `libopenh264`'s bitrate mode with that average, and its peak becomes `rc_max_rate`. |
| Quality target | `libopenh264` has no constant-quality mode, so a `QualityTarget` becomes a bit rate: 0.09 bits per pixel per frame at the default level, doubling every six levels better and halving every six worse, bounded by the configured ceiling. That is a documented mapping rather than a knob the encoder has. |
| Frame skipping | Off. Dropping frames to hold a bit rate is the wrong trade for a recorder: a missing frame is a visible stutter, an overshoot is a slightly larger file. `libopenh264` warns that it cannot hold the rate exactly without it, and that is the intended answer. |
| Preset | `Speed` is CAVLC and constrained baseline; `Balanced` and `Quality` are CABAC and High. Those are the only quality-for-speed knobs FFmpeg exposes on this encoder. |
| Keyframes | The interval becomes the GOP length. See below — it is a bound, not a spacing. |
| Colour | The primaries, transfer function, matrix and range go into the stream's VUI as ITU-T H.273 code points, and the same values configure the conversion. |
| B-frames | None; `libopenh264` has none. Every packet's decode timestamp equals its presentation timestamp. |

### Keyframes are a bound, not a spacing

`libopenh264` inserts an IDR of its own whenever its scene-change detector
fires, and FFmpeg exposes no option to turn that off. Measured on the moving
test pattern with a one-second interval configured at 60 fps, keyframes arrived
at frames 0, 17, 34, 51, 68 and 85; on a still picture they arrived at 0 and 60,
exactly as configured.

So what this backend guarantees is that no more than the configured interval of
recording passes between cut points, plus whatever extra ones the encoder
decided to add. That is the property the replay buffer needs (SPEC.md section 7).
Both halves are tested: the exact spacing on a still picture, the bound on a
moving one.

A frame that asks to be a keyframe becomes one, whatever the interval says.

### Parameter sets are put back in front of every keyframe

The session is opened with libavcodec's global-header flag, which is what puts
the sequence and picture parameter sets in `extradata` where a container wants
them — and available before a single frame has been encoded, as the trait
requires. `libopenh264` then leaves them out of the coded pictures, so
`next_packet` puts them back in front of any keyframe that does not already
carry them.

That is not a precaution. Removing it was measured: `ffmpeg` cannot decode the
resulting elementary stream at all, and a clip cut from the middle of a
recording would begin at a keyframe no decoder could start on.

It is not free, and it is not the "few hundred bytes" the parameter sets
themselves are. Putting them in front means copying the *whole coded keyframe*
into a buffer of the session's, because the packet's buffer belongs to
libavcodec and is exactly the size of what it coded. Measured by printing the
length of each keyframe buffer from `h264_output_is_a_decodable_stream`, which
encodes the 1280x720 test pattern at 60 fps and 20 Mbit/s CBR: the parameter
sets are 31 bytes, and the six keyframes in its 90 frames copied 50 340,
71 251, 66 935, 71 481, 66 584 and 71 271 bytes. So the cost is tens of
kilobytes per keyframe, once every second or two, and it scales with the
keyframe rather than with the parameter sets: a 1440p keyframe is larger again.

That is still cheap beside the 14 MB readback every frame makes, which is why it
is done this way rather than with a scatter-gather packet. It is not, however,
the rounding error "a few hundred bytes" would suggest, and a reader sizing a
hot path from that number would be out by two to three orders of magnitude.

### Timestamps

The codec's time base is one nanosecond, so the presentation time a frame went
in with is exactly the one that comes out. A capture timestamp is neither a
whole number of frames nor a whole number of microseconds — a sixtieth of a
second is 16 666 666.67 ns — and at any coarser base the encoder would quietly
round the capture clock. What a container stores is the muxer's decision.

### Colour

The BGRA bytes are converted to 4:2:0 by `swscale`, from the same pinned build,
with the matrix and the range the configuration asks for; the same values are
written into the stream. A test encodes red, green and blue, decodes them with
FFmpeg and asserts they come back — the end-to-end check that the description in
the stream agrees with the conversion that produced the samples, which is the
failure that ruins a recording invisibly.

It has the same limit as the hardware path's version of that test: because the
tag and the conversion are derived from the same `ColourSpace`, they move
together, so the test proves they agree rather than proving which matrix was
chosen. What it does catch, measured by breaking it, is a conversion table that
does not match the tag: swapping BT.709's coefficients for BT.601's decodes pure
red as `[255, 23, 0]` and fails.

HDR is not supported. A 10-bit surface is refused by name, with the format that
would work.

### Verification

What the tests in `crates/encoder/src/software` check (AGENTS.md sections 22
and 53):

- **The stream is real.** 90 frames are encoded, the bitstream is parsed
  in-process for a sequence parameter set, a picture parameter set and an IDR,
  and `ffprobe` is asked what it sees: `codec_name=h264`, `profile=High`,
  1280x720, `yuv420p`, `color_space=bt709`, `color_range=tv`,
  `nb_read_frames=90`.
- **It is the picture that went in.** The submitted pattern's bright band moves
  a fixed distance every frame; the stream is decoded back to pixels and the
  band has to be found moving by that distance in every consecutive pair. A
  recorder that produced 90 copies of one frame would pass "it decodes" and
  fails this.
- **Colour survives**, as above.
- **A texture can be reused the moment `submit` returns.** One surface is
  overwritten immediately after each `submit`, the way a frame pool recycles
  one, and the decoded pictures still have to be the colours that were
  submitted.
- **Keyframes**, as above, and a forced one arrives where it was asked for.
- **Bad input is refused, not encoded.** An odd picture size, a codec this
  backend does not produce, a 10-bit surface, a null device, a timestamp that
  goes backwards, a frame whose declared size is wrong, a frame whose *texture*
  is the wrong size, a frame whose texture is the wrong *shape* — a mip chain, a
  texture array or a multisampled surface — and use after shutdown each produce
  an error naming what was wrong. The texture checks matter more than they look:
  `CopyResource` copies whole resources and returns `void`, so without them a
  mismatch is silently nothing at all and the encoder codes whatever the staging
  texture held last. The shape cases are tested after a real frame has been
  encoded, so that the staging texture holds a picture and a silent copy would
  genuinely produce a stale one.
- **Nothing leaks.** Sixteen sessions are opened, used and dropped without
  `shut_down`, which is the path an unwind takes.

Unlike the NVENC tests, these need no encoding hardware: they create a Direct3D
device on the graphics hardware and fall back to WARP, the software rasteriser
that ships with Windows, so the encoder a user with no GPU will be recording
with is covered on every CI run. What they cannot fall back on is FFmpeg — a
missing `ffprobe` or `ffmpeg` *executable* is a skip that says so, and
`CLIPPED_REQUIRE_ENCODER=1` turns it into a failure. A missing FFmpeg *library*
is not a skip and cannot be: it is linked rather than loaded, so the test
process never starts, which is the subject of the next section.

### What linking FFmpeg here costs, and where it is paid

`crates/encoder` names `rusty_ffmpeg`, which links the FFmpeg import libraries.
That is not only a fact about this crate's own tests: **every executable that
depends on `clipped-encoder` now imports `avcodec-62.dll`, `avutil-60.dll` and
`swscale-9.dll`**, which pull in `swresample-6.dll`. Windows resolves imports
before `main` runs, so the process does not start without those four beside it
or on `PATH`. Measured on this branch, with a copy of `clipped-recorder.exe`
alone in an empty directory. Run from `cmd`, `clipped-recorder.exe --help`
prints nothing at all and exits `0xC0000135` — `STATUS_DLL_NOT_FOUND`. A shell
that diagnoses the loader failure for you, such as Git Bash, says which library
is missing:

```text
clipped-recorder.exe: error while loading shared libraries: swscale-9.dll: cannot open shared object file: No such file or directory
```

That is `--help`, not `record`: `capabilities` and `list-windows` fail the same
way, before any argument is parsed. Adding the four libraries makes `--help`
exit 0 again. Before this crate named the binding, only `clipped-muxer`'s
dependents were in that position, and `clipped-recorder` is not one of them —
which is why nobody would find this by running the recorder from a target
directory.

Shipping the FFmpeg libraries alongside Clipped was already required
([ADR 0004](adr/0004-ffmpeg-dependency-strategy.md), and
[#123](https://github.com/wildware-uk/clipped/issues/123) for the LGPL
obligations). What is new is that they are needed for every subcommand rather
than only for recording, so packaging cannot treat them as an encoder-only
payload.

In a build tree the same thing bites contributors. `crates/muxer/build.rs` is
what copies those libraries beside the binaries in the target directory, and it
runs when `clipped-muxer` is built; `clipped-encoder` is not a dependent of the
muxer, so `cargo test -p clipped-encoder` on a checkout where the muxer has
never been built fails at process start in exactly the way above. `cargo test
--workspace` — what CI runs — is unaffected, and building the workspace once
fixes it. [#158](https://github.com/wildware-uk/clipped/issues/158) is the
proper fix: the copy belongs to the workspace rather than to one crate.

## Not written yet

- Quick Sync ([#17](https://github.com/wildware-uk/clipped/issues/17)). A
  machine with an Intel GPU and no NVIDIA or AMD card encodes on the CPU today.
- AV1 on AMF ([#165](https://github.com/wildware-uk/clipped/issues/165)); see
  [The AMF backend](#the-amf-backend) for why.
- Software HEVC or AV1. The pinned build carries `libsvtav1`, and
  [#157](https://github.com/wildware-uk/clipped/issues/157) is what it would
  take — a capability-report change with an encoder attached.
- Anything that connects capture to encoding to a container. `clipped-muxer`
  writes Matroska since
  [#21](https://github.com/wildware-uk/clipped/issues/21), and nothing yet
  drives the three together, so `recorder record` still exits 3.
- Reconfiguring a running session when the captured target changes size. Today
  a frame of a different size is refused, and the caller has to open a new
  encoder.
- Recovering from a driver reset. A lost device is reported as
  `EncodeErrorKind::DeviceLost` and marked transient, and nothing yet acts on
  it: there is no session loop to recover into
  ([#148](https://github.com/wildware-uk/clipped/issues/148)). A full session
  table, the other half of that scope bullet, is handled and tested.
- B-frames, 10-bit and HDR, and lookahead.
