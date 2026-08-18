//! Encoding frames that were captured on another adapter.
//!
//! Every hardware backend here refuses a Direct3D 11 device created on somebody
//! else's silicon, and each vendor's runtime refuses it first and less kindly:
//! AMF answers `AMF_INVALID_ARG` and NVENC answers `NV_ENC_ERR_NO_ENCODE_DEVICE`
//! ([issue #443](https://github.com/wildware-uk/clipped/issues/443)). On a
//! machine with two GPUs, capture creates its device on the default adapter —
//! measured on the development machine, `D3D11CreateDevice` with no adapter
//! lands on the RTX 4090 — so the AMD encoder cannot be opened at all, however
//! plainly a user asked for it.
//!
//! This module is the way out that does not need capture to move: the encoder
//! gets a device of its own on its own adapter, and every frame is carried
//! across.
//!
//! ```text
//! capture's ID3D11Texture2D  (video memory, adapter A)
//!         │  CopyResource + Map          crate::readback
//! system memory
//!         │  UpdateSubresource           this module
//! the encoder's ID3D11Texture2D (video memory, adapter B)
//! ```
//!
//! # Why through system memory
//!
//! Because Direct3D 11 has no other route. A shared NT handle is how one device
//! hands a texture to another, and it was measured on the two-GPU development
//! machine before this was written: a texture created on the NVIDIA adapter with
//! `D3D11_RESOURCE_MISC_SHARED_NTHANDLE`, whose handle
//! `ID3D11Device1::OpenSharedResource1` on the AMD adapter refuses with
//! `E_INVALIDARG`. Cross-adapter sharing is a Direct3D 12 heap flag with no
//! Direct3D 11 equivalent, and reaching for it would mean a second graphics API
//! inside the encoder. So the pixels go through the CPU, and the cost of that is
//! measured rather than described — see [`Bridged::shut_down`], which logs it,
//! and `docs/encoder-pipeline.md`, which records what it came to.
//!
//! # When this is used, and when it must not be
//!
//! Only when the alternative is not recording at all. The caller decides:
//! `clipped-session` reaches for this when a user named an encoder and asked for
//! no substitute, and never for the ranked candidates of an automatic choice — a
//! machine with a hardware encoder on capture's own adapter must use that one
//! for nothing rather than this one for a copy per frame.
//!
//! # Ownership
//!
//! [`Bridged`] owns the encoder it wraps and the [`AdapterBridge`] under it, in
//! that order, so the session inside the encoder is closed before the device it
//! was opened against is released. The bridge owns its device, its immediate
//! context, the texture it copies into and the readback staging texture on the
//! caller's device. It owns no captured frame: the caller's texture is read
//! inside [`AdapterBridge::carry`] and never retained, which is what
//! [`crate::SourceTexture`] requires.

use core::ffi::c_void;
use core::time::Duration;
use std::time::Instant;

use windows::core::Interface as _;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

use crate::adapter::{encoding_adapter, AdapterId};
use crate::backend::VideoEncoder;
use crate::codec::{EncoderKind, Resolution};
use crate::config::{EncoderConfig, SurfaceFormat};
use crate::error::{EncodeContext, EncodeError, EncodeErrorKind};
use crate::frame::{DeviceKind, GraphicsDevice, SourceFrame, SourceTexture, SurfaceKind};
use crate::packet::EncodedPacket;
use crate::readback::Readback;
use crate::software::readback_failure;

