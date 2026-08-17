//! What the user is looking at, and whether it is something to record.
//!
//! A global hotkey carries no target. Pressing "Start or stop recording" with
//! nothing running used to be refused for exactly that reason — "a hotkey does
//! not say which window to record" — while the desktop application answered the
//! same question for its Record button from a foreground hook of its own
//! ([issue #416](https://github.com/wildware-uk/clipped/issues/416)). The
//! recorder has to be able to answer it without a window, because it is the
//! process that starts at login and outlives every window (ADR 0002, ADR 0006),
//! so the answer lives here.
//!
//! # Why a poll, and not a hook
//!
//! `apps/desktop/src-tauri/src/foreground.rs` follows the foreground through
//! `EVENT_SYSTEM_FOREGROUND` and remembers what it saw. It has to: opening a
//! notification-area menu *gives the foreground to the taskbar*, so by the time
//! a menu item is clicked, asking Windows what is in front answers with the
//! shell.
//!
//! A key press does not do that. `RegisterHotKey` with a null window posts
//! `WM_HOTKEY` to a thread's queue and raises nothing, so the window the user
//! was playing in is still the foreground window when the handler runs
//! (`docs/hotkeys.md`, "Threading" — the press reaches a handler in tens of
//! microseconds). The question is therefore asked at the moment it is answered,
//! which buys three things a remembered answer does not have:
//!
//! - **Nothing runs when nothing is pressed.** No hook, no message pump and no
//!   thread in a process that may be running as a service-like `serve` with no
//!   interactive desktop at all. There, [`GetForegroundWindow`] answers null and
//!   this reports [`NotRecordable::Nothing`] rather than failing.
//! - **It cannot be stale.** A remembered window may have closed, and a
//!   remembered process identifier may since name something else entirely.
//! - **It is the truth about the press.** "What was in front when the key went
//!   down" is what the user meant, and it is what this asks.
//!
//! # Looking, then deciding
//!
//! ```text
//! foreground_target()
//!   │
//!   ├── seen()   ── needs Windows ──▶ SeenForeground
//!   │
//!   └── offer(seen)  ── pure, and where the rules live ──▶ ForegroundTarget
//! ```
//!
//! The same split [`crate::enumerate_windows`] and [`crate::resolve`] make, for
//! the same reason: "what is this window?" can only be answered by Windows and
//! has no judgement in it, while "may this one be recorded?" is all judgement
//! and needs no desktop (AGENTS.md section 25).
//!
//! And the looking half is [`crate::window`]'s own, not a second copy of it:
//! the foreground window is described by the same `describe` that
//! [`crate::enumerate_windows`] describes every other window with, so
//! "capturable" means here what it means in a window list and in
//! [`crate::resolve`] — which is what the recorder applies to the process
//! identifier this produces, one moment later (AGENTS.md section 55).
//!
//! # What is deliberately refused
//!
//! Two things beyond the [`Exclusion`]s any window can carry, and both are
//! exclusions rather than a guess at what a game is:
//!
//! - **The shell's own surfaces**, by window class: the taskbar, the
//!   notification overflow, Start, Search and the desktop. Pressing the key
//!   while the Start menu is open should not record the Start menu. A File
//!   Explorer window is `CabinetWClass` and is *not* on the list — somebody may
//!   legitimately want to record one.
//! - **Clipped's own windows.** Recording the Clipped window because somebody
//!   pressed the key while looking at it is worse than refusing, which is what
//!   issue #416 asked this to decide. See [`is_clipped`] for how that is asked,
//!   and why it is asked of the process table rather than of one executable
//!   name.

use core::fmt;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetDesktopWindow, GetForegroundWindow, GetShellWindow,
};

use crate::error::WindowsError;
use crate::process::ProcessNames;
use crate::process_table::{process_table, ProcessTableEntry};
use crate::window::{describe, Exclusion, WindowHandle, WindowInfo};

