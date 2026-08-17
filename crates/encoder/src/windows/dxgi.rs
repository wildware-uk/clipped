//! Enumerating display adapters, and reading their driver versions.
//!
//! This is the cheap half of a probe: a factory, a loop, and a struct read per
//! adapter. Its answer is the capability cache's key, so it runs on every
//! detection whether the cache hits or not.

use windows::core::Interface;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIDevice, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE,
    DXGI_ERROR_NOT_FOUND,
};

use crate::adapter::{Adapter, AdapterId, DriverVersion};
use crate::codec::Vendor;
use crate::error::{EncodeError, EncodeErrorKind};
use crate::frame::GraphicsDevice;
use crate::probe::ProbeError;

/// Refuses a device created on an adapter this encoder cannot encode from.
///
/// **All three hardware backends call this, and none of them may write its own
/// version** (AGENTS.md section 55). The failure it prevents is not obscure: a
/// machine with a discrete NVIDIA card and integrated AMD graphics — the
/// ordinary gaming laptop — captures on the default adapter, and whichever
/// vendor's encoder is not on that adapter is handed a device belonging to
/// somebody else. What each runtime says about that is a status code that names
/// no adapter: AMF answers `AMF_INVALID_ARG` from `AMFContext::InitDX11`, which
/// reads as a broken driver
/// ([issue #443](https://github.com/wildware-uk/clipped/issues/443)).
///
/// It does **not** make the encoder work on such a machine. Capturing on that
/// adapter, or copying frames across adapters to reach it, is the larger
/// question the issue carries; this is the part that is right whichever way that
/// goes.
///
/// `encoder` is the backend's short name as its own error messages use it —
/// "AMF", "NVENC", "Quick Sync" — rather than [`crate::EncoderKind`]'s display
/// name, which already carries the vendor and would repeat it in the sentence.
///
/// # Errors
///
/// [`EncodeErrorKind::Configuration`] naming both vendors, or naming the device
/// as one that would not answer as a DXGI device at all.
///
/// # Safety
///
/// `device` must hold null or a live `ID3D11Device` owned by the caller.
pub(super) unsafe fn require_adapter_vendor(
    device: &GraphicsDevice,
    wanted: Vendor,
    encoder: &str,
    fail: &impl Fn(EncodeErrorKind) -> EncodeError,
) -> Result<(), EncodeError> {
    // SAFETY: the caller guarantees the handle is null or a live `ID3D11Device`
    // it owns; this borrows it for the length of the call and releases nothing.
    match unsafe { device_vendor(device.as_raw()) } {
        Some(found) if found == wanted => Ok(()),
        Some(other) => Err(fail(EncodeErrorKind::Configuration {
            detail: format!(
                "{encoder} encodes on {wanted} graphics and this device was created on a \
                 {other} adapter; capture on the {wanted} adapter to encode with {encoder}, \
                 or use that adapter's own encoder"
            ),
        })),
        None => Err(fail(EncodeErrorKind::Configuration {
            detail: "the graphics device did not answer as a DXGI device, so the adapter it \
                     was created on could not be identified"
                .to_owned(),
        })),
    }
}

/// Which vendor's adapter a graphics device was created on.
///
/// [`None`] when the device is null or does not answer as a DXGI device, which
/// is a question unanswered rather than evidence of a particular vendor — the
/// callers say so in those words rather than guessing.
///
/// **Shared by every backend that cares**, and each of them does for the same
/// reason: a vendor's encoder runtime initialises against a Direct3D device, and
/// a device created on somebody else's adapter is refused by that runtime with a
/// status code that says nothing about adapters. Quick Sync has checked this
/// since it was written; AMF did not, and the result was that
/// `--encoder amf` on a machine with a discrete NVIDIA card and integrated AMD
/// graphics failed with `AMFContext::InitDX11 failed with AMF_INVALID_ARG (4)`
/// ([issue #443](https://github.com/wildware-uk/clipped/issues/443)).
///
/// # Safety
///
/// `device` must be null or a live `ID3D11Device` owned by the caller.
pub(super) unsafe fn device_vendor(device: *mut core::ffi::c_void) -> Option<Vendor> {
    if device.is_null() {
        return None;
    }

    // SAFETY: the caller guarantees `device` is a live COM object it owns.
    // `from_raw_borrowed` takes no reference of its own, so the borrow ends
    // with this function and nothing here releases the caller's device.
    let unknown = unsafe { windows::core::IUnknown::from_raw_borrowed(&device) }?;
    let dxgi: IDXGIDevice = unknown.cast().ok()?;

    // SAFETY: `dxgi` is a live DXGI device obtained by querying the caller's
    // device, and both calls are ordinary queries that return by value or as a
    // new reference this function drops.
    let description = unsafe { dxgi.GetAdapter().ok()?.GetDesc().ok()? };

    Some(Vendor::from_pci_id(description.VendorId))
}

