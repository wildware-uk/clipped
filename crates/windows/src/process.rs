//! An open process, and what can be asked of it: its executable, when it
//! started, and whether it is still running.
//!
//! # Ownership
//!
//! This module owns the one genuinely owned Windows handle in the crate. A
//! window handle and a monitor handle are weak references that are never closed
//! (see [`crate::WindowHandle`]); a process handle from `OpenProcess` is a
//! kernel object that leaks the process's kernel structures until it is closed.
//! [`ProcessHandle`] therefore closes it in [`Drop`], and is the only thing in
//! this crate with a destructor. There is no way to obtain the raw handle from
//! outside this crate, so there is no way to keep one past the close
//! (AGENTS.md section 58).
//!
//! # Holding one is what makes an identifier mean something
//!
//! Windows reuses process identifiers, often within seconds on a busy machine,
//! so a bare number is not a durable name for a process. An open handle is: the
//! kernel keeps the identifier reserved for as long as any handle to the
//! process object exists, even after the process itself has exited. That is why
//! [`crate::ProcessTree`] holds one per member rather than a list of numbers,
//! and it is the difference between scoping audio to a game and scoping it to
//! whatever inherited the game's identifier.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, WaitForSingleObject,
    PROCESS_ACCESS_RIGHTS, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE,
};

/// Longest path `QueryFullProcessImageNameW` is asked for, in UTF-16 units.
///
/// Windows paths can exceed `MAX_PATH` when long paths are enabled, and this
/// call reports `ERROR_INSUFFICIENT_BUFFER` rather than truncating, so the
/// buffer is sized for the extended limit. It is a stack array used once per
/// process per enumeration, so the size costs nothing worth optimising.
const MAX_IMAGE_PATH: usize = 32_768;

/// An open handle to another process, closed when this value is dropped.
///
/// Opened with `PROCESS_QUERY_LIMITED_INFORMATION`, which is the least
/// privilege that can answer "what is this process called?" and the only right
/// this crate needs. It is deliberately not `PROCESS_QUERY_INFORMATION`: the
/// limited right is granted for processes at a higher integrity level, and
/// asking for more than is needed is how a recorder ends up requiring
/// administrator rights to list windows.
#[derive(Debug)]
pub(crate) struct ProcessHandle(HANDLE);

// SAFETY: a process handle is a kernel object owned by this process, not by the
// thread that opened it: every call this crate makes on one — the queries below
// and `CloseHandle` — is valid from any thread, and none of them keeps
// thread-local state. Without this a `ProcessTree` could not be built on one
// thread and moved onto the thread that captures audio, because `HANDLE` is a
// raw pointer and so `!Send` by default.
unsafe impl Send for ProcessHandle {}

impl ProcessHandle {
    /// Opens `process_id` to ask what it is running.
    ///
    /// Failure is ordinary, and which failure it is matters to a caller. A
    /// process that exited between being enumerated and being opened is gone —
    /// `ERROR_INVALID_PARAMETER`, because the identifier names nothing — and
    /// that is news about the machine rather than about this application. A
    /// protected or higher-integrity process — the anti-cheat services that sit
    /// alongside games, most of the system ones — answers `ERROR_ACCESS_DENIED`
    /// however long it goes on running, and is a limit worth reporting.
    pub(crate) fn open(process_id: u32) -> Result<Self, windows::core::Error> {
        Self::open_with(process_id, PROCESS_QUERY_LIMITED_INFORMATION)
    }

    /// Opens `process_id` to follow it: to ask what it is running *and* to be
    /// able to wait on it.
    ///
    /// The extra right is `SYNCHRONIZE`, without which `WaitForSingleObject`
    /// refuses the handle and [`has_exited`](Self::has_exited) could never
    /// answer. It is granted for a process the user owns, which is every
    /// process a game consists of, and refused along with everything else for
    /// one running as another account — so asking for it costs nothing where it
    /// matters and fails no earlier than [`open`](Self::open) would where it
    /// does not.
    pub(crate) fn open_to_follow(process_id: u32) -> Result<Self, windows::core::Error> {
        Self::open_with(
            process_id,
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
        )
    }