/// The window classes of the shell's own surfaces.
///
/// Every one of these can have the foreground because the user reached for
/// Windows rather than because they were using an application, and none of them
/// is something to record. `CabinetWClass` — a File Explorer window — is
/// deliberately absent.
///
/// **This list is the desktop application's**, kept in step with
/// `apps/desktop/src-tauri/src/foreground.rs` by
/// `tests/integration/tests/foreground_rules.rs`. The two processes cannot
/// share the code — the window may link no crate of this workspace but
/// `clipped-ipc` (ADR 0002, `tests/integration/tests/workspace_layering.rs`) —
/// so what they share is the list, and a test that fails when one side changes
/// it alone. That is the third acceptance criterion of issue #416: the Record
/// button and the hotkey must agree about what the target is.
pub const SHELL_SURFACE_CLASSES: &[&str] = &[
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "NotifyIconOverflowWindow",
    "TopLevelWindowForOverflowXamlIsland",
    "Windows.UI.Core.CoreWindow",
    "XamlExplorerHostIslandWindow",
    "Progman",
    "WorkerW",
];

/// The executables Clipped ships, as `docs/packaging.md` lists them.
///
/// A window belonging to one of these, or to anything one of them started, is
/// never offered for recording.
pub const CLIPPED_EXECUTABLES: &[&str] = &["clipped-desktop.exe", "clipped-recorder.exe"];

/// What the user was looking at when they pressed the key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForegroundTarget {
    /// A window this recorder can record, described exactly as
    /// [`crate::enumerate_windows`] would have described it.
    Recordable(Box<WindowInfo>),
    /// Nothing to record, and what was in front instead.
    ///
    /// Carried rather than reduced to [`None`], because "nothing happened" is
    /// the whole failure mode of a hotkey: whoever pressed it needs to be told
    /// what it found rather than that it found nothing (AGENTS.md section 15).
    NothingToRecord(NotRecordable),
}

/// Why what was in front is not something to record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotRecordable {
    /// No window has the foreground at all.
    ///
    /// A locked workstation, a session with no interactive desktop, and the
    /// moment after the last window on a desktop closes.
    Nothing,

    /// One of the shell's own surfaces: the taskbar, Start, Search, the
    /// desktop.
    ShellSurface {
        /// The window class it was recognised by.
        class: String,
    },

    /// Clipped's own window, or one drawn by a process Clipped started.
    Clipped {
        /// The executable that owns it, such as `clipped-desktop.exe`.
        process_name: String,
    },

    /// Windows names no process for the window, so there is nothing to record
    /// *as*.
    NoProcess,

    /// A window like any other, that this recorder cannot capture.
    NotCapturable {
        /// The executable that owns it, if Windows would say.
        process_name: Option<String>,
        /// The first reason it cannot be captured.
        exclusion: Exclusion,
    },
}

impl NotRecordable {
    /// What was there instead, named by its executable.
    ///
    /// The executable and never the window title: a title is user content and
    /// the surest way to put somebody's document name into a log line
    /// (AGENTS.md section 13). [`WindowInfo::title`] is available to a caller
    /// that has a real reason for it; a refusal does not.
    fn named(process_name: Option<&str>) -> String {
        process_name.map_or_else(
            || "an application Windows would not name".to_owned(),
            |name| format!("`{name}`"),
        )
    }
}

impl fmt::Display for NotRecordable {
    /// The sentence a refusal is made of, saying what was found rather than
    /// that something failed (AGENTS.md section 45).
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Nothing => {
                formatter.write_str("nothing is in front: no window has the foreground")
            }
            Self::ShellSurface { class } => write!(
                formatter,
                "what is in front is part of Windows itself — the taskbar, Start, Search or the \
                 desktop (`{class}`)"
            ),
            Self::Clipped { process_name } => write!(
                formatter,
                "what is in front is Clipped's own window (`{process_name}`)"
            ),
            Self::NoProcess => {
                formatter.write_str("Windows names no process for the window that is in front")
            }
            Self::NotCapturable {
                process_name,
                exclusion,
            } => write!(
                formatter,
                // The exclusion's own words, which are written to follow a
                // colon like this one: "hidden, so it has nothing to capture".
                "the window in front, {}, cannot be recorded: {}",
                Self::named(process_name.as_deref()),
                exclusion.explanation()
            ),
        }
    }
}

