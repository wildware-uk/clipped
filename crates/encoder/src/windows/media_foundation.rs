//! Asking Windows which hardware encoders the display drivers registered.
//!
//! This is the measurement that makes per-codec support a fact rather than a
//! guess. Every hardware encoder on Windows registers a Media Foundation
//! transform per codec it can produce, and enumerating them by output type asks
//! the driver that is installed right now — so a driver update that adds AV1
//! shows up the next time this runs, and one that never had it never claims it.
//!
//! # What this costs, and what it deliberately does not do
//!
//! Enumeration starts Media Foundation and creates activation objects. It does
//! **not** activate them, which is what would create a transform, allocate GPU
//! memory and take an encode session slot from the game that may be running.
//! The trade-off is that this cannot report bitrate ranges, B-frame support or
//! maximum resolutions — those need a live transform — so those come from the
//! reference table and are labelled inferred (`docs/encoder-capabilities.md`).
//!
//! # Ownership
//!
//! `MFTEnumEx` hands back a task-allocated array of interface pointers, and
//! this module owns both: each pointer is moved out and released when it drops,
//! and the array itself is freed with `CoTaskMemFree` (AGENTS.md section 58).
//! Media Foundation and COM are shut down by guards, so an early return cannot
//! leave either running.

use core::ptr;

use windows::core::{GUID, PWSTR};
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, MFMediaType_Video, MFShutdown, MFStartup, MFTEnumEx,
    MFT_ENUM_HARDWARE_VENDOR_ID_Attribute, MFT_FRIENDLY_NAME_Attribute, MFVideoFormat_AV1,
    MFVideoFormat_H264, MFVideoFormat_HEVC, MFSTARTUP_NOSOCKET, MFT_CATEGORY_VIDEO_ENCODER,
    MFT_ENUM_ADAPTER_LUID, MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER,
    MFT_REGISTER_TYPE_INFO, MF_VERSION,
};
use windows::Win32::System::Com::{
    CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_MULTITHREADED,
};

use crate::adapter::AdapterId;
use crate::codec::{Codec, Vendor};
use crate::probe::{HardwareEncoder, ProbeError};

/// The Media Foundation subtype for each codec this project can ask for.
const SUBTYPES: [(Codec, GUID); 3] = [
    (Codec::H264, MFVideoFormat_H264),
    (Codec::Hevc, MFVideoFormat_HEVC),
    (Codec::Av1, MFVideoFormat_AV1),
];

/// The prefix Media Foundation puts in front of a hardware transform's PCI
/// vendor identifier, as in `VEN_10DE`.
const VENDOR_PREFIX: &str = "VEN_";

/// `RPC_E_CHANGED_MODE`: COM is already initialised in the other threading
/// model. Not a failure — the apartment that exists is used as it is.
const RPC_E_CHANGED_MODE: i32 = 0x8001_0106_u32 as i32;

/// Every hardware video encoder Windows lists, by codec.
pub(super) fn hardware_encoders() -> Result<Vec<HardwareEncoder>, ProbeError> {
    let _com = ComApartment::enter()?;
    let _media_foundation = MediaFoundation::start()?;

    let mut encoders = Vec::new();
    for (codec, subtype) in SUBTYPES {
        encoders.extend(encoders_for(codec, subtype)?);
    }
    Ok(encoders)
}

