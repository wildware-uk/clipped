//! Which processes are Clipped, so that Clipped is never offered for recording.
//!
//! This application is not one process. The window is this one; the interface
//! *inside* the window is drawn by WebView2, which Windows runs in
//! `msedgewebview2.exe` processes that this one starts. Those processes have
//! windows of their own — the developer tools are a top-level, visible window
//! belonging to the WebView2 host, not to this process — so a rule that
//! excluded only this process's own windows left the record control offering
//! **`Start recording msedgewebview2.exe`**
//! ([issue #390](https://github.com/wildware-uk/clipped/issues/390)).
//!
//! One caller, [`foreground`](crate::foreground), and one question:
//! [`includes`].
//!
//! # Why parentage, and not the executable's name
//!
//! `msedgewebview2.exe` is not Clipped. It is the runtime *any* application may
//! host a webview in — Microsoft Teams, the Windows widgets board, and every
//! other Tauri application — and those are recordable like anything else.
//! Matching on the name would refuse all of them, which is a second bug in
//! place of the first. What identifies *this* application's webview host is not
//! what it is called: it is that this process started it.
//!
//! # Why not `ICoreWebView2::BrowserProcessId`
//!
//! WebView2 will say which process hosts it, which looks like the exact answer
//! and is the one the issue suggested. It was not taken, for three reasons:
//!
//! - **It is a number remembered at one moment.** WebView2 recreates its
//!   browser process after a crash (`ProcessFailed`), and Windows reuses
//!   process identifiers, so a remembered identifier can come to name a
//!   different application entirely — an exclusion that would then refuse to
//!   record somebody's game, silently. Parentage is asked afresh every time and
//!   cannot go stale.
//! - **It is not available when it is first needed.** The webview is reached
//!   through the event loop, so the identifier arrives after `setup` has run,
//!   and the foreground can change before then.
//! - **It answers about the browser process only**, where "everything this
//!   process started" also covers the helpers it starts in turn.
//!
//! # Identifier reuse, and the comparison that defeats it
//!
//! The process table records the identifier of a process's creator, and never
//! says whether it still means anything: a process whose creator has exited
//! goes on naming a number Windows is free to hand out again — quite possibly
//! to this process. Read naively, the table would then claim some
//! long-running application as Clipped's own child, and the record control
//! would stop offering it for as long as both were running, with nothing on
//! screen to explain why.
//!
//! So a candidate must also have started no earlier than this process did,
//! which no process created before Clipped can manage. It is the same
//! comparison `clipped_windows::ProcessTree` makes about the same hazard, for
//! the same reason.
//!
//! # Cost, measured
//!
//! One read of the process table per foreground change, on the thread that runs
//! the message loop. Measured on a Windows 11 machine with 377 processes
//! running: **7 ms** for a whole call, of which 5 ms is
//! `CreateToolhelp32Snapshot` itself — a kernel copy of the process table, so a
//! release build does not make it cheaper. Following the chain of parents
//! afterwards is tens of microseconds.
//!
//! That is worth paying here and would not be anywhere near a capture: a
//! foreground change is somebody switching windows, so it happens at human
//! speed and a handful of times a minute, and the thread it delays is drawing
//! nothing at that moment — the interface is rendered by the WebView2 processes
//! and the recording is a separate process entirely (ADR 0002, AGENTS.md
//! section 18).
//!
//! It is not made cheaper by remembering an answer. A process identifier that
//! was a stranger a moment ago can be Clipped's own webview after it is reused,
//! and a cache that got that wrong would offer to record Clipped again — which
//! is the bug this module exists to fix.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use windows::Win32::Foundation::{CloseHandle, ERROR_NO_MORE_FILES, FILETIME, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// Whether `process_id` is this process, or a process this one started.
///
/// The webview host drawing this application's interface is the case that
/// matters, and it is a child of this process; anything it starts in turn is
/// covered too, because descent is followed rather than parentage alone.
///
/// A process this application did not start answers `false`, including one
/// that *started* this application: a Clipped launched from a terminal must
/// not make that terminal unrecordable.
///
/// Failure is not fatal and is not an error the user is shown. If Windows will
/// not produce the process table, this answers `false` and says so once: the
/// consequence is that the record control may again offer Clipped's own
/// webview, which is a strange offer, where refusing everything instead would
/// leave the control unable to record anything at all.
pub(crate) fn includes(process_id: u32) -> bool {
    if process_id == std::process::id() {
        return true;
    }
    // The idle process, which is what Windows answers for a window it has no
    // process for. Nothing descends from it.
    if process_id == 0 {
        return false;
    }

    match process_parentage() {
        Ok(table) => started_by(
            &table,
            process_id,
            std::process::id(),
            started_at(process_id),
            this_process_started_at(),
        ),
        Err(error) => {
            report_once(&format!(
                "Clipped could not read the process table, so it cannot tell its own webview host \
                 from another application and may offer to record itself: {error}"
            ));
            false
        }
    }
}

