//! Top-level windows: which ones exist, and which of them can be recorded.

use core::ffi::c_void;
use core::fmt;
use core::mem;

use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::UI::HiDpi::{
    GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClientRect, GetDesktopWindow, GetShellWindow, GetWindowDisplayAffinity,
    GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
    IsWindow, IsWindowVisible, GWL_EXSTYLE, WDA_NONE, WS_EX_TOOLWINDOW,
};

use crate::error::WindowsError;
use crate::geometry::{PixelSize, DEFAULT_DPI};
use crate::monitor::{monitor_handle_for_window, rect_to_pixels, MonitorHandle};
use crate::process::ProcessNames;

/// A reference to a top-level window.
///
/// # Ownership
///
/// There is none, and that is the whole point. An `HWND` is a weak reference
/// owned by the thread that created the window, in whichever process that is.
/// Nothing here allocates it, nothing here releases it, and there is no
/// `CloseHandle` for one — so this type has no destructor and copying it is
/// free (AGENTS.md section 58).
///
/// The consequence is that a handle can stop naming a window at any moment,
/// between two lines of the code holding it. Every query taking one is fallible
/// for that reason and reports [`WindowsError::WindowGone`], and
/// [`is_window`] is the cheap check a capture loop can make.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WindowHandle(isize);

impl WindowHandle {
    /// Wraps a raw `HWND`.
    #[must_use]
    pub const fn from_raw(handle: isize) -> Self {
        Self(handle)
    }

    /// The raw `HWND`, for platform code that needs it back.
    #[must_use]
    pub const fn as_raw(self) -> isize {
        self.0
    }

    /// The handle widened for `clipped_capture::TargetHandle`, which carries a
    /// platform handle as an opaque `u64`.
    ///
    /// The conversion is here, rather than a `From` implementation on the
    /// capture type, because `clipped-windows` is layer 0 and `clipped-capture`
    /// is layer 1: this crate must not depend on that one (README.md,
    /// "Dependency direction"). The primitive lives at the bottom and the layer
    /// above converts, which is the direction the layering allows.
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0 as u64
    }

    // Not `const`: casting a pointer to an integer is not something the
    // compiler will do at compile time, and an `HWND` is a pointer.
    pub(crate) fn from_hwnd(handle: HWND) -> Self {
        Self(handle.0 as isize)
    }

    pub(crate) fn to_hwnd(self) -> HWND {
        HWND(self.0 as *mut c_void)
    }
}

impl fmt::Display for WindowHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "0x{:08x}", self.0)
    }
}

/// Why a window is not something this recorder can capture.
///
/// Every top-level window Windows will enumerate is reported by
/// [`enumerate_windows`], each carrying the first reason from this list that
/// applies to it, or none at all if it can be captured. Nothing is filtered out
/// silently: a user asking "why is my game not in the list?" needs an answer,
/// and a list that quietly omits it cannot give one.
///
/// The reasons are checked in the order they are declared here, cheapest and
/// most fundamental first, so a window that is both invisible and cloaked
/// reports [`Self::Invisible`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Exclusion {
    /// The desktop itself, or the shell window behind every other window.
    ///
    /// `Progman` — the window `GetShellWindow` returns — is where the desktop
    /// icons and the wallpaper live. Capturing it produces the wallpaper, never
    /// the desktop a user means, because everything else is a different window
    /// composited on top. Recording the whole screen is a monitor capture, and
    /// that is a different target kind.
    ShellWindow,

    /// The window is hidden: `IsWindowVisible` is false.
    ///
    /// Most windows on a desktop are in this state. Applications keep hidden
    /// message-only and helper windows around permanently, and a hidden window
    /// has no pixels to capture at all.
    Invisible,

    /// The window is cloaked by the Desktop Window Manager.
    ///
    /// Cloaking is visibility's modern sibling and the reason `IsWindowVisible`
    /// alone is not enough: a cloaked window is still "visible" by the old
    /// definition but is not composited. Suspended Store applications are
    /// cloaked, and so is every window belonging to a virtual desktop other
    /// than the current one — which is why a list built without this check
    /// shows a user several copies of applications they cannot see.
    Cloaked,

    /// The window has the `WS_EX_TOOLWINDOW` style.
    ///
    /// That style is an application saying "this is a palette or a helper, not
    /// a document": it is what keeps a window out of the taskbar and out of
    /// Alt-Tab. Tooltips, tray helpers, drop shadows and IME candidate windows
    /// all carry it, they number in the dozens on an ordinary desktop, and none
    /// of them is ever what someone meant to record.
    ToolWindow,

    /// The window has no title.
    ///
    /// A window with an empty title cannot be named on a command line, cannot
    /// be told apart from the other untitled ones in a list, and in practice is
    /// an implementation detail of some application rather than a document
    /// window. This is the one exclusion that is a judgement about usefulness
    /// rather than about capturability, which is why it is a reported reason
    /// rather than a filter: `list-windows --all` shows these.
    Untitled,

    /// The window's client area has no pixels in it.
    ///
    /// Zero-sized windows exist in numbers — they are how applications receive
    /// broadcast messages — and there is nothing for a capture backend to
    /// point at. Minimised windows are deliberately *not* excluded here: see
    /// [`WindowInfo::is_minimised`].
    ZeroSized,

    /// The window's owner has asked Windows to keep it out of captures.
    ///
    /// `SetWindowDisplayAffinity` with `WDA_MONITOR` or
    /// `WDA_EXCLUDEFROMCAPTURE` is what DRM video players, some launchers and
    /// password managers use. The system enforces it: capture produces a black
    /// rectangle or nothing at all. Reporting it here is what lets the recorder
    /// say so instead of writing a black recording, which
    /// [issue #97](https://github.com/wildware-uk/clipped/issues/97) makes a
    /// requirement.
    ContentProtected,
}

