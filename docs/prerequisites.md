# Development prerequisites

Everything needed to build and test Clipped on Windows, how to tell whether you
already have it, and what to install if you do not.

Run the check first — it answers all of the below for your machine in a couple
of seconds:

```text
powershell -ExecutionPolicy Bypass -File scripts/check-prerequisites.ps1
```

The `-ExecutionPolicy Bypass` prefix is not decoration: run the script the
natural way instead and Windows refuses it outright. [Running the
scripts](#running-the-scripts) below explains why, and gives two ways out.

It prints one line per prerequisite and exits non-zero listing exactly what is
missing and what to do about it. The point is that you find out the Windows SDK
is absent from this script, not from a linker error several minutes into a
build.

Most of what it reports is something to install. One is not: FFmpeg is fetched
into the repository by `scripts/fetch-ffmpeg.ps1`, and the four environment
variables it needs come from the committed `.cargo/config.toml` rather than from
your shell. [docs/ffmpeg.md](ffmpeg.md) is the page for that, and the check
names it.

Once the check passes:

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace
cargo test --workspace
```

## Running the scripts

Every PowerShell command on this page carries the same prefix, because a stock
Windows install refuses to run any `.ps1` file at all. Do the natural thing:

```text
.\scripts\check-prerequisites.ps1
```

and the shell answers:

```text
.\scripts\check-prerequisites.ps1 : File ...\scripts\check-prerequisites.ps1
cannot be loaded because running scripts is disabled on this system.
```

That is the execution policy, and our scripts are not singled out: Windows
client editions default to `Restricted`, which runs no script of any kind.
`powershell -ExecutionPolicy Bypass -File ...` sets the policy for that one
process, for the lifetime of that one command. It changes nothing on the
machine and needs no administrator rights, which is why every command on this
page is written that way, and why CI invokes the scripts in exactly the same
form.

If typing the prefix each time grates, set the policy once for your own account
instead. This one does change machine state — your user's setting, which
persists — but it still needs no administrator rights:

```text
Set-ExecutionPolicy -ExecutionPolicy RemoteSigned -Scope CurrentUser
```

`RemoteSigned` runs local scripts and demands a signature only from files marked
as having come from the internet. `git clone` marks nothing, so a cloned
checkout's scripts run and `.\scripts\check-prerequisites.ps1` works as typed.
It is what Microsoft recommends for developer machines, and it is strictly
narrower than `Unrestricted`.

Two things to know about that route:

- **Downloading the repository as a zip does mark the files**, and
  `RemoteSigned` then blocks them — with a different message, complaining that
  the file is not digitally signed rather than that scripts are disabled. Clear
  the mark:

  ```text
  Get-ChildItem -Recurse scripts\*.ps1 | Unblock-File
  ```

- **A managed machine may have the policy set by Group Policy**, which
  `-Scope CurrentUser` cannot override, so the command above has no effect
  there. `Get-ExecutionPolicy -List` shows all five scopes; if `MachinePolicy`
  or `UserPolicy` is anything other than `Undefined`, that is what is in force
  and the per-process prefix is the only route open to you.

  ```text
  Get-ExecutionPolicy -List
  ```

## Summary

| Prerequisite | Required version | Enforced by |
| --- | --- | --- |
| Windows | 10 21H2 (build 19044) or later, or Windows 11 | check script |
| Visual Studio Build Tools | 2022, "Desktop development with C++" | check script |
| Windows SDK | any Windows 10/11 SDK | check script |
| Rust | exactly the channel in `rust-toolchain.toml` | rustup, check script |
| LLVM (`libclang`) | any recent release | check script |
| FFmpeg libraries | the pin in `scripts/fetch-ffmpeg.ps1` | check script; see [FFmpeg libraries](#ffmpeg-libraries) |
| Node | major version from `.nvmrc` | check script; see [Node](#node) |
| WebView2 runtime | any; ships with Windows 11 | see [WebView2](#webview2) |
| GPU driver | vendor-current | reported, not enforced |
| `ffprobe` (test tool) | any recent build | check script, warning only |

Required prerequisites fail the check. Ones that only matter for parts of the
project that do not exist yet, or that only affect recording quality, produce a
warning and leave the exit code alone.

## Windows

`SPEC.md` section 3 targets **Windows 11 / modern Windows 10**. Concretely that
means **build 19044 (Windows 10 21H2) or later**, because 21H2 is the oldest
Windows 10 release Microsoft still services. Windows 11 is the primary target;
Windows 10 is supported but gets less testing.

No API-level floor has been established yet. `crates/capture` is a
documentation-only placeholder, so there is no capture backend to name a
Windows Graphics Capture entry point against and no ADR recording one. If the
backend built in M1 needs something newer than 19044, the number moves then and
the specific API that forced it is recorded with the change.

Check:

```text
winver
```

The dialogue shows the edition and the build number. The check script reports
the same number and refuses anything older.

Fix: Settings > Windows Update. If the machine cannot reach 19044, it cannot run
Clipped.

## Visual Studio Build Tools

Rust on Windows compiles against the MSVC toolchain, so the C++ linker and the
platform libraries have to be present even though Clipped contains no C++ of its
own. Without them `cargo build` fails at link time with `link.exe not found`.

Check:

```text
& "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe" -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -latest
```

An empty result means no installation carries the C++ toolset — including the
common case of Visual Studio being installed without the C++ workload.

Fix: install [Visual Studio 2022 Build
Tools](https://visualstudio.microsoft.com/downloads/) (under "Tools for Visual
Studio") and tick **Desktop development with C++**. A full Visual Studio
installation with that workload works equally well; Build Tools is simply the
smaller download.

## Windows SDK

Provides the headers and import libraries behind the Windows APIs the recorder
uses — Windows Graphics Capture, WASAPI, DXGI — reached from Rust through the
`windows` crates. The "Desktop development with C++" workload includes it, so
installing it separately is usually unnecessary.

Check:

```text
Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\Include"
```

Directories named like `10.0.26100.0` are installed SDK versions. Clipped does
not require a specific one.

Fix: add the "Windows 11 SDK" component in the Visual Studio Installer, or
install the [standalone
SDK](https://developer.microsoft.com/windows/downloads/windows-sdk/).

## Rust

The toolchain is pinned exactly by `rust-toolchain.toml`, including the `rustfmt`
and `clippy` components and the `x86_64-pc-windows-msvc` target. The file
explains why the pin is exact rather than `stable`. Install rustup and it applies
the pin automatically inside this repository — there is nothing to configure and
no reason to switch toolchains by hand.

Note that `rust-version` in `Cargo.toml` is a different, lower number on purpose:
that is the minimum Rust the crates promise to compile on, while
`rust-toolchain.toml` is what development and CI actually use.

Check:

```text
rustup show active-toolchain
cargo fmt --version
cargo clippy --version
```

Run from the repository root. `rustup show active-toolchain` reports the
toolchain a `cargo` command typed there would actually use, and names the reason
in brackets — which for a healthy checkout is `rust-toolchain.toml`.

Being installed and being in effect are separate things, and the check script
tests both. Two things override `rust-toolchain.toml`, and neither is visible in
the file:

- a directory override left behind by `rustup override set`;
- the `RUSTUP_TOOLCHAIN` environment variable.

Either one puts you on a toolchain the repository never asked for, and the only
symptom is lint or format output nobody else can reproduce. Clear them with
`rustup override unset` in the repository root and by removing
`RUSTUP_TOOLCHAIN` from the environment.

Fix: install [rustup](https://rustup.rs), then from the repository root:

```text
rustup toolchain install
rustup component add rustfmt clippy
```

The first command reads `rust-toolchain.toml` and fetches exactly what it names.
A toolchain installed before the pin existed may be missing the components,
which is what the second command covers.

## LLVM

Clipped contains no C or C++ of its own, but `crates/muxer` links against the
FFmpeg C libraries, and the Rust binding to them generates its FFI from FFmpeg's
own headers while the workspace builds. That is `bindgen`, which loads
`libclang.dll` at run time — so LLVM is a build requirement. Without it,
`cargo build --workspace` fails inside the `rusty_ffmpeg` build script with
`Unable to find libclang`, a message that mentions neither FFmpeg nor LLVM.

Check:

```text
clang --version
```

Any recent release will do; no version is pinned. LLVM's Windows installer puts
`libclang.dll` beside `clang.exe`, which is where the binding looks first.

Fix:

```text
winget install LLVM.LLVM
```

If `libclang.dll` lives somewhere the build cannot find — an LLVM distributed
inside another toolchain, for instance — point `LIBCLANG_PATH` at the directory
containing it rather than moving the file. The check script reads the same
variable, so it agrees with the build about where libclang came from.

## FFmpeg libraries

This is the one prerequisite that is fetched rather than installed. Clipped
links dynamically against a prebuilt, LGPL-only FFmpeg pinned to one immutable
release asset and verified by checksum; the decision is
[ADR 0004](adr/0004-ffmpeg-dependency-strategy.md) and the contributor page is
[docs/ffmpeg.md](ffmpeg.md).

Fix, from the repository root:

```text
powershell -ExecutionPolicy Bypass -File scripts/fetch-ffmpeg.ps1
```

That downloads 67 MB into the gitignored `third-party/ffmpeg/current`, verifies
its SHA-256 and extracts it — 168 MB on disk, and a further 409 MB in
`target/debug` once the DLLs have been copied beside the binaries.

Nothing has to be set afterwards, and no new shell is needed. The four variables
the build reads are in the workspace's `.cargo/config.toml`, which Cargo reads
before it runs anything under the repository; the fetch script only fetches,
because a script cannot set a variable in the shell that ran it.

Check:

```text
powershell -ExecutionPolicy Bypass -File scripts/check-prerequisites.ps1
```

`FFMPEG_LINK_MODE` must be `dynamic`. It is not the binding's default, and it is
not a performance preference: dynamic linking is how Clipped satisfies the
LGPL's relinking requirement, so a machine set to `static` would produce
binaries the project cannot distribute. The check script fails on it for that
reason, and it reads your environment before the configuration file — because
so does Cargo, and a variable you set yourself is what would decide the build.

Re-running the fetch script is cheap and safe. Over an intact installation it
verifies the recorded pin, touches no network and says so.

## Node

`.nvmrc` pins the Node version. Node is not needed to build or test the Rust
workspace — the recorder has no JavaScript in it — but it is required to build
the desktop application, and `apps/desktop/package.json` now exists, so the
check script treats a missing Node as a failure rather than a warning.

The same pin is read in two more places, so that nobody has to remember it:

- `engines.node` in the root `package.json`, which `npm install` checks.
- `actions/setup-node` with `node-version-file: .nvmrc` in CI, so the workflow
  and contributors read the pin from the same file.

One dependency is held back by this pin: `jsdom`, which the component tests run
in, requires Node 22.22.2 or newer from version 30 onwards, so the workspace
stays on `jsdom` 29. Moving the pin forward and moving `jsdom` with it is one
change, not two.

Check:

```text
node --version
```

A different major version to `.nvmrc` is a problem; a slightly older patch on the
right major is not.

Fix: install the pinned version from [nodejs.org](https://nodejs.org), or use a
version manager that understands `.nvmrc` and run `nvm install` (nvm-windows) or
`fnm use` in the repository root.

## WebView2

The desktop application renders its interface in Microsoft's WebView2, which is
the Edge rendering engine hosted in an ordinary window. Tauri links against its
loader; there is no bundled browser and no Electron-sized runtime in the
installer.

Windows 11 ships WebView2 as part of the operating system, and it is present on
most Windows 10 installations through Edge. Nothing needs installing on a
machine that has it, and there is no version to pin: it is evergreen, updated by
Windows rather than by Clipped.

Check: the "Microsoft Edge WebView2 Runtime" entry in Installed apps, or

```text
Get-ItemProperty 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}' -ErrorAction SilentlyContinue | Select-Object pv
```

Fix: install the Evergreen Runtime from
[Microsoft](https://developer.microsoft.com/microsoft-edge/webview2/). The
installer Clipped produces does this for the user automatically when it is
missing — `webviewInstallMode` in `tauri.conf.json` — which is a download from
Microsoft at install time and the one piece of network access the installation
performs.

The check script does not test for WebView2. It is present by default on every
supported Windows version, and a missing one shows up immediately: the window
does not open and the application says why.

## GPU and drivers

Clipped prefers hardware encoding, which the vendor exposes through the graphics
driver:

| Vendor | Encoder | AV1 support |
| --- | --- | --- |
| NVIDIA | NVENC | Ada (RTX 40 series) and later |
| AMD | AMF | RDNA 3 (RX 7000 series) and later |
| Intel | Quick Sync | Arc, and Xe-based integrated graphics |

H.264 and HEVC are available much further back on all three. Where no hardware
encoder is usable, recording falls back to software encoding, which costs game
performance — it works, but it is not the configuration Clipped is designed
around.

No minimum driver version is enforced. Which codecs and features an encoder
actually supports is a question only the encoder can answer, so the recorder
queries it at runtime rather than inferring capability from an adapter name. What
the check script does is report each adapter, its driver version and its driver
date, and warn when a driver is more than two years old — an old driver is the
first thing to rule out when hardware encoding misbehaves.

Check:

```text
Get-CimInstance Win32_VideoController | Select-Object Name, DriverVersion, DriverDate
```

Fix: install the current driver from
[NVIDIA](https://www.nvidia.com/download/index.aspx),
[AMD](https://www.amd.com/support) or
[Intel](https://www.intel.com/content/www/us/en/download-center/home.html).
Prefer the vendor's own package over the one Windows Update offers, which can lag
behind.

## ffprobe

`ffprobe` is used by tests to inspect generated recordings — that a container
opens, that the expected streams exist, that timestamps look sane (AGENTS.md
section 22). It is not needed to build, so the check script only warns.

It has nothing to do with [FFmpeg libraries](#ffmpeg-libraries) above. This one
is whatever FFmpeg is on your `PATH`, used as a command-line tool by tests; the
other is a pinned build linked into the recorder.

Check:

```text
ffprobe -version
```

Fix:

```text
winget install Gyan.FFmpeg
```

or unpack a build from [ffmpeg.org](https://ffmpeg.org/download.html) and put its
`bin` directory on `PATH`.

## The check script

`scripts/check-prerequisites.ps1` takes every external thing it probes as a
parameter, so its outcome can be steered without touching the machine:

```text
powershell -ExecutionPolicy Bypass -File scripts/check-prerequisites.ps1 `
    -RustupCommand clipped-no-such-rustup `
    -VsWherePath C:\clipped-no-such-directory\vswhere.exe
