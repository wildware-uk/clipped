//! Turning the BGRA bytes that came back from the GPU into the 4:2:0 planes an
//! encoder wants.
//!
//! A hardware encoder converts colour inside the encoder, as part of encoding
//! (`docs/encoder-pipeline.md`). A CPU encoder has to be given planes, so this
//! is the second cost the software path pays that the hardware path does not:
//! at 2560x1440 it reads 14 MB and writes 5.5 MB, per frame, on the CPU.
//!
//! It is `swscale`, from the same pinned FFmpeg build as the encoder, because
//! the alternative is writing a colour conversion by hand — several hundred
//! lines of arithmetic whose mistakes look like slightly wrong colour rather
//! than like a failure, against a library that has SIMD implementations of it
//! for every instruction set the machine might have.
//!
//! # The tag and the conversion have to agree
//!
//! The encoder writes a colour description into the stream saying how the luma
//! and chroma samples should be interpreted; this module decides what they
//! actually contain. If the two disagree, every recording plays back washed out
//! or oversaturated and nothing anywhere reports a problem (AGENTS.md section
//! 22). So both come from the same [`ColourSpace`], and a test encodes red,
//! green and blue and decodes them back to check that they survived.
//!
//! # Ownership
//!
//! [`Converter`] owns its `SwsContext` and frees it on drop. It owns neither
//! the pixels it reads nor the frame it writes into.

use core::ffi::c_int;
use core::time::Duration;
use std::time::Instant;

use rusty_ffmpeg::ffi;

use crate::codec::Resolution;
use crate::config::{ColourMatrix, ColourRange, ColourSpace};

/// A conversion that could not be set up or performed.
#[derive(Debug)]
pub(super) struct ConvertError {
    /// The `swscale` call that failed.
    pub(super) operation: &'static str,
    /// What was wrong with it.
    pub(super) detail: String,
}

/// `SWS_BILINEAR`, the scaling filter.
///
/// Source and destination are the same size, so nothing is scaled and the
/// filter only decides how the chroma planes are subsampled: bilinear averages
/// the four source pixels behind each chroma sample, where `SWS_POINT` would
/// keep one and discard three. Averaging is what a screen full of thin bright
/// UI text needs, and the difference in cost between the two is small next to
/// the colour conversion both have to do.
const SWS_BILINEAR: c_int = 2;

/// Converts BGRA frames to YUV 4:2:0.
pub(super) struct Converter {
    scaler: *mut ffi::SwsContext,
    resolution: Resolution,
    elapsed: Duration,
}

impl Converter {
    /// Builds a converter for `resolution`, producing samples that mean what
    /// `colour` says they mean.
    pub(super) fn open(resolution: Resolution, colour: ColourSpace) -> Result<Self, ConvertError> {
        let width = c_int::try_from(resolution.width).unwrap_or(c_int::MAX);
        let height = c_int::try_from(resolution.height).unwrap_or(c_int::MAX);

        // SAFETY: the formats are constants from the binding, the dimensions
        // are positive, and the three filter/parameter arguments are documented
        // as optional and passed as null. Ownership of the context returned
        // passes to this value, which frees it in `Drop`.
        let scaler = unsafe {
            ffi::sws_getContext(
                width,
                height,
                ffi::AV_PIX_FMT_BGRA,
                width,
                height,
                ffi::AV_PIX_FMT_YUV420P,
                SWS_BILINEAR,
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null(),
            )
        };
        if scaler.is_null() {
            return Err(ConvertError {
                operation: "sws_getContext",
                detail: format!("no BGRA to YUV 4:2:0 conversion for {resolution}"),
            });
        }

        let converter = Self {
            scaler,
            resolution,
            elapsed: Duration::ZERO,
        };
        converter.set_colour(colour)?;
        Ok(converter)
    }

    /// Tells `swscale` which matrix to use and what range to produce.
    ///
    /// Without this it uses its own default — BT.601 limited range — whatever
    /// the stream is about to claim, which is exactly the disagreement this
    /// module's documentation describes.
    fn set_colour(&self, colour: ColourSpace) -> Result<(), ConvertError> {
        // SAFETY: `sws_getCoefficients` returns a pointer to a static table
        // inside swscale for any input, falling back to the default table for
        // an unrecognised one, and never null.
        let coefficients = unsafe { ffi::sws_getCoefficients(coefficient_table(colour.matrix())) };

        // SAFETY: the context is live, both tables are the static ones swscale
        // just returned, and the last three arguments are the documented
        // neutral values for brightness, contrast and saturation (0, 1<<16,
        // 1<<16 in 16.16 fixed point).
        let code = unsafe {
            ffi::sws_setColorspaceDetails(
                self.scaler,
                // The source is BGRA, which is always full range.
                coefficients,
                1,
                coefficients,
                c_int::from(colour.range() == ColourRange::Full),
                0,
                1 << 16,
                1 << 16,
            )
        };
        if code < 0 {
            return Err(ConvertError {
                operation: "sws_setColorspaceDetails",
                detail: format!(
                    "{} could not be requested of the converter (FFmpeg error {code})",
                    colour
                ),
            });
        }
        Ok(())
    }

