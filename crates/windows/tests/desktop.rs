//! Enumeration against the real Windows API, using windows this test creates.
//!
//! The half of target selection that needs Windows is only interesting when it
//! meets a real desktop, and a desktop full of whatever the developer happens
//! to have open is not something to assert against. So this test creates its
//! own windows — a visible one, a hidden one, a tool window, an untitled one —
//! puts them somewhere nobody will see them, and checks that enumeration says
//! what it should about each. Every one of them is destroyed before the test
//! returns.
//!
//! That makes these tests deterministic (AGENTS.md section 25) and, crucially,
//! runnable on a CI machine: they assume a window station and a desktop to put
//! a window on, which a GitHub Actions Windows runner has, but they assume
//! nothing about what is running on it. Nothing here is `#[ignore]`d.
//!
//! The pure half — deciding which of several matching windows was meant — is
//! unit tested in `src/selection.rs` against constructed desktops, where the
//! interesting cases can actually be arranged.

use std::sync::atomic::{AtomicU32, Ordering};

use clipped_windows::{
    enumerate_monitors, enumerate_windows, is_window, monitor_for_window, resolve, window_geometry,
    Exclusion, PixelSize, ResolveError, TargetSelector, WindowHandle, WindowInfo, WindowsError,
    DEFAULT_DPI,
};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, UnregisterClassW,
    WINDOW_EX_STYLE, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP, WS_VISIBLE,
};

/// The size every fixture window is created at.
///
/// `WS_POPUP` has no frame, so the client area is the whole window and the
/// expected client size is exactly this — no `AdjustWindowRect` arithmetic to
/// get wrong, and a mismatch means enumeration measured the wrong rectangle
/// rather than that the test's maths is off.
const FIXTURE_SIZE: PixelSize = PixelSize::new(320, 240);

/// Where fixture windows are put: far outside any real display, so that running
/// the test suite does not flash windows across the developer's screen.
/// `MONITOR_DEFAULTTONEAREST` still resolves a monitor for a window here, which
/// is the behaviour `monitor_for_window` documents.
const OFFSCREEN: (i32, i32) = (-32_000, -32_000);

/// Distinguishes the window classes this test registers, so that tests running
/// in parallel threads cannot collide over a class name.
static NEXT_FIXTURE: AtomicU32 = AtomicU32::new(0);

/// A real top-level window, destroyed when the test drops it.
///
/// # Ownership
///
/// This owns two things Windows will not clean up on its own until the process
/// exits: the window and the window class registered for it. `Drop` releases
/// both, in that order, so a failing assertion — which unwinds — cannot leave a
/// window on the desktop for the next test to enumerate (AGENTS.md section 58).
struct Fixture {
    window: HWND,
    class_name: Vec<u16>,
    instance: HINSTANCE,
}