/// The hardware encoders that produce one codec.
fn encoders_for(codec: Codec, subtype: GUID) -> Result<Vec<HardwareEncoder>, ProbeError> {
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: subtype,
    };

    let mut activates: *mut Option<IMFActivate> = ptr::null_mut();
    let mut count = 0_u32;

    // SAFETY: `output` outlives the call; the two out parameters are live for
    // its duration; and the flags are documented `MFT_ENUM_FLAG` values. On
    // success the callee has allocated an array of `count` interface pointers
    // with the task allocator, which the block below takes ownership of.
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            None,
            Some(&output),
            &mut activates,
            &mut count,
        )
    }
    .map_err(|error| api_error("MFTEnumEx", &error))?;

    if activates.is_null() {
        return Ok(Vec::new());
    }

    let mut encoders = Vec::new();
    for index in 0..count as usize {
        // SAFETY: `index` is below `count`, which is how many pointers
        // `MFTEnumEx` wrote, so this reads inside the array. `read` moves the
        // reference out rather than copying it: each element is taken exactly
        // once and released when `activate` drops at the end of the iteration,
        // which is the release the caller of `MFTEnumEx` owes.
        let activate = unsafe { ptr::read(activates.add(index)) };
        if let Some(activate) = activate {
            if let Some(encoder) = describe(&activate, codec) {
                // A driver may register the same transform more than once —
                // AMD's registers its H.264 encoder twice on the machine this
                // was written on — and two entries that agree about vendor,
                // adapter, codec and name say nothing the first did not. They
                // are dropped here rather than in the report so that the count
                // a caller sees is a count of encoders.
                if !encoders.contains(&encoder) {
                    encoders.push(encoder);
                }
            }
        }
    }

    // SAFETY: `activates` was allocated by `MFTEnumEx` with the task allocator
    // and every pointer inside it has been moved out above, so freeing the
    // array now frees only the array.
    unsafe { CoTaskMemFree(Some(activates.cast())) };

    Ok(encoders)
}

/// Reads what is known about one transform without activating it.
///
/// Returns `None` for a transform with no vendor identifier: those are the
/// software transforms that slipped past the hardware filter, and a
/// [`HardwareEncoder`] that cannot be attributed to a vendor is no use to
/// detection.
fn describe(activate: &IMFActivate, codec: Codec) -> Option<HardwareEncoder> {
    let vendor = vendor(activate)?;
    let name = string_attribute(activate, &MFT_FRIENDLY_NAME_Attribute)
        .unwrap_or_else(|| format!("{vendor} hardware encoder"));

    Some(HardwareEncoder::new(vendor, adapter(activate), codec, name))
}

/// The PCI vendor behind a transform, from `MFT_ENUM_HARDWARE_VENDOR_ID`.
fn vendor(activate: &IMFActivate) -> Option<Vendor> {
    let raw = string_attribute(activate, &MFT_ENUM_HARDWARE_VENDOR_ID_Attribute)?;
    let digits = raw.trim().strip_prefix(VENDOR_PREFIX)?;
    u32::from_str_radix(digits, 16)
        .ok()
        .map(Vendor::from_pci_id)
}

/// The adapter a transform belongs to, where Windows reports one.
///
/// `MFT_ENUM_ADAPTER_LUID` is not set by every driver or on every Windows
/// version, and its absence is ordinary: attribution then falls back to the
/// vendor, which is always present.
fn adapter(activate: &IMFActivate) -> Option<AdapterId> {
    let mut luid = [0_u8; 8];
    // SAFETY: the key is a `'static` constant from the projection and the
    // buffer is exactly the eight bytes a `LUID` occupies; `GetBlob` fails
    // rather than writing past a buffer that is too small.
    unsafe { activate.GetBlob(&MFT_ENUM_ADAPTER_LUID, &mut luid, None) }.ok()?;

    let low = u32::from_le_bytes([luid[0], luid[1], luid[2], luid[3]]);
    let high = i32::from_le_bytes([luid[4], luid[5], luid[6], luid[7]]);
    Some(AdapterId::from_luid(low, high))
}

/// Reads a string attribute, freeing what Media Foundation allocated for it.
fn string_attribute(activate: &IMFActivate, key: &GUID) -> Option<String> {
    let mut value = PWSTR::null();
    let mut length = 0_u32;

    // SAFETY: `key` outlives the call, and the two out parameters are live for
    // its duration. On success `value` points at a task-allocated string that
    // this function frees below; on failure it is left untouched and nothing is
    // freed.
    unsafe { activate.GetAllocatedString(key, &mut value, &mut length) }.ok()?;

    // SAFETY: `value` is the nul-terminated string just allocated, and it is
    // read before being freed.
    let text = unsafe { value.to_string() }.ok();

    // SAFETY: `value` came from the task allocator, is freed exactly once, and
    // is not used afterwards.
    unsafe { CoTaskMemFree(Some(value.0.cast())) };

    text
}

