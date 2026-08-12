//! One captured frame, copied out of the GPU so that something can write it to
//! a file.
//!
//! Everything else in this crate keeps a frame on the GPU from the compositor
//! to the encoder, because a readback per frame would undo the one performance
//! property the whole pipeline is built around (`docs/capture-pipeline.md`).
//! A screenshot is the one thing that genuinely has to break that rule: a PNG
//! is bytes in system memory, and there is no route from a Direct3D texture to
//! a file that does not pass through them.
//!
//! What this module is, therefore, is the *narrowest possible* exception —
//! **one** frame, on request, never per frame — and the type that carries the
//! result away from the capture thread.
//!
//! # Responsibilities
//!
//! - [`StillFrame`]: a frame's pixels in system memory, owned, [`Send`], and
//!   valid for as long as its holder wants it. That is the whole point: a
//!   [`CapturedFrame`](crate::CapturedFrame) may not outlive the acquisition
//!   that produced it or leave the capture thread, and a screenshot has to do
//!   both.
//! - [`StillError`]: why a frame could not be copied.
//!
//! # Not responsible for
//!
//! Encoding, naming or writing anything. This crate does not know what a PNG
//! is and does not touch the filesystem; `clipped-session`'s `screenshot`
//! module is what turns a [`StillFrame`] into a file
//! ([issue #67](https://github.com/wildware-uk/clipped/issues/67),
//! `docs/screenshots.md`).
//!
//! # The copy itself
//!
//! [`windows::D3d11StillCopier`](crate::windows::D3d11StillCopier) is the
//! implementation, and it is deliberately two-phase — the GPU copy on the frame
//! the user asked for, the map and the memory copy on a later one — so that
//! taking a screenshot during a recording does not stall the capture thread
//! waiting for the GPU. `docs/capture-pipeline.md` has the measurement.

use core::fmt;

use crate::{CaptureTimestamp, FrameFormat, FrameSize, PixelFormat};

/// One frame's pixels, in system memory, owned by whoever holds this.
///
/// # Why this exists rather than a `Vec<u8>` and three integers
///
/// Because the three integers are the part that gets lost. A buffer read back
/// from a Direct3D staging texture is **not** tightly packed: its rows are
/// `stride` bytes apart, and `stride` is whatever the driver chose, routinely
/// larger than `width * 4`. Code that assumes otherwise produces an image that
/// is skewed diagonally — recognisable, obviously wrong, and produced silently
/// — so the stride travels with the pixels and [`row`](Self::row) is the only
/// way to reach a line.
///
/// # Ownership and threading
///
/// It owns its buffer and borrows nothing, so it is [`Send`] and outlives the
/// backend, the frame and the capture thread. That is exactly what a screenshot
/// needs: the pixels leave the capture thread and are encoded and written
/// somewhere that is allowed to touch a disk (AGENTS.md section 20).
#[derive(Clone, PartialEq, Eq)]
pub struct StillFrame {
    pixels: Vec<u8>,
    stride: usize,
    format: FrameFormat,
    timestamp: CaptureTimestamp,
}

impl StillFrame {
    /// Wraps pixels that have already been copied out of a frame.
    ///
    /// `pixels` holds `format.size().height()` rows `stride` bytes apart, in
    /// `format.pixel_format()`. Trailing bytes past the last row are allowed
    /// and ignored, because a mapped texture is entitled to have them.
    ///
    /// Public so that a caller can build one without a GPU. Everything
    /// downstream of a screenshot — naming, encoding, writing, the failure when
    /// the disk is full — is testable on any machine because of it (AGENTS.md
    /// section 26).
    ///
    /// # Errors
    ///
    /// [`StillError::MalformedBuffer`] if `stride` is narrower than one row of
    /// `format`, or if `pixels` is shorter than the rows it claims to hold.
    /// Both are caught here rather than at the point a row is read, because a
    /// buffer that is one row short is a bug in the copy and the useful place
    /// to say so is where the numbers still exist.
    pub fn new(
        pixels: Vec<u8>,
        stride: usize,
        format: FrameFormat,
        timestamp: CaptureTimestamp,
    ) -> Result<Self, StillError> {
        let size = format.size();
        let minimum_stride = row_bytes(format);
        let needed =
            stride
                .checked_mul(size.height() as usize)
                .ok_or(StillError::MalformedBuffer {
                    stride,
                    size,
                    bytes: pixels.len(),
                })?;

        if stride < minimum_stride || pixels.len() < needed {
            return Err(StillError::MalformedBuffer {
                stride,
                size,
                bytes: pixels.len(),
            });
        }

        Ok(Self {
            pixels,
            stride,
            format,
            timestamp,
        })
    }

    /// The frame's size and pixel layout.
    #[must_use]
    pub const fn format(&self) -> FrameFormat {
        self.format
    }

    /// The frame's dimensions.
    #[must_use]
    pub const fn size(&self) -> FrameSize {
        self.format.size()
    }

