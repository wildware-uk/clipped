//! Tests for the Quick Sync backend.
//!
//! # What runs here, and what cannot
//!
//! There is no Intel GPU on the machine this backend was written on (issue
//! #17), so nothing here encodes a frame. What these tests do check is
//! everything that does not need Intel silicon, and they check it against the
//! real machine rather than against a mock:
//!
//! - the configuration is refused before a device is touched, for every reason
//!   it can be refused for;
//! - each codec is initialised with the extension buffers whose settings apply
//!   to it and with no others;
//! - a Direct3D 11 device created on a *different* vendor's adapter is refused
//!   by name, which is the whole of this backend's hybrid-graphics behaviour and
//!   is exercised here on a real NVIDIA or AMD device;
//! - detection reports Quick Sync as unavailable, with a reason, and the
//!   encoders that do work are still recommended.
//!
//! The translation from Clipped's configuration to oneVPL's structures is
//! tested in `settings.rs`, the runtime search in `api.rs`, and the frame
//! allocator in `allocator.rs` — where the Direct3D 11 half really does
//! allocate and release textures, on whatever GPU is present.
//!
//! # Why a missing Intel GPU is not a failure under `CLIPPED_REQUIRE_ENCODER`
//!
//! That lever exists so "the encoder tests passed" cannot quietly mean "the
//! encoder tests did nothing", and it is set when the NVENC evidence is
//! produced on this machine. Which vendors' silicon a machine contains is a
//! fact about the machine, in the same way that a card without an AV1 encoder
//! is: the NVENC tests treat that as `unsupported_here` rather than as a
//! failure, and the absence of an Intel adapter is the same kind of fact. The
//! tests that would need one say so on standard error and the pull request says
//! it too, which is where a reader looks for what was not checked.

use core::ffi::c_void;
use std::io::Write as _;

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory1};

use crate::backend::VideoEncoder as _;
use crate::codec::{Codec, EncoderKind, Resolution, Vendor};
use crate::config::{BitRate, EncoderConfig, FrameRate, RateControl, SurfaceFormat};
use crate::detection::{detect, Availability, Unavailable};
use crate::error::{EncodeError, EncodeErrorKind};
use crate::frame::{DeviceKind, GraphicsDevice};
use crate::probe::{probe, Probing};
use crate::recommendation::recommend;

use super::{sys, InitParams, QuickSyncEncoder};

/// A configuration the tests vary one thing at a time from.
fn config(codec: Codec, resolution: Resolution) -> EncoderConfig {
    EncoderConfig::new(
        codec,
        resolution,
        FrameRate::FPS_60,
        RateControl::constant(BitRate::megabits_per_second(20)),
    )
}

// ---------------------------------------------------------------------------
// Refusals that need no device at all
// ---------------------------------------------------------------------------

#[test]
fn an_odd_picture_size_is_refused_with_a_reason() {
    // 4:2:0 chroma has no representation for an odd dimension. oneVPL would
    // refuse it too, with MFX_ERR_INVALID_VIDEO_PARAM and no mention of which
    // parameter.
    let error = open_expecting_failure(config(Codec::H264, Resolution::new(1919, 1080)));

    assert!(
        matches!(error.kind(), EncodeErrorKind::Configuration { .. }),
        "{error}"
    );
    assert!(
        error.to_string().contains("odd dimension"),
        "the message does not say what is wrong: {error}"
    );
    assert!(
        error
            .to_string()
            .starts_with("Intel Quick Sync could not encode 1919x1080 H.264"),
        "the message does not name the encoder, size and codec: {error}"
    );
}

#[test]
fn a_picture_with_no_size_is_refused() {
    let error = open_expecting_failure(config(Codec::Hevc, Resolution::new(0, 1080)));

    assert!(
        matches!(error.kind(), EncodeErrorKind::Configuration { .. }),
        "{error}"
    );
}

#[test]
fn a_picture_too_large_for_the_interface_is_refused() {
    // oneVPL counts picture dimensions in sixteen bits, and the surface it
    // allocates is the picture rounded up to a multiple of sixteen — so the
    // limit is a little below 65535 rather than at it. Without this check the
    // size would wrap on the way into `mfxFrameInfo` and the encoder would be
    // configured for a picture nobody asked for.
    let error = open_expecting_failure(config(Codec::H264, Resolution::new(65_530, 1080)));

    assert!(
        matches!(error.kind(), EncodeErrorKind::Configuration { .. }),
        "{error}"
    );
    assert!(
        error
            .to_string()
            .contains("larger than oneVPL can describe"),
        "{error}"
    );
}

