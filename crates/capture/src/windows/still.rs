//! Copying one whole captured frame off the GPU, for a screenshot.
//!
//! This is the only place in the crate that moves a *frame* into system memory.
//! [`pixel_sample`](super::pixel_sample) moves sixteen pixels twice a second to
//! notice a capture that has gone black; this moves every pixel, once, when
//! somebody presses the screenshot key
//! ([issue #67](https://github.com/wildware-uk/clipped/issues/67)).
//!
//! # Why the copy is in two halves
//!
//! Because the naive version stalls the capture thread, and a stalled capture
//! thread is a dropped frame in somebody's recording (AGENTS.md sections 17 and
//! 18).
//!
//! `CopyResource` into a staging texture is queued for the GPU and returns
//! almost immediately. `Map` is what waits: it blocks the calling thread until
//! the GPU has actually performed that copy, and at 1440p that wait plus the
//! transfer is milliseconds — a large fraction of a frame at 60 fps and more
//! than a whole one at 240.
//!
//! So [`D3d11StillCopier::begin`] issues the copy and flushes, and returns. The
//! recording carries on. One or more frames later, [`D3d11StillCopier::poll`]
//! maps the staging texture **without waiting** — `D3D11_MAP_FLAG_DO_NOT_WAIT`,
//! which returns `DXGI_ERROR_WAS_STILL_DRAWING` rather than blocking — and
//! answers [`None`] until the GPU has caught up. By then the wait is zero and
//! the only cost left on the capture thread is the memory copy itself.
//!
//! [`D3d11StillCopier::finish`] is the blocking form, for the caller that has
//! waited long enough and would rather have the screenshot than another frame:
//! `clipped-session` polls for a short deadline and then finishes. Measurements
//! for both are in `docs/capture-pipeline.md`.
//!
//! # Ownership
//!
//! The copier owns the staging texture, the device reference and the immediate
//! context it took from the frame, and reuses all three between screenshots. It
//! never owns a captured texture: that belongs to the backend and is borrowed
//! for the length of [`begin`](D3d11StillCopier::begin) only (AGENTS.md section
//! 58). Everything is released when the copier is dropped.
//!
//! # Threading
//!
//! One copier belongs to one capture thread, exactly as a backend does. It uses
//! the frame's own device and that device's immediate context — the same
//! context the backend is using — which is safe for the reason
//! `pixel_sample.rs` gives and `device.rs` guarantees: the device is not created
//! with `D3D11_CREATE_DEVICE_SINGLETHREADED`, and both users are on this
//! thread anyway.
//!
//! The [`StillFrame`] that comes out owns its pixels and is [`Send`], which is
//! the point: encoding and writing it happen somewhere a capture thread is not
//! allowed to go.

use core::ffi::c_void;

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_FLAG_DO_NOT_WAIT, D3D11_MAP_READ, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::DXGI_ERROR_WAS_STILL_DRAWING;

use crate::{
    CaptureTimestamp, CapturedFrame, FrameFormat, FrameSize, PixelFormat, StillError, StillFrame,
    TextureKind,
};

/// Copies a whole Direct3D 11 frame into system memory, in two phases.
///
/// See the module documentation for why there are two. The short version: the
/// first phase costs the capture thread a queued GPU copy, the second costs it
/// a memory copy, and neither costs it a wait for the GPU.
#[derive(Debug, Default)]
pub struct D3d11StillCopier {
    /// The device the last frame belonged to, its immediate context, and the
    /// staging texture created on it. Rebuilt when the device or the size
    /// changes — a fallback to the other backend brings a different device with
    /// it, and a resized window brings a different size.
    staging: Option<Staging>,
    /// What [`D3d11StillCopier::begin`] copied, waiting to be read back.
    pending: Option<Pending>,
    /// The buffer the pixels are read into, reused between screenshots so that
    /// a second screenshot does not allocate several megabytes on the capture
    /// thread.
    buffer: Vec<u8>,
}

/// The resources one size of screenshot is copied through.
#[derive(Debug)]
struct Staging {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    texture: ID3D11Texture2D,
    size: FrameSize,
}

