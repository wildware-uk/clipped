//! Which application the user was last looking at.
//!
//! The tray menu's Start Recording has to name something to record, and the
//! protocol's `start_recording` takes a window, a process name or a process
//! identifier (`docs/ipc.md`). A tray has no picker and no screen of its own, so
//! the only answer that is both useful and honest is *the application that was
//! in front when the menu was opened* — which is what this module remembers.
//!
//! # Why a window event hook rather than asking when the menu opens
//!
//! [`GetForegroundWindow`] at the moment a menu item is clicked answers with the
//! shell: opening a notification-area menu gives the foreground to the taskbar,
//! and `TrackPopupMenu` requires it. By then the answer wanted is one change
//! behind.
//!
//! So the foreground is followed as it happens, through
//! `EVENT_SYSTEM_FOREGROUND`. It costs nothing until a foreground window changes
//! — no timer, no polling, no thread — which is the same choice
//! `clipped-game-detection` made for process starts (issue #41), for the same
//! reason: this process runs beside a game (AGENTS.md section 18).
//!
//! # What is deliberately not remembered
//!
//! Two things, and both are exclusions rather than a guess at what a game is:
//!
//! - **This process's own windows.** Recording Clipped's window would be absurd,
//!   and clicking the tray is a foreground change to this process.
//! - **The shell's own surfaces**, by window class: the taskbar, the notification
//!   overflow, Start, Search and the desktop. Opening the tray menu raises the
//!   taskbar, so without this the answer would be `explorer.exe` every time.
//!   The list is of *shell surfaces*, not of Explorer — a File Explorer window
//!   is `CabinetWClass` and is remembered like anything else, because somebody
//!   may legitimately want to record one.
//!
//! Everything else is remembered as it comes. Nothing here decides what is worth
//! recording; the user does, by having been in it.
//!
//! # Threading
//!
//! [`follow_the_foreground_window`] must be called on the thread that runs the
//! message loop, which in a Tauri application is the main thread: an
//! out-of-context hook delivers its callbacks through the hooking thread's
//! message queue, so a hook installed on a thread that never pumps messages
//! never fires. [`last_seen`] may be called from anywhere.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HWND, MAX_PATH};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetWindowThreadProcessId, IsWindowVisible,
    EVENT_SYSTEM_FOREGROUND, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
};

/// The window classes of the shell's own surfaces.
///
/// Every one of these can take the foreground because the user reached for
/// Clipped rather than because they were using it. `Shell_TrayWnd` is the
/// taskbar, which is what the tray icon lives in, so it is the one that matters;
/// the rest are the surfaces beside it. `CabinetWClass` — a File Explorer window
/// — is deliberately absent.
const SHELL_WINDOW_CLASSES: &[&str] = &[
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "NotifyIconOverflowWindow",
    "TopLevelWindowForOverflowXamlIsland",
    "Windows.UI.Core.CoreWindow",
    "XamlExplorerHostIslandWindow",
    "Progman",
    "WorkerW",
];

/// The last application window the user was in, as the tray describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ForegroundWindow {
    /// The process that owns it, which is what `start_recording` is given.
    ///
    /// A process identifier rather than a window handle, because the protocol
    /// takes one and the recorder resolves the window itself — one set of rules
    /// about what a recordable window is, in the recorder, rather than a second
    /// copy of them here (AGENTS.md section 55).
    pub(crate) process_id: u32,
    /// The executable's file name, such as `cs2.exe`, for the menu to name.
    ///
    /// The executable rather than the window title. A title is user content and
    /// the surest way to put somebody's document name into a screenshot
    /// (AGENTS.md section 13); the recorder's own `target` follows the same rule.
    pub(crate) process_name: String,
}

/// The last foreground window seen, shared with the hook's callback.
static LAST_SEEN: Mutex<Option<ForegroundWindow>> = Mutex::new(None);

/// Whether the hook is installed, so that a second call is not a second hook.
static FOLLOWING: AtomicBool = AtomicBool::new(false);