/// COM, entered for as long as this value lives.
///
/// Media Foundation needs an initialised apartment. A process that already has
/// one in the other threading model is left as it is — `RPC_E_CHANGED_MODE` is
/// the documented way to be told that — and in that case nothing is
/// uninitialised on the way out, because this did not initialise it.
#[derive(Debug)]
struct ComApartment {
    owned: bool,
}

impl ComApartment {
    fn enter() -> Result<Self, ProbeError> {
        // SAFETY: no reserved parameter is passed and the flag is one of the
        // documented `COINIT` values. The matching `CoUninitialize` is in
        // `Drop`, and only when this call is what initialised the apartment.
        let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };

        if result.is_ok() {
            return Ok(Self { owned: true });
        }
        if result.0 == RPC_E_CHANGED_MODE {
            return Ok(Self { owned: false });
        }
        Err(ProbeError::Api {
            operation: "CoInitializeEx",
            code: result.0,
            message: String::new(),
        })
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: balances the `CoInitializeEx` that succeeded in `enter`,
            // once, on the same thread.
            unsafe { CoUninitialize() };
        }
    }
}

/// Media Foundation, started for as long as this value lives.
#[derive(Debug)]
struct MediaFoundation;

impl MediaFoundation {
    fn start() -> Result<Self, ProbeError> {
        // SAFETY: `MF_VERSION` is the version constant the projection was
        // generated against, and `MFSTARTUP_NOSOCKET` is a documented flag —
        // the network stack is not wanted for enumerating transforms. The
        // matching `MFShutdown` is in `Drop`.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) }
            .map_err(|error| api_error("MFStartup", &error))?;
        Ok(Self)
    }
}

impl Drop for MediaFoundation {
    fn drop(&mut self) {
        // SAFETY: balances the `MFStartup` that succeeded in `start`, once.
        if let Err(error) = unsafe { MFShutdown() } {
            tracing::debug!(%error, "Media Foundation did not shut down cleanly");
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
    fn a_vendor_prefix_is_read_as_a_pci_identifier() {
        // The parsing half of `vendor`, which is the part that can be wrong
        // without a GPU to be wrong about. The interface half is exercised by
        // `enumeration_answers_on_this_machine` below.
        assert_eq!(
            u32::from_str_radix("VEN_10DE".strip_prefix(VENDOR_PREFIX).unwrap(), 16)
                .map(Vendor::from_pci_id),
            Ok(Vendor::Nvidia)
        );
    }

    #[test]
    fn enumeration_answers_on_this_machine() {
        // What this can assert is narrow on purpose: which encoders exist
        // depends on the machine, and a test that demanded NVENC would fail on
        // a CI runner with no GPU. What must hold everywhere is that the call
        // completes, that COM and Media Foundation are started and stopped
        // without a crash, and that every encoder that does come back is
        // attributed to a vendor and named.
        let encoders = hardware_encoders().expect("Media Foundation can be asked on Windows");

        for encoder in &encoders {
            assert!(
                !encoder.name().is_empty(),
                "a transform came back unnamed: {encoder:?}"
            );
            assert_ne!(
                encoder.vendor(),
                Vendor::Microsoft,
                "a Microsoft transform is not a hardware encoder: {encoder:?}"
            );
        }
    }

    #[test]
    fn enumeration_can_be_repeated() {
        // Enumeration takes and releases a COM apartment, a Media Foundation
        // startup and one interface pointer per transform, ten times over. What
        // this catches is a release that is wrong rather than merely absent: a
        // second `CoTaskMemFree` of the activation array fails it with a heap
        // corruption, which is how it was checked.
        //
        // What it does not catch is written down so that nobody trusts it
        // further than it goes. `MFStartup` is reference counted, so a leaked
        // `MFShutdown` leaves every later round working perfectly — verified by
        // replacing the guard with `mem::forget` and watching this pass. A
        // leaked interface pointer is invisible for the same reason. Those rest
        // on the `SAFETY` comments and on review, not on this test.
        for _ in 0..10 {
            hardware_encoders().expect("Media Foundation can be asked repeatedly");
        }
    }
}
