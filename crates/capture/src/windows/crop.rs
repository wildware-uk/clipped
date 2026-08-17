//! The texture an odd-sized source is cropped into, and the texture creation
//! both backends share.
//!
//! 4:2:0 chroma has no representation for an odd width or height, so every
//! encoder in `clipped-encoder` refuses one, and an ordinary bordered window
//! sized to 1000x600 has a client area of 986x593
//! ([issue #561](https://github.com/wildware-uk/clipped/issues/561)). Every
//! backend here therefore reports
//! [`FrameSize::rounded_down_to_even`](crate::FrameSize::rounded_down_to_even)
//! of what the target measures.
//!
//! Reporting it is the easy half. The hard half is that the *texture* has to be
//! that size too: a frame that declares 986x592 while its texture holds 986x593
//! is a lie the pipeline cannot detect and the file cannot admit — the software
//! encoder refuses it outright, and what the three hardware encoders would do
//! with a surface a row taller than the session they were opened for is a
//! question about three vendors' drivers rather than about this code. So the row
//! is genuinely cropped away, and this is what does it where the source cannot
//! be asked for the smaller picture directly.
//!
//! # Who needs it, and who does not
//!
//! - **Desktop Duplication capturing a window** does not. It already copies a
//!   crop of the desktop image into a destination texture of the size it was
//!   given (`Destination`, in `desktop_duplication.rs`), so an even destination
//!   costs nothing: the copy that was already happening copies one row less.
//! - **Desktop Duplication capturing a display** needs it only when the display
//!   mode itself has an odd dimension — which a physical monitor never has, and
//!   a remote-desktop session sized to a window frequently does.
//! - **Windows Graphics Capture** needs it whenever the item's content has an
//!   odd dimension, for either kind of target. It hands out the compositor's own
//!   frame-pool texture, and the pool's size is what the compositor composes
//!   into: a pool deliberately one row shorter than the content would make every
//!   frame's `ContentSize` disagree with it, which is precisely how that backend
//!   recognises a resize, and what the compositor does with the row it cannot
//!   fit is undocumented — a crop and a rescale look identical in the API and
//!   only one of them is honest. So the pool keeps the content's own size and
//!   the crop happens here, where it is one `CopySubresourceRegion` with a known
//!   answer.
//!
//! The cost is therefore one GPU-to-GPU copy per frame, paid only by a capture
//! whose source has an odd dimension, and only where the alternative was not
//! recording at all. It is a copy on the capture thread, not a readback: no
//! `Map`, no CPU wait, nothing that leaves video memory (AGENTS.md section 18).

use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET,
    D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

use crate::{CaptureError, CaptureMethod, FrameSize, TargetKind};

/// The size `measured` will be recorded at, or a refusal naming why it cannot be
/// recorded at all.
///
/// One place rather than one per backend, because the refusal is the same fact
/// in both: a target a single pixel wide or a single pixel high has no even size
/// inside it, and there is nothing to record. It is stated here instead of being
/// discovered at the encoder, which would report it as "no encoder could be
/// opened" and name neither the target nor the reason.
pub(super) fn recordable_size(
    measured: FrameSize,
    method: CaptureMethod,
    target: TargetKind,
) -> Result<FrameSize, CaptureError> {
    measured
        .rounded_down_to_even()
        .ok_or(CaptureError::UnsupportedTarget {
            method,
            target,
            reason: "it is one pixel wide or one pixel high, and 4:2:0 chroma has no \
                     representation for an odd dimension, so there is no even picture \
                     inside it to record",
        })
}

/// Creates the kind of texture a backend hands out as a frame: `size`, BGRA,
/// one mip level, one slice, no multisampling.
///
/// Shared by everything here that owns a frame texture so that the description
/// is written once. The bind flags are what the two consumers need between them
/// — a render target so the part of a frame no window covers can be cleared, a
/// shader resource because that is what an encoder binds — and both are cheap
/// enough that splitting them per caller would buy nothing but a second
/// description to keep in step.
pub(super) fn create_frame_texture(
    device: &ID3D11Device,
    size: FrameSize,
) -> Result<ID3D11Texture2D, windows::core::Error> {
    let description = D3D11_TEXTURE2D_DESC {
        Width: size.width(),
        Height: size.height(),
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };

    let mut texture: Option<ID3D11Texture2D> = None;
    // SAFETY: `description` is a live local describing a texture with no initial
    // data, and the out parameter is the representation windows-rs uses for one
    // of that type. On success it holds one reference, released when the
    // returned texture drops.
    unsafe { device.CreateTexture2D(&raw const description, None, Some(&raw mut texture)) }?;

    texture.ok_or_else(|| {
        windows::core::Error::new(
            windows::Win32::Foundation::E_FAIL,
            "CreateTexture2D reported success without returning a texture",
        )
    })
}

