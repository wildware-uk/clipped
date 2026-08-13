# Packaging: what the installer carries, and how it gets there

Clipped installs as two executables, not one. The window
(`clipped-desktop.exe`) is a client of a separate recorder process
(`clipped-recorder.exe`) that owns capture, encoding and the library
([ADR 0002](adr/0002-separate-recorder-process.md),
[ADR 0006](adr/0006-recorder-lifetime-and-supervision.md)). The window looks for
the recorder **beside its own executable** and starts it there; nothing searches
the `PATH`, because a recorder found on the `PATH` could be any build of any age.

So the installer has to put the recorder next to the window, and the FFmpeg
runtime libraries next to the recorder. This page is how that happens, why it is
arranged this way, and what a release still owes on top of it.

## What ships

```text
%LOCALAPPDATA%\Clipped\            (the default install location)
    clipped-desktop.exe            the window
    clipped-recorder.exe           the recorder, from target\release
    avcodec-62.dll                 ─┐
    avdevice-62.dll                 │
    avfilter-11.dll                 │  the pinned FFmpeg build's bin\*.dll,
    avformat-62.dll                 │  copied unmodified
    avutil-60.dll                   │  (docs/ffmpeg.md)
    swresample-6.dll                │
    swscale-9.dll                  ─┘
    uninstall.exe
```

**The recorder** is a release build, produced by
`cargo build --release -p clipped-recorder` in the repository's own workspace.
It is not built by `npm run build:app`; the installer build refuses if it is not
already there, rather than silently building a debug one or shipping without it.

**The FFmpeg libraries** are every `.dll` in `bin` of the build that
[`scripts/fetch-ffmpeg.ps1`](../scripts/fetch-ffmpeg.ps1) installs into
`third-party/ffmpeg/current`, copied unmodified. The recorder links FFmpeg
dynamically, and Windows resolves a DLL from the directory of the executable that
needs it, so without them the installed recorder does not start at all — it fails
at load time with `STATUS_DLL_NOT_FOUND` and no window to say so in.

Every DLL is shipped rather than the four the recorder imports directly, for the
reason [`crates/ffmpeg-runtime`](../crates/ffmpeg-runtime/src/lib.rs) gives about
the copies it makes into the target directory: `libavformat` loads its siblings,
and the set changes with the FFmpeg version, so a hand-written list would be a
second copy of the pin to keep in step and would fail on a user's machine rather
than in a build.

**`ffmpeg.exe`, `ffplay.exe` and `ffprobe.exe` are not shipped.** They sit in the
same `bin` directory and are test tools ([docs/ffmpeg.md](ffmpeg.md)): nothing in Clipped
shells out to them, and shipping a program nobody runs is a licence obligation
taken on for nothing.