/// Opens an encoder for frames that may be on another adapter's device.
///
/// `open` is the caller's own dispatch to one backend — this crate has no
/// factory (`crate::backend`, "There is no factory") — and is called with the
/// device the encoder should be opened against: the caller's own when capture
/// and the encoder are already on one adapter, and a device this function
/// created on the encoder's adapter when they are not.
///
/// **Costs a copy of every frame through system memory when it bridges**, so a
/// caller with a cheaper encoder available should open that one instead.
/// Nothing here decides that; the module documentation says who does.
///
/// # Errors
///
/// [`EncodeErrorKind::Configuration`] when the machine has no adapter from the
/// encoder's vendor to move the frames to, or when they are in a layout this
/// cannot carry, and [`EncodeErrorKind::Api`] when Direct3D refuses the device
/// or the texture. Otherwise whatever `open` returned.
pub fn open_across_adapters(
    capture: &GraphicsDevice,
    encoder: EncoderKind,
    config: EncoderConfig,
    open: impl FnOnce(&GraphicsDevice) -> Result<Box<dyn VideoEncoder>, EncodeError>,
) -> Result<Box<dyn VideoEncoder>, EncodeError> {
    let Some(bridge) = AdapterBridge::required(capture, encoder, &config)? else {
        return open(capture);
    };

    let inner = open(&bridge.device())?;
    tracing::warn!(
        encoder = %encoder.log_encoder_family(),
        resolution = %config.resolution(),
        "this encoder is on another adapter from the capture, so every frame is being copied \
         between them through system memory; an encoder on the capture's own adapter would not \
         need that"
    );
    Ok(Box::new(Bridged { inner, bridge }))
}

/// A device on the encoder's adapter, and the two copies that reach it.
struct AdapterBridge {
    /// The device the encoder is opened against, on its vendor's adapter.
    device: ID3D11Device,
    /// Its immediate context, which is what uploads each frame.
    context: ID3D11DeviceContext,
    /// The texture the encoder binds, on [`Self::device`].
    destination: ID3D11Texture2D,
    /// The staging texture on the *caller's* device, and the copy out of video
    /// memory into it.
    readback: Readback,
    /// Accumulated time in [`Self::carry`].
    elapsed: Duration,
    /// How many frames that covers.
    frames: u64,
}

impl AdapterBridge {
    /// The bridge `encoder` needs to encode frames from `capture`, or [`None`]
    /// when it needs none.
    ///
    /// [`None`] covers three cases that are all "let the backend answer for
    /// itself": a software encoder, which has no adapter; a device already on
    /// the encoder's vendor's adapter, which is the ordinary machine; and a
    /// device that will not say which adapter it is on, where inventing a
    /// mismatch would be a guess (`crate::windows::dxgi::device_vendor`).
    fn required(
        capture: &GraphicsDevice,
        encoder: EncoderKind,
        config: &EncoderConfig,
    ) -> Result<Option<Self>, EncodeError> {
        let context = EncodeContext::new(encoder, config.codec(), config.resolution());
        let fail = |kind| EncodeError::new(context, kind);

        let Some(wanted) = encoder.vendor() else {
            return Ok(None);
        };
        // SAFETY: the handle is the caller's, and `GraphicsDevice` promises it
        // is null or a live `ID3D11Device` that outlives this call.
        let Some(found) = (unsafe { super::dxgi::device_vendor(capture.as_raw()) }) else {
            return Ok(None);
        };
        if found == wanted {
            return Ok(None);
        }

        // The one layout `crate::readback` copies. Refused by name here rather
        // than as a format mismatch three calls later, and unreachable today
        // because no encoder in this crate accepts anything else
        // ([#99](https://github.com/wildware-uk/clipped/issues/99)).
        if config.source_format() != SurfaceFormat::Bgra8Unorm {
            return Err(fail(EncodeErrorKind::Configuration {
                detail: format!(
                    "{} frames are captured on the {found} adapter and {} encodes on {wanted} \
                     graphics; carrying frames across adapters is only implemented for {} ones",
                    config.source_format(),
                    encoder.log_encoder_family(),
                    SurfaceFormat::Bgra8Unorm
                ),
            }));
        }

        let adapters = super::dxgi::adapters().map_err(|error| {
            fail(EncodeErrorKind::Configuration {
                detail: format!("the display adapters could not be enumerated: {error}"),
            })
        })?;
        // The same rule the capability probe uses to decide which adapter an
        // encoder is attributed to, so that the device opened here is the one
        // the report said the encoder was on
        // (`crate::adapter::encoding_adapter`).
        let adapter = encoding_adapter(&adapters, wanted).ok_or_else(|| {
            fail(EncodeErrorKind::Configuration {
                detail: format!(
                    "{} encodes on {wanted} graphics, frames are captured on the {found} adapter, \
                     and this machine has no {wanted} adapter to carry them to",
                    encoder.log_encoder_family()
                ),
            })
        })?;

        let device = create_device(adapter.id()).map_err(|error| {
            fail(EncodeErrorKind::Api {
                operation: "D3D11CreateDevice",
                status: error.code().0,
                status_name: "an HRESULT",
                detail: format!(
                    "no Direct3D 11 device could be created on {}: {error}",
                    adapter.description()
                ),
            })
        })?;

        // SAFETY: the device was created immediately above and is live; the
        // reference it hands back is owned by this value and released on drop.
        let device_context = unsafe { device.GetImmediateContext() }.map_err(|error| {
            fail(EncodeErrorKind::Api {
                operation: "ID3D11Device::GetImmediateContext",
                status: error.code().0,
                status_name: "an HRESULT",
                detail: error.to_string(),
            })
        })?;

        let destination = create_destination(&device, config.resolution()).map_err(|error| {
            fail(EncodeErrorKind::Api {
                operation: "ID3D11Device::CreateTexture2D",
                status: error.code().0,
                status_name: "an HRESULT",
                detail: format!(
                    "a {} texture could not be created on {}: {error}",
                    config.resolution(),
                    adapter.description()
                ),
            })
        })?;

        // SAFETY: `capture` is the caller's device, which `GraphicsDevice`
        // promises stays live for as long as anything opened against it — and
        // this readback is dropped with the encoder that owns it. It takes its
        // own reference.
        let readback = unsafe { Readback::open(capture.as_raw(), config.resolution()) }
            .map_err(|error| fail(readback_failure(error)))?;

        tracing::info!(
            encoder = %encoder.log_encoder_family(),
            capture_adapter = %found,
            encoder_adapter = adapter.description(),
            resolution = %config.resolution(),
            "opened a device on the encoder's own adapter to carry captured frames to"
        );

        Ok(Some(Self {
            device,
            context: device_context,
            destination,
            readback,
            elapsed: Duration::ZERO,
            frames: 0,
        }))
    }

