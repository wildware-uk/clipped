//! Frames: their shape, the texture they live in, and who owns it.

use core::ffi::c_void;
use core::fmt;
use core::marker::PhantomData;
use core::num::NonZeroU32;

use crate::CaptureTimestamp;

/// A frame's dimensions in pixels.
///
/// Neither dimension can be zero. That is not pedantry: a minimised window
/// reports a zero-sized client area on Windows, and a backend that passes it on
/// produces an encoder session that fails on its first frame. A backend with
/// nothing to report has [`Acquisition::Timeout`](crate::Acquisition::Timeout)
/// to say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameSize {
    width: NonZeroU32,
    height: NonZeroU32,
}

impl FrameSize {
    /// Builds a size, or [`None`] if either dimension is zero.
    #[must_use]
    pub const fn new(width: u32, height: u32) -> Option<Self> {
        match (NonZeroU32::new(width), NonZeroU32::new(height)) {
            (Some(width), Some(height)) => Some(Self { width, height }),
            _ => None,
        }
    }

    /// The width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width.get()
    }

    /// The height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height.get()
    }
}

impl fmt::Display for FrameSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}", self.width, self.height)
    }
}

/// How a frame's pixels are laid out.
///
/// Only formats a capture API actually hands over appear here. Conversion is
/// the encoder's problem, not this crate's: a capture backend passes on what it
/// was given and says what it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PixelFormat {
    /// 8 bits per channel, blue-green-red-alpha order, unsigned normalised.
    /// What both Windows capture APIs produce for ordinary SDR content.
    Bgra8Unorm,
    /// 10 bits per colour channel and 2 for alpha, unsigned normalised. The
    /// usual format for HDR content
    /// ([issue #99](https://github.com/wildware-uk/clipped/issues/99)).
    Rgb10A2Unorm,
    /// 16-bit floating point per channel, the other HDR representation
    /// ([issue #99](https://github.com/wildware-uk/clipped/issues/99)).
    Rgba16Float,
}

impl fmt::Display for PixelFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Bgra8Unorm => "BGRA8 unorm",
            Self::Rgb10A2Unorm => "RGB10A2 unorm",
            Self::Rgba16Float => "RGBA16 float",
        })
    }
}

/// The shape of every frame a backend is currently producing.
///
/// Returned by [`CaptureBackend::initialise`](crate::CaptureBackend::initialise)
/// and [`CaptureBackend::resize`](crate::CaptureBackend::resize) so that the
/// encoder can be configured before the first frame arrives, and reconfigured
/// when the target changes shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameFormat {
    size: FrameSize,
    pixel_format: PixelFormat,
}

impl FrameFormat {
    /// Describes frames of `size` in `pixel_format`.
    #[must_use]
    pub const fn new(size: FrameSize, pixel_format: PixelFormat) -> Self {
        Self { size, pixel_format }
    }

    /// The frame dimensions.
    #[must_use]
    pub const fn size(&self) -> FrameSize {
        self.size
    }

    /// The pixel layout.
    #[must_use]
    pub const fn pixel_format(&self) -> PixelFormat {
        self.pixel_format
    }
}

impl fmt::Display for FrameFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.size, self.pixel_format)
    }
}

/// What sort of graphics resource a frame's pixels are in.
///
/// The interface is platform-neutral but a texture is not: the encoder has to
/// know what it has been handed before it can bind it. Naming the kinds in one
/// closed enumeration keeps the platform knowledge in one visible place instead
/// of spread through casts (AGENTS.md section 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TextureKind {
    /// A Direct3D 11 `ID3D11Texture2D`, on the adapter the backend was
    /// initialised against.
    D3d11Texture2D,
}

impl fmt::Display for TextureKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::D3d11Texture2D => "ID3D11Texture2D",
        })
    }
}

/// A borrowed graphics resource holding one frame's pixels.
///
/// # Ownership
///
/// **The backend owns this texture. The caller never does.** The lifetime
/// parameter is a borrow of the backend that produced it, so the compiler
/// refuses to let it outlive the [`CapturedFrame`] it came in, and refuses to
/// let that frame survive the next call to
/// [`acquire`](crate::CaptureBackend::acquire). Within that window the backend
/// guarantees the pixels do not change and the resource is not recycled; after
/// it, both are likely.
///
/// The consequence for a consumer is a rule with no exceptions: **copy the
/// handle nowhere.** Submit it to the encoder while you hold the frame, or copy
/// the pixels into a resource you own. Storing the raw pointer in a queue for
/// another thread to encode later is the single most attractive mistake in this
/// pipeline, and it produces corrupted frames rather than a crash, days apart
/// from the code that caused it.
pub struct FrameTexture<'frame> {
    kind: TextureKind,
    handle: *mut c_void,
    borrow: PhantomData<&'frame ()>,
}

impl<'frame> FrameTexture<'frame> {
    /// Wraps a texture the backend owns for at least the frame's lifetime.
    ///
    /// # Safety
    ///
    /// If anything dereferences `handle` — which it can only reach through
    /// [`as_raw`](Self::as_raw) — then `handle` must be a live resource of
    /// `kind`, owned by the caller of this function, and it must stay valid and
    /// unrecycled until this `FrameTexture` is dropped.
    ///
    /// Constructing one is the point at which a backend makes that promise, so
    /// a backend author has to write a `SAFETY` comment saying why it holds
    /// (AGENTS.md section 58).
    #[must_use]
    pub const unsafe fn new(kind: TextureKind, handle: *mut c_void) -> Self {
        Self {
            kind,
            handle,
            borrow: PhantomData,
        }
    }

    /// What kind of resource [`as_raw`](Self::as_raw) returns.
    #[must_use]
    pub const fn kind(&self) -> TextureKind {
        self.kind
    }

