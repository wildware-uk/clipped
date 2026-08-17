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
//! - **Clipped's own windows**, which is not the same as this process's own
//!   windows and is why [`this_application`](crate::this_application) exists.
//!   Recording Clipped would be absurd, and clicking the tray is a foreground
//!   change to this process — but the interface *inside* the window is drawn by
//!   WebView2, in `msedgewebview2.exe` processes this one starts, and those have
//!   windows of their own. The developer tools are a top-level, visible window
//!   belonging to the webview host, so raising them used to leave the record
//!   control offering `msedgewebview2.exe`
//!   ([issue #390](https://github.com/wildware-uk/clipped/issues/390)). The
//!   exclusion is therefore "this process, or a process it started", asked of
//!   the process table rather than of an executable's name: another
//!   application that happens to host a webview is recordable like anything
//!   else.
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
//! # Looking, then deciding
//!
//! ```text
//! describe(window)
//!   │
//!   ├── look_at(window)  ── needs Windows ──▶  SeenWindow
//!   │
//!   └── offer(seen)      ── decides, and names what it accepts ──▶ ForegroundWindow
//!         │
//!         └── worth_offering(&seen)  ── pure, and where the rules live
//! ```
//!
//! The split is the same one `clipped_windows` makes between enumerating
//! windows and resolving one, and for the same reason: "what is this window?"
//! can only be answered by Windows and has no judgement in it, while "may this
//! one be offered?" is all judgement and needs no desktop — so it is a function
//! over written-down windows and is tested as one (AGENTS.md section 25).
//!
//! [`offer`] is the deciding half taken as far as it goes — up to and including
//! the refusal — because a rule that passes its own test and is never asked is
//! no rule at all. Each of [`worth_offering`]'s is tested on a written-down
//! window, and none of those tests would notice [`offer`] not consulting it;
//! what would follow is [issue #390]'s reported symptom, the record control
//! reading *Start recording msedgewebview2.exe* again. So the refusal is
//! asserted where it happens, on a window [`look_at`] never had to see.
//!
//! [issue #390]: https://github.com/wildware-uk/clipped/issues/390
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

use crate::this_application;

/// The window classes of the shell's own surfaces.
///
/// Every one of these can take the foreground because the user reached for
/// Clipped rather than because they were using it. `Shell_TrayWnd` is the
/// taskbar, which is what the tray icon lives in, so it is the one that matters;
/// the rest are the surfaces beside it. `CabinetWClass` — a File Explorer window
/// — is deliberately absent.
///
/// **The recorder has this list too**, as
/// `clipped_windows::SHELL_SURFACE_CLASSES`: the "Start or stop recording"
/// hotkey has to answer the same question in a process that may have no window
/// open at all ([issue
/// #416](https://github.com/wildware-uk/clipped/issues/416)), and this
/// application may not link the crate that answers it there (ADR 0002). The two
/// copies are kept in step by `tests/integration/tests/foreground_rules.rs`: an
/// entry added here and not there leaves the button and the key offering to
/// record different things, which is invisible to every other test.
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
///
/// Looking and deciding, and nothing of its own: one expression, so that
/// neither half can be dropped without the other failing to compile.
fn describe(window: HWND) -> Option<ForegroundWindow> {
    offer(look_at(window)?)
}

/// How the tray would name a window Windows has described, or [`None`] if the
/// rules refuse it.
///
/// The deciding half of [`describe`], which needs no window of its own —
/// [`look_at`] has already asked Windows everything — so the refusal
/// [issue #390](https://github.com/wildware-uk/clipped/issues/390) is about can
/// be asserted here, at the point it actually happens, rather than only on
/// [`worth_offering`] one call below.
///
/// Naming comes after the rules and not before, so that a window Clipped will
/// not offer is never opened to ask what it is called.
fn offer(seen: SeenWindow) -> Option<ForegroundWindow> {
    if !worth_offering(&seen) {
        return None;
    }

    Some(ForegroundWindow {
        process_id: seen.process_id,
        process_name: process_name(seen.process_id)?,
    })
}

/// What Windows says about a window a foreground change named.
///
/// Gathered before anything is decided, so that [`worth_offering`] is a
/// function of a flag, a string and a number rather than of a desktop.
///
/// `#[must_use]` because a described window that is not then decided about is
/// the whole of the bug: [`describe`] reaching Windows and dropping the answer
/// compiles perfectly and leaves the tray with nothing to offer for anything.
/// No test here can catch that — a visible window belonging to some other
/// application is the one thing a test cannot conjure, and this process's own
/// windows are exactly the ones the rules refuse — and in a **test** build the
/// dead-code lint cannot either, because the tests below keep every function
/// in this file alive. The compiler can, so it is asked to.
#[must_use]
#[derive(Clone, Debug, PartialEq, Eq)]
struct SeenWindow {
    /// Whether it is on screen at all.
    visible: bool,
    /// Its window class, which is how the shell's own surfaces are told apart
    /// from applications.
    class: String,
    /// The process that owns it, or zero if Windows has none for it.
    process_id: u32,
    /// Whether that process is Clipped: this process, or one it started.
    ///
    /// Two processes rather than one, and the second is the point. Clipped's
    /// interface is drawn by WebView2, in `msedgewebview2.exe`, which this
    /// process starts and which has top-level windows of its own — the
    /// developer tools among them
    /// ([issue #390](https://github.com/wildware-uk/clipped/issues/390)).
    this_application: bool,
}

