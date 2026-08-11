# FFmpeg

Clipped uses FFmpeg as libraries, not as a command-line tool: containers,
remuxing, thumbnails and waveforms all go through `libavformat` and
`libavcodec` inside the recorder process. This page is how to build against it,
what is pinned, and what has to happen when the pin moves.

Why it is set up this way — and in particular why the build must not be a GPL
one — is [ADR 0004](adr/0004-ffmpeg-dependency-strategy.md).

## Setting up a clean clone

Two steps, once.

**1. Install LLVM.** The FFmpeg binding generates its bindings from FFmpeg's own
headers at build time, which needs `libclang.dll`.

```text
winget install LLVM.LLVM
```

**2. Fetch the pinned FFmpeg build.** A 67 MB download, 168 MB extracted into
`third-party/ffmpeg/current`, which is gitignored.

```text
powershell -ExecutionPolicy Bypass -File scripts/fetch-ffmpeg.ps1
```

Then build, in the same shell:

```text
cargo build --workspace
cargo test --workspace
```

There is no third step. The fetch script sets nothing in your environment,
because nothing it set could reach the shell that invoked it; the committed
`.cargo/config.toml` names the four variables instead, and Cargo reads it before
it runs anything under the repository.

Expect `target/debug` to grow by about 409 MB the first time you build: the
seven FFmpeg DLLs are 136 MB and `crates/muxer/build.rs` copies them beside the
binaries, the test executables and the examples. A release build costs the same
again.

`scripts/check-prerequisites.ps1` reports both of the steps above, so run it
first if anything is unclear about the state of the machine.

`cargo test -p clipped-muxer` is the one to run if you want to confirm the link
specifically: it loads the libraries and asserts that what loaded is the pinned
build.

### When it goes wrong

| Symptom | Cause and remedy |
| --- | --- |
| `!!!!!!! rusty_ffmpeg: No linking method set!` while building `rusty_ffmpeg` | `FFMPEG_LIBS_DIR` reached the build empty. Either `.cargo/config.toml` is missing from the checkout, or you are building from outside the repository — Cargo reads that file from the directory it runs in and its ancestors. |
| `FFmpeg include dir: ... doesn't exits` | The headers are not where the build was told to look. If the path is inside `third-party/ffmpeg/current`, the fetch script has not run; otherwise an `FFMPEG_INCLUDE_DIR` set in your shell is overriding the workspace. |
| `Unable to find libclang` | LLVM is not installed, or not on `PATH`. Install it, or set `LIBCLANG_PATH` to the directory containing `libclang.dll`. |
| The linker cannot find `avformat.lib`, or reports unresolved `av*` symbols | `FFMPEG_LIBS_DIR` points at a build that is not the shared one, or `FFMPEG_LINK_MODE` is not `dynamic`. Unset any FFmpeg variables in your shell and let `.cargo/config.toml` answer. |
| `The code execution cannot proceed because avformat-62.dll was not found` | The DLLs beside the binary were deleted or never copied. Build again: `crates/muxer/build.rs` declares each copy as an input, so a missing one makes the build script re-run and restore it. |
| A test fails saying it linked against a different FFmpeg to the pinned one | The FFmpeg variables point at some other FFmpeg — a system-wide install, or a build left over from an older pin. |

## What is pinned

