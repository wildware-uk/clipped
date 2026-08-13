# 0007. The recorder links the Visual C++ runtime statically and the universal CRT dynamically

- Status: Accepted
- Date: 2026-08-13
- Issue: [#407](https://github.com/wildware-uk/clipped/issues/407)

## Context

[Issue #226](https://github.com/wildware-uk/clipped/issues/226) put
`clipped-recorder.exe` and the FFmpeg libraries into the installer, and that
exposed a dependency nobody had had to think about while the only recorder
anybody ran was one they had built themselves.

`dumpbin /DEPENDENTS` on a release build, measured on the commit this decision
was made against:

| Binary | Imports `VCRUNTIME140.dll` |
| --- | --- |
| `clipped-recorder.exe` | **yes** |
| `clipped-desktop.exe` | no |
| `avcodec-62.dll`, `avformat-62.dll`, `avutil-60.dll`, `swscale-9.dll` | no |

`VCRUNTIME140.dll` belongs to the Microsoft Visual C++ 2015-2022 redistributable.
It is not part of Windows. A clean Windows install with no Visual Studio, no
games and no other native application does not have it, and on such a machine
**the window starts and the recorder does not**: the loader ends the recorder
with `STATUS_DLL_NOT_FOUND` before its first instruction, so nothing it might
have logged is logged, and the supervisor sees a process that started and was
gone by the next poll (`crates/ipc/src/supervisor.rs`).

Four things shape the answer.

**The failure is invisible on every machine that could reproduce it.** Anyone who
can build Clipped has the redistributable, because the build tools install it.
So "it works here" is evidence of nothing, and whatever is chosen has to be
checkable without a clean machine — from the binary rather than from running it.

**The asymmetry is not something the project decided.** Every Rust program built
for `x86_64-pc-windows-msvc` imports `VCRUNTIME140.dll`; a `fn main` that prints
one line does. `clipped-desktop.exe` is the exception, and it is the exception
because `tauri-build` emits eleven `/NODEFAULTLIB` and `/DEFAULTLIB` link
arguments of its own, visible in
`apps/desktop/src-tauri/target/release/build/clipped-desktop-*/output`. Nothing
in either manifest states the difference, and nothing would have reported it if
the window had lost it in a Tauri upgrade.

**The recorder shares a heap with FFmpeg, and does so today by construction
rather than by discipline.** `avutil-60.dll` imports `malloc`, `free`, `calloc`
and `realloc` from `api-ms-win-crt-heap-l1-1-0.dll`; so does
`clipped-recorder.exe`. Both therefore resolve to one `ucrtbase.dll` and one
heap. Nothing in the codebase relies on this — see the audit under "Static
linking of the whole C runtime" below — but nothing enforces it either, and it is
the safety net under the entire FFmpeg FFI surface
([ADR 0004](0004-ffmpeg-dependency-strategy.md)).

**Clipped is MPL-2.0 and its installer will be redistributed by people who are
not this project.** Whatever the installer carries, a fork carries; whatever
terms attach to it attach to them. That makes "ship Microsoft's redistributable"
a licensing decision about the project rather than a packaging convenience, in
exactly the way ADR 0004 found for FFmpeg.

Not in scope: what the window links, which `tauri-build` already decides and
which this record only observes; code signing and releases
([#123](https://github.com/wildware-uk/clipped/issues/123)); and the FFmpeg
libraries themselves, which the installer already carries (ADR 0004,
`docs/packaging.md`).

## Decision

`apps/recorder/build.rs` links the recorder with a **static Visual C++ runtime
and a dynamic universal CRT** — the arrangement `tauri-build` already uses for
the window. An installed Clipped then imports nothing outside Windows except the
FFmpeg libraries it carries itself, and there is no redistributable to ship, to
run, or to keep current.

Microsoft's C runtime is three libraries, and they are chosen separately:

| Part | Static | Dynamic | Chosen |
| --- | --- | --- | --- |
| Startup and CRT glue | `libcmt.lib` | `msvcrt.lib` | static |
| Compiler runtime — exception handling, `memcpy` | `libvcruntime.lib` | `VCRUNTIME140.dll` | **static** |
| Universal CRT — `malloc`, `free`, `printf` | `libucrt.lib` | `api-ms-win-crt-*.dll` | **dynamic** |

Only the middle row is the redistributable. The universal CRT has been a
component of Windows since Windows 10 and is serviced by Windows Update, so
importing it costs nothing and shipping a copy of it would be shipping a copy of
the operating system.

Concretely:

- The build script emits eleven link arguments: eight `/NODEFAULTLIB` refusals
  and three `/DEFAULTLIB` choices. The refusals are needed because every `.rlib`
  in the standard library carries a `/DEFAULTLIB:msvcrt.lib` directive of its
  own and the linker takes the union of what it is given; naming `libcmt.lib`
  without refusing `msvcrt.lib` links both.
- They are emitted as `cargo:rustc-link-arg`, which reaches **every target of
  the package** — the binary, the examples and the integration tests — rather
  than `cargo:rustc-link-arg-bins`. A supervision fixture linked differently
  from the recorder it stands in for is a test proving something else.
- Only on `msvc`. The `*-pc-windows-gnu` targets link a different runtime
  entirely and none of these arguments mean anything to their linker.
- **`apps/recorder/tests/runtime_libraries.rs` asserts both halves**, by reading
  the PE import directory of `clipped-recorder.exe`: that no library from the
  redistributable is imported, and that `api-ms-win-crt-heap-l1-1-0.dll` still
  is. The second is what stops the decision being quietly undone, because
  `-C target-feature=+crt-static` satisfies the first and is what a contributor
  reaching for the obvious fix would write. This is the same device ADR 0004
  uses for the licence position: an assertion, so that the claim in this file
  stays true rather than remaining a claim.
- The recorder gains a `build.rs` for this and nothing else.

The symptom is fixed separately and regardless of this choice, because the
loader can still refuse a recorder for other reasons — a half-replaced install,
FFmpeg libraries from a different pin, a build somebody else made.
`SupervisorError::NotLoadable` recognises `STATUS_DLL_NOT_FOUND` and
`STATUS_ENTRYPOINT_NOT_FOUND` in a recorder's exit status, says that Windows
would not load it and that it therefore logged nothing, and is **not retried** —
a library that is not on the machine does not arrive because the recorder was
started a second time.

## Alternatives

### Ship the redistributable and run it from the installer

Carry `vc_redist.x64.exe` in the NSIS installer and run it during setup, through
Tauri's NSIS hooks. It is what Microsoft recommends, it fixes the machine rather
than one application, and it is the arrangement every user's other software
already relies on.

It was rejected on what it does to everyone who redistributes Clipped, which
under MPL-2.0 is anybody.

Microsoft's terms are specific, and they are conditions on distribution rather
than advice. From the Visual Studio 2022 license terms, *Distributable Code —
Distribution Requirements*: for any Distributable Code you distribute, you must
"add significant primary functionality to it in your programs"; "distribute
Distributable Code included in a setup program only as part of that setup
program without modification"; "**require distributors and external end users to
agree to terms that protect it at least as much as this agreement**"; "display
your valid copyright notice on your programs"; and "**indemnify, defend, and
hold harmless Microsoft from any claims, including attorneys' fees, related to
the distribution or use of your programs**". Microsoft's own documentation adds
that "distribution of the Visual C++ Runtime Redistributable package, merge
modules, and individual binaries is **limited to licensed Visual Studio
users**".

Clipped can satisfy all of that for its own releases. What it cannot do is
satisfy it on behalf of a fork. Putting the redistributable in the installer
means every person who takes this repository, builds an installer and passes it
on has to hold a Visual Studio licence, has to bind *their* distributors and end
users to Microsoft's terms, and has taken on an uncapped indemnity to Microsoft
— none of which MPL-2.0 asks of them and none of which this project can waive.
Trading that away to avoid eleven link arguments is not a trade worth making.

It has smaller costs too, which would not have decided it on their own: about
25 MB of installer for a component most machines already have; a second thing to
keep current; a version check against
`HKEY_LOCAL_MACHINE\SOFTWARE\...\VC\Runtimes\x64`, because the package fails
rather than no-ops when a newer one is installed; and an installer that is no
longer per-user, since the redistributable needs an administrator and
`installMode` is deliberately `currentUser` (`docs/packaging.md`).

What would make it win later: a dependency that genuinely needs the dynamic
runtime — a third-party static library built against `/MD` that will not link
against `libcmt.lib`. Then the redistributable becomes the only option and the
licensing has to be worked through properly rather than avoided.

### Ship the runtime DLLs app-local

Copy `vcruntime140.dll` and `vcruntime140_1.dll` into the install directory
beside the binaries, the way the FFmpeg libraries already are.

**It is permitted.** The individual binaries are on the REDIST list, so the
grant covers them, and app-local is a deployment location Microsoft documents
rather than one it forbids: "It's also possible to directly install the
Redistributable DLLs in the *application local folder*. The application local
folder is the folder that contains your executable application file." Every
requirement quoted above applies unchanged, so this alternative inherits the
whole of the previous one's objection — the licensed-user restriction, the
flow-down obligation and the indemnity all still bind a fork.

It is also the option Microsoft explicitly steers away from, in the same
sentence: "**For servicing reasons, we don't recommend that you use this
installation location.**" The reasoning is given for merge modules and applies
identically here — central deployment "makes it possible for Microsoft to
service runtime library files independently", whereas otherwise "an update to
the runtime library files requires you to update and redeploy your installer.
**Your app could be vulnerable to bugs or security issues until you do.**" A
copy of Microsoft's runtime beside `clipped-recorder.exe` is a copy Windows
Update cannot reach, and Clipped becomes responsible for shipping Microsoft's
security fixes at Clipped's release cadence.

So it is worse than shipping the redistributable — same licence obligations,
plus a servicing obligation — for the sole benefit of not needing an
administrator. Rejected.

### Static linking of the whole C runtime (`-C target-feature=+crt-static`)

One flag, nothing to ship, no licence question at all: the static CRT is linked
into the program like any other library and the redistributable never enters the
picture. It is the obvious answer, and it deserved the closest look, because it
is what a contributor will propose the next time this comes up.

It also gets the recorder a **second heap**, and that is the reason it lost.

`+crt-static` links `libucrt.lib` as well as `libvcruntime.lib`, so
`clipped-recorder.exe` imports no CRT library at all — measured: a recorder built
with the flag and without `build.rs` imports `KERNEL32`, `ntdll`, the COM and
Direct3D libraries and the four FFmpeg DLLs, and not one `api-ms-win-crt-*`. Its
`malloc` and `free` are then its own, statically linked, while `avutil-60.dll`
continues to import `malloc` and `free` from `api-ms-win-crt-heap-l1-1-0.dll`.
Memory allocated by one and released by the other is corruption — usually
silent, and reported from somewhere unrelated.

**Does anything cross that boundary today? No.** Every FFmpeg function the
workspace calls was enumerated and each allocation was traced to the call that
releases it:

- Every FFmpeg object is released by its own FFmpeg function:
  `av_frame_alloc`/`av_frame_free`, `av_packet_alloc`/`av_packet_free`,
  `avcodec_alloc_context3`/`avcodec_free_context`,
  `avformat_alloc_output_context2`/`avformat_free_context`,
  `avformat_open_input`/`avformat_close_input`, `avio_open`/`avio_closep`,
  `sws_getContext`/`sws_freeContext`, `av_dict_set`/`av_dict_free`. Allocation
  and release are both inside FFmpeg.
- The two places a buffer changes owner already use FFmpeg's allocator and say
  why. `crates/muxer/src/writer.rs` fills a codec's `extradata` with
  `av_malloc` — "FFmpeg frees `extradata` with the stream, so the copy has to be
  made with FFmpeg's allocator and handed over rather than borrowed from the
  layout" — and `crates/muxer/src/av.rs` sets `AVFormatContext::url` with
  `av_strdup`.
- Nothing goes the other way. There is no `into_raw`, no `Vec::from_raw_parts`,
  no `CString::from_raw` and no `libc::free` anywhere in the workspace; every
  read of FFmpeg memory is a `slice::from_raw_parts(...).to_vec()` copy.
  `avio_alloc_context`, `av_buffer_create` and `av_packet_from_data` — the three
  calls that would hand FFmpeg a buffer to free — are not used, and no callback
  is registered into FFmpeg at all.
- Files never cross either: `avio_open` opens the output inside FFmpeg, so no
  descriptor or `FILE*` is shared.

So the case against `+crt-static` is not "it would corrupt memory today". It is
that it converts a property currently guaranteed by the linker into a rule that
has to be remembered, in the one crate whose entire job is the FFmpeg FFI, where
the compiler cannot check it, no test can detect a violation, and the symptom of
getting it wrong is a crash somewhere else entirely. The three calls that would
break it are ordinary things for a muxer to reach for — `avio_alloc_context` is
how output is written anywhere other than to a path, which is a plausible future
for a replay buffer. Accepting that liability would be a reasonable price if the
alternative were shipping 25 MB and a licence; it is not a reasonable price for
saving ten link arguments over an arrangement that keeps the heap shared and has
no other cost.

The hybrid arrangement also happens to override the flag: with `build.rs` in
place, `RUSTFLAGS="-C target-feature=+crt-static"` still produces a recorder that
imports `api-ms-win-crt-heap-l1-1-0.dll`, because `/NODEFAULTLIB:libucrt.lib`
and `/DEFAULTLIB:ucrt.lib` are the linker's last word. That is deliberate — the
decision is in one file rather than in whatever flags a build happens to carry —
and `tests/runtime_libraries.rs` fails only when the build script itself is
changed, which is the mutation worth catching.

What would make it win later: the recorder no longer linking FFmpeg, or the
FFmpeg pin moving to a build with a statically linked CRT of its own. Neither is
in prospect.

### Leave it, and document the prerequisite

Say in the release notes that Clipped needs the Visual C++ redistributable, and
let the user install it. It costs nothing to build and it is honest.

Rejected because of *how* it fails. A user who has not read the release notes
sees a window that opens, looks entirely healthy, and never records anything;
the failure happens before `main`, so the log directory the message would point
them at is empty. AGENTS.md section 15 forbids silent failure and section 45
asks that a message leave the user an action. A prerequisite that most users
have already, that fails invisibly for the rest, and that is one build script
away from not existing, is not a prerequisite worth documenting.

The message half of it was worth keeping regardless, and is: see
`SupervisorError::NotLoadable` above.

## Consequences

- **An installed Clipped depends on Windows and on nothing else it does not
  carry.** `docs/packaging.md` states this, and
  `apps/recorder/tests/runtime_libraries.rs` is what keeps it true. The
  acceptance criterion in #407 is a clean machine, which cannot be tested here;
  what is tested is the loader's question — which libraries the executable
  imports — and every remaining import is either a Windows component or an
  FFmpeg library in the same directory.
- **A larger binary, and a copy of the compiler runtime per executable.**
  Measured: a release `clipped-recorder.exe` grows from 7,085,056 to 7,106,048
  bytes — 21 KB, three tenths of one per cent. The same two arrangements
  measured on a program with nothing else in it, where the runtime is all there
  is to see: 132,608 bytes ordinarily, 154,112 with this decision, 229,888 with
  `+crt-static`. So the flag would have cost roughly four times as much, and
  both figures are negligible beside the 136 MB of FFmpeg the installer already
  carries (ADR 0004). The recorder and the window each carry their own copy,
  which is the price of not having a shared one.
- **Security fixes to the compiler runtime now require a Clipped release.**
  This is the same servicing objection raised against app-local deployment
  above, and it is only *smaller* here rather than absent: the statically linked
  half is the exception handling and the `mem*` intrinsics, which is a much
  smaller and much less exposed surface than the whole CRT, and the parts that
  parse input — `printf`, the locale machinery, the heap — stay in
  `ucrtbase.dll` and stay serviced by Windows Update. It is still a real cost,
  and the honest statement of it is that Clipped has accepted responsibility for
  a small piece of Microsoft's code.
- **The recorder and the window are now linked the same way, on purpose.** They
  were linked differently by accident, and that accident is the whole of #407.
  If Tauri ever stops emitting its own link arguments, the window will need this
  build script's treatment too; nothing here detects that, because the window is
  in a detached workspace this test cannot reach
  (`apps/desktop/src-tauri/Cargo.toml`). Worth watching on a Tauri major.
- **One heap in the recorder process, still.** Everything in it — Clipped's
  code, the SQLite compiled into it, the FFmpeg libraries, and the vendor
  encoder libraries loaded at runtime (`amfrt64.dll`, `nvEncodeAPI64.dll`) —
  allocates from `ucrtbase.dll`. That is
  not a licence to free across an FFI boundary: the vendor libraries have their
  own allocators and their own release calls, and `crates/muxer` should go on
  using FFmpeg's allocator for anything FFmpeg will free, because the property
  is about this build rather than about the API. It is a safety net, not a
  design.
- **C code compiled into the recorder keeps working, and it is worth knowing
  why.** There is already some: `libsqlite3-sys` builds `sqlite3.c` with the
  `cc` crate, so the recording library's database engine is C in this binary
  (`crates/storage`). `cc` chooses `/MD` for it, because `crt-static` is not
  set, and its objects therefore ask the linker for `msvcrt.lib` —
  which `/NODEFAULTLIB:msvcrt.lib` refuses, leaving every symbol they need to
  resolve from `libcmt.lib` and `ucrt.lib`. A release build links it with no
  warnings, and SQLite's `malloc` is `ucrtbase.dll`'s, the same one everything
  else in the process uses. That is the arrangement working as intended rather
  than a lucky escape: keeping the universal CRT dynamic is what makes a `/MD`
  object and a static `vcruntime` compatible in the first place.

  The case that could still break is a **prebuilt** third-party `.lib` compiled
  against `/MD`, which cannot be recompiled with `/MT` and may bring
  expectations `libcmt.lib` does not meet. There is none today, and this is the
  sentence that explains the `LNK2005` when the first one arrives.
- **`STATUS_DLL_NOT_FOUND` is now a named failure rather than a number.** The
  supervisor's message names the status, says the recorder never ran and so
  logged nothing, and tells the user to reinstall. It costs one more variant of
  `SupervisorError`, and it is what turns a broken install — the FFmpeg
  libraries deleted by an antivirus, a half-finished update — from an
  unexplained "the recorder exited with status -1073741515" into something with
  an action attached.
- **Nothing here is enforced for a build made outside this repository.** Someone
  who builds `clipped-recorder.exe` with `cargo build` on their own machine gets
  the same treatment, because the build script travels with the source; someone
  who rebuilds it another way does not. That is why the supervisor's message
  still names the redistributable rather than assuming it can never be the
  cause.
