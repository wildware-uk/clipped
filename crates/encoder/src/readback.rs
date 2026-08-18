//! Getting a captured frame off the GPU and into system memory.
//!
//! This module is the whole reason software encoding is expensive, and it is
//! kept in one file so that the cost is somewhere a reader can find rather than
//! spread through the encoder.
//!
//! A hardware encoder is handed the capture backend's own texture and reads it
//! where it already lives (`docs/encoder-pipeline.md`). A CPU encoder cannot:
//! the pixels are in video memory, and `libopenh264` is a function call that
//! wants a pointer. So every frame makes a round trip that the hardware path
//! never makes:
//!
//! ```text
//! ID3D11Texture2D (BGRA, video memory)
//!         │  CopyResource            ← the GPU copy
//! staging ID3D11Texture2D (BGRA, system memory, CPU-readable)
//!         │  Map(D3D11_MAP_READ)     ← the wait: the CPU blocks until the
//!         │                             GPU has finished the copy
//!   &[u8] in system memory
//! ```
//!
//! At 2560x1440 that is 14 MB per frame — 850 MB/s at 60 frames a second — plus
//! a CPU/GPU synchronisation in the middle of the game's frame, because `Map`
//! without `D3D11_MAP_FLAG_DO_NOT_WAIT` waits for the copy to retire. Both
//! costs are measured rather than described: [`Readback::elapsed`] accumulates
//! the time spent here, and the encoder logs it when the session closes
//! (AGENTS.md sections 18 and 19).
//!
//! # Why one staging texture and not a rotating pool
//!
//! A pool of two or three staging textures would let the CPU read frame *n*
//! while the GPU copies frame *n + 1*, which is how a latency-tolerant readback
//! is normally built, and it would hide most of the wait. It is not what this
//! does, for a reason that comes from the interface rather than from
//! convenience: [`VideoEncoder::submit`](crate::VideoEncoder::submit) promises
//! that nothing derived from the caller's texture outlives the call, so the copy
//! has to have happened before `submit` returns whatever else is true. Waiting
//! one frame later would mean holding the caller's texture for that frame, and
//! a capture backend recycles its frame pool surfaces.
//!
//! What a pool would buy is therefore not the copy but the *wait*, and buying
//! that needs a pipeline that can hold a frame back — which is a change to the
//! encoder interface, not to this file
//! ([#156](https://github.com/wildware-uk/clipped/issues/156)).
//!
//! # Ownership
//!
//! [`Readback`] owns the staging texture and its own reference to the device and
//! the immediate context, all three released when it is dropped. It owns no
//! frame: the texture handed to [`Readback::map`] belongs to the capture
//! backend, is read inside that call, and is not retained.

use core::ffi::c_void;
use core::time::Duration;
use std::time::Instant;

use windows::core::Interface as _;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Resource, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

use crate::codec::Resolution;

/// What can go wrong between the texture and the bytes.
///
/// Deliberately not [`EncodeErrorKind`](crate::EncodeErrorKind): this module
/// knows about Direct3D and nothing about encoders, and the caller
/// ([`crate::SoftwareEncoder`]) is where the two meet.
#[derive(Debug)]
pub(crate) enum ReadbackError {
    /// The handle is not a live Direct3D 11 device, or not a texture.
    NotDirect3D {
        /// What was expected, as in `ID3D11Device`.
        expected: &'static str,
        /// What Windows said when it was asked for that interface.
        detail: String,
    },
    /// The frame is not the size or the format the session was opened for.
    ///
    /// Checked rather than assumed because `CopyResource` returns `void`: a
    /// mismatch there is silently nothing at all, and the encoder would code
    /// whatever the staging texture happened to contain — a black or a stale
    /// picture, with nothing in any log to explain it.
    Mismatched {
        /// What is wrong, in the reader's terms.
        detail: String,
    },
    /// A Direct3D call failed.
    Api {
        /// The call, as in `ID3D11Device::CreateTexture2D`.
        operation: &'static str,
        /// The `HRESULT` it returned, which is what tells a lost device from
        /// an ordinary failure.
        code: i32,
        /// What it said.
        detail: String,
    },
}

/// The staging texture a frame is copied through, and the timings of doing it.
pub(crate) struct Readback {
    /// Kept alive for the session, because the staging texture belongs to it.
    _device: ID3D11Device,
    context: ID3D11DeviceContext,
    staging: ID3D11Texture2D,
    resolution: Resolution,
    elapsed: Duration,
    frames: u64,
}

