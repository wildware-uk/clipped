//! What `foreground_target` answers, against a window this test puts in front.
//!
//! The rules are unit tested in `src/foreground.rs` against written-down
//! windows, where every refusal can actually be arranged. What no unit test can
//! reach is the one line joining them to Windows: `GetForegroundWindow`, and
//! describing what it names. A `foreground_target` that asked for the wrong
//! window — the shell window, the desktop, its own — would pass every test in
//! that module, and the symptom would be a hotkey that refuses to record
//! anything, or records the wrong thing, on a machine nobody is testing on.
//!
//! # Why this one is `#[ignore]`d
//!
//! It takes the foreground, and it synthesises an input event in order to be
//! allowed to. Windows grants `SetForegroundWindow` only to a process that has
//! the foreground already or has just produced input (`docs/testing.md`,
//! `tests/capture/README.md`), so this moves the mouse by zero pixels first.
//! Both of those are things to do to a developer's desktop deliberately and
//! never as a side effect of `cargo test`, and a hosted runner has no
//! compositor to do them on at all.
//!
//! ```powershell
//! cargo test -p clipped-windows --test foreground -- --ignored --nocapture
//! ```
//!
//! A run in which Windows refuses the transition **fails**, saying so. A test
//! that quietly passed without entering the case it is named for is worse than
//! no test at all (AGENTS.md section 54).

use clipped_windows::{foreground_target, ForegroundTarget, WindowHandle};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_MOVE, MOUSEINPUT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetForegroundWindow, RegisterClassW,
    SetForegroundWindow, UnregisterClassW, WINDOW_EX_STYLE, WNDCLASSW, WS_POPUP, WS_VISIBLE,
};

/// Where the fixture window is put: on screen, because a window Windows
/// considers off every display is not a window a user is looking at, and this
/// test is about the window a user is looking at.
const ON_SCREEN: (i32, i32, i32, i32) = (40, 40, 320, 240);

#[test]
#[ignore = "takes the foreground, and synthesises input to be allowed to: docs/testing.md"]
fn the_window_in_front_is_what_would_be_recorded() {
    let title = format!("clipped-windows foreground test {}", std::process::id());
    let fixture = Fixture::create(&title);

    given_the_foreground(fixture.window);

    match foreground_target().expect("Windows answers what has the foreground") {
        ForegroundTarget::Recordable(window) => {
            assert_eq!(
                window.handle(),
                WindowHandle::from_raw(fixture.window.0 as isize),
                "the window offered for recording is not the one in front",
            );
            assert_eq!(
                window.process_id(),
                std::process::id(),
                "and it has to name the process that owns it, because that is what \
                 `start_recording` is given",
            );
            assert_eq!(window.title(), title);
        }
        ForegroundTarget::NothingToRecord(reason) => panic!(
            "an ordinary visible titled window was in front and was refused: {reason}. That is \
             the hotkey refusing to record the game the user is playing",
        ),
    }
}

/// Puts `window` in front, or fails saying that the run did not enter the case
/// it is named for.
fn given_the_foreground(window: HWND) {
    // Windows grants the foreground to a process that has just synthesised
    // input. A zero-pixel mouse movement is the smallest thing that qualifies
    // and moves no pointer anybody would notice.
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: MOUSEEVENTF_MOVE,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    // SAFETY: one initialised `INPUT` is passed by slice, and the size argument
    // is the one Windows requires for it.
    let sent = unsafe {
        SendInput(
            &[input],
            i32::try_from(size_of::<INPUT>()).expect("an INPUT is small"),
        )
    };
    assert_eq!(
        sent, 1,
        "this session would not accept a synthetic input event, so the \
         foreground could not be taken and this run proves nothing"
    );

    // SAFETY: `window` is a live window this process created.
    let _ = unsafe { SetForegroundWindow(window) };

    // SAFETY: takes nothing, and the handle it returns is only compared.
    let in_front = unsafe { GetForegroundWindow() };
    assert_eq!(
        in_front.0, window.0,
        "Windows refused to give this test's window the foreground, so NOTHING WAS EXERCISED: \
         run this from an interactive session, in the foreground, with nothing else grabbing it",
    );
}

/// A real, visible, titled top-level window, destroyed when it is dropped.
struct Fixture {
    window: HWND,
    class_name: Vec<u16>,
    instance: HINSTANCE,
}

impl Fixture {
    fn create(title: &str) -> Self {
        // SAFETY: `None` asks for this process's own module handle, which
        // always exists and needs no release.
        let module = unsafe { GetModuleHandleW(None) }.expect("this process has a module handle");
        let instance = HINSTANCE(module.0);

        let mut class_name = utf16(&format!(
            "clipped-windows-foreground-{}",
            std::process::id()
        ));
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_procedure),
            hInstance: instance,
            lpszClassName: PCWSTR(class_name.as_mut_ptr()),
            ..Default::default()
        };
        // SAFETY: `class` borrows `class_name`, which lives in this struct for
        // as long as the registration does and is unregistered in `Drop` before
        // the buffer is freed.
        let atom = unsafe { RegisterClassW(&raw const class) };
        assert!(atom != 0, "the window class could not be registered");

        let mut window_title = utf16(title);
        // SAFETY: both string pointers are null-terminated UTF-16 buffers that
        // outlive the call; everything else is an integer or a style constant.
        // The window is owned by this fixture and destroyed in `Drop`.
        let window = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                PCWSTR(class_name.as_mut_ptr()),
                PCWSTR(window_title.as_mut_ptr()),
                WS_POPUP | WS_VISIBLE,
                ON_SCREEN.0,
                ON_SCREEN.1,
                ON_SCREEN.2,
                ON_SCREEN.3,
                None,
                None,
                Some(instance),
                None,
            )
        }
        .expect("a top-level window can be created on this desktop");

        Self {
            window,
            class_name,
            instance,
        }
    }
}

impl Drop for Fixture {
    /// Releases both things Windows will not clean up until the process exits,
    /// in the order it requires, so that a failing assertion — which unwinds —
    /// leaves nothing on the desktop (AGENTS.md section 58).
    fn drop(&mut self) {
        // SAFETY: the window was created on this thread, which is what
        // `DestroyWindow` requires, and is destroyed exactly once.
        let _ = unsafe { DestroyWindow(self.window) };
        // SAFETY: the class was registered by this fixture with this instance
        // and this name, and its only window has just been destroyed.
        let _ =
            unsafe { UnregisterClassW(PCWSTR(self.class_name.as_mut_ptr()), Some(self.instance)) };
    }
}

/// Windows' own default behaviour.
///
/// # Safety
///
/// Called only by Windows, with the arguments it documents, and forwards them
/// unchanged.
unsafe extern "system" fn window_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: the arguments are the ones Windows passed, forwarded unchanged.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

fn utf16(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}