/// A texture holding the even-sized top-left crop of an odd-sized source.
///
/// # Which row and column go
///
/// The bottom one and the right-hand one. The crop is anchored at the top-left
/// corner because that is where both capture APIs anchor a picture, so the crop
/// is the same picture with its last row missing rather than the same picture
/// shifted half a pixel.
///
/// # Ownership
///
/// It owns the destination texture and its own reference to the device context,
/// both released when it drops. It owns no source: the texture handed to
/// [`fill_from`](Self::fill_from) belongs to the compositor or to DXGI, is read
/// inside that call, and is not retained.
pub(super) struct EvenCrop {
    /// The immediate context the copy is issued on, used only from the capture
    /// thread that owns the backend.
    context: ID3D11DeviceContext,
    /// The texture handed to the caller as the frame.
    texture: ID3D11Texture2D,
    /// The size it was created at, which is the extent of every copy into it.
    size: FrameSize,
}

impl EvenCrop {
    /// Creates a `size` destination on `device`.
    ///
    /// `size` must be the source's size rounded down to even, and the caller
    /// must rebuild this whenever the source's size changes: the copy below
    /// reads a `size` region out of whatever it is given, and Direct3D documents
    /// a read from outside the source as undefined.
    pub(super) fn create(
        device: &ID3D11Device,
        size: FrameSize,
    ) -> Result<Self, windows::core::Error> {
        let texture = create_frame_texture(device, size)?;

        // SAFETY: `device` is live, and the immediate context comes back with a
        // reference this value then owns.
        let context = unsafe { device.GetImmediateContext() }?;

        Ok(Self {
            context,
            texture,
            size,
        })
    }

    /// The texture the caller will be handed as this frame.
    pub(super) const fn texture(&self) -> &ID3D11Texture2D {
        &self.texture
    }

    /// Copies the top-left [`size`](Self::size) of `source` into this texture.
    ///
    /// One GPU command. It does not wait for the copy to retire, and it does not
    /// need to: the encoder reads the destination through the same immediate
    /// context, and Direct3D orders work on one context.
    pub(super) fn fill_from(&self, source: &ID3D11Texture2D) {
        let region = D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: self.size.width(),
            bottom: self.size.height(),
            back: 1,
        };

        // SAFETY: both textures belong to this device — the destination was
        // created on it above, and the source is a frame from a capture running
        // on it — and both are BGRA with one mip level, one slice and no
        // multisampling. The region is inside the source, because this value is
        // created from the source's own size rounded down and is rebuilt
        // whenever that size changes, and it is exactly the destination, which
        // was created at that size. The immediate context is used only from the
        // capture thread that owns the backend.
        unsafe {
            self.context.CopySubresourceRegion(
                &self.texture,
                0,
                0,
                0,
                0,
                source,
                0,
                Some(&raw const region),
            );
        }
    }
}