impl Fixture {
    /// Creates a window with `title` and the given extra style, shown or not.
    fn create(title: &str, extra_style: WINDOW_EX_STYLE, visible: bool) -> Self {
        // SAFETY: `None` asks for the handle of the current process's own
        // module, which always exists and needs no release.
        let module = unsafe { GetModuleHandleW(None) }.expect("this process has a module handle");
        let instance = HINSTANCE(module.0);

        let ordinal = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let mut class_name = utf16(&format!(
            "clipped-windows-fixture-{}-{ordinal}",
            std::process::id()
        ));

        let class = WNDCLASSW {
            lpfnWndProc: Some(window_procedure),
            hInstance: instance,
            lpszClassName: PCWSTR(class_name.as_mut_ptr()),
            ..Default::default()
        };

        // SAFETY: `class` borrows `class_name`, which lives in this struct for
        // as long as the class registration does, and is unregistered in `Drop`
        // before the buffer is freed. `RegisterClassW` copies the rest.
        let atom = unsafe { RegisterClassW(&raw const class) };
        assert!(atom != 0, "the window class could not be registered");

        let mut window_title = utf16(title);
        let style = if visible {
            WS_POPUP | WS_VISIBLE
        } else {
            WS_POPUP
        };

        // SAFETY: both string pointers refer to null-terminated UTF-16 buffers
        // that outlive the call, and every other argument is an integer or a
        // documented style constant. The window returned is owned by this
        // `Fixture` and destroyed in `Drop`.
        let window = unsafe {
            CreateWindowExW(
                extra_style,
                PCWSTR(class_name.as_mut_ptr()),
                PCWSTR(window_title.as_mut_ptr()),
                style,
                OFFSCREEN.0,
                OFFSCREEN.1,
                i32::try_from(FIXTURE_SIZE.width()).expect("the fixture is small"),
                i32::try_from(FIXTURE_SIZE.height()).expect("the fixture is small"),
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

    fn handle(&self) -> WindowHandle {
        WindowHandle::from_raw(self.window.0 as isize)
    }

    /// Destroys the window now, so that a test can assert what happens next.
    fn destroy_window(&mut self) {
        if self.window.is_invalid() {
            return;
        }
        // SAFETY: the window was created on this thread — which is what
        // `DestroyWindow` requires — and has not been destroyed, because the
        // handle is cleared immediately afterwards.
        unsafe { DestroyWindow(self.window) }.expect("the window can be destroyed");
        self.window = HWND(std::ptr::null_mut());
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.destroy_window();
        // SAFETY: the class was registered by this fixture with this instance
        // and this name, and the window using it has just been destroyed, which
        // is what `UnregisterClassW` requires.
        let _ =
            unsafe { UnregisterClassW(PCWSTR(self.class_name.as_mut_ptr()), Some(self.instance)) };
    }
}

/// The window procedure for fixture windows: Windows' own default behaviour.
///
/// # Safety
///
/// Called only by Windows, with the arguments it documents. It forwards them
/// unchanged and dereferences nothing.
unsafe extern "system" fn window_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: the arguments are exactly the ones Windows passed, forwarded to
    // the default handler unchanged.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

fn utf16(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A title no other window on the machine can plausibly have.
fn unique_title(label: &str) -> String {
    format!(
        "clipped-windows test {label} {} {}",
        std::process::id(),
        NEXT_FIXTURE.load(Ordering::Relaxed)
    )
}

/// Finds `handle` in a fresh enumeration of the desktop.
fn enumerated(handle: WindowHandle) -> WindowInfo {
    enumerate_windows()
        .expect("the desktop can be enumerated")
        .into_iter()
        .find(|window| window.handle() == handle)
        .unwrap_or_else(|| panic!("{handle} was created but is not in the enumeration"))
}

#[test]
fn a_visible_titled_window_is_enumerated_with_its_title_process_and_size() {
    let title = unique_title("visible");
    let fixture = Fixture::create(&title, WINDOW_EX_STYLE::default(), true);

    let window = enumerated(fixture.handle());

    assert_eq!(window.title(), title);
    assert_eq!(window.process_id(), std::process::id());
    assert_eq!(
        window.process_name().map(str::to_lowercase),
        current_executable_name().map(|name| name.to_lowercase()),
        "the window should be attributed to the test binary that created it"
    );
    assert_eq!(
        window.geometry().client_size(),
        FIXTURE_SIZE,
        "a WS_POPUP window's client area is the whole window"
    );
    assert!(
        window.geometry().dpi() >= DEFAULT_DPI,
        "a window is never scaled below 96 DPI: {}",
        window.geometry().dpi()
    );
    assert_eq!(window.exclusion(), None, "this window is capturable");
    assert!(!window.is_minimised());
}

#[test]
fn a_hidden_window_is_listed_and_reported_as_invisible() {
    let fixture = Fixture::create(&unique_title("hidden"), WINDOW_EX_STYLE::default(), false);

    let window = enumerated(fixture.handle());

    assert_eq!(window.exclusion(), Some(Exclusion::Invisible));
    assert!(!window.is_capturable());
}

#[test]
fn a_tool_window_is_listed_and_reported_as_one() {
    let fixture = Fixture::create(&unique_title("tool"), WS_EX_TOOLWINDOW, true);

    let window = enumerated(fixture.handle());

    assert_eq!(window.exclusion(), Some(Exclusion::ToolWindow));
}

#[test]
fn an_untitled_window_is_listed_and_reported_as_untitled() {
    let fixture = Fixture::create("", WINDOW_EX_STYLE::default(), true);

    let window = enumerated(fixture.handle());

    assert_eq!(window.exclusion(), Some(Exclusion::Untitled));
}

#[test]
fn a_window_can_be_resolved_from_the_real_desktop_by_title_process_and_handle() {
    let title = unique_title("resolvable");
    let fixture = Fixture::create(&title, WINDOW_EX_STYLE::default(), true);
    let windows = enumerate_windows().expect("the desktop can be enumerated");

    let selectors = [
        TargetSelector::WindowTitle(title.clone()),
        TargetSelector::WindowHandle(fixture.handle()),
    ];
    for selector in selectors {
        let resolved = resolve(&windows, &selector)
            .unwrap_or_else(|error| panic!("{selector} should resolve: {error}"));
        assert_eq!(resolved.handle(), fixture.handle());
    }

    // The process identifier is deliberately not in the list above: this test
    // binary owns every fixture window every test in this file has created, so
    // asking for it by PID is genuinely ambiguous — which is the behaviour, not
    // a limitation of the test.
    let by_process = resolve(&windows, &TargetSelector::ProcessId(std::process::id()));
    match by_process {
        Ok(resolved) => assert_eq!(
            resolved.handle(),
            fixture.handle(),
            "if this process owns one capturable window, it is this one"
        ),
        Err(ResolveError::Ambiguous { candidates, .. }) => assert!(
            candidates
                .iter()
                .any(|candidate| candidate.handle() == fixture.handle()),
            "the ambiguity should include this window"
        ),
        Err(error) => panic!("this process owns at least one capturable window: {error}"),
    }
}

#[test]
fn a_title_matching_two_real_windows_reports_both() {
    let title = unique_title("twin");
    let first = Fixture::create(&title, WINDOW_EX_STYLE::default(), true);
    let second = Fixture::create(&title, WINDOW_EX_STYLE::default(), true);

    let windows = enumerate_windows().expect("the desktop can be enumerated");
    let error = resolve(&windows, &TargetSelector::WindowTitle(title))
        .expect_err("two windows share this title, and neither is the right one");

    let ResolveError::Ambiguous { candidates, .. } = &error else {
        panic!("expected an ambiguity, got {error}");
    };
    let handles: Vec<WindowHandle> = candidates.iter().map(WindowInfo::handle).collect();
    assert!(
        handles.contains(&first.handle()) && handles.contains(&second.handle()),
        "both windows should be reported: {handles:?}"
    );
}

#[test]
fn a_window_that_closes_is_reported_as_gone_rather_than_measured() {
    let mut fixture = Fixture::create(&unique_title("closing"), WINDOW_EX_STYLE::default(), true);
    let handle = fixture.handle();

    assert!(is_window(handle));
    let before = window_geometry(handle).expect("the window is open");
    assert_eq!(before.client_size(), FIXTURE_SIZE);

    // What happens to a recording when the game exits.
    fixture.destroy_window();

    assert!(
        !is_window(handle),
        "the handle should no longer be a window"
    );
    let error = window_geometry(handle).expect_err("a destroyed window cannot be measured");
    assert!(
        matches!(error, WindowsError::WindowGone { handle: gone } if gone == handle),
        "unexpected error: {error}"
    );
    assert!(
        !enumerate_windows()
            .expect("the desktop can be enumerated")
            .iter()
            .any(|window| window.handle() == handle),
        "a destroyed window should not appear in a later enumeration"
    );
}

#[test]
fn every_enumerated_window_is_distinct_and_internally_consistent() {
    let windows = enumerate_windows().expect("the desktop can be enumerated");
    assert!(
        !windows.is_empty(),
        "a Windows session always has top-level windows"
    );

    let mut handles: Vec<WindowHandle> = windows.iter().map(WindowInfo::handle).collect();
    let total = handles.len();
    handles.sort_unstable();
    handles.dedup();
    assert_eq!(handles.len(), total, "a window was enumerated twice");

    for window in &windows {
        assert!(is_window(window.handle()) || window.exclusion().is_some());
        assert_eq!(
            window.is_capturable(),
            window.exclusion().is_none(),
            "capturability and the reported reason must agree for {}",
            window.handle()
        );
        if window.is_capturable() {
            assert!(
                !window.title().is_empty(),
                "a capturable window always has a title to select it by"
            );
            assert!(
                window.is_minimised() || !window.geometry().client_size().is_empty(),
                "a capturable window that is not minimised has pixels"
            );
        }
    }
}

#[test]
fn the_display_a_window_is_on_can_be_named_and_measured() {
    let fixture = Fixture::create(&unique_title("monitor"), WINDOW_EX_STYLE::default(), true);

    let monitors = enumerate_monitors().expect("monitors can be enumerated");
    assert!(
        !monitors.is_empty(),
        "a Windows session with a desktop has at least one display"
    );
    for monitor in &monitors {
        assert!(!monitor.device_name().is_empty(), "{monitor:?}");
        assert!(!monitor.bounds().size().is_empty(), "{monitor:?}");
        assert!(monitor.dpi() >= DEFAULT_DPI, "{monitor:?}");
    }

    let monitor = monitor_for_window(fixture.handle()).expect("the window is on some display");
    assert!(
        monitors
            .iter()
            .any(|candidate| candidate.handle() == monitor.handle()),
        "the window's display should be one of the enumerated ones: {monitor:?}"
    );

    // The window's own geometry has to agree about which display it is on.
    let geometry = window_geometry(fixture.handle()).expect("the window is open");
    assert_eq!(geometry.monitor(), monitor.handle());
}

/// The file name of the executable running this test.
fn current_executable_name() -> Option<String> {
    std::env::current_exe()
        .ok()?
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
}