impl Exclusion {
    /// A short lower-case word for a list or a log field.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ShellWindow => "shell",
            Self::Invisible => "invisible",
            Self::Cloaked => "cloaked",
            Self::ToolWindow => "tool-window",
            Self::Untitled => "untitled",
            Self::ZeroSized => "zero-sized",
            Self::ContentProtected => "content-protected",
        }
    }

    /// A sentence explaining the exclusion to whoever is reading a list.
    #[must_use]
    pub const fn explanation(self) -> &'static str {
        match self {
            Self::ShellWindow => "the desktop or shell window; record a monitor instead",
            Self::Invisible => "hidden, so it has nothing to capture",
            Self::Cloaked => "cloaked by the compositor: suspended, or on another virtual desktop",
            Self::ToolWindow => "a tool window: a palette or helper, not a document window",
            Self::Untitled => "untitled, so there is no way to name it",
            Self::ZeroSized => "its client area has no pixels",
            Self::ContentProtected => "its owner excluded it from capture; it would record black",
        }
    }
}

impl fmt::Display for Exclusion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The measurements of a window that change while it is being recorded.
///
/// Separate from the rest of [`WindowInfo`] because a title and a process do
/// not move and these do: a window is dragged to another display, the user
/// changes the scaling, the game switches resolution. A capture session
/// re-reads this with [`window_geometry`]; it does not re-enumerate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowGeometry {
    client_size: PixelSize,
    dpi: u32,
    monitor: MonitorHandle,
}

impl WindowGeometry {
    /// Records a client size, a DPI and the display the window is on.
    #[must_use]
    pub const fn new(client_size: PixelSize, dpi: u32, monitor: MonitorHandle) -> Self {
        Self {
            client_size,
            dpi,
            monitor,
        }
    }

    /// The size of the window's client area, in physical pixels.
    ///
    /// The client area, not the window: the frame, title bar and drop shadow
    /// are not part of what a game renders and are not what should be recorded.
    /// This is what a capture backend and an encoder are configured from.
    #[must_use]
    pub const fn client_size(self) -> PixelSize {
        self.client_size
    }

    /// The DPI of the window: 96 at 100% scaling, 144 at 150%.
    ///
    /// Per-monitor, so dragging a window between differently scaled displays
    /// changes it. It is reported rather than applied: this crate measures in
    /// physical pixels throughout, and the scale factor is here because a user
    /// reading "2560x1440 at 144 DPI" can tell that what they are seeing at
    /// 150% is a 2560-pixel-wide capture and not a 1707-pixel-wide one.
    #[must_use]
    pub const fn dpi(self) -> u32 {
        self.dpi
    }

    /// The display showing most of the window.
    #[must_use]
    pub const fn monitor(self) -> MonitorHandle {
        self.monitor
    }
}

/// A top-level window, as it was when the desktop was enumerated.
///
/// A snapshot, and stale the moment it is returned: windows close, are renamed,
/// are moved to another display and are minimised while a list of them is being
/// printed. Nothing here should be treated as a promise about the present, and
/// [`WindowInfo::handle`] is the only field worth keeping — everything else can
/// be read again from it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WindowInfo {
    handle: WindowHandle,
    title: String,
    process_id: u32,
    process_name: Option<String>,
    geometry: WindowGeometry,
    minimised: bool,
    exclusion: Option<Exclusion>,
}

