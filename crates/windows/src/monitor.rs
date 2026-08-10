//! Displays: where they are, how large they are, and how much they are scaled.

use core::fmt;
use core::mem;

use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromWindow, HDC, HMONITOR, MONITORINFO,
    MONITORINFOEXW, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

use crate::geometry::{PixelRect, PixelSize, DEFAULT_DPI};
use crate::window::WindowHandle;
use crate::WindowsError;

/// `MONITORINFOF_PRIMARY` from `winuser.h`, restated because the bindings do
/// not generate it: it is a plain preprocessor definition with no type
/// attached, so the metadata the `windows` crate is generated from has nothing
/// to emit. It is the only bit `dwFlags` has ever had.
const MONITORINFOF_PRIMARY: u32 = 0x0000_0001;

/// A reference to a display.
///
/// Like [`WindowHandle`] this is a weak reference and is never closed: an
/// `HMONITOR` is not a kernel object and there is nothing to release
/// (AGENTS.md section 58).
///
/// It is, however, shorter-lived than it looks. Windows invalidates every
/// `HMONITOR` whenever displays are attached, detached or rearranged, so a
/// handle stored across a display change names nothing even if the same
/// physical monitor is still plugged in. Treat one as valid for as long as the
/// enumeration that produced it, and re-enumerate after a display change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MonitorHandle(isize);

impl MonitorHandle {
    /// Wraps a raw `HMONITOR`.
    #[must_use]
    pub const fn from_raw(handle: isize) -> Self {
        Self(handle)
    }

    /// The raw `HMONITOR`, for platform code that needs it back.
    #[must_use]
    pub const fn as_raw(self) -> isize {
        self.0
    }

    /// The handle widened for [`clipped_capture::TargetHandle`], which carries
    /// a platform handle as an opaque `u64`.
    ///
    /// [`clipped_capture::TargetHandle`]: https://docs.rs/clipped-capture
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0 as u64
    }

    // Not `const`: casting a pointer to an integer is not something the
    // compiler will do at compile time, and an `HMONITOR` is a pointer.
    pub(crate) fn from_hmonitor(handle: HMONITOR) -> Self {
        Self(handle.0 as isize)
    }

    pub(crate) fn to_hmonitor(self) -> HMONITOR {
        HMONITOR(self.0 as *mut core::ffi::c_void)
    }
}

impl fmt::Display for MonitorHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "0x{:08x}", self.0)
    }
}

/// What is known about a display.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MonitorInfo {
    handle: MonitorHandle,
    device_name: String,
    bounds: PixelRect,
    work_area: PixelRect,
    dpi: u32,
    primary: bool,
}

impl MonitorInfo {
    /// Describes a display. Enumeration produces these; the constructor is
    /// public so that tests and callers can build one without a display.
    #[must_use]
    pub const fn new(
        handle: MonitorHandle,
        device_name: String,
        bounds: PixelRect,
        work_area: PixelRect,
        dpi: u32,
        primary: bool,
    ) -> Self {
        Self {
            handle,
            device_name,
            bounds,
            work_area,
            dpi,
            primary,
        }
    }

    /// The handle this display was enumerated as.
    #[must_use]
    pub const fn handle(&self) -> MonitorHandle {
        self.handle
    }

    /// The display device name, such as `\\.\DISPLAY1`.
    ///
    /// This is the GDI device name, not the monitor's marketing name: it is
    /// what identifies the display to every other Windows API, and it is stable
    /// for as long as the arrangement is.
    #[must_use]
    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// The display's rectangle in virtual-desktop coordinates.
    #[must_use]
    pub const fn bounds(&self) -> PixelRect {
        self.bounds
    }

    /// The part of the display not covered by the taskbar and other appbars.
    #[must_use]
    pub const fn work_area(&self) -> PixelRect {
        self.work_area
    }

    /// The display's effective DPI: 96 at 100% scaling, 144 at 150%.
    #[must_use]
    pub const fn dpi(&self) -> u32 {
        self.dpi
    }