/// Enumerates every display adapter DXGI knows about.
///
/// Includes the ones that cannot encode — the Microsoft Basic Render Driver,
/// and anything else flagged as software — because "the only adapter in this
/// machine is a software rasteriser" is the most useful thing a report about a
/// virtual machine can say.
pub(super) fn adapters() -> Result<Vec<Adapter>, ProbeError> {
    // SAFETY: `CreateDXGIFactory1` takes no arguments beyond the interface
    // identifier, which the generic parameter supplies, and returns an owned
    // reference that `IDXGIFactory1` releases on drop.
    let factory: IDXGIFactory1 =
        unsafe { CreateDXGIFactory1() }.map_err(|error| api_error("CreateDXGIFactory1", &error))?;

    let mut adapters = Vec::new();
    for index in 0.. {
        // SAFETY: the factory is alive for the whole loop, and `EnumAdapters1`
        // is documented to return `DXGI_ERROR_NOT_FOUND` once `index` is past
        // the last adapter, which is the loop's only exit.
        let adapter = match unsafe { factory.EnumAdapters1(index) } {
            Ok(adapter) => adapter,
            Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(error) => return Err(api_error("IDXGIFactory1::EnumAdapters1", &error)),
        };

        match describe(&adapter) {
            Ok(described) => adapters.push(described),
            Err(error) => {
                // One adapter that will not describe itself should not hide the
                // others: a machine with a broken virtual display still has a
                // GPU worth reporting (AGENTS.md section 16).
                tracing::warn!(
                    index,
                    %error,
                    "a display adapter could not be described and was left out of the report"
                );
            }
        }
    }

    Ok(adapters)
}

/// Reads one adapter's description and driver version.
fn describe(adapter: &IDXGIAdapter1) -> Result<Adapter, ProbeError> {
    // SAFETY: `GetDesc1` reads the adapter and returns a description by value;
    // it takes no pointers from this side.
    let description = unsafe { adapter.GetDesc1() }
        .map_err(|error| api_error("IDXGIAdapter1::GetDesc1", &error))?;

    let name = String::from_utf16_lossy(&description.Description);
    let name = name.trim_end_matches('\0').trim().to_owned();
    let software = description.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0;

    Ok(Adapter::new(
        AdapterId::from_luid(
            description.AdapterLuid.LowPart,
            description.AdapterLuid.HighPart,
        ),
        name,
        Vendor::from_pci_id(description.VendorId),
        description.DeviceId,
        description.DedicatedVideoMemory as u64,
        software,
    )
    .with_driver_version(driver_version(adapter)))
}

/// The user-mode driver version, where the adapter reports one.
///
/// `CheckInterfaceSupport` answers for Direct3D 10 and later drivers and
/// returns an error otherwise — notably for the software adapters, which have
/// no driver version to give. That is a "not reported", not a failure: the
/// cache key can hold the absence, and refusing to enumerate an adapter because
/// it declined to give a version would lose the adapter entirely.
fn driver_version(adapter: &IDXGIAdapter1) -> Option<DriverVersion> {
    // SAFETY: the interface identifier is a `'static` constant from the
    // projection, and the version is returned by value.
    let supported = unsafe { adapter.CheckInterfaceSupport(&IDXGIDevice::IID) };

    match supported {
        Ok(version) => Some(DriverVersion::from_raw(version as u64)),
        Err(error) => {
            tracing::debug!(%error, "an adapter did not report a driver version");
            None
        }
    }
}