impl WindowInfo {
    /// Describes a window.
    ///
    /// [`enumerate_windows`] is what normally produces these. The constructor
    /// is public so that the selection logic in [`crate::resolve`] can be
    /// tested against constructed desktops, which is what makes that logic
    /// testable without one.
    #[must_use]
    pub const fn new(
        handle: WindowHandle,
        title: String,
        process_id: u32,
        process_name: Option<String>,
        geometry: WindowGeometry,
        minimised: bool,
        exclusion: Option<Exclusion>,
    ) -> Self {
        Self {
            handle,
            title,
            process_id,
            process_name,
            geometry,
            minimised,
            exclusion,
        }
    }

    /// The handle to pass back to Windows, and to a capture backend.
    #[must_use]
    pub const fn handle(&self) -> WindowHandle {
        self.handle
    }

    /// The window's title, as the title bar and Alt-Tab show it.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The identifier of the process that created the window.
    #[must_use]
    pub const fn process_id(&self) -> u32 {
        self.process_id
    }

    /// The executable behind the window, such as `cs2.exe`.
    ///
    /// [`None`] when Windows would not say. That is normal rather than
    /// exceptional: a process running at a higher integrity level than the
    /// recorder — an elevated application, most anti-cheat services — refuses
    /// to be opened, and a process that exited between being enumerated and
    /// being asked is simply gone.
    #[must_use]
    pub fn process_name(&self) -> Option<&str> {
        self.process_name.as_deref()
    }

    /// Where the window is and how large it was when it was enumerated.
    #[must_use]
    pub const fn geometry(&self) -> WindowGeometry {
        self.geometry
    }

    /// Whether the window is minimised.
    ///
    /// Deliberately not an [`Exclusion`]. A minimised game is a perfectly
    /// legitimate thing to point a recorder at — it is about to be restored —
    /// and the handle stays valid throughout. What is true is that Windows
    /// Graphics Capture produces no frames for a minimised window, and that its
    /// client size while minimised is not the size it will have when restored,
    /// so a caller that starts a recording here should expect the first frames
    /// to arrive on restore and the size to change with them.
    #[must_use]
    pub const fn is_minimised(&self) -> bool {
        self.minimised
    }

    /// Why this window cannot be captured, or [`None`] if it can.
    #[must_use]
    pub const fn exclusion(&self) -> Option<Exclusion> {
        self.exclusion
    }

    /// Whether this window is a thing the recorder can be pointed at.
    ///
    /// "Capturable" here means precisely this: Windows will enumerate it as a
    /// top-level window, it is visible and composited, it is not the shell, it
    /// is a document window rather than a tool window, it has a name to select
    /// it by, it has pixels in it, and its owner has not excluded it from
    /// capture. It does *not* mean a backend has agreed to capture it — that is
    /// `clipped_capture::select`'s answer, and it depends on which backends the
    /// build has.
    #[must_use]
    pub const fn is_capturable(&self) -> bool {
        self.exclusion.is_none()
    }

    /// Whether the window's owner has excluded it from screen capture.
    #[must_use]
    pub const fn is_content_protected(&self) -> bool {
        matches!(self.exclusion, Some(Exclusion::ContentProtected))
    }
}

/// Whether `handle` still names a window.
///
/// The cheap check a capture loop makes to notice that the game has closed. It
/// is inherently a statement about the past — the window can be destroyed
/// between this returning `true` and the next line — so it is a way of noticing
/// that capture should stop, not a way of proving it is safe to continue. The
/// proof that a handle is still good is a call that succeeds with it.
#[must_use]
pub fn is_window(handle: WindowHandle) -> bool {
    // SAFETY: `IsWindow` is the one Windows call that is defined for an
    // arbitrary handle value — answering that question is its entire purpose —
    // so there is no precondition to establish. It reads no memory this code
    // owns.
    unsafe { IsWindow(Some(handle.to_hwnd())) }.as_bool()
}

