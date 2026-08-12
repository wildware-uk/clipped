//! Reading a handful of pixels back off the GPU, to see whether a capture has
//! gone black.
//!
//! This is the one place in the capture pipeline that moves pixels from the GPU
//! to system memory, and it exists because the failure it detects cannot be seen
//! any other way: a capture that has silently stopped working keeps returning
//! frames, and every pixel in them is zero (`docs/capture-pipeline.md`, and
//! [issue #97](https://github.com/wildware-uk/clipped/issues/97)).
//!
//! # What it costs, and why that is acceptable
//!
//! Sixteen pixels, not a frame. Each is copied on the GPU into a 16x1 staging
//! texture with `CopySubresourceRegion`, and the strip is mapped once — so the
//! transfer is 64 bytes, and the expense is not the bytes but the `Map`, which
//! waits for those copies to finish and therefore stalls the pipeline briefly.
//! That is why [`BlackFrameWatch`](crate::BlackFrameWatch) rations sampling to
//! twice a second rather than doing it per frame: at 60 fps, 58 frames in 60
//! are never touched.
//!
//! The staging texture is created per sample rather than kept. Two
//! `CreateTexture2D` calls a second for a 16-pixel texture is not a cost worth
//! measuring, and a cached one would have to be invalidated when the format
//! changes, when the target is resized, and — the case that would actually be
//! got wrong — when a fallback puts a *different backend's device* behind the
//! frames.
//!
//! # Threading
//!
//! Sampling uses the frame's own device and its immediate context, which is the
//! same context the backend that produced the frame is using. Both happen on the
//! capture thread, one after the other, which is what makes that safe: a
//! `ID3D11DeviceContext` may not be used from two threads at once, and here it
//! is only ever used from one.

use core::ffi::c_void;

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Texture2D, D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

use crate::{CapturedFrame, FrameSample, FrameSampler, PixelFormat, TextureKind};

/// How many pixels one sample reads.
///
/// Sixteen, as a 4x4 grid over the frame. Enough that a frame with anything at
/// all drawn in it — a heads-up display in one corner, a status bar along the
/// bottom — is very unlikely to be reported as black, and few enough that the
/// readback stays a rounding error. A grid rather than random points because a
/// grid covers the frame evenly, and because a sample somebody can predict is a
/// sample somebody can reproduce.
const GRID: u32 = 4;

/// The pixels one sample reads.
const SAMPLES: u32 = GRID * GRID;

/// Reads pixels out of Direct3D 11 frames.
///
/// The [`FrameSampler`] a recording on Windows uses. It holds nothing: every
/// resource it needs comes from the frame it is given, and is released before
/// the call returns.
#[derive(Debug, Default, Clone, Copy)]
pub struct D3d11FrameSampler;

impl D3d11FrameSampler {
    /// A sampler. It has no state to configure.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl FrameSampler for D3d11FrameSampler {
    fn sample(&mut self, frame: &CapturedFrame<'_>) -> Option<FrameSample> {
        // Only the format both Windows capture backends produce today. HDR
        // frames are 10 or 16 bits per channel and would need their own
        // arithmetic; returning `None` says "no evidence" rather than guessing,
        // which is the answer that cannot end a recording wrongly
        // ([issue #99](https://github.com/wildware-uk/clipped/issues/99)).
        if frame.format().pixel_format() != PixelFormat::Bgra8Unorm
            || frame.texture().kind() != TextureKind::D3d11Texture2D
        {
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

        // SAFETY: `texture` is live for the length of this call; both calls
        // return owned references windows-rs releases on drop.
        let device = unsafe { texture.GetDevice() }.ok()?;
        // SAFETY: as above. The context is the frame's own device's immediate
        // context, used only on this thread — see the module documentation.
        let context = unsafe { device.GetImmediateContext() }.ok()?;

        let mut description = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: `description` is a live local of the type the signature names.
        unsafe { texture.GetDesc(&raw mut description) };
        if description.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            return None;
        }

        // The frame's declared size, never more than the texture actually holds.
        // A capture API is entitled to hand over a texture larger than the
        // content in it, and sampling the unused margin would be sampling
        // whatever was left there — which is usually black, and would be a false
        // accusation.
        let width = description.Width.min(frame.format().size().width());
        let height = description.Height.min(frame.format().size().height());
        if width == 0 || height == 0 {
            return None;
        }

        let staging = create_strip(&device, description.Format)?;

        for point in 0..SAMPLES {
            let (x, y) = grid_point(point, width, height);
            let region = D3D11_BOX {
                left: x,
                top: y,
                front: 0,
                right: x + 1,
                bottom: y + 1,
                back: 1,
            };
            // SAFETY: both textures belong to `device`; the region is one pixel
            // inside the source, clamped to its description above; and the
            // destination pixel is inside the 16x1 strip because `point` is
            // less than `SAMPLES`.
            unsafe {
                context.CopySubresourceRegion(
                    &staging,
                    0,
                    point,
                    0,
                    0,
                    texture,
                    0,
                    Some(&raw const region),
                );
            }
        }

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: `staging` is a live staging texture created for reading, and
        // `mapped` is a live local. The map is matched by the unmap below, and
        // `pData` is read only while the mapping is held, only within the
        // `SAMPLES` four-byte pixels the strip is known to contain.
        let sample = unsafe {
            context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&raw mut mapped))
                .ok()?;
            let mut sample = FrameSample::empty();
            for point in 0..SAMPLES {
                let pixel = mapped
                    .pData
                    .cast::<u32>()
                    .add(point as usize)
                    .read_unaligned();
                let [blue, green, red, _alpha] = pixel.to_le_bytes();
                sample = sample.with_pixel(red, green, blue);
            }
            context.Unmap(&staging, 0);
            sample
        };

        Some(sample)
    }
}

