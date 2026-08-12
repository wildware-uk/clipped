//! Which processes are a game, right now, as they come and go.
//!
//! A game is not one process. A launcher starts it, an anti-cheat wrapper may
//! sit between them, some titles re-execute themselves, and the process that
//! actually renders audio is often not the one whose window is being captured
//! ([ADR 0003](../../../docs/adr/0003-process-specific-audio-capture.md)). So
//! "capture the game's audio" means capturing a *set* of processes, and the set
//! changes for as long as the game runs.
//!
//! This is not the same question `clipped_game_detection`'s process watcher
//! answers, and it is worth being precise about the difference because both
//! walk parent chains. Detection asks *did a game start* — it collects a burst
//! of process starts into one launch and stops caring. This asks *which
//! processes are it, right now* — membership with a lifetime, maintained for
//! the whole of a recording, and answered as a list of identifiers something
//! else can hand to Windows.
//!
//! # The shape of it: pin, then follow
//!
//! ```text
//! ProcessTree::rooted_at(pid)  ── opens a handle per member ──▶  pinned set
//!                                                                    │
//!                                       refresh()  ── at most once per interval
//!                                                                    │
//!                          TreeChange { joined, exited, refused } + members()
//! ```
//!
//! Two rules do all the work.
//!
//! **A member is a process this tree holds a handle to.** Not a number it
//! remembers. Windows reuses process identifiers — often within seconds on a
//! busy machine — and an open handle is what stops one being reused: the kernel
//! keeps the identifier reserved while any handle to the process object exists,
//! even after the process has exited. A tree that kept only numbers would, on a
//! long session, eventually scope a game's audio track to whatever inherited a
//! dead helper's identifier. Nobody would notice until they opened the file.
//!
//! **Membership is inherited and sticky.** The root is a member; a process
//! whose creator is a member becomes one, and stays one. Sticky is what
//! survives a launcher exiting while the game it started lives on: the parent's
//! identifier is still pinned, so its orphans are still reachable, which a
//! fresh walk of the parent chain from the root could not manage — Windows does
//! not re-parent orphans, it leaves them naming a process that no longer
//! exists.
//!
//! # Identifier reuse, and the two comparisons that defeat it
//!
//! Pinning protects identifiers this tree already holds. The remaining hazard
//! is *adoption*: the process table says process C's creator is member P, but
//! the table is a copy, and by the time C is opened it may be a different
//! process wearing the same number. Or C's real creator may be a long-dead
//! process that happened to hold P's number before P did — the table records a
//! creator's identifier, and never says whether it still means anything.
//!
//! Both are settled by creation times, which Windows guarantees are ordered:
//!
//! | Rule | Rejects |
//! | --- | --- |
//! | C started no later than the moment the process table was read | a process that took C's identifier after the table was copied |
//! | C started no earlier than its claimed parent | a process whose real creator held the parent's identifier before the parent did |
//!
//! Neither rejection is permanent: a candidate refused today is looked at again
//! at the next scan, with a fresh table and a fresh reading of the clock. That
//! matters because the clock those comparisons use is the system clock rather
//! than a monotonic one — the only clock a process creation time is expressed
//! against — so an adjustment mid-session costs one interval of one process's
//! audio rather than a wrong answer that persists.
//!
//! # What the catalogue's `child_processes` means here: nothing
//!
//! `clipped_game_detection`'s catalogue carries a list of executable names a
//! game is known to spawn, deliberately not as a match key. It is not a
//! membership key here either, and for a stronger reason: a name cannot say
//! *which* process it means. Admitting every process called
//! `anticheat-service.exe` would put a service shared by several games — or
//! anything a user renamed — into one game's audio track, which is exactly the
//! silent misattribution the comparisons above exist to prevent. Membership is
//! kernel parentage, verified, or it is nothing. If a game is ever found to
//! produce audio from a process that is genuinely not descended from it, the
//! answer is to root a second tree at that process deliberately, not to match
//! on a name.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, ERROR_NO_MORE_FILES, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

use crate::process::{file_time_now, FileTime, ProcessHandle};
use crate::WindowsError;

/// One row of the process table.
///
/// Deliberately not more than this. The table is read every scan and gives a
/// name for nothing, whereas an executable path costs an `OpenProcess` per
/// process on the machine — several hundred — and this module opens only the
/// handful of processes it is actually adopting.
#[derive(Clone, Debug)]
struct TableRow {
    /// The process identifier.
    pid: u32,
    /// The identifier of the process that created this one, which may since
    /// have exited and may since name something else entirely.
    parent_pid: u32,
    /// The executable's file name, such as `cs2.exe`.
    name: String,
}