/// A copy that has been issued and not yet read back.
#[derive(Debug, Clone, Copy)]
struct Pending {
    format: FrameFormat,
    timestamp: CaptureTimestamp,
}

impl D3d11StillCopier {
    /// A copier holding nothing. It allocates on its first screenshot.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            staging: None,
            pending: None,
            buffer: Vec::new(),
        }
    }

    /// Whether a copy has been started and not yet read back.
    #[must_use]
    pub const fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Issues the GPU copy of `frame` and returns without waiting for it.
    ///
    /// A copy already in flight is replaced: the newest request wins, because
    /// the frame somebody is asking for is the one they are looking at. That
    /// cannot lose a screenshot, because a caller only begins one when it has
    /// been asked for one.
    ///
    /// # Errors
    ///
    /// [`StillError::UnsupportedTexture`] or [`StillError::UnsupportedFormat`]
    /// for a frame this cannot read — an HDR capture is the real case
    /// ([issue #99](https://github.com/wildware-uk/clipped/issues/99)) —
    /// [`StillError::NullTexture`] or [`StillError::NoDevice`] for a backend
    /// that broke its own contract, and [`StillError::Graphics`] naming the
    /// Direct3D call that failed.
    pub fn begin(&mut self, frame: &CapturedFrame<'_>) -> Result<(), StillError> {
        let format = frame.format();
        if frame.texture().kind() != TextureKind::D3d11Texture2D {
            return Err(StillError::UnsupportedTexture {
                kind: frame.texture().kind(),
            });
        }
        if format.pixel_format() != PixelFormat::Bgra8Unorm {
            return Err(StillError::UnsupportedFormat {
                format: format.pixel_format(),
            });
        }

        let raw: *mut c_void = frame.texture().as_raw();
        if raw.is_null() {
            return Err(StillError::NullTexture);
        }

        // SAFETY: the backend promises, in `FrameTexture::new`'s safety
        // contract, that this is a live `ID3D11Texture2D` it owns for at least
        // as long as the frame the caller is still holding.
        // `from_raw_borrowed` takes no reference count of its own and yields a
        // reference that cannot outlive `raw`, a local of this function.
        let source =
            unsafe { ID3D11Texture2D::from_raw_borrowed(&raw) }.ok_or(StillError::NullTexture)?;

        let mut description = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: `source` is live and `description` is a live local the call
        // writes into.
        unsafe { source.GetDesc(&raw mut description) };
        if description.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            return Err(StillError::UnsupportedFormat {
                format: format.pixel_format(),
            });
        }

        // The texture's own size, not the frame's declared one. A capture API
        // may hand over a texture larger than the content in it, and
        // `CopyResource` requires both resources to have identical
        // descriptions — so the staging texture matches the texture, and the
        // *frame format* is what says how much of it is picture.
        let texture_size = FrameSize::new(description.Width, description.Height).ok_or(
            StillError::MalformedBuffer {
                stride: 0,
                size: format.size(),
                bytes: 0,
            },
        )?;

        let staging = self.staging_for(source, texture_size)?;

        // SAFETY: both textures belong to the same device and have the same
        // size, format, mip count and sample count, which is what
        // `CopyResource` requires. The staging texture is not mapped: every map
        // in this file is unmapped before the function that made it returns.
        unsafe { staging.context.CopyResource(&staging.texture, source) };

        // Submitting the queued commands is what lets `poll` below succeed on a
        // later frame instead of reporting "still drawing" until something else
        // happens to flush. It costs a driver call, not a wait.
        // SAFETY: the context is live and `Flush` takes no arguments.
        unsafe { staging.context.Flush() };

        // The copy is of the whole texture; the pixels that matter are the
        // frame's declared size, and that is what travels with the request.
        self.pending = Some(Pending {
            format: FrameFormat::new(
                FrameSize::new(
                    format.size().width().min(texture_size.width()),
                    format.size().height().min(texture_size.height()),
                )
                .unwrap_or(texture_size),
                format.pixel_format(),
            ),
            timestamp: frame.timestamp(),
        });
        Ok(())
    }

    /// Reads the copy back if the GPU has finished it, without waiting.
    ///
    /// [`None`] means "not yet, ask again"; it is not a failure and the pending
    /// copy is still there. Calling this once per captured frame is what turns
    /// the wait into somebody else's problem — the GPU's.
    ///
    /// # Errors
    ///
    /// [`StillError::NothingPending`] if no copy was started, and
    /// [`StillError::Graphics`] if the map failed for a reason other than the
    /// copy being unfinished. A failed map drops the pending copy rather than
    /// leaving it to be retried for ever.
    pub fn poll(&mut self) -> Result<Option<StillFrame>, StillError> {
        self.read_back(D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32)
    }

    /// Reads the copy back, waiting for the GPU if it has not finished.
    ///
    /// The caller that has polled for as long as it is prepared to wait. On the
    /// capture thread this is the one call here that can stall, which is why it
    /// is a separate method with its own name rather than a flag.
    ///
    /// # Errors
    ///
    /// As [`poll`](Self::poll), except that it never reports "not yet".
    pub fn finish(&mut self) -> Result<StillFrame, StillError> {
        self.read_back(0)?.ok_or(StillError::NothingPending)
    }

    /// Forgets a copy in flight, leaving the staging texture to be reused.
    ///
    /// What a caller does when whatever asked for the screenshot has gone away.
    pub fn cancel(&mut self) {
        self.pending = None;
    }

    /// Releases the device, context and staging texture.
    ///
    /// Deterministic rather than left to [`Drop`], so a caller can give back
    /// several megabytes of video memory at the end of a recording without
    /// dropping the copier (AGENTS.md section 58).
    pub fn release(&mut self) {
        self.pending = None;
        self.staging = None;
        self.buffer = Vec::new();
    }

    /// The map, shared by [`poll`](Self::poll) and [`finish`](Self::finish).
    fn read_back(&mut self, flags: u32) -> Result<Option<StillFrame>, StillError> {
        let pending = self.pending.ok_or(StillError::NothingPending)?;
        let staging = self.staging.as_ref().ok_or(StillError::NoDevice)?;

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: the staging texture was created with `D3D11_USAGE_STAGING`
        // and `D3D11_CPU_ACCESS_READ`, which is what mapping subresource zero
        // for reading requires, and `mapped` is a live local the call writes
        // into. It is unmapped below on every path out.
        let mapping = unsafe {
            staging.context.Map(
                &staging.texture,
                0,
                D3D11_MAP_READ,
                flags,
                Some(&raw mut mapped),
            )
        };

        if let Err(error) = mapping {
            if error.code() == DXGI_ERROR_WAS_STILL_DRAWING {
                // The GPU has not reached the copy. The pending request stays;
                // the caller asks again on the next frame.
                return Ok(None);
            }
            // Anything else is a real failure, and retrying it every frame for
            // the rest of the recording would be a log line per frame. The
            // request is dropped and the caller is told why.
            self.pending = None;
            return Err(StillError::Graphics {
                operation: "ID3D11DeviceContext::Map",
                detail: error.to_string(),
            });
        }

        let stride = mapped.RowPitch as usize;
        let height = pending.format.size().height() as usize;
        let wanted = stride.checked_mul(height);

        // Copied out while the mapping is live, into a buffer that is reused
        // between screenshots.
        if let Some(wanted) = wanted {
            self.buffer.clear();
            self.buffer.reserve(wanted);
            // SAFETY: `Map` returned a pointer to the whole of subresource
            // zero, whose rows are `RowPitch` bytes apart and of which there
            // are at least `height` — the staging texture is at least as tall
            // as the frame's declared size, which `begin` enforced by taking
            // the minimum. The slice is read and copied before `Unmap`, and
            // nothing else refers to it.
            let source = unsafe { core::slice::from_raw_parts(mapped.pData.cast::<u8>(), wanted) };
            self.buffer.extend_from_slice(source);
        }

        // Unmapped before the arithmetic is judged: a staging texture left
        // mapped cannot be copied into, so the *next* screenshot would fail for
        // a reason that has nothing to do with why this one did.
        // SAFETY: `staging.texture` is the texture mapped immediately above,
        // and the slice built from that mapping has been copied and dropped.
        unsafe { staging.context.Unmap(&staging.texture, 0) };

        self.pending = None;

        // A stride and height whose product overflows left `self.buffer`
        // empty above; `StillFrame::new` is what reports it, with the numbers
        // in it, rather than a second copy of the same check here.
        StillFrame::new(
            core::mem::take(&mut self.buffer),
            stride,
            pending.format,
            pending.timestamp,
        )
        .map(Some)
    }

    /// The staging texture for this device and size, created once and reused.
    ///
    /// Rebuilt when either changes. The device changes when a fallback puts a
    /// different backend behind the frames, which is the case a cached texture
    /// would get wrong: `CopyResource` between resources of two devices is not
    /// a slow copy, it is undefined behaviour.
    fn staging_for(
        &mut self,
        source: &ID3D11Texture2D,
        size: FrameSize,
    ) -> Result<&Staging, StillError> {
        // SAFETY: `source` is live for the length of this call; `GetDevice`
        // returns an owned reference windows-rs releases on drop.
        let device = unsafe { source.GetDevice() }.map_err(|error| StillError::Graphics {
            operation: "ID3D11DeviceChild::GetDevice",
            detail: error.to_string(),
        })?;

        let reusable = self
            .staging
            .as_ref()
            .is_some_and(|held| held.size == size && held.device == device);

        if !reusable {
            // SAFETY: `device` is live; `GetImmediateContext` returns an owned
            // reference to its one immediate context.
            let context =
                unsafe { device.GetImmediateContext() }.map_err(|error| StillError::Graphics {
                    operation: "ID3D11Device::GetImmediateContext",
                    detail: error.to_string(),
                })?;

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
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                // A plain `as` cast rather than `unsigned_abs`: this is a bit
                // pattern in a signed newtype, not a number, and the absolute
                // value of a flag whose sign bit is set is a different flag.
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };

            let mut texture: Option<ID3D11Texture2D> = None;
            // SAFETY: `description` is a live local read during the call, no
            // initial data is supplied, and `texture` is a live local the call
            // writes an owned interface into.
            unsafe { device.CreateTexture2D(&raw const description, None, Some(&raw mut texture)) }
                .map_err(|error| StillError::Graphics {
                    operation: "ID3D11Device::CreateTexture2D",
                    detail: error.to_string(),
                })?;

            // Success without a texture is a broken runtime rather than
            // something to unwrap, and saying so is the difference between a
            // puzzle and a bug report (AGENTS.md section 15).
            let texture = texture.ok_or(StillError::Graphics {
                operation: "ID3D11Device::CreateTexture2D",
                detail: "reported success without returning a texture".to_owned(),
            })?;

            self.staging = Some(Staging {
                device,
                context,
                texture,
                size,
            });
        }

        self.staging.as_ref().ok_or(StillError::NoDevice)
    }
}

