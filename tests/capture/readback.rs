//! Reading a captured frame's pixels back into system memory, for tests only.
//!
//! # Why this is here and not in `clipped-capture`
//!
//! Nothing in the product ever does this. A frame stays on the GPU from the
//! compositor to the encoder — that is the single performance property the
//! whole pipeline is built around (`docs/capture-pipeline.md`), and a readback
//! per frame would undo it. A *test*, though, has to look at the pixels to have
//! an opinion about them, so the copy lives here, in the test that needs it,
//! where it cannot be reached by accident from anything that ships.
//!
//! # Which device does the copy
//!
//! The frame's own. A Direct3D 11 texture belongs to the device that created it
//! — here, the one inside the capture backend — and a texture cannot be copied
//! into a resource of a different device without a shared handle. So the reader
//! asks the texture which device it belongs to, and creates its staging texture
//! there.
//!
//! That device's immediate context is also used by the capture backend's frame
//! pool, on the compositor's thread. That is safe because the backend does not
//! create its device with `D3D11_CREATE_DEVICE_SINGLETHREADED`
//! (`crates/capture/src/windows/device.rs`), which is exactly the flag that
//! turns off the runtime's own synchronisation of the immediate context. If
//! that ever changes, this file has to change with it.
//!
//! # Ownership
//!
//! [`FrameReader`] owns the device reference, the immediate context and the
//! staging texture it reuses between frames, and releases all three when it is
//! dropped. It never owns the captured texture: that belongs to the backend and
//! is borrowed for the duration of one call (AGENTS.md section 58).

use core::fmt;

use clipped_capture::{FrameTexture, TextureKind};
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM};

/// One frame's pixels, in system memory.
pub(crate) struct Image {
    /// BGRA8 pixels, `stride` bytes a row.
    pub(crate) pixels: Vec<u8>,
    /// Bytes between the start of one row and the next.
    pub(crate) stride: usize,
    /// Width in pixels.
    pub(crate) width: u32,
    /// Height in pixels.
    pub(crate) height: u32,
}

/// Copies captured frames into system memory so a test can look at them.
#[derive(Debug, Default)]
pub(crate) struct FrameReader {
    device: Option<ID3D11Device>,
    context: Option<ID3D11DeviceContext>,
    staging: Option<(ID3D11Texture2D, u32, u32)>,
}

impl FrameReader {
    /// Copies `texture` into system memory.
    ///
    /// # Errors
    ///
    /// [`ReadbackError`] if the texture is not the kind or format this
    /// understands, or if any Direct3D call failed. A test treats every one of
    /// them as a failure rather than as a frame to skip: a capture that cannot
    /// be read is not evidence of anything.
    pub(crate) fn read(&mut self, texture: &FrameTexture<'_>) -> Result<Image, ReadbackError> {
        if texture.kind() != TextureKind::D3d11Texture2D {
            return Err(ReadbackError::UnsupportedTexture {
                kind: texture.kind(),
            });
        }
        let raw = texture.as_raw();
        if raw.is_null() {
            return Err(ReadbackError::NullTexture);
        }

        // SAFETY: the backend promises, in `FrameTexture::new`'s safety
        // contract, that this is a live `ID3D11Texture2D` it owns for at least
        // as long as the frame this was taken from — which the caller still
        // holds. `from_raw_borrowed` takes no reference count and produces a
        // reference that cannot outlive `raw`, which is a local of this
        // function.
        let source = unsafe { ID3D11Texture2D::from_raw_borrowed(&raw) }
            .ok_or(ReadbackError::NullTexture)?;

        let mut description = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: `source` is a live texture and `description` is a live local
        // the call writes into.
        unsafe { source.GetDesc(&raw mut description) };

        if description.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            return Err(ReadbackError::UnsupportedFormat {
                format: description.Format,
            });
        }

        let context = self.context_for(source)?.clone();
        let staging = self.staging_for(description.Width, description.Height)?;

