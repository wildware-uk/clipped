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
use crate::probe::ProbeError;

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
}