```

`scripts/test-check-prerequisites.ps1` is the test for it. It runs the real
script as a child process and asserts the exit code and the reported text a
contributor would read. Every case is driven by fixtures — stand-in commands, a
registry key under `HKCU`, a stand-in LLVM and FFmpeg tree with the Cargo
configuration that points at it, a JSON description of the display adapters — so
no case can pass or fail because of what happens to be installed on the machine
running it. The environment variables the FFmpeg and libclang checks read are
cleared for the duration of the suite for the same reason.

Two shapes of wrongness are covered, because they are detected by different code
and only one of them is easy:

- **absent** — the probe points at a command, path or registry key that
  genuinely does not exist;
- **present but wrong** — the probe points at a stand-in that answers the way
  the real tool answers in that state: `vswhere` printing `[]` for a Visual
  Studio install with no C++ workload, `node` reporting a different major
  version, `rustup` refusing a toolchain that is not installed, `rustup`
  reporting a toolchain that overrides the pin.

```text
powershell -ExecutionPolicy Bypass -File scripts/test-check-prerequisites.ps1
```

Both scripts need only Windows PowerShell 5.1, which ships with Windows. Run
them after changing either the pins or the checks.

## Optional tooling

Not prerequisites, but worth having:

- **Git** — required to clone the repository, and assumed by the contribution
  workflow.
- **Windows Terminal** — the check script colours its output, which the legacy
  console renders less clearly.
