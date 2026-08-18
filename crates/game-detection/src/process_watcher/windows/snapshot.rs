//! The process table, and the poller that falls back to it.
//!
//! Two jobs, one mechanism. `clipped_windows::process_table` gives the whole
//! process table in one call, which is what the watcher needs *once* at start
//! so that a game already running has a name to report when it exits — and it
//! is also the only way left to notice a process starting when WMI cannot be
//! reached, which is what the fallback poller does with it.
//!
//! The read itself is not this crate's: `clipped-windows` owns
//! `CreateToolhelp32Snapshot` and the handle discipline around it, so that no
//! crate of the recorder's workspace copies either (AGENTS.md section 55). The
//! desktop application has a read of its own, which it cannot share for
//! reasons that are about layering rather than about this crate — they are in
//! `clipped_windows::process_table`'s documentation, and the two reads are
//! listed in `tests/integration/tests/process_table_reads.rs`. This module is
//! left with the two things that are
//! this watcher's own — resolving a baseline's executable paths, and diffing
//! the table on a poll — plus the error type conversion the rest of the
//! watcher expects.

use std::collections::HashSet;
use std::sync::mpsc::Sender;
use std::sync::Arc;

use clipped_windows::{process_image_path, ProcessTableEntry};

use super::super::config::WatchConfig;
use super::super::error::SourceError;
use super::super::process::ProcessSnapshot;
use super::super::source::{SourceEvent, SourceMessage};
use super::stop::Stop;

/// Every process running now, by identifier, parent and name.
///
/// A thin wrapper over [`clipped_windows::process_table`] that turns its
/// [`clipped_windows::WindowsError`] into this watcher's own error type, so
/// that nothing else in the crate needs to know the read is platform code.
///
/// # Errors
///
/// [`SourceError`] when Windows will not produce the process table at all,
/// which is the machine being in trouble rather than an ordinary condition.
pub(crate) fn process_table() -> Result<Vec<ProcessTableEntry>, SourceError> {
    clipped_windows::process_table()
        .map_err(|error| SourceError::new("clipped_windows::process_table", error))
}

/// Every process running now, with its executable path where Windows gives one.
///
/// This is the watcher's baseline, and it is the expensive call in this module:
/// resolving a path means opening every process on the machine, which on an
/// ordinary desktop is a few hundred `OpenProcess` calls. It is paid once, while
/// the application is starting anyway, and `docs/game-detection.md` measures
/// what building a watcher costs rather than leaving that as an assertion.
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
fn resolve(row: &ProcessTableEntry) -> ProcessSnapshot {
    ProcessSnapshot::new(
        row.pid(),
        row.parent_pid(),
        process_image_path(row.pid()),
        row.name(),
    )
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
        let live: HashSet<u32> = current.iter().map(ProcessTableEntry::pid).collect();

        for row in current.iter().filter(|row| !known.contains(&row.pid())) {
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
            .find(|row| row.pid() == std::process::id())
            .expect("a process is in its own process table");

        assert!(
            own.name().to_lowercase().ends_with(".exe"),
            "expected an executable name, got {}",
            own.name()
        );
        assert_ne!(own.parent_pid(), own.pid());
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
}