    /// Whether this is the primary display, the one at the origin.
    #[must_use]
    pub const fn is_primary(&self) -> bool {
        self.primary
    }
}

impl fmt::Display for MonitorInfo {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.device_name, self.bounds)?;
        if self.primary {
            formatter.write_str(", primary")?;
        }
        Ok(())
    }
}

/// Every display currently attached, in the order Windows reports them.
///
/// # Errors
///
/// [`WindowsError::Api`] if `EnumDisplayMonitors` fails. A display that is
/// removed midway through the enumeration is dropped from the result rather
/// than failing it.
pub fn enumerate_monitors() -> Result<Vec<MonitorInfo>, WindowsError> {
    let mut handles: Vec<HMONITOR> = Vec::new();

    // SAFETY: the callback below is a plain `extern "system"` function that
    // does not unwind, and `handles` outlives the call because
    // `EnumDisplayMonitors` is synchronous — it has returned before this
    // statement ends, and the callback is not stored anywhere. The `LPARAM`
    // carries a pointer to `handles` and is the only thing the callback
    // dereferences.
    unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor),
            LPARAM(&raw mut handles as isize),
        )
    }
    .ok()
    .map_err(|source| WindowsError::api("EnumDisplayMonitors", source))?;

    Ok(handles
        .into_iter()
        .filter_map(|handle| monitor_info(MonitorHandle::from_hmonitor(handle)).ok())
        .collect())
}

/// The display a window is on, by area: the one showing most of it.
///
/// A window straddling two displays is on exactly one as far as capture is
/// concerned, and Windows' own answer — the display with the largest
/// intersection — is the one every other API agrees with.
///
/// # Errors
///
/// [`WindowsError::MonitorGone`] if the display has been detached between the
/// two calls this makes, and [`WindowsError::Api`] if Windows refuses one of
/// them.
pub fn monitor_for_window(window: WindowHandle) -> Result<MonitorInfo, WindowsError> {
    monitor_info(monitor_handle_for_window(window))
}

/// The handle of the display showing most of `window`.
///
/// `MONITOR_DEFAULTTONEAREST` means this answers even for a window positioned
/// off every display, which happens: a window remembers a position from a
/// monitor that has since been unplugged.
pub(crate) fn monitor_handle_for_window(window: WindowHandle) -> MonitorHandle {
    // SAFETY: `MonitorFromWindow` takes a window handle by value and returns a
    // handle. It has no failure mode with `MONITOR_DEFAULTTONEAREST` — an
    // invalid or destroyed window yields the nearest monitor to the origin
    // rather than a null handle — so there is nothing to check.
    let monitor = unsafe { MonitorFromWindow(window.to_hwnd(), MONITOR_DEFAULTTONEAREST) };
    MonitorHandle::from_hmonitor(monitor)
}

/// Reads a display's name, geometry and DPI.
///
/// # Errors
///
/// [`WindowsError::MonitorGone`] if `GetMonitorInfoW` rejects the handle, which
/// is what a detached or superseded display looks like.
pub fn monitor_info(handle: MonitorHandle) -> Result<MonitorInfo, WindowsError> {
    let mut info = MONITORINFOEXW {
        monitorInfo: MONITORINFO {
            cbSize: u32::try_from(mem::size_of::<MONITORINFOEXW>())
                .expect("MONITORINFOEXW is far smaller than u32::MAX"),
            ..Default::default()
        },
        ..Default::default()
    };

    // SAFETY: `GetMonitorInfoW` writes into a `MONITORINFOEX` when `cbSize`
    // says the buffer is one, which it does above; the cast is the documented
    // way to pass the extended form, and `info` is a live local for the whole
    // call. The function reports failure through its return value, checked
    // immediately.
    let read =
        unsafe { GetMonitorInfoW(handle.to_hmonitor(), (&raw mut info).cast::<MONITORINFO>()) };
    if !read.as_bool() {
        return Err(WindowsError::MonitorGone { handle });
    }

    let device_name = String::from_utf16_lossy(&info.szDevice)
        .trim_end_matches('\0')
        .to_owned();

    Ok(MonitorInfo::new(
        handle,
        device_name,
        rect_to_pixels(info.monitorInfo.rcMonitor),
        rect_to_pixels(info.monitorInfo.rcWork),
        effective_dpi(handle),
        info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
    ))
}