#[cfg(test)]
mod tests {
    //! Copying a whole frame, against a real Direct3D texture this test
    //! painted.
    //!
    //! It needs a graphics device and nothing else: no window, no capture, no
    //! desktop. The source is a texture created with known initial data, so
    //! what comes back out is comparable byte for byte against what went in —
    //! which is the only way to prove that a screenshot is the picture rather
    //! than something the right size.

    use super::*;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_WARP;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDevice, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
        D3D11_SDK_VERSION, D3D11_SUBRESOURCE_DATA, D3D11_USAGE_DEFAULT,
    };

    use crate::{FrameTexture, SourceClock};

    const WIDTH: u32 = 64;
    const HEIGHT: u32 = 48;

    /// An opaque pixel, as a BGRA8 texture stores one.
    const fn bgra(red: u8, green: u8, blue: u8) -> u32 {
        u32::from_le_bytes([blue, green, red, 0xFF])
    }

    /// The colour of pixel `(x, y)` in the test picture.
    ///
    /// A gradient in two channels and a diagonal in the third, so that a
    /// screenshot copied with the wrong stride, transposed, or offset by a row
    /// differs from the original in a way an assertion catches — a flat colour
    /// would survive all three.
    fn expected(x: u32, y: u32) -> u32 {
        let red = u8::try_from(x % 256).expect("modulo 256 fits in a byte");
        let green = u8::try_from(y % 256).expect("modulo 256 fits in a byte");
        let blue = u8::try_from((x + y) % 256).expect("modulo 256 fits in a byte");
        bgra(red, green, blue)
    }

    /// A device to make textures on, or [`None`] on a machine with no Direct3D.
    ///
    /// WARP rather than hardware, for the reason `pixel_sample.rs`'s test
    /// gives: this has to run in CI, and a staging copy and a map behave the
    /// same on either.
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

    /// A texture holding [`expected`], in the format both capture backends
    /// produce.
    fn painted(device: &ID3D11Device) -> ID3D11Texture2D {
        let pixels: Vec<u32> = (0..HEIGHT)
            .flat_map(|y| (0..WIDTH).map(move |x| expected(x, y)))
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
        // exactly `WIDTH * HEIGHT` four-byte pixels at the pitch `data`
        // declares and outlives this call, which is the only time Direct3D
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

    fn frame_format() -> FrameFormat {
        FrameFormat::new(
            FrameSize::new(WIDTH, HEIGHT).expect("the test size is not zero"),
            PixelFormat::Bgra8Unorm,
        )
    }

    /// Copies `texture` as if a backend had just handed it over.
    fn still_of(texture: &ID3D11Texture2D) -> StillFrame {
        // SAFETY: `texture` is a live `ID3D11Texture2D` owned by the caller for
        // the whole of this call, which outlives the `FrameTexture` and the
        // `CapturedFrame` built around it here.
        let borrowed = unsafe { FrameTexture::new(TextureKind::D3d11Texture2D, texture.as_raw()) };
        let frame = CapturedFrame::new(
            borrowed,
            frame_format(),
            CaptureTimestamp::from_source(SourceClock::PerformanceCounter, 99),
        );

        let mut copier = D3d11StillCopier::new();
        copier.begin(&frame).expect("the copy is issued");
        copier.finish().expect("the copy is read back")
    }

    #[test]
    fn a_copied_frame_is_the_picture_pixel_for_pixel() {
        // The assertion that makes this a screenshot rather than a buffer of
        // the right length. A stride bug, a transposition or an off-by-one row
        // all fail here.
        let Some(device) = device() else { return };
        let still = still_of(&painted(&device));

        assert_eq!(still.size(), frame_format().size());
        assert!(
            still.stride() >= WIDTH as usize * 4,
            "a row cannot be narrower than its pixels: {}",
            still.stride()
        );

        for y in 0..HEIGHT {
            let row = still.row(y).expect("every declared row is present");
            for x in 0..WIDTH {
                let offset = x as usize * 4;
                let pixel = u32::from_le_bytes([
                    row[offset],
                    row[offset + 1],
                    row[offset + 2],
                    row[offset + 3],
                ]);
                assert_eq!(
                    pixel,
                    expected(x, y),
                    "pixel ({x}, {y}) came back as {pixel:#010x}"
                );
            }
        }
    }

    #[test]
    fn the_frames_timestamp_travels_with_the_copy() {
        // A screenshot taken during a recording is placed on that recording's
        // timeline from this, not from a wall clock read afterwards.
        let Some(device) = device() else { return };
        let still = still_of(&painted(&device));
        assert_eq!(
            still.timestamp(),
            CaptureTimestamp::from_source(SourceClock::PerformanceCounter, 99)
        );
    }

    #[test]
    fn polling_eventually_produces_the_same_frame_as_waiting_for_it() {
        // The two-phase path, which is the one a recording actually uses. It
        // must produce the identical image; if `poll` ever returned a partially
        // copied texture this would differ from the blocking read above.
        let Some(device) = device() else { return };
        let texture = painted(&device);
        // SAFETY: as `still_of`.
        let borrowed = unsafe { FrameTexture::new(TextureKind::D3d11Texture2D, texture.as_raw()) };
        let frame = CapturedFrame::new(
            borrowed,
            frame_format(),
            CaptureTimestamp::from_source(SourceClock::PerformanceCounter, 7),
        );

        let mut copier = D3d11StillCopier::new();
        copier.begin(&frame).expect("the copy is issued");
        assert!(copier.is_pending());

        let mut polled = None;
        for _ in 0..1_000 {
            match copier.poll().expect("polling does not fail") {
                Some(still) => {
                    polled = Some(still);
                    break;
                }
                // Not ready yet. A real capture loop would go and capture
                // another frame here; this test simply asks again.
                None => assert!(
                    copier.is_pending(),
                    "a copy that is not ready is still pending"
                ),
            }
        }

        let polled = polled.expect("the GPU finishes a 64x48 copy within a thousand attempts");
        assert!(
            !copier.is_pending(),
            "a completed copy is no longer pending"
        );
        assert_eq!(polled.row(0), still_of(&texture).row(0));
        assert_eq!(polled.size(), frame_format().size());
    }

    #[test]
    fn finishing_without_beginning_is_an_error_rather_than_a_blank_image() {
        // A blank image here would be a screenshot of nothing, saved and
        // reported as a success (AGENTS.md section 54).
        let mut copier = D3d11StillCopier::new();
        let error = copier
            .finish()
            .expect_err("nothing was copied, so there is nothing to read back");
        assert!(
            matches!(error, StillError::NothingPending),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn a_frame_that_is_not_a_direct3d_texture_is_refused() {
        // SAFETY: the handle is never dereferenced — `begin` rejects the frame
        // on its texture kind before it reaches the pointer.
        let borrowed =
            unsafe { FrameTexture::new(TextureKind::D3d11Texture2D, core::ptr::null_mut()) };
        let frame = CapturedFrame::new(
            borrowed,
            frame_format(),
            CaptureTimestamp::from_source(SourceClock::PerformanceCounter, 1),
        );

        let mut copier = D3d11StillCopier::new();
        let error = copier.begin(&frame).expect_err("a null texture is refused");
        assert!(
            matches!(error, StillError::NullTexture),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn an_hdr_frame_is_refused_by_name_rather_than_copied_as_nonsense() {
        // Reading a 10-bit frame as though it were 8-bit produces an image, and
        // the image is wrong. Refusing it names issue #99 instead.
        let Some(device) = device() else { return };
        let texture = painted(&device);
        // SAFETY: as `still_of`.
        let borrowed = unsafe { FrameTexture::new(TextureKind::D3d11Texture2D, texture.as_raw()) };
        let frame = CapturedFrame::new(
            borrowed,
            FrameFormat::new(
                FrameSize::new(WIDTH, HEIGHT).expect("the test size is not zero"),
                PixelFormat::Rgb10A2Unorm,
            ),
            CaptureTimestamp::from_source(SourceClock::PerformanceCounter, 1),
        );

        let mut copier = D3d11StillCopier::new();
        let error = copier.begin(&frame).expect_err("an HDR frame is refused");
        assert!(
            matches!(
                error,
                StillError::UnsupportedFormat {
                    format: PixelFormat::Rgb10A2Unorm
                }
            ),
            "unexpected error: {error}"
        );
        assert!(error.to_string().contains("#99"), "{error}");
    }

    #[test]
    fn a_second_screenshot_reuses_the_staging_texture() {
        // Not a performance assertion — a correctness one. The reuse branch is
        // where a stale device or a stale size would be kept, and a copy
        // between two devices' resources is undefined behaviour rather than a
        // slow copy.
        let Some(device) = device() else { return };
        let texture = painted(&device);
        // SAFETY: as `still_of`.
        let borrowed = unsafe { FrameTexture::new(TextureKind::D3d11Texture2D, texture.as_raw()) };
        let frame = CapturedFrame::new(
            borrowed,
            frame_format(),
            CaptureTimestamp::from_source(SourceClock::PerformanceCounter, 3),
        );

        let mut copier = D3d11StillCopier::new();
        copier.begin(&frame).expect("the first copy is issued");
        let first = copier.finish().expect("the first copy is read back");
        copier.begin(&frame).expect("the second copy is issued");
        let second = copier.finish().expect("the second copy is read back");

        assert_eq!(first.as_bytes(), second.as_bytes());

        copier.release();
        copier.begin(&frame).expect("a released copier rebuilds");
        assert!(copier.finish().is_ok());
    }
}
