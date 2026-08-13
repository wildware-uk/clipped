# 0008. AV1 is the codec Clipped commits to, and the AVC and HEVC exposure is named rather than assumed away

- Status: Proposed
- Date: 2026-08-13
- Issue: [#257](https://github.com/wildware-uk/clipped/issues/257)

> **This is not legal advice, and nobody who wrote it is a lawyer.** It is an
> inventory of what Clipped distributes, what the published licensing terms say
> about that, and which questions are left over. Every factual claim below is
> marked **sourced** — traceable to a document or to this repository — or
> **reading**, meaning it is an inference someone qualified may disagree with.
> No statement here says that distributing Clipped is safe, because nothing
> available to this project can establish that.

## Context

[docs/licensing.md](../licensing.md) settles what a release of Clipped has to
carry in order to satisfy the **copyright** licences of the things it ships:
MPL-2.0 for its own code, LGPL v3 for the FFmpeg DLLs, and several hundred
permissive crate licences. It says so itself, in a section that exists to mark
the hole: none of that touches **patents**.

The two are unrelated in the way that matters here. A copyright licence gives
permission to copy, modify and distribute a *particular expression* — this
source, these binaries. A patent gives its holder the right to exclude others
from *practising an invention*, whoever wrote the code and under whatever terms
they published it. H.264 and HEVC are standards assembled from patented
techniques, held by dozens of companies, and licensed collectively through
patent pools. So `libopenh264` being BSD-2-Clause, `libkvazaar` being BSD-3 and
FFmpeg being LGPL are answers to a question the pools are not asking. ADR 0004
said as much in one consequence and deferred it; this record is that deferral
coming due.

Why now rather than later: a pool's claim attaches to **distribution**, and the
first signed public release is the first distribution ([#123](https://github.com/wildware-uk/clipped/issues/123)).
Before that, this is a decision that can still be made cheaply — codecs can be
dropped, an FFmpeg build can be narrowed, a default can be fixed. Afterwards it
is a decision about binaries that are already on other people's machines.

**Not in scope:** obtaining any licence, forming a legal opinion, or deciding
whether Clipped will ever be a commercial product. The output is the inventory,
the position that follows from it, and the questions a maintainer would have to
put to a lawyer.

### What Clipped actually distributes today

Everything in this section was checked on 2026-08-13 against the pinned build at
`third-party/ffmpeg/current` and the code in this repository, rather than
assumed. **Sourced.**

The seven FFmpeg DLLs go into the installer
([docs/packaging.md](../packaging.md)). `bin/` in the fetched build contains
those seven plus `ffmpeg.exe`, `ffplay.exe` and `ffprobe.exe`, and **nothing
else** — in particular there is no `openh264.dll`. Every external library named
in the build's `configure` line is compiled *into* `avcodec-62.dll`. That single
fact decides most of what follows, and it is checkable with `ffmpeg -buildconf`
and a directory listing.

What that DLL can do, against what Clipped asks it to do:

| Codec | In the shipped DLLs | Called by Clipped today | Family |
| --- | --- | --- | --- |
| **H.264 / AVC** | `libopenh264` (software encoder, statically linked), `h264_nvenc`, `h264_amf`, `h264_qsv`, `h264_mf`, `h264_vaapi`, `h264_vulkan`, `h264_d3d12va`; decoders `h264`, `libopenh264`, plus vendor decoders | Encoded by `SoftwareEncoder` through `libopenh264` (`crates/encoder/src/software/avcodec.rs:47`) and by the hardware backends through the vendor SDKs, not through FFmpeg. Decoded when a thumbnail is generated. | Via LA AVC pool |
| **HEVC / H.265** | `libkvazaar` (software encoder), `hevc_nvenc`, `hevc_amf`, `hevc_qsv`, `hevc_mf`, `hevc_vaapi`, `hevc_vulkan`, `hevc_d3d12va`; decoder `hevc` and vendor decoders | Encoded **only** on vendor hardware, through the vendor SDKs. `libkvazaar` is present and nothing in the workspace names it. Decoded for thumbnails. | Access Advance, VCL Advance (ex-Via LA), and unaffiliated holders |
| **AV1** | `libsvtav1`, `libaom-av1`, `librav1e` (software encoders), `av1_nvenc`, `av1_amf`, `av1_qsv`, and the rest; decoders `av1`, `libdav1d` | Encoded on vendor hardware where the machine measures support. Software AV1 is [#157](https://github.com/wildware-uk/clipped/issues/157) and is not written. Decoded for thumbnails. | AOMedia Patent License 1.0; separately, Sisvel's AV1 pool |
| **VVC / H.266** | `libvvenc` encoder, `vvc` decoder | Nothing. Clipped has no VVC anywhere. | Access Advance VVC, VCL Advance |
| **AAC** | FFmpeg's native `aac` encoder, `aac_mf`, decoder | Nothing encodes it. `clipped-export` can *recognise* it in a source file and copy its packets. | Via LA AAC pool |
| **Opus** | `libopus`, native `opus` | Nothing yet. It is one of the two candidates in [#392](https://github.com/wildware-uk/clipped/issues/392). | Royalty-free grants |
| **JPEG, PNG, WebP** | `mjpeg`, `png`, `libwebp` | Thumbnails are MJPEG (`crates/library/src/thumbnail/render.rs:1271`); screenshots are PNG, JPEG or lossless WebP | JPEG's patents expired long ago; PNG and WebP were designed unencumbered |

Three further facts about the product, all checkable:

- **Recorded audio is uncompressed.** `RECORDING_AUDIO_CODEC` is `PcmS16Le`
  (`crates/muxer/src/audio.rs:53`), so no audio codec is practised at record
  time at all. This is why AAC is a *future* question and not a present one.
- **Export re-encodes nothing.** `crates/export` implements the stream-copy path
  only; the re-encoding path is [#90](https://github.com/wildware-uk/clipped/issues/90).
  Copying coded packets from one container to another practises neither an
  encoder nor a decoder.
- **The default codec is `Automatic`, not AV1.** `RecordingSettings::default`
  yields `CodecPreference::Automatic` (`crates/session/src/settings.rs:288`),
  and `best_codec_for` (`crates/session/src/encoding.rs:217`) walks
  `Codec::EFFICIENCY_ORDER` — `[Av1, Hevc, H264]` — taking the first codec the
  machine was *measured* to support and falling back to H.264. So AV1 is the
  first choice, and on the large population of machines whose GPU registers no
  AV1 encoder — anything NVIDIA before Ada, anything AMD before RDNA 3, most
  Intel before Arc — the default silently resolves to **HEVC**, and on older
  hardware again to **H.264**. Saying "Clipped defaults to AV1" is true of the
  preference and false of the outcome on a lot of real machines, and the
  distinction matters for every paragraph below.

### What the published terms say

**Sourced**, with links in [Sources](#sources).

| | What the document says |
| --- | --- |
| **AVC, Via LA** | Royalties are per unit on "AVC encoders and decoders": 1–100,000 units a year at **$0.00** (available to one legal entity in an affiliated group), 100,001–5,000,000 at $0.20 each, above that $0.10 each, under an enterprise cap of $9.75M a year from 2017 onwards. |
| **openh264, Cisco** | "The binary form of this Software is distributed by Cisco under the AVC/H.264 Patent Portfolio License from MPEG LA", and the coverage is conditioned on the fact that "the Cisco-provided binary is separately downloaded to an end user's device, and **not integrated into or combined with third party software prior to being downloaded**". The FAQ adds that "Cisco is only covering the royalties that would apply to the binary module", that products using it "must download it at the time the product or project is installed", and that "Cisco will not be liable for any licensing fees incurred by other parties". |
| **HEVC, Access Advance** | A royalty is due "upon the Sale of a Consumer HEVC Product". On software specifically, the FAQ says: "In general, HEVC software downloaded by users requires a license. However, there are some situations wherein a license is not needed", and directs the reader to contact them about their particular software. There is no published 100,000-unit free tier equivalent to AVC's. HEVC is licensed by more than one pool — Access Advance, and the former Via LA HEVC pool now operating as VCL Advance — plus holders in neither, so no single agreement clears the standard. |
| **AV1, AOMedia** | The AOMedia Patent License 1.0 grants a "non-sublicensable, perpetual, worldwide, non-exclusive… patent license to its Necessary Claims to make, use, sell, offer for sale, import or distribute any Implementation", no-charge and royalty-free, to any implementer regardless of Alliance membership, with defensive termination if the licensee sues another implementation over Necessary Claims. |
| **AV1, Sisvel** | Sisvel operates an AV1 pool on behalf of holders who are not AOMedia members and made no royalty-free pledge. Sisvel's own position is that "the Alliance for Open Media legitimately grants royalty-free access to intellectual property contributed by its members" and that its pool covers other patents; it has so far licensed hardware implementations rather than software. |
| **Opus** | Royalty-free patent grants from Xiph.Org, Broadcom and Microsoft/Skype, written to be open-source-compatible. No pool exists. |
| **AAC** | Via LA's AAC pool charges per-unit royalties on products shipping an AAC encoder or decoder. |

One corroborating observation, from a secondary source and offered as
illustration rather than as authority: Microsoft sells **HEVC Video Extensions**
in the Microsoft Store for a small fee, and gives away an otherwise identical
"from Device Manufacturer" edition to OEMs who have already paid, rather than
including HEVC in Windows. A company with Microsoft's licensing position still
treats per-copy HEVC as something somebody has to pay for. That is the shape of
the problem in one example.

## Decision

**Clipped treats AV1 as the codec it commits to, uses AVC and HEVC only where a
GPU vendor's already-licensed silicon does the encoding, ships no software AVC
or HEVC encoding that it can avoid shipping, and does not make its first signed
public release until a maintainer has put the four questions below to a lawyer
and written the answer down.**

That resolves into six things a contributor can check against a pull request.

1. **`Codec::EFFICIENCY_ORDER` is a licensing constraint as well as an
   efficiency one.** AV1 stays first. A change that promotes HEVC or H.264 above
   it, for compression reasons or compatibility reasons, is a change to this
   record and not a tuning decision.

2. **Clipped adds no second software encoder for a pool codec.** `libkvazaar`
   (HEVC) and `libvvenc` (VVC) are inside the DLLs and stay uncalled. The
   software fallback stays exactly one encoder. Where a *second* software codec
   is wanted, [#157](https://github.com/wildware-uk/clipped/issues/157)'s
   software AV1 through `libsvtav1` is the one to write, and this record makes
   that preference explicit rather than incidental: it is the software encoder
   whose patent position is a published royalty-free grant rather than a pool.

3. **The audio codec for the player and export paths
   ([#392](https://github.com/wildware-uk/clipped/issues/392)) is Opus, not
   AAC.** That issue records the choice as open and says the work "needs no new
   dependency, no licence review" — true of copyright, false of patents. AAC is
   a per-unit pool royalty; Opus was designed to avoid one, and is what
   WebView2's WebM support wants anyway. Choosing AAC for the MP4 export path
   would add a *third* pool to the inventory to save a container conversion.

4. **A release states its codec position.** The notices payload
   (`scripts/collect-notices.ps1`) already reads the FFmpeg build and names it;
   it must also say which codecs the shipped libraries can encode and decode,
   and state plainly that no patent licence for any codec is granted by Clipped
   or by any licence in the payload. MPL-2.0 section 2.1(b) grants patent rights
   *from contributors, over their contributions* — it grants nothing over a
   third party's standard-essential patents, and a user who reads a repository
   full of licence files should not be left to infer otherwise.

5. **Hardware encoding is the preferred path for AVC and HEVC, and the reason is
   recorded as a reading, not a fact.** The argument is that the encoding is
   performed by silicon whose vendor holds the licences, and that Clipped is an
   application invoking a licensed component rather than an implementation of
   the standard. That is the ordinary industry practice and it is what every
   comparable recorder relies on. It is **not** established here: Access
   Advance's own FAQ declines to state a general rule for software and asks
   implementers to contact them, which is the opposite of a safe harbour.

6. **The first signed public release is blocked on the questions below being
   asked.** Not on a particular answer — on somebody qualified having answered.

### The four questions for a lawyer

Written to be answerable, rather than as "are we allowed to ship this".

1. **Does the installer make Clipped an "AVC encoder/decoder product"?** It
   distributes `avcodec-62.dll` containing a statically linked `libopenh264`
   H.264 encoder and FFmpeg's own H.264 and HEVC decoders, and the application
   calls the encoder. If it does: does the 1–100,000 units at $0.00 tier reach a
   project distributing free downloads, and **who is the licensee** when the
   distributor is an individual or an unincorporated open-source project rather
   than a company selling units? The Via LA tier is written as "available to one
   legal entity in an affiliated group", which presumes a legal entity.

2. **Does an application that drives NVENC, AMF or Quick Sync need its own
   AVC or HEVC licence, given the vendor licensed the silicon?** And is the
   answer different for the two standards and for the several HEVC pools? This
   is the question the whole hardware-first position rests on, and it is the one
   the published material comes closest to refusing to answer.

3. **Does shipping software encoders Clipped never calls create an
   obligation?** `libkvazaar` and `libvvenc` are inside a DLL we redistribute.
   Does obligation attach to the capability distributed, or to the capability
   offered to the user? The answer decides whether the "build our own FFmpeg"
   alternative below is necessary or merely tidy.

4. **Does the distribution channel change the answer?** A GitHub release, a
   signed installer from a UK company, and the Microsoft Store are three
   different arrangements, and the Store is the one where a platform's own
   licensing may already cover some of this.

## Alternatives

### Ship what is pinned today, and take out the licences

Sign up to Via LA for AVC and to Access Advance and VCL Advance for HEVC, pay
what the tiers say, and stop thinking about it. For AVC that is plausibly £0 a
year: an open-source recorder is nowhere near 100,000 units, and the published
first tier is $0.00.

It was not chosen as *the* answer because it is not one decision, it is three
enrolments, at least one of which (HEVC) has no free tier, all of which require
a legal entity to sign, and none of which a documentation ticket can perform. It
remains the right thing to do if question 1 or question 2 comes back as "yes,
you need one" — and it is deliberately not ruled out here, because the ordinary
outcome of asking a lawyer may well be "sign the AVC licence, it costs nothing,
and stop worrying". What this record refuses to do is *assume* that outcome.

### Build FFmpeg ourselves, without the software AVC, HEVC and VVC encoders

`--disable-libopenh264 --disable-libkvazaar --disable-libvvenc`, and the shipped
DLLs would then contain no software encoder for any pool codec. The hardware
wrappers are thin shims over the vendor SDKs and could stay; the decoders would
have to stay too, because thumbnails have to decode whatever the machine
recorded.

This is the strongest exposure-reducing option that keeps the product intact,
and it is genuinely close. It loses on cost and on scope. ADR 0004 rejected
building FFmpeg from source, with reasons that still hold — an afternoon of
MSYS2 for every contributor, minutes on every cold CI run — and reversing that
is a decision about the build system, not about codecs. It also does not remove
the decoders, and it costs the software fallback its encoder unless
[#157](https://github.com/wildware-uk/clipped/issues/157) lands first.

**What would make it win later:** question 3 coming back as "yes, shipping the
capability is what counts". At that point ADR 0004's rejection is outweighed and
this becomes the work, in this order: software AV1 (#157), then our own build.

### Use Cisco's openh264 the way Firefox does

Cisco's whole reason for publishing openh264 binaries is to move this obligation
onto itself. Firefox downloads the module from Cisco's servers at runtime and
Cisco pays the pool. If that mechanism applied, the AVC half of this record
would evaporate.

**It does not apply, and this is the sharpest finding here.** The binary licence
conditions the coverage on the Cisco-built binary being "separately downloaded
to an end user's device, and not integrated into or combined with third party
software prior to being downloaded". Clipped ships openh264's *code*, compiled
by BtbN into `avcodec-62.dll`, inside an installer — combined with third-party
software, not separately downloaded, and not Cisco's binary at all. Cisco's FAQ
is explicit that it covers "its own binary module" and "will not be liable for
any licensing fees incurred by other parties". So the BSD-2-Clause source
licence is what Clipped has, and a BSD licence grants no patent rights over the
standard.

Adopting the mechanism properly — downloading Cisco's `openh264` DLL at first
run and loading it dynamically — is technically possible and was considered. It
was rejected for this ticket because it contradicts SPEC.md's local-first,
no-silent-network stance (AGENTS.md section 14), it makes the software fallback
depend on Cisco's CDN existing, FFmpeg would have to be built to load it
dynamically, and Cisco's own condition on being "not integrated into or combined
with third party software" is written for a plugin model rather than for a
bundled recorder. **What would make it win later:** a maintainer wanting the
software H.264 fallback specifically, and question 1 coming back badly. It is
the only mechanism that makes software H.264 free, so it stays on the table.

### AV1 only: refuse to encode AVC or HEVC at all

The cleanest position available. AOMedia's licence is royalty-free by
construction and Clipped would practise nothing else.

Rejected on product grounds, not licensing ones. AV1 hardware encoding exists
only on NVIDIA Ada and later, AMD RDNA 3 and later and Intel Arc; every machine
older than that would be left with software AV1 — several times the CPU cost of
H.264 — or with nothing. A recorder that will not record on a GTX 1080 is not
the product SPEC.md section 9 describes, and the risk being avoided is one that
every comparable recorder carries. It is also the option that would be *forced*
if question 2 came back badly and no licence were obtainable, so it is worth
having written down.

### Hardware only: keep AVC and HEVC, delete the software fallback

Encode AVC and HEVC exclusively on vendor silicon and remove `SoftwareEncoder`.
This removes the one place where Clipped's own process performs AVC encoding in
software, which is the clearest exposure in the inventory.

Rejected because the software fallback is what records on a machine whose
hardware encoder is absent, broken, out of session slots or held by another
application, and issue #18 exists precisely because that machine is real.
Deleting it trades a recording that happens for a risk that might not be there.
The narrower version — keep the fallback but make it AV1 (#157) — gets most of
the benefit without the loss, and is why point 2 of the Decision names it.

### Say nothing and ship

Every screen recorder has this question and most say nothing publicly about it.
The pools have not historically pursued small open-source projects, and the
practical risk to a free tool with no revenue is low.

Rejected because it is not a decision, it is the absence of one, and because it
puts the cost on somebody else: a contributor who forks Clipped, a company that
bundles it, or a maintainer who later wants to sell something. Writing the
inventory down costs one document and makes all of those cheaper. Note also what
this record does *not* claim — it does not say the risk is high. It says the
risk is unquantified by anyone qualified, and that a signed public release is
the wrong moment to still be guessing.

## Consequences

- **AV1 stops being only an efficiency preference.** `Codec::EFFICIENCY_ORDER`
  and `best_codec_for` now have a second reason to be in that order, recorded
  where somebody reordering them will see it. Nothing in the code changes today.
- **[#392](https://github.com/wildware-uk/clipped/issues/392) has one fewer
  open decision, and gained a reason.** Opus for both the player path and the
  export path. If a future maintainer wants AAC for MP4 compatibility, that is a
  change to this record, and the thing to weigh is a third patent pool against a
  container.
- **[#157](https://github.com/wildware-uk/clipped/issues/157) is promoted from
  "a narrow case" to the preferred software encoder.** Its own text calls
  software AV1 "a real but narrow case"; this record adds the reason it is worth
  doing anyway, and the day it lands, dropping `libopenh264` becomes a cheap
  option rather than a loss of function.
- **Work this creates**, none of it in this pull request:
  - the notices payload has to name the codecs and disclaim patent grants
    (Decision point 4) — a change to `scripts/collect-notices.ps1` and its test,
    belonging with [#123](https://github.com/wildware-uk/clipped/issues/123);
  - the four questions have to be put to a lawyer and the answers written into
    this record, which is what would move it from Proposed to Accepted;
  - if question 3 comes back badly, building our own FFmpeg becomes real work
    against ADR 0004.
- **What becomes hard:** offering HEVC as a *headline* feature, promoting it in
  the UI above AV1, or adding software HEVC — each of those now has to argue
  with this record first. That is the intended cost.
- **What has to be watched:**
  - **AVC patent expiry.** The AVC pool's patents are old and the standard dates
    from 2003; as they expire the AVC half of this record shrinks towards
    nothing. Nobody here has checked the expiry dates, and that is exactly the
    sort of question a lawyer answers in a paragraph.
  - **Pool terms move, sometimes quietly.** Via LA restructured its AVC
    *streaming* fees for new licensees from 2026, and Access Advance has been
    deferring an HEVC rate increase through 2026. Neither is Clipped's category —
    both are evidence that "we checked the rates once" has a shelf life.
  - **Sisvel's AV1 pool.** It has licensed hardware implementations so far. If it
    starts licensing software implementations, AV1 stops being the clean answer
    and this record needs revisiting rather than repeating.
  - **A player in the window ([#304](https://github.com/wildware-uk/clipped/issues/304)).**
    Playback in WebView2 decodes through the platform, not through Clipped, and
    the platform's licensing is Microsoft's — but it is a new place where
    decoding happens, and worth a sentence here when it lands.
- **The honest limit of this record.** It establishes what is distributed and
  what the published terms say. It does not establish whether an obligation
  exists, because the two documents that would settle it — Via LA's AVC
  agreement as applied to a free open-source download, and Access Advance's view
  of software running on licensed hardware — do not answer the question in
  public, and one of them explicitly says to ask.

## Sources

Retrieved 2026-08-13.

| Claim | Source |
| --- | --- |
| AVC per-unit rates, the 100,000-unit tier and the enterprise cap | [Via LA, AVC/H.264 licence fees](https://www.via-la.com/licensing-programs/avc-h-264/) |
| openh264 binary coverage, and its "not integrated into or combined with third party software" condition | [openh264.org BINARY_LICENSE.txt](https://www.openh264.org/BINARY_LICENSE.txt) |
| Cisco covers only its own binary module; others bear their own fees | [openh264.org FAQ](https://www.openh264.org/faq.html) |
| HEVC royalty due on sale of a Consumer HEVC Product | [Access Advance, where and when a royalty is due](https://accessadvance.com/topic-where-and-when-is-a-royalty-due/) |
| "In general, HEVC software downloaded by users requires a license… contact us" | [Access Advance FAQ](https://accessadvance.com/faq/) |
| AOMedia Patent License 1.0 terms | [aomedia.org patent licence](https://aomedia.org/license/patent-license/) |
| Sisvel's AV1 pool and its position on AOMedia's grant | [Sisvel, VP9/AV1 Q&A](https://www.sisvel.com/insights/vp9-av1-q-and-a/) |
| Opus royalty-free patent grants | [opus-codec.org licence](https://opus-codec.org/license/) |
| AAC per-unit pool royalties | [Via LA, AAC](https://www.via-la.com/licensing-programs/aac/) |
| Windows ships HEVC as a paid Store extension | secondary reporting, e.g. [Microsoft Q&A on HEVC Video Extensions](https://learn.microsoft.com/en-us/answers/questions/1182851/hevc-video-extensions) — illustrative only |

The claims about what Clipped ships were taken from `ffmpeg -buildconf`,
`ffmpeg -encoders` and `ffmpeg -decoders` run against
`third-party/ffmpeg/current`, from a listing of that build's `bin/` directory,
and from the files cited inline. They can be re-derived by anyone with the
repository.
