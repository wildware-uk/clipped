# FFmpeg

Clipped uses FFmpeg as libraries, not as a command-line tool: containers,
remuxing, thumbnails and waveforms all go through `libavformat` and
`libavcodec` inside the recorder process. This page is how to build against it,
what is pinned, and what has to happen when the pin moves.

Why it is set up this way — and in particular why the build must not be a GPL
one — is [ADR 0004](adr/0004-ffmpeg-dependency-strategy.md).

## Setting up a clean clone

Three steps, once.

**1. Install LLVM.** The FFmpeg binding generates its bindings from FFmpeg's own
headers at build time, which needs `libclang.dll`.

```text
winget install LLVM.LLVM
```

**2. Fetch the pinned FFmpeg build.** About 60 MB, extracted into
`third-party/ffmpeg/`, which is gitignored.

```text
powershell -ExecutionPolicy Bypass -File scripts/fetch-ffmpeg.ps1 -PersistEnvironment
```

`-PersistEnvironment` writes `FFMPEG_DIR` to your user environment so new shells
find it. Without it the script only prints the value, and you set it yourself.

**3. Open a new shell and build.**

```text
cargo build --workspace
cargo test --workspace
```

`cargo test -p clipped-muxer` is the one to run if you want to confirm the link
specifically: it loads the libraries and asserts that what loaded is the pinned
build.

### When it goes wrong

| Symptom | Cause |
| --- | --- |
| `Could not find ffmpeg with vcpkg`, or a `pkg-config` error, while building `ffmpeg-sys-the-third` | `FFMPEG_DIR` is not set in this shell. Re-run the fetch script, or open a new shell if you used `-PersistEnvironment`. |
| `Unable to find libclang` | LLVM is not installed, or not on `PATH`. Install it, or set `LIBCLANG_PATH` to the directory containing `libclang.dll`. |
| `The code execution cannot proceed because avformat-62.dll was not found` | Something ran a binary that was built before the FFmpeg libraries were copied beside it. `cargo build` again; `crates/muxer/build.rs` does the copying. |
| A test fails saying it linked against a different FFmpeg to the pinned one | `FFMPEG_DIR` points at some other FFmpeg — a system-wide install, or a build left over from an older pin. |

The build error for a missing `FFMPEG_DIR` is poor, and it comes from a
dependency's build script rather than from anything in this repository, so it
cannot be improved from here. Catching it in `scripts/check-prerequisites.ps1`
instead is [issue #122](https://github.com/wildware-uk/clipped/issues/122).

## What is pinned

| | |
| --- | --- |
| Build | [BtbN/FFmpeg-Builds](https://github.com/BtbN/FFmpeg-Builds) `autobuild-2026-08-09-13-03` |
| Asset | `ffmpeg-n8.1.2-34-g9b6c8969e0-win64-lgpl-shared-8.1.zip` |
| SHA-256 | `2936e5449886641b4279ca3fc554b678c8e9a2d20dd0c0a34fe7208b254a0905` |
| FFmpeg | 8.1.2 — `libavformat` 62, `libavcodec` 62, `libavutil` 60 |
| Licence | LGPL v3 or later. No GPL components: built with `--disable-libx264 --disable-libx265` and no `--enable-gpl`. |
| Binding | `ffmpeg-the-third 5.0.0+ffmpeg-8.1`, pinned in `[workspace.dependencies]` |

The tag is a dated one on purpose. The builder also publishes a rolling `latest`
tag with stable file names, and its contents change daily, so no checksum can be
recorded against it and no two builds would be the same.

The builder publishes no checksums of its own. The SHA-256 above was computed
from the artefact reviewed when the pin was chosen; a dated tag is immutable, so
a mismatch means either a corrupted download or an artefact that is not the one
this pin was verified against.

## How it links

`scripts/fetch-ffmpeg.ps1` extracts a normal FFmpeg prefix — `bin`, `include`,
`lib` — and `FFMPEG_DIR` points at it. From there:

- **Linking.** `ffmpeg-sys-the-third` reads `FFMPEG_DIR`, generates bindings
  from `include`, and links against the import libraries in `lib`.
- **Running.** Windows resolves a DLL from the directory of the executable that
  needs it, so `crates/muxer/build.rs` copies the DLLs from `bin` into the Cargo
  target directory beside the binaries and the test executables. Nothing has to
  be on `PATH`, and no other FFmpeg on the machine can be picked up by accident.

`crates/muxer` owns the link; other crates reach FFmpeg through the single
`[workspace.dependencies]` entry rather than adding their own.

## Moving the pin

Three places, and they are meant to be changed in one commit.

1. **`scripts/fetch-ffmpeg.ps1`** — the `Tag`, `Asset` and `Sha256` parameter
   defaults. Compute the checksum from the artefact you actually downloaded:

   ```text
   Get-FileHash -Algorithm SHA256 .\ffmpeg-....zip
   ```

2. **`[workspace.dependencies]` in the root `Cargo.toml`** — only when the
   FFmpeg major version moves. The binding version carries the FFmpeg version it
   was built against in its metadata (`5.0.0+ffmpeg-8.1`); use the release that
   matches.

3. **`crates/muxer/tests/ffmpeg_linkage.rs`** — the expected build identifier
   and library versions. Run `bin/ffprobe.exe -version` from the newly fetched
   build to read them off, or run the test and take them from the failure.

Then:

```text
powershell -ExecutionPolicy Bypass -File scripts/fetch-ffmpeg.ps1
cargo test -p clipped-muxer
```

The licence assertions in that test are not a formality. They fail on a GPL
build even though it reports identical version numbers, which is exactly the
mistake worth catching: `gpl-shared` and `lgpl-shared` differ by one word in a
file name.

Older builds are left on disk when the pin moves — the fetch script names them
so you can delete them.

## Using it from CI

The script is non-interactive and safe to run unconditionally. Under GitHub
Actions it appends `FFMPEG_DIR` to `GITHUB_ENV`, so later steps inherit it
without any further wiring. Each pin extracts into its own directory named after
the asset, so caching `third-party/ffmpeg` keyed on the asset name works, and a
cache hit turns the step into a checksum-free no-op that touches no network.

Wiring this into the workflow belongs to
[issue #4](https://github.com/wildware-uk/clipped/issues/4).

## ffprobe is a different thing

`docs/prerequisites.md` also mentions `ffprobe`, and that is unrelated to any of
the above. It is a *test* tool: media tests assert on finished files with it
(AGENTS.md section 22), which has none of the constraints that made the library
link necessary. It is whatever FFmpeg happens to be on your `PATH`, it is not
pinned, and it is not linked into anything.
