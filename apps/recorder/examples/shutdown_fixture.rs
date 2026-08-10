//! A process that waits for Ctrl+C, for `tests/ctrl_c.rs` to interrupt.
//!
//! Testing signal handling needs a real process to send a real signal to, and
//! the recorder itself exits immediately because there is no capture engine to
//! run. This fixture stands in for the recording loop that will replace it: it
//! installs the same Ctrl+C handler through the same
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

use clipped_recorder::shutdown::{install_ctrl_c_handler, run_until_shutdown, ShutdownSignal};

fn main() -> ExitCode {
    let Some(marker) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: shutdown_fixture <marker-path>");
        return ExitCode::FAILURE;
    };

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

/// Re-enables Ctrl+C for this process.
///
/// The test spawns this fixture with `CREATE_NEW_PROCESS_GROUP`, which is what
/// lets it send a console control event to the child alone rather than to the
/// whole test run. A process created that way starts with Ctrl+C *disabled*, so
/// it has to ask for it back before a handler can see anything. Nothing outside
/// this fixture needs to do that, which is why it lives here and not in the
/// recorder.
#[cfg(windows)]
fn allow_ctrl_c() {
    // SAFETY: `SetConsoleCtrlHandler` takes an optional handler pointer and a
    // BOOL. Passing `None` with `FALSE` removes the inherited "ignore Ctrl+C"
    // entry rather than registering anything, so no pointer is dereferenced and
    // no callback outlives anything. The return value is ignored deliberately:
    // if the process was not started in a new group there is nothing to remove
    // and the call fails harmlessly.
    unsafe {
        windows_sys::Win32::System::Console::SetConsoleCtrlHandler(None, 0);
    }
}

/// Nothing to do: only Windows disables Ctrl+C for a new process group.
#[cfg(not(windows))]
fn allow_ctrl_c() {}