/// A snapshot handle, closed when this value is dropped.
///
/// The handle from `CreateToolhelp32Snapshot` is a kernel object like any
/// other: leaking one leaks the copy of the process table behind it, and a tree
/// takes one every scan for as long as a recording lasts (AGENTS.md section
/// 58).
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
fn process_table() -> Result<Vec<TableRow>, WindowsError> {
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
        rows.push(TableRow {
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

/// What is known about one member, without the handle that pins it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Ancestry {
    /// The identifier of the process that created this one.
    ///
    /// Meaningful because the creator is itself pinned for as long as this
    /// member is remembered: within a tree, a parent identifier stays a name
    /// for the process that really was the parent.
    parent: u32,
    /// When this process started.
    created: FileTime,
    /// Whether it is still running. A member that has exited is kept — as a
    /// *ghost* — for as long as anything descended from it is alive.
    live: bool,
}

/// The membership rules, with no Windows in them.
///
/// Everything that decides *who belongs* lives here and is a function of
/// numbers, so the interesting cases — a launcher exiting under its game, a
/// recycled identifier claiming a member's parentage — are tested against
/// written-down process trees rather than against whatever the machine happens
/// to be running. The platform half is [`ProcessTree`], which opens handles,
/// reads the table and asks this what to do with the answers.
#[derive(Debug, Default)]
struct Lineage {
    members: HashMap<u32, Ancestry>,
}

impl Lineage {
    /// Whether `pid` is a member, alive or a ghost.
    fn contains(&self, pid: u32) -> bool {
        self.members.contains_key(&pid)
    }

    /// When the member `pid` started, if it is one.
    fn created(&self, pid: u32) -> Option<FileTime> {
        self.members.get(&pid).map(|member| member.created)
    }

    /// Records a member.
    ///
    /// A member inserted dead is a ghost from the start: it is never in
    /// [`Self::live`], so it is never announced as joining and can never be
    /// announced as exiting either — which matters, because a consumer told to
    /// remove an identifier it was never given would be right to be confused.
    fn insert(&mut self, pid: u32, parent: u32, created: FileTime, live: bool) {
        self.members.insert(
            pid,
            Ancestry {
                parent,
                created,
                live,
            },
        );
    }

    /// Marks `pid` as no longer running, keeping it as a ghost.
    fn mark_exited(&mut self, pid: u32) {
        if let Some(member) = self.members.get_mut(&pid) {
            member.live = false;
        }
    }

    /// The rows that claim descent from a member and are not members already.
    fn candidates<'rows>(&self, rows: &'rows [TableRow]) -> Vec<&'rows TableRow> {
        rows.iter()
            .filter(|row| self.contains(row.parent_pid) && !self.contains(row.pid))
            .collect()
    }

    /// Forgets ghosts that no living member descends from, answering with the
    /// identifiers released.
    ///
    /// A ghost is kept because its identifier is the only route to its orphans;
    /// once none of them is left there is nothing to reach, and holding the
    /// handle any longer would be a leak in a process that runs for days
    /// (AGENTS.md section 59). Releasing it lets Windows reuse the identifier
    /// again, which is safe precisely because it is no longer a member: a later
    /// process wearing that number claims descent from nothing.
    fn sweep(&mut self) -> Vec<u32> {
        let mut reachable: HashSet<u32> = HashSet::new();
        for (pid, _) in self.members.iter().filter(|(_, member)| member.live) {
            let mut step = *pid;
            // Walk up towards the root, marking. Bounded by the number of
            // members, which also makes it safe against a cycle that cannot
            // occur but would otherwise hang: no chain can visit more members
            // than exist.
            for _ in 0..=self.members.len() {
                reachable.insert(step);
                let Some(member) = self.members.get(&step) else {
                    break;
                };
                if member.parent == step {
                    break;
                }
                step = member.parent;
            }
        }

        let released: Vec<u32> = self
            .members
            .keys()
            .copied()
            .filter(|pid| !reachable.contains(pid))
            .collect();
        for pid in &released {
            self.members.remove(pid);
        }
        released
    }

    /// Every living member, in ascending identifier order.
    fn live(&self) -> Vec<u32> {
        let mut live: Vec<u32> = self
            .members
            .iter()
            .filter(|(_, member)| member.live)
            .map(|(pid, _)| *pid)
            .collect();
        live.sort_unstable();
        live
    }
}

