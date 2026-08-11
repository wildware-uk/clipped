# 0004. FFmpeg is a pinned LGPL build, linked dynamically through a sys binding

- Status: Accepted
- Date: 2026-08-10
- Issue: [#7](https://github.com/wildware-uk/clipped/issues/7)

## Amendments

**2026-08-11 — who may name the binding
([#155](https://github.com/wildware-uk/clipped/issues/155)).** The Decision
below originally ended with "No other crate depends on `rusty_ffmpeg`
directly." The software encoder fallback
([#18](https://github.com/wildware-uk/clipped/issues/18)) made that false:
`crates/encoder` links the binding to reach `libopenh264` inside the pinned
build.

This is a correction of wording, not a change of decision, so it is made in
place rather than by a superseding record (see
[the ADR README](README.md#status-and-supersession)). The decision — a pinned
LGPL build, linked dynamically, through a raw sys binding — is unchanged, and
this ADR's own Consequences already named `libopenh264` as the answer for #18,
so a second consumer was foreseen when it was written. What was wrong was
stating a *rule about who may name the FFI* as though it were a property of the
decision. The rule that replaces it is in the Decision bullet below: a crate
with no lower-layer route to what it needs may name the binding, and the safe
wrappers over the container API still live in exactly one place.

## Context

Clipped writes containers, remuxes without re-encoding, decodes for thumbnails
and reads audio for waveforms. All of that is FFmpeg, used as libraries rather
than as a subprocess: the recording path needs per-packet control of timestamps
and stream identity while a session is being written, which `ffmpeg.exe` cannot
give (SPEC.md section 4).

Four constraints shape the choice, and only one of them is technical.

**Clipped is MPL-2.0 and distributes binaries.** FFmpeg is not one licence. Its
default configuration is LGPL; `--enable-gpl` adds components such as libx264
and libx265 and moves the whole library to GPL; `--enable-nonfree` produces
something that cannot be distributed at all. Which build we link against
therefore decides whether the application can be given to anyone, and that
decision propagates: an encoder chosen in M1 against a GPL build cannot be
un-chosen later without rewriting the encoder. This is why the decision is being
made in M0 rather than when the muxer is written.

**Every dependency's own licence has to clear the allow-list.** `deny.toml`
names the complete set of licences a crate in this graph may carry — MIT,
Apache-2.0, BSD-2/3-Clause, ISC, MPL-2.0, Unicode, Zlib — and `cargo deny check`
runs on every pull request. A binding whose licence is not on that list fails
CI, and adding a licence to the list is a decision about the project rather than
a convenience for one crate (CONTRIBUTING.md, AGENTS.md section 11).

**A build has to be reproducible.** AGENTS.md section 51 expects CI to build and
test the workspace, and a media layer that links against whatever FFmpeg the
machine happens to have is a source of failures that reproduce nowhere. Whatever
is used has to be the same bytes on every machine and in every CI run.

**Contributor setup has to stay honest about its cost.** AGENTS.md section 49
asks for a clean `cargo build`, and section 50 for documented platform
requirements. FFmpeg is the largest external thing this project depends on, so
whatever it costs a contributor should be documented steps that a script checks,
rather than a list nobody wrote down.

Not in scope: the Matroska muxer itself
([issue #21](https://github.com/wildware-uk/clipped/issues/21)), which encoders
are used ([issues #14](https://github.com/wildware-uk/clipped/issues/14)
to [#18](https://github.com/wildware-uk/clipped/issues/18)), and what a release
ships ([issue #123](https://github.com/wildware-uk/clipped/issues/123)). This
record fixes the licence position and the linking model those depend on.

## Decision

Clipped links **dynamically** against a **prebuilt, LGPL-only FFmpeg**, pinned
to one immutable artefact and verified by checksum, through the **`rusty_ffmpeg`**
binding — a raw `-sys` crate, over which `crates/muxer` writes its own safe
wrappers. No FFmpeg source is vendored and no GPL component is ever linked.

Concretely:

- The pin is [BtbN/FFmpeg-Builds](https://github.com/BtbN/FFmpeg-Builds) release
  `autobuild-2026-08-09-13-03`, asset
  `ffmpeg-n8.1.2-34-g9b6c8969e0-win64-lgpl-shared-8.1.zip`, SHA-256
  `2936e544…0905`. `scripts/fetch-ffmpeg.ps1` downloads it, verifies the
  checksum, extracts it into the gitignored `third-party/ffmpeg/current`, and
  does nothing at all on a second run. The tag is a dated one: `latest` moves
  daily, and a fetch step that silently picks up a different build every run is
  not a pin.
- **The build is LGPL v3**, not v2.1, because it is configured with
  `--enable-version3`. It reports `LGPL version 3 or later` for itself, it is
  built with `--disable-libx264 --disable-libx265`, and it carries neither
  `--enable-gpl` nor `--enable-nonfree`. Its `LICENSE.txt` is the LGPL v3 text.
- The binding is `rusty_ffmpeg 0.17.0+ffmpeg.8.1`, **MIT**, pinned in
  `[workspace.dependencies]` so the crate version and the FFmpeg version move
  together. It links FFmpeg and generates the FFI with `bindgen`, and offers
  nothing above that.
- **`crates/muxer` owns the safe API over the container.** Because the binding
  is a `-sys` crate, every safe abstraction over FFmpeg's *container* API in
  Clipped is written here. Today that is `linkage`, which reports the loaded
  libraries and probes the build, and the wrappers around `AVFormatContext`,
  `AVStream` and `AVPacket`, written with the muxer that needs them (#21).

  A crate that needs something inside the pinned build and has **no
  lower-layer route to it** may name `rusty_ffmpeg` directly. `crates/encoder`
  does, to reach `libopenh264` for the software fallback (#18): it sits at
  layer 1 and `crates/muxer` at layer 2, so the muxer's wrappers are above it
  and out of reach, and the dependency direction may not be inverted to share
  them. What such a crate must not do is write a second safe API over the
  *container*; that stays in one place. See the amendment above.
- `crates/muxer/tests/ffmpeg_linkage.rs` asserts that what is loaded is the
  pinned artefact, that it is LGPL, and that it still contains the components
  later milestones depend on. Those assertions are how the licence position
  stays true rather than remaining a claim in this file.
- Four environment variables, all derived from that one path in the committed
  `.cargo/config.toml`: `FFMPEG_INCLUDE_DIR` (headers for `bindgen`),
  `FFMPEG_LIBS_DIR` (import libraries), `FFMPEG_LINK_MODE=dynamic` — **not** the
  binding's default, and the basis of the whole LGPL position — and `FFMPEG_DIR`,
  which is Clipped's own and names the prefix so `crates/muxer/build.rs` can copy
  the runtime DLLs beside the binaries it builds. They are configuration, not
  shell state, because a fetch script cannot export a variable into the shell
  that ran it: the first version of this decision asked contributors to persist
  them and open a new terminal, and the documented build sequence did not work as
  written. Cargo's `[env]` table has neither problem, resolves the paths relative
  to the checkout, and still yields to a variable of the same name set by hand.
- Contributors install LLVM (`winget install LLVM.LLVM`). The binding runs
  `bindgen` over the FFmpeg headers at build time, so `libclang.dll` is a hard
  prerequisite. `scripts/check-prerequisites.ps1` checks for it, and for the
  fetched FFmpeg, so both are reported by the setup check rather than as a build
  failure.
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

### `ffmpeg-the-third`, the highest-level binding

`ffmpeg-the-third` is the most active FFmpeg binding in the Rust ecosystem and
the one with the most API above the FFI: safe RAII wrappers over formats,
streams, packets and codec contexts, an iterator over registered muxers, typed
errors. It publishes promptly — `6.0.0+ffmpeg-9.0` on 2026-08-09, four months
after `5.0.0+ffmpeg-8.1` — and it would have removed the need to write any of
the wrapper code this decision now commits us to. An earlier attempt at this
issue chose it.

It was rejected on its licence, and the rejection is not a preference. Both it
and its `ffmpeg-sys-the-third` sys crate are **WTFPL**, which is not on
`deny.toml`'s allow-list, so `cargo deny check` fails on it and the pull request
cannot go green. Merging it would therefore have meant adding WTFPL to the
allow-list — a project-level licensing decision taken silently, in a pull
request about build configuration. WTFPL is a free software licence by the FSF's
reckoning and imposes nothing on us, not even attribution, but it is not
OSI-approved and it carries no warranty disclaimer, which some downstream
packagers refuse outright. That is not a trade worth making for an abstraction
we have reason to own anyway.

What would make it win later: a relicence. It permits relicensing explicitly, so
a maintainer could publish under MIT tomorrow, and the migration from raw FFI to
its API would be a contained piece of work in one crate.

### `rsmpeg`, the middle layer

`rsmpeg` is on paper the best fit of the three: MIT licensed, deliberately thin,
safe RAII wrappers over exactly the objects a muxer needs, with the raw FFI
re-exported for anything it does not cover. It is built on `rusty_ffmpeg`, so
choosing it would have meant the same sys crate and the same fetch script, plus
wrappers we would not have had to write.

It lost on maintenance, and the evidence is from published releases rather than
from impressions. Its newest release is `0.18.0+ffmpeg.8.0`, published
2025-08-24 — a year ago — and it constrains `rusty_ffmpeg` to `0.16`, which caps
FFmpeg at 8.0. The builder we pin publishes release-branch Windows artefacts for
7.1 and 8.1, not 8.0, so `rsmpeg` in practice means FFmpeg 7.1, and the n7.1
LGPL artefact has no `av1_nvenc` in it. A binding a year without a release, that
also costs a SPEC-level capability, is not a close call.

What would make it win later: a release supporting the FFmpeg version we pin. It
remains the better-designed binding of the two MIT options, and because it sits
on the same sys crate the migration would be additive rather than a rewrite.

### Build FFmpeg from source as part of the build

Compile FFmpeg ourselves. That would make the exact configuration ours — we
could disable everything unused and produce a smaller, auditable library with no
third-party binary to trust.

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

Note that this is not the binding's default: `rusty_ffmpeg` links statically
unless `FFMPEG_LINK_MODE=dynamic` is set. The variable is therefore part of the
licence position and not a build detail, which is why it is committed in
`.cargo/config.toml` rather than left to a contributor to remember, and why
`scripts/check-prerequisites.ps1` fails on any other value it finds in the
environment.

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

### Checking in a pre-generated binding instead of requiring LLVM

`bindgen` is the reason a contributor has to install a gigabyte of LLVM, and
`rusty_ffmpeg` supports avoiding it: set `FFMPEG_BINDING_PATH` to a binding file
generated once and committed, and the build script copies it instead of running
`bindgen`. Unlike under `ffmpeg-the-third`, whose sys crate uses libclang
directly for deprecated-API detection as well, this option is genuinely
available to us.

It was rejected on the failure mode. The generated binding is a multi-megabyte
Rust file derived from the FFmpeg headers, so a copy of it would live in this
MPL-2.0 repository and would have to be regenerated by hand — with LLVM, by
whoever moves the pin — every time the pin moves. If that step is ever missed,
the binding describes one FFmpeg and the DLLs are another: struct layouts and
function signatures disagree, and the result is undefined behaviour at run time
rather than an error at build time. Generating from the headers actually present
makes that class of mistake impossible. LLVM is the honest cost, it is one
`winget` command, the prerequisite check now names it, and GitHub's Windows
runners ship it already.

What would make it win later: a contributor population for whom the LLVM
download is a genuine barrier, plus a way to make the regeneration automatic and
verified rather than remembered.

### Pinning the rolling `latest` tag

The builder publishes a `latest` tag with stable asset names, which would keep
us on current FFmpeg with no maintenance.

Rejected because it is the opposite of a pin: the bytes behind those names
change daily, no checksum can be recorded against them, and a CI failure could
not be told apart from an upstream rebuild. Moving the pin should be a commit
that a person made.

## Consequences

- **We write and own the safe FFmpeg wrappers.** This is the price of the MIT
  binding, and it is a real one: the hard part of an FFmpeg binding is not the
  ten calls a muxer makes but the lifetime and ownership rules around
  `AVFormatContext`, `AVPacket`, `AVDictionary` and codec parameters (AGENTS.md
  section 58), where mistakes appear as memory corruption rather than as
  compile errors. Two things make it acceptable. The muxer needs precise control
  of packet timestamps and multi-track audio anyway, so much of this is code we
  would have written over any binding; and owning the abstraction keeps the
  FFmpeg surface Clipped depends on small and visible. The wrappers are built
  incrementally with the code that needs them — `linkage` today, the container
  writer under #21 — and every `unsafe` block carries a `// SAFETY:` comment
  that argues the case.
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
  concrete. Section 4 of the LGPL is the list, and a release has to satisfy all
  of it:
  - **4(a)** — a prominent notice with each copy that FFmpeg is used, and that
    FFmpeg and its use are covered by the LGPL.
  - **4(b)** — a copy of the GNU **GPL** as well as the LGPL. This is the item
    easiest to miss, because the artefact does not contain it: LGPL v3 is
    written as a set of additional permissions on top of GPL v3, so both texts
    have to ship, and `LICENSE.txt` in the fetched build is the LGPL v3 text
    alone. The GPL v3 text has to be added deliberately.
  - **4(c)** — where copyright notices are displayed at run time, FFmpeg's must
    be among them, with a pointer to those two licence texts. Clipped has no
    about screen yet, so this becomes real when one is written.
  - **4(d)(1)** — dynamic linking, which is why the DLLs ship unmodified and
    separate from our own binaries, and why static linking is refused.
  - **4(e)** does not bite: it only applies where section 6 of the GPL would
    require Installation Information, and we take 4(d)(1) rather than 4(d)(0)
    and ship no locked-down device.

  On top of section 4, the DLLs are the Library itself being conveyed, so a
  release must say which FFmpeg version it carries and offer the corresponding
  source for that exact build — the release tag and the FFmpeg commit named in
  the artefact identify it, and mirroring the source archive alongside a release
  is the simplest way to discharge the offer. Never modify the DLLs without
  publishing the changes. All of this is tracked as
  [issue #123](https://github.com/wildware-uk/clipped/issues/123) rather than
  left to be rediscovered during packaging.
- **Our own code is unaffected.** No FFmpeg source is copied into this
  repository, so MPL-2.0's file-level copyleft applies to Clipped's files and
  nothing else, and these crates stay usable by others under MPL-2.0.
- **All seven FFmpeg libraries are linked, not the two we use.** The binding
  emits link directives for `avcodec`, `avdevice`, `avfilter`, `avformat`,
  `avutil`, `swresample` and `swscale` unconditionally; there is no feature to
  narrow it. So all seven DLLs are copied beside every binary and all seven ship
  in a release, and the LGPL obligations above cover all of them. Most of that
  set is wanted eventually — `swscale` and `swresample` for thumbnails and
  waveforms — but `avdevice` and `avfilter` are carried for nothing today: 32 MB
  of the 136 MB the seven DLLs weigh. That 136 MB is copied into three
  directories per profile, so a debug build of the workspace puts 409 MB under
  `target/debug` on top of the 168 MB extracted under `third-party/ffmpeg`. The
  contributor-facing pages say so rather than leaving it to be discovered.
- **Two setup steps a contributor cannot skip**: an LLVM install and one run of
  `scripts/fetch-ffmpeg.ps1`. Both are checked by
  `scripts/check-prerequisites.ps1`, so the failure a contributor meets is a
  named prerequisite rather than "No linking method set!" from a build script.
- **CI fetches FFmpeg on every run and caches it.** The fetch script is
  non-interactive and no-ops on a warm cache without touching the network. It
  exports nothing into the job, because the runner is configured by the same
  checked-out `.cargo/config.toml` as a contributor's machine — which is what
  makes a green CI run evidence that the documented steps work. The cache key is
  the pinned asset name, read out of the fetch script. The runner already ships
  LLVM.
- **Moving the pin touches three places** and all three fail loudly if missed:
  the parameters in `scripts/fetch-ffmpeg.ps1`, the binding version in
  `[workspace.dependencies]` if the FFmpeg major changes, and the expected
  versions in `crates/muxer/tests/ffmpeg_linkage.rs`. `docs/ffmpeg.md` describes
  the procedure.
- **The binding has a bus factor of about one, and it is the slowest of the
  three to follow FFmpeg.** `rusty_ffmpeg` published `0.17.0+ffmpeg.8.1` on
  2026-04-10 and has published nothing since; FFmpeg 9.0 support has not
  appeared in a release, while `ffmpeg-the-third` published one on 2026-08-09.
  That costs nothing while we pin 8.1 and would cost time if we wanted 9.0
  quickly. It is the thing to watch, and the exits are documented above.
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