        // SAFETY: both textures belong to the same device and have the same
        // size, format, mip count and sample count, which is what
        // `CopyResource` requires. The staging texture is not mapped: every map
        // in this file is unmapped before it returns.
        unsafe { context.CopyResource(staging, source) };

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: `staging` was created with `D3D11_USAGE_STAGING` and
        // `CPU_ACCESS_READ`, which is what mapping subresource zero for reading
        // requires, and `mapped` is a live local the call writes into.
        unsafe { context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&raw mut mapped)) }.map_err(
            |source| ReadbackError::Direct3D {
                operation: "ID3D11DeviceContext::Map",
                source,
            },
        )?;

        let stride = mapped.RowPitch as usize;
        // Checked rather than assumed, as `render.rs::write_pattern` does with
        // the same arithmetic: the product is the length of a slice built from
        // a raw pointer, and a wrapped one would be a read past the end of the
        // mapping rather than a wrong number.
        let length = stride.checked_mul(description.Height as usize);

        let pixels = length.map(|length| {
            // SAFETY: `Map` returned a pointer to the whole of subresource
            // zero, whose rows are `RowPitch` bytes apart and of which there
            // are `Height`. The slice is read and copied out before `Unmap`
            // below, and nothing else refers to it.
            unsafe { core::slice::from_raw_parts(mapped.pData.cast::<u8>(), length) }.to_vec()
        });

        // Unmapped before the length is inspected: a staging texture left
        // mapped cannot be copied into, so the next frame would fail for a
        // reason that has nothing to do with why this one did.
        // SAFETY: `staging` is the texture mapped immediately above, and the
        // slice built from the mapping has been copied and dropped.
        unsafe { context.Unmap(staging, 0) };

        let pixels = pixels.ok_or(ReadbackError::MappingTooLarge {
            stride,
            height: description.Height,
        })?;

        Ok(Image {
            pixels,
            stride,
            width: description.Width,
            height: description.Height,
        })
    }

    /// The immediate context of the device `texture` belongs to, created once
    /// and reused.
    fn context_for(
        &mut self,
        texture: &ID3D11Texture2D,
    ) -> Result<&ID3D11DeviceContext, ReadbackError> {
        if self.context.is_none() {
            // SAFETY: `texture` is live; `GetDevice` returns an owned reference
            // to the device that created it, which windows-rs releases on drop.
            let device =
                unsafe { texture.GetDevice() }.map_err(|source| ReadbackError::Direct3D {
                    operation: "ID3D11DeviceChild::GetDevice",
                    source,
                })?;

            // SAFETY: `device` is live; `GetImmediateContext` returns an owned
            // reference to its one immediate context.
            let context = unsafe { device.GetImmediateContext() }.map_err(|source| {
                ReadbackError::Direct3D {
                    operation: "ID3D11Device::GetImmediateContext",
                    source,
                }
            })?;

            self.context = Some(context);
            self.device = Some(device);
        }
        self.context.as_ref().ok_or(ReadbackError::NoDevice)
    }

    /// A CPU-readable texture of this size, created once per size.
    fn staging_for(&mut self, width: u32, height: u32) -> Result<&ID3D11Texture2D, ReadbackError> {
        if !matches!(&self.staging, Some((_, held_width, held_height))
            if *held_width == width && *held_height == height)
        {
            let device = self.device.as_ref().ok_or(ReadbackError::NoDevice)?;
            let description = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                // A plain `as` cast rather than `unsigned_abs`: this is a bit
                // pattern in a signed newtype, not a number, and the absolute
                // value of a flag whose sign bit is set is a different flag
                // altogether.
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };

            let mut texture = None;
            // SAFETY: `description` is a live local read during the call, no
            // initial data is supplied, and `texture` is a live local the call
            // writes an owned interface into.
            unsafe { device.CreateTexture2D(&description, None, Some(&mut texture)) }.map_err(
                |source| ReadbackError::Direct3D {
                    operation: "ID3D11Device::CreateTexture2D",
                    source,
                },
            )?;

            // Success without a texture is a broken runtime rather than a
            // texture that would not name its device, and saying so is the
            // difference between a puzzle and a bug report (AGENTS.md section
            // 15).
            let texture = texture.ok_or_else(|| ReadbackError::Direct3D {
                operation: "ID3D11Device::CreateTexture2D",
                source: windows::core::Error::new(
                    windows::Win32::Foundation::E_FAIL,
                    "reported success without returning a texture",
                ),
            })?;

            self.staging = Some((texture, width, height));
        }

        self.staging
            .as_ref()
            .map(|(texture, _, _)| texture)
            .ok_or(ReadbackError::NoDevice)
    }
}

/// Why a captured frame could not be read.
#[derive(Debug)]
pub(crate) enum ReadbackError {
    /// The frame is not in a Direct3D 11 texture.
    UnsupportedTexture {
        /// What it is in instead.
        kind: TextureKind,
    },
    /// The frame is not BGRA8, which is what the test pattern is drawn in.
    UnsupportedFormat {
        /// The format the capture backend produced.
        format: DXGI_FORMAT,
    },
    /// The backend handed over a null texture, which it promises not to do.
    NullTexture,
    /// The texture would not name the device that owns it, so there is nowhere
    /// to create a staging texture.
    NoDevice,
    /// The mapped texture describes more bytes than an address can hold.
    MappingTooLarge {
        /// Bytes between one row and the next, as Direct3D reported them.
        stride: usize,
        /// Rows in the mapped texture.
        height: u32,
    },
    /// A Direct3D call failed.
    Direct3D {
        /// Which call.
        operation: &'static str,
        /// What Direct3D said.
        source: windows::core::Error,
    },
}

impl fmt::Display for ReadbackError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedTexture { kind } => write!(
                formatter,
                "this test can only read an ID3D11Texture2D and the frame arrived in a {kind}"
            ),
            Self::UnsupportedFormat { format } => write!(
                formatter,
                "the test pattern is drawn in BGRA8 and the capture produced format {}; \
                 an HDR display can do this, and the pattern would have to grow a second \
                 encoding before this test could read one",
                format.0
            ),
            Self::NullTexture => {
                formatter.write_str("the capture backend handed over a null texture")
            }
            Self::NoDevice => {
                formatter.write_str("the captured texture would not name the device that owns it")
            }
            Self::MappingTooLarge { stride, height } => write!(
                formatter,
                "the mapped texture says its {height} rows are {stride} bytes apart, which is \
                 more bytes than this process can address"
            ),
            Self::Direct3D { operation, source } => {
                write!(formatter, "{operation} failed: {source}")
            }
        }
    }
}

impl std::error::Error for ReadbackError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Direct3D { source, .. } => Some(source),
            Self::UnsupportedTexture { .. }
            | Self::UnsupportedFormat { .. }
            | Self::NullTexture
            | Self::NoDevice
            | Self::MappingTooLarge { .. } => None,
        }
    }
}

impl fmt::Debug for Image {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Image")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("stride", &self.stride)
            .field("bytes", &self.pixels.len())
            .finish()
    }
}