/// What Clipped would record if it were asked right now.
///
/// Asks Windows what has the foreground, describes it the way a window listing
/// would, and applies the rules above. A refusal is a value rather than an
/// error: there being nothing sensible in front is an ordinary state of a
/// desktop, not a failure of this call (AGENTS.md section 16).
///
/// # Errors
///
/// [`WindowsError`] when Windows would not describe a window it has just named
/// as the foreground one. A window that disappeared between the two calls is
/// not that: it reports [`NotRecordable::Nothing`], because by then nothing
/// *is* in front.
///
/// # Cost
///
/// One `GetForegroundWindow`, the handful of syscalls
/// [`crate::enumerate_windows`] spends on a single window, and — only if the
/// window is not already refused — one read of the process table, which is a
/// `CreateToolhelp32Snapshot` of a few hundred rows. That is paid on a key
/// press and nowhere else. Nothing here may be called from a capture thread
/// (AGENTS.md section 20).
pub fn foreground_target() -> Result<ForegroundTarget, WindowsError> {
    // SAFETY: takes no arguments, and the handle it returns is only passed back
    // to Windows or compared. It is null when no window has the foreground,
    // which `seen` checks for.
    let window = unsafe { GetForegroundWindow() };

    Ok(match seen(window)? {
        None => ForegroundTarget::NothingToRecord(NotRecordable::Nothing),
        Some(seen) => offer(seen),
    })
}

/// Everything Windows will say about the window in front.
///
/// Gathered before anything is decided, so that [`offer`] is a function of a
/// class name, a described window and a boolean rather than of a desktop.
///
/// `#[must_use]` for the reason the desktop application's equivalent carries
/// it: a described window that is then not decided about compiles perfectly and
/// leaves the hotkey with nothing to record for anything, and no test here can
/// catch it — the one window a test can be sure of is one of this process's
/// own, which is exactly what the rules refuse.
#[must_use]
#[derive(Debug, Clone, PartialEq, Eq)]
struct SeenForeground {
    /// Its window class, which is how the shell's own surfaces are recognised.
    class: String,
    /// Whether Clipped drew it: the desktop application, this recorder, or
    /// anything either of them started.
    clipped: bool,
    /// Everything [`crate::enumerate_windows`] would have said about it.
    window: WindowInfo,
}

/// Reads what Windows knows about the foreground window.
///
/// Everything needing a desktop is here and no rule is: a window the rules will
/// refuse is described just the same, and refused by [`offer`].
///
/// [`None`] when nothing has the foreground, or when the window went away
/// between being named and being measured.
fn seen(window: HWND) -> Result<Option<SeenForeground>, WindowsError> {
    if window.is_invalid() {
        return Ok(None);
    }

    // The same three arguments `enumerate_windows` passes, so that a foreground
    // window reads exactly as it would read in a window listing.
    //
    // SAFETY: neither call takes an argument, and both return handles that are
    // only ever compared here.
    let shell = unsafe { GetShellWindow() };
    // SAFETY: as above.
    let desktop = unsafe { GetDesktopWindow() };

    let Some(described) = describe(window, shell, desktop, &mut ProcessNames::default())? else {
        return Ok(None);
    };

    let class = class_name(WindowHandle::from_hwnd(window)).unwrap_or_default();
    let clipped = is_clipped(&described, &process_table().unwrap_or_default());

    Ok(Some(SeenForeground {
        class,
        clipped,
        window: described,
    }))
}

/// Whether a described window is one to record, and what it is if it is not.
///
/// Every rule is here, which is what makes each of them testable against a
/// written-down window.
///
/// The order is by how much it tells whoever pressed the key. Clipped's own
/// window and a shell surface are ordinary, capturable windows — nothing in
/// [`Exclusion`] would refuse either — so the two would otherwise be reported
/// as "recordable", which is the bug. Between the remaining reasons, the
/// specific one comes first.
fn offer(seen: SeenForeground) -> ForegroundTarget {
    if seen.clipped {
        return ForegroundTarget::NothingToRecord(NotRecordable::Clipped {
            // A window this rule refuses is one Clipped drew, and Clipped can
            // always name its own executables; the fallback is for the case
            // where the *process table* named it and `OpenProcess` would not.
            process_name: seen.window.process_name().unwrap_or("Clipped").to_owned(),
        });
    }

    if SHELL_SURFACE_CLASSES.contains(&seen.class.as_str()) {
        return ForegroundTarget::NothingToRecord(NotRecordable::ShellSurface {
            class: seen.class,
        });
    }

    if let Some(exclusion) = seen.window.exclusion() {
        return ForegroundTarget::NothingToRecord(NotRecordable::NotCapturable {
            process_name: seen.window.process_name().map(ToOwned::to_owned),
            exclusion,
        });
    }

    // Zero is what Windows answers for a window that has stopped naming one,
    // and a recording scoped to process 0 would be scoped to the System Idle
    // Process (issue #26).
    if seen.window.process_id() == 0 {
        return ForegroundTarget::NothingToRecord(NotRecordable::NoProcess);
    }

    ForegroundTarget::Recordable(Box::new(seen.window))
}