    /// The underlying resource, for the encoder to bind.
    ///
    /// Valid only while this `FrameTexture` is alive, and only as a
    /// [`kind`](Self::kind). Dereferencing it is `unsafe` and belongs in the
    /// platform module that understands the type.
    #[must_use]
    pub const fn as_raw(&self) -> *mut c_void {
        self.handle
    }
}

impl fmt::Debug for FrameTexture<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameTexture")
            .field("kind", &self.kind)
            .field("handle", &self.handle)
            .finish()
    }
}

/// One frame, borrowed from the backend that captured it.
///
/// # Lifetime
///
/// A `CapturedFrame` borrows the backend mutably, so exactly one can exist at a
/// time and it must be dropped before the next
/// [`acquire`](crate::CaptureBackend::acquire). This is the whole of the
/// ownership contract, and it is checked at compile time rather than
/// documented and hoped for. It also happens to be what the underlying APIs
/// demand: Desktop Duplication permits one outstanding frame per duplication
/// and `AcquireNextFrame` fails until `ReleaseFrame` has been called, and
/// Windows Graphics Capture recycles frames back into its pool.
///
/// `CapturedFrame` is deliberately not [`Send`]: it holds a raw texture handle,
/// which makes it thread-bound automatically, and that matches the pipeline —
/// frames are encoded on the capture thread and only encoded packets cross to
/// another one. See `docs/capture-pipeline.md`.
#[derive(Debug)]
pub struct CapturedFrame<'frame> {
    texture: FrameTexture<'frame>,
    format: FrameFormat,
    timestamp: CaptureTimestamp,
    frames_missed: Option<u32>,
}

impl<'frame> CapturedFrame<'frame> {
    /// Assembles a frame around a texture the backend owns.
    ///
    /// `timestamp` must be the one the frame arrived with; see
    /// [`CaptureTimestamp`] for why reading a clock here is a bug rather than
    /// an approximation.
    #[must_use]
    pub const fn new(
        texture: FrameTexture<'frame>,
        format: FrameFormat,
        timestamp: CaptureTimestamp,
    ) -> Self {
        Self {
            texture,
            format,
            timestamp,
            frames_missed: None,
        }
    }

    /// Records how many frames the source produced since the last acquisition
    /// that this one did not deliver.
    ///
    /// This is the difference between "the game rendered 40 frames" and "we
    /// recorded 40 frames", and without it a dropped-frame count is guesswork.
    ///
    /// Not every API says. Desktop Duplication reports it outright, in
    /// `DXGI_OUTDUPL_FRAME_INFO::AccumulatedFrames`; Windows Graphics Capture
    /// has no such field and no event for the frames it skips, so its backend
    /// derives the figure from the source's timestamps as a lower bound (see
    /// `docs/capture-pipeline.md`, "Dropped frames"). Either is a legitimate
    /// answer to this method. Leaving it unset says the backend does not know
    /// rather than that nothing was dropped, which is why the accessor returns
    /// an [`Option`] and not a zero.
    #[must_use]
    pub const fn with_frames_missed(mut self, frames_missed: u32) -> Self {
        self.frames_missed = Some(frames_missed);
        self
    }

    /// The pixels, owned by the backend and valid until this frame is dropped.
    #[must_use]
    pub const fn texture(&self) -> &FrameTexture<'frame> {
        &self.texture
    }

    /// The frame's size and pixel layout.
    #[must_use]
    pub const fn format(&self) -> FrameFormat {
        self.format
    }

    /// When the source produced the frame, on the source's own clock.
    #[must_use]
    pub const fn timestamp(&self) -> CaptureTimestamp {
        self.timestamp
    }

    /// Frames the source produced but did not hand over since the previous
    /// acquisition, when it reports that at all.
    #[must_use]
    pub const fn frames_missed(&self) -> Option<u32> {
        self.frames_missed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SourceClock;

    #[test]
    fn a_zero_dimension_is_not_a_frame_size() {
        assert!(FrameSize::new(0, 1080).is_none());
        assert!(FrameSize::new(1920, 0).is_none());
        assert!(FrameSize::new(0, 0).is_none());
        assert!(FrameSize::new(1920, 1080).is_some());
    }

    #[test]
    fn sizes_and_formats_print_the_way_diagnostics_expect() {
        let size = FrameSize::new(2560, 1440).expect("2560x1440 is a valid size");
        assert_eq!(size.to_string(), "2560x1440");
        assert_eq!(
            FrameFormat::new(size, PixelFormat::Bgra8Unorm).to_string(),
            "2560x1440 BGRA8 unorm"
        );
    }

    #[test]
    fn a_frame_carries_the_texture_timestamp_and_drop_count_it_was_given() {
        let size = FrameSize::new(1920, 1080).expect("1920x1080 is a valid size");
        let format = FrameFormat::new(size, PixelFormat::Bgra8Unorm);
        let timestamp = CaptureTimestamp::from_source(SourceClock::PerformanceCounter, 42_000);

        // SAFETY: this test never calls `as_raw`, let alone dereferences the
        // handle, so the validity requirement is vacuous. It exercises the
        // accessors only; there is no Direct3D device in a unit test.
        let texture =
            unsafe { FrameTexture::new(TextureKind::D3d11Texture2D, core::ptr::null_mut()) };

        let frame = CapturedFrame::new(texture, format, timestamp).with_frames_missed(3);

        assert_eq!(frame.format(), format);
        assert_eq!(frame.timestamp(), timestamp);
        assert_eq!(frame.frames_missed(), Some(3));
        assert_eq!(frame.texture().kind(), TextureKind::D3d11Texture2D);
    }
}