    /// The device on the encoder's adapter, for a backend to open against.
    ///
    /// # Ownership
    ///
    /// The handle borrows this bridge, which owns the only reference to the
    /// device. Nothing opened against it may outlive the bridge, which is why
    /// [`Bridged`] holds the encoder in the field before it.
    fn device(&self) -> GraphicsDevice {
        // SAFETY: the device is live for as long as `self` is, and it is an
        // `ID3D11Device`, which is what `DeviceKind::D3d11` names.
        unsafe { GraphicsDevice::new(DeviceKind::D3d11, self.device.as_raw()) }
    }

    /// Copies one captured frame onto the encoder's adapter.
    ///
    /// Returns the texture the encoder should bind, which belongs to this
    /// bridge and is overwritten by the next call — so the encoder has to be
    /// finished with it by then, which
    /// [`VideoEncoder::submit`](crate::VideoEncoder::submit) promises it is.
    ///
    /// # Errors
    ///
    /// Whatever the copy out of video memory or the upload into it failed with,
    /// carried in `context` so that the recording names the encoder it was
    /// feeding rather than this module.
    ///
    /// # Safety
    ///
    /// `frame`'s texture must be a live `ID3D11Texture2D` on the device this
    /// bridge was built against, as [`crate::SourceTexture`] requires.
    unsafe fn carry(
        &mut self,
        frame: &SourceFrame<'_>,
        context: EncodeContext,
    ) -> Result<*mut c_void, EncodeError> {
        let started = Instant::now();

        // Split so that the closure can borrow the destination and the context
        // while `readback` is borrowed mutably.
        let Self {
            context: device_context,
            destination,
            readback,
            ..
        } = self;

        let upload = |pixels: &[u8], stride: usize| {
            // SAFETY: `destination` was created with one mip level and one
            // array slice, so subresource 0 is the whole of it; the box is
            // absent, meaning the whole resource; and `pixels` holds `stride`
            // bytes for each of the picture's rows, which is what the row pitch
            // argument declares. The pointer is valid until this closure
            // returns, and `UpdateSubresource` copies out of it before it does.
            unsafe {
                device_context.UpdateSubresource(
                    &*destination,
                    0,
                    None,
                    pixels.as_ptr().cast::<c_void>(),
                    u32::try_from(stride).unwrap_or(u32::MAX),
                    0,
                );
                // Submit the copy rather than leaving it in the context's
                // command buffer: the encoder reads this texture through the
                // vendor's runtime, which has a queue of its own, and a frame
                // still sitting here when it does would be the previous picture
                // with nothing to say so.
                device_context.Flush();
            }
        };

        // SAFETY: the caller guarantees the frame's texture is live on the
        // device the readback was opened against, for the length of this call.
        let uploaded = unsafe { readback.map(frame.texture().as_raw(), upload) }
            .map_err(|error| EncodeError::new(context, readback_failure(error)));

        self.elapsed += started.elapsed();
        self.frames += 1;
        uploaded?;
        Ok(self.destination.as_raw())
    }
}

