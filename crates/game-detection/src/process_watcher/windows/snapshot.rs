//! The process table, read directly, and the poller that falls back to it.
//!
//! Two jobs, one mechanism. `CreateToolhelp32Snapshot` gives the whole process
//! table in one call, which is what the watcher needs *once* at start so that a
//! game already running has a name to report when it exits — and it is also
//! the only way left to notice a process starting when WMI cannot be reached,
//! which is what the fallback poller does with it.

use std::collections::HashSet;
use std::sync::mpsc::Sender;
use std::sync::Arc;

use clipped_windows::process_image_path;
use windows::Win32::Foundation::{CloseHandle, ERROR_NO_MORE_FILES, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

use super::super::config::WatchConfig;
use super::super::error::SourceError;
use super::super::process::ProcessSnapshot;
use super::super::source::{SourceEvent, SourceMessage};
use super::stop::Stop;

/// One row of the process table: everything it holds that the watcher wants.
///
/// Deliberately not a [`ProcessSnapshot`]: the table has no executable path in
/// it, and finding one costs a call per process. The poller resolves paths only
/// for processes it has not seen before, which on an idle machine is none of
/// them.
#[derive(Clone, Debug)]
pub(crate) struct TableRow {
    pub(crate) pid: u32,
    pub(crate) parent_pid: u32,
    pub(crate) name: String,
}

/// A snapshot handle, closed when this value is dropped.
///
/// The handle from `CreateToolhelp32Snapshot` is a kernel object like any
/// other: leaking one leaks the copy of the process table behind it, and the
/// poller takes one every interval for as long as Clipped runs (AGENTS.md
/// section 58).
struct SnapshotHandle(HANDLE);

impl SnapshotHandle {
    /// Takes a snapshot of the process table.
    fn take() -> Result<Self, SourceError> {
        // SAFETY: the call takes two integers and returns either a handle or an
        // error. The handle it returns is owned by the value built from it here
        // and closed exactly once, in `Drop`.
        let handle = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
            .map_err(|error| SourceError::new("CreateToolhelp32Snapshot", error))?;
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

/// Every process running now, by identifier, parent and name.
///
/// # Errors
///
/// [`SourceError`] when Windows will not produce the process table at all,
/// which is the machine being in trouble rather than an ordinary condition.
/// Individual rows are never dropped: the enumeration either works or does not.
pub(crate) fn process_table() -> Result<Vec<TableRow>, SourceError> {
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
        return Err(SourceError::new("Process32FirstW", error));
    }

    let mut rows = Vec::with_capacity(256);
    loop {
        rows.push(TableRow {
            pid: entry.th32ProcessID,
            parent_pid: entry.th32ParentProcessID,
            name: executable_name(&entry.szExeFile),
        });

        // SAFETY: as above; `entry` is reused, which is what this API expects.
        match unsafe { Process32NextW(snapshot.0, &mut entry) } {
            Ok(()) => {}
            Err(error) if error.code() == ERROR_NO_MORE_FILES.to_hresult() => break,
            Err(error) => return Err(SourceError::new("Process32NextW", error)),
        }
    }

    Ok(rows)
}

/// Every process running now, with its executable path where Windows gives one.
///
/// This is the watcher's baseline, and it is the expensive call in this module:
/// resolving a path means opening every process on the machine. It is paid once
/// at start, and `docs/game-detection.md` records what it cost.
///
/// # Errors
///
/// [`SourceError`] when the process table cannot be read.
pub(crate) fn baseline() -> Result<Vec<ProcessSnapshot>, SourceError> {
    Ok(process_table()?.iter().map(resolve).collect())
}

/// Turns a table row into a snapshot, opening the process for its path.
///
/// Protected and higher-integrity processes refuse to be opened at all, which
/// is ordinary rather than a fault — see `clipped_windows::process_image_path`
/// — and leaves the snapshot with a name and no path.
fn resolve(row: &TableRow) -> ProcessSnapshot {
    ProcessSnapshot::new(
        row.pid,
        row.parent_pid,
        process_image_path(row.pid),
        &row.name,
    )
}

/// The name out of a `PROCESSENTRY32W`, up to its terminator.
fn executable_name(raw: &[u16]) -> String {
    let end = raw.iter().position(|unit| *unit == 0).unwrap_or(raw.len());
    String::from_utf16_lossy(&raw[..end])
}

/// Polls the process table and reports what changed.
///
/// This is the fallback, and it is the thing the design exists to avoid: it
/// wakes every [`WatchConfig::source_interval`] whether anything happened or
/// not. It is still worth having, because the alternative when WMI is
/// unavailable is no detection at all, and it is deliberately the *only* thing
/// in the watcher that behaves this way.
///
/// The interval comes from [`WatchConfig::source_interval`] and not from the
/// field behind it, which is public and may say anything. Enumerating every
/// process on the machine as often as a caller asks would make this the
/// high-frequency polling loop the whole design exists to avoid (AGENTS.md
/// section 18), so the one-second floor applies here exactly as it does to the
/// subscription's `WITHIN` clause.
///
/// Returns when `stop` is signalled or when the process table cannot be read,
/// in which case it reports [`SourceMessage::Lost`] first.
pub(crate) fn poll(
    config: WatchConfig,
    known: &[u32],
    events: &Sender<SourceMessage>,
    stop: &Arc<Stop>,
) {
    let mut known: HashSet<u32> = known.iter().copied().collect();
    let interval = config.source_interval();

    while !stop.sleep(interval) {
        let current = match process_table() {
            Ok(current) => current,
            Err(error) => {
                let _ = events.send(SourceMessage::Lost(error));
                return;
            }
        };

        // A set, not a scan: a machine running a game has a few hundred
        // processes, and comparing two lists of that size by scanning is
        // fifty thousand comparisons every interval for nothing (AGENTS.md
        // section 18).
        let live: HashSet<u32> = current.iter().map(|row| row.pid).collect();

        for row in current.iter().filter(|row| !known.contains(&row.pid)) {
            if events
                .send(SourceMessage::Event(SourceEvent::Started(resolve(row))))
                .is_err()
            {
                return;
            }
        }

        for pid in known.difference(&live) {
            if events
                .send(SourceMessage::Event(SourceEvent::Exited { pid: *pid }))
                .is_err()
            {
                return;
            }
        }

        known = live;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_process_table_contains_this_process() {
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
    fn the_baseline_resolves_paths_where_windows_allows_it() {
        let baseline = baseline().expect("the process table can always be read");
        let own = baseline
            .iter()
            .find(|process| process.pid == std::process::id())
            .expect("a process is in its own baseline");

        assert!(
            own.image_path.is_some(),
            "a process can always open itself, so its own path must resolve"
        );
        assert_eq!(
            own.image_name,
            std::env::current_exe()
                .ok()
                .and_then(|path| path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned()))
                .expect("the running executable has a name")
        );
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
