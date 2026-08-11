//! Finding the graphics device a captured frame lives on.
//!
//! # Why this is needed at all
//!
//! An encoder session is opened against one device and can only bind textures
//! belonging to it (`clipped_encoder::GraphicsDevice`). The textures come from
//! the capture backend, which creates and owns its own device and — quite
//! deliberately — never hands it out: `CaptureBackend` has no accessor for one,
//! because every native resource a backend uses is the backend's for its whole
//! life (`docs/capture-pipeline.md`).
//!
//! So the session asks the *texture*. `ID3D11DeviceChild::GetDevice` is the
//! documented way to get from a resource to the device that created it, it is
//! how `crates/capture`'s own tests read a pixel out of a captured frame, and
//! it cannot be wrong about the answer in the way a second `D3D11CreateDevice`
//! could — a session that created its own device would be encoding textures
//! from another one, which either fails to bind or copies every frame across
//! adapters.
//!
//! # Ownership
//!
//! [`FrameDevice`] holds one reference to the device, taken by `GetDevice` and
//! released when it is dropped (AGENTS.md section 58). That reference is what
//! keeps the device alive for as long as the encoder session opened against it,
//! which matters because the capture backend is shut down first on some paths
//! out of a recording.
//!
//! It borrows nothing from the frame it was derived from: the frame is released
//! back to the compositor immediately afterwards, and the device outlives it.

use core::ffi::c_void;

use clipped_capture::{CapturedFrame, TextureKind};
use clipped_encoder::{DeviceKind, GraphicsDevice};
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};

/// The Direct3D 11 device a captured frame's texture belongs to.
///
/// Not `Clone`: one recording opens one encoder against one device, and a
/// second handle to it would only make the lifetime harder to see.
#[derive(Debug)]
pub(crate) struct FrameDevice(ID3D11Device);

impl FrameDevice {
    /// Asks `frame`'s texture which device created it.
    ///
    /// Returns [`None`] when the texture is not a kind this can interrogate, or
    /// when Direct3D declines — neither is expected, and a caller reports it as
    /// "the graphics device behind the capture could not be identified" rather
    /// than guessing at a device of its own.
    pub(crate) fn of(frame: &CapturedFrame<'_>) -> Option<Self> {
        if frame.texture().kind() != TextureKind::D3d11Texture2D {
            return None;
        }
        let raw: *mut c_void = frame.texture().as_raw();
        if raw.is_null() {
            return None;
        }

        // SAFETY: the pointer came from a live `CapturedFrame`, whose texture
        // the backend guarantees is a valid `ID3D11Texture2D` for as long as
        // the frame exists — and the frame is borrowed for this call.
        // `from_raw_borrowed` takes no reference of its own, so nothing here
        // can release the backend's texture.
        let texture = unsafe { ID3D11Texture2D::from_raw_borrowed(&raw) }?;

        // SAFETY: `texture` is live for the length of this call.
        // `ID3D11DeviceChild::GetDevice` returns an owned reference to the
        // device that created the resource, which windows-rs releases when the
        // `ID3D11Device` below is dropped.
        let device = unsafe { texture.GetDevice() }.ok()?;
        Some(Self(device))
    }

    /// The device as an encoder is handed one.
    ///
    /// # Safety of the value returned
    ///
    /// [`GraphicsDevice`] is a borrowed handle with no lifetime of its own, so
    /// it must not outlive this [`FrameDevice`]. Every caller in this crate
    /// passes it straight into an encoder's `open`, which takes its own
    /// reference to the device before returning.
    pub(crate) fn as_graphics_device(&self) -> GraphicsDevice {
        // SAFETY: `self.0` is a live `ID3D11Device` this value owns a reference
        // to, so the handle is valid for at least as long as `self`, which is
        // what `GraphicsDevice::new` requires. `as_raw` returns the interface
        // pointer without transferring ownership.
        unsafe { GraphicsDevice::new(DeviceKind::D3d11, self.0.as_raw()) }
    }
}