/// Creates a Direct3D 11 device on one adapter.
fn create_device(adapter: AdapterId) -> Result<ID3D11Device, windows::core::Error> {
    let adapter = super::device::find_adapter(adapter)?;

    let mut device: Option<ID3D11Device> = None;
    // SAFETY: `adapter` is a live DXGI adapter, the driver type must be
    // `UNKNOWN` when an adapter is named and the software rasteriser handle is
    // null to match, the feature level list and `device` are live locals, and
    // `D3D11_SDK_VERSION` is the constant the header requires. On success
    // `device` holds one reference, released on drop.
    unsafe {
        D3D11CreateDevice(
            &adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            // The same flag capture creates its device with: the frames are
            // `B8G8R8A8`, and this device has to hold one.
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;
    }

    device.ok_or_else(|| {
        windows::core::Error::new(
            windows::Win32::Foundation::E_FAIL,
            "D3D11CreateDevice reported success without returning a device",
        )
    })
}

/// The texture on the encoder's adapter that each frame is uploaded into.
fn create_destination(
    device: &ID3D11Device,
    resolution: Resolution,
) -> Result<ID3D11Texture2D, windows::core::Error> {
    #[allow(clippy::cast_sign_loss)]
    let description = D3D11_TEXTURE2D_DESC {
        Width: resolution.width,
        Height: resolution.height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        // What a capture backend's own textures are bound as, so that a vendor
        // runtime which inspects the description sees what it would have seen
        // had capture been on this adapter.
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };

    let mut texture: Option<ID3D11Texture2D> = None;
    // SAFETY: the description is a live local, no initial data is supplied
    // because every frame overwrites the whole texture, and `texture` is a live
    // out-parameter receiving one reference.
    unsafe { device.CreateTexture2D(&description, None, Some(&mut texture)) }?;

    texture.ok_or_else(|| {
        windows::core::Error::new(
            windows::Win32::Foundation::E_FAIL,
            "CreateTexture2D reported success without returning a texture",
        )
    })
}

/// An encoder on one adapter, fed frames captured on another.
///
/// # Ownership
///
/// The fields are in the order they have to be released in: the encoder first,
/// because its session was opened against the bridge's device, and the bridge
/// second. Rust drops fields in declaration order, and that is the whole of the
/// guarantee.
struct Bridged {
    inner: Box<dyn VideoEncoder>,
    bridge: AdapterBridge,
}

impl core::fmt::Debug for Bridged {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Bridged")
            .field("encoder", &self.inner.encoder())
            .field("frames", &self.bridge.frames)
            .finish()
    }
}

impl VideoEncoder for Bridged {
    fn encoder(&self) -> EncoderKind {
        self.inner.encoder()
    }

    fn configuration(&self) -> &EncoderConfig {
        self.inner.configuration()
    }

    fn parameter_sets(&self) -> &[u8] {
        self.inner.parameter_sets()
    }

    fn submit(&mut self, frame: &SourceFrame<'_>) -> Result<(), EncodeError> {
        let context = EncodeContext::new(
            self.inner.encoder(),
            self.inner.configuration().codec(),
            self.inner.configuration().resolution(),
        );

        // SAFETY: `frame` borrows the capture backend's texture for the whole
        // of this call, which is what `carry` requires of it.
        let carried = unsafe { self.bridge.carry(frame, context) }?;

        // SAFETY: `carried` is the bridge's own texture, live for as long as
        // the bridge is and not overwritten until the next `carry` — which
        // cannot happen inside this call. The borrow ends with `local`, before
        // this function returns.
        let texture = unsafe { SourceTexture::new(SurfaceKind::D3d11Texture2D, carried) };
        let local = SourceFrame::new(
            texture,
            frame.format(),
            frame.resolution(),
            frame.presentation_time(),
        );
        let local = if frame.forces_keyframe() {
            local.forcing_keyframe()
        } else {
            local
        };

        self.inner.submit(&local)
    }