| | |
| --- | --- |
| Build | [BtbN/FFmpeg-Builds](https://github.com/BtbN/FFmpeg-Builds) `autobuild-2026-08-09-13-03` |
| Asset | `ffmpeg-n8.1.2-34-g9b6c8969e0-win64-lgpl-shared-8.1.zip` |
| SHA-256 | `2936e5449886641b4279ca3fc554b678c8e9a2d20dd0c0a34fe7208b254a0905` |
| FFmpeg | 8.1.2 — `libavformat` 62, `libavcodec` 62, `libavutil` 60 |
| Licence | LGPL v3 or later. No GPL components: built with `--disable-libx264 --disable-libx265` and no `--enable-gpl`. |
| Binding | `rusty_ffmpeg 0.17.0+ffmpeg.8.1` (MIT), pinned in `[workspace.dependencies]` |

The tag is a dated one on purpose. The builder also publishes a rolling `latest`
tag with stable file names, and its contents change daily, so no checksum can be
recorded against it and no two builds would be the same.

The builder publishes no checksums of its own. The SHA-256 above was computed
from the artefact reviewed when the pin was chosen; a dated tag is immutable, so
a mismatch means either a corrupted download or an artefact that is not the one
this pin was verified against.

## How it links

`scripts/fetch-ffmpeg.ps1` extracts a normal FFmpeg prefix — `bin`, `include`,
`lib` — into `third-party/ffmpeg/current`, and the workspace's
`.cargo/config.toml` names four variables from that one path:

| Variable | Read by | Meaning |
| --- | --- | --- |
| `FFMPEG_INCLUDE_DIR` | `rusty_ffmpeg` | Headers `bindgen` generates the FFI from. |
| `FFMPEG_LIBS_DIR` | `rusty_ffmpeg` | Import libraries the linker resolves against. |
| `FFMPEG_LINK_MODE` | `rusty_ffmpeg` | `dynamic`. Not the default, and not optional: static linking of an LGPL build carries obligations Clipped does not intend to take on (ADR 0004). |
| `FFMPEG_DIR` | `crates/muxer/build.rs` | The prefix, so the DLLs in `bin` can be found. |

They live in that file rather than in your shell for a plain reason: a script
cannot export a variable into the shell that ran it. The alternative was to have
the fetch script write them to your user environment and to tell you to open a
new terminal — which works, but only for the next terminal, and not for the one
holding the instructions you are reading. Cargo's `[env]` table has none of that
problem, and it keeps the variables attached to the checkout rather than to the
machine.

The entries are not `force`d, so a variable set in your shell still wins. That
is the escape hatch for building against an FFmpeg you built yourself — and it
is also how `FFMPEG_LINK_MODE=static` gets set by accident, which is why
`scripts/check-prerequisites.ps1` reads the environment first, this file second,
and fails on anything but `dynamic`.

The build directory has a fixed name rather than the asset's, so moving the pin
does not touch `.cargo/config.toml` and old builds do not pile up at 168 MB
each. Which build is installed is recorded in
`third-party/ffmpeg/current/.clipped-ffmpeg-pin.json`, and the prerequisite
check reads it back.

From there:

- **Linking.** `rusty_ffmpeg` generates bindings from `include` and links
  dynamically against the import libraries in `lib`. It links all seven FFmpeg
  libraries; there is no way to narrow that.
- **Running.** Windows resolves a DLL from the directory of the executable that
  needs it, so `crates/muxer/build.rs` copies the DLLs from `bin` into the Cargo
  target directory beside the binaries, the test executables and the examples.
  Nothing has to be on `PATH`, and no other FFmpeg on the machine can be picked
  up by accident. Each copied file is declared as a build-script input, so
  deleting one is repaired by the next build rather than becoming a missing-DLL
  dialogue.

`crates/muxer` owns the link. `rusty_ffmpeg` is a `-sys` crate with no safe API,
so the safe wrappers over FFmpeg live in `crates/muxer` too, and other crates
reach FFmpeg through them rather than adding their own dependency on the
binding.

## Moving the pin

Three places, and they are meant to be changed in one commit.

1. **`scripts/fetch-ffmpeg.ps1`** — the `Tag`, `Asset` and `Sha256` parameter
   defaults. Compute the checksum from the artefact you actually downloaded:

   ```text
   Get-FileHash -Algorithm SHA256 .\ffmpeg-....zip
   ```

2. **`[workspace.dependencies]` in the root `Cargo.toml`** — only when the
   FFmpeg major version moves. The binding version carries the FFmpeg version it
   was built against in its metadata (`0.17.0+ffmpeg.8.1`), and its `ffmpeg8_1`
   feature names the same thing; use the release and the feature that match.

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

`.cargo/config.toml` is deliberately not on that list: it names
`third-party/ffmpeg/current`, which is where every pin is installed. The
previous build is replaced rather than left beside the new one, so nothing
accumulates and nothing has to be deleted by hand.

## Using it from CI

The script is non-interactive and safe to run unconditionally, and it needs no
wiring into the job's environment: the workflow checks the repository out, so it
has `.cargo/config.toml` for the same reason a contributor does.
`.github/workflows/ci.yml` caches `third-party/ffmpeg` keyed on the pinned asset
name read out of the fetch script, so moving the pin misses the cache and a hit
turns the step into a no-op that touches no network.

The runner also needs `libclang.dll`. GitHub's `windows-latest` image ships LLVM,
and the workflow's "Locate libclang for bindgen" step finds it and exports
`LIBCLANG_PATH`.

## ffprobe is a different thing

`docs/prerequisites.md` also mentions `ffprobe`, and that is unrelated to any of
the above. It is a *test* tool: media tests assert on finished files with it
(AGENTS.md section 22), which has none of the constraints that made the library
link necessary. It is not linked into anything, and nothing in the recorder
shells out to it.

The media harness (`tests/media`, [docs/testing.md](testing.md)) is what runs
it, on behalf of every crate that writes a file, and it prefers the
`ffprobe.exe` that ships inside the pinned build in
`third-party/ffmpeg/current/bin` over whichever one is on `PATH`. The fetch
script installs it on every machine that builds this workspace, CI included, so
those tests run everywhere the crate compiles instead of passing, failing or
quietly skipping depending on what somebody happened to install. An `ffprobe` on
your `PATH` is still useful for looking at a file by hand, and
`docs/prerequisites.md` still suggests having one.

The same is true of `ffmpeg.exe`, which the harness uses for one thing:
decoding an audio track to samples, so that a test can assert a track carries
its own tone and none of another source's. It is a test tool on exactly the same
terms — nothing in the recorder shells out to it, and nothing ever should.