/// Turns a `windows` error into one this crate's callers can read.
fn api_error(operation: &'static str, error: &windows::core::Error) -> ProbeError {
    ProbeError::Api {
        operation,
        code: error.code().0,
        message: error.message(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_null_device_has_no_vendor() {
        // A device nobody supplied is a question unanswered, not a vendor. The
        // callers turn `None` into "the adapter could not be identified" rather
        // than into a guess about which GPU it was.
        //
        // SAFETY: null is one of the two values the contract permits.
        let vendor = unsafe { device_vendor(core::ptr::null_mut()) };

        assert_eq!(vendor, None);
    }

    #[test]
    fn every_adapter_in_this_machine_is_described() {
        // A test about the machine it runs on, which is the only way to check
        // that the projection was driven correctly: whatever adapters exist,
        // each has to come back with a name and a vendor rather than an empty
        // string. A machine with no adapters at all — some virtualised CI
        // images — passes vacuously, which is why the no-hardware behaviour is
        // tested from injected facts instead.
        let adapters = adapters().expect("DXGI can be asked on a Windows machine");

        for adapter in &adapters {
            assert!(
                !adapter.description().is_empty(),
                "an adapter came back with no description: {adapter:?}"
            );
            assert!(
                !adapter.description().contains('\0'),
                "the description kept its padding: {:?}",
                adapter.description()
            );
        }
    }

    /// The inference [`crate::adapter::capture_adapter`] rests on, checked
    /// against the machine it is running on.
    ///
    /// That function decides which encoders a report calls available, and it is
    /// not a measurement: it reads Microsoft's documentation for
    /// `D3D11CreateDevice` — pass `NULL` "to use the default adapter, which is
    /// the first adapter that is enumerated by `IDXGIFactory1::EnumAdapters`" —
    /// and applies it to the enumeration [`adapters`] already performs. This
    /// creates the device `clipped_capture` creates, by the same call with the
    /// same arguments, and asks which adapter it actually landed on.
    ///
    /// **It can fail.** On a machine with more than one adapter, a
    /// `capture_adapter` that took the last entry, or the one with the most
    /// video memory, or any order other than DXGI's own, gives a different
    /// answer from the device — which is the whole point of asking the device.
    ///
    /// A machine with no hardware adapter skips: the inference has nothing to
    /// be wrong about there, and the report it feeds has no hardware encoder in
    /// it either.
    #[test]
    fn the_device_capture_creates_lands_on_the_adapter_that_is_inferred_for_it() {
        use windows::Win32::Foundation::HMODULE;
        use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
        use windows::Win32::Graphics::Direct3D11::{
            D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
        };

        let adapters = adapters().expect("DXGI can be asked on a Windows machine");
        let Some(inferred) = crate::adapter::capture_adapter(&adapters) else {
            eprintln!(
                "SKIPPED (adapter): this machine reports no adapter that could host a hardware \
                 encoder, so capture has none to land on"
            );
            return;
        };

        // Exactly the call `crates/capture/src/windows/device.rs` makes: no
        // adapter, the hardware driver type, and BGRA support. Copying its
        // arguments rather than approximating them is what makes this a test of
        // that device rather than of a device like it.
        let mut device: Option<ID3D11Device> = None;
        // SAFETY: no adapter is named, so a driver type must be; the module
        // handle is null as that requires, no feature level list is given, and
        // the out parameter is a live local of the projected type. The context
        // and feature level out parameters are absent because neither is wanted.
        let created = unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
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
            eprintln!(
                "SKIPPED (adapter): this machine would not create a hardware Direct3D 11 device, \
                 so capture could not either"
            );
            return;
        };

        let dxgi: IDXGIDevice = device
            .cast()
            .expect("a Direct3D 11 device is a DXGI device");
        // SAFETY: `dxgi` is the live device just created, and both calls are
        // ordinary queries returning by value or as a reference dropped here.
        let description = unsafe {
            dxgi.GetAdapter()
                .expect("a device knows its adapter")
                .GetDesc()
                .expect("an adapter describes itself")
        };
        let landed = AdapterId::from_luid(
            description.AdapterLuid.LowPart,
            description.AdapterLuid.HighPart,
        );

        assert_eq!(
            landed,
            inferred.id(),
            "capture will create its device on adapter {landed}, and detection believes it will \
             be on {} ({}). Every encoder that is not that adapter's vendor is refused a \
             recording, so an inference that is wrong here reports the wrong encoders as \
             available",
            inferred.id(),
            inferred.description()
        );
    }

    /// The refusal all three hardware backends share, driven against every
    /// adapter this machine has.
    ///
    /// Each adapter is asked for a vendor it is not, which is the arrangement
    /// issue #443 is about, and the sentence has to name both — the vendor that
    /// encodes and the vendor whose device arrived — because a status code that
    /// names neither is what sent the original report looking for a driver
    /// fault.
    #[test]
    fn a_device_from_another_vendors_adapter_is_refused_by_naming_both_vendors() {
        use crate::codec::EncoderKind;
        use crate::error::EncodeContext;
        use crate::windows::device::ProbeDevice;

        let adapters = adapters().expect("DXGI can be asked on a Windows machine");
        let usable: Vec<_> = adapters
            .iter()
            .filter(|adapter| adapter.can_host_hardware_encoder())
            .collect();
        if usable.is_empty() {
            eprintln!("SKIPPED (adapter): no adapter here could host a hardware encoder");
            return;
        }

        let context = EncodeContext::new(
            EncoderKind::Amf,
            crate::codec::Codec::H264,
            crate::codec::Resolution::new(1280, 720),
        );
        let fail = |kind| EncodeError::new(context, kind);

        for adapter in usable {
            let Ok(probe) = ProbeDevice::on(adapter.id()) else {
                continue;
            };
            let device = probe.as_graphics_device();

            // The vendor this adapter is not. Picked from the adapter itself so
            // that the test asks a real question on whatever silicon is here.
            let wanted = if adapter.vendor() == Vendor::Nvidia {
                Vendor::Amd
            } else {
                Vendor::Nvidia
            };

            // SAFETY: `device` borrows the live probe device for the length of
            // the call.
            let error = unsafe { require_adapter_vendor(&device, wanted, "TestEncoder", &fail) }
                .expect_err("an adapter is not the vendor it is not");
            let message = error.to_string();

            assert!(
                message.contains(&wanted.to_string())
                    && message.contains(&adapter.vendor().to_string()),
                "the refusal has to name the vendor that encodes and the vendor whose device \
                 arrived, or it is another status code nobody can act on: {message}"
            );

            // And the same adapter asked for its own vendor is accepted, or the
            // assertion above would pass on a function that refused everything.
            //
            // SAFETY: `device` borrows the live probe device for the length of
            // the call, exactly as above.
            let accepted =
                unsafe { require_adapter_vendor(&device, adapter.vendor(), "TestEncoder", &fail) };
            assert!(
                accepted.is_ok(),
                "{} was refused its own adapter",
                adapter.vendor()
            );
        }
    }
}