    fn next_packet(&mut self) -> Result<Option<EncodedPacket<'_>>, EncodeError> {
        self.inner.next_packet()
    }

    fn finish(&mut self) -> Result<(), EncodeError> {
        self.inner.finish()
    }

    fn shut_down(&mut self) {
        self.inner.shut_down();

        let (elapsed, frames) = (self.bridge.elapsed, self.bridge.frames);
        if frames == 0 {
            return;
        }
        #[allow(clippy::cast_precision_loss)]
        let per_frame = elapsed.as_secs_f64() * 1000.0 / frames as f64;
        // Said in the log rather than left to a benchmark, because this is a
        // cost a user is paying for a configuration they chose, and this is the
        // only place they can see it (AGENTS.md sections 19 and 35).
        tracing::info!(
            encoder = %self.inner.encoder().log_encoder_family(),
            frames,
            carried_ms_per_frame = per_frame,
            carried_total_ms = elapsed.as_secs_f64() * 1000.0,
            "closed an encoder that was fed frames from another adapter"
        );
    }
}

#[cfg(test)]
mod tests {
    //! What runs where.
    //!
    //! Two of these need **two graphics cards from two vendors**, which no
    //! hosted runner has and most developer machines do not either, so they are
    //! `#[ignore]`d rather than skipped: a test that decides for itself that it
    //! could not run reads as a pass (`docs/testing.md`). On a machine with
    //! both:
    //!
    //! ```text
    //! cargo test -p clipped-encoder --lib -- --ignored --nocapture --test-threads=1 windows::bridge
    //! ```
    //!
    //! The rest need no graphics device at all and run everywhere.

    use core::time::Duration;

    use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;

    use super::*;
    use crate::codec::{Codec, Vendor};
    use crate::config::{BitRate, ColourSpace, FrameRate, KeyframeInterval, RateControl};
    use crate::windows::amf::tests::SESSIONS as AMF_SESSIONS;
    use crate::windows::hardware_test::{
        assert_colour_close, decode_middle_pixels, skipped, tool, TestGpu,
    };
    use crate::windows::nvenc::tests::SESSIONS as NVENC_SESSIONS;
    use crate::windows::{AmfEncoder, NvencEncoder};

    /// Small enough that a two-GPU run finishes quickly, large enough that the
    /// copy is a real one.
    const TEST_SIZE: Resolution = Resolution::new(1280, 720);

    /// One solid colour per frame, all different, so that a bridge which
    /// repeated a frame, dropped one, or carried an uninitialised texture shows
    /// the wrong colour somewhere.
    const COLOURS: [[u8; 3]; 6] = [
        [220, 30, 30],
        [30, 200, 30],
        [40, 60, 230],
        [230, 220, 40],
        [20, 20, 20],
        [200, 200, 200],
    ];

    fn config(codec: Codec) -> EncoderConfig {
        EncoderConfig::new(
            codec,
            TEST_SIZE,
            FrameRate::FPS_60,
            RateControl::constant(BitRate::megabits_per_second(20)),
        )
        .with_source_format(SurfaceFormat::Bgra8Unorm)
        .with_colour_space(ColourSpace::BT709_LIMITED)
        // Every frame coded from scratch, so that a wrong colour cannot be
        // blamed on prediction from its neighbour.
        .with_keyframe_interval(KeyframeInterval::every(
            Duration::from_millis(1),
            FrameRate::FPS_60,
        ))
    }