    /// Opens `process_id` with exactly `access` and nothing more.
    fn open_with(
        process_id: u32,
        access: PROCESS_ACCESS_RIGHTS,
    ) -> Result<Self, windows::core::Error> {
        // SAFETY: `OpenProcess` takes only integers and returns either a handle
        // or an error; there are no pointers or lifetimes involved. The handle
        // it returns is owned by the `ProcessHandle` built from it here and
        // closed exactly once, in `Drop`.
        let handle = unsafe { OpenProcess(access, false, process_id) };
        handle.map(Self)
    }

    /// The full path of the executable this process is running.
    fn image_path(&self) -> Option<String> {
        let mut buffer = [0_u16; MAX_IMAGE_PATH];
        let mut length =
            u32::try_from(buffer.len()).expect("MAX_IMAGE_PATH fits in the u32 the API takes");

        // SAFETY: `buffer` is a live array of `length` UTF-16 units for the
        // duration of the call, which is what the pointer and the in/out length
        // describe; `self.0` is a process handle this value owns and has not
        // closed. The call writes at most `length` units and updates `length`
        // to the number it wrote.
        let result = unsafe {
            QueryFullProcessImageNameW(
                self.0,
                PROCESS_NAME_WIN32,
                PWSTR(buffer.as_mut_ptr()),
                &mut length,
            )
        };
        result.ok()?;

        let written = usize::try_from(length).ok()?;
        Some(String::from_utf16_lossy(&buffer[..written]))
    }

    /// When this process started, as a Windows file time.
    ///
    /// The second half of a durable name for a process. Windows guarantees that
    /// an identifier and a creation time together identify one process for the
    /// life of the machine: an identifier can come round again, but the process
    /// that gets it started later than the one that had it. Comparing creation
    /// times is therefore how [`crate::ProcessTree`] tells a game's child from
    /// a stranger that inherited its parent's number.
    ///
    /// [`None`] only if Windows refuses to answer for a handle it granted,
    /// which is not a case this crate has seen.
    pub(crate) fn creation_time(&self) -> Option<FileTime> {
        let mut created = FILETIME::default();
        let mut exited = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();

        // SAFETY: `self.0` is a handle this value owns and has not closed,
        // opened with `PROCESS_QUERY_LIMITED_INFORMATION`, which is the right
        // this call needs. All four out parameters are live `FILETIME`s for the
        // duration of the call; the API requires all four even though only the
        // first is wanted.
        unsafe { GetProcessTimes(self.0, &mut created, &mut exited, &mut kernel, &mut user) }
            .ok()?;

        Some(FileTime::from(created))
    }

    /// Whether the process has exited.
    ///
    /// A wait of zero on a process object, which is signalled once the process
    /// ends. This is the cheap check — no snapshot, no enumeration, a single
    /// syscall against a handle already held — and it is why a tree can notice
    /// a member leaving without asking Windows for the process table.
    ///
    /// `GetExitCodeProcess` would answer the same question and is deliberately
    /// not used: it reports `STILL_ACTIVE`, which is the ordinary number 259,
    /// so a process that exits with code 259 reads as running for ever.
    ///
    /// Only a handle from [`open_to_follow`](Self::open_to_follow) can answer.
    /// One from [`open`](Self::open) lacks `SYNCHRONIZE`, so the wait fails and
    /// this says `false` however long the process has been gone.
    pub(crate) fn has_exited(&self) -> bool {
        // SAFETY: `self.0` is a handle this value owns and has not closed. A
        // timeout of zero makes the call return immediately whatever the
        // process is doing, so this blocks nothing.
        let wait = unsafe { WaitForSingleObject(self.0, 0) };
        wait == WAIT_OBJECT_0
    }
}

/// A moment on the Windows file time scale: 100-nanosecond ticks since 1601.
///
/// Compared and never converted. What the values *are* does not matter here —
/// only that a process created after another has a larger one — and the two
/// sources this crate reads them from, `GetProcessTimes` and
/// `GetSystemTimeAsFileTime`, are the same clock in the same units.
///
/// That clock is the system clock rather than a monotonic one, so an
/// adjustment — a time server correcting a drifting machine, a user changing
/// the date — can move it. See [`crate::ProcessTree`] for what that costs,
/// which is one rescan of a candidate process rather than a wrong answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct FileTime(u64);

