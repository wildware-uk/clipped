//! Frames going in: the graphics device, the texture, and who owns them.
//!
//! The whole point of a hardware encoder is that the picture never leaves the
//! GPU. A capture backend hands over a texture the compositor already owns, the
//! encoder binds that texture, and no frame is ever copied through system
//! memory (AGENTS.md section 18). A round trip through the CPU at 2560x1440 and
//! 60 frames a second is roughly 850 MB/s of pure waste, and it is invisible:
//! the recording still works, it just costs the game its frame rate.
//!
//! So the types here carry handles rather than pixels, which makes ownership
//! the thing they have to get right.
//!
//! # Ownership
//!
//! **Nothing here owns anything.** A [`GraphicsDevice`] and a [`SourceTexture`]
//! are borrowed handles: the caller owns the device and the capture backend
//! owns the texture, exactly as `clipped-capture` documents. The lifetime on
//! [`SourceFrame`] ties the borrow to the frame it came from, so a frame cannot
//! outlive the capture that produced it, and both constructors are `unsafe`
//! because the compiler cannot check the claim being made — that the handle is
//! live, is what it says it is, and stays valid for the borrow.

use core::ffi::c_void;
use core::fmt;
use core::marker::PhantomData;
use core::time::Duration;

use crate::codec::Resolution;
use crate::config::SurfaceFormat;

/// What sort of graphics API a device belongs to.
///
/// The interface is platform-neutral; a device handle is not. Naming the kinds
/// in one closed enumeration keeps the platform knowledge visible instead of
/// spread through casts (AGENTS.md section 5), and mirrors
/// `clipped_capture::TextureKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DeviceKind {
    /// A Direct3D 11 `ID3D11Device`.
    D3d11,
}

impl fmt::Display for DeviceKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::D3d11 => "ID3D11Device",
        })
    }
}

/// The graphics device the frames to be encoded live on.
///
/// An encoder session is opened against one device and can only bind textures
/// belonging to it. That is not an implementation detail to be worked around:
/// a texture from another device would have to be copied across adapters, which
/// is the cost this whole interface exists to avoid. The caller therefore
/// passes the device its capture backend created, and the encoder refuses
/// anything else by failing to bind it.
///
/// # Ownership
///
/// Borrowed for the duration of the call that opens a session. A backend that
/// needs the device afterwards takes its own reference — on Windows, by
/// incrementing the COM reference count — so that the session cannot outlive
/// the device it was built from.
#[derive(Debug, Clone, Copy)]
pub struct GraphicsDevice {
    kind: DeviceKind,
    handle: *mut c_void,
}

impl GraphicsDevice {
    /// Names a device the caller owns.
    ///
    /// # Safety
    ///
    /// `handle` must be a live device of `kind`, owned by the caller, and it
    /// must stay valid for as long as any encoder opened against it is alive.
    #[must_use]
    pub const unsafe fn new(kind: DeviceKind, handle: *mut c_void) -> Self {
        Self { kind, handle }
    }

    /// Which graphics API [`as_raw`](Self::as_raw) returns a device for.
    #[must_use]
    pub const fn kind(&self) -> DeviceKind {
        self.kind
    }

    /// The underlying device, for a backend to open a session against.
    #[must_use]
    pub const fn as_raw(&self) -> *mut c_void {
        self.handle
    }
}

/// What sort of graphics resource a frame's pixels are in.
///
/// The counterpart of `clipped_capture::TextureKind`, and deliberately the same
/// shape: `clipped-session` converts one into the other, and a conversion
/// between two identical enumerations is easier to keep correct than a
/// dependency that the layering rules forbid (README, "Dependency direction").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SurfaceKind {
    /// A Direct3D 11 `ID3D11Texture2D`, on the device the encoder was opened
    /// against.
    D3d11Texture2D,
}

impl fmt::Display for SurfaceKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::D3d11Texture2D => "ID3D11Texture2D",
        })
    }
}

/// A borrowed graphics resource holding one frame's pixels.
///
/// # Ownership
///
/// **The capture backend owns this texture. The encoder never does.** The
/// lifetime is a borrow of whatever produced it, so the compiler refuses to let
/// it outlive the [`SourceFrame`] it arrived in. Within that window the encoder
/// may register and read it; afterwards it may not, and an encoder that kept
/// the raw handle would read a recycled frame pool slot and produce corruption
/// days away from the code that caused it.
///
/// The consequence for a backend author is a rule: everything an encoder does
/// with this handle happens inside
/// [`VideoEncoder::submit`](crate::VideoEncoder::submit). Nothing derived from
/// it may be stored.
pub struct SourceTexture<'frame> {
    kind: SurfaceKind,
    handle: *mut c_void,
    borrow: PhantomData<&'frame ()>,
}