    fn frame<'texture>(texture: &'texture ID3D11Texture2D, index: usize) -> SourceFrame<'texture> {
        // SAFETY: the texture is a live `ID3D11Texture2D` on the capture device,
        // and the borrow ties the frame to it, so it cannot outlive it.
        let surface = unsafe {
            SourceTexture::new(
                SurfaceKind::D3d11Texture2D,
                texture.as_raw().cast::<c_void>(),
            )
        };
        SourceFrame::new(
            surface,
            SurfaceFormat::Bgra8Unorm,
            TEST_SIZE,
            Duration::from_micros(16_667 * index as u64),
        )
    }

    /// A handle no adapter answers for, which is what the two tests below need
    /// to reach the decision without a graphics card.
    fn no_device() -> GraphicsDevice {
        // SAFETY: null is one of the two values `GraphicsDevice` permits, and
        // nothing dereferences it: the decision is made before any Direct3D
        // call.
        unsafe { GraphicsDevice::new(DeviceKind::D3d11, core::ptr::null_mut()) }
    }

    /// Which device `open_across_adapters` handed to the backend.
    ///
    /// The opener refuses, so nothing is left open and the answer is about the
    /// decision rather than about the hardware.
    fn opened_against(capture: &GraphicsDevice, encoder: EncoderKind) -> *mut c_void {
        let mut given = core::ptr::null_mut();
        let result = open_across_adapters(capture, encoder, config(Codec::H264), |device| {
            given = device.as_raw();
            Err(EncodeError::new(
                EncodeContext::new(encoder, Codec::H264, TEST_SIZE),
                EncodeErrorKind::NotRunning,
            ))
        });

        assert!(result.is_err(), "the test opener never returns an encoder");
        given
    }

    #[test]
    fn the_software_encoder_is_never_carried_across_adapters() {
        // It has no adapter to be on the wrong side of, and asking DXGI which
        // one it lives on would be a question with no answer. The caller's own
        // device goes straight through.
        let capture = no_device();

        assert_eq!(
            opened_against(&capture, EncoderKind::Software),
            capture.as_raw(),
            "a software encoder must be handed the caller's own device"
        );
    }

    #[test]
    fn a_device_that_will_not_name_its_adapter_is_left_to_the_backend() {
        // "Which adapter is this on?" unanswered is not evidence of a mismatch,
        // and building a bridge on a guess would copy every frame of a recording
        // that needed no copy. The backend gets the device it was given, and
        // refuses it in its own words if it must
        // (`crate::windows::dxgi::require_adapter_vendor`).
        let capture = no_device();

        assert_eq!(
            opened_against(&capture, EncoderKind::Amf),
            capture.as_raw(),
            "an unidentifiable device must not be bridged on a guess"
        );
    }

    #[test]
    #[ignore = "needs an NVIDIA and an AMD adapter in the same machine"]
    fn amf_encodes_frames_captured_on_an_nvidia_device() {
        // Issue #443 itself. Capture creates its device on the default adapter,
        // which on the machine this was written on is the RTX 4090, and
        // `--encoder amf` could not open a session at all: `AMFContext::InitDX11`
        // answered `AMF_INVALID_ARG (4)`. Making `open_across_adapters` call
        // `open(capture)` unconditionally fails this test with exactly that.
        carries_frames_to(Vendor::Nvidia, EncoderKind::Amf, |device, config| {
            AmfEncoder::open(device, config)
                .map(|encoder| Box::new(encoder) as Box<dyn VideoEncoder>)
        });
    }

    #[test]
    #[ignore = "needs an NVIDIA and an AMD adapter in the same machine"]
    fn nvenc_encodes_frames_captured_on_an_amd_device() {
        // The same machine the other way round, which is the laptop whose
        // integrated part is the default adapter. Measured before this was
        // written: NVENC on an AMD device answers `nvEncOpenEncodeSessionEx
        // failed with NV_ENC_ERR_NO_ENCODE_DEVICE (1)`. The defect is not AMF's,
        // so the way out is not AMF's either.
        carries_frames_to(Vendor::Amd, EncoderKind::Nvenc, |device, config| {
            NvencEncoder::open(device, config)
                .map(|encoder| Box::new(encoder) as Box<dyn VideoEncoder>)
        });
    }

    #[test]
    #[ignore = "needs an NVIDIA and an AMD adapter in the same machine"]
    fn an_encoder_on_the_capture_adapter_keeps_the_callers_own_device() {
        // The case that has to stay free. A bridge built whenever one was
        // possible would cost every ordinary recording a copy per frame, so this
        // asserts the identity of the handle rather than that an encoder opened:
        // opening proves nothing about which device it opened against.
        //
        // Two adapters to be a real answer. On a machine with only the NVIDIA
        // one there is nowhere else the device could have come from, so the
        // assertion would hold however the decision was made.
        if !two_vendors_present() {
            return;
        }
        let Some(gpu) = TestGpu::open(Vendor::Nvidia, TEST_SIZE, &NVENC_SESSIONS) else {
            return;
        };
        let capture = gpu.graphics_device();

        assert_eq!(
            opened_against(&capture, EncoderKind::Nvenc),
            capture.as_raw(),
            "an encoder already on the capture adapter must not be bridged"
        );
    }

    /// Whether this machine has both an NVIDIA and an AMD adapter that could
    /// hold an encoder.
    fn two_vendors_present() -> bool {
        let adapters = super::super::dxgi::adapters().expect("DXGI can be asked on Windows");
        let has = |vendor| {
            adapters
                .iter()
                .any(|adapter| adapter.vendor() == vendor && adapter.can_host_hardware_encoder())
        };
        if has(Vendor::Nvidia) && has(Vendor::Amd) {
            return true;
        }
        skipped("this machine does not have both an NVIDIA and an AMD adapter");
        false
    }

    /// Encodes one solid colour per frame from a device on `capture_vendor`'s
    /// adapter through an encoder that lives on another, and decodes the result.
    ///
    /// The colours are what makes this more than "it opened": a bridge that
    /// carried the wrong texture, carried it a frame late, or carried nothing at
    /// all would still produce a bitstream, and would produce the wrong colours.
    fn carries_frames_to(
        capture_vendor: Vendor,
        encoder: EncoderKind,
        open: impl Fn(&GraphicsDevice, EncoderConfig) -> Result<Box<dyn VideoEncoder>, EncodeError>,
    ) {
        if !two_vendors_present() {
            return;
        }
        let Some(ffmpeg) = tool("ffmpeg") else {
            skipped("no ffmpeg to decode what was encoded with");
            return;
        };
        let sessions = if encoder == EncoderKind::Amf {
            &AMF_SESSIONS
        } else {
            &NVENC_SESSIONS
        };
        let Some(gpu) = TestGpu::open(capture_vendor, TEST_SIZE, sessions) else {
            return;
        };
        let capture = gpu.graphics_device();

        let mut session = open_across_adapters(&capture, encoder, config(Codec::H264), |device| {
            assert_ne!(
                device.as_raw(),
                capture.as_raw(),
                "the capture device is on the {capture_vendor} adapter, so it cannot be what \
                 {encoder} is opened against"
            );
            open(device, config(Codec::H264))
        })
        .unwrap_or_else(|error| {
            panic!(
                "{encoder} could not be opened for frames captured on the {capture_vendor} \
                 adapter: {error}"
            )
        });

        let mut stream = Vec::new();
        let mut packets = 0usize;
        for (index, colour) in COLOURS.iter().enumerate() {
            let texture = gpu.solid_texture(*colour);
            session
                .submit(&frame(&texture, index))
                .expect("the frame is submitted");
            while let Some(packet) = session.next_packet().expect("a packet is produced") {
                assert!(
                    !packet.data().is_empty(),
                    "an empty packet is not a coded picture"
                );
                packets += 1;
                stream.extend_from_slice(packet.data());
            }
        }
        session.finish().expect("the stream can be finished");
        while let Some(packet) = session.next_packet().expect("a packet is produced") {
            packets += 1;
            stream.extend_from_slice(packet.data());
        }

        assert_eq!(
            packets,
            COLOURS.len(),
            "every frame carried across has to come back as a coded picture"
        );

        let decoded =
            decode_middle_pixels(&ffmpeg, &stream, COLOURS.len(), TEST_SIZE, "clipped-bridge");
        for (index, (got, expected)) in decoded.iter().zip(COLOURS.iter()).enumerate() {
            assert_colour_close(
                *got,
                *expected,
                &format!(
                    "frame {index} was captured on the {capture_vendor} adapter and encoded on \
                     another; a bridge carrying the wrong picture would show here"
                ),
            );
        }

        session.shut_down();
    }
}