impl From<FILETIME> for FileTime {
    fn from(value: FILETIME) -> Self {
        Self(u64::from(value.dwHighDateTime) << 32 | u64::from(value.dwLowDateTime))
    }
}

#[cfg(test)]
impl FileTime {
    /// A file time for tests, which care only about the ordering.
    pub(crate) const fn from_ticks(ticks: u64) -> Self {
        Self(ticks)
    }
}

/// The current moment, on the same scale [`ProcessHandle::creation_time`]
/// answers in.
pub(crate) fn file_time_now() -> FileTime {
    // SAFETY: the call takes no arguments in this binding — it fills a
    // `FILETIME` the binding owns and returns it by value — and cannot fail.
    FileTime::from(unsafe { GetSystemTimeAsFileTime() })
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from `OpenProcess` in `open`, is not the
        // pseudo-handle of the current process, and cannot have been closed
        // already because nothing else can reach it: the field is private, the
        // type is not `Copy`, and `Drop` runs once.
        //
        // The result is deliberately discarded rather than logged. There is
        // nothing a caller could do about a handle that will not close, this
        // crate emits no diagnostics of its own, and `CloseHandle` on a handle
        // this code owns fails only if the process has been corrupted
        // (AGENTS.md section 15 allows an ignored failure that is documented).
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// The full path of the executable `process_id` is running, such as
/// `C:\Program Files\Steam\steamapps\common\Counter-Strike Global Offensive\game\bin\win64\cs2.exe`.
///
/// [`None`] when the process cannot be opened or has exited. That is common
/// enough to be unremarkable — see [`ProcessHandle::open`] — so callers report
/// it as an unknown executable rather than as a failure.
///
/// # Privacy
///
/// The answer is a user path and can carry an account name and a library
/// layout. It is fine to *match* on, and it must not reach a log line without
/// going through `clipped_logging::RedactedPath` first (docs/logging.md).
#[must_use]
pub fn process_image_path(process_id: u32) -> Option<PathBuf> {
    let path = ProcessHandle::open(process_id).ok()?.image_path()?;
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// The file name of the executable `process_id` is running, such as `cs2.exe`.
///
/// [`None`] when the process cannot be opened or has exited. That is common
/// enough to be unremarkable — see [`ProcessHandle::open`] — so callers report
/// it as an unknown name rather than as a failure.
#[must_use]
pub fn process_image_name(process_id: u32) -> Option<String> {
    let path = process_image_path(process_id)?;
    let name = Path::new(&path).file_name()?.to_string_lossy().into_owned();
    (!name.is_empty()).then_some(name)
}

/// Remembers the executable name behind each process identifier.
///
/// One enumeration meets a hundred or so windows belonging to a couple of dozen
/// processes, and a browser alone can contribute twenty of them. Opening the
/// same process twenty times is the single most expensive thing enumeration
/// does, so it is done once per process (AGENTS.md section 18).
///
/// The cache is deliberately per-enumeration and not shared: process
/// identifiers are reused by Windows, so a cache that outlived one pass over
/// the desktop would eventually attach a dead process's name to a live one.
#[derive(Debug, Default)]
pub(crate) struct ProcessNames {
    known: HashMap<u32, Option<String>>,
}

impl ProcessNames {
    /// The executable name for `process_id`, looked up at most once.
    pub(crate) fn name_of(&mut self, process_id: u32) -> Option<String> {
        self.known
            .entry(process_id)
            .or_insert_with(|| process_image_name(process_id))
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_can_name_its_own_executable() {
        let name = process_image_name(std::process::id())
            .expect("a process can always open and name itself");
        assert!(
            name.to_lowercase().ends_with(".exe"),
            "expected a Windows executable name, got {name}"
        );
    }

    #[test]
    fn the_name_is_the_final_component_of_the_path() {
        let process = std::process::id();
        let path = process_image_path(process).expect("a process can always open and name itself");
        let name = process_image_name(process).expect("and can therefore name its executable");

        assert!(
            path.is_absolute(),
            "expected an absolute image path, got {}",
            path.display()
        );
        // Checked against what the process itself says it is running, not only
        // for internal consistency: a path that had lost or gained a component
        // would agree with the name derived from it and still be wrong.
        let expected = std::env::current_exe().expect("a process knows its own executable");
        assert_eq!(
            path.file_name(),
            expected.file_name(),
            "expected the running executable, got {}",
            path.display()
        );
        assert_eq!(
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned()),
            Some(name),
            "the name must be the path's final component and nothing else"
        );
    }

    #[test]
    fn a_process_identifier_that_cannot_exist_has_no_path() {
        // 0 is the system idle process; see the test below for why it is the
        // one identifier that answers the same way on every machine.
        assert_eq!(process_image_path(0), None);
    }

    #[test]
    fn a_process_identifier_that_cannot_exist_has_no_name() {
        // 0 is the system idle process, which has no image and cannot be
        // opened. It is the one identifier guaranteed never to answer, on any
        // machine, which makes it the only deterministic negative case
        // available (AGENTS.md section 25).
        assert_eq!(process_image_name(0), None);
    }

    #[test]
    fn a_live_process_has_started_and_has_not_finished() {
        let before = file_time_now();
        let handle = ProcessHandle::open_to_follow(std::process::id())
            .expect("a process can always open itself");
        let created = handle
            .creation_time()
            .expect("Windows answers for a handle it granted");

        assert!(
            created <= before,
            "this process started before this test ran"
        );
        assert!(
            !handle.has_exited(),
            "a process asking about itself is by definition still running"
        );
    }

    #[test]
    fn a_process_that_has_exited_says_so_while_its_handle_is_held() {
        // The handle keeps the identifier reserved after the exit, which is the
        // property the process tree is built on: the number cannot come to mean
        // something else while this value is alive.
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/c", "exit", "0"])
            .spawn()
            .expect("cmd.exe is on every Windows installation");
        let handle =
            ProcessHandle::open_to_follow(child.id()).expect("a parent can open its own child");
        child.wait().expect("the child was spawned by this process");

        assert!(handle.has_exited());
        assert!(
            handle.creation_time().is_some(),
            "an exited process still has a creation time while a handle is held"
        );
    }

    #[test]
    fn a_handle_opened_only_for_querying_cannot_report_an_exit() {
        // The distinction the two constructors exist for, asserted rather than
        // left as a claim in a doc comment: without `SYNCHRONIZE` the wait
        // fails, and a tree built on such a handle would hold a member that
        // never leaves.
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/c", "exit", "0"])
            .spawn()
            .expect("cmd.exe is on every Windows installation");
        let queryable = ProcessHandle::open(child.id()).expect("a parent can open its own child");
        let followed =
            ProcessHandle::open_to_follow(child.id()).expect("a parent can open its own child");
        child.wait().expect("the child was spawned by this process");

        assert!(followed.has_exited());
        assert!(!queryable.has_exited(), "the wait is refused, not answered");
    }

    #[test]
    fn a_file_time_is_its_two_halves_in_the_right_order() {
        let raw = FILETIME {
            dwLowDateTime: 0x4444_3333,
            dwHighDateTime: 0x2222_1111,
        };

        assert_eq!(
            FileTime::from(raw),
            FileTime::from_ticks(0x2222_1111_4444_3333)
        );
        assert!(FileTime::from_ticks(1) < FileTime::from_ticks(2));
    }

    #[test]
    fn the_cache_answers_the_same_way_the_uncached_lookup_does() {
        let mut names = ProcessNames::default();
        let expected = process_image_name(std::process::id());

        assert_eq!(names.name_of(std::process::id()), expected);
        // The second call must come from the cache and must agree with the
        // first; a cache that returned `None` on a hit would be invisible here
        // without this.
        assert_eq!(names.name_of(std::process::id()), expected);
        assert_eq!(names.known.len(), 1);
    }
}
