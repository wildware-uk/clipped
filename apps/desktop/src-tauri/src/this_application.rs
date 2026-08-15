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
//! # Identifier reuse, and the comparisons that defeat it
//!
//! The process table records the identifier of a process's creator, and never
//! says whether it still means anything: a process whose creator has exited
//! goes on naming a number Windows is free to hand out again — quite possibly
//! to this process, or to something this process started. Read naively, the
//! table would then claim some long-running application as Clipped's own, and
//! the record control would stop offering it for as long as both were running,
//! with nothing on screen to explain why.
//!
//! So parentage is not believed on its own. Every link of the chain is held
//! against the two processes' creation times, which Windows guarantees are
//! ordered:
//!
//! | Rule | Rejects |
//! | --- | --- |
//! | a process started no earlier than the parent the table gives it | a link whose creator exited and whose identifier has since come round again |
//! | a process started no later than the moment the table was read | an identifier recycled between the table being read and the process being opened |
//!
//! **Every link, and not only the process being asked about.** The walk passes
//! through strangers' identifiers on its way up, so one stale link anywhere in
//! the chain is enough to hang a whole third-party subtree beneath Clipped —
//! and each process under it becomes silently unrecordable, which is the same
//! bug as the one this module fixes, pointed at somebody else's application.
//! Checking only the candidate would miss exactly that: a process younger than
//! Clipped, whose *own* start time therefore says nothing, reached through a
//! parent identifier that has been reused.
//!
//! Those are the two comparisons `clipped_windows::ProcessTree` makes about the
//! same hazard, for the same reason. They are made again here rather than
//! shared, and that is not a second implementation by accident:
//! `tests/integration/tests/workspace_layering.rs` allows this crate exactly
//! one member of the recorder's workspace, `clipped-ipc`, so that closing this
//! window can never reach capture or encoding (ADR 0002). `clipped-windows` is
//! not that one, and linking it to borrow a two-line comparison would put the
//! capture layer inside the window's process.
//!
//! The two copies differ in two places, and both are written down rather than
//! left to be discovered. The rule for an unknown start time is one — see
//! [`consistent_with`]. The clock the second comparison is made against is the
//! other: that crate still reads its moment from Windows' coarse system clock,
//! which is exactly what
//! [issue #406](https://github.com/wildware-uk/clipped/issues/406) found here
//! and is raised for it as
//! [issue #432](https://github.com/wildware-uk/clipped/issues/432). See
//! [`file_time_now`], which is where this one reads its own.
//!
//! # Cost, measured
//!
//! One read of the process table per foreground change, on the thread that runs
//! the message loop, and one `OpenProcess` per link of the chain walked
//! afterwards.
//!
//! Measured by asking about every one of the 360 processes running on a Windows
//! 11 machine, a debug build: **5.6 ms median and 7.1 ms at the 95th
//! percentile** for a whole call, of which 5.6 ms is `CreateToolhelp32Snapshot`
//! itself — a kernel copy of the process table, so a release build does not make
//! it cheaper. Walking the chain and timing every link of it is the remaining
//! **15 µs median, 113 µs at the very worst**, which is three parts in a
//! thousand of the call and is why the per-link comparison is affordable at all.
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
use windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
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

    let this_process = std::process::id();

    // `process_parentage` is called here rather than inside, so that neither of
    // the two answers above pays for a read of the process table.
    includes_given_the_process_table(
        process_id,
        this_process,
        process_parentage(),
        started_at_asking_windows,
    )
}

