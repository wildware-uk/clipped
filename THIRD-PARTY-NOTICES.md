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
Notices for the files a *release build* ships alongside Clipped belong with the
packaging work, which does not exist yet.

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