#[test]
fn a_ten_bit_surface_is_refused_by_name() {
    // HDR is not supported, and the way to say so is to name the format that
    // would work rather than to fail somewhere inside the runtime
    // (https://github.com/wildware-uk/clipped/issues/99).
    let error = open_expecting_failure(
        config(Codec::Hevc, Resolution::new(1920, 1080))
            .with_source_format(SurfaceFormat::Rgb10A2Unorm),
    );

    assert!(
        matches!(
            error.kind(),
            EncodeErrorKind::SurfaceFormatUnsupported { .. }
        ),
        "{error}"
    );
    assert!(error.to_string().contains("BGRA8 unorm"), "{error}");
}

#[test]
fn a_null_device_is_refused_before_anything_dereferences_it() {
    // SAFETY: null is a value `GraphicsDevice` documents as something a caller
    // may not pass; this test is what proves the encoder says so rather than
    // dereferencing it.
    let device = unsafe { GraphicsDevice::new(DeviceKind::D3d11, core::ptr::null_mut()) };
    let error = QuickSyncEncoder::open(&device, config(Codec::H264, Resolution::new(1920, 1080)))
        .expect_err("a null device cannot be encoded from");

    assert!(
        error.to_string().contains("null"),
        "the message does not say what is wrong: {error}"
    );
}

// ---------------------------------------------------------------------------
// What each codec is initialised with
// ---------------------------------------------------------------------------

#[test]
fn a_codec_is_only_given_the_extension_buffers_its_settings_apply_to() {
    // `mfxExtCodingOption2::RepeatPPS` is documented in `mfxstructures.h` as an
    // AVC control, so attaching that buffer to an HEVC or AV1 initialisation
    // would claim a setting the codec does not have — and is a plausible
    // MFX_ERR_UNSUPPORTED from `MFXVideoENCODE_Init` for nothing in return.
    // What each codec does about in-band parameter sets is on
    // `settings::coding_option2`.
    #[allow(clippy::cast_sign_loss)]
    let signal = sys::MFX_EXTBUFF_VIDEO_SIGNAL_INFO as sys::mfxU32;
    #[allow(clippy::cast_sign_loss)]
    let options = sys::MFX_EXTBUFF_CODING_OPTION2 as sys::mfxU32;
    #[allow(clippy::cast_sign_loss)]
    let av1 = sys::MFX_EXTBUFF_AV1_BITSTREAM_PARAM as sys::mfxU32;

    for (codec, expected) in [
        (Codec::H264, vec![signal, options]),
        (Codec::Hevc, vec![signal]),
        (Codec::Av1, vec![signal, av1]),
    ] {
        let mut params = InitParams::new(&config(codec, Resolution::new(1920, 1080)));
        let video = params.video();

        // SAFETY: `params` owns the structure and outlives this borrow, and
        // every pointer in its extension array addresses one of its own fields.
        let attached: Vec<sys::mfxU32> = unsafe {
            (0..usize::from((*video).NumExtParam))
                .map(|index| (*(*(*video).ExtParam.add(index))).BufferId)
                .collect()
        };

        assert_eq!(
            attached, expected,
            "{codec} is initialised with the wrong extension buffers"
        );
    }
}

// ---------------------------------------------------------------------------
// Adapter selection, which is the whole of the hybrid-graphics behaviour
// ---------------------------------------------------------------------------

#[test]
fn a_device_on_another_vendors_adapter_is_refused_by_name() {
    // The test this machine can actually run, and the one that matters for
    // hybrid graphics: a Quick Sync session encodes on the Intel adapter and on
    // no other, so a device created on a discrete GPU has to be refused with a
    // sentence that says which adapter it was created on and what to do about
    // it — not with a status code from inside the runtime.
    let Some((device, vendor)) = device_on_a_non_intel_adapter() else {
        let _ = writeln!(
            std::io::stderr(),
            "SKIPPED (encoder, hardware): every adapter in this machine is Intel, so there is no \
             wrong adapter to be refused"
        );
        return;
    };

    // SAFETY: the device is alive for the whole of this test and no encoder
    // outlives it — `open` fails, so none is created at all.
    let handle = unsafe { GraphicsDevice::new(DeviceKind::D3d11, device.as_raw()) };
    let error = QuickSyncEncoder::open(&handle, config(Codec::H264, Resolution::new(1920, 1080)))
        .expect_err("Quick Sync cannot encode from a device on another vendor's adapter");

    let message = error.to_string();
    assert!(
        matches!(error.kind(), EncodeErrorKind::Configuration { .. }),
        "{message}"
    );
    assert!(
        message.contains(&vendor.to_string()),
        "the message does not name the adapter the device was created on: {message}"
    );
    assert!(
        message.contains("Intel"),
        "the message does not say which adapter would work: {message}"
    );
}