/// Re-reads the size, DPI and display of a window that is being captured.
///
/// # Errors
///
/// [`WindowsError::WindowGone`] if the window has been destroyed — either
/// because `GetClientRect` rejected the handle or because `MonitorFromWindow`
/// answered with a null display — which during a recording means the recording
/// is over, and [`WindowsError::Api`] if `GetClientRect` fails for any other
/// reason.
pub fn window_geometry(handle: WindowHandle) -> Result<WindowGeometry, WindowsError> {
    if !is_window(handle) {
        return Err(WindowsError::WindowGone { handle });
    }

    let mut rect = RECT::default();
    // SAFETY: `rect` is a live local for the duration of the call, which is
    // all `GetClientRect` requires of the pointer; the handle named a window a
    // moment ago and the call reports it through its return value if that has
    // stopped being true.
    unsafe { GetClientRect(handle.to_hwnd(), &raw mut rect) }.map_err(|source| {
        // A window destroyed between the check above and this call fails with
        // ERROR_INVALID_WINDOW_HANDLE. That race is not avoidable — there is no
        // atomic "measure this window if it still exists" — so it is reported
        // as what it is rather than as an API fault.
        if is_window(handle) {
            WindowsError::api("GetClientRect", source)
        } else {
            WindowsError::WindowGone { handle }
        }
    })?;

    // A null `HMONITOR` here means the same thing as a failed `GetClientRect`:
    // the window was destroyed during the measurement. It is reported rather
    // than stored, so that a `WindowGeometry` always names a display that
    // existed when it was built and a capture backend never receives a handle
    // that nothing will accept.
    let monitor = monitor_handle_for_window(handle).ok_or(WindowsError::WindowGone { handle })?;

    Ok(WindowGeometry::new(
        rect_to_pixels(rect).size(),
        window_dpi(handle),
        monitor,
    ))
}

/// Every top-level window on the current desktop, capturable or not.
///
/// The order is the one `EnumWindows` uses, which is front-to-back in Z order:
/// the window the user is looking at comes first. That is worth preserving — it
/// is the order a person scanning a list expects — so nothing here sorts.
///
/// # Ambiguity is the caller's problem
///
/// This returns *all* of them, including the ones that cannot be captured and
/// the several that may match whatever the user typed. Choosing between them is
/// [`crate::resolve`], which is a pure function over this list precisely so
/// that it can be tested without a desktop.
///
/// # Errors
///
/// [`WindowsError::Api`] if `EnumWindows` fails outright, or if measuring one
/// of the windows it reported fails for a reason other than that window having
/// closed. Windows that are destroyed while the enumeration is running are
/// dropped from the result rather than failing it, because that happens on any
/// real desktop; nothing else is dropped, because a window missing from the
/// list with no reason given is the one thing this module exists to prevent.
pub fn enumerate_windows() -> Result<Vec<WindowInfo>, WindowsError> {
    let handles = top_level_windows()?;

    // The shell window is read once rather than per window: it cannot change
    // while this runs without the shell having restarted, and that invalidates
    // the whole enumeration anyway.
    //
    // SAFETY: takes no arguments, touches no memory this code owns, and returns
    // a handle that is only ever compared for equality here.
    let shell = unsafe { GetShellWindow() };
    // SAFETY: as above.
    let desktop = unsafe { GetDesktopWindow() };

    let mut process_names = ProcessNames::default();
    handles
        .into_iter()
        .filter_map(|handle| describe(handle, shell, desktop, &mut process_names).transpose())
        .collect()
}

/// The handles `EnumWindows` reports, collected before anything is asked of
/// them.
///
/// Collecting first and describing afterwards is deliberate. A window procedure
/// belonging to another application is blocked for as long as an enumeration
/// callback runs, and describing a window means opening its process — orders of
/// magnitude more work than pushing a handle into a `Vec`. The cost is that a
/// window can be destroyed between the two passes, which [`describe`] handles
/// by dropping it.
fn top_level_windows() -> Result<Vec<HWND>, WindowsError> {
    let mut handles: Vec<HWND> = Vec::new();

    // SAFETY: `collect_window` is a plain `extern "system"` function that does
    // not unwind, and `handles` outlives the call: `EnumWindows` is synchronous
    // and does not store the callback or the parameter. The `LPARAM` is a
    // pointer to `handles`, which is what `collect_window` documents needing.
    let result = unsafe { EnumWindows(Some(collect_window), LPARAM(&raw mut handles as isize)) };

    if let Err(source) = result {
        // `EnumWindows` reports failure when a *window* disappeared mid-walk as
        // well as when the enumeration itself failed, and in the first case it
        // sets the last error to ERROR_SUCCESS. Treating that as a failure
        // would make listing windows unreliable on a busy desktop, so the
        // handles collected so far are kept.
        if source.code().is_err() {
            return Err(WindowsError::api("EnumWindows", source));
        }
    }

    Ok(handles)
}

