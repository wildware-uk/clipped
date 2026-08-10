# 0004. FFmpeg is a pinned LGPL build, linked dynamically

- Status: Accepted
- Date: 2026-08-10
- Issue: [#7](https://github.com/wildware-uk/clipped/issues/7)

## Context

Clipped writes containers, remuxes without re-encoding, decodes for thumbnails
and reads audio for waveforms. All of that is FFmpeg, used as libraries rather
than as a subprocess: the recording path needs per-packet control of timestamps
and stream identity while a session is being written, which `ffmpeg.exe` cannot
give (SPEC.md section 4).

Three constraints shape the choice, and only one of them is technical.

**Clipped is MPL-2.0 and distributes binaries.** FFmpeg is not one licence. Its
default configuration is LGPL; `--enable-gpl` adds components such as libx264
and libx265 and moves the whole library to GPL; `--enable-nonfree` produces
something that cannot be distributed at all. Which build we link against
therefore decides whether the application can be given to anyone, and that
decision propagates: an encoder chosen in M1 against a GPL build cannot be
un-chosen later without rewriting the encoder. This is why the decision is being
made in M0 rather than when the muxer is written.

**A build has to be reproducible.** AGENTS.md section 51 expects CI to build and
test the workspace, and a media layer that links against whatever FFmpeg the
machine happens to have is a source of failures that reproduce nowhere. Whatever
is used has to be the same bytes on every machine and in every CI run.

**Contributor setup has to stay honest about its cost.** AGENTS.md section 49
asks for a clean `cargo build`, and section 50 for documented platform
requirements. FFmpeg is the largest external thing this project depends on, so
whatever it costs a contributor should be one documented step rather than a
list of undocumented ones.

Not in scope: the Matroska muxer itself
([issue #21](https://github.com/wildware-uk/clipped/issues/21)), which encoders
are used ([issues #14](https://github.com/wildware-uk/clipped/issues/14)
to [#18](https://github.com/wildware-uk/clipped/issues/18)), and what a release
ships ([issue #123](https://github.com/wildware-uk/clipped/issues/123)). This
record fixes the licence position and the linking model those depend on.

## Decision

Clipped links **dynamically** against a **prebuilt, LGPL-only FFmpeg**, pinned
to one immutable artefact and verified by checksum, through the
**`ffmpeg-the-third`** binding. No FFmpeg source is vendored and no GPL
component is ever linked.

Concretely:

- The pin is [BtbN/FFmpeg-Builds](https://github.com/BtbN/FFmpeg-Builds) release
  `autobuild-2026-08-09-13-03`, asset
  `ffmpeg-n8.1.2-34-g9b6c8969e0-win64-lgpl-shared-8.1.zip`, SHA-256
  `2936e544…0905`. `scripts/fetch-ffmpeg.ps1` downloads it, verifies the
  checksum, extracts it into the gitignored `third-party/ffmpeg/`, and does
  nothing at all on a second run. The tag is a dated one: `latest` moves daily,
  and a fetch step that silently picks up a different build every run is not a
  pin.
- **The build is LGPL v3**, not v2.1, because it is configured with
  `--enable-version3`. It reports `LGPL version 3 or later` for itself, and it
  is built with `--disable-libx264 --disable-libx265` and no `--enable-gpl`.
- The binding is `ffmpeg-the-third 5.0.0+ffmpeg-8.1`, pinned in
  `[workspace.dependencies]` so the crate version and the FFmpeg version move
  together. Default features are off; each crate opts into the components it
  uses.
- `crates/muxer` owns the link. Its `linkage` module reports the loaded
  libraries and probes the build, and `crates/muxer/tests/ffmpeg_linkage.rs`
  asserts that what is loaded is the pinned artefact, that it is LGPL, and that
  it still contains the components later milestones depend on. Those assertions
  are how the licence position stays true rather than remaining a claim in this
  file.
- Contributors install LLVM (`winget install LLVM.LLVM`). The binding runs
  `bindgen` and libclang-based feature detection over the FFmpeg headers at
  build time, so libclang is a hard prerequisite rather than a convenience.
- FFmpeg 8.1 rather than 7.1 for a specific reason: the builder's n7.1 LGPL
  Windows artefact does not contain `av1_nvenc`, and its n8.1 artefact does.
  AV1 on NVIDIA hardware is in SPEC.md section 9, so 7.1 would have quietly cost
  a headline capability.

## Alternatives

### Shell out to `ffmpeg.exe`

Run the command-line tool as a child process. It is by far the cheapest thing to
build: no linking, no bindgen, no libclang, no ABI to track, and a licence
position that is easier still, since a separate process invoked over a
command-line interface is not a derived work in the way a linked library is.

It was rejected for the recording path, which is where the requirement lives.
Recording is a live pipeline: encoded packets arrive from the encoder with
timestamps the capture clock decided
([issue #22](https://github.com/wildware-uk/clipped/issues/22)), several audio
tracks have to be interleaved into one container as they are produced
([issue #28](https://github.com/wildware-uk/clipped/issues/28)), and the file
has to stay recoverable if the process is killed (AGENTS.md section 17). Feeding
that through a pipe means re-encoding or re-deriving timestamps at the boundary,
and it means a crash of a process we do not control losing the session. The
argument is not that shelling out is impossible, it is that the container writer
has to be inside the process that owns the packets.

`ffprobe` remains a *test* tool for exactly this reason: assertions about a
finished file (AGENTS.md section 22) have none of these constraints.

### Build FFmpeg from source as part of the build

`ffmpeg-the-third` can compile FFmpeg itself. That would make the exact
configuration ours — we could disable everything unused and produce a smaller,
auditable library with no third-party binary to trust.

It was rejected on cost. Building FFmpeg on Windows needs a POSIX-ish
environment (MSYS2 or a cross-compiler), NASM and a long list of external
libraries, which turns a five-minute contributor setup into an afternoon and
adds many minutes to every cold CI run. The auditability it buys is largely
available anyway: `configure` records its arguments into the binary, and
`crates/muxer/tests/ffmpeg_linkage.rs` reads them back and fails on a GPL build.
What would make it win later: needing a component no public LGPL build ships, or
needing to patch FFmpeg.

### Static linking of an LGPL build

Link the LGPL libraries statically, producing a single executable with no DLLs
beside it.

The LGPL permits this, but only on terms that are expensive here. Section 4 of
the LGPL requires that the recipient be able to relink the application against a
modified version of the library; with static linking that means distributing our
object files or equivalent alongside every release, and keeping that mechanism
working. Dynamic linking satisfies the same requirement by construction — the
DLL beside the executable is already replaceable — for the price of shipping
seven files instead of one. That is the cheapest possible way to comply, and
compliance here is not optional.

### A GPL FFmpeg build, with libx264 and libx265

Use the GPL build. It is the one most projects use, it is what nearly every
tutorial assumes, and it would hand us libx264 — the software H.264 encoder that
everyone reaches for and the one
[issue #18](https://github.com/wildware-uk/clipped/issues/18) currently names.

It was rejected outright, and this is the load-bearing rejection in this record.
Linking GPL code into Clipped would require Clipped itself to be distributed
under the GPL. MPL-2.0 permits that combination in one direction — MPL-2.0
allows a covered work to be distributed as part of a Larger Work under the GPL —
but taking it would relicense every binary we publish, bind every future
contribution to the GPL, and remove the ability of anyone else to use these
crates in a differently licensed program. That is a project-level decision about
what Clipped *is*, not a build configuration, and nothing about a software
encoder fallback justifies making it. If the project ever wants x264, the
sequence is: change the project licence deliberately, then change this record —
not the other way round.

### `rsmpeg`, on top of `rusty_ffmpeg`

`rsmpeg` is the strongest alternative binding and on paper it is the better fit:
MIT licensed, deliberately thin, safe RAII wrappers over `AVFormatContext`,
`AVStream` and `AVPacket` with the raw FFI re-exported as `rsmpeg::ffi` for
anything it does not cover. That is exactly the shape a muxer wants.

It lost on maintenance, and the evidence is specific rather than a feeling. Its
newest release is `0.18.0+ffmpeg.8.0` from August 2025 and its default branch
has had no commit since; two separate pull requests adding FFmpeg 8.1 support
have sat open since April and May 2026. Because it constrains `rusty_ffmpeg` to
`0.16`, using it means capping FFmpeg at 8.0 — and the builder we pin publishes
release-branch Windows artefacts for 7.1 and 8.1, not 8.0. So `rsmpeg` in
practice means FFmpeg 7.1, and the n7.1 LGPL artefact has no `av1_nvenc` in it.
A dormant binding that also costs a SPEC-level capability is not a close call.

What would make it win later: a release supporting the FFmpeg version we pin. It
remains the better-designed binding, and the migration is not large.

### `rusty_ffmpeg`'s raw FFI, with wrappers written here

`rusty_ffmpeg` — the crate underneath `rsmpeg` — is MIT, is actively maintained
(`0.17.0+ffmpeg.8.1`, April 2026), and matches the pinned FFmpeg exactly. Taking
it directly and writing our own safe wrappers would give a clean licence, an
active dependency, and no abstraction between us and the C API.

It was rejected because it trades a licence wart for a correctness risk. The
wrappers are not the ten calls a muxer needs; they are the lifetimes and
ownership rules around `AVFormatContext`, `AVPacket`, `AVDictionary` and codec
parameters, which is where FFmpeg bindings are actually hard and where mistakes
show up as memory corruption rather than as compile errors. Writing that from
scratch to serve one crate is the definition of work that should be borrowed.
This is the fallback if `ffmpeg-the-third` is abandoned, and the fallback is
cheap because `ffmpeg-the-third` re-exports its own sys crate as `ffi`.

### Checking in a pre-generated binding instead of requiring LLVM

`bindgen` is the reason a contributor has to install a gigabyte of LLVM, and
some projects avoid it by committing the generated bindings.

It is not available here. `ffmpeg-sys-the-third` does not read a pre-generated
binding; beyond `bindgen` it also uses libclang directly to detect which
deprecated APIs the installed FFmpeg still has, so libclang would remain
required even if the binding file were committed. The option only exists under
`rusty_ffmpeg`, via its `FFMPEG_BINDING_PATH`, and it comes with its own costs
there: a multi-megabyte generated file derived from LGPL headers living in an
MPL-2.0 repository, regenerated by hand on every pin move, and able to disagree
silently with the DLLs actually loaded — which is undefined behaviour rather
than a build failure. Requiring LLVM is the honest cost.

### Pinning the rolling `latest` tag

The builder publishes a `latest` tag with stable asset names, which would keep
us on current FFmpeg with no maintenance.

Rejected because it is the opposite of a pin: the bytes behind those names
change daily, no checksum can be recorded against them, and a CI failure could
not be told apart from an upstream rebuild. Moving the pin should be a commit
that a person made.

## Consequences

- **The software encoder fallback cannot be x264 or x265.**
  [Issue #18](https://github.com/wildware-uk/clipped/issues/18) says "x264 or
  equivalent"; the answer is now "equivalent", and the equivalents are in the
  pinned build already: `libopenh264` (BSD-2) for H.264, `libsvtav1` (BSD-3 plus
  the Alliance for Open Media patent licence) and `libaom` (BSD-2) for AV1, and
  `libkvazaar` (BSD-3) or `libvvenc` where HEVC-class software encoding is
  wanted. `crates/muxer/tests/ffmpeg_linkage.rs` asserts that `libopenh264` and
  `libsvtav1` are present, so a pin that dropped them fails the build rather
  than surfacing in M1. This consequence is the reason this ADR had to be
  written before #18, not after.
- **Distributing Clipped now carries LGPL v3 obligations**, and they are
  concrete: ship the FFmpeg DLLs unmodified and separate from our own binaries,
  include the FFmpeg licence text (`LICENSE.txt` in the fetched build), say in
  the application which FFmpeg version is used, and offer the corresponding
  source for that exact build — the release tag and the FFmpeg commit named in
  the artefact are enough to identify it, and mirroring the source archive
  alongside a release is the simplest way to discharge the offer. Never link
  statically, and never modify the DLLs without publishing the changes. This is
  tracked as [issue #123](https://github.com/wildware-uk/clipped/issues/123)
  rather than left to be rediscovered during packaging.
- **Our own code is unaffected.** No FFmpeg source is copied into this
  repository, so MPL-2.0's file-level copyleft applies to Clipped's files and
  nothing else, and these crates stay usable by others under MPL-2.0.
- **Two setup steps a contributor cannot skip**: an LLVM install and one run of
  `scripts/fetch-ffmpeg.ps1`. The second is cheap and idempotent; the first is a
  large download and is now a documented prerequisite
  (`docs/prerequisites.md`).
- **A missing `FFMPEG_DIR` produces a poor error.** The failure comes from
  `ffmpeg-sys-the-third`'s build script, which falls back to `pkg-config` and
  reports that instead of naming the fetch script, and no build script of ours
  runs early enough to say something better. `docs/ffmpeg.md` and
  `docs/prerequisites.md` cover it; teaching
  `scripts/check-prerequisites.ps1` to catch it first is
  [issue #122](https://github.com/wildware-uk/clipped/issues/122).
- **CI has to fetch FFmpeg and install LLVM before it builds anything.** The
  fetch script is non-interactive, caches naturally (each pin lands in its own
  directory) and exports `FFMPEG_DIR` through `GITHUB_ENV`. Wiring it in belongs
  to [issue #4](https://github.com/wildware-uk/clipped/issues/4), which owns the
  workflow.
- **Moving the pin touches three places** and all three fail loudly if missed:
  the parameters in `scripts/fetch-ffmpeg.ps1`, the binding version in
  `[workspace.dependencies]` if the FFmpeg major changes, and the expected
  versions in `crates/muxer/tests/ffmpeg_linkage.rs`. `docs/ffmpeg.md` describes
  the procedure.
- **The binding has a bus factor of about one**, and its lineage is two
  previously abandoned crates (`ffmpeg` then `ffmpeg-next`). It is currently the
  most active FFmpeg binding in the ecosystem — it shipped FFmpeg 9.0 support
  the day after FFmpeg 9.0 — but that is the thing to watch, and the exit is
  documented above.
- **Its licence, WTFPL, is a wart we accepted.** It is a free software licence
  by the FSF's reckoning and imposes no obligation on us at all — not even
  attribution — but it is not OSI-approved, it carries no warranty disclaimer,
  and some organisations refuse it outright. It permits relicensing explicitly,
  so vendoring the crate under a conventional licence is available if a
  distributor ever objects. `rsmpeg` and `rusty_ffmpeg` are both MIT, which is
  the one axis on which the rejected alternative is cleaner.
- **We depend on one person's build infrastructure.** BtbN's builds are the only
  ones publishing LGPL-only shared Windows artefacts on this cadence, and they
  ship no checksums, so the SHA-256 recorded in the fetch script is one we
  computed from the artefact we reviewed. If those builds stop, the fallback is
  to mirror the pinned zip and, eventually, to build FFmpeg ourselves — which is
  the alternative rejected above, and it would become the right answer.
- **Codec patent licensing is untouched by any of this.** Copyright licences are
  not patent licences: distributing an application that encodes H.264 or HEVC
  has patent-pool implications regardless of whether the encoder is LGPL, BSD or
  GPL. That risk exists for any recorder and is out of scope here, but it should
  be looked at properly before Clipped is distributed as a signed release rather
  than discovered then.
- **`av1_nvenc` being present is not the same as AV1 encoding working.** The
  pinned build exposes it; whether a given machine can use it is a runtime
  question for [issue #14](https://github.com/wildware-uk/clipped/issues/14).
- **This is a Windows decision.** The pin, the fetch script and the DLL handling
  are Windows-specific, which matches SPEC.md section 3. A future port would
  need its own artefacts, though nothing above would have to be re-argued except
  where the libraries come from.