impl Readback {
    /// Creates the staging texture for `resolution` on `device`.
    ///
    /// # Safety
    ///
    /// `device` must be a live `ID3D11Device` that stays valid for as long as
    /// the returned value — which is what
    /// [`GraphicsDevice`](crate::GraphicsDevice) promises of the handle inside
    /// it. This function takes its own reference to it, so the device outlives
    /// the caller's borrow, but it cannot outlive the caller's *device*.
    pub(crate) unsafe fn open(
        device: *mut c_void,
        resolution: Resolution,
    ) -> Result<Self, ReadbackError> {
        // SAFETY: the caller guarantees `device` is a live COM object. The
        // borrowed reference is upgraded to an owned one by `cast`, which is a
        // `QueryInterface` and therefore also the check that this really is a
        // Direct3D 11 device rather than some other interface — worth one call
        // at open time, because everything after it assumes it.
        let device: ID3D11Device = unsafe { ID3D11Device::from_raw_borrowed(&device) }
            .ok_or_else(|| ReadbackError::NotDirect3D {
                expected: "ID3D11Device",
                detail: "the device handle is null".to_owned(),
            })?
            .cast()
            .map_err(|error| ReadbackError::NotDirect3D {
                expected: "ID3D11Device",
                detail: error.to_string(),
            })?;

        // SAFETY: `device` is live and holds a reference of its own. The
        // immediate context comes back with a reference this value then owns.
        let context =
            unsafe { device.GetImmediateContext() }.map_err(|error| ReadbackError::Api {
                operation: "ID3D11Device::GetImmediateContext",
                code: error.code().0,
                detail: error.to_string(),
            })?;

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
            // Staging is the only usage a CPU may read: it lives in system
            // memory, the GPU can copy into it, and nothing can bind it.
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            // The flag constants are signed in the bindings and the field is
            // unsigned, which is a `windows` crate detail rather than a
            // conversion that can be wrong: the value is a small positive flag.
            #[allow(clippy::cast_sign_loss)]
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };

        let mut staging: Option<ID3D11Texture2D> = None;
        // SAFETY: the description is a live local, no initial data is supplied
        // (a staging texture is filled by copying into it), and `staging` is a
        // live out-parameter which receives one reference.
        unsafe { device.CreateTexture2D(&description, None, Some(&mut staging)) }.map_err(
            |error| ReadbackError::Api {
                operation: "ID3D11Device::CreateTexture2D",
                code: error.code().0,
                detail: format!("a {resolution} staging texture could not be created: {error}"),
            },
        )?;
        let staging = staging.ok_or_else(|| ReadbackError::Api {
            operation: "ID3D11Device::CreateTexture2D",
            code: 0,
            detail: "reported success without a texture".to_owned(),
        })?;