/// The answers Windows gives about one window, gathered in one place.
///
/// Everything here costs a syscall; nothing here is a decision. Separating the
/// asking from the deciding is what makes [`exclusion_for`] a pure function
/// that can be tested through every one of its branches, rather than something
/// only a live desktop in the right state could exercise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WindowFacts {
    is_shell: bool,
    visible: bool,
    cloaked: bool,
    tool_window: bool,
    content_protected: bool,
    minimised: bool,
}

impl WindowFacts {
    /// Asks Windows about `window`.
    fn read(window: WindowHandle, is_shell: bool) -> Self {
        Self {
            is_shell,
            visible: is_visible(window),
            cloaked: is_cloaked(window),
            tool_window: has_tool_window_style(window),
            content_protected: is_content_protected(window),
            minimised: is_minimised(window),
        }
    }
}

/// Reads everything known about one window, or [`None`] if it has gone.
///
/// The two outcomes are kept apart deliberately. A window destroyed between
/// `EnumWindows` collecting its handle and this measuring it is [`None`] and
/// disappears from the listing, which is the documented behaviour and the only
/// silent drop this module allows. Any other failure is an error: dropping it
/// too would quietly change the "N of M top-level windows" denominator and
/// leave a user asking why their game is not in the list with no answer.
fn describe(
    handle: HWND,
    shell: HWND,
    desktop: HWND,
    process_names: &mut ProcessNames,
) -> Result<Option<WindowInfo>, WindowsError> {
    let window = WindowHandle::from_hwnd(handle);
    let geometry = match window_geometry(window) {
        Ok(geometry) => geometry,
        Err(WindowsError::WindowGone { .. }) => return Ok(None),
        Err(error) => return Err(error),
    };

    let title = window_title(window);
    let facts = WindowFacts::read(window, handle == shell || handle == desktop);
    let exclusion = exclusion_for(&facts, &title, geometry.client_size());

    let process_id = window_process_id(window);
    let process_name = process_names.name_of(process_id);

    Ok(Some(WindowInfo::new(
        window,
        title,
        process_id,
        process_name,
        geometry,
        facts.minimised,
        exclusion,
    )))
}

/// The first reason a window cannot be captured, in the order documented on
/// [`Exclusion`].
fn exclusion_for(facts: &WindowFacts, title: &str, client_size: PixelSize) -> Option<Exclusion> {
    if facts.is_shell {
        return Some(Exclusion::ShellWindow);
    }
    if !facts.visible {
        return Some(Exclusion::Invisible);
    }
    if facts.cloaked {
        return Some(Exclusion::Cloaked);
    }
    if facts.tool_window {
        return Some(Exclusion::ToolWindow);
    }
    if title.is_empty() {
        return Some(Exclusion::Untitled);
    }
    // A minimised window reports a client area of nothing, or of the icon-sized
    // rectangle it is reduced to. Neither is the size it will have when it is
    // restored, and neither means it cannot be captured, so the zero-size test
    // is not applied to it.
    if !facts.minimised && client_size.is_empty() {
        return Some(Exclusion::ZeroSized);
    }
    if facts.content_protected {
        return Some(Exclusion::ContentProtected);
    }
    None
}

/// The window's title, or an empty string if it has none.
fn window_title(window: WindowHandle) -> String {
    // SAFETY: no memory is passed; the call reads the window's own title
    // length. A destroyed window returns 0, which is handled below.
    let length = unsafe { GetWindowTextLengthW(window.to_hwnd()) };
    let Ok(length) = usize::try_from(length) else {
        return String::new();
    };
    if length == 0 {
        return String::new();
    }

    // One more for the terminator `GetWindowTextW` always writes, and which is
    // not counted by `GetWindowTextLengthW`. The title can also have grown
    // between the two calls, in which case it is truncated rather than retried:
    // this is a display name, and a title that changes while it is being read
    // is changing faster than any list of it can be useful.
    let mut buffer = vec![0_u16; length + 1];

    // SAFETY: `buffer` is a live, uniquely borrowed slice of `length + 1` UTF-16
    // units; the binding takes a slice rather than a raw pointer, so the length
    // the call is given cannot disagree with the allocation.
    let written = unsafe { GetWindowTextW(window.to_hwnd(), &mut buffer) };
    let written = usize::try_from(written).unwrap_or(0);

    String::from_utf16_lossy(&buffer[..written])
}

