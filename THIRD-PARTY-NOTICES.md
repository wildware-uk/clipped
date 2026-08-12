# Third-party notices

Clipped is Mozilla Public License 2.0 (see [LICENSE](LICENSE)). This file
records the third-party material that is **redistributed inside this
repository**, together with the notices those licences require us to carry
(AGENTS.md sections 11 and 12).

It is not a dependency list. Crates fetched from crates.io are recorded in
`Cargo.lock` and checked against [deny.toml](deny.toml); FFmpeg is downloaded by
`scripts/fetch-ffmpeg.ps1` into a gitignored directory and linked dynamically,
so no FFmpeg code is redistributed here — see
[docs/adr/0004-ffmpeg-dependency-strategy.md](docs/adr/0004-ffmpeg-dependency-strategy.md).

**A release ships more than this repository contains**, and the difference is
where most of the licence obligations live: FFmpeg's DLLs, and the notices of
every Rust crate compiled into the binaries.
[docs/licensing.md](docs/licensing.md) is the list of what a release has to
carry and where each item stands; `scripts/collect-notices.ps1` assembles the
payload, and copies this file into it unchanged.

## The GNU General Public License version 3 (text)

**Where:** `licences/GPL-3.0.txt`

**What:** the text of the GPL v3, verbatim. It is here because a release that
ships FFmpeg has to install it, and the FFmpeg build does not contain it: LGPL
v3 is written as a set of additional permissions on top of GPL v3, so section
4(b) of the LGPL asks for both texts, and the artefact's own `LICENSE.txt` is
the LGPL alone. `scripts/collect-notices.ps1` copies this file into the licences
payload beside that one. See
[docs/licensing.md](docs/licensing.md#section-4-of-the-lgpl-item-by-item).

Nothing in Clipped is under the GPL, and nothing may be
([ADR 0004](docs/adr/0004-ffmpeg-dependency-strategy.md)). This is a licence
document being carried, not a licence being taken on.

**Source:** [FFmpeg/FFmpeg](https://github.com/FFmpeg/FFmpeg), `COPYING.GPLv3`
at commit `9b6c8969e05b4f0b29f0f85cd501be6b3e582e6b` — the commit the pinned
FFmpeg build was made from, so the text carried is the one that build's own
licence refers to. SHA-256
`8ceb4b9ee5adedde47b31e975c1d90c73ad27b6b165a1dcd80c7c545eb65b903`.

**Licence:** the document is copyright the Free Software Foundation and carries
its own terms, reproduced below from the second paragraph of the file itself.
Copying it verbatim, which is all that is done here and all that a release does,
is what those terms permit.

```text
 Copyright (C) 2007 Free Software Foundation, Inc. <http://fsf.org/>
 Everyone is permitted to copy and distribute verbatim copies
 of this license document, but changing it is not allowed.
```

**How it was produced, and what was changed:** downloaded from
`https://raw.githubusercontent.com/FFmpeg/FFmpeg/9b6c8969e05b4f0b29f0f85cd501be6b3e582e6b/COPYING.GPLv3`
and committed unmodified. Nothing may be edited in it — "changing it is not
allowed" is the term above, and a licence text with a typo in it is no longer
the licence.

## AMD Advanced Media Framework headers (AMF SDK)

**Where:** `crates/encoder/src/windows/amf/sys.rs`

**What:** Rust FFI bindings generated with
[bindgen](https://github.com/rust-lang/rust-bindgen) 0.72.1 from the public
headers of AMD's AMF SDK. The file is a derivative of those headers — AMD's type
names, field names, enumerators and constant values — so their licence and
notices travel with it.

**Source:**
[GPUOpen-LibrariesAndSDKs/AMF](https://github.com/GPUOpen-LibrariesAndSDKs/AMF),
tag `v1.4.30`, commit `a118570647cfa579af8875c3955a314c3ddd7058`, the headers
under `amf/public/include/`. The bindings are generated from `core/Factory.h`,
`core/Surface.h`, `core/Buffer.h`, `components/VideoEncoderVCE.h`,
`components/VideoEncoderHEVC.h`, `components/ColorSpace.h` and
`components/VideoConverter.h`, and from everything those include.

**Licence:** MIT, as reproduced below from the headers themselves. The notice
regarding standards above it is AMD's and is part of the same header block, so
it is carried too.

**How it was generated, and what was changed:** the command is recorded in
[docs/encoder-pipeline.md](docs/encoder-pipeline.md#the-amf-bindings-and-their-licence)
and repeated in the file's own header comment. Nothing in the generated output
is edited by hand; the only modifications are the ones the command asks for
(comments stripped) and the attribution comment added at the top of the file.
The identifiers bindgen cannot generate — the property-name and component-name
macros, which are wide string literals, the interface identifiers, which are
static inline functions, and the version-packing macro — are transcribed by hand
into `settings.rs` and `api.rs`, each beside a comment naming what it came from.

```text
Notice Regarding Standards.  AMD does not provide a license or sublicense to
any Intellectual Property Rights relating to any standards, including but not
limited to any audio and/or video codec technologies such as MPEG-2, MPEG-4;
AVC/H.264; HEVC/H.265; AAC decode/FFMPEG; AAC encode/FFMPEG; VC-1; and MP3
(collectively, the "Media Technologies"). For clarity, you will pay any
royalties due for such third party technologies, which may include the Media
Technologies that are owed as a result of AMD providing the Software to you.

MIT license

Copyright (c) 2018 Advanced Micro Devices, Inc. All rights reserved.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.  IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.
```

## NVIDIA Video Codec SDK header (`nvEncodeAPI.h`)

**Where:** `crates/encoder/src/windows/nvenc/sys.rs`

**What:** Rust FFI bindings generated with
[bindgen](https://github.com/rust-lang/rust-bindgen) 0.72.1 from NVIDIA's
`nvEncodeAPI.h`. The file is a derivative of that header — NVIDIA's type names,
field names, enumerators and constant values — so the header's licence and
notices travel with it.

**Source:** [FFmpeg/nv-codec-headers](https://github.com/FFmpeg/nv-codec-headers),
tag `n12.0.16.2`, `include/ffnvcodec/nvEncodeAPI.h`, SHA-256
`808db2a21232839ee8a6057601f1964e90b20385f6dbf080f3cd33f6470c66c4`. That
distribution is what makes the header redistributable: the copy inside the Video
Codec SDK carries NVIDIA's SDK agreement instead.

**Licence:** MIT, as reproduced below from the header itself.

**How it was generated, and what was changed:** the command is recorded in
[docs/encoder-pipeline.md](docs/encoder-pipeline.md#the-bindings-and-their-licence)
and repeated in the file's own header comment. Nothing in the generated output
is edited by hand; the only modifications are the ones the command asks for
(comments stripped, some items excluded) and the attribution comment added at
the top of the file. The identifiers bindgen cannot generate — the `static const
GUID` values and the `*_VER` version macros — are transcribed by hand into
`settings.rs` and `api.rs`, each beside a comment naming what it came from.

```text
This copyright notice applies to this header file only:

Copyright (c) 2010-2022 NVIDIA Corporation

Permission is hereby granted, free of charge, to any person
obtaining a copy of this software and associated documentation
files (the "Software"), to deal in the Software without
restriction, including without limitation the rights to use,
copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the software, and to permit persons to whom the
software is furnished to do so, subject to the following
conditions:

The above copyright notice and this permission notice shall be
included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES
OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT
HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY,
WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR
OTHER DEALINGS IN THE SOFTWARE.
```

## Intel oneVPL API headers (`mfxvideo.h` and what it includes)

**Where:** `crates/encoder/src/windows/quicksync/sys.rs`

**What:** Rust FFI bindings generated with
[bindgen](https://github.com/rust-lang/rust-bindgen) 0.72.1 from Intel's oneVPL
public API headers. The file is a derivative of those headers — Intel's type
names, field names, enumerators and constant values — so their licence and
notices travel with it.

**Source:** [intel/libvpl](https://github.com/intel/libvpl), tag `v2.15.0`,
`api/vpl/`. Five headers are reachable from `mfxvideo.h`, and all five
contribute to the generated file:

| File | SHA-256 |
| --- | --- |
| `mfxvideo.h` | `242cf5ebedd0101c7867ad004c562e9db6364d98a4c905e28f622e02b5b39519` |
| `mfxsession.h` | `d9a3d568df1db1b6267e0992c71cbe2522566e7b16bb80f8e6c70a473a4598c7` |
| `mfxstructures.h` | `d0d31b2006fa6053346338ae984708106d4b9df43a94f2e956d340e104e4dad5` |
| `mfxcommon.h` | `c290e6ae15f35436097f0b605c02566fa67094cf449d090520d23c7e3fa194a8` |
| `mfxdefs.h` | `1b3dd675af7927d74d716cef0a45e2079ffbc1e988290a6db558665bdc217bd6` |

**Licence:** MIT, as reproduced below from that repository's `LICENSE`; each
header also carries `SPDX-License-Identifier: MIT` at the top of itself. Only
the API headers are used. No part of the libvpl dispatcher or runtime is
redistributed here: Clipped loads the runtime the Intel graphics driver
installed, which is why the licence question stops at the headers (see
[docs/encoder-pipeline.md](docs/encoder-pipeline.md#the-quick-sync-backend)).

**How it was generated, and what was changed:** the command is recorded in
[docs/encoder-pipeline.md](docs/encoder-pipeline.md#the-bindings-and-their-licence-1)
and repeated in the file's own header comment. Nothing in the generated output
is edited by hand; the only modifications are the ones the command asks for
(comments stripped, functions excluded) and the attribution comment added at the
top of the file. The entry point signatures bindgen was told not to generate —
there is no import library to link them against — are transcribed by hand into
`api.rs`, each beside the declaration it came from.

```text
MIT License

Copyright (c) 2020 Intel Corporation

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