impl SourceTexture<'_> {
    /// Wraps a texture its owner keeps alive for at least the frame's lifetime.
    ///
    /// # Safety
    ///
    /// `handle` must be a live resource of `kind`, owned by the caller, on the
    /// same device the encoder was opened against, and it must stay valid and
    /// unrecycled until this `SourceTexture` is dropped.
    #[must_use]
    pub const unsafe fn new(kind: SurfaceKind, handle: *mut c_void) -> Self {
        Self {
            kind,
            handle,
            borrow: PhantomData,
        }
    }

    /// What kind of resource [`as_raw`](Self::as_raw) returns.
    #[must_use]
    pub const fn kind(&self) -> SurfaceKind {
        self.kind
    }

    /// The underlying resource, for the encoder to bind.
    #[must_use]
    pub const fn as_raw(&self) -> *mut c_void {
        self.handle
    }
}

impl fmt::Debug for SourceTexture<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceTexture")
            .field("kind", &self.kind)
            .field("handle", &self.handle)
            .finish()
    }
}

/// One frame on its way into an encoder.
///
/// # Timestamps
///
/// [`presentation_time`](Self::presentation_time) is the frame's position in
/// the recording, measured from whatever the session chose as time zero. It
/// comes from the capture timestamp, converted by the caller; an encoder must
/// never read a clock to fill it in, because the interval between capture and
/// encode is exactly the jitter that would land in the file (AGENTS.md section
/// 22).
///
/// It must not go backwards. An encoder rejects a frame that does, rather than
/// producing a file that stalls on playback.
#[derive(Debug)]
pub struct SourceFrame<'frame> {
    texture: SourceTexture<'frame>,
    format: SurfaceFormat,
    resolution: Resolution,
    presentation_time: Duration,
    force_keyframe: bool,
}

impl<'frame> SourceFrame<'frame> {
    /// Assembles a frame around a texture its owner keeps alive.
    ///
    /// `resolution` and `format` describe what is actually in the texture, and
    /// a backend passes them straight to the hardware: they are not checked
    /// against the resource, because reading a texture description per frame
    /// costs a driver call for something the capture backend already knows.
    #[must_use]
    pub const fn new(
        texture: SourceTexture<'frame>,
        format: SurfaceFormat,
        resolution: Resolution,
        presentation_time: Duration,
    ) -> Self {
        Self {
            texture,
            format,
            resolution,
            presentation_time,
            force_keyframe: false,
        }
    }

    /// Asks for this frame to be encoded as a keyframe whatever the configured
    /// interval says.
    ///
    /// What the replay buffer uses to make a clip start where the user asked
    /// (SPEC.md section 7), and what a session uses after a resolution change.
    #[must_use]
    pub const fn forcing_keyframe(mut self) -> Self {
        self.force_keyframe = true;
        self
    }

    /// The pixels, owned by the capture backend.
    #[must_use]
    pub const fn texture(&self) -> &SourceTexture<'frame> {
        &self.texture
    }

    /// How the pixels are laid out.
    #[must_use]
    pub const fn format(&self) -> SurfaceFormat {
        self.format
    }

    /// The size of the picture in the texture.
    #[must_use]
    pub const fn resolution(&self) -> Resolution {
        self.resolution
    }

    /// Where this frame sits in the recording.
    #[must_use]
    pub const fn presentation_time(&self) -> Duration {
        self.presentation_time
    }

    /// Whether this frame was asked to be a keyframe.
    #[must_use]
    pub const fn forces_keyframe(&self) -> bool {
        self.force_keyframe
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_carries_what_it_was_given() {
        // SAFETY: this test never calls `as_raw`, let alone dereferences the
        // handle, so the validity requirement is vacuous. There is no Direct3D
        // device in a unit test; the accessors are what is being exercised.
        let texture =
            unsafe { SourceTexture::new(SurfaceKind::D3d11Texture2D, core::ptr::null_mut()) };

        let frame = SourceFrame::new(
            texture,
            SurfaceFormat::Bgra8Unorm,
            Resolution::new(2560, 1440),
            Duration::from_millis(16),
        );

        assert_eq!(frame.format(), SurfaceFormat::Bgra8Unorm);
        assert_eq!(frame.resolution(), Resolution::new(2560, 1440));
        assert_eq!(frame.presentation_time(), Duration::from_millis(16));
        assert!(!frame.forces_keyframe());
        assert_eq!(frame.texture().kind(), SurfaceKind::D3d11Texture2D);
    }

    #[test]
    fn a_frame_can_be_asked_to_be_a_keyframe() {
        // SAFETY: as above — the handle is never read.
        let texture =
            unsafe { SourceTexture::new(SurfaceKind::D3d11Texture2D, core::ptr::null_mut()) };

        let frame = SourceFrame::new(
            texture,
            SurfaceFormat::Bgra8Unorm,
            Resolution::HD_1080P,
            Duration::ZERO,
        )
        .forcing_keyframe();

        assert!(frame.forces_keyframe());
    }
}