/// The process that created `window`, or [`None`] if the window has gone.
///
/// The identifier of the process that *created* the window, which is not always
/// the process a user would name: a game whose launcher owns the window reports
/// the launcher. That is the right answer for scoping audio all the same, because
/// [`ProcessTree`](crate::ProcessTree) is rooted here and takes in the children,
/// and because the alternative — guessing which of a tree's processes is "the
/// game" — is a guess a recording would act on.
///
/// Zero is folded into [`None`]. Windows returns it for a handle that has
/// stopped naming a window, and a caller that scoped a capture to process 0
/// would be scoping it to the System Idle Process
/// ([issue #26](https://github.com/wildware-uk/clipped/issues/26)).
#[must_use]
pub fn window_process(window: WindowHandle) -> Option<u32> {
    match window_process_id(window) {
        0 => None,
        process_id => Some(process_id),
    }
}

/// The identifier of the process that created the window, or 0 if it has gone.
fn window_process_id(window: WindowHandle) -> u32 {
    let mut process_id = 0_u32;
    // SAFETY: `process_id` is a live local for the call, which writes it at
    // most once. The return value — the thread identifier — is not wanted.
    unsafe { GetWindowThreadProcessId(window.to_hwnd(), Some(&raw mut process_id)) };
    process_id
}

fn is_visible(window: WindowHandle) -> bool {
    // SAFETY: takes a handle by value, touches no memory this code owns, and is
    // defined for a handle that has stopped being valid (it answers false).
    unsafe { IsWindowVisible(window.to_hwnd()) }.as_bool()
}

fn is_minimised(window: WindowHandle) -> bool {
    // SAFETY: as `is_visible`.
    unsafe { IsIconic(window.to_hwnd()) }.as_bool()
}

fn has_tool_window_style(window: WindowHandle) -> bool {
    // SAFETY: `GetWindowLongPtrW` with `GWL_EXSTYLE` reads a documented field
    // of the window's own state and returns it by value; no pointer is
    // involved. It returns 0 for a window that has gone, which reads as "no
    // extended styles" and is the right answer for something being filtered.
    let styles = unsafe { GetWindowLongPtrW(window.to_hwnd(), GWL_EXSTYLE) };
    let styles = u32::try_from(styles & 0xffff_ffff).unwrap_or(0);
    styles & WS_EX_TOOLWINDOW.0 != 0
}

/// Whether the compositor is hiding the window: suspended, or on another
/// virtual desktop.
fn is_cloaked(window: WindowHandle) -> bool {
    let mut cloaked = 0_u32;

    // SAFETY: the pointer and the size describe the same live local `u32`, which
    // is what `DWMWA_CLOAKED` is documented to write. A mismatch here is the one
    // way this call can be unsound, and the size is taken from the variable's
    // own type rather than written out.
    let read = unsafe {
        DwmGetWindowAttribute(
            window.to_hwnd(),
            DWMWA_CLOAKED,
            (&raw mut cloaked).cast::<c_void>(),
            u32::try_from(mem::size_of_val(&cloaked)).expect("a u32 is four bytes"),
        )
    };

    // The Desktop Window Manager refuses the question for a window that has
    // gone, and on the rare configuration where composition is off. Neither
    // means cloaked, and treating a failure as "cloaked" would empty the list.
    read.is_ok() && cloaked != 0
}

/// Whether the window's owner has excluded it from screen capture.
fn is_content_protected(window: WindowHandle) -> bool {
    let mut affinity = 0_u32;

    // SAFETY: `affinity` is a live local written at most once by the call, which
    // takes no other memory. Failure is reported through the return value.
    let read = unsafe { GetWindowDisplayAffinity(window.to_hwnd(), &raw mut affinity) };

    // A failure means Windows would not say. Reporting "not protected" is the
    // honest default: it lets the recorder try, and a backend that then records
    // black is issue #97's problem to detect. Claiming protection here would
    // hide a capturable window from the list on the strength of a failed call.
    read.is_ok() && affinity != WDA_NONE.0
}

/// The window's DPI, or [`DEFAULT_DPI`] if Windows will not say.
fn window_dpi(window: WindowHandle) -> u32 {
    // SAFETY: takes a handle by value and returns an integer; it touches no
    // memory this code owns and returns 0 for an invalid window.
    let dpi = unsafe { GetDpiForWindow(window.to_hwnd()) };
    if dpi == 0 {
        DEFAULT_DPI
    } else {
        dpi
    }
}