impl core::fmt::Debug for EvenCrop {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("EvenCrop")
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use windows::core::Interface as _;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_WARP;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDevice, ID3D11Resource, D3D11_CPU_ACCESS_READ, D3D11_MAPPED_SUBRESOURCE,
        D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_SUBRESOURCE_DATA, D3D11_USAGE_STAGING,
    };

    use super::*;

    /// The client area of an ordinary bordered window sized to 1000x600 on
    /// Windows 11 at 96 DPI, which is the measurement on issue #561.
    const ODD: (u32, u32) = (986, 593);

    /// What that window is recorded at.
    const EVEN: (u32, u32) = (986, 592);

    #[test]
    fn a_target_measuring_one_pixel_is_refused_by_name_rather_than_by_the_encoder() {
        let error = recordable_size(
            FrameSize::new(1920, 1).expect("1920x1 is a valid size"),
            CaptureMethod::WindowsGraphicsCapture,
            TargetKind::Window,
        )
        .expect_err("a one-pixel-high target has no even picture inside it");

        let message = error.to_string();
        assert!(
            message.contains("4:2:0"),
            "the refusal has to say why, or this is the encoder's opaque \
             'no encoder could be opened' again: {message}"
        );

        // And the ordinary case is not refused, without which the assertion
        // above would pass just as well against a function that refused
        // everything.
        assert_eq!(
            recordable_size(
                size(ODD),
                CaptureMethod::WindowsGraphicsCapture,
                TargetKind::Window,
            )
            .expect("an odd size is recordable one row short"),
            size(EVEN)
        );
    }

    #[test]
    fn an_odd_source_is_cropped_to_the_even_picture_inside_it_and_not_resampled() {
        // The whole of issue #561's honesty requirement, in one place and
        // without a window: a frame that declares 986x592 has to *be* 986x592,
        // and it has to be the same picture with its last row and column
        // missing rather than the same picture squeezed into a smaller texture.
        // A copy with the wrong box — or a later change to a scaling blit, which
        // is what the compositor might already be doing if the frame pool were
        // asked to do this instead — moves the pixels this reads.
        let Some(device) = warp_device() else {
            return;
        };

        let source = source_texture(&device, ODD);
        let crop =
            EvenCrop::create(&device, size(EVEN)).expect("a device can create a 986x592 texture");
        crop.fill_from(&source);

        let mut description = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: the texture is live and `description` is a live local the call
        // writes into.
        unsafe { crop.texture().GetDesc(&raw mut description) };
        assert_eq!(
            (description.Width, description.Height),
            EVEN,
            "the texture handed to the encoder has to be the size the frame declares"
        );

        let pixels = read_back(&device, crop.texture(), EVEN);
        for (x, y) in [(0, 0), (500, 300), (EVEN.0 - 1, EVEN.1 - 1)] {
            assert_eq!(
                pixels[(y * EVEN.0 + x) as usize],
                colour(x, y),
                "the pixel at ({x}, {y}) of the crop is not the pixel at ({x}, {y}) of the \
                 source, so this is not the top-left crop of it"
            );
        }
    }

    /// A pair as a [`FrameSize`].
    fn size((width, height): (u32, u32)) -> FrameSize {
        FrameSize::new(width, height).expect("a test size is not zero")
    }

    /// The colour of the source pixel at `(x, y)`, as BGRA bytes.
    ///
    /// Every pixel of the source is distinct, so a crop that took the wrong
    /// region — or resampled — reads a colour that names where it actually came
    /// from.
    fn colour(x: u32, y: u32) -> [u8; 4] {
        [
            (x % 251) as u8,
            (y % 241) as u8,
            ((x / 251) * 8 + (y / 241)) as u8,
            255,
        ]
    }

    /// A BGRA texture of `size` whose every pixel is [`colour`].
    fn source_texture(device: &ID3D11Device, (width, height): (u32, u32)) -> ID3D11Texture2D {
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&colour(x, y));
            }
        }

        let description = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let data = D3D11_SUBRESOURCE_DATA {
            pSysMem: pixels.as_ptr().cast(),
            SysMemPitch: width * 4,
            SysMemSlicePitch: 0,
        };

        let mut texture: Option<ID3D11Texture2D> = None;
        // SAFETY: the description and the initial data are live locals, and the
        // buffer they point at outlives the call; the pitch is the row length it
        // was filled with. The out parameter receives one reference.
        unsafe {
            device.CreateTexture2D(
                &raw const description,
                Some(&raw const data),
                Some(&raw mut texture),
            )
        }
        .expect("a device can create a filled BGRA texture");
        texture.expect("CreateTexture2D reported success")
    }

    /// Reads `texture` back into system memory, one entry per pixel.
    fn read_back(
        device: &ID3D11Device,
        texture: &ID3D11Texture2D,
        (width, height): (u32, u32),
    ) -> Vec<[u8; 4]> {
        let description = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };

        let mut staging: Option<ID3D11Texture2D> = None;
        // SAFETY: `description` is a live local for a texture with no initial
        // data, and the out parameter receives one reference.
        unsafe { device.CreateTexture2D(&raw const description, None, Some(&raw mut staging)) }
            .expect("a device can create a staging texture");
        let staging = staging.expect("CreateTexture2D reported success");

        // SAFETY: the device is live and the context comes back owned.
        let context = unsafe { device.GetImmediateContext() }.expect("an immediate context");
        // SAFETY: both textures have the same size, format and shape, which is
        // what `CopyResource` requires, and both are live.
        unsafe {
            context.CopyResource(
                &staging.cast::<ID3D11Resource>().expect("a resource"),
                &texture.cast::<ID3D11Resource>().expect("a resource"),
            );
        }

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: the staging texture was created readable, subresource 0 is the
        // whole of it, and `mapped` is a live out parameter. The unmap below is
        // paired with this map.
        unsafe { context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&raw mut mapped)) }
            .expect("a staging texture can be mapped");

        let stride = mapped.RowPitch as usize;
        // SAFETY: `Map` succeeded, so `pData` points at `RowPitch` bytes for each
        // of the texture's rows, readable until the `Unmap` below.
        let bytes = unsafe {
            core::slice::from_raw_parts(mapped.pData.cast::<u8>(), stride * height as usize)
        };

        let mut pixels = Vec::with_capacity((width * height) as usize);
        for y in 0..height as usize {
            for x in 0..width as usize {
                let at = y * stride + x * 4;
                pixels.push([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
            }
        }

        // SAFETY: paired with the map above, on the same subresource of the same
        // texture.
        unsafe { context.Unmap(&staging, 0) };
        pixels
    }

    /// A WARP device, so that this runs on a machine with no GPU and inside a
    /// CI container: what is being tested is a copy, and WARP performs the same
    /// one.
    fn warp_device() -> Option<ID3D11Device> {
        let mut device: Option<ID3D11Device> = None;
        // SAFETY: no adapter is named, which is what a driver type other than
        // `UNKNOWN` requires; the module handle is null because the driver type
        // is not the software rasteriser; no feature levels are requested or
        // returned; the out parameter is a live local receiving one reference.
        let created = unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_WARP,
                HMODULE::default(),
                Default::default(),
                None,
                D3D11_SDK_VERSION,
                Some(&raw mut device),
                None,
                None,
            )
        };

        match created {
            Ok(()) => device,
            Err(error) => {
                // A machine with no Direct3D at all is a legitimate place to run
                // the rest of the suite; say so loudly enough to notice if it
                // becomes the normal outcome.
                let _ = writeln!(
                    std::io::stderr(),
                    "SKIPPED (capture): no WARP Direct3D 11 device on this machine: {error}"
                );
                None
            }
        }
    }
}
