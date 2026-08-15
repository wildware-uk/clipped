//! Asks this machine's Desktop Duplication whether it delivers frames at all.
//!
//! Written to tell two failures apart that look identical from a test: a
//! duplication that is broken, and a desktop that simply is not changing.
//! Every output is duplicated twice — once against an idle desktop, and once
//! while a window on that output repaints in alternating colours, which is a
//! real present rather than a redraw of the same pixels.

#![cfg(windows)]

use std::time::{Duration, Instant};

use windows::core::{w, Interface};
use windows::Win32::Foundation::{COLORREF, HMODULE, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
    DXGI_ERROR_NOT_FOUND, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, DXGI_OUTPUT_DESC,
};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, FillRect, GetDC, ReleaseDC, HBRUSH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, PeekMessageW, RegisterClassW,
    ShowWindow, TranslateMessage, PM_REMOVE, SW_SHOWNOACTIVATE, WNDCLASSW, WS_EX_NOACTIVATE,
    WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};

/// How long each pass asks for frames.
const PASS: Duration = Duration::from_secs(3);

/// The per-acquire timeout, matching the one the tests use.
const ACQUIRE_MILLISECONDS: u32 = 250;

fn main() {
    let _ = clipped_windows::enable_per_monitor_dpi_awareness();

    // SAFETY: the type parameter supplies the interface; the call takes nothing
    // else and returns an owned reference.
    let factory: IDXGIFactory1 = match unsafe { CreateDXGIFactory1() } {
        Ok(factory) => factory,
        Err(error) => {
            println!("no DXGI factory: {error}");
            return;
        }
    };

    for adapter_index in 0.. {
        // SAFETY: the factory is live; NOT_FOUND ends the enumeration.
        let adapter = match unsafe { factory.EnumAdapters1(adapter_index) } {
            Ok(adapter) => adapter,
            Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(error) => {
                println!("enumerating adapters stopped: {error}");
                break;
            }
        };

        // SAFETY: `adapter` is live; `GetDesc` returns by value.
        let name = unsafe { adapter.GetDesc() }
            .map(|description| {
                String::from_utf16_lossy(&description.Description)
                    .trim_end_matches('\0')
                    .to_owned()
            })
            .unwrap_or_else(|_| "unknown".to_owned());

        for output_index in 0.. {
            // SAFETY: as above.
            let output = match unsafe { adapter.EnumOutputs(output_index) } {
                Ok(output) => output,
                Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(error) => {
                    println!("  enumerating outputs stopped: {error}");
                    break;
                }
            };

            // SAFETY: `output` is live; `GetDesc` returns by value.
            let description: DXGI_OUTPUT_DESC = match unsafe { output.GetDesc() } {
                Ok(description) => description,
                Err(error) => {
                    println!("  [{name}] output {output_index} would not describe itself: {error}");
                    continue;
                }
            };

            let bounds = description.DesktopCoordinates;
            let device_name = String::from_utf16_lossy(&description.DeviceName)
                .trim_end_matches('\0')
                .to_owned();
            println!(
                "\n[{name}] {device_name} {}x{} at ({},{}) attached={}",
                bounds.right - bounds.left,
                bounds.bottom - bounds.top,
                bounds.left,
                bounds.top,
                description.AttachedToDesktop.as_bool()
            );

            if !description.AttachedToDesktop.as_bool() {
                println!("  not attached to the desktop, so nothing to duplicate");
                continue;
            }

            let Ok(output1) = output.cast::<IDXGIOutput1>() else {
                println!("  no IDXGIOutput1, so this output cannot be duplicated");
                continue;
            };

            let mut device: Option<ID3D11Device> = None;
            // SAFETY: the adapter is named, so the driver type must be UNKNOWN
            // and the module handle null. The out parameter is a live local.
            let created = unsafe {
                D3D11CreateDevice(
                    &adapter,
                    D3D_DRIVER_TYPE_UNKNOWN,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&raw mut device),
                    None,
                    None,
                )
            };
            let Some(device) = created.ok().and(device) else {
                println!("  no Direct3D 11 device on this adapter");
                continue;
            };

            // SAFETY: both the output and the device are live, and the
            // duplication returned is owned.
            let duplication = match unsafe { output1.DuplicateOutput(&device) } {
                Ok(duplication) => duplication,
                Err(error) => {
                    println!("  DuplicateOutput refused: {error}");
                    continue;
                }
            };

            let idle = drain(&duplication);
            println!("  idle desktop      -> {idle}");

            match MarkerWindow::at(bounds.left + 120, bounds.top + 120) {
                Some(window) => {
                    let painting = drain_while_painting(&duplication, &window);
                    println!("  window repainting -> {painting}");
                }
                None => println!("  window repainting -> no window could be created"),
            }
        }
    }
}

/// What one pass of acquisition saw.
struct Tally {
    frames: u32,
    timeouts: u32,
    accumulated: u32,
    error: Option<String>,
}