/// The display's effective DPI, or [`DEFAULT_DPI`] if Windows will not say.
///
/// `GetDpiForMonitor` fails only for a handle that has stopped being valid, and
/// the caller has already established that it is valid by reading the monitor
/// info. Reporting 96 in that narrow window is better than failing a whole
/// enumeration over a scale factor.
fn effective_dpi(handle: MonitorHandle) -> u32 {
    let mut horizontal = 0_u32;
    let mut vertical = 0_u32;

    // SAFETY: both out-parameters are live locals for the duration of the call,
    // and the handle is passed by value. `GetDpiForMonitor` writes each once
    // and reports failure through its return value.
    let read = unsafe {
        GetDpiForMonitor(
            handle.to_hmonitor(),
            MDT_EFFECTIVE_DPI,
            &mut horizontal,
            &mut vertical,
        )
    };

    // The horizontal and vertical values are always equal on Windows; the API
    // reports both because it predates that being true.
    match read {
        Ok(()) if horizontal > 0 => horizontal,
        _ => DEFAULT_DPI,
    }
}

pub(crate) fn rect_to_pixels(rect: RECT) -> PixelRect {
    PixelRect::new(
        rect.left,
        rect.top,
        PixelSize::new(
            rect.right.saturating_sub(rect.left).unsigned_abs(),
            rect.bottom.saturating_sub(rect.top).unsigned_abs(),
        ),
    )
}

/// Collects the handle of each display into the `Vec<HMONITOR>` behind `state`.
///
/// # Safety
///
/// `state` must be a pointer to a live `Vec<HMONITOR>` that outlives the
/// enclosing `EnumDisplayMonitors` call, and the enumeration must be the only
/// thing touching it. Both hold: the only caller is [`enumerate_monitors`],
/// which passes a local and waits for the call to return.
unsafe extern "system" fn collect_monitor(
    monitor: HMONITOR,
    _device_context: HDC,
    _clip: *mut RECT,
    state: LPARAM,
) -> windows::core::BOOL {
    // SAFETY: `state` is the pointer `enumerate_monitors` passed, to a `Vec`
    // that is still borrowed by the in-progress call. Nothing else holds a
    // reference to it while the enumeration runs, so this exclusive reference
    // is unique.
    let handles = unsafe { &mut *(state.0 as *mut Vec<HMONITOR>) };
    handles.push(monitor);
    // Keep going: every display is wanted.
    true.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_windows_rectangle_becomes_an_origin_and_a_size() {
        let rect = RECT {
            left: -1920,
            top: -120,
            right: 0,
            bottom: 960,
        };
        let converted = rect_to_pixels(rect);

        assert_eq!(converted.left(), -1920);
        assert_eq!(converted.top(), -120);
        assert_eq!(converted.size(), PixelSize::new(1920, 1080));
    }

    #[test]
    fn a_monitor_handle_survives_the_round_trip_through_the_windows_type() {
        let handle = MonitorHandle::from_raw(0x7ffe_0001);
        assert_eq!(
            MonitorHandle::from_hmonitor(handle.to_hmonitor()),
            handle,
            "the pointer round trip must not lose bits"
        );
    }

    #[test]
    fn a_monitor_that_no_longer_exists_is_reported_as_gone() {
        // A handle that was never a monitor stands in for one that has been
        // superseded by a display change: `GetMonitorInfoW` rejects both the
        // same way, which is the path a recording has to survive.
        let handle = MonitorHandle::from_raw(0x0badf00d);
        let error = monitor_info(handle).expect_err("this was never a monitor");

        assert!(
            matches!(error, WindowsError::MonitorGone { handle: reported } if reported == handle),
            "unexpected error: {error}"
        );
    }
}
