//! The executable behind a process identifier.
//!
//! # Ownership
//!
//! This module owns the one genuinely owned Windows handle in the crate. A
//! window handle and a monitor handle are weak references that are never closed
//! (see [`crate::WindowHandle`]); a process handle from `OpenProcess` is a
//! kernel object that leaks the process's kernel structures until it is closed.
//! [`ProcessHandle`] therefore closes it in [`Drop`], and is the only thing in
//! this crate with a destructor. There is no way to obtain the raw handle from
//! outside this module, so there is no way to keep one past the close
//! (AGENTS.md section 58).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
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
struct ProcessHandle(HANDLE);

impl ProcessHandle {
    /// Opens `process_id` for querying, or reports why it could not be opened.
    ///
    /// Failure is ordinary. A process that exited between being enumerated and
    /// being opened is gone, and a protected or higher-integrity process — the
    /// anti-cheat services that sit alongside games, most of the system ones —
    /// refuses to be opened at all. Neither is an error worth propagating: the
    /// caller loses the executable name and nothing else.
    fn open(process_id: u32) -> Option<Self> {
        // SAFETY: `OpenProcess` takes only integers and returns either a handle
        // or an error; there are no pointers or lifetimes involved. The handle
        // it returns is owned by the `ProcessHandle` built from it here and
        // closed exactly once, in `Drop`.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id) };
        handle.ok().map(Self)
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
    let path = ProcessHandle::open(process_id)?.image_path()?;
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
