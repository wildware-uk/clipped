//! A "recorder" Windows refuses to load, for `tests/supervision.rs` to point a
//! supervisor at.
//!
//! [Issue #407](https://github.com/wildware-uk/clipped/issues/407) is a recorder
//! that is killed by the loader before its first instruction, because a library
//! it imports is not on the machine. Nothing it might have logged is logged, and
//! the supervisor sees only a process that started and was gone by the next
//! poll — which is why it used to be reported as a recorder that exited, sending
//! the user to an empty log directory.
//!
//! Reproducing that needs a program the loader really will refuse, on any
//! machine, every time. An executable that *exits* with `0xC0000135` would be a
//! fixture asserting the constant in the test, not the behaviour: it would still
//! pass if Windows changed how a refusal is reported.
//!
//! So this binary imports a function from a DLL that does not exist. `raw-dylib`
//! is what makes that possible without an import library — the linker
//! synthesises the import from the name alone — and the name is one no machine
//! will ever have. Windows resolves imports before `main`, finds nothing, and
//! ends the process with `STATUS_DLL_NOT_FOUND`, which is exactly the failure a
//! machine without the Microsoft Visual C++ redistributable produced.
//!
//! There is nothing to run and nothing to assert here. Being started is the
//! whole of what this fixture does.

// A library no machine has, in a namespace nothing else uses. `+verbatim` keeps
// the name exactly as written, including the extension, so that the failure
// names something recognisable rather than a library somebody might one day
// install.
#[cfg(windows)]
#[link(
    name = "clipped-no-such-runtime.dll",
    kind = "raw-dylib",
    modifiers = "+verbatim"
)]
extern "C" {
    // Never called, and never resolved: naming it is what puts the library in
    // the import table.
    fn clipped_a_function_that_does_not_exist();
}

#[cfg(windows)]
fn main() {
    // SAFETY: unreachable. Windows ends this process while resolving imports,
    // before any code in it runs, which is the entire purpose of the fixture.
    // The call is here because an import nothing references is an import the
    // linker discards.
    unsafe { clipped_a_function_that_does_not_exist() }
}

#[cfg(not(windows))]
fn main() {
    // The loader being asked for a library that is not there is a Windows
    // failure with a Windows status code; there is nothing to stand in for it
    // elsewhere, and `tests/supervision.rs` is `#![cfg(windows)]` for the same
    // reason.
}
