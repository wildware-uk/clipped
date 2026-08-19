# Licensing: what a release of Clipped has to carry

Clipped is [MPL-2.0](../LICENSE). It ships FFmpeg's LGPL v3 libraries beside its
own binaries, and it links several hundred permissively licensed Rust crates
into them. Those licences place conditions on **distribution**, not on the
build, so nothing in this page is enforced by `cargo build` and none of it is
discharged by a file in this repository. It is discharged by what a release
puts on a user's machine.

Three documents divide this up, and it is worth knowing which is which:

|                                                                  |                                                                                                                                   |
| ---------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------- |
| [CONTRIBUTING.md](../CONTRIBUTING.md#licensing-and-dependencies) | Which licences a _dependency_ may carry, and what to do when you add one.                                                         |
| [ADR 0004](adr/0004-ffmpeg-dependency-strategy.md)               | Why FFmpeg is a pinned, LGPL-only build linked dynamically. That decision is what makes the obligations below satisfiable at all. |
| [docs/releasing.md](releasing.md)                                | When a release may happen at all, and the gate that refuses to build one while the obligations below are unmet.                   |
| **This page**                                                    | What a _release_ has to do, which of it exists today, and how each part was checked.                                              |

> **The binaries and the paperwork are both packaged.**
> `npm run build:app` produces an installer carrying the recorder and the seven
> FFmpeg DLLs beside the window
> ([#226](https://github.com/wildware-uk/clipped/issues/226),
> [docs/packaging.md](packaging.md)) — the half of the obligations below whose
> subject is _the libraries_: they are conveyed unmodified, as ordinary files,
> replaceable in place. It also carries the licence texts and third-party
> notices, in `licences/` beside the binaries
> ([#123](https://github.com/wildware-uk/clipped/issues/123)):
> `scripts/collect-notices.ps1` produces them and
> `scripts/stage-installer-payload.ps1` puts them in the bundle, **refusing to
> build an installer at all** when they are absent. The release workflow runs
> the collector before either job stages anything; a local `npm run build:app`
> is refused until you have run it yourself, and the refusal names it — in the same shape as the refusals for a missing recorder or
> a missing FFmpeg, and for a stronger reason. A build without them is not one
> anybody is licensed to distribute, so it is a refusal rather than a warning.
>
> **The source of the FFmpeg it ships goes up with it.** The other half of the
> LGPL is not a file in the installer:
> [`scripts/fetch-ffmpeg-source.ps1`](../scripts/fetch-ffmpeg-source.ps1)
> assembles the source of the exact build being conveyed, and the release
> workflow attaches it to the release in the same call that attaches the
> installer — same page, same moment, no request to make. See "Conveying the
> libraries themselves" below.
>
> What remains before a release is not packaging. It is the questions
> [docs/releasing.md](releasing.md) gates on — a signed build, and the codec
> patent position ([#257](https://github.com/wildware-uk/clipped/issues/257)).
>
> Nothing can distribute an unlicensed build by accident.
> [`.github/workflows/release.yml`](../.github/workflows/release.yml) is the only
> thing in this repository that builds an installer and publishes it, and it
> refuses while any of the six texts below is absent from what
> `bundle.resources` collects — naming each missing file — or while the
> corresponding source of the FFmpeg it would ship is missing, incomplete, or
> the source of some other build. See
> [docs/releasing.md](releasing.md#the-licence-gate).

## Clipped's own code

MPL-2.0, file-level copyleft. Modifications to Clipped's own files stay open;
the licence permits linking against LGPL FFmpeg and against the permissive Rust
ecosystem. Every crate in both workspaces declares `license = "MPL-2.0"`.

An installed build carries `LICENSE.txt` — the same text as
[LICENSE](../LICENSE) — at the root of its licences directory.

## FFmpeg

Clipped uses `libavformat`, `libavcodec` and their siblings from a prebuilt,
LGPL-only FFmpeg, pinned by [`scripts/fetch-ffmpeg.ps1`](../scripts/fetch-ffmpeg.ps1)
and linked dynamically. The DLLs are copied beside every binary that links them
and are shipped unmodified.

**Version 3, not 2.1.** The pinned build is configured with
`--enable-version3` and reports `LGPL version 3 or later` for itself. That
matters for section 4(b) below: LGPL v3 is written as a set of additional
permissions on top of GPL v3, so both texts have to ship, and the artefact
contains only one of them.

### Section 4 of the LGPL, item by item

Section 4 is the list that applies to conveying a work that uses the Library —
Clipped's binaries. Everything in the "Discharged by" column is produced by
[`scripts/collect-notices.ps1`](../scripts/collect-notices.ps1), which writes a
directory an installer copies wholesale.

|             | Requirement                                                                                                                | Discharged by                                                                                                                                                                            | State                                                                                                                               |
| ----------- | -------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| **4(a)**    | A prominent notice with each copy that the Library is used and that the Library and its use are covered by the LGPL.       | `ffmpeg/NOTICE.md` in the payload. It names the exact build, its version, its `configure` arguments and every DLL shipped, all read out of the installed build rather than written down. | Generated **and installed**, in `licences/ffmpeg/` beside the binaries ([#123](https://github.com/wildware-uk/clipped/issues/123)). |
| **4(b)**    | A copy of the GNU **GPL** as well as the LGPL.                                                                             | `ffmpeg/LGPL-3.0.txt`, copied from the fetched build's own `LICENSE.txt`, and `ffmpeg/GPL-3.0.txt`, from [`licences/GPL-3.0.txt`](../licences/GPL-3.0.txt) in this repository.           | Generated **and installed**.                                                                                                        |
| **4(c)**    | Where copyright notices are displayed at run time, FFmpeg's must be among them, with a pointer to those two licence texts. | An about or diagnostics screen.                                                                                                                                                          | **Does not exist.** Clipped has no about screen. The recorder logs which build it loaded, which is not the same thing; see "Reporting which FFmpeg is in use" below.                                      |
| **4(d)(1)** | Either a shared-library mechanism that lets the user replace the Library, or Installation Information.                     | Dynamic linking. The DLLs sit beside the executables as ordinary files and Windows resolves them from there.                                                                             | **Met by construction, and verified** — see "Replacing the FFmpeg libraries" below.                                                 |
| **4(e)**    | Installation Information where GPL section 6 would require it.                                                             | Nothing. It only applies under 4(d)(0), and Clipped takes 4(d)(1) and ships no locked-down device.                                                                                       | Does not apply.                                                                                                                     |

### Conveying the libraries themselves

The DLLs are the Library, conveyed. On top of section 4, a release therefore has
to say which FFmpeg it carries and offer the **corresponding source of that
exact build**.

[`scripts/fetch-ffmpeg-source.ps1`](../scripts/fetch-ffmpeg-source.ps1) assembles
it. Two trees, because either alone is incomplete:

- **FFmpeg**, at the commit the shipped build was made from. The builder puts it
  in the asset name — `ffmpeg-n8.1.2-34-g9b6c8969e0-…` — and the libraries
  report the same string at run time through `av_version_info()`, which is what
  `crates/muxer/tests/ffmpeg_linkage.rs` asserts against the pin.
- **The build recipe**, at the release tag the artefact was published under.
  FFmpeg's `configure` arguments and the versions of every external library
  compiled into the DLLs live in
  [BtbN/FFmpeg-Builds](https://github.com/BtbN/FFmpeg-Builds), not in FFmpeg, so
  the source of FFmpeg alone would not let anyone rebuild what shipped.

Provenance is a commit id rather than a checksum, deliberately: a GitHub source
archive is generated on demand and its bytes are not promised to be stable,
whereas a commit id is a hash of the tree it names. The script fetches the
commit and checks that what arrived has that id.

For the pin as of this page:

|                     |                                                                               |
| ------------------- | ----------------------------------------------------------------------------- |
| Binary asset        | `ffmpeg-n8.1.2-34-g9b6c8969e0-win64-lgpl-shared-8.1.zip`                      |
| FFmpeg commit       | `9b6c8969e05b4f0b29f0f85cd501be6b3e582e6b`                                    |
| Build recipe commit | `2437e7b868da3c11872367b15f3c613b87c24819` (tag `autobuild-2026-08-09-13-03`) |

**The release attaches both archives and the generated
`CORRESPONDING-SOURCE.md`, on the same page as the installer.**
[`.github/workflows/release.yml`](../.github/workflows/release.yml) runs
`fetch-ffmpeg-source.ps1` in its gate job, the corresponding-source gate refuses
the release unless what it produced is present, complete and the source of the
build the pin now names, and the job that drafts the release attaches those
files in the same `gh release create` call as the installer
([docs/releasing.md](releasing.md#the-corresponding-source-gate)). Nobody has to
remember to do it, and no window exists in which the installer is downloadable
and its source is not.

The choice made here is to **publish the source**, not to offer it. A written
offer is permitted, but it is a commitment to answer requests from anybody who
holds a copy of the binary, for years, by a project with no staffed address; and
it would put a promise on a stranger's machine that this repository would have
to keep long after anybody is reading it. Publishing it beside the download
costs 22 MB per release, is discharged the moment the release is drafted, and
needs nobody to answer anything. It also has to stay that way: pointing at a
third party's release page would not be enough on its own, because the
obligation runs to whoever received the binary and BtbN's builds are deleted
after a few months.

**Never modify the DLLs.** If Clipped ever ships a patched FFmpeg, the patch has
to be published with the source and the modification marked. Nothing in the
build does this today and nothing should start.

### Which licence text is which

Both texts in the payload are traceable, which matters because "the LGPL v3
text" and "the GPL v3 text" are easy to conflate and only one of them is in the
artefact:

| File                  | Comes from                                                                                                           | SHA-256                                                            |
| --------------------- | -------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------ |
| `ffmpeg/LGPL-3.0.txt` | `LICENSE.txt` in the fetched build. Byte-identical to `COPYING.LGPLv3` in FFmpeg at the pinned commit.               | `da7eabb7bafdf7d3ae5e9f223aa5bdc1eece45ac569dc21b3b037520b4464768` |
| `ffmpeg/GPL-3.0.txt`  | [`licences/GPL-3.0.txt`](../licences/GPL-3.0.txt), taken verbatim from `COPYING.GPLv3` in FFmpeg at the same commit. | `8ceb4b9ee5adedde47b31e975c1d90c73ad27b6b165a1dcd80c7c545eb65b903` |

### Reporting which FFmpeg is in use

**The recorder logs it, and nothing shows it to a user.** `clipped-muxer`'s
`linkage` module answers the question — `linked_build()` returns the build
identifier, the library versions, the `configure` arguments and the licence the
loaded libraries report — and it is called in two places outside the tests:

- [`apps/recorder/src/main.rs`](../apps/recorder/src/main.rs) logs the build
  identifier, the licence and the three library versions as the first line of
  every recorder log, before anything else runs. Read out of the libraries the
  process actually loaded rather than from the pin, so a machine running an
  FFmpeg of its own — which `.cargo/config.toml` deliberately allows — says so.
- [`apps/recorder/src/capabilities.rs`](../apps/recorder/src/capabilities.rs)
  answers the capability query from the same call.

That is enough for a bug report and enough to tell which build's source is the
corresponding one. It is **not** section 4(c): a line in a log file is not a
copyright notice displayed at run time, and the desktop application still has no
about screen and no diagnostics screen naming it
([#101](https://github.com/wildware-uk/clipped/issues/101),
[#256](https://github.com/wildware-uk/clipped/issues/256)). A user who never
opens a log still cannot tell which FFmpeg they have.

### Replacing the FFmpeg libraries

This is the permission the whole arrangement rests on, so it is tested rather
than asserted. Verified on 2026-08-12, against binaries built from `ecc9aa5`:

Three copies of the FFmpeg-linked test executables were staged in separate
directories, each with a different set of FFmpeg DLLs beside it — Windows
resolves a DLL from the directory of the executable that needs it — and each was
run from the same working directory. Every FFmpeg-linked test in the workspace
is in the table; the two that drive the recorder as a child process
(`synthetic_recording`, `abrupt_termination`) had the DLLs staged beside the
example binary as well.

| DLLs beside the binaries                                   | `clipped_muxer` unit tests | `mkv_writing`  | `synthetic_recording` | `abrupt_termination` | `ffmpeg_linkage`   |
| ---------------------------------------------------------- | -------------------------- | -------------- | --------------------- | -------------------- | ------------------ |
| The pinned build, `n8.1.2-34-g9b6c8969e0-20260809`         | 24 passed                  | 12 passed      | 1 passed              | 2 passed             | 4 passed           |
| A different LGPL 8.1 build, `n8.1-11-g75d37c499d-20260430` | 24 passed                  | 12 passed      | 1 passed              | 2 passed             | 3 passed, 1 failed |
| FFmpeg 7.1, `n7.1.5-12-g1fdbca85aa`                        | will not start             | will not start | 1 failed              | 2 failed             | will not start     |

The middle row is the result. Every test that uses FFmpeg passes against a build
made four months earlier from a different commit, with a different
configuration — including the one that writes a Matroska file through
`libavformat` and probes the result, the one that runs the synthetic-recording
example as a separate process, and `abrupt_termination`, which kills that
process with `TerminateProcess` part-way through and then demuxes what survived.
That last one is the strongest of the five for this purpose: it is the test ADR
0001's container choice rests on, and it exercises `libavformat`'s cluster
writing on a file that was never closed. The single failure is
`loaded_libraries_are_the_pinned_ffmpeg_build`, which exists to assert that the
loaded build _is_ the pin:

```text
assertion `left == right` failed: linked against a different FFmpeg to the pinned one.
Loaded: FFmpeg n8.1-11-g75d37c499d-20260430 (LGPL version 3 or later);
libavformat 62.12.100, libavcodec 62.28.100, libavutil 60.26.100
  left: "n8.1-11-g75d37c499d-20260430"
 right: "n8.1.2-34-g9b6c8969e0-20260809"
```

That failure is what makes the row mean anything: it proves the substituted
libraries really were the ones loaded, rather than the pinned DLLs having been
picked up from somewhere else — and it comes from the same staged directory, run
in the same pass, as the four columns that passed.

The bottom row is the control. FFmpeg 7.1 carries different library majors
(`avformat-61` where 8.1 has `avformat-62`), so nothing that needs those
libraries can start. Three of the five executables link them directly and exit
with `0xC0000135` (`STATUS_DLL_NOT_FOUND`) before any test runs. The other two
link no FFmpeg themselves — they drive the recorder as a child process — so they
start, and then every one of their tests fails, because the recorder they launch
is the thing that cannot start. Either way nothing passes: a substitution that
is _not_ interface-compatible does not silently succeed. "Compatible" means the
same major version of each library — which is exactly what the DLL file names
carry, so a user can see it — and the relinking permission is about a modified
version of the same library, not about any FFmpeg at all.

To repeat it: `cargo test -p clipped-muxer --no-run`, then copy the five test
executables into a `deps` directory of their own and
`target/debug/examples/synthetic_recording.exe` into an `examples` directory
beside it — the layout `crates/muxer/tests/support` expects — put the DLLs in
both, replace them with another `win64-lgpl-shared` build of the same FFmpeg
major, and run the executables. Run `ffmpeg_linkage` from the same directory as
the rest: its failure is what proves which libraries were loaded. Do not run any
of it through `cargo test`: `clipped-ffmpeg-runtime` puts the pinned DLLs back
over the substituted ones on the next build, which is correct behaviour and
would silently undo the experiment.

## The Rust dependency tree

`scripts/collect-notices.ps1` writes `THIRD-PARTY-NOTICES-RUST.md` into the
payload: every third-party crate Clipped is built from, with the licence text
that crate publishes inside itself. On 2026-08-12 that was 275 crates across
both workspaces.

Two decisions in it are worth stating, because the wrong version of each is easy
to reach and impossible to see afterwards:

- **It reproduces the notices, not just the names.** MIT, BSD and ISC all
  require the copyright line and the permission notice to travel with the
  binary, and the copyright line is in the crate's own licence file rather than
  in its metadata. So each entry carries the file.
- **It lists the normal-dependency closure, which is a superset of what is
  linked.** The closure is taken over both workspaces, resolved for
  `x86_64-pc-windows-msvc` with all features. `dev-dependencies` and
  `build-dependencies` are excluded, so `bindgen`, `cc` and `tauri-build` are
  absent — nothing reached only over those edges is in a binary at all.
  Clipped's own crates are excluded too; `LICENSE.txt` in the same payload
  covers them.

  It is a superset rather than the linked set, and says so, because a
  procedural macro is an ordinary dependency of the crate that uses it. So
  `serde_derive`, `thiserror-impl`, `syn`, `quote`, `proc-macro2` and
  `unicode-ident` are all listed, and every one of them runs in the compiler and
  is no more present in a shipped binary than `bindgen` is. Telling those apart
  would mean deciding per crate which edges lead only into the compiler, and
  being wrong in the direction that drops a crate that _is_ linked. A notice for
  something you did not receive costs you a paragraph; a missing notice costs
  you a right, so the rule chosen is the one that cannot under-report. The
  generated file states the same thing at the top, so nobody reading only the
  payload is told the narrower claim.

This is not the same question as the one CI asks.
[`deny.toml`](../deny.toml) checks every crate in both graphs — including
dev-dependencies — against the licence allow-list, and rejects anything else.
That is about what the project will accept. The notices file is about what a
user has been given. Both are needed and neither substitutes for the other.

Material vendored into this repository — generated FFI bindings, copied headers —
is a third thing again, recorded by hand in
[THIRD-PARTY-NOTICES.md](../THIRD-PARTY-NOTICES.md) as CONTRIBUTING.md requires.
That file is copied into the payload as it stands.

## Codec patents: a different question, answered elsewhere

Copyright licences are not patent licences. Distributing an application that
encodes H.264 or HEVC has patent-pool implications regardless of whether the
encoder is LGPL, BSD or GPL, and nothing on this page touches that: everything
above is discharged by shipping a file or a notice, and no notice buys a patent
licence.

[**ADR 0008**](adr/0008-codec-patent-position.md) is the position
([#257](https://github.com/wildware-uk/clipped/issues/257)). It inventories what
the pinned build can encode and decode against what Clipped actually calls,
separates encoding done by GPU silicon the vendor licensed from encoding done by
software Clipped ships, and sets out what it constrains — AV1 stays the first
choice, no second software encoder for a pool codec, Opus rather than AAC for
[#392](https://github.com/wildware-uk/clipped/issues/392), and a release that
states its codec position rather than leaving a reader to infer one from a
directory of licence texts. It is `Accepted`: the position is decided and
constrains pull requests now.

**AV1 is the preference, not the outcome.** The default resolves to the most
efficient codec the machine was *measured* to support, so AV1 is chosen only on
NVIDIA Ada and later, AMD RDNA 3 and later and Intel Arc. On everything older
it resolves to **HEVC** — the one standard in that inventory with no free tier
and more than one pool — and on older hardware again to H.264. Most recordings
Clipped makes today are therefore HEVC, encoded on vendor silicon. ADR 0008
says so in its Decision; it is repeated here because this page is the other
place somebody arrives at the question.

What it does *not* settle is whether an obligation exists, and it says so — part
of it is four questions for a lawyer that nobody in this repository can answer.
Those questions block the first signed public release, and that block is a
release gate rather than a sentence: `scripts/check-release-gates.ps1` refuses
every tag until the answers are written into the record
([docs/releasing.md](releasing.md#the-codec-patent-gate)).

Two things it decides that belong on this page:

- **A release has to say what it can encode and decode, and that no patent
  licence comes with it.** `scripts/collect-notices.ps1` names the FFmpeg build
  today; naming the codecs, and disclaiming any patent grant over them, is part
  of [#123](https://github.com/wildware-uk/clipped/issues/123) rather than a
  separate ticket.
- **MPL-2.0's patent grant does not reach codec patents.** Section 2.1(b) grants
  rights from contributors over their contributions. It grants nothing over a
  third party's standard-essential patents, and a payload full of licence files
  should not be allowed to imply otherwise.

## The release checklist

Steps 1 to 6 are performed by
[`.github/workflows/release.yml`](../.github/workflows/release.yml) on a tag, and
two of them are gated rather than trusted: it will not build an installer while
step 5 is unsatisfied, and will not build one while step 4 is. Steps 7 and 8 are
still a description of what a release has to do rather than a procedure anybody
can run end to end; [docs/releasing.md](releasing.md#making-a-release) is where
they sit in the order of a release.

1. `scripts/fetch-ffmpeg.ps1` — install the pinned build.
2. Build the recorder and the desktop application in release.
3. `scripts/collect-notices.ps1` — write the licences payload from the build
   that is actually installed. It refuses to write anything if the FFmpeg it
   finds reports a licence other than LGPL, or if either licence text is
   missing. `-Destination` may be an empty directory or a payload from a
   previous run; anything else is refused rather than emptied, and the payload
   is assembled beside the destination and moved onto it only once it is
   complete, so a failed run never leaves half of one where the last one was.
4. `scripts/fetch-ffmpeg-source.ps1` — assemble the corresponding source and its
   manifest. The release workflow runs this in its gate job, into a directory
   outside the cached FFmpeg tree, and refuses the release unless what it
   produced is complete and is the source of the pinned build.
5. Include the payload in the installer, beside the binaries and the seven
   FFmpeg DLLs. Both are staged by
   [`scripts/stage-installer-payload.ps1`](../scripts/stage-installer-payload.ps1)
   and collected by `bundle.resources` ([docs/packaging.md](packaging.md)).
6. Attach the source archives and `CORRESPONDING-SOURCE.md` to the release. The
   workflow does this in the same `gh release create` call as the installer, so
   the source cannot lag behind the binary it corresponds to.
7. Check the about or diagnostics screen names the FFmpeg build and points at
   the licence texts ([#256](https://github.com/wildware-uk/clipped/issues/256)).
8. Substitute the DLLs in the installed application with another compatible
   build and confirm it still records, as under "Replacing the FFmpeg libraries"
   above.

Steps 3 and 4 have their own tests —
[`scripts/test-collect-notices.ps1`](../scripts/test-collect-notices.ps1) and
[`scripts/test-fetch-ffmpeg-source.ps1`](../scripts/test-fetch-ffmpeg-source.ps1)
— and so does the refusal that stops a release happening without them
([`scripts/test-check-release-gates.ps1`](../scripts/test-check-release-gates.ps1)).
CI runs all three, in the Rust job, after the step that installs the pinned
FFmpeg they need. Unlike `scripts/test-check-prerequisites.ps1`, which is run by
hand because it guards a contributor's setup and fails visibly for whoever broke
it, these guard what a release is obliged to give a user, and a regression in
them is silent by construction: a payload missing two hundred notices reads
exactly like a complete one.
