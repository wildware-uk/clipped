//! The process table, read in one call.
//!
//! `CreateToolhelp32Snapshot` is the one Windows call that answers "what is
//! running, and who started it?" for the whole machine at once, and it is read
//! from exactly one place in the workspace: here (AGENTS.md section 55).
//! [`crate::ProcessTree`] uses it to find a game's children it has never met,
//! and `clipped_game_detection`'s process watcher uses it for the same table —
//! once as a baseline when it starts, and repeatedly as the fallback poller it
//! falls back to when it cannot subscribe to WMI. Two callers with different
//! needs is why [`ProcessTableEntry`] stops where it does: a name and a
//! parentage, not an executable path, because resolving a path is an
//! `OpenProcess` per row and only the watcher's baseline wants to pay for it
//! ([`process_image_path`](crate::process_image_path) is the call that does).

use windows::Win32::Foundation::{CloseHandle, ERROR_NO_MORE_FILES, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

use crate::WindowsError;

/// One row of the process table.
///
/// Deliberately not more than this. The table is read every scan and gives a
/// name for nothing, whereas an executable path costs an `OpenProcess` per
/// process on the machine — several hundred — and a caller that wants one asks
/// for it per process, not per table.
#[derive(Clone, Debug)]
pub struct ProcessTableEntry {
    pid: u32,
    parent_pid: u32,
    name: String,
}

impl ProcessTableEntry {
    /// The process identifier.
    #[must_use]
    pub const fn pid(&self) -> u32 {
        self.pid
    }

    /// The identifier of the process that created this one, which may since
    /// have exited and may since name something else entirely.
    #[must_use]
    pub const fn parent_pid(&self) -> u32 {
        self.parent_pid
    }

    /// The executable's file name, such as `cs2.exe`.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
impl ProcessTableEntry {
    /// A row for tests, which build trees and diffs without reading the real
    /// process table.
    pub(crate) fn for_test(pid: u32, parent_pid: u32, name: &str) -> Self {
        Self {
            pid,
            parent_pid,
            name: name.to_owned(),
        }
    }
}

/// A snapshot handle, closed when this value is dropped.
///
/// The handle from `CreateToolhelp32Snapshot` is a kernel object like any
/// other: leaking one leaks the copy of the process table behind it, and a
/// caller may take one every second for as long as Clipped runs (AGENTS.md
/// section 58).
struct SnapshotHandle(HANDLE);

impl SnapshotHandle {
    /// Takes a snapshot of the process table.
    fn take() -> Result<Self, WindowsError> {
        // SAFETY: the call takes two integers and returns either a handle or an
        // error. The handle it returns is owned by the value built from it here
        // and closed exactly once, in `Drop`.
        let handle = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
            .map_err(|error| WindowsError::api("CreateToolhelp32Snapshot", error))?;
        Ok(Self(handle))
    }
}

impl Drop for SnapshotHandle {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from `CreateToolhelp32Snapshot`, is not a
        // pseudo-handle, and cannot already have been closed: the field is
        // private, the type is not `Copy`, and `Drop` runs once.
        //
        // The result is discarded deliberately. There is nothing a caller could
        // do about a handle that will not close, and this call fails only for a
        // handle that was never valid (AGENTS.md section 15 allows an ignored
        // failure that is documented).
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Every process running now, by identifier, creator and name.
///
/// # Errors
///
/// [`WindowsError::Api`] when Windows will not produce the process table at
/// all, which is the machine being in trouble rather than an ordinary
/// condition. Individual rows are never dropped: the enumeration either works
/// or does not.
pub fn process_table() -> Result<Vec<ProcessTableEntry>, WindowsError> {
    let snapshot = SnapshotHandle::take()?;

    let mut entry = PROCESSENTRY32W {
        dwSize: u32::try_from(size_of::<PROCESSENTRY32W>())
            .expect("PROCESSENTRY32W is far smaller than u32::MAX"),
        ..Default::default()
    };

    // SAFETY: `entry` is a live, correctly sized `PROCESSENTRY32W` — the API
    // rejects the call outright if `dwSize` is wrong — and `snapshot.0` is a
    // handle this scope owns and has not closed.
    if let Err(error) = unsafe { Process32FirstW(snapshot.0, &mut entry) } {
        return Err(WindowsError::api("Process32FirstW", error));
    }

    let mut rows = Vec::with_capacity(512);
    loop {
        rows.push(ProcessTableEntry {
            pid: entry.th32ProcessID,
            parent_pid: entry.th32ParentProcessID,
            name: executable_name(&entry.szExeFile),
        });

        // SAFETY: as above; `entry` is reused, which is what this API expects.
        match unsafe { Process32NextW(snapshot.0, &mut entry) } {
            Ok(()) => {}
            Err(error) if error.code() == ERROR_NO_MORE_FILES.to_hresult() => break,
            Err(error) => return Err(WindowsError::api("Process32NextW", error)),
        }
    }

    Ok(rows)
}

/// The name out of a `PROCESSENTRY32W`, up to its terminator.
fn executable_name(raw: &[u16]) -> String {
    let end = raw.iter().position(|unit| *unit == 0).unwrap_or(raw.len());
    String::from_utf16_lossy(&raw[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_process_table_contains_this_process_and_names_it() {
        let rows = process_table().expect("the process table can always be read");
        let own = rows
            .iter()
            .find(|row| row.pid == std::process::id())
            .expect("a process is in its own process table");

        assert!(
            own.name.to_lowercase().ends_with(".exe"),
            "expected an executable name, got {}",
            own.name
        );
        assert_ne!(own.parent_pid, own.pid);
    }

    #[test]
    fn a_name_stops_at_its_terminator() {
        let mut raw = [0_u16; 8];
        for (slot, unit) in raw.iter_mut().zip("game.exe".encode_utf16()) {
            *slot = unit;
        }

        assert_eq!(executable_name(&raw), "game.exe");
        assert_eq!(executable_name(&[b'a' as u16, 0, b'b' as u16]), "a");
        assert_eq!(executable_name(&[]), "");
    }
}