    /// Converts one frame's worth of BGRA pixels into `frame`.
    ///
    /// `stride` is the source row pitch in bytes, which is the graphics
    /// driver's and is usually larger than four times the width.
    ///
    /// # Safety
    ///
    /// `frame` must be a live, writable `AVFrame` holding YUV 4:2:0 planes of
    /// the resolution this converter was opened for. `pixels` must hold
    /// `stride` bytes for each of that many rows, which is what
    /// [`Readback::map`](super::readback::Readback::map) hands over.
    pub(super) unsafe fn convert(
        &mut self,
        pixels: &[u8],
        stride: usize,
        frame: *mut ffi::AVFrame,
    ) -> Result<(), ConvertError> {
        let started = Instant::now();
        let height = c_int::try_from(self.resolution.height).unwrap_or(c_int::MAX);
        let source_stride = [c_int::try_from(stride).unwrap_or(c_int::MAX), 0, 0, 0];
        let source: [*const u8; 4] = [
            pixels.as_ptr(),
            core::ptr::null(),
            core::ptr::null(),
            core::ptr::null(),
        ];

        // SAFETY: the context is live; the source pointer holds `stride` bytes
        // for each of `height` rows, as the caller guarantees and as the arrays
        // declare; and the destination pointers and strides are the caller's
        // frame, which the caller guarantees is a writable YUV 4:2:0 picture of
        // the size this context was built for. `swscale` writes only inside
        // those planes.
        let converted = unsafe {
            ffi::sws_scale(
                self.scaler,
                source.as_ptr(),
                source_stride.as_ptr(),
                0,
                height,
                (*frame).data.as_ptr(),
                (*frame).linesize.as_ptr(),
            )
        };

        self.elapsed += started.elapsed();
        if converted != height {
            return Err(ConvertError {
                operation: "sws_scale",
                detail: format!("converted {converted} of {height} rows"),
            });
        }
        Ok(())
    }

    /// How long has been spent converting.
    pub(super) const fn elapsed(&self) -> Duration {
        self.elapsed
    }
}

impl Drop for Converter {
    fn drop(&mut self) {
        // SAFETY: this value owns the context, it is freed exactly once, and
        // `sws_freeContext` accepts null — which the context never is, because
        // `open` fails rather than returning one.
        unsafe { ffi::sws_freeContext(self.scaler) };
    }
}

impl core::fmt::Debug for Converter {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Converter")
            .field("resolution", &self.resolution)
            .field("elapsed", &self.elapsed)
            .finish()
    }
}

// SAFETY: a `SwsContext` is a plain conversion state with no thread affinity —
// FFmpeg's own filters move one between threads — and nothing here shares it:
// `Converter` is owned by one `SoftwareEncoder`, which is `Send` and not
// `Sync`, and every method that touches the context takes `&mut self`.
unsafe impl Send for Converter {}

/// Which `SWS_CS_*` table produces `matrix`.
///
/// The numbers are the same as the ITU-T H.273 code points the bitstream
/// carries, and deliberately so: swscale defined its constants from the
/// standard. They are written out here rather than taken from
/// [`ColourMatrix::code_point`] anyway, because the two happening to agree is
/// not a reason for a conversion to depend on a bitstream constant — and a test
/// asserts that they still do agree, so a future divergence is a failure rather
/// than wrong colour.
const fn coefficient_table(matrix: ColourMatrix) -> c_int {
    match matrix {
        // SWS_CS_ITU709
        ColourMatrix::Bt709 => 1,
        // SWS_CS_ITU601, which swscale also calls SWS_CS_SMPTE170M and
        // SWS_CS_DEFAULT.
        ColourMatrix::Bt601 => 5,
        // SWS_CS_BT2020
        ColourMatrix::Bt2020Ncl => 9,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_conversion_matrix_is_the_one_the_stream_will_claim() {
        // The failure this guards is silent: a stream tagged BT.709 whose
        // samples were converted with the BT.601 matrix decodes without an
        // error and plays back with visibly wrong colour.
        for matrix in [
            ColourMatrix::Bt709,
            ColourMatrix::Bt601,
            ColourMatrix::Bt2020Ncl,
        ] {
            assert_eq!(
                coefficient_table(matrix),
                c_int::try_from(matrix.code_point()).expect("a code point fits in an int"),
                "the {matrix} conversion table and the {matrix} code point written into the \
                 bitstream have diverged"
            );
        }
    }
}
