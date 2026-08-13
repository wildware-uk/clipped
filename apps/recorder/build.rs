//! Links the recorder against a static Visual C++ runtime and a dynamic UCRT,
//! so that an installed recorder needs nothing Windows does not already have.
//!
//! An ordinary `cargo build` of any Rust program on `*-pc-windows-msvc` imports
//! `VCRUNTIME140.dll` — a hello-world does — and that library is part of the
//! Microsoft Visual C++ 2015-2022 redistributable rather than part of Windows.
//! On a machine that has never had the redistributable the recorder therefore
//! fails at load time with `STATUS_DLL_NOT_FOUND`, before `main`, before any
//! log file exists ([issue
//! #407](https://github.com/wildware-uk/clipped/issues/407)).
//!
//! [ADR 0007](../../docs/adr/0007-visual-c-runtime-linkage.md) is the decision
//! and the alternatives; this file is the whole of the implementation.
//!
//! # What the linker is being told
//!
//! Microsoft's C runtime is three libraries, not one, and they can be chosen
//! separately:
//!
//! | Part | Where it lives | Chosen here |
//! | --- | --- | --- |
//! | The startup and CRT glue | `libcmt.lib` / `msvcrt.lib` | static |
//! | The compiler runtime — exception handling, `memcpy` | `libvcruntime.lib` / `VCRUNTIME140.dll` | **static** |
//! | The universal CRT — `malloc`, `free`, `printf` | `libucrt.lib` / `api-ms-win-crt-*.dll` | **dynamic** |
//!
//! Only the middle row is the redistributable. The universal CRT has been a
//! component of Windows since Windows 10, serviced by Windows Update, so
//! importing it costs nothing and shipping it would be shipping a copy of the
//! operating system.
//!
//! Keeping it dynamic is not only about size. `libavutil` and its siblings
//! import `malloc` and `free` from `api-ms-win-crt-heap-l1-1-0.dll`, which is
//! the same `ucrtbase.dll` this recorder imports them from, so **the recorder
//! and FFmpeg allocate from one heap**. `-C target-feature=+crt-static` — the
//! obvious one-flag answer, and the one ADR 0007 rejects — links the universal
//! CRT statically too, giving the recorder a second heap in the one process
//! whose work is passing buffers to and from FFmpeg. `tests/runtime_libraries.rs`
//! asserts both halves of this, so the arrangement fails a test rather than
//! surviving as a comment.
//!
//! This is the same set of switches `tauri-build` already emits for
//! `clipped-desktop.exe`, which is why the window starts on a clean machine and
//! the recorder did not. Both executables are now linked the same way, rather
//! than differently by accident.
//!
//! # Why `/NODEFAULTLIB` is needed at all
//!
//! Every `.rlib` in the standard library carries a `/DEFAULTLIB:msvcrt.lib`
//! directive of its own, and the linker takes the union of what it is given.
//! Naming `libcmt.lib` without removing `msvcrt.lib` links both, and two C
//! runtimes in one binary is `LNK2005` at best and one heap freeing another
//! heap's memory at worst. So each library that must not appear is refused by
//! name, in both its release and debug spellings, before the replacements are
//! named.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // The `*-pc-windows-gnu` targets link a different runtime entirely and none
    // of these switches mean anything to their linker. Read from the target
    // rather than from `cfg!`, which would describe the machine running the
    // build script.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }

    for argument in [
        // Refused: the dynamic compiler runtime, and the debug spellings of
        // both, which a mixed build would otherwise pull in.
        "/NODEFAULTLIB:libvcruntimed.lib",
        "/NODEFAULTLIB:vcruntime.lib",
        "/NODEFAULTLIB:vcruntimed.lib",
        // Refused: the dynamic CRT glue that the standard library asks for, and
        // its debug spelling.
        "/NODEFAULTLIB:libcmtd.lib",
        "/NODEFAULTLIB:msvcrt.lib",
        "/NODEFAULTLIB:msvcrtd.lib",
        // Refused: the *static* universal CRT. This is the one that separates
        // this arrangement from `+crt-static`, and the reason the heap stays
        // shared with FFmpeg.
        "/NODEFAULTLIB:libucrt.lib",
        "/NODEFAULTLIB:libucrtd.lib",
        // Chosen: static glue, static compiler runtime, dynamic universal CRT.
        "/DEFAULTLIB:libcmt.lib",
        "/DEFAULTLIB:libvcruntime.lib",
        "/DEFAULTLIB:ucrt.lib",
    ] {
        // Every target of this package rather than only its binary, so that the
        // example fixtures and the integration tests are linked the way the
        // recorder a user installs is linked. A fixture that differs from the
        // program it stands in for is a test that proves something else.
        println!("cargo:rustc-link-arg={argument}");
    }
}