    /// Bytes between the start of one row and the start of the next.
    ///
    /// Never smaller than [`row_bytes`] of the format, and usually larger.
    #[must_use]
    pub const fn stride(&self) -> usize {
        self.stride
    }

    /// When the source produced the frame, on the source's own clock.
    ///
    /// Carried through unchanged so that a screenshot taken during a recording
    /// can be placed on that recording's timeline by the same conversion every
    /// other timestamp goes through ([`CaptureClock`](crate::CaptureClock)),
    /// rather than by reading a wall clock at the moment the file was written.
    #[must_use]
    pub const fn timestamp(&self) -> CaptureTimestamp {
        self.timestamp
    }

    /// Row `y`, exactly [`row_bytes`] long, or [`None`] past the last row.
    #[must_use]
    pub fn row(&self, y: u32) -> Option<&[u8]> {
        if y >= self.format.size().height() {
            return None;
        }
        let start = self.stride.checked_mul(y as usize)?;
        let end = start.checked_add(row_bytes(self.format))?;
        self.pixels.get(start..end)
    }

    /// Every row, top to bottom.
    pub fn rows(&self) -> impl ExactSizeIterator<Item = &[u8]> + '_ {
        (0..self.format.size().height()).map(|y| {
            self.row(y)
                .expect("`new` checked that every declared row is present")
        })
    }

    /// The whole buffer, rows included, exactly as it was copied.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.pixels
    }

    /// How many bytes the pixels occupy.
    #[must_use]
    pub fn byte_count(&self) -> usize {
        self.pixels.len()
    }
}

impl fmt::Debug for StillFrame {
    /// Everything except the pixels.
    ///
    /// A `{:?}` of several megabytes of image in a log line is not a
    /// diagnostic; it is a way to make a log file unreadable, and it is
    /// somebody's screen contents in a support bundle (AGENTS.md section 13).
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StillFrame")
            .field("format", &self.format)
            .field("stride", &self.stride)
            .field("bytes", &self.pixels.len())
            .field("timestamp", &self.timestamp)
            .finish()
    }
}

/// How many bytes one row of `format` occupies, ignoring any padding after it.
#[must_use]
pub const fn row_bytes(format: FrameFormat) -> usize {
    format.size().width() as usize * bytes_per_pixel(format.pixel_format())
}

/// How many bytes one pixel of `format` occupies.
///
/// Every format this crate names is a fixed number of bytes per pixel, which is
/// what makes the row arithmetic above exact rather than an estimate. A planar
/// or subsampled format would not fit this shape, and adding one would have to
/// change [`StillFrame`] rather than this function.
#[must_use]
pub const fn bytes_per_pixel(format: PixelFormat) -> usize {
    match format {
        PixelFormat::Bgra8Unorm | PixelFormat::Rgb10A2Unorm => 4,
        PixelFormat::Rgba16Float => 8,
    }
}

/// Why a frame could not be copied out of the GPU.
#[derive(Debug)]
#[non_exhaustive]
pub enum StillError {
    /// The frame is not in a graphics resource this copier understands.
    UnsupportedTexture {
        /// What it is in.
        kind: crate::TextureKind,
    },
    /// The frame's pixels are in a layout this copier cannot read.
    ///
    /// Today that is anything but [`PixelFormat::Bgra8Unorm`], which is what
    /// both Windows capture backends produce for ordinary content. An HDR
    /// capture is 10 or 16 bits a channel and reaches here
    /// ([issue #99](https://github.com/wildware-uk/clipped/issues/99)), which
    /// is the same boundary SPEC.md section 26 draws when it puts HDR-aware
    /// screenshots in a later milestone.
    UnsupportedFormat {
        /// The layout the capture produced.
        format: PixelFormat,
    },
    /// The backend handed over a null texture, which it promises not to do.
    NullTexture,
    /// The captured texture would not name the device that owns it, so there is
    /// nowhere to create the staging resource the copy needs.
    NoDevice,
    /// A copy was asked to finish and none had been started.
    NothingPending,
    /// The pixels do not describe the frame they claim to.
    MalformedBuffer {
        /// Bytes between one row and the next.
        stride: usize,
        /// The frame the buffer claims to hold.
        size: FrameSize,
        /// How many bytes there actually are.
        bytes: usize,
    },
    /// A graphics call failed.
    Graphics {
        /// Which call.
        operation: &'static str,
        /// What it said, already rendered: this variant is constructed on
        /// Windows from a `windows::core::Error` and the crate's
        /// platform-neutral half must still be able to name it.
        detail: String,
    },
}