/// Whether a candidate's creation time is consistent with the parentage the
/// process table claimed for it.
///
/// See the identifier-reuse table in the module documentation: the candidate
/// must have existed when the table was read, and must not predate the parent
/// it claims. Equality passes both ways — the file time's resolution is
/// coarser than the interval in which a process can be created and enumerated,
/// so demanding a strict inequality would refuse legitimate children.
fn consistent_with(candidate: FileTime, parent: FileTime, table_read_at: FileTime) -> bool {
    candidate <= table_read_at && candidate >= parent
}

/// Whether a failure to open a process means "never" rather than "not any
/// more".
///
/// Access denied is a standing limit: the process runs at a higher integrity
/// level than this application and will answer the same way for as long as it
/// exists, so it is worth telling somebody about. Every other failure is the
/// process having exited between the table being read and the open, which is
/// ordinary and says nothing about the machine.
fn is_permission_refusal(error: &windows::core::Error) -> bool {
    error.code() == ERROR_ACCESS_DENIED.to_hresult()
}

/// What changed about a tree's membership.
///
/// Empty is the usual answer: most seconds of most recordings have no process
/// starting or ending in them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TreeChange {
    joined: Vec<u32>,
    exited: Vec<u32>,
    refused: Vec<String>,
}

impl TreeChange {
    /// Processes that have joined the tree, in ascending identifier order.
    #[must_use]
    pub fn joined(&self) -> &[u32] {
        &self.joined
    }

    /// Members that have exited, in ascending identifier order.
    ///
    /// Their identifiers stay reserved until this tree releases them, so one
    /// named here cannot already mean something else.
    #[must_use]
    pub fn exited(&self) -> &[u32] {
        &self.exited
    }

    /// The executable names of processes that belong to the tree but which
    /// Windows would not let this application open.
    ///
    /// **Diagnostics, not membership.** A process that cannot be opened cannot
    /// be pinned, and an unpinned identifier is not one to scope a capture to,
    /// so these are not members and audio they produce lands wherever
    /// unattributed audio lands. In practice this is a game's anti-cheat or
    /// crash-reporting *service*, which runs as the system account and does not
    /// play anything; something here that is plainly part of the game is worth
    /// a report rather than a shrug.
    ///
    /// Names rather than identifiers, deliberately: there is nothing a caller
    /// may legitimately do with the identifier of a process this tree has
    /// refused, and a name is what makes the log line worth reading. The same
    /// names recur every scan for as long as the processes do.
    #[must_use]
    pub fn refused(&self) -> &[String] {
        &self.refused
    }

    /// Whether nothing at all happened.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.joined.is_empty() && self.exited.is_empty() && self.refused.is_empty()
    }
}

/// The set of processes that are one game, maintained while it runs.
///
/// Built from the process the session decided is the game, and kept current by
/// [`refresh`](Self::refresh). What it is for is scoping audio capture: the
/// identifiers [`members`](Self::members) returns are what
/// `ActivateAudioInterfaceAsync` is given to include a game's audio in one
/// track, or to exclude it from another
/// ([issue #26](https://github.com/wildware-uk/clipped/issues/26),
/// [issue #27](https://github.com/wildware-uk/clipped/issues/27)).
///
/// # One root, and why not the whole launch
///
/// A tree is rooted at the game, not at the launch that produced it.
/// `clipped_game_detection` reports a launcher, any wrapper and the game as one
/// group, and rooting here at the group would put the launcher's own sounds —
/// Steam's notification chime, a launcher's video advertisement — into the
/// track labelled with the game's name. The launcher is the game's *parent*,
/// and a parent is not a member.
///
/// # Threading
///
/// One owner, and no thread of its own. [`refresh`](Self::refresh) takes
/// `&mut self` and does all the work on the calling thread, so a tree cannot be
/// shared between threads and cannot surprise anything with a background scan.
/// It is [`Send`], so it can be built where the game is identified and moved
/// onto the thread that owns the capture.
///
/// Do not refresh from a video capture thread: a scan reads the whole process
/// table, which is measured in milliseconds rather than microseconds
/// (`docs/audio-routing.md`). An audio thread that wakes every few
/// milliseconds against a buffer of a few hundred can afford it; a frame loop
/// cannot (AGENTS.md section 20).
///
/// # Ownership
///
/// One open process handle per member, released when the member is swept or
/// when the tree is dropped. Nothing else is held.
///
/// # Example
///
/// ```no_run
/// use clipped_windows::ProcessTree;
///
/// # let game_pid = 0_u32;
/// let mut tree = ProcessTree::rooted_at(game_pid)?;
/// loop {
///     if !tree.refresh()?.is_empty() {
///         // Re-scope the capture to `tree.members()`.
///     }
///     if tree.members().is_empty() {
///         break; // The game and everything it started have gone.
///     }
/// }
/// # Ok::<(), clipped_windows::WindowsError>(())
/// ```
#[derive(Debug)]
pub struct ProcessTree {
    lineage: Lineage,
    /// The handle pinning each member's identifier. Exactly the same key set as
    /// [`Lineage::members`]; the two are separate so that the rules can be
    /// tested without a process to open.
    pins: HashMap<u32, ProcessHandle>,
    live: Vec<u32>,
    rescan_interval: Duration,
    last_scan: Instant,
}

