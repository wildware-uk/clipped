//! A "recorder" that exits at once without listening, for
//! `tests/supervision.rs` to point a supervisor at.
//!
//! The restart policy's whole reason to exist is a recorder that cannot start:
//! a missing runtime, a broken installation, a configuration it will not accept.
//! Testing that the supervisor stops trying needs a program that fails that way
//! every time, immediately and identically, and no real failure is reliable
//! enough to be a test fixture.
//!
//! It ignores its arguments — the supervisor passes `serve --endpoint NAME` — and
//! exits with a code chosen to be recognisable in a message.

use std::process::ExitCode;

/// The exit code this fixture always produces.
///
/// Not 0 and not 1: it should be obvious in a test failure that the number came
/// from here rather than from a real recorder failing.
const EXIT_CODE: u8 = 42;

fn main() -> ExitCode {
    eprintln!("never_listens: exiting with {EXIT_CODE} without listening, on purpose");
    ExitCode::from(EXIT_CODE)
}