**The licence texts and third-party notices are not shipped yet.** That is
[#123](https://github.com/wildware-uk/clipped/issues/123), and
[`scripts/collect-notices.ps1`](../scripts/collect-notices.ps1) already produces
the payload; [docs/licensing.md](licensing.md) says what a release owes and what
each part is discharged by. **An installer built today is not one that may be
distributed.**

## What it depends on from Windows, and what it carries

An installed Clipped needs **the Windows it already targets — build 19044 or
later ([docs/prerequisites.md](prerequisites.md)) — and nothing else
installed**. Every library either of its executables imports is either a
component of that operating system or a file in the directory beside it.

| | |
| --- | --- |
| **From Windows** | `kernel32`, `ntdll`, `user32`, `advapi32`, `ole32`, `combase`, `oleaut32`, `shell32`, `comctl32`, `gdi32`, `shlwapi`, `dwmapi`, `d3d11`, `dxgi`, `mfplat`, `mmdevapi`, and the `api-ms-win-crt-*` universal CRT forwarders, which resolve to `ucrtbase.dll`. All are Windows components, serviced by Windows Update. |
| **Carried in the install directory** | The seven FFmpeg libraries above, and the recorder itself. |
| **Not needed** | The **Microsoft Visual C++ 2015-2022 redistributable**. Neither executable imports `VCRUNTIME140.dll`; both link the compiler runtime statically and the universal CRT dynamically ([ADR 0007](adr/0007-visual-c-runtime-linkage.md)). |
| **Installed if absent** | **WebView2**, which the window draws the interface in. It is present on Windows 11 and on current Windows 10; `tauri.conf.json` leaves `webviewInstallMode` at Tauri's default, so the installer runs Microsoft's bootstrapper where it is not. |

That last row is the one that used to be false. `clipped-recorder.exe` imported
`VCRUNTIME140.dll` and `clipped-desktop.exe` did not, so on a machine without the
redistributable the window opened, looked healthy and recorded nothing: the
recorder was ended by the loader with `STATUS_DLL_NOT_FOUND` before `main`, with
no log file to say so ([#407](https://github.com/wildware-uk/clipped/issues/407)).
`apps/recorder/build.rs` is the fix and
`apps/recorder/tests/runtime_libraries.rs` reads the recorder's import table on
every CI run, so the table above fails a test rather than going quietly out of
date.

Both facts about the recorder are asserted there: that it imports nothing from
the redistributable, and that it *does* still import the universal CRT — which
is what keeps its heap the same heap the FFmpeg libraries allocate from. ADR
0007 says why the one-flag alternative, `-C target-feature=+crt-static`, was not
taken.

## How it gets there

Three pieces, each doing one thing.

| | |
| --- | --- |
| [`scripts/stage-installer-payload.ps1`](../scripts/stage-installer-payload.ps1) | Copies the recorder and the FFmpeg DLLs into `apps/desktop/src-tauri/installer-payload`, and refuses — naming the missing file, where it looked and the command that produces it — if either is absent. |
| `beforeBuildCommand` in `tauri.conf.json` | Runs that script before anything else a `tauri build` does, so the refusal reaches any installer build and not only `npm run build:app`. |
| `bundle.resources` in `tauri.conf.json` | Maps `installer-payload/` to `""`, the root of the resource directory — which on Windows is the directory the executable is installed into. |

The staging directory is gitignored and is created by the window crate's
`build.rs`.

### Why a staging directory, and not the real paths

`bundle.resources` could have named `../../../target/release/clipped-recorder.exe`
and `../../../third-party/ffmpeg/current/bin/*.dll` directly, and copied nothing.
It does not, because `tauri-build` copies every declared resource into the Cargo
target directory *from the window crate's build script*, which runs on every
`cargo build` of that crate — including the `cargo clippy --all-targets` and
`cargo test` that CI's Desktop UI job runs. In `ResourcePaths`, a declared path
that does not exist is an error and a glob that matches nothing is an error too,
so naming those paths directly would make `cargo test --manifest-path
apps/desktop/src-tauri/Cargo.toml` require a release recorder and a fetched
FFmpeg — neither of which that job has, wants, or should grow.

A directory that exists and is empty is the one shape `ResourcePaths` skips. So
an ordinary build of the window finds the staging directory empty and copies
nothing, and only an installer build fills it.

### Why not `externalBin`

Tauri's sidecar mechanism is the obvious candidate and was rejected for three
reasons, in increasing order of weight:

1. **It renames.** `externalBin` looks for
   `clipped-recorder-x86_64-pc-windows-msvc.exe` and installs it as
   `clipped-recorder.exe`. The triple has to be produced by something, so a copy
   step is needed anyway — the mechanism does not remove work, it adds a naming
   convention on top of it.
2. **It cannot carry the DLLs.** `externalBin` appends `.exe` on Windows and is
   for executables. The FFmpeg libraries would need `bundle.resources` regardless,
   so `externalBin` means two mechanisms delivering one payload into one
   directory.
3. **It has no empty state.** An `externalBin` path is not a glob and not a
   directory, so it must exist for *every* `cargo build` of the window crate, for
   the reason above. Adopting it would make CI's Desktop UI job — and any
   contributor running `cargo test` on the window — depend on a release build of
   the recorder.

A build step that copied into the bundle *after* `tauri build` was rejected
outright: an NSIS installer is a compiled archive, and there is nothing to copy
into once it exists.

## Building one

```powershell
cargo build --release -p clipped-recorder   # the recorder the installer carries
npm run build:app                           # the installer
```

The result is
`apps/desktop/src-tauri/target/release/bundle/nsis/Clipped_<version>_x64-setup.exe`.

Skip the first command and the second stops before it builds anything, saying so:

```text
The Clipped installer cannot be built.

  Missing: clipped-recorder.exe, the recording process the desktop application starts
  Looked in: ...\target\release\clipped-recorder.exe

  Run this first:

      cargo build --release -p clipped-recorder

  An installer without it installs a Clipped that reports the recorder missing and records nothing.
```

The same happens, naming `scripts/fetch-ffmpeg.ps1`, when the FFmpeg build is
not installed.

The installer is a per-user one: `installMode` is Tauri's default `currentUser`,
so it needs no administrator, installs under `%LOCALAPPDATA%`, and writes its
uninstall entry under `HKEY_CURRENT_USER`.

## What is not solved here

- **Licence texts and notices**, and the corresponding source offer:
  [#123](https://github.com/wildware-uk/clipped/issues/123).
- **Code signing, versioning and releases.** Nothing here is signed, so Windows
  SmartScreen will warn about it.