#[test]
fn a_recording_cannot_start_on_this_machine_and_says_why() {
    // The end-to-end refusal, from the outside: whatever this machine is,
    // `open` either succeeds on an Intel adapter or fails with a sentence that
    // names the encoder, the codec and the resolution before it says anything
    // else (AGENTS.md section 15).
    let Some(device) = any_hardware_device() else {
        let _ = writeln!(
            std::io::stderr(),
            "SKIPPED (encoder, hardware): this machine has no Direct3D 11 hardware adapter"
        );
        return;
    };

    // SAFETY: as above — the device outlives every use of the handle.
    let handle = unsafe { GraphicsDevice::new(DeviceKind::D3d11, device.as_raw()) };
    match QuickSyncEncoder::open(&handle, config(Codec::H264, Resolution::new(1280, 720))) {
        Ok(encoder) => {
            // Only reachable on a machine whose first hardware adapter is
            // Intel, which is not the machine this was written on. Nothing
            // asserts on encoding, because no frame is submitted here — but a
            // session that opened and produced no SPS/PPS is a session that
            // cannot be muxed, and that is worth catching on the hardware this
            // backend most needs coverage from rather than letting the branch
            // pass with no assertion at all.
            assert_eq!(encoder.encoder(), EncoderKind::QuickSync);
            assert!(
                !encoder.parameter_sets().is_empty(),
                "an H.264 session opened and read back no sequence or picture parameter set, so \
                 nothing could store them out of band: {encoder:?}"
            );
            let _ = writeln!(
                std::io::stderr(),
                "a Quick Sync session opened on this machine: {encoder:?}, {} bytes of parameter \
                 sets",
                encoder.parameter_sets().len()
            );
        }
        Err(error) => {
            assert!(
                error
                    .to_string()
                    .starts_with("Intel Quick Sync could not encode 1280x720 H.264: "),
                "the failure does not name what was being attempted: {error}"
            );
            let _ = writeln!(std::io::stderr(), "Quick Sync is unavailable here: {error}");
        }
    }
}

// ---------------------------------------------------------------------------
// Detection, and what the absence of Quick Sync does to everything else
// ---------------------------------------------------------------------------

#[test]
fn detection_reports_quick_sync_with_a_reason_when_it_is_not_there() {
    let facts = probe(
        &crate::windows::WindowsProbe::new(),
        // Detection reasons only: no session is needed to learn that there is no
        // Intel adapter, and opening one here would cost a slot for nothing.
        Probing::WithoutSessions,
    )
    .expect("this machine can be probed");
    let report = detect(&facts);
    let quick_sync = report
        .encoder(EncoderKind::QuickSync)
        .expect("every encoder family is in the report");

    let has_intel = report
        .adapters()
        .iter()
        .any(|adapter| adapter.vendor() == Vendor::Intel);

    // Where a recording's frames will be, which is what decides whether an
    // encoder that is present can be opened at all (issue #443).
    let captures_on_intel = report
        .adapters()
        .iter()
        .find(|adapter| adapter.can_host_hardware_encoder())
        .is_some_and(|adapter| adapter.vendor() == Vendor::Intel);

    match quick_sync.availability() {
        Availability::Available => {
            assert!(
                has_intel,
                "Quick Sync is reported available on a machine with no Intel adapter"
            );
            assert!(
                captures_on_intel,
                "Quick Sync is reported available on a machine whose frames will be captured \
                 on somebody else's adapter, and `open` refuses exactly that device"
            );
        }
        Availability::OnAnotherAdapter { capture } => {
            assert!(
                has_intel,
                "an encoder said to be on another adapter still has to be an encoder that is \
                 here at all"
            );
            assert_ne!(
                capture,
                Vendor::Intel,
                "the adapter frames are captured on is Intel's, so this is where Quick Sync \
                 would work rather than where it would not"
            );
        }
        Availability::Unavailable(reason) => {
            if !has_intel {
                assert_eq!(
                    reason,
                    Unavailable::NoVendorAdapter,
                    "a machine with no Intel adapter should say so rather than blame the driver"
                );
            }
            assert!(
                !reason.to_string().is_empty(),
                "an unavailable encoder with no reason is a support question nobody can answer"
            );
        }
    }
}