impl ProcessTree {
    /// How long a scan's answer is trusted for, unless a caller says otherwise.
    ///
    /// The number is a trade between how long a helper that starts mid-session
    /// can go on playing into the wrong track and what the tracking costs. A
    /// second is short against the several seconds it takes a person to notice
    /// a sound, and the measured cost of a scan at this rate is in
    /// `docs/audio-routing.md`.
    pub const DEFAULT_RESCAN_INTERVAL: Duration = Duration::from_secs(1);

    /// Builds a tree rooted at `root`, adopting everything already descended
    /// from it.
    ///
    /// A game that has been running for an hour before Clipped opens is
    /// therefore complete from the first call rather than acquiring its
    /// children one interval at a time.
    ///
    /// # Errors
    ///
    /// [`WindowsError::ProcessUnavailable`] if `root` cannot be opened, which
    /// means either that it has already exited or that it runs at a higher
    /// integrity level than this application. Either way there is no tree to
    /// build and no audio to scope, so it is a failure rather than an empty
    /// answer. [`WindowsError::Api`] if Windows will not produce the process
    /// table.
    pub fn rooted_at(root: u32) -> Result<Self, WindowsError> {
        let handle = ProcessHandle::open_to_follow(root)
            .map_err(|_| WindowsError::ProcessUnavailable { process_id: root })?;
        let created = handle
            .creation_time()
            .ok_or(WindowsError::ProcessUnavailable { process_id: root })?;

        let mut lineage = Lineage::default();
        // The root's own creator is not a member and never becomes one; naming
        // itself as its parent keeps the sweep's walk to the root terminating
        // without a special case for it.
        lineage.insert(root, root, created, true);

        let mut tree = Self {
            lineage,
            pins: HashMap::from([(root, handle)]),
            live: vec![root],
            rescan_interval: Self::DEFAULT_RESCAN_INTERVAL,
            last_scan: Instant::now(),
        };
        tree.scan()?;
        Ok(tree)
    }

    /// Sets how often [`refresh`](Self::refresh) is willing to read the process
    /// table.
    ///
    /// The default is [`Self::DEFAULT_RESCAN_INTERVAL`]. A shorter interval
    /// picks a new child up sooner and costs a scan more often; zero scans on
    /// every call, which is for tests rather than for a recording.
    #[must_use]
    pub const fn with_rescan_interval(mut self, interval: Duration) -> Self {
        self.rescan_interval = interval;
        self
    }

    /// Brings membership up to date, reporting what changed.
    ///
    /// Cheap to call as often as a caller likes: the process table is read at
    /// most once per rescan interval, and a call inside that window does
    /// nothing and reports nothing. Membership is therefore up to one interval
    /// stale in both directions — a process that has started may not be a
    /// member yet, and one that has exited may not have been noticed — which is
    /// the documented interval the design trades for not polling.
    ///
    /// # Errors
    ///
    /// [`WindowsError::Api`] if the process table cannot be read. Membership is
    /// left exactly as it was and the next call tries again; a scan that fails
    /// is not a tree that has emptied.
    pub fn refresh(&mut self) -> Result<TreeChange, WindowsError> {
        if self.last_scan.elapsed() < self.rescan_interval {
            return Ok(TreeChange::default());
        }
        self.last_scan = Instant::now();
        self.scan()
    }

    /// The living members, in ascending identifier order.
    ///
    /// Empty once the game and everything it started have exited, which is how
    /// a caller knows the tree has nothing left to capture. Each identifier is
    /// pinned by a handle this tree holds, so none of them can come to mean
    /// another process while it is listed here.
    #[must_use]
    pub fn members(&self) -> &[u32] {
        &self.live
    }

