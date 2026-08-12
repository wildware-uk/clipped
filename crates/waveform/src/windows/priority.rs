//! Getting out of a game's way.
//!
//! AGENTS.md section 18 is the requirement: this application runs alongside
//! games, and generating waveforms for a library of recordings is exactly the
//! background work that must not take anything a game needs. Two things are
//! asked of Windows, and both matter.
//!
//! **`THREAD_PRIORITY_LOWEST`** drops the thread three levels below normal, so
//! the scheduler runs it only when nothing else in its priority class wants the
//! processor.
//!
//! **`THREAD_MODE_BACKGROUND_BEGIN`** does more than that: it drops the thread
//! to the lowest scheduling priority *and* puts its disk reads at background I/O
//! priority. That second half is the important one here. Summarising a recording
//! means reading the whole file, and a recording in progress is writing to the
//! same disk; a low-priority thread issuing normal-priority reads would still
//! take disk bandwidth from the recorder. Background I/O priority is what
//! Windows gives its own indexer for the same reason.
//!
//! Both are per-thread and reversible, and this crate applies them to a thread
//! it created and owns, never to a caller's.
//!
//! # When it does not work
//!
//! `SetThreadPriority` can fail. The service reports what actually happened
//! through [`crate::WorkerPriority`] rather than assuming, because "we asked for
//! background priority" and "the thread is running at background priority" are
//! different statements and only the second one is worth anything.

use windows::Win32::System::Threading::{
    GetCurrentThread, GetThreadPriority, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
    THREAD_MODE_BACKGROUND_END, THREAD_PRIORITY_LOWEST,
};

use crate::service::WorkerPriority;

/// Puts the calling thread into the background, and reports what took.
///
/// Call once, from the thread itself. Reversed by [`leave`].
pub(crate) fn enter() -> WorkerPriority {
    // SAFETY: `GetCurrentThread` returns a pseudo-handle to the calling thread.
    // It needs no closing and is valid wherever it is used on this thread.
    let thread = unsafe { GetCurrentThread() };

    // SAFETY: `thread` is a valid thread handle and the value is a documented
    // priority constant.
    let lowest = unsafe { SetThreadPriority(thread, THREAD_PRIORITY_LOWEST) }.is_ok();
    // Read here, before background mode, because that is the only point at
    // which `GetThreadPriority` answers the question "did the thread take the
    // lowest scheduling priority?". Once the thread is in background mode
    // Windows reports the background value instead, which is lower still but is
    // not `THREAD_PRIORITY_LOWEST`.
    //
    // SAFETY: as above. Returns `THREAD_PRIORITY_ERROR_RETURN` on failure,
    // which is a value no priority has, so it is reported as it is.
    let scheduling = unsafe { GetThreadPriority(thread) };

    // SAFETY: as above. Failure is expected when the thread is already in
    // background mode, which is why the result is reported rather than checked.
    let background = unsafe { SetThreadPriority(thread, THREAD_MODE_BACKGROUND_BEGIN) }.is_ok();
    // SAFETY: as above.
    let observed = unsafe { GetThreadPriority(thread) };

    WorkerPriority::new(lowest && scheduling == LOWEST, background, observed)
}

/// Takes the calling thread back out of background mode.
///
/// Only meaningful if [`enter`] managed it. Called before the worker thread
/// ends, so that a thread returned to a pool — which this crate does not do
/// today, but a future host might — is not left in background I/O mode.
pub(crate) fn leave() {
    // SAFETY: as in `enter`.
    let thread = unsafe { GetCurrentThread() };
    // SAFETY: as in `enter`. Failing here means the thread was not in
    // background mode, which is the state this is trying to reach.
    let _ = unsafe { SetThreadPriority(thread, THREAD_MODE_BACKGROUND_END) };
}

/// What `GetThreadPriority` reports for a thread at `THREAD_PRIORITY_LOWEST`.
pub(crate) const LOWEST: i32 = THREAD_PRIORITY_LOWEST.0;