/// Whether Clipped drew this window: the desktop application, this recorder, or
/// anything either of them started.
///
/// # Why the process table, and not one executable name
///
/// Clipped's interface is drawn by WebView2, in `msedgewebview2.exe` processes
/// the desktop application starts, and those have top-level windows of their
/// own — the developer tools among them. Matching the window's own executable
/// left the desktop application's record control offering
/// **`Start recording msedgewebview2.exe`**
/// ([issue #390](https://github.com/wildware-uk/clipped/issues/390)), and
/// matching `msedgewebview2.exe` by name instead would refuse Teams, the
/// widgets board and every other application that hosts a webview. So the
/// question is asked of parentage: is the window's process, or anything that
/// started it, one of Clipped's own executables.
///
/// # What this cannot do, and the desktop application can
///
/// The window's own process asks `this_application::includes`, which walks
/// *descent from itself* and defends against identifier reuse with each
/// process's creation time. From here Clipped is another process — often
/// started by the shell at sign-in, not by this one — so it is recognised by
/// the executables it ships as (`docs/packaging.md`). The two ways this is
/// weaker are worth stating:
///
/// - Another application whose executable is literally named
///   `clipped-desktop.exe` would not be offered.
/// - `parent_pid` is a number Windows reuses, so a webview whose parent has
///   exited can be traced to whatever holds that identifier now. The cost of
///   either going wrong is one refusal, said out loud, with the executable
///   named in it — not a silent recording of the wrong thing.
fn is_clipped(window: &WindowInfo, process_table: &[ProcessTableEntry]) -> bool {
    if window.process_name().is_some_and(is_clipped_executable) {
        return true;
    }

    // Deliberately reached even when the table could not be read, in which case
    // it is empty and the answer is the line above: a Clipped window is still
    // refused, and only the webview host it started stops being recognised.
    let mut process = window.process_id();
    // The table is a few hundred rows, so a chain longer than that is a cycle
    // rather than a family: `parent_pid` names a process that may have exited,
    // and an identifier Windows has reused can point back down the chain.
    let mut remaining = process_table.len();
    while remaining > 0 {
        remaining -= 1;
        let Some(entry) = process_table.iter().find(|entry| entry.pid() == process) else {
            return false;
        };
        if is_clipped_executable(entry.name()) {
            return true;
        }
        if entry.parent_pid() == 0 || entry.parent_pid() == entry.pid() {
            return false;
        }
        process = entry.parent_pid();
    }

    false
}

/// Whether an executable file name is one of Clipped's.
///
/// Case-insensitively: Windows file names are, and the process table reports
/// whatever case the file has on disk.
fn is_clipped_executable(name: &str) -> bool {
    CLIPPED_EXECUTABLES
        .iter()
        .any(|ours| ours.eq_ignore_ascii_case(name))
}