    /// Whether `pid` is a living member of this tree.
    #[must_use]
    pub fn contains(&self, pid: u32) -> bool {
        self.live.binary_search(&pid).is_ok()
    }

    /// One pass: notice exits, adopt new children, release exhausted ghosts.
    fn scan(&mut self) -> Result<TreeChange, WindowsError> {
        let mut change = TreeChange::default();

        // Exits first, so that a parent which has just died is still a member
        // and its orphans can be adopted in the same pass.
        //
        // Each is a wait of zero on a handle already held, not a search of the
        // table: a member's exit is therefore noticed without the answer
        // depending on an identifier still meaning what it did.
        for pid in self.lineage.live() {
            if self.pins.get(&pid).is_some_and(ProcessHandle::has_exited) {
                self.lineage.mark_exited(pid);
                change.exited.push(pid);
            }
        }

        // The table is read once and the moment before it is read is what
        // candidates are judged against. Before rather than after: a process
        // created while the table was being copied is then refused and looked
        // at again next scan, whereas the other order would admit an identifier
        // recycled during the copy. Refusing a member for one interval is a
        // second of audio in the wrong track; admitting a stranger is a
        // stranger in the recording.
        let table_read_at = file_time_now();
        let rows = process_table()?;

        // Depth, not breadth: a game that started three processes deep between
        // scans is adopted in one call rather than one generation per interval.
        // Each pass adopts at least one member or stops, so this terminates.
        loop {
            let candidates = self.lineage.candidates(&rows);
            if candidates.is_empty() {
                break;
            }

            let joined_before = change.joined.len();
            for row in candidates {
                self.consider(row, table_read_at, &mut change);
            }
            if change.joined.len() == joined_before {
                break;
            }
        }

        for released in self.lineage.sweep() {
            self.pins.remove(&released);
        }

        self.live = self.lineage.live();
        change.joined.sort_unstable();
        change.exited.sort_unstable();
        Ok(change)
    }

