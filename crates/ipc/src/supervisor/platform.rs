//! Starting a process that outlives the one that started it.
//!
//! The only Windows call the supervisor makes that is not a named pipe, kept
//! here so that everything above it is platform-independent (AGENTS.md section
//! 5).

use std::path::Path;
use std::process::Child;

use super::SupervisorError;

/// Starts `executable` so that it survives this process ending, however this
/// process ends.
///
/// Three things are being asked for, and each is a separate hazard:
///
/// - **`DETACHED_PROCESS`** — no console. A child that inherits a console
///   receives that console's Ctrl+C and Ctrl+Break, and receives
///   `CTRL_CLOSE_EVENT` when the console window is closed, which would end the
///   recorder when a developer closes the terminal they started the application
///   from. The release build of the desktop application has no console to
///   inherit anyway; this makes the debug build behave the same way rather than
///   differently.
/// - **`CREATE_NEW_PROCESS_GROUP`** — its own group, so a console control event
///   aimed at this process's group cannot reach it either. The recorder asks
///   Ctrl+C back on for itself at startup, which is what keeps it interruptible
///   when it *is* started from a terminal
///   (`clipped_recorder::shutdown::allow_ctrl_c`).
/// - **Null standard streams** — nothing of this process is on the other end of
///   the recorder's stdout or stderr. A pipe whose reader has exited turns the
///   next write into a broken-pipe error, and the recorder writes diagnostics to
///   standard error continuously (`docs/logging.md`); it must not start failing
///   because a window closed. Its log files are where those diagnostics are
///   read from, which is what makes discarding the streams honest rather than
///   lossy.
///
/// The returned [`Child`] is a handle for asking whether it has exited. Dropping
/// it does not kill the process on Windows, which is the behaviour this relies
/// on: the caller keeps it while it waits for the recorder to start listening
/// and may drop it afterwards without touching the recording.
///
/// # Errors
///
/// [`SupervisorError::Spawn`] if the operating system refused, and
/// [`SupervisorError::Unsupported`] off Windows.
#[cfg(windows)]
pub(super) fn spawn_detached(executable: &Path, args: &[&str]) -> Result<Child, SupervisorError> {
    use std::os::windows::process::CommandExt as _;
    use std::process::{Command, Stdio};

    use windows::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};

    Command::new(executable)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(DETACHED_PROCESS.0 | CREATE_NEW_PROCESS_GROUP.0)
        .spawn()
        .map_err(|source| SupervisorError::Spawn {
            path: executable.to_path_buf(),
            source,
        })
}

/// Always fails: there is no detached process, because there is no transport
/// for it to serve.
///
/// # Errors
///
/// Always [`SupervisorError::Unsupported`].
#[cfg(not(windows))]
pub(super) fn spawn_detached(_executable: &Path, _args: &[&str]) -> Result<Child, SupervisorError> {
    Err(SupervisorError::Unsupported)
}