        Ok(Self {
            _device: device,
            context,
            staging,
            resolution,
            elapsed: Duration::ZERO,
            frames: 0,
        })
    }

    /// Copies `texture` into system memory and hands the pixels to `use_pixels`.
    ///
    /// The borrow is over as soon as `use_pixels` returns: the staging texture
    /// is unmapped on the way out, by every path including a panic in the
    /// closure, so the same buffer is available to the next frame.
    ///
    /// `use_pixels` receives the BGRA bytes and the row stride, which is the
    /// driver's and is usually larger than four times the width.
    ///
    /// # Safety
    ///
    /// `texture` must be a live `ID3D11Texture2D` on the device this was opened
    /// against, valid for the duration of the call.
    pub(crate) unsafe fn map<T>(
        &mut self,
        texture: *mut c_void,
        use_pixels: impl FnOnce(&[u8], usize) -> T,
    ) -> Result<T, ReadbackError> {
        let started = Instant::now();

        // SAFETY: the caller guarantees a live `ID3D11Texture2D`. This borrows
        // it without taking a reference, so nothing here can keep it alive past
        // the call — which is the promise `submit` makes to the capture
        // backend.
        let source = unsafe { ID3D11Texture2D::from_raw_borrowed(&texture) }.ok_or_else(|| {
            ReadbackError::NotDirect3D {
                expected: "ID3D11Texture2D",
                detail: "the frame's texture handle is null".to_owned(),
            }
        })?;
        self.check(source)?;

        // SAFETY: both textures are live, have identical dimensions, format,
        // mip and array counts — `check` has just confirmed it of the source
        // and `open` created the destination that way — which is what
        // `CopyResource` requires. Nothing else is using the immediate context:
        // this method takes `&mut self`, and `SoftwareEncoder` is not `Sync`.
        unsafe {
            self.context.CopyResource(
                &self
                    .staging
                    .cast::<ID3D11Resource>()
                    .map_err(|error| ReadbackError::Api {
                        operation: "ID3D11Texture2D::QueryInterface(ID3D11Resource)",
                        code: error.code().0,
                        detail: error.to_string(),
                    })?,
                &source
                    .cast::<ID3D11Resource>()
                    .map_err(|error| ReadbackError::Api {
                        operation: "ID3D11Texture2D::QueryInterface(ID3D11Resource)",
                        code: error.code().0,
                        detail: error.to_string(),
                    })?,
            );
        }

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: the staging texture is live and was created with
        // `D3D11_CPU_ACCESS_READ`, subresource 0 is the whole of it because it
        // has one mip level and one array slice, and `mapped` is a live
        // out-parameter. Without `D3D11_MAP_FLAG_DO_NOT_WAIT` this blocks until
        // the copy above has retired, which is the synchronisation this module
        // exists to be honest about.
        unsafe {
            self.context
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
        }
        .map_err(|error| ReadbackError::Api {
            operation: "ID3D11DeviceContext::Map",
            code: error.code().0,
            detail: error.to_string(),
        })?;

        // Unmapped by `Drop` rather than at the end of this function, so that a
        // panic inside `use_pixels` cannot leave the staging texture mapped —
        // which would fail every later frame and leak the mapping until the
        // device died (AGENTS.md section 58).
        let guard = Mapping {
            context: &self.context,
            staging: &self.staging,
        };

        let stride = mapped.RowPitch as usize;
        let height = self.resolution.height as usize;
        if mapped.pData.is_null() || stride < self.resolution.width as usize * 4 {
            return Err(ReadbackError::Api {
                operation: "ID3D11DeviceContext::Map",
                code: 0,
                detail: format!(
                    "reported success with {} bytes per row for a {} picture",
                    stride, self.resolution
                ),
            });
        }

        // The clock stops here and starts again for the unmap, so that what is
        // attributed to the readback is the copy and the wait and nothing else.
        // `use_pixels` is the colour conversion, which is measured separately
        // and is the same order of magnitude: timing the two together would
        // make a report of where the frame time went point at the wrong stage,
        // which is the whole purpose of measuring it (AGENTS.md section 19).
        self.elapsed += started.elapsed();

        // SAFETY: `Map` reported success, so `pData` points at `RowPitch`
        // bytes for each of the texture's `Height` rows, readable until the
        // matching `Unmap` — which `guard` performs after this borrow has
        // ended. The bytes are plain data with no alignment requirement beyond
        // one byte.
        let pixels =
            unsafe { core::slice::from_raw_parts(mapped.pData.cast::<u8>(), stride * height) };
        let result = use_pixels(pixels, stride);

        let unmapping = Instant::now();
        drop(guard);
        self.elapsed += unmapping.elapsed();
        self.frames += 1;
        Ok(result)
    }

    /// Fails unless the frame is the picture this staging texture can take.
    ///
    /// Everything `CopyResource` requires to match, not only the parts a reader
    /// would think of: it copies whole resources, so the source has to be the
    /// same *shape* as the staging texture — one mip level, one array slice and
    /// one sample — as well as the same size and format. A multisampled or
    /// array-sliced source of the identical size and format would otherwise
    /// pass this check, be copied by a call that returns `void`, and leave the
    /// encoder coding whatever the staging texture held last, which is the
    /// failure [`ReadbackError::Mismatched`] exists to make impossible.
    ///
    /// Desktop Duplication and Windows Graphics Capture hand over neither
    /// today, so this is a guard rather than a live case; the point of a guard
    /// is that it covers what its documentation says it covers.
    fn check(&self, source: &ID3D11Texture2D) -> Result<(), ReadbackError> {
        let mut description = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: `source` is live and `description` is a live out-parameter.
        // `GetDesc` reads the description the runtime already holds; it makes
        // no driver call.
        unsafe { source.GetDesc(&mut description) };

        if description.Width != self.resolution.width
            || description.Height != self.resolution.height
        {
            return Err(ReadbackError::Mismatched {
                detail: format!(
                    "a {}x{} texture arrived for a session reading {} frames",
                    description.Width, description.Height, self.resolution
                ),
            });
        }
        if description.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            return Err(ReadbackError::Mismatched {
                detail: format!(
                    "the frame's texture is DXGI format {}, and this encoder copies \
                     DXGI_FORMAT_B8G8R8A8_UNORM ({}) frames",
                    description.Format.0, DXGI_FORMAT_B8G8R8A8_UNORM.0
                ),
            });
        }
        if description.SampleDesc.Count != 1 {
            return Err(ReadbackError::Mismatched {
                detail: format!(
                    "the frame's texture is {}x multisampled, and a multisampled texture \
                     has to be resolved before it can be copied, not read as a picture",
                    description.SampleDesc.Count
                ),
            });
        }
        if description.MipLevels != 1 {
            return Err(ReadbackError::Mismatched {
                detail: format!(
                    "the frame's texture is a chain of {} mip levels, and this encoder \
                     copies a single-level texture",
                    description.MipLevels
                ),
            });
        }
        if description.ArraySize != 1 {
            return Err(ReadbackError::Mismatched {
                detail: format!(
                    "the frame's texture is an array of {} slices, and this encoder copies \
                     a single-slice texture",
                    description.ArraySize
                ),
            });
        }
        Ok(())
    }

    /// How long has been spent copying frames out of video memory, and over how
    /// many frames.
    pub(crate) const fn elapsed(&self) -> (Duration, u64) {
        (self.elapsed, self.frames)
    }
}

impl core::fmt::Debug for Readback {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Readback")
            .field("resolution", &self.resolution)
            .field("frames", &self.frames)
            .field("elapsed", &self.elapsed)
            .finish()
    }
}

/// Unmaps the staging texture when it goes out of scope.
struct Mapping<'readback> {
    context: &'readback ID3D11DeviceContext,
    staging: &'readback ID3D11Texture2D,
}

impl Drop for Mapping<'_> {
    fn drop(&mut self) {
        // SAFETY: this value is created only after a successful `Map` of
        // subresource 0 of this texture on this context, and it is dropped
        // exactly once, so the map and the unmap are paired.
        unsafe { self.context.Unmap(self.staging, 0) };
    }
}