/// Whether `application` started `process`, given whatever Windows made of the
/// process table.
///
/// Split from [`includes`] for the arm that cannot be reached any other way: a
/// machine that will not produce its process table at all is not a state a test
/// can put Windows into, and what is answered for it is a decision rather than
/// an oversight — see [`includes`]. Written this way, both arms are a function
/// of arguments a test can write down.
///
/// Reads no clock and takes no snapshot of its own: the rule that pairs the two
/// lives in [`process_parentage`], and this is handed the result of it.
fn includes_given_the_process_table(
    process: u32,
    application: u32,
    process_table: Result<(u64, Vec<Parentage>), windows::core::Error>,
    started_at: impl Fn(u32) -> Option<u64>,
) -> bool {
    match process_table {
        Ok((table_read_at, table)) => {
            started_by(&table, process, application, table_read_at, started_at)
        }
        Err(error) => {
            report_once(&format!(
                "Clipped could not read the process table, so it cannot tell its own webview host \
                 from another application and may offer to record itself: {error}"
            ));
            false
        }
    }
}

/// The creation times the real call site judges the process table by: Windows'
/// answer for any process, and the remembered one for this process.
///
/// A named function rather than a closure inside [`includes`] because it is the
/// one place where the per-link identifier-reuse defence is wired to the
/// machine, and it needs a test of its own. Every written-down tree in this
/// module supplies its own times, so a version of this that answered [`None`]
/// for anything but this process would leave all of them passing while turning
/// that defence off on a real machine entirely — an unknown time leaves the link
/// to the table, which is the whole of what [`consistent_with`] does with one.
fn started_at_asking_windows(process_id: u32) -> Option<u64> {
    if process_id == std::process::id() {
        // A value that cannot change, and this is the one process on the chain
        // that is asked about on every single call.
        this_process_started_at()
    } else {
        started_at(process_id)
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

/// Whether `application` started `process`, following the table's parent
/// identifiers upwards and checking every link against `started_at`.
///
/// Strictly descended: a process is not started by itself, which is why
/// [`includes`] answers that case before asking this.
///
/// Every rule about *who belongs* is here and is a function of numbers, so the
/// cases that matter — a webview host two generations down, a process naming a
/// parent that has exited, an identifier that has come round again part way up
/// the chain, a table that describes a loop — are tested against written-down
/// process trees rather than against whatever the machine happens to be running
/// (AGENTS.md section 25). `started_at` is a function rather than a pair of
/// times for the same reason: a test writes the tree's creation times down
/// beside its parentage, and the real call site passes
/// [`started_at_asking_windows`].
fn started_by(
    table: &[Parentage],
    process: u32,
    application: u32,
    table_read_at: u64,
    started_at: impl Fn(u32) -> Option<u64>,
) -> bool {
    let mut step = process;
    let mut step_started = started_at(step);

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
        let parent = row.parent_process_id;
        // The idle process, which is where every chain ends.
        if parent == 0 {
            return false;
        }

        // Before the identifier is compared with this application's, because
        // the last link of the chain is a link like any other: an unbelievable
        // one there is what would claim a stranger as this process's own child.
        let parent_started = started_at(parent);
        if !consistent_with(step_started, parent_started, table_read_at) {
            return false;
        }
        if parent == application {
            return true;
        }

        step = parent;
        step_started = parent_started;
    }

    false
}

/// Whether a process's creation time is consistent with the parent the process
/// table claimed for it.
///
/// See the identifier-reuse table in the module documentation: the process must
/// have existed when the table was read, and must not predate the parent it
/// names. Equality passes both ways — the file time's resolution is coarser
/// than the interval in which a process can create another or be created and
/// enumerated, and demanding a strict inequality would refuse a webview host
/// started in the same tick as the window.
///
/// Both halves compare two moments, and neither means anything unless the two
/// were read from the same clock. `parent` and `process` always are: they are
/// what `GetProcessTimes` says about two processes. `table_read_at` is the one
/// that had to be *chosen*, and choosing wrongly is not a rounding error —
/// reading it from Windows' coarse system clock made this refuse real children
/// of this process, because that clock stands still between the machine's timer
/// ticks while a creation time does not (issue #406, and see [`file_time_now`],
/// which is where the choice is made and argued).
///
/// A start time [`None`] is one Windows would not give, which is a question
/// unanswered rather than evidence of a stranger, so the table decides that
/// link alone. That is where this parts company with
/// `clipped_windows::ProcessTree`, which refuses a candidate it cannot time:
/// there, refusing costs an interval of a game's audio and is retried a moment
/// later, whereas refusing here means offering to record Clipped's own webview,
/// which is the bug this module exists to fix. Opening a process for
/// `PROCESS_QUERY_LIMITED_INFORMATION` is refused for the handful of protected
/// processes on the machine, none of which Clipped can have started.
fn consistent_with(process: Option<u64>, parent: Option<u64>, table_read_at: u64) -> bool {
    let (Some(process), Some(parent)) = (process, parent) else {
        return true;
    };

    process <= table_read_at && process >= parent
}

/// Every process running now, by identifier and creator, and the moment just
/// before they were read.
///
/// The order the two are produced in is a rule and not a detail. The moment is
/// read *first*, and creation times are read against it afterwards, so that a
/// process wearing an identifier only since the table was copied is refused and
/// looked at again on the next foreground change. The other order would quietly
/// admit exactly that identifier.
///
/// Returning them together does not enforce that: swapping the two statements
/// below compiles, passes every test in this module, and is wrong, because no
/// test can see which order two adjacent statements are in. What it does is
/// keep both reads in one short function with the rule written beside them,
/// instead of leaving each caller to read a clock whenever it liked — which is
/// a smaller claim than structure, and is the one that is true.
///
/// # Errors
///
/// The Windows error, when the process table cannot be produced at all — the
/// machine being in trouble rather than an ordinary condition. Individual rows
/// are never dropped: the enumeration either works or does not.
fn process_parentage() -> Result<(u64, Vec<Parentage>), windows::core::Error> {
    let read_at = file_time_now();
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

    Ok((read_at, rows))
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

/// Now, in the same 100-nanosecond ticks a creation time is expressed in, and
/// read from the same clock.
///
/// `GetSystemTimePreciseAsFileTime` and not `GetSystemTimeAsFileTime`, which
/// this read until [issue #406](https://github.com/wildware-uk/clipped/issues/406).
/// The two are not one clock at two resolutions; the second is a *stale copy*
/// of the first. It answers with the value written at the machine's last timer
/// tick — 15.625 ms apart by default, and shorter only while some process on
/// the machine is holding the timer resolution down — where a creation time is
/// stamped by the kernel at the moment the process is created, in between
/// ticks. Measured over 400 spawns on a developer machine, not one creation
/// time landed on the coarse clock's grid.
///
/// A process created a millisecond *before* this was called was therefore
/// answered a moment older than itself, and [`consistent_with`]'s `process <=
/// table_read_at` refused it: 27 of those 400 real children of the test
/// process, on a machine whose timer another application was holding down to
/// half a millisecond. Judged against a modelled 15.625 ms tick — a machine
/// with nothing holding it down, which is the default and is what a CI runner
/// often is — the same 400 spawns refuse 373. That is the bug issue #390
/// exists to prevent, offering to record Clipped's own webview host, and it
/// needs only that the host be started within one tick of the foreground
/// change being judged. It was found as a test that failed on CI and passed
/// on a re-run, which is what a real defect looks like from a distance.
///
/// The alternative repair — keeping the coarse clock and allowing a tick of
/// slack on the comparison — was not taken. It would widen the window in which
/// a recycled identifier is admitted to a whole tick, to work around a
/// staleness that is not there once the right clock is read. Reading the
/// precise one costs a serialising instruction against the performance
/// counter: tens of nanoseconds, beside the 5.6 ms `CreateToolhelp32Snapshot`
/// it is sampled next to.
fn file_time_now() -> u64 {
    // SAFETY: takes nothing and returns a `FILETIME` by value.
    ticks(unsafe { GetSystemTimePreciseAsFileTime() })
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

    Some(ticks(created))
}

/// A file time as the one number it is: 100-nanosecond ticks since 1601.
///
/// Windows splits it across two 32-bit halves because the structure predates a
/// compiler that could return a 64-bit integer; comparing the halves separately
/// is the classic way to get a time comparison wrong.
fn ticks(time: FILETIME) -> u64 {
    u64::from(time.dwHighDateTime) << 32 | u64::from(time.dwLowDateTime)
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

    use windows::Win32::Foundation::E_FAIL;
    use windows::Win32::System::Threading::CREATE_NO_WINDOW;

    use super::*;

    /// The identifier this application's own process wears in these tests.
    const THIS: u32 = 1_000;

    /// When it started, in the file time's 100-nanosecond ticks.
    const THIS_STARTED: u64 = 133_000_000_000_000_000;

    /// When the process table was read: after everything in these written-down
    /// trees started, which is the ordinary case.
    const TABLE_READ_AT: u64 = THIS_STARTED + 1_000_000_000;

    fn row(process_id: u32, parent_process_id: u32) -> Parentage {
        Parentage {
            process_id,
            parent_process_id,
        }
    }

    /// The creation times of a written-down tree, by identifier.
    ///
    /// Anything not named started half a second after this application, which
    /// is what a process this application started looks like and so leaves the
    /// table to answer. A test that is *about* a creation time names the ones
    /// it is about.
    fn started(named: &[(u32, u64)]) -> impl Fn(u32) -> Option<u64> + '_ {
        move |of| {
            Some(
                named
                    .iter()
                    .find(|(process, _)| *process == of)
                    .map_or(THIS_STARTED + 5_000_000, |(_, at)| *at),
            )
        }
    }

    /// A live process this one started, for the tests that need a real one
    /// rather than a written-down tree.
    ///
    /// It stands in for the WebView2 host, which a test has no way to start.
    /// `cmd /C pause` waits for a keystroke on standard input, which is a pipe
    /// nothing writes to, so it stays alive until it is killed; `CREATE_NO_WINDOW`
    /// is what keeps a console application from opening a console of its own.
    ///
    /// The caller kills it, and must: a `cmd.exe` left behind waits for a
    /// keystroke for as long as the machine is up.
    fn a_process_this_one_started() -> std::process::Child {
        std::process::Command::new("cmd")
            .args(["/C", "pause"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW.0)
            .spawn()
            .expect("cmd.exe is on every Windows installation")
    }

    /// Whether `process` is one this application started, with every creation
    /// time in the ordinary order.
    fn started_by_this_application(table: &[Parentage], process: u32) -> bool {
        started_by(
            table,
            process,
            THIS,
            TABLE_READ_AT,
            started(&[(THIS, THIS_STARTED)]),
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

        assert!(!started_by_this_application(&table, 900));
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
                TABLE_READ_AT,
                started(&[(THIS, THIS_STARTED), (5_000, THIS_STARTED - 1)]),
            ),
            "a process that existed before this one cannot be a child of it"
        );
        assert!(
            started_by(
                &table,
                5_000,
                THIS,
                TABLE_READ_AT,
                started(&[(THIS, THIS_STARTED), (5_000, THIS_STARTED)]),
            ),
            "the same tick is not before: a webview host started with the window reads this way"
        );
    }

    #[test]
    fn a_reused_identifier_part_way_up_the_chain_never_claims_a_stranger() {
        // The hazard a check on the candidate alone cannot see, and the reason
        // every link is checked rather than the first.
        //
        // 6_000 is this application's webview host, started after it. 8_000 is
        // somebody else's application, started by a process that has since
        // exited and whose identifier Windows has since given to that webview
        // host — so the table now reads as though this application started it,
        // two links up. 8_000 is *younger* than this application, so its own
        // start time says nothing; what is impossible is the link between it
        // and the identifier it names, and only that link says so.
        //
        // Left unchecked, every process under 8_000 becomes unrecordable for
        // as long as both are running, with nothing on screen to explain it.
        let table = vec![row(THIS, 900), row(6_000, THIS), row(8_000, 6_000)];
        let times = started(&[
            (THIS, THIS_STARTED),
            (6_000, THIS_STARTED + 5_000_000),
            (8_000, THIS_STARTED + 1_000_000),
        ]);

        assert!(
            !started_by(&table, 8_000, THIS, TABLE_READ_AT, &times),
            "another application's process must not be claimed through a link that has been reused"
        );
        assert!(
            started_by(&table, 6_000, THIS, TABLE_READ_AT, &times),
            "and the real webview host, whose links all hold, still is this application's"
        );
    }

    #[test]
    fn an_identifier_recycled_since_the_table_was_read_is_not_claimed() {
        // The table said 53_008 was this process's child, and when it was read
        // that was true. By the time the process is opened to be timed, 53_008
        // has exited and Windows has given the number to something that started
        // after the table was copied — so the process being asked about is not
        // the process the table described.
        let table = vec![row(THIS, 900), row(53_008, THIS)];

        assert!(
            !started_by(
                &table,
                53_008,
                THIS,
                TABLE_READ_AT,
                started(&[(THIS, THIS_STARTED), (53_008, TABLE_READ_AT + 1)]),
            ),
            "a process that did not exist when the table was read is not in it"
        );
        assert!(
            started_by(
                &table,
                53_008,
                THIS,
                TABLE_READ_AT,
                started(&[(THIS, THIS_STARTED), (53_008, TABLE_READ_AT)]),
            ),
            "the same tick is not after: a process created as the table was copied reads this way"
        );
    }

    #[test]
    fn a_start_time_windows_would_not_give_leaves_that_link_to_the_table() {
        // Refusing to decide without one would turn a permission failure into
        // a record control that offers Clipped's own webview again.
        let table = vec![row(THIS, 900), row(53_008, THIS)];

        assert!(started_by(&table, 53_008, THIS, TABLE_READ_AT, |of| {
            (of != 53_008).then_some(THIS_STARTED)
        }));
        assert!(started_by(&table, 53_008, THIS, TABLE_READ_AT, |of| {
            (of != THIS).then_some(THIS_STARTED)
        }));
    }

    #[test]
    fn this_process_is_part_of_this_application() {
        assert!(includes(std::process::id()));
    }

    #[test]
    fn the_idle_process_is_not_part_of_this_application() {
        // Process 0 is what Windows answers for a window it has no process
        // for, so this is the identifier the foreground hands over whenever it
        // could not name one — and `includes` decides it before the table is
        // ever read.
        //
        // Which way it is decided is not a formality. Answering `true` would
        // claim the idle process as Clipped's own, and every window Windows
        // declined to name a process for would silently stop being offered for
        // recording: the bug issue #390 is about, pointed at whatever the user
        // was actually looking at.
        assert!(
            !includes(0),
            "the idle process is neither this process nor one it started"
        );
    }

    #[test]
    fn a_process_this_one_started_is_part_of_this_application_and_its_creator_is_not() {
        // The half of `includes` that the written-down tables above cannot
        // reach: that the real process table is read, that the identifiers in
        // it are this machine's, and that the walk runs in the direction the
        // exclusion needs.
        //
        // Membership only: this says nothing about the creation times the walk
        // was given, which is what the test below is for.
        let mut child = a_process_this_one_started();

        let child_is_ours = includes(child.id());
        let creator = process_parentage()
            .expect("the process table can always be read")
            .1
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

    #[test]
    fn the_times_the_real_call_site_judges_the_table_by_are_the_ones_windows_gives() {
        // The wiring, rather than the rule. Every tree above writes its own
        // creation times down, so all of them go on passing if the times the
        // real call site supplies stop being real — and an unknown time leaves
        // its link to the table (`consistent_with`), so a
        // `started_at_asking_windows` that answered `None` for every process
        // but this one would switch the whole per-link identifier-reuse
        // defence off on a real machine, silently, which is the defect issue
        // #390's fix exists to prevent.
        let mut child = a_process_this_one_started();
        let child_started = started_at_asking_windows(child.id());
        let _ = child.kill();
        let _ = child.wait();

        let this_started = started_at_asking_windows(std::process::id());

        assert!(
            child_started.is_some(),
            "a process this one started is timed by asking Windows about it"
        );
        assert!(
            this_process_started_at().is_some(),
            "and Windows always times the process doing the asking"
        );
        assert_eq!(
            this_started,
            this_process_started_at(),
            "which this process is answered from, being the one asked about every time"
        );
        assert!(
            child_started >= this_started,
            "the times are the processes' own, and a child cannot have started before its parent"
        );
    }

    #[test]
    fn the_creation_time_the_real_call_site_reads_is_a_whole_file_time() {
        // `ticks` is held to this below, and that is not enough on its own:
        // `creation_time` is the only place a real process's creation time is
        // produced, and it could drop the high half itself — the same mistake,
        // made one call frame up from the helper written to prevent it. Every
        // tree in this module writes its own times down, so every one of them
        // goes on passing while the production path returns half a time.
        //
        // The consequence is not a rounding error. `table_read_at` comes from
        // `file_time_now`, which keeps its high half, so `process <=
        // table_read_at` would be vacuously true and the recycled-identifier
        // half of `consistent_with` would switch off entirely; `process >=
        // parent` would meanwhile compare two numbers across a wrap every 429
        // seconds.
        //
        // A file time is 100-nanosecond ticks since 1601, so a process running
        // now started around 1.3 x 10^17 of them in — some thirty million
        // times `u32::MAX`. A creation time that fits in thirty-two bits is
        // therefore not a time at all: it is one half of one.
        let before_it_started = file_time_now();
        let mut child = a_process_this_one_started();
        let child_started = started_at_asking_windows(child.id());
        let _ = child.kill();
        let _ = child.wait();

        assert!(
            child_started.is_some_and(|started| started > u64::from(u32::MAX)),
            "a real process's creation time carries a high half: {child_started:?}"
        );
        assert!(
            this_process_started_at().is_some_and(|started| started > u64::from(u32::MAX)),
            "and so does the one remembered for this process: {:?}",
            this_process_started_at()
        );
        // The size is not the whole of it. Dropping the *low* half leaves a
        // number well above `u32::MAX` and up to 429 seconds early, and
        // reading one of the other three `FILETIME`s leaves a number that is
        // not a moment at all — neither of them a moment a process spawned
        // just now could have started at.
        assert!(
            child_started.is_some_and(|started| started >= before_it_started),
            "and it is the moment the process started, which was after this one was read: \
             {child_started:?} against {before_it_started}"
        );
    }

    #[test]
    fn a_process_created_just_before_the_table_was_read_is_not_refused_as_younger_than_it() {
        // The defect behind issue #406, which arrived looking like a flaky
        // test: `a_process_this_one_started_is_part_of_this_application_and_
        // its_creator_is_not` failed on CI and passed on a re-run of the same
        // commit, five times over two days, on branches touching none of this.
        // It was not flaky. It was reporting that `consistent_with`'s `process
        // <= table_read_at` refuses a real child of this process, and it is
        // the only test in this module that could — every written-down tree
        // above supplies both sides of that comparison itself, so all of them
        // pass whichever clock `file_time_now` reads.
        //
        // The two sides came from different clocks. A creation time is stamped
        // by the kernel when the process is created; `table_read_at` came from
        // `GetSystemTimeAsFileTime`, which does not read the clock at all — it
        // answers with the value written at the machine's last timer tick,
        // 15.625 ms apart by default. A process created in between two ticks
        // therefore carried a moment *later* than the "now" sampled after it,
        // and was refused as one that did not exist when the table was read.
        //
        // Which is why this is written with no process in it. A creation time
        // is a reading of the system clock taken by the kernel, so a reading
        // of the system clock taken here stands in for one exactly, and does
        // it without waiting on a spawn — whose several milliseconds are the
        // only reason the failure needs a machine at the default tick to show
        // up at all. Measured over 400 real spawns: none of their creation
        // times landed on the coarse clock's grid, 27 were refused outright on
        // a machine whose timer another application was holding down to half a
        // millisecond, and 373 are refused when the same samples are judged
        // against a modelled 15.625 ms tick.
        //
        // Both directions are asserted, because both are ways of getting the
        // moment wrong and only one of them is the bug that was found. A
        // `table_read_at` behind the clock refuses this application's own
        // webview host, which is the defect issue #390 exists to prevent; one
        // *ahead* of the clock — a slack term, or a constant — would admit an
        // identifier recycled after the table was read, which is the hazard
        // the comparison is there for in the first place. The moment has to be
        // the moment.
        //
        // SAFETY: takes nothing and returns a `FILETIME` by value.
        let created = ticks(unsafe { GetSystemTimePreciseAsFileTime() });
        let table_read_at = file_time_now();

        assert!(
            consistent_with(Some(created), Some(created), table_read_at),
            "a process created at {created} existed when the table was read, and the moment that \
             read was judged by is {} ticks behind it",
            created.saturating_sub(table_read_at)
        );

        let table_read_at = file_time_now();
        // SAFETY: as above.
        let by_now = ticks(unsafe { GetSystemTimePreciseAsFileTime() });

        assert!(
            table_read_at <= by_now,
            "and the moment is not ahead of the clock either, which would admit an identifier \
             recycled after the table was read: {table_read_at} against {by_now}"
        );
    }

    #[test]
    fn a_file_time_is_the_one_number_its_two_halves_make() {
        // Two file times read in one run of a test share a high half, so a
        // `ticks` that dropped it is invisible for the 429 seconds it takes
        // the low half to wrap — and then wrong by seven minutes, in a
        // comparison that decides whether a stranger's application is
        // recordable.
        assert_eq!(
            ticks(FILETIME {
                dwHighDateTime: 1,
                dwLowDateTime: 0,
            }),
            1 << 32,
            "the high half is the top thirty-two bits of the number"
        );
        assert_eq!(
            ticks(FILETIME {
                dwHighDateTime: 0,
                dwLowDateTime: u32::MAX,
            }),
            u64::from(u32::MAX),
            "and the low half is the bottom thirty-two, unshifted"
        );
        assert_eq!(
            ticks(FILETIME {
                dwHighDateTime: 0x01DB_4E3A,
                dwLowDateTime: 0x9C4F_1200,
            }),
            0x01DB_4E3A_9C4F_1200,
            "which together are one number, of the size a file time in this century is"
        );
    }

    #[test]
    fn a_process_table_windows_will_not_produce_leaves_the_window_recordable() {
        // The failure the machine has to be in trouble for, and the one arm no
        // written-down tree reaches. Refusing everything instead would leave
        // the record control unable to record anything at all, so the
        // deliberate answer is the other way: everything stays recordable,
        // including — for as long as the table cannot be read — Clipped's own
        // webview.
        //
        // Same process, same times, same application: only whether the table
        // could be read differs, and the answers differ with it. This is the
        // one test that prints the report `includes` documents, once.
        let times = started(&[(THIS, THIS_STARTED)]);

        assert!(
            includes_given_the_process_table(
                53_008,
                THIS,
                Ok((TABLE_READ_AT, vec![row(THIS, 900), row(53_008, THIS)])),
                &times,
            ),
            "the webview host is this application's when the table can be read"
        );
        assert!(
            !includes_given_the_process_table(
                53_008,
                THIS,
                Err(windows::core::Error::from_hresult(E_FAIL)),
                &times,
            ),
            "and is left recordable when it cannot, rather than every window being refused"
        );
    }
}