/// A process, and the process that created it.
///
/// Deliberately nothing else. The table is read on every foreground change and
/// this is the whole of what the question needs; an executable name would cost
/// nothing here but would invite matching on one, which is the thing this
/// module exists to avoid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Parentage {
    /// The process's own identifier.
    process_id: u32,
    /// The identifier of the process that created it, which may since have
    /// exited and may since name something else entirely.
    parent_process_id: u32,
}

/// Whether `application` started `process`, given a process table and when the
/// two of them started.
///
/// Strictly descended: a process is not started by itself, which is why
/// [`includes`] answers that case before asking this.
///
/// Every rule about *who belongs* is here and is a function of numbers, so the
/// cases that matter — a webview host two generations down, a process naming a
/// parent that has exited, an identifier that has come round again, a table
/// that describes a loop — are tested against written-down process trees rather
/// than against whatever the machine happens to be running (AGENTS.md section
/// 25).
///
/// A start time [`None`] is one Windows would not give, which is a question
/// unanswered rather than evidence of a stranger: the table decides alone.
fn started_by(
    table: &[Parentage],
    process: u32,
    application: u32,
    started: Option<u64>,
    application_started: Option<u64>,
) -> bool {
    // Windows guarantees creation times are ordered, so a process created by
    // another cannot predate it — and no reading of the table can make it so.
    // Equality passes: the file time's resolution is coarser than the interval
    // in which a process can create another, and demanding a strict inequality
    // would refuse a webview host started in the same tick as the window.
    if let (Some(started), Some(application_started)) = (started, application_started) {
        if started < application_started {
            return false;
        }
    }

    descends_from(table, process, application)
}

/// Whether the table's parent identifiers lead from `process` up to
/// `application`.
fn descends_from(table: &[Parentage], process: u32, application: u32) -> bool {
    let mut step = process;
    // Bounded by the number of processes: a chain of parents cannot visit more
    // processes than exist. That is also what makes a table describing a loop
    // — which cannot happen, but which would otherwise hang the thread drawing
    // the window — merely a wasted scan.
    for _ in 0..table.len() {
        let Some(row) = table.iter().find(|row| row.process_id == step) else {
            // The chain has reached a process that is not in the table, which
            // is one that exited while the table was being read. There is
            // nothing further to follow.
            return false;
        };
        if row.parent_process_id == application {
            return true;
        }
        // The idle process, which is where every chain ends.
        if row.parent_process_id == 0 {
            return false;
        }
        step = row.parent_process_id;
    }

    false
}

/// Every process running now, by identifier and creator.
///
/// # Errors
///
/// The Windows error, when the process table cannot be produced at all — the
/// machine being in trouble rather than an ordinary condition. Individual rows
/// are never dropped: the enumeration either works or does not.
fn process_parentage() -> Result<Vec<Parentage>, windows::core::Error> {
    let snapshot = Snapshot::take()?;

    let mut entry = PROCESSENTRY32W {
        dwSize: u32::try_from(size_of::<PROCESSENTRY32W>())
            .expect("PROCESSENTRY32W is far smaller than u32::MAX"),
        ..Default::default()
    };

    // SAFETY: `entry` is a live, correctly sized `PROCESSENTRY32W` — the API
    // rejects the call outright if `dwSize` is wrong — and `snapshot.0` is a
    // handle this scope owns and has not closed.
    unsafe { Process32FirstW(snapshot.0, &mut entry) }?;

    let mut rows = Vec::with_capacity(512);
    loop {
        rows.push(Parentage {
            process_id: entry.th32ProcessID,
            parent_process_id: entry.th32ParentProcessID,
        });

        // SAFETY: as above; `entry` is reused, which is what this API expects.
        match unsafe { Process32NextW(snapshot.0, &mut entry) } {
            Ok(()) => {}
            Err(error) if error.code() == ERROR_NO_MORE_FILES.to_hresult() => break,
            Err(error) => return Err(error),
        }
    }

    Ok(rows)
}

