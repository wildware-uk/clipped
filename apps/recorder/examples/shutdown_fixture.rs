//! A process that waits for Ctrl+C, for `tests/ctrl_c.rs` to interrupt.
//!
//! The signal path, isolated from everything a real recording needs. The
//! recorder itself is interrupted for real in the same test file, against a
//! real window and a real file — but that test needs a GPU and a desktop
//! session, so it is `#[ignore]`d, and this fixture is what keeps the signal,
//! the seam and the finalisation hook covered on every run and on every
//! machine.
//!
//! It installs the same Ctrl+C handler through the same
//! [`clipped_recorder::shutdown`] API, waits on the same signal, and finalises
//! through the same seam. Nothing about the path under test is simulated —
//! only the frames are missing.
//!
//! It is an example rather than a second binary so that it is built for tests
//! and not shipped with the recorder.
//!
//! ```text
//! shutdown_fixture <marker-path>
//! ```
//!
//! It prints `ready` on standard output once the handler is installed — the
//! signal must not be sent before that — and on shutdown writes the
//! [`StopReason`] to `<marker-path>` before exiting with code 0.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use clipped_recorder::shutdown::{
    allow_ctrl_c, install_ctrl_c_handler, run_until_shutdown, ShutdownSignal,
};

fn main() -> ExitCode {
    let Some(marker) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: shutdown_fixture <marker-path>");
        return ExitCode::FAILURE;
    };

    // The test spawns this fixture with `CREATE_NEW_PROCESS_GROUP`, which is
    // what lets it send a console control event to the child alone rather than
    // to the whole test run — and what leaves Ctrl+C disabled until it is asked
    // for back. The recorder does the same thing for the same reason, through
    // the same function.
    allow_ctrl_c();

    let signal = ShutdownSignal::new();
    if let Err(error) = install_ctrl_c_handler(&signal) {
        eprintln!("shutdown_fixture: {error}");
        return ExitCode::FAILURE;
    }

    // The test waits for this before sending the signal.
    println!("ready");
    if std::io::stdout().flush().is_err() {
        return ExitCode::FAILURE;
    }

    let outcome: Result<(), ()> = run_until_shutdown(
        &signal,
        |signal| {
            signal.wait();
            Ok(())
        },
        |reason| {
            // Standing in for flushing the encoder and closing the container.
            // Writing the reason is what lets the test prove the hook ran, and
            // ran for the right reason.
            let _ = fs::write(&marker, reason.to_string());
        },
    );

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(()) => ExitCode::FAILURE,
    }
}