/// Where the `point`th sample falls in a frame of `width` by `height`.
///
/// The grid is inset by half a cell, so no sample lands on the very edge of the
/// frame: a one-pixel border is exactly the sort of thing that is black in a
/// perfectly healthy capture.
const fn grid_point(point: u32, width: u32, height: u32) -> (u32, u32) {
    let column = point % GRID;
    let row = point / GRID;
    let x = (width * (2 * column + 1)) / (2 * GRID);
    let y = (height * (2 * row + 1)) / (2 * GRID);
    // `width` and `height` are non-zero, and the fraction is strictly below one,
    // so both stay inside the frame; the `min` is belt and braces against a
    // future grid size that does not divide evenly.
    (min(x, width - 1), min(y, height - 1))
}

const fn min(left: u32, right: u32) -> u32 {
    if left < right {
        left
    } else {
        right
    }
}

/// Creates the 16x1 staging texture one sample is read through.
fn create_strip(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT,
) -> Option<ID3D11Texture2D> {
    let description = D3D11_TEXTURE2D_DESC {
        Width: SAMPLES,
        Height: 1,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
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
    // SAFETY: the description is a live local describing a 16-pixel staging
    // texture with no initial data; the out parameter is the shape windows-rs
    // uses for one of that type. On success it holds one reference, released
    // when the returned texture is dropped.
    unsafe {
        device
            .CreateTexture2D(&raw const description, None, Some(&raw mut staging))
            .ok()?;
    }
    staging
}

#[cfg(test)]
mod tests {
    //! Sampling, against a real Direct3D texture whose colour this test put
    //! there.
    //!
    //! This is the half of black-frame detection that cannot be unit tested with
    //! a fake: the policy in `crate::blackness` decides what a sample *means*,
    //! and this decides what the sample *is*. It needs a graphics device but no
    //! window, no capture and no desktop — the source is a texture cleared to a
    //! known colour, which is the "known-black source" issue #97 asks for and
    //! the very dark scene it must not accuse, side by side.

    use super::*;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_WARP;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDevice, ID3D11Device, D3D11_BIND_SHADER_RESOURCE,
        D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11_SUBRESOURCE_DATA,
        D3D11_USAGE_DEFAULT,
    };

    use crate::{CaptureTimestamp, FrameFormat, FrameSize, FrameTexture, SourceClock};

    const WIDTH: u32 = 640;
    const HEIGHT: u32 = 360;

    /// An opaque pixel of the given colour, as a BGRA8 texture stores one.
    const fn bgra(red: u8, green: u8, blue: u8) -> u32 {
        u32::from_le_bytes([blue, green, red, 0xFF])
    }

    /// A device to make textures on, or [`None`] on a machine with no Direct3D
    /// at all.
    ///
    /// WARP rather than hardware, for the reason `device.rs`'s test gives: this
    /// has to run in CI, and the thing under test — a staging copy and a map —
    /// behaves the same on either.
    fn device() -> Option<ID3D11Device> {
        let mut device: Option<ID3D11Device> = None;
        // SAFETY: every pointer argument is either absent or the address of a
        // live local `Option<ID3D11Device>`, which is the representation
        // windows-rs uses for an out parameter of that type.
        let created = unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_WARP,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
        };
        if let Err(error) = created {
            eprintln!("skipped: no WARP Direct3D 11 device on this machine: {error}");
            return None;
        }
        device
    }

    /// A texture painted by `pixel`, in the format both capture backends
    /// produce.
    ///
    /// The pixels are supplied as the texture's initial data rather than
    /// rendered into it, so what is under the sampler is exactly the bytes this
    /// test wrote — no clear colour conversion, no rounding, and nothing to
    /// flush.
    fn texture_of(device: &ID3D11Device, pixel: impl Fn(u32, u32) -> u32) -> ID3D11Texture2D {
        let pixels: Vec<u32> = (0..HEIGHT)
            .flat_map(|y| (0..WIDTH).map(move |x| (x, y)))
            .map(|(x, y)| pixel(x, y))
            .collect();

        let description = D3D11_TEXTURE2D_DESC {
            Width: WIDTH,
            Height: HEIGHT,
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
            SysMemPitch: WIDTH * 4,
            SysMemSlicePitch: 0,
        };
        let mut texture: Option<ID3D11Texture2D> = None;
        // SAFETY: `description` and `data` are live locals; `pixels` holds
        // exactly `WIDTH * HEIGHT` four-byte pixels laid out at the pitch `data`
        // declares, and it outlives this call, which is the only time Direct3D
        // reads it.
        unsafe {
            device
                .CreateTexture2D(
                    &raw const description,
                    Some(&raw const data),
                    Some(&raw mut texture),
                )
                .expect("a BGRA8 texture with initial data");
        }
        texture.expect("CreateTexture2D returned success and a texture")
    }

    /// Samples a texture as if a backend had just handed it over.
    fn sample_of(texture: &ID3D11Texture2D) -> Option<FrameSample> {
        let format = FrameFormat::new(
            FrameSize::new(WIDTH, HEIGHT).expect("the test size is not zero"),
            PixelFormat::Bgra8Unorm,
        );
        // SAFETY: `texture` is a live `ID3D11Texture2D` owned by the caller for
        // the whole of this call, which outlives the `FrameTexture` and the
        // `CapturedFrame` built around it here.
        let borrowed = unsafe { FrameTexture::new(TextureKind::D3d11Texture2D, texture.as_raw()) };
        let frame = CapturedFrame::new(
            borrowed,
            format,
            CaptureTimestamp::from_source(SourceClock::PerformanceCounter, 0),
        );
        D3d11FrameSampler::new().sample(&frame)
    }

    #[test]
    fn a_black_texture_samples_as_black() {
        // The known-black source issue #97 asks for. Its alpha is opaque, which
        // is the second half of the assertion: a pixel is judged on its colour
        // channels, and a frame whose alpha is 255 everywhere is still black.
        let Some(device) = device() else { return };
        let texture = texture_of(&device, |_, _| bgra(0, 0, 0));

        let sample = sample_of(&texture).expect("a BGRA8 texture can be sampled");
        assert_eq!(sample.sampled(), SAMPLES);
        assert_eq!(sample.lit(), 0);
        assert_eq!(sample.brightest(), 0);
        assert!(sample.is_black());
    }

    #[test]
    fn a_very_dark_texture_does_not_sample_as_black() {
        // The false positive the whole design turns on: a night-time game frame
        // is dark, not empty. A blue channel of 4 is darker than anything a game
        // renders and still nowhere near zero.
        let Some(device) = device() else { return };
        let texture = texture_of(&device, |_, _| bgra(0, 0, 4));

        let sample = sample_of(&texture).expect("a BGRA8 texture can be sampled");
        assert_eq!(sample.sampled(), SAMPLES);
        assert_eq!(
            sample.lit(),
            SAMPLES,
            "every sampled pixel had colour in it, however little"
        );
        assert_eq!(sample.brightest(), 4);
        assert!(!sample.is_black());
    }

    #[test]
    fn a_black_frame_with_one_lit_corner_is_not_black() {
        // A heads-up display in one corner and darkness everywhere else: the
        // grid has to reach it, or a working capture of a dark scene is reported
        // as broken. The painted region is the top-left quarter, which contains
        // exactly one of the sixteen grid points.
        let Some(device) = device() else { return };
        let texture = texture_of(&device, |x, y| {
            if x < WIDTH / 4 && y < HEIGHT / 4 {
                bgra(255, 255, 255)
            } else {
                bgra(0, 0, 0)
            }
        });

        let sample = sample_of(&texture).expect("a BGRA8 texture can be sampled");
        assert_eq!(
            sample.lit(),
            1,
            "exactly the one grid point inside the painted corner"
        );
        assert_eq!(sample.brightest(), 255);
        assert!(!sample.is_black());
    }

    #[test]
    fn an_unsampleable_frame_is_no_evidence_rather_than_black_evidence() {
        // A frame with no texture behind it — which is what a caller holding a
        // frame from a backend this sampler does not understand amounts to.
        // Reporting it as black would end recordings over a readback failure.
        let format = FrameFormat::new(
            FrameSize::new(WIDTH, HEIGHT).expect("the test size is not zero"),
            PixelFormat::Bgra8Unorm,
        );
        // SAFETY: the handle is never dereferenced — `sample` returns before
        // reaching it, which is what this test asserts.
        let borrowed =
            unsafe { FrameTexture::new(TextureKind::D3d11Texture2D, core::ptr::null_mut()) };
        let frame = CapturedFrame::new(
            borrowed,
            format,
            CaptureTimestamp::from_source(SourceClock::PerformanceCounter, 0),
        );

        assert_eq!(D3d11FrameSampler::new().sample(&frame), None);
    }

    #[test]
    fn the_grid_stays_inside_the_frame_and_off_its_edges() {
        for point in 0..SAMPLES {
            let (x, y) = grid_point(point, WIDTH, HEIGHT);
            assert!(
                x < WIDTH && y < HEIGHT,
                "point {point} is outside the frame"
            );
            assert!(
                x > 0 && y > 0,
                "point {point} is on the frame's edge, which is black in healthy captures"
            );
        }

        // The smallest frame there is, where every cell rounds to the same pixel.
        for point in 0..SAMPLES {
            assert_eq!(grid_point(point, 1, 1), (0, 0));
        }
    }
}