/// A process-table snapshot, closed when this value is dropped.
///
/// The handle is a kernel object like any other, and one is taken on every
/// foreground change for as long as Clipped is open — which may be days
/// (AGENTS.md section 58).
struct Snapshot(HANDLE);

impl Snapshot {
    /// Takes a snapshot of the process table.
    fn take() -> Result<Self, windows::core::Error> {
        // SAFETY: the call takes two integers and returns either a handle or an
        // error. The handle it returns is owned by the value built from it here
        // and closed exactly once, in `Drop`.
        let handle = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }?;
        Ok(Self(handle))
    }
}

impl Drop for Snapshot {
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

/// When `process_id` started, in the file time's 100-nanosecond ticks, or
/// [`None`] if Windows would not say.
///
/// Opened with `PROCESS_QUERY_LIMITED_INFORMATION`, which is the least that
/// answers this question and the only one that works against a process running
/// at a higher integrity level than this one — which many games, and every
/// elevated application, are.
fn started_at(process_id: u32) -> Option<u64> {
    // SAFETY: no pointers are passed in, and the returned handle is closed
    // below on every path out.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id) }
        .ok()
        .filter(|handle| !handle.is_invalid())?;

    let started = creation_time(process);

    // SAFETY: `process` is a handle this function opened and has not closed.
    let _ = unsafe { CloseHandle(process) };
    started
}

/// When this process started, read once and remembered.
///
/// A value that cannot change, and the comparison it is for happens on every
/// foreground change.
fn this_process_started_at() -> Option<u64> {
    static STARTED: OnceLock<Option<u64>> = OnceLock::new();
    // SAFETY: `GetCurrentProcess` takes nothing and returns a pseudo-handle,
    // which is not a resource and must not be closed.
    *STARTED.get_or_init(|| creation_time(unsafe { GetCurrentProcess() }))
}

/// When the process behind `process` started, in 100-nanosecond ticks.
fn creation_time(process: HANDLE) -> Option<u64> {
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();

    // SAFETY: `process` is a handle the caller owns, opened for at least
    // `PROCESS_QUERY_LIMITED_INFORMATION`, and all four out-parameters are
    // live, writable `FILETIME`s. All four are required even though only the
    // first is wanted.
    unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) }.ok()?;

    Some(u64::from(created.dwHighDateTime) << 32 | u64::from(created.dwLowDateTime))
}

/// Says something to a developer's console, the first time only.
///
/// The conditions this reports recur on every foreground change for as long as
/// they last, and a window that is open for days must not fill a log with the
/// same sentence (AGENTS.md section 35). A release build has no console, which
/// is why this is not the way anything a *user* must know is said.
fn report_once(what: &str) {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    if !REPORTED.swap(true, Ordering::Relaxed) {
        eprintln!("{what}");
    }
}

#[cfg(test)]
mod tests {
    use std::os::windows::process::CommandExt as _;

    use windows::Win32::System::Threading::CREATE_NO_WINDOW;

    use super::*;

    /// The identifier this application's own process wears in these tests.
    const THIS: u32 = 1_000;

    /// When it started, in the file time's 100-nanosecond ticks.
    const THIS_STARTED: u64 = 133_000_000_000_000_000;

    fn row(process_id: u32, parent_process_id: u32) -> Parentage {
        Parentage {
            process_id,
            parent_process_id,
        }
    }

    /// Whether `process` is one this application started, with both start times
    /// known and `process` the younger — which is the ordinary case, and leaves
    /// the table to answer.
    fn started_by_this_application(table: &[Parentage], process: u32) -> bool {
        started_by(
            table,
            process,
            THIS,
            Some(THIS_STARTED + 5_000_000),
            Some(THIS_STARTED),
        )
    }

    #[test]
    fn the_webview_host_this_process_started_is_part_of_this_application() {
        // The shape issue #390 is about, as it is on a real machine: the window
        // is this process, `msedgewebview2.exe` is a child of it, and the
        // developer tools it raises are a top-level window of that child's own.
        let table = vec![row(THIS, 900), row(53_008, THIS)];

        assert!(started_by_this_application(&table, 53_008));
    }

    #[test]
    fn a_helper_the_webview_host_started_in_turn_is_part_of_this_application() {
        // WebView2 is several processes: a browser process, a renderer per
        // site, a GPU process, a crash handler. They are grandchildren of this
        // one, so parentage alone is not enough — descent has to be followed.
        let table = vec![row(THIS, 900), row(53_008, THIS), row(53_100, 53_008)];

        assert!(started_by_this_application(&table, 53_100));
    }