/// Makes this process see the desktop in physical pixels.
///
/// **Call this once, from the executable, before any other function in this
/// crate.** It is a process-wide mode, not a per-call option, which is why it
/// is an explicit call rather than something enumeration does quietly on the
/// caller's behalf.
///
/// Without it, Windows lies to the process for compatibility: `GetClientRect`
/// on a window on a 150%-scaled display reports 1707x960 for a window that is
/// really 2560x1440, and `GetDpiForWindow` reports 96 everywhere. A recorder
/// that believed either would configure its encoder for the wrong size and
/// produce a soft, letterboxed recording on every high-DPI machine — a bug that
/// is invisible to anyone developing at 100% scaling, which is most of why this
/// documentation is this long.
///
/// # Errors
///
/// [`WindowsError::Api`] if the mode could not be set, which is a real problem
/// and not a formality: on a display scaled above 100% every size this crate
/// then reports is the compatibility fiction described above. The one rejection
/// that is *not* a problem — the mode was already set — is
/// [`DpiAwareness::AlreadySet`] rather than an error, so that a caller does not
/// have to tell the two apart by inspecting an `HRESULT` (AGENTS.md section
/// 15).
pub fn enable_per_monitor_dpi_awareness() -> Result<DpiAwareness, WindowsError> {
    // SAFETY: the argument is a documented constant handle to a DPI awareness
    // context; nothing is allocated, borrowed or shared. The call is
    // process-wide state, which is why the documentation above insists it
    // happens once, before anything reads a window size.
    match unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) } {
        Ok(()) => Ok(DpiAwareness::Set),
        // Windows rejects a second attempt to set the awareness mode with
        // ERROR_ACCESS_DENIED, and that is the *only* meaning that code has
        // here: the argument is a constant, so there is no invalid-parameter
        // case to confuse it with. Anything else means the process is not
        // per-monitor aware and the caller needs to know.
        Err(source) if source.code() == ERROR_ACCESS_DENIED.to_hresult() => {
            Ok(DpiAwareness::AlreadySet)
        }
        Err(source) => Err(WindowsError::api("SetProcessDpiAwarenessContext", source)),
    }
}

/// What [`enable_per_monitor_dpi_awareness`] found the process to be.
///
/// The distinction exists so that a caller can log the two differently. "It was
/// already per-monitor aware" is a detail nobody needs; "it could not be made
/// per-monitor aware" means every measurement on a scaled display is wrong, and
/// a caller that cannot tell them apart has to pick one level for both and will
/// pick the quiet one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DpiAwareness {
    /// This call set the mode, and it was not set before.
    Set,

    /// The mode was already set, by an application manifest or by an earlier
    /// call. The process is per-monitor DPI aware either way, so there is
    /// nothing here for a user to act on.
    AlreadySet,
}