/// Starts following the foreground window, and seeds it with what is in front
/// now.
///
/// Call once, from the thread that runs the message loop. A second call does
/// nothing. Failure is not fatal and is not an error the user is shown: the
/// consequence is that the tray's Start Recording has nothing to name and says
/// so, which is a state the menu already has to render.
///
/// The hook is never removed. It lives for the life of the process, and Windows
/// releases it when the process ends; unhooking it would need the same thread
/// that installed it, at a moment that thread is about to exit anyway.
pub(crate) fn follow_the_foreground_window() {
    if FOLLOWING.swap(true, Ordering::SeqCst) {
        return;
    }

    // Whatever is in front right now, so that a menu opened before the first
    // foreground change still has an answer.
    // SAFETY: no arguments, and the handle it returns is only ever passed back
    // to Windows. It may be null, which `describe` checks for.
    remember(unsafe { GetForegroundWindow() });

    // SAFETY: the callback is a real `extern "system"` function of the right
    // signature, and every pointer argument is passed by Windows. The hook is
    // out-of-context, so the callback runs on this thread through its message
    // queue rather than inside another process. `WINEVENT_SKIPOWNPROCESS` is
    // belt and braces over `remember`'s own check.
    let hook = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(on_foreground_changed),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };

    if hook.is_invalid() {
        FOLLOWING.store(false, Ordering::SeqCst);
        eprintln!(
            "Clipped could not follow the foreground window, so the tray cannot offer to record \
             what is in front of you."
        );
    }
}

/// The last application window the user was in, if one has been seen.
pub(crate) fn last_seen() -> Option<ForegroundWindow> {
    LAST_SEEN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Windows telling us the foreground window changed.
///
/// # Safety
///
/// Called by Windows with the signature `WINEVENTPROC` requires. It reads no
/// pointer it was given and it must not unwind across the FFI boundary, which
/// is what the catch is for — a panic here would abort a process that may be
/// supervising a recording (AGENTS.md section 17).
unsafe extern "system" fn on_foreground_changed(
    _hook: HWINEVENTHOOK,
    _event: u32,
    window: HWND,
    _object_id: i32,
    _child_id: i32,
    _thread: u32,
    _time: u32,
) {
    let _ = std::panic::catch_unwind(|| remember(window));
}

/// Stores `window` as the last one the user was in, unless it is one to ignore.
fn remember(window: HWND) {
    let Some(seen) = describe(window) else {
        return;
    };

    let mut last = LAST_SEEN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *last = Some(seen);
}

/// What a window is, or [`None`] if it is not one to offer recording.
fn describe(window: HWND) -> Option<ForegroundWindow> {
    if window.is_invalid() {
        return None;
    }
    // SAFETY: `window` is a handle Windows gave us; an invalid one returns
    // false rather than faulting.
    if !unsafe { IsWindowVisible(window) }.as_bool() {
        return None;
    }
    if SHELL_WINDOW_CLASSES.contains(&class_name(window)?.as_str()) {
        return None;
    }

    let mut process_id = 0_u32;
    // SAFETY: `process_id` is a real, writable `u32`, which is the whole of what
    // this call requires of the caller.
    unsafe { GetWindowThreadProcessId(window, Some(&mut process_id)) };
    if process_id == 0 || process_id == std::process::id() {
        return None;
    }

    Some(ForegroundWindow {
        process_id,
        process_name: process_name(process_id)?,
    })
}

/// A window's class name, or [`None`] if Windows would not say.
fn class_name(window: HWND) -> Option<String> {
    // 256 is the documented maximum length of a registered class name.
    let mut buffer = [0_u16; 256];
    // SAFETY: the buffer is a real, writable array and its length is what is
    // passed; the call writes at most that many characters.
    let written = unsafe { GetClassNameW(window, &mut buffer) };
    if written <= 0 {
        return None;
    }

    Some(String::from_utf16_lossy(&buffer[..written as usize]))
}

/// A process's executable file name, such as `cs2.exe`.
///
/// Opened with `PROCESS_QUERY_LIMITED_INFORMATION`, which is the least that
/// answers this question and the only one that works against a process running
/// at a higher integrity level than this one — which many games, and every
/// elevated application, are.
fn process_name(process_id: u32) -> Option<String> {
    // SAFETY: no pointers are passed in, and the returned handle is closed
    // below on every path out.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id) }
        .ok()
        .filter(|handle| !handle.is_invalid())?;

    let mut buffer = [0_u16; MAX_PATH as usize];
    let mut length = buffer.len() as u32;
    // SAFETY: `buffer` outlives the call, `length` describes it correctly, and
    // the call writes at most `length` characters and updates it with how many.
    let queried = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_FORMAT(0),
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
    };
    // SAFETY: `process` is a handle this function opened and has not closed.
    let _ = unsafe { CloseHandle(process) };
    queried.ok()?;

    let path = String::from_utf16_lossy(&buffer[..length as usize]);
    // The file name, because that is what `--process` matches and what a menu
    // can show. The full path is somebody's install location and has no place
    // in a menu label.
    Some(
        path.rsplit(['\\', '/'])
            .next()
            .filter(|name| !name.is_empty())?
            .to_owned(),
    )
}
