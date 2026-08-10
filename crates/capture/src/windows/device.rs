//! The Direct3D 11 device a capture backend captures into.

use windows::core::Interface;
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dxgi::{IDXGIDevice, DXGI_ADAPTER_DESC};
use windows::Win32::System::WinRT::Direct3D11::CreateDirect3D11DeviceFromDXGIDevice;

/// The Direct3D 11 device that owns every texture a backend produces, in both
/// the forms the capture APIs ask for.
///
/// Windows Graphics Capture is a WinRT API and wants an `IDirect3DDevice`; the
/// textures it hands back are `ID3D11Texture2D`s belonging to the `ID3D11Device`
/// underneath it, which is what the encoder will bind. They are two views of one
/// device, and keeping them together is what stops a later change from creating
/// a second device and quietly introducing a cross-adapter copy per frame.
///
/// # Ownership
///
/// The backend owns exactly one of these for its whole life, and releasing it
/// releases the device: both fields are reference-counted COM interfaces, and
/// dropping the struct calls `Release` on each. Every texture the backend hands
/// out belongs to this device, so the device must outlive every frame — which
/// it does, because [`CapturedFrame`](crate::CapturedFrame) borrows the backend
/// and the device is a field of it.
#[derive(Debug, Clone)]
pub(super) struct CaptureDevice {
    /// The Direct3D 11 device itself, which is what
    /// [`adapter_description`](Self::adapter_description) interrogates and what
    /// every texture the compositor hands back belongs to. Holding it is also
    /// what keeps the device alive for as long as those textures are in use.
    d3d11: ID3D11Device,
    /// The same device as WinRT sees it, which is what
    /// `Direct3D11CaptureFramePool` takes.
    winrt: IDirect3DDevice,
}

impl CaptureDevice {
    /// Creates a hardware device on the default adapter.
    ///
    /// `BGRA_SUPPORT` is required rather than preferred: the compositor hands
    /// capture frames over as `B8G8R8A8`, and a device created without it
    /// cannot make a render target view of one.
    ///
    /// No feature level is requested. Windows Graphics Capture needs 11_0 and
    /// every adapter that can run this application has it, so naming a list
    /// here would only add a way to be wrong later.
    ///
    /// # Errors
    ///
    /// The `HRESULT` from `D3D11CreateDevice` or from the WinRT interop, both
    /// of which mean there is no usable graphics device and no capture is
    /// possible.
    pub(super) fn create() -> Result<Self, windows::core::Error> {
        Self::create_with_driver_type(D3D_DRIVER_TYPE_HARDWARE)
    }

    fn create_with_driver_type(driver_type: D3D_DRIVER_TYPE) -> Result<Self, windows::core::Error> {
        let mut d3d11: Option<ID3D11Device> = None;

        // SAFETY: every pointer argument is either absent — no adapter, so the
        // default one; a null module handle, which is what the API requires
        // unless the driver type is the software rasteriser; no feature level
        // list; no returned feature level; no returned immediate context — or
        // the address of a live local `Option<ID3D11Device>`, which is the
        // representation windows-rs uses for an out parameter of that type.
        // `D3D11_SDK_VERSION` is the constant the header requires. On success
        // `d3d11` holds one reference, which `ID3D11Device`'s `Drop` releases.
        unsafe {
            D3D11CreateDevice(
                None,
                driver_type,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut d3d11),
                None,
                None,
            )?;
        }

        let d3d11 = d3d11.ok_or_else(|| {
            windows::core::Error::new(
                windows::Win32::Foundation::E_FAIL,
                "D3D11CreateDevice reported success without returning a device",
            )
        })?;

        let dxgi: IDXGIDevice = d3d11.cast()?;

        // SAFETY: `dxgi` is a live `IDXGIDevice` obtained by querying the
        // device created immediately above, which is precisely what this
        // function documents as its argument. The returned `IInspectable` is
        // an owned reference that windows-rs releases on drop.
        let winrt = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)? }.cast()?;

        Ok(Self { d3d11, winrt })
    }

    /// The device as WinRT sees it, for `Direct3D11CaptureFramePool`.
    pub(super) const fn winrt(&self) -> &IDirect3DDevice {
        &self.winrt
    }

    /// The adapter this device was created on, as its driver describes it.
    ///
    /// Logged once when capture starts. On a machine with more than one GPU —
    /// a laptop with an integrated and a discrete adapter, or this project's own
    /// development machine — "which adapter captured?" is the first question
    /// asked of a capture that produced black frames or of an encoder that could
    /// not bind a texture, and it is unanswerable from a log that does not say
    /// (AGENTS.md sections 19 and 35).
    ///
    /// Returns [`None`] rather than an error: a device that cannot describe
    /// itself is a diagnostic disappointment, not a reason to refuse to record.
    pub(super) fn adapter_description(&self) -> Option<String> {
        let dxgi: IDXGIDevice = self.d3d11.cast().ok()?;

        // SAFETY: `GetAdapter` and `GetDesc` are ordinary DXGI queries on a
        // live device, taking no pointers from this side and retaining nothing.
        // Both return owned values windows-rs releases on drop.
        let description: DXGI_ADAPTER_DESC = unsafe { dxgi.GetAdapter().ok()?.GetDesc().ok()? };

        let name: String = String::from_utf16_lossy(&description.Description)
            .trim_end_matches('\0')
            .to_owned();
        (!name.is_empty()).then_some(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::core::IUnknown;
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_WARP;

    use crate::windows::apartment::ensure_multi_threaded_apartment;

    #[test]
    fn a_device_exposes_the_same_adapter_through_both_interfaces() {
        // WARP rather than hardware so that this runs on a machine with no GPU
        // and inside a CI container; the interop path being exercised — DXGI to
        // WinRT — is the same one, and it is the part that can be got wrong.
        // A hardware device is what `create` asks for and what the measured
        // runs in the pull request used.
        ensure_multi_threaded_apartment().expect("the process can have an MTA");
        let device = match CaptureDevice::create_with_driver_type(D3D_DRIVER_TYPE_WARP) {
            Ok(device) => device,
            Err(error) => {
                // A build machine with no Direct3D at all is a legitimate
                // environment for the rest of the suite; say so rather than
                // fail, and say it loudly enough to notice if it becomes the
                // normal outcome.
                eprintln!("skipped: no WARP Direct3D 11 device on this machine: {error}");
                return;
            }
        };

        let round_tripped: IDXGIDevice = device
            .winrt()
            .cast::<windows::Win32::System::WinRT::Direct3D11::IDirect3DDxgiInterfaceAccess>()
            .and_then(|access|
                // SAFETY: `IDirect3DDxgiInterfaceAccess::GetInterface` returns
                // the DXGI interface named by the type parameter, and
                // `IDXGIDevice` is one every Direct3D 11 device implements.
                unsafe { access.GetInterface::<IDXGIDevice>() })
            .expect("the WinRT device wraps the DXGI device it was made from");

        // Compared through `IUnknown`, because COM only promises pointer
        // identity for that interface; two `QueryInterface` calls for the same
        // derived interface are not required to return the same pointer.
        let round_tripped: IUnknown = round_tripped
            .cast()
            .expect("every interface is an IUnknown");
        let original: IUnknown = device
            .d3d11
            .cast()
            .expect("a Direct3D 11 device is an IUnknown");

        assert_eq!(
            round_tripped.as_raw(),
            original.as_raw(),
            "the WinRT and Direct3D 11 views must be one device: two devices \
             would mean a cross-adapter copy of every captured frame"
        );
    }
}