impl fmt::Display for StillError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedTexture { kind } => write!(
                formatter,
                "a screenshot can only be copied out of an ID3D11Texture2D and this frame is in \
                 a {kind}"
            ),
            Self::UnsupportedFormat { format } => write!(
                formatter,
                "this build can only copy {} frames and the capture produced {format}; \
                 HDR screenshots are issue #99",
                PixelFormat::Bgra8Unorm
            ),
            Self::NullTexture => {
                formatter.write_str("the capture backend handed over a null texture")
            }
            Self::NoDevice => {
                formatter.write_str("the captured texture would not name the device that owns it")
            }
            Self::NothingPending => {
                formatter.write_str("no frame copy had been started, so there is none to finish")
            }
            Self::MalformedBuffer {
                stride,
                size,
                bytes,
            } => write!(
                formatter,
                "a {size} image with rows {stride} bytes apart needs more than the {bytes} bytes \
                 that were copied"
            ),
            Self::Graphics { operation, detail } => {
                write!(formatter, "{operation} failed: {detail}")
            }
        }
    }
}

impl std::error::Error for StillError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SourceClock;

    fn format(width: u32, height: u32) -> FrameFormat {
        FrameFormat::new(
            FrameSize::new(width, height).expect("a test size is not zero"),
            PixelFormat::Bgra8Unorm,
        )
    }

    fn timestamp() -> CaptureTimestamp {
        CaptureTimestamp::from_source(SourceClock::PerformanceCounter, 1_234)
    }

    #[test]
    fn a_padded_buffer_reads_back_one_row_at_a_time() {
        // The failure this holds is the one that produces a diagonally skewed
        // screenshot: a driver hands over 8 bytes of padding after each row,
        // and code that treats the buffer as tightly packed reads the padding
        // as the start of the next line.
        let format = format(2, 3);
        let stride = 16;
        let mut pixels = vec![0_u8; stride * 3];
        for (y, row) in pixels.chunks_mut(stride).enumerate() {
            row[..8].fill(u8::try_from(y).expect("three rows fit in a byte"));
        }

        let still = StillFrame::new(pixels, stride, format, timestamp())
            .expect("the buffer holds every row it claims to");

        assert_eq!(still.row(0), Some([0_u8; 8].as_slice()));
        assert_eq!(still.row(1), Some([1_u8; 8].as_slice()));
        assert_eq!(still.row(2), Some([2_u8; 8].as_slice()));
        assert_eq!(still.row(3), None, "there is no fourth row of a 2x3 image");
        assert_eq!(still.rows().len(), 3);
    }

    #[test]
    fn a_buffer_that_is_short_of_a_row_is_refused_rather_than_read() {
        // One row short. Left unchecked this is a read past the end of the
        // buffer on the last line of every screenshot.
        let format = format(4, 4);
        let stride = 16;
        let pixels = vec![0_u8; stride * 3];

        let error = StillFrame::new(pixels, stride, format, timestamp())
            .expect_err("three rows cannot hold a four-row image");
        assert!(
            matches!(error, StillError::MalformedBuffer { .. }),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn a_stride_narrower_than_a_row_is_refused() {
        let format = format(4, 2);
        let error = StillFrame::new(vec![0_u8; 64], 8, format, timestamp())
            .expect_err("a four-pixel row cannot fit in eight bytes");
        assert!(
            matches!(error, StillError::MalformedBuffer { stride: 8, .. }),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn trailing_bytes_past_the_last_row_are_allowed() {
        // A mapped staging texture is entitled to be larger than the rows in
        // it, and refusing that would refuse every real screenshot.
        let format = format(2, 2);
        let still = StillFrame::new(vec![7_u8; 1_024], 32, format, timestamp())
            .expect("a buffer larger than the image is still a valid image");
        assert_eq!(still.row(1), Some([7_u8; 8].as_slice()));
        assert_eq!(still.byte_count(), 1_024);
    }

    #[test]
    fn the_debug_form_carries_no_pixels() {
        // A screenshot is somebody's screen. `{:?}` of one in a log line is a
        // privacy failure as well as an unreadable log (AGENTS.md section 13).
        let still = StillFrame::new(vec![0xAB; 1_024], 32, format(2, 2), timestamp())
            .expect("a valid image");
        let rendered = format!("{still:?}");
        assert!(rendered.contains("bytes: 1024"), "{rendered}");
        assert!(
            !rendered.contains("171"),
            "the pixels are in it: {rendered}"
        );
        assert!(!rendered.contains("ab"), "the pixels are in it: {rendered}");
    }

    #[test]
    fn a_row_is_the_pixels_and_never_the_padding() {
        let format = format(3, 1);
        let still =
            StillFrame::new(vec![0_u8; 64], 64, format, timestamp()).expect("a valid image");
        assert_eq!(
            still.row(0).map(<[u8]>::len),
            Some(12),
            "three BGRA8 pixels are twelve bytes, whatever the stride is"
        );
        assert_eq!(row_bytes(format), 12);
    }

    #[test]
    fn every_pixel_format_has_a_size_and_the_hdr_ones_are_wider() {
        // The arithmetic in `row_bytes` is only exact because each of these is
        // a fixed width. A new variant has to answer here before it compiles.
        assert_eq!(bytes_per_pixel(PixelFormat::Bgra8Unorm), 4);
        assert_eq!(bytes_per_pixel(PixelFormat::Rgb10A2Unorm), 4);
        assert_eq!(bytes_per_pixel(PixelFormat::Rgba16Float), 8);
    }
}