/// A window's class name, or [`None`] if Windows would not say.
fn class_name(window: WindowHandle) -> Option<String> {
    // 256 is the documented maximum length of a registered class name.
    let mut buffer = [0_u16; 256];
    // SAFETY: the buffer is a real, writable array and its length is what the
    // call is given; it writes at most that many characters.
    let written = unsafe { GetClassNameW(window.to_hwnd(), &mut buffer) };
    if written <= 0 {
        return None;
    }

    Some(String::from_utf16_lossy(&buffer[..written as usize]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{PixelSize, DEFAULT_DPI};
    use crate::monitor::MonitorHandle;
    use crate::window::WindowGeometry;

    /// A window of another application, on screen, that this recorder can
    /// capture.
    fn a_game_window(process_id: u32, process_name: &str) -> WindowInfo {
        WindowInfo::new(
            WindowHandle::from_raw(0x1234),
            "Counter-Strike 2".to_owned(),
            process_id,
            Some(process_name.to_owned()),
            WindowGeometry::new(
                PixelSize::new(2560, 1440),
                DEFAULT_DPI,
                MonitorHandle::from_raw(1),
            ),
            false,
            None,
        )
    }

    fn a_game() -> SeenForeground {
        SeenForeground {
            class: "SDL_app".to_owned(),
            clipped: false,
            window: a_game_window(4_242, "cs2.exe"),
        }
    }

    /// The pid a refusal or an offer is about, so that a test can say which
    /// window came back.
    fn offered(seen: SeenForeground) -> Result<u32, NotRecordable> {
        match offer(seen) {
            ForegroundTarget::Recordable(window) => Ok(window.process_id()),
            ForegroundTarget::NothingToRecord(reason) => Err(reason),
        }
    }

    #[test]
    fn the_window_the_user_is_playing_in_is_what_would_be_recorded() {
        assert_eq!(offered(a_game()), Ok(4_242));
    }

    #[test]
    fn a_file_explorer_window_is_recordable() {
        // The list is of shell *surfaces*, not of Explorer. A File Explorer
        // window is an ordinary window somebody may want to record, and issue
        // #416 asks for that decision explicitly.
        let explorer = SeenForeground {
            class: "CabinetWClass".to_owned(),
            window: a_game_window(9_001, "explorer.exe"),
            ..a_game()
        };

        assert_eq!(offered(explorer), Ok(9_001));
    }

    #[test]
    fn the_shells_own_surfaces_are_refused_by_class() {
        for class in SHELL_SURFACE_CLASSES {
            let surface = SeenForeground {
                class: (*class).to_owned(),
                ..a_game()
            };

            assert_eq!(
                offered(surface),
                Err(NotRecordable::ShellSurface {
                    class: (*class).to_owned()
                }),
                "{class} would have been recorded",
            );
        }
    }

    #[test]
    fn clippeds_own_window_is_refused_and_named() {
        // Issue #416: "Recording the Clipped window because somebody pressed
        // the key while looking at it is worse than refusing." It is an
        // ordinary, capturable window, so nothing else here would refuse it.
        let ours = SeenForeground {
            class: "Tauri Window".to_owned(),
            clipped: true,
            window: a_game_window(7_100, "clipped-desktop.exe"),
        };

        assert_eq!(
            offered(ours),
            Err(NotRecordable::Clipped {
                process_name: "clipped-desktop.exe".to_owned()
            })
        );
    }

    #[test]
    fn a_window_that_cannot_be_captured_is_refused_with_the_reason_a_window_list_would_give() {
        let hidden = SeenForeground {
            window: WindowInfo::new(
                WindowHandle::from_raw(0x1234),
                String::new(),
                4_242,
                Some("cs2.exe".to_owned()),
                WindowGeometry::new(
                    PixelSize::new(2560, 1440),
                    DEFAULT_DPI,
                    MonitorHandle::from_raw(1),
                ),
                false,
                Some(Exclusion::ContentProtected),
            ),
            ..a_game()
        };

        assert_eq!(
            offered(hidden),
            Err(NotRecordable::NotCapturable {
                process_name: Some("cs2.exe".to_owned()),
                exclusion: Exclusion::ContentProtected,
            })
        );
    }

    #[test]
    fn a_window_windows_has_no_process_for_is_refused() {
        // `start_recording` takes a process identifier, and zero names the
        // System Idle Process rather than nothing (issue #26).
        let orphan = SeenForeground {
            window: a_game_window(0, "cs2.exe"),
            ..a_game()
        };

        assert_eq!(offered(orphan), Err(NotRecordable::NoProcess));
    }

    /// Every refusal has to say what was found, because a hotkey that does
    /// nothing and says nothing is the failure this whole module is about
    /// (issue #416's second acceptance criterion).
    #[test]
    fn every_refusal_says_what_was_in_front_instead() {
        // Each refusal, and the words in it somebody could act on. A `Display`
        // that answered "no target" for all of them would satisfy every other
        // test in this file.
        let refusals = [
            (NotRecordable::Nothing, "no window has the foreground"),
            (
                NotRecordable::ShellSurface {
                    class: "Shell_TrayWnd".to_owned(),
                },
                "taskbar",
            ),
            (
                NotRecordable::Clipped {
                    process_name: "clipped-desktop.exe".to_owned(),
                },
                "clipped-desktop.exe",
            ),
            (NotRecordable::NoProcess, "no process"),
            (
                // Which window: naming it is what makes the refusal something
                // a user can act on.
                NotRecordable::NotCapturable {
                    process_name: Some("cs2.exe".to_owned()),
                    exclusion: Exclusion::ContentProtected,
                },
                "cs2.exe",
            ),
            (
                // And what is wrong with it, in the words a window listing
                // gives for the same window.
                NotRecordable::NotCapturable {
                    process_name: None,
                    exclusion: Exclusion::Cloaked,
                },
                "another virtual desktop",
            ),
        ];

        for (refusal, expected) in refusals {
            let sentence = refusal.to_string();
            assert!(
                sentence.contains(expected),
                "a refusal has to say what was in front: expected {expected:?} in {sentence:?}",
            );
            assert!(
                !sentence.contains("Counter-Strike 2"),
                "and never the window's title, which is user content (AGENTS.md section 13): \
                 {sentence}",
            );
        }
    }

    /// A process table shaped like a machine running Clipped beside a game.
    ///
    /// `explorer.exe` starts the desktop application at sign-in, the desktop
    /// application starts its webview host, and the game is nothing to do with
    /// any of them.
    fn a_machine_running_clipped() -> Vec<ProcessTableEntry> {
        vec![
            ProcessTableEntry::for_test(4, 0, "System"),
            ProcessTableEntry::for_test(900, 4, "explorer.exe"),
            ProcessTableEntry::for_test(7_100, 900, "clipped-desktop.exe"),
            ProcessTableEntry::for_test(7_200, 7_100, "msedgewebview2.exe"),
            ProcessTableEntry::for_test(7_300, 7_200, "msedgewebview2.exe"),
            ProcessTableEntry::for_test(1_500, 4, "clipped-recorder.exe"),
            ProcessTableEntry::for_test(4_242, 900, "cs2.exe"),
            ProcessTableEntry::for_test(5_000, 900, "Teams.exe"),
            ProcessTableEntry::for_test(5_001, 5_000, "msedgewebview2.exe"),
        ]
    }

    #[test]
    fn a_game_is_not_clipped() {
        assert!(!is_clipped(
            &a_game_window(4_242, "cs2.exe"),
            &a_machine_running_clipped()
        ));
    }

    #[test]
    fn the_desktop_applications_own_window_is_clipped() {
        assert!(is_clipped(
            &a_game_window(7_100, "clipped-desktop.exe"),
            &a_machine_running_clipped()
        ));
    }

    #[test]
    fn the_webview_drawing_clippeds_interface_is_clipped() {
        // Issue #390's reported symptom, asked from the recorder's side: the
        // developer tools are a top-level, visible window belonging to
        // `msedgewebview2.exe`, and the only thing about that process which
        // says Clipped is that Clipped started it. Two generations down,
        // because WebView2 starts renderers of its own.
        assert!(is_clipped(
            &a_game_window(7_200, "msedgewebview2.exe"),
            &a_machine_running_clipped()
        ));
        assert!(is_clipped(
            &a_game_window(7_300, "msedgewebview2.exe"),
            &a_machine_running_clipped()
        ));
    }

    #[test]
    fn another_applications_webview_is_not_clipped() {
        // The same executable, a different application. An exclusion by name
        // would have taken Teams, the widgets board and every other Tauri
        // application with it, which is a second bug in place of the first.
        assert!(!is_clipped(
            &a_game_window(5_001, "msedgewebview2.exe"),
            &a_machine_running_clipped()
        ));
    }

    #[test]
    fn the_shell_that_started_clipped_is_not_clipped() {
        // Descent, not ancestry in both directions: Explorer starts the desktop
        // application at sign-in, and a rule that read that the other way round
        // would refuse to record File Explorer, and on a machine where Clipped
        // was launched from a terminal, that terminal.
        assert!(!is_clipped(
            &a_game_window(900, "explorer.exe"),
            &a_machine_running_clipped()
        ));
    }

    #[test]
    fn a_clipped_window_is_still_recognised_when_the_process_table_could_not_be_read() {
        // What `seen` does with a `CreateToolhelp32Snapshot` that failed: the
        // table is empty, and the window's own executable is still checked. The
        // webview host is what stops being recognised, which is the degradation
        // this arm is a decision about rather than an oversight.
        assert!(is_clipped(
            &a_game_window(7_100, "clipped-desktop.exe"),
            &[]
        ));
        assert!(!is_clipped(
            &a_game_window(7_200, "msedgewebview2.exe"),
            &[]
        ));
    }

    #[test]
    fn a_process_table_that_points_at_itself_is_not_walked_for_ever() {
        // `parent_pid` is a number Windows reuses, so a chain that loops is a
        // thing a real machine can produce. A walk that did not bound itself
        // would hang the handler thread of a hotkey press.
        let looping = vec![
            ProcessTableEntry::for_test(10, 11, "a.exe"),
            ProcessTableEntry::for_test(11, 10, "b.exe"),
        ];

        assert!(!is_clipped(&a_game_window(10, "a.exe"), &looping));
    }

    #[test]
    fn clippeds_executables_are_matched_whatever_case_they_are_written_in() {
        assert!(is_clipped(
            &a_game_window(7_100, "Clipped-Desktop.exe"),
            &[ProcessTableEntry::for_test(7_100, 0, "Clipped-Desktop.exe")]
        ));
    }

    /// The two fields no written-down window reaches, on a window that is on
    /// every machine.
    ///
    /// Both are read by [`seen`] and used by [`offer`], and neither has a
    /// caller that would notice it going wrong:
    ///
    /// - **The class.** Left empty — read from the wrong handle, or dropped —
    ///   every shell surface becomes recordable, because `""` is in no list.
    ///   The taskbar is then what a press records, and every test above still
    ///   passes.
    /// - **Whether Clipped drew it.** A constant `true` refuses every window on
    ///   the machine and the hotkey records nothing at all; a constant `false`
    ///   offers Clipped's own window, which is the thing issue #416 says is
    ///   worse than refusing.
    ///
    /// The desktop window is the one window that can be asked about without
    /// creating one: it exists in every session, needs no display, and belongs
    /// to a system process started at boot — which is neither this process nor
    /// anything Clipped could have started.
    #[test]
    fn the_class_and_the_owner_are_read_from_the_window_that_is_in_front() {
        // SAFETY: takes nothing, and the handle it returns is only passed back
        // to Windows.
        let desktop = unsafe { GetDesktopWindow() };

        let seen = seen(desktop)
            .expect("Windows describes the desktop window")
            .expect("the desktop window has not gone");

        assert!(
            !seen.class.is_empty(),
            "the window class was not read, so no shell surface can be recognised",
        );
        assert_ne!(
            seen.window.process_id(),
            0,
            "the desktop window has an owning process, which is what makes the next line a real \
             question",
        );
        assert!(
            !seen.clipped,
            "a window belonging to a system process was read as Clipped's own, so nothing on this \
             machine would be offered for recording",
        );
    }

    /// The one line no written-down window reaches: asking Windows what is in
    /// front, and describing it.
    ///
    /// It asserts no particular answer, because what has the foreground on a
    /// machine running a test suite is not something a test may decide. What it
    /// does assert is that the call completes and that whatever comes back is
    /// self-consistent — a recordable answer names a process, and a refusal
    /// says something. A `foreground_target` that returned an error on an
    /// ordinary desktop, or a "recordable" window belonging to process 0, would
    /// fail here and nowhere else.
    #[test]
    fn asking_windows_what_is_in_front_answers_something_usable() {
        let target = foreground_target().expect("Windows answers what has the foreground");

        match target {
            ForegroundTarget::Recordable(window) => {
                assert_ne!(
                    window.process_id(),
                    0,
                    "a window offered for recording has to name a process to record",
                );
                assert!(
                    window.exclusion().is_none(),
                    "a window offered for recording must be one a window listing would offer",
                );
            }
            ForegroundTarget::NothingToRecord(reason) => {
                assert!(
                    !reason.to_string().is_empty(),
                    "a refusal has to say what was in front",
                );
            }
        }
    }
}