impl std::fmt::Display for Tally {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} frames, {} timeouts, {} accumulated updates",
            self.frames, self.timeouts, self.accumulated
        )?;
        if let Some(error) = &self.error {
            write!(formatter, ", stopped by {error}")?;
        }
        Ok(())
    }
}

fn drain(duplication: &IDXGIOutputDuplication) -> Tally {
    acquire_until(duplication, PASS, || {})
}

fn drain_while_painting(duplication: &IDXGIOutputDuplication, window: &MarkerWindow) -> Tally {
    let mut alternate = false;
    acquire_until(duplication, PASS, || {
        alternate = !alternate;
        window.paint(alternate);
    })
}

fn acquire_until(
    duplication: &IDXGIOutputDuplication,
    how_long: Duration,
    mut between: impl FnMut(),
) -> Tally {
    let mut tally = Tally {
        frames: 0,
        timeouts: 0,
        accumulated: 0,
        error: None,
    };
    let deadline = Instant::now() + how_long;
    while Instant::now() < deadline {
        between();
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        // SAFETY: the duplication is live and both out parameters are live
        // locals of the projected types.
        let acquired = unsafe {
            duplication.AcquireNextFrame(ACQUIRE_MILLISECONDS, &raw mut info, &raw mut resource)
        };
        match acquired {
            Ok(()) => {
                tally.frames += 1;
                tally.accumulated += info.AccumulatedFrames;
                drop(resource);
                // SAFETY: a frame was acquired, so exactly one release is owed.
                let _ = unsafe { duplication.ReleaseFrame() };
            }
            Err(error) if error.code() == DXGI_ERROR_WAIT_TIMEOUT => tally.timeouts += 1,
            Err(error) => {
                tally.error = Some(format!("{error}"));
                break;
            }
        }
    }
    tally
}

/// A visible topmost window that can repaint in two colours.
struct MarkerWindow {
    window: HWND,
    brushes: [HBRUSH; 2],
}

impl MarkerWindow {
    fn at(x: i32, y: i32) -> Option<Self> {
        let class = w!("clipped_duplication_probe");
        let class_definition = WNDCLASSW {
            lpfnWndProc: Some(procedure),
            lpszClassName: class,
            ..Default::default()
        };
        // SAFETY: every pointer is either null or a static wide literal, and the
        // procedure is a real `extern "system"` function. A repeat registration
        // failing is expected and harmless.
        let _ = unsafe { RegisterClassW(&raw const class_definition) };

        // SAFETY: the class is registered above, the strings are static wide
        // literals, and no parent, menu or creation parameter is passed.
        let window = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                class,
                w!("clipped duplication probe"),
                WS_POPUP | WS_VISIBLE,
                x,
                y,
                400,
                300,
                None,
                None,
                None,
                None,
            )
        }
        .ok()?;

        // SAFETY: the window was just created.
        let _ = unsafe { ShowWindow(window, SW_SHOWNOACTIVATE) };

        // SAFETY: the colours are literals; each brush is deleted in `Drop`.
        let brushes = unsafe {
            [
                CreateSolidBrush(COLORREF(0x0020_80F0)),
                CreateSolidBrush(COLORREF(0x00F0_8020)),
            ]
        };
        Some(Self { window, brushes })
    }

    fn paint(&self, alternate: bool) {
        let mut client = RECT::default();
        // SAFETY: `client` is a live local and the window is live.
        if unsafe { GetClientRect(self.window, &raw mut client) }.is_err() {
            return;
        }
        // SAFETY: the window is live, so `GetDC` returns a context for it; the
        // rectangle and brush are live for the call and the context is released
        // immediately afterwards.
        unsafe {
            let context = GetDC(Some(self.window));
            FillRect(
                context,
                &raw const client,
                self.brushes[usize::from(alternate)],
            );
            ReleaseDC(Some(self.window), context);
        }
        pump();
    }
}

impl Drop for MarkerWindow {
    fn drop(&mut self) {
        // SAFETY: the window is live and owned here; the brushes were created
        // here and are not owned by any class.
        unsafe {
            let _ = DestroyWindow(self.window);
            for brush in self.brushes {
                let _ = DeleteObject(brush.into());
            }
        }
        pump();
    }
}

extern "system" fn procedure(
    window: HWND,
    message: u32,
    w: WPARAM,
    l: LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    // SAFETY: this is the default handling every message not handled here needs.
    unsafe { DefWindowProcW(window, message, w, l) }
}

fn pump() {
    let mut message = windows::Win32::UI::WindowsAndMessaging::MSG::default();
    // SAFETY: `message` is a live local; `PM_REMOVE` takes each message out of
    // the queue, so the loop terminates.
    while unsafe { PeekMessageW(&raw mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
        // SAFETY: the message came from `PeekMessageW` and is live.
        unsafe {
            let _ = TranslateMessage(&raw const message);
            windows::Win32::UI::WindowsAndMessaging::DispatchMessageW(&raw const message);
        }
    }
}
