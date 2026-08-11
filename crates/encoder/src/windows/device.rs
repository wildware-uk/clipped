//! A Direct3D 11 device to ask an encoder questions through.
//!
//! Every hardware encoder on Windows is reached through a graphics device: an
//! NVENC session is opened against an `ID3D11Device`, and an AMF context is
//! initialised with one. Encoding never needs this module — a backend is handed
//! the device the capture backend created, and creating a second one would put
//! the encoder on a different device from the frames (see
//! [`crate::frame::GraphicsDevice`]). Capability probing has no capture to
//! borrow a device from, so it makes one, uses it for the length of the query
//! and destroys it.
//!
//! # Ownership
//!
//! [`ProbeDevice`] owns the device. The COM wrapper releases it on drop, and
//! the raw pointer handed out by [`ProbeDevice::as_graphics_device`] borrows
//! it, so a session cannot outlive the device it was opened against.
//!
//! # Which adapter
//!
//! The caller names one. A machine with an NVIDIA card and an AMD integrated
//! part has both encoders, and a device created with `D3D_DRIVER_TYPE_HARDWARE`
//! lands on whichever adapter DXGI enumerates first — so NVENC would be asked
//! about its own hardware roughly half the time. Naming the adapter is what
//! makes the answer belong to the encoder the report attributes it to.

use windows::core::Interface;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, DXGI_ERROR_NOT_FOUND,
};

use crate::adapter::AdapterId;
use crate::frame::{DeviceKind, GraphicsDevice};

/// A Direct3D 11 device created for the length of a capability query.
#[derive(Debug)]
pub(super) struct ProbeDevice {
    device: ID3D11Device,
}

impl ProbeDevice {
    /// Creates a device on the adapter with this identifier.
    ///
    /// # Errors
    ///
    /// The `windows` error from DXGI or Direct3D, or `DXGI_ERROR_NOT_FOUND`
    /// when no adapter has that identifier — which can happen between the
    /// enumeration that produced it and this call, on a machine whose GPU was
    /// just disabled.
    pub(super) fn on(adapter: AdapterId) -> Result<Self, windows::core::Error> {
        let adapter = find_adapter(adapter)?;

        let mut device: Option<ID3D11Device> = None;
        // SAFETY: the driver type is `UNKNOWN`, which is what Direct3D requires
        // when an adapter is named, and the software rasteriser handle is null
        // to match it. The feature level list and `device` are live locals for
        // the duration of the call, and the context and feature level
        // out-parameters are `None` because neither is wanted: a probe issues
        // no draw calls.
        unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                // No debug layer and no BGRA guarantee: nothing here draws or
                // presents, so the only requirement is a device the encoder
                // runtimes will accept.
                D3D11_CREATE_DEVICE_FLAG(0),
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&raw mut device),
                None,
                None,
            )?;
        }

        device
            .map(|device| Self { device })
            .ok_or_else(|| windows::core::Error::from_hresult(DXGI_ERROR_NOT_FOUND))
    }

    /// The device, as an encoder backend takes one.
    ///
    /// # Ownership
    ///
    /// The returned handle borrows `self`, which owns the only reference to the
    /// device. Nothing opened against it may outlive this [`ProbeDevice`].
    pub(super) fn as_graphics_device(&self) -> GraphicsDevice {
        // SAFETY: the device is live for as long as `self` is, and the borrow
        // in the signature ties the handle to it. It is an `ID3D11Device`,
        // which is what `DeviceKind::D3d11` names.
        unsafe { GraphicsDevice::new(DeviceKind::D3d11, self.device.as_raw()) }
    }
}

/// The DXGI adapter with this identifier.
fn find_adapter(wanted: AdapterId) -> Result<IDXGIAdapter1, windows::core::Error> {
    // SAFETY: `CreateDXGIFactory1` takes no arguments beyond the interface
    // identifier, which the generic parameter supplies, and returns an owned
    // reference that `IDXGIFactory1` releases on drop.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }?;

    for index in 0.. {
        // SAFETY: the factory is alive for the whole loop, and `EnumAdapters1`
        // is documented to return `DXGI_ERROR_NOT_FOUND` once `index` is past
        // the last adapter, which is what ends it.
        let adapter = match unsafe { factory.EnumAdapters1(index) } {
            Ok(adapter) => adapter,
            Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(error) => return Err(error),
        };
        // SAFETY: `GetDesc1` reads the adapter and returns a description by
        // value; it takes no pointers from this side.
        let description = match unsafe { adapter.GetDesc1() } {
            Ok(description) => description,
            // One adapter that will not describe itself must not hide the one
            // being looked for, which may be the next in the list.
            Err(error) => {
                tracing::debug!(index, %error, "a display adapter could not be described");
                continue;
            }
        };

        let id = AdapterId::from_luid(
            description.AdapterLuid.LowPart,
            description.AdapterLuid.HighPart,
        );
        if id == wanted {
            return Ok(adapter);
        }
    }

    Err(windows::core::Error::from_hresult(DXGI_ERROR_NOT_FOUND))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::windows::dxgi;

    #[test]
    fn a_device_can_be_created_on_every_adapter_that_could_hold_an_encoder() {
        // A test about the machine it runs on, which is the only way to check
        // that the adapter lookup and the creation flags are right: whatever
        // adapters exist, a device has to come back for each one that could
        // host a hardware encoder. A machine with none — a virtualised CI
        // runner — passes vacuously, and the probe it stands in for produces no
        // measurement there either.
        let adapters = dxgi::adapters().expect("DXGI can be asked on a Windows machine");

        for adapter in adapters
            .iter()
            .filter(|adapter| adapter.can_host_hardware_encoder())
        {
            let device = ProbeDevice::on(adapter.id()).unwrap_or_else(|error| {
                panic!(
                    "a Direct3D 11 device could not be created on {}: {error}",
                    adapter.description()
                )
            });
            assert!(
                !device.as_graphics_device().as_raw().is_null(),
                "a device that created successfully must have a handle"
            );
        }
    }

    #[test]
    fn an_adapter_that_is_not_there_is_an_error_rather_than_the_wrong_device() {
        // No machine has this identifier, so this exercises the lookup's
        // failure path without depending on what is installed. Falling back to
        // "any adapter" here would measure one GPU's limits and file them under
        // another's.
        let error = ProbeDevice::on(AdapterId::from_luid(0xFFFF_FFFF, 0x7FFF_FFFF))
            .expect_err("no adapter has that identifier");
        assert_eq!(error.code(), DXGI_ERROR_NOT_FOUND);
    }
}