/// Reads what Windows knows about `window`.
///
/// Everything that needs a desktop is here, and no rule is: a window this
/// module will refuse is described just the same, and refused by
/// [`worth_offering`].
fn look_at(window: HWND) -> Option<SeenWindow> {
    if window.is_invalid() {
        return None;
    }

    let mut process_id = 0_u32;
    // SAFETY: `process_id` is a real, writable `u32`, which is the whole of what
    // this call requires of the caller.
    unsafe { GetWindowThreadProcessId(window, Some(&mut process_id)) };

    Some(SeenWindow {
        // SAFETY: `window` is a handle Windows gave us; an invalid one returns
        // false rather than faulting.
        visible: unsafe { IsWindowVisible(window) }.as_bool(),
        class: class_name(window)?,
        process_id,
        this_application: this_application::includes(process_id),
    })
}

/// Whether a window is one to offer for recording.
///
/// Every rule is here, which is what makes each of them testable: a window that
/// is not on screen, one of the shell's own surfaces, one Windows has no
/// process for, and — the exclusion
/// [issue #390](https://github.com/wildware-uk/clipped/issues/390) is about —
/// any window belonging to Clipped itself, whichever of its processes drew it.
fn worth_offering(seen: &SeenWindow) -> bool {
    seen.visible
        && seen.process_id != 0
        && !seen.this_application
        && !SHELL_WINDOW_CLASSES.contains(&seen.class.as_str())
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

#[cfg(test)]
mod tests {
    use windows::Win32::UI::WindowsAndMessaging::GetDesktopWindow;

    use super::*;

    /// A window of another application, on screen, owned by a process that is
    /// nothing to do with Clipped.
    fn a_game() -> SeenWindow {
        SeenWindow {
            visible: true,
            class: "SDL_app".to_owned(),
            process_id: 4_242,
            this_application: false,
        }
    }

    #[test]
    fn an_application_window_is_offered() {
        assert!(worth_offering(&a_game()));
    }

    #[test]
    fn clippeds_own_webview_is_not_offered() {
        // Issue #390. The developer tools are a top-level, visible window with
        // a class of their own, belonging to `msedgewebview2.exe` — a process
        // this one started, which is the only thing about it that says Clipped.
        // Raising them used to leave the record button reading "Start recording
        // msedgewebview2.exe", and pressing it recorded Clipped.
        let devtools = SeenWindow {
            class: "Chrome_WidgetWin_1".to_owned(),
            process_id: 53_008,
            this_application: true,
            ..a_game()
        };

        assert!(!worth_offering(&devtools));
    }

    #[test]
    fn another_applications_webview_is_still_offered() {
        // The same window class, the same executable, and a different
        // application: Teams, the widgets board, somebody else's Tauri
        // application. An exclusion by name would have taken these with it, and
        // a user may legitimately want to record one.
        let theirs = SeenWindow {
            class: "Chrome_WidgetWin_1".to_owned(),
            process_id: 28_844,
            this_application: false,
            ..a_game()
        };

        assert!(worth_offering(&theirs));
    }

    #[test]
    fn clippeds_own_window_is_not_offered() {
        // The window this process draws itself. Clicking the tray is a
        // foreground change to it.
        let ours = SeenWindow {
            class: "Tauri Window".to_owned(),
            this_application: true,
            ..a_game()
        };

        assert!(!worth_offering(&ours));
    }

    #[test]
    fn the_shells_own_surfaces_are_not_offered() {
        // Opening the tray menu raises the taskbar, so without this the answer
        // would be `explorer.exe` every time the menu was used.
        for class in SHELL_WINDOW_CLASSES {
            let surface = SeenWindow {
                class: (*class).to_owned(),
                ..a_game()
            };

            assert!(!worth_offering(&surface), "{class} was offered");
        }
    }

    #[test]
    fn a_file_explorer_window_is_offered() {
        // The list above is of shell *surfaces*, not of Explorer. A File
        // Explorer window is an ordinary window somebody may want to record.
        let explorer = SeenWindow {
            class: "CabinetWClass".to_owned(),
            ..a_game()
        };

        assert!(worth_offering(&explorer));
    }

    #[test]
    fn a_window_that_is_not_on_screen_is_not_offered() {
        let hidden = SeenWindow {
            visible: false,
            ..a_game()
        };

        assert!(!worth_offering(&hidden));
    }

    #[test]
    fn a_window_windows_has_no_process_for_is_not_offered() {
        // `start_recording` takes a process identifier, and zero names nothing.
        let orphan = SeenWindow {
            process_id: 0,
            ..a_game()
        };

        assert!(!worth_offering(&orphan));
    }

    /// A window carrying an identifier `process_name` genuinely answers for.
    ///
    /// This is the whole difficulty of testing [`offer`], and the reason the
    /// two tests below share one helper. `offer` ends in
    /// `process_name(seen.process_id)?`, so a made-up identifier makes it
    /// answer [`None`] whatever the rules decide — a refusal that proves
    /// nothing, and passes just as happily with [`worth_offering`] deleted from
    /// the call site.
    ///
    /// This process's own identifier is the one a test can be certain of. It
    /// needs no window, no fixture and no other application: a process can
    /// always be opened for `PROCESS_QUERY_LIMITED_INFORMATION` against
    /// itself, on any machine and in a session with no desktop at all
    /// (AGENTS.md section 25). The name is returned alongside so that a failure
    /// can say what was offered.
    fn a_process_that_can_be_named() -> (SeenWindow, String) {
        let process_id = std::process::id();
        let name = process_name(process_id)
            .expect("a process can always name itself, which is what makes this test honest");

        (
            SeenWindow {
                process_id,
                ..a_game()
            },
            name,
        )
    }

    #[test]
    fn a_window_the_rules_refuse_is_not_named_even_though_naming_it_would_have_worked() {
        // Issue #390, asserted where the refusal happens rather than one call
        // below it. Every rule in `worth_offering` is tested on a written-down
        // window, and not one of those tests can tell whether anything asks it.
        // A call site that consulted the rules and threw the answer away —
        // `let _ = worth_offering(&seen);` — used to leave the whole suite
        // green and clippy `-D warnings` clean, while the record control read
        // "Start recording msedgewebview2.exe" again, which is the bug as it
        // was reported. Deleting the call outright is the one shape dead code
        // happens to catch, and only because `worth_offering` has no second
        // caller to keep it alive.
        //
        // This process stands in for the webview host. `worth_offering`
        // refuses both for the same reason and by the same field, and unlike
        // the webview host it is here in every test run. That it can be named
        // is the point: the `None` below is the rule's doing and can be
        // nothing else's.
        let (seen, name) = a_process_that_can_be_named();
        let ours = SeenWindow {
            class: "Chrome_WidgetWin_1".to_owned(),
            this_application: true,
            ..seen
        };

        assert_eq!(
            offer(ours),
            None,
            "a window of Clipped's own was offered, as {name}"
        );
    }

    #[test]
    fn a_window_the_rules_accept_is_named_by_the_process_that_owns_it() {
        // The other half of the pair, and what makes the refusal above mean
        // something: the same identifier, the same everything, and one field
        // different. An `offer` that answered `None` to everything would refuse
        // Clipped's window for entirely the wrong reason and leave the tray
        // with nothing to record, so the difference between these two tests has
        // to be the rule and not the process.
        let (seen, name) = a_process_that_can_be_named();
        let process_id = seen.process_id;

        assert_eq!(
            offer(seen),
            Some(ForegroundWindow {
                process_id,
                process_name: name,
            })
        );
    }

    #[test]
    fn a_window_this_application_did_not_draw_is_read_as_somebody_elses_and_offered() {
        // The one line none of the rules above reaches: `look_at` asking
        // `this_application::includes` about *the window's* process. Nothing
        // in this file would notice a miswiring there, and the consequence is
        // not a cosmetic one — a constant `true`, a negation, or this process's
        // identifier passed in place of the window's marks every window as
        // Clipped's own, `worth_offering` then refuses all of them, and the
        // record control has nothing to offer for anything the user does. It
        // is the primary control on the Home screen, so it would be dead in
        // the shipped application while the whole suite and clippy stayed
        // green.
        //
        // The desktop window stands in for "a window Clipped did not draw",
        // and it is the one window that can be asked for without opening one:
        // it exists in every session, needs no display, and belongs to a system
        // process started at boot — which is neither this process nor anything
        // this process could have started.
        //
        // SAFETY: takes nothing, and the handle it returns is only ever passed
        // back to Windows.
        let desktop = unsafe { GetDesktopWindow() };
        let seen = look_at(desktop).expect("Windows describes the desktop window");

        assert_ne!(
            seen.process_id, 0,
            "the desktop window has an owning process, which is what makes this a real question"
        );
        assert_ne!(
            seen.process_id,
            std::process::id(),
            "and it is not this process, so the answer below is about the window's process"
        );
        assert!(
            !seen.this_application,
            "a window belonging to a system process is not Clipped"
        );
        assert!(
            worth_offering(&seen),
            "so the record control is still able to offer something"
        );
    }
}