#[test]
fn the_encoders_that_work_are_still_recommended() {
    // Quick Sync existing as a backend must not change what a machine without
    // it is told to use. On the machine this was written on that is NVENC; on a
    // machine with no hardware encoder at all it is the software fallback, which
    // is why this asserts the property rather than a vendor.
    let facts = probe(
        &crate::windows::WindowsProbe::new(),
        // Detection reasons only: no session is needed to learn that there is no
        // Intel adapter, and opening one here would cost a slot for nothing.
        Probing::WithoutSessions,
    )
    .expect("this machine can be probed");
    let report = detect(&facts);
    let ranking = recommend(&report);

    assert!(
        !ranking.is_empty(),
        "the software encoder is available on every machine, so this is never empty"
    );

    let available: Vec<EncoderKind> = report
        .encoders()
        .iter()
        .filter(|encoder| encoder.availability().is_available())
        .map(crate::detection::EncoderReport::kind)
        .collect();
    assert!(
        available.contains(&ranking[0].encoder()),
        "the recommendation chose an encoder the report says is unavailable"
    );

    let has_intel = report
        .adapters()
        .iter()
        .any(|adapter| adapter.vendor() == Vendor::Intel);
    if !has_intel {
        assert!(
            ranking
                .iter()
                .all(|choice| choice.encoder() != EncoderKind::QuickSync),
            "a machine with no Intel adapter was offered Quick Sync"
        );
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Opens with a device that is never looked at, and expects a refusal.
///
/// Every configuration checked here is checked before `open` touches the
/// device — deliberately, so that a bad configuration does not need a graphics
/// device to be reported — and this passing a dangling handle is what keeps
/// that ordering honest.
fn open_expecting_failure(config: EncoderConfig) -> EncodeError {
    // SAFETY: the handle is never dereferenced: `open` validates the
    // configuration before it looks at the device.
    let device =
        unsafe { GraphicsDevice::new(DeviceKind::D3d11, core::ptr::dangling_mut::<c_void>()) };
    QuickSyncEncoder::open(&device, config).expect_err("this configuration cannot be encoded")
}

/// A Direct3D 11 device on the first hardware adapter that is not Intel, with
/// that adapter's vendor.
fn device_on_a_non_intel_adapter() -> Option<(ID3D11Device, Vendor)> {
    adapters()
        .into_iter()
        .find(|(_, vendor)| *vendor != Vendor::Intel && *vendor != Vendor::Microsoft)
        .and_then(|(adapter, vendor)| device_on(&adapter).map(|device| (device, vendor)))
}

/// A Direct3D 11 device on any hardware adapter.
///
/// Shared with the allocator's tests, which need a real device to create real
/// textures on and have the same reason to skip the Basic Render Driver.
pub(super) fn any_hardware_device() -> Option<ID3D11Device> {
    adapters()
        .into_iter()
        .find(|(_, vendor)| *vendor != Vendor::Microsoft)
        .and_then(|(adapter, _)| device_on(&adapter))
}

/// Every adapter DXGI reports, with its vendor.
fn adapters() -> Vec<(IDXGIAdapter, Vendor)> {
    // SAFETY: the factory is created and released by this function, and takes
    // no arguments beyond the interface identifier the generic supplies.
    let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else {
        return Vec::new();
    };

    let mut found = Vec::new();
    for index in 0.. {
        // SAFETY: the factory is alive for the whole loop, and `EnumAdapters`
        // is documented to fail once `index` is past the last adapter, which is
        // the loop's only exit.
        let Ok(adapter) = (unsafe { factory.EnumAdapters(index) }) else {
            break;
        };
        // SAFETY: `adapter` is live and `GetDesc` returns by value.
        let Ok(description) = (unsafe { adapter.GetDesc() }) else {
            continue;
        };
        found.push((adapter, Vendor::from_pci_id(description.VendorId)));
    }
    found
}

/// A Direct3D 11 device on one adapter.
fn device_on(adapter: &IDXGIAdapter) -> Option<ID3D11Device> {
    let mut device: Option<ID3D11Device> = None;

    // SAFETY: the adapter is live, the driver type is the one the
    // documentation requires when an adapter is given, and `device` is a live
    // local out-parameter. The device is released when the test drops it.
    unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            windows::Win32::Foundation::HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
    }
    .ok()?;

    device
}
