//! Capture backends built on Windows APIs.
//!
//! Everything in this module and its children is compiled only on Windows, and
//! it is the only place in `clipped-capture` that names a Windows type. The
//! platform-neutral half of the crate — the trait, the vocabulary and the
//! selection policy — still builds and still runs its unit tests on a machine
//! that is not Windows, which is what keeps AGENTS.md section 5's boundary
//! checkable rather than aspirational.
//!
//! # What is here
//!
//! - [`WindowsGraphicsCapture`], the `Windows.Graphics.Capture` backend
//!   ([issue #12](https://github.com/wildware-uk/clipped/issues/12)).
//! - [`DesktopDuplication`], the DXGI Desktop Duplication backend
//!   ([issue #13](https://github.com/wildware-uk/clipped/issues/13)), which is
//!   the fallback SPEC.md section 8 names below it.
//! - [`D3d11FrameSampler`], which reads a few pixels back off a captured frame
//!   so that a capture that has gone black can be noticed
//!   ([issue #97](https://github.com/wildware-uk/clipped/issues/97)). It is not
//!   a backend; it is the platform half of
//!   [`BlackFrameWatch`](crate::BlackFrameWatch).
//! - [`D3d11StillCopier`], which copies a whole captured frame into system
//!   memory so that a screenshot can be written from a frame the recording
//!   already had ([issue #67](https://github.com/wildware-uk/clipped/issues/67)).
//!   It is the platform half of [`StillFrame`](crate::StillFrame).
//!
//! # Apartments and devices
//!
//! COM apartment initialisation (`apartment.rs`) and Direct3D device creation
//! (`device.rs`) live here rather than in `clipped-windows` because
//! `clipped-windows` is still a documentation-only crate and this is the first
//! code in the workspace that needs either. Both are small and self-contained
//! precisely so that moving them down a layer, when a second subsystem needs
//! them, is a move rather than a rewrite — and the apartment in particular is
//! already process-wide rather than per-capture, which is the shape it would
//! have in `clipped-windows` anyway.

mod apartment;
mod crop;
mod desktop_duplication;
mod device;
mod graphics_capture;
mod pixel_sample;
mod still;

pub use desktop_duplication::DesktopDuplication;
pub use graphics_capture::WindowsGraphicsCapture;
pub use pixel_sample::D3d11FrameSampler;
pub use still::D3d11StillCopier;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

use crate::FrameSize;

/// The window's client area *now*, or [`None`] if it has no size or has gone.
///
/// Shared by both backends because both ask the same question for the same
/// reason: the size a window is being recorded at came from an enumeration that
/// is already stale, and the only authority on the size it is now is the window
/// itself. Desktop Duplication crops to it every frame; Windows Graphics Capture
/// compares a frame's shape against it to tell a genuine resize from the
/// transient shape a window takes on while it is minimised or being restored
/// ([issue #383](https://github.com/wildware-uk/clipped/issues/383)).
pub(super) fn client_size(window: HWND) -> Option<FrameSize> {
    let mut rect = RECT::default();
    // SAFETY: `rect` is a live local for the duration of the call, which is all
    // `GetClientRect` requires of the pointer; a handle that has stopped being a
    // window is reported through the return value.
    unsafe { GetClientRect(window, &raw mut rect) }.ok()?;

    FrameSize::new(
        rect.right.saturating_sub(rect.left).unsigned_abs(),
        rect.bottom.saturating_sub(rect.top).unsigned_abs(),
    )
}