    /// Decides one candidate, adopting it if it can be pinned and verified.
    fn consider(&mut self, row: &TableRow, table_read_at: FileTime, change: &mut TreeChange) {
        let Some(parent_created) = self.lineage.created(row.parent_pid) else {
            return;
        };

        let handle = match ProcessHandle::open_to_follow(row.pid) {
            Ok(handle) => handle,
            Err(error) => {
                if is_permission_refusal(&error) {
                    // Windows will not let this application open it, and never
                    // will: it runs at a higher integrity level. Worth
                    // reporting, because its audio will not be in the game's
                    // track.
                    change.refused.push(row.name.clone());
                }
                // Otherwise it exited between the table being read and now.
                // There is nothing to capture and nothing to say.
                return;
            }
        };

        let Some(created) = handle.creation_time() else {
            return;
        };
        if !consistent_with(created, parent_created, table_read_at) {
            // An identifier that has come round again. Not adopted, not
            // reported: the candidate is examined again at the next scan, by
            // which time the table will describe whatever is really there.
            return;
        }

        // A process that has exited already is still adopted, silently. It is
        // the only route to any orphan it left behind, and the identifier is
        // pinned from here on, so the route stays honest.
        let live = !handle.has_exited();
        self.lineage.insert(row.pid, row.parent_pid, created, live);
        self.pins.insert(row.pid, handle);
        if live {
            change.joined.push(row.pid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tree has to be movable onto the thread that owns a capture.
    const fn assert_send<T: Send>() {}
    const _: () = assert_send::<ProcessTree>();

    fn row(pid: u32, parent_pid: u32, name: &str) -> TableRow {
        TableRow {
            pid,
            parent_pid,
            name: name.to_owned(),
        }
    }

    fn at(ticks: u64) -> FileTime {
        FileTime::from_ticks(ticks)
    }

    fn rooted(pid: u32, created: u64) -> Lineage {
        let mut lineage = Lineage::default();
        lineage.insert(pid, pid, at(created), true);
        lineage
    }

    #[test]
    fn a_child_of_a_member_is_a_candidate_and_a_stranger_is_not() {
        let mut lineage = rooted(100, 10);
        lineage.insert(200, 100, at(20), true);
        let rows = vec![
            // The child of a member, and not a member itself: the only one.
            row(300, 200, "helper.exe"),
            // Descended from nothing this tree knows.
            row(400, 999, "notepad.exe"),
            // Members already. Offering one of these again would have it
            // reopened and reported as joining on every scan for the rest of
            // the recording.
            row(200, 100, "game-child.exe"),
            row(100, 50, "game.exe"),
        ];

        let candidates: Vec<u32> = lineage
            .candidates(&rows)
            .iter()
            .map(|row| row.pid)
            .collect();

        assert_eq!(candidates, vec![300]);
    }

    #[test]
    fn a_candidate_that_started_after_the_table_was_read_is_refused() {
        // The identifier was recycled between the snapshot and the open: what
        // is there now cannot have been in the table.
        assert!(!consistent_with(at(500), at(10), at(400)));
        assert!(consistent_with(at(400), at(10), at(400)));
    }

    #[test]
    fn a_candidate_older_than_its_claimed_parent_is_refused() {
        // Its real creator held the parent's identifier before the parent did.
        assert!(!consistent_with(at(5), at(10), at(400)));
        assert!(consistent_with(at(10), at(10), at(400)));
    }

    #[test]
    fn a_ghost_is_kept_while_an_orphan_of_it_lives_and_released_after() {
        let mut lineage = rooted(100, 10);
        lineage.insert(200, 100, at(20), true);
        lineage.insert(300, 200, at(30), true);

        // The middle process exits — a launcher handing over — and the leaf
        // lives on with a parent identifier that names a dead process.
        lineage.mark_exited(200);
        assert!(lineage.sweep().is_empty(), "300 still descends through 200");
        assert_eq!(lineage.live(), vec![100, 300]);

        // Once the leaf goes, nothing descends through the ghost any more and
        // both are released — the handles with them, so Windows may reuse those
        // identifiers again.
        lineage.mark_exited(300);
        let mut released = lineage.sweep();
        released.sort_unstable();
        assert_eq!(released, vec![200, 300]);
        assert!(!lineage.contains(200));
        assert!(
            lineage.contains(100),
            "the root is kept: it is itself a member, live or not"
        );
    }

    #[test]
    fn an_orphan_of_a_ghost_is_still_adopted() {
        let mut lineage = rooted(100, 10);
        lineage.insert(200, 100, at(20), true);
        lineage.mark_exited(200);

        // The child appeared in the table only after its parent had died. A
        // walk of the parent chain from the root would never reach it, because
        // Windows leaves an orphan naming a process that no longer exists.
        let rows = vec![row(300, 200, "game-child.exe")];
        let candidates: Vec<u32> = lineage
            .candidates(&rows)
            .iter()
            .map(|row| row.pid)
            .collect();

        assert_eq!(candidates, vec![300]);
    }

    #[test]
    fn everything_is_released_once_the_last_member_has_gone() {
        let mut lineage = rooted(100, 10);
        lineage.insert(200, 100, at(20), true);

        lineage.mark_exited(200);
        lineage.mark_exited(100);
        let mut released = lineage.sweep();
        released.sort_unstable();

        assert_eq!(released, vec![100, 200]);
        assert!(lineage.live().is_empty());
    }

    #[test]
    fn a_process_adopted_after_it_died_is_a_ghost_from_the_start() {
        let mut lineage = rooted(100, 10);
        // Found in the table, opened, and already gone: adopted as a ghost so
        // that its orphans stay reachable, but never live, so it is never
        // reported as joining and never reported as exiting either.
        lineage.insert(200, 100, at(20), false);

        assert_eq!(lineage.live(), vec![100]);
        assert!(
            lineage.contains(200),
            "it has to remain a member, or its orphans lose their route in"
        );
    }

    #[test]
    fn only_a_denial_is_worth_reporting_as_a_refusal() {
        let denied = windows::core::Error::from_hresult(
            windows::Win32::Foundation::ERROR_ACCESS_DENIED.to_hresult(),
        );
        // What `OpenProcess` answers for an identifier that names nothing,
        // which is what a process that exited a moment ago leaves behind.
        let gone = windows::core::Error::from_hresult(
            windows::Win32::Foundation::ERROR_INVALID_PARAMETER.to_hresult(),
        );

        assert!(is_permission_refusal(&denied));
        assert!(!is_permission_refusal(&gone));
    }

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

    #[test]
    fn a_tree_cannot_be_rooted_at_a_process_that_does_not_exist() {
        // 0 is the system idle process: the one identifier that cannot be
        // opened on any machine, which makes it the only deterministic negative
        // case available (AGENTS.md section 25).
        let error = ProcessTree::rooted_at(0).expect_err("the idle process cannot be opened");

        assert!(
            matches!(error, WindowsError::ProcessUnavailable { process_id: 0 }),
            "unexpected error: {error}"
        );
    }
}