/// Collects each top-level window handle into the `Vec<HWND>` behind `state`.
///
/// # Safety
///
/// `state` must be a pointer to a live `Vec<HWND>` that outlives the enclosing
/// `EnumWindows` call, and nothing else may touch that `Vec` while the
/// enumeration runs. Both hold: the only caller is [`top_level_windows`], which
/// passes a local and waits for the call to return.
unsafe extern "system" fn collect_window(handle: HWND, state: LPARAM) -> windows::core::BOOL {
    // SAFETY: `state` is the pointer `top_level_windows` passed, to a `Vec` that
    // is still alive and borrowed only by this in-progress call, so this
    // exclusive reference is unique.
    let handles = unsafe { &mut *(state.0 as *mut Vec<HWND>) };
    handles.push(handle);
    // Never stop early: a filtered enumeration cannot explain what it left out.
    true.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_handle_survives_the_round_trip_through_the_windows_type() {
        let handle = WindowHandle::from_raw(0x0001_04ac);
        assert_eq!(WindowHandle::from_hwnd(handle.to_hwnd()), handle);
        assert_eq!(handle.as_u64(), 0x0001_04ac);
        assert_eq!(handle.to_string(), "0x000104ac");
    }

    /// An ordinary application window: visible, composited, titled and sized.
    fn capturable() -> WindowFacts {
        WindowFacts {
            is_shell: false,
            visible: true,
            cloaked: false,
            tool_window: false,
            content_protected: false,
            minimised: false,
        }
    }

    const SIZED: PixelSize = PixelSize::new(1920, 1080);
    const EMPTY: PixelSize = PixelSize::new(0, 0);

    #[test]
    fn an_ordinary_application_window_is_capturable() {
        assert_eq!(
            exclusion_for(&capturable(), "Counter-Strike 2", SIZED),
            None
        );
    }

    #[test]
    fn each_fact_windows_reports_produces_its_own_exclusion() {
        /// One fact about a window, as the thing that sets it.
        type SetFact = fn(&mut WindowFacts);

        let cases: [(SetFact, Exclusion); 5] = [
            (|facts| facts.is_shell = true, Exclusion::ShellWindow),
            (|facts| facts.visible = false, Exclusion::Invisible),
            (|facts| facts.cloaked = true, Exclusion::Cloaked),
            (|facts| facts.tool_window = true, Exclusion::ToolWindow),
            (
                |facts| facts.content_protected = true,
                Exclusion::ContentProtected,
            ),
        ];

        for (break_it, expected) in cases {
            let mut facts = capturable();
            break_it(&mut facts);
            assert_eq!(
                exclusion_for(&facts, "Counter-Strike 2", SIZED),
                Some(expected),
                "changing one fact should report {expected}"
            );
        }
    }

    #[test]
    fn a_window_that_is_several_kinds_of_unrecordable_reports_the_documented_first_one() {
        // The shell window is untitled and zero-sized on some configurations,
        // and the reason a user is shown has to be the one that explains what
        // the window is rather than the last thing that happened to be checked.
        let facts = WindowFacts {
            is_shell: true,
            visible: false,
            cloaked: true,
            tool_window: true,
            content_protected: true,
            minimised: false,
        };
        assert_eq!(
            exclusion_for(&facts, "", EMPTY),
            Some(Exclusion::ShellWindow)
        );

        let facts = WindowFacts {
            is_shell: false,
            ..facts
        };
        assert_eq!(exclusion_for(&facts, "", EMPTY), Some(Exclusion::Invisible));

        let facts = WindowFacts {
            visible: true,
            ..facts
        };
        assert_eq!(exclusion_for(&facts, "", EMPTY), Some(Exclusion::Cloaked));

        let facts = WindowFacts {
            cloaked: false,
            ..facts
        };
        assert_eq!(
            exclusion_for(&facts, "", EMPTY),
            Some(Exclusion::ToolWindow)
        );

        let facts = WindowFacts {
            tool_window: false,
            ..facts
        };
        assert_eq!(exclusion_for(&facts, "", EMPTY), Some(Exclusion::Untitled));
    }

    #[test]
    fn an_untitled_window_is_reported_before_its_size_is_judged() {
        assert_eq!(
            exclusion_for(&capturable(), "", SIZED),
            Some(Exclusion::Untitled)
        );
    }

    #[test]
    fn a_minimised_window_is_not_excluded_for_having_no_pixels() {
        let facts = capturable();
        assert_eq!(
            exclusion_for(&facts, "Counter-Strike 2", EMPTY),
            Some(Exclusion::ZeroSized)
        );

        let minimised = WindowFacts {
            minimised: true,
            ..facts
        };
        assert_eq!(
            exclusion_for(&minimised, "Counter-Strike 2", EMPTY),
            None,
            "a minimised window is a legitimate target whose size is not final"
        );
    }

    #[test]
    fn every_exclusion_explains_itself_in_two_registers() {
        for reason in [
            Exclusion::ShellWindow,
            Exclusion::Invisible,
            Exclusion::Cloaked,
            Exclusion::ToolWindow,
            Exclusion::Untitled,
            Exclusion::ZeroSized,
            Exclusion::ContentProtected,
        ] {
            assert!(!reason.as_str().is_empty());
            assert!(
                !reason.as_str().contains(' '),
                "{reason} is used as a column value and a log field, so it cannot contain a space"
            );
            assert!(
                reason.explanation().len() > reason.as_str().len(),
                "{reason} explains itself with no more words than it names itself with"
            );
        }
    }

    #[test]
    fn a_handle_that_was_never_a_window_is_reported_as_gone() {
        // The path a capture loop takes when the game closes: the handle it has
        // held since the recording started stops naming a window.
        let handle = WindowHandle::from_raw(0x0bad_f00d);
        assert!(!is_window(handle));

        let error = window_geometry(handle).expect_err("this was never a window");
        assert!(
            matches!(error, WindowsError::WindowGone { handle: gone } if gone == handle),
            "unexpected error: {error}"
        );
    }
}