    #[test]
    fn a_webview_belonging_to_another_application_is_still_recordable() {
        // The whole reason this is parentage rather than an executable name.
        // Somebody else's Tauri application, or Teams, or the widgets board,
        // hosts `msedgewebview2.exe` too, and a user may legitimately want to
        // record it.
        let table = vec![row(THIS, 900), row(28_844, 32_892), row(32_892, 900)];

        assert!(!started_by_this_application(&table, 28_844));
        assert!(!started_by_this_application(&table, 32_892));
    }

    #[test]
    fn the_process_that_started_this_one_is_not_part_of_this_application() {
        // Descent, not kinship. A Clipped started from a terminal must not
        // make that terminal unrecordable — and a walk that followed the chain
        // the wrong way would do exactly that.
        let table = vec![row(THIS, 900), row(900, 4)];

        assert!(!started_by(&table, 900, THIS, Some(1), Some(THIS_STARTED)));
    }

    #[test]
    fn a_process_whose_parent_has_exited_is_not_claimed() {
        // Its creator is gone and is no longer in the table, so the chain
        // cannot be followed any further. Nothing about it says Clipped.
        let table = vec![row(THIS, 900), row(7_000, 6_999)];

        assert!(!started_by_this_application(&table, 7_000));
    }

    #[test]
    fn a_table_that_describes_a_loop_is_answered_rather_than_followed_for_ever() {
        // It cannot happen. It runs on the thread that draws the window, so
        // "cannot happen" is not a good enough reason to let it hang.
        let table = vec![row(THIS, 900), row(2_000, 3_000), row(3_000, 2_000)];

        assert!(!started_by_this_application(&table, 2_000));
    }

    #[test]
    fn an_application_older_than_this_one_is_never_claimed_as_its_child() {
        // The identifier-reuse defence, and the one case where the table is
        // wrong rather than incomplete. A long-running application whose
        // creator has exited goes on naming that creator's identifier, and
        // Windows may since have given the number to Clipped: the table then
        // says in so many words that the application is Clipped's own child,
        // and without this the record control would quietly stop offering it
        // for as long as both were running.
        let table = vec![row(THIS, 900), row(5_000, THIS)];

        assert!(
            !started_by(
                &table,
                5_000,
                THIS,
                Some(THIS_STARTED - 1),
                Some(THIS_STARTED)
            ),
            "a process that existed before this one cannot be a child of it"
        );
        assert!(
            started_by(&table, 5_000, THIS, Some(THIS_STARTED), Some(THIS_STARTED)),
            "the same tick is not before: a webview host started with the window reads this way"
        );
    }

    #[test]
    fn a_start_time_windows_would_not_give_leaves_the_table_to_answer() {
        // Refusing to decide without one would turn a permission failure into
        // a record control that offers Clipped's own webview again.
        let table = vec![row(THIS, 900), row(53_008, THIS)];

        assert!(started_by(&table, 53_008, THIS, None, Some(THIS_STARTED)));
        assert!(started_by(&table, 53_008, THIS, Some(THIS_STARTED), None));
    }

    #[test]
    fn this_process_is_part_of_this_application() {
        assert!(includes(std::process::id()));
    }

    #[test]
    fn a_process_this_one_started_is_part_of_this_application_and_its_creator_is_not() {
        // The half of `includes` that the written-down tables above cannot
        // reach: that the real process table is read, that the identifiers in
        // it are this machine's, and that the walk runs in the direction the
        // exclusion needs. A child stands in for the WebView2 host, which a
        // test has no way to start.
        //
        // `cmd /C pause` waits for a keystroke on standard input, which is a
        // pipe nothing writes to, so it stays alive until it is killed;
        // `CREATE_NO_WINDOW` is what keeps a console application from opening
        // a console of its own.
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "pause"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW.0)
            .spawn()
            .expect("cmd.exe is on every Windows installation");

        let child_is_ours = includes(child.id());
        let creator = process_parentage()
            .expect("the process table can always be read")
            .into_iter()
            .find(|row| row.process_id == std::process::id())
            .expect("a process is in its own process table")
            .parent_process_id;
        let creator_is_ours = includes(creator);

        let _ = child.kill();
        let _ = child.wait();

        assert!(
            child_is_ours,
            "a process this one started is part of this application"
        );
        assert!(!creator_is_ours, "the process that started this one is not");
    }
}
