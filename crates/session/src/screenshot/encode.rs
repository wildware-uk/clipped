//! Turning one captured frame into the bytes of a PNG, a JPEG or a WebP.
//!
//! # Why FFmpeg and not an image crate
//!
//! Because Clipped already links FFmpeg, and its pinned build already carries
//! `png`, `mjpeg` and `libwebp` encoders (`docs/ffmpeg.md`). Adding an imaging
//! crate would mean a second implementation of something the process already
//! has loaded, a second licence to account for, and a second set of security
//! advisories to watch — for a picture format FFmpeg encodes in twenty lines
//! (AGENTS.md sections 10 and 55). `crates/library/src/thumbnail/render.rs`
//! makes the same call for the same reason, and this file follows its shape
//! deliberately: one encoder context, one frame, one packet, all freed by
//! `Drop`.
//!
//! # What FFmpeg is used for and what it is not
//!
//! It encodes. It does not open files, does not demux and does not touch a
//! container: the bytes come back as a `Vec<u8>` and `super::write` is what
//! puts them on a disk. A still image is one packet, so there is no muxer
//! involved at all — a `.png` file *is* the packet.
//!
//! # Colour
//!
//! Handled explicitly, in both halves, because the failure when it is not is
//! silent. A captured frame is BGRA8 with full-range samples. PNG and lossless
//! WebP take those samples unchanged — the conversion is a channel reorder and
//! nothing else, so a screenshot is exactly the pixels the game drew. JPEG has
//! to become YUV, and there swscale is told the destination is full range and
//! the encoder is told the same thing; the two disagreeing produces a picture
//! that is slightly grey with nothing anywhere reporting a problem, which is
//! the same trap `docs/thumbnails.md` and `crates/encoder`'s converter both
//! document.
//!
//! # Threading
//!
//! Nothing here is called from a capture thread. Encoding a 4K PNG is tens of
//! milliseconds of processor; it happens on whichever thread asked for the
//! screenshot, after the pixels have already left the GPU (AGENTS.md section
//! 20).

use core::ffi::{c_char, c_int};
use core::ptr;

use clipped_capture::{row_bytes, PixelFormat, StillFrame};
use rusty_ffmpeg::ffi;

use super::{ScreenshotError, ScreenshotFormat};

/// `SWS_POINT`: nearest-neighbour.
///
/// The screenshot is the frame's own size, so nothing is being scaled and the
/// filter never runs. Naming the cheapest one makes that explicit rather than
/// leaving a bilinear filter to be read as though resampling were happening.
const SWS_POINT: c_int = 0x10;

/// Whether this build has an encoder for `format`.
pub(super) fn is_available(format: ScreenshotFormat) -> bool {
    // SAFETY: reads a static table inside libavcodec and returns null or a
    // pointer into it.
    !unsafe { ffi::avcodec_find_encoder(codec_id(format)) }.is_null()
}

/// Encodes `still` and returns the file's bytes.
///
/// `quality` is the MJPEG quantiser scale and is ignored by the other formats.
pub(super) fn encode(
    still: &StillFrame,
    format: ScreenshotFormat,
    quality: u32,
) -> Result<Vec<u8>, ScreenshotError> {
    if still.format().pixel_format() != PixelFormat::Bgra8Unorm {
        // Reaching here means a copier handed over something it should have
        // refused; saying which layout is what makes that debuggable.
        return Err(ScreenshotError::Encode {
            format,
            detail: format!(
                "a screenshot can only be encoded from {} and this frame is {}",
                PixelFormat::Bgra8Unorm,
                still.format().pixel_format()
            ),
        });
    }

    let width = still.size().width();
    let height = still.size().height();
    let target = target_format(format);

    let mut picture = Frame::for_picture(format, width, height, target)?;
    let mut converter = Converter::open(format, width, height, target)?;
    converter.convert(format, still, &picture)?;

    let mut encoder = StillEncoder::open(format, width, height, target, quality)?;
    encoder.encode(format, &mut picture)
}

/// Which FFmpeg encoder writes this format.
const fn codec_id(format: ScreenshotFormat) -> ffi::AVCodecID {
    match format {
        ScreenshotFormat::Png => ffi::AV_CODEC_ID_PNG,
        ScreenshotFormat::Jpeg => ffi::AV_CODEC_ID_MJPEG,
        ScreenshotFormat::WebP => ffi::AV_CODEC_ID_WEBP,
    }
}

/// The pixel layout that encoder is fed.
///
/// - PNG takes RGB directly, so the alpha channel is dropped rather than
///   written. A captured frame's alpha is whatever the compositor left there
///   and is routinely zero over the whole picture; writing it produces a
///   screenshot that looks correct in an image viewer and is entirely
///   transparent in anything that respects it.
/// - Lossless WebP takes BGRA as it stands, which is what makes it lossless:
///   there is no colour conversion between the captured frame and the file.
/// - JPEG cannot take RGB, so 4:2:0 it is, at full range.
const fn target_format(format: ScreenshotFormat) -> c_int {
    match format {
        ScreenshotFormat::Png => ffi::AV_PIX_FMT_RGB24,
        ScreenshotFormat::Jpeg => ffi::AV_PIX_FMT_YUV420P,
        ScreenshotFormat::WebP => ffi::AV_PIX_FMT_BGRA,
    }
}

/// An `AVFrame` and the picture buffer it owns.
#[derive(Debug)]
struct Frame {
    raw: *mut ffi::AVFrame,
}

impl Frame {
    /// A frame owning a writable picture of the given size and layout.
    fn for_picture(
        format: ScreenshotFormat,
        width: u32,
        height: u32,
        pixel_format: c_int,
    ) -> Result<Self, ScreenshotError> {
        // SAFETY: allocates a frame, or returns null.
        let raw = unsafe { ffi::av_frame_alloc() };
        if raw.is_null() {
            return Err(ScreenshotError::Encode {
                format,
                detail: "a picture could not be allocated".to_owned(),
            });
        }
        let frame = Self { raw };

        // SAFETY: the frame is live, exclusively owned and holds no buffer yet.
        // These four fields are what `av_frame_get_buffer` reads to decide what
        // to allocate.
        unsafe {
            (*raw).format = pixel_format;
            (*raw).width = c_int::try_from(width).unwrap_or(c_int::MAX);
            (*raw).height = c_int::try_from(height).unwrap_or(c_int::MAX);
            // A screenshot's samples are full range whatever the destination
            // layout is, and the encoder below is told the same thing.
            (*raw).color_range = ffi::AVCOL_RANGE_JPEG;
        }

        // SAFETY: the frame is live and describes a picture; zero asks
        // libavutil for its own default alignment.
        let code = unsafe { ffi::av_frame_get_buffer(raw, 0) };
        if code < 0 {
            return Err(ScreenshotError::Encode {
                format,
                detail: format!(
                    "a {width}x{height} picture could not be allocated: {}",
                    describe(code)
                ),
            });
        }
        Ok(frame)
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            // SAFETY: allocated by `av_frame_alloc` and not used again.
            unsafe { ffi::av_frame_free(&raw mut self.raw) };
        }
    }
}

/// The BGRA-to-whatever-the-encoder-takes conversion.
#[derive(Debug)]
struct Converter {
    context: *mut ffi::SwsContext,
    height: c_int,
}

impl Converter {
    fn open(
        format: ScreenshotFormat,
        width: u32,
        height: u32,
        target: c_int,
    ) -> Result<Self, ScreenshotError> {
        let width = c_int::try_from(width).unwrap_or(c_int::MAX);
        let height = c_int::try_from(height).unwrap_or(c_int::MAX);

        // SAFETY: every argument is a plain integer or a documented null; the
        // returned context is null or owned by this value, which frees it in
        // `Drop`. Source and destination sizes are equal, so nothing is scaled.
        let context = unsafe {
            ffi::sws_getContext(
                width,
                height,
                ffi::AV_PIX_FMT_BGRA,
                width,
                height,
                target,
                SWS_POINT,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            )
        };
        if context.is_null() {
            return Err(ScreenshotError::Encode {
                format,
                detail: format!(
                    "no conversion from BGRA at {width}x{height} to pixel format {target}"
                ),
            });
        }

        let converter = Self { context, height };
        converter.set_full_range();
        Ok(converter)
    }

    /// Tells swscale that both sides are full range.
    ///
    /// Without it swscale assumes limited range on the YUV side and every JPEG
    /// screenshot comes out slightly grey — the silent failure this module's
    /// documentation names. Ignored for the RGB destinations, where swscale
    /// refuses the call and its own defaults are already right.
    fn set_full_range(&self) {
        // SAFETY: `sws_getCoefficients` reads a static table inside swscale for
        // any input and never returns null.
        let coefficients = unsafe { ffi::sws_getCoefficients(ffi::SWS_CS_ITU709 as c_int) };
        // SAFETY: the context is live, both tables are the static one swscale
        // just returned, and the last three arguments are the documented
        // neutral values for brightness, contrast and saturation.
        let code = unsafe {
            ffi::sws_setColorspaceDetails(
                self.context,
                coefficients,
                1,
                coefficients,
                1,
                0,
                1 << 16,
                1 << 16,
            )
        };
        if code < 0 {
            tracing::debug!(
                "swscale would not take a colour range for this conversion; using its defaults"
            );
        }
    }

    /// Converts `still` into `picture`.
    fn convert(
        &mut self,
        format: ScreenshotFormat,
        still: &StillFrame,
        picture: &Frame,
    ) -> Result<(), ScreenshotError> {
        let stride = c_int::try_from(still.stride()).unwrap_or(c_int::MAX);
        let source: [*const u8; 4] = [
            still.as_bytes().as_ptr(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
        ];
        let strides: [c_int; 4] = [stride, 0, 0, 0];

        // The buffer really does hold every row the stride claims: `StillFrame`
        // refuses to exist otherwise, which is what makes the pointer arithmetic
        // swscale is about to do stay inside it.
        debug_assert!(still.byte_count() >= still.stride() * still.size().height() as usize);
        debug_assert!(still.stride() >= row_bytes(still.format()));

        // SAFETY: the context is live and was built for a BGRA source of
        // exactly this size; `source[0]` points at `height` rows `stride` bytes
        // apart, which `StillFrame::new` guaranteed; and the destination planes
        // are the live picture allocated for this conversion's output size.
        // swscale writes only inside those planes.
        let rows = unsafe {
            ffi::sws_scale(
                self.context,
                source.as_ptr(),
                strides.as_ptr(),
                0,
                self.height,
                (*picture.raw).data.as_ptr(),
                (*picture.raw).linesize.as_ptr(),
            )
        };

        if rows != self.height {
            return Err(ScreenshotError::Encode {
                format,
                detail: format!(
                    "converting the frame produced {rows} of {} rows",
                    self.height
                ),
            });
        }
        Ok(())
    }
}

impl Drop for Converter {
    fn drop(&mut self) {
        // SAFETY: this value owns the context, it is freed exactly once, and
        // `sws_freeContext` accepts null — which it never is, because `open`
        // fails rather than returning one.
        unsafe { ffi::sws_freeContext(self.context) };
    }
}

/// One still-image encoder, open for one picture.
#[derive(Debug)]
struct StillEncoder {
    context: *mut ffi::AVCodecContext,
}

impl StillEncoder {
    fn open(
        format: ScreenshotFormat,
        width: u32,
        height: u32,
        pixel_format: c_int,
        quality: u32,
    ) -> Result<Self, ScreenshotError> {
        // SAFETY: reads a static table inside libavcodec and returns null or a
        // pointer into it.
        let codec = unsafe { ffi::avcodec_find_encoder(codec_id(format)) };
        if codec.is_null() {
            // Not an `Encode` failure: the format is simply not in this build,
            // which is a different thing to tell a user and the one SPEC.md
            // section 26's "where practical" is about.
            return Err(ScreenshotError::FormatUnavailable { format });
        }

        // SAFETY: `codec` is a live descriptor; this returns null or a context
        // this value owns from here on.
        let context = unsafe { ffi::avcodec_alloc_context3(codec) };
        if context.is_null() {
            return Err(ScreenshotError::Encode {
                format,
                detail: "an encoder context could not be allocated".to_owned(),
            });
        }
        let encoder = Self { context };

        // SAFETY: the context is live, exclusively owned and not yet open, and
        // every field written here is one libavcodec documents as the caller's
        // to set before `avcodec_open2`.
        unsafe {
            (*context).width = c_int::try_from(width).unwrap_or(c_int::MAX);
            (*context).height = c_int::try_from(height).unwrap_or(c_int::MAX);
            (*context).pix_fmt = pixel_format;
            (*context).color_range = ffi::AVCOL_RANGE_JPEG;
            // A still image, so the time base is arbitrary; one second a frame
            // keeps the numbers small.
            (*context).time_base = ffi::AVRational { num: 1, den: 1 };
        }

        if format == ScreenshotFormat::Jpeg {
            // SAFETY: as above. MJPEG's quality is a quantiser scale rather
            // than a bitrate, and this flag is what makes libavcodec read it.
            unsafe {
                (*context).flags |= ffi::AV_CODEC_FLAG_QSCALE as c_int;
                (*context).global_quality =
                    c_int::try_from(quality * ffi::FF_QP2LAMBDA).unwrap_or(c_int::MAX);
            }
        }

        if format == ScreenshotFormat::WebP {
            // The whole reason this format is worth having: without it libwebp
            // encodes lossily and a "lossless WebP" setting would be a lie
            // (AGENTS.md section 54). A build whose libwebp does not know the
            // option refuses the format rather than writing a lossy file under
            // that name.
            //
            // SAFETY: the context is live and not yet open, so `priv_data` is
            // the encoder's own option block; the name is a NUL-terminated
            // static string and `AV_OPT_SEARCH_CHILDREN` is not needed because
            // the option belongs to `priv_data` itself.
            let code =
                unsafe { ffi::av_opt_set_int((*context).priv_data, c"lossless".as_ptr(), 1, 0) };
            if code < 0 {
                return Err(ScreenshotError::Encode {
                    format,
                    detail: format!(
                        "this build's WebP encoder would not be set to lossless: {}",
                        describe(code)
                    ),
                });
            }
        }

        // SAFETY: the context is live and configured; the options argument is
        // documented as nullable.
        let code = unsafe { ffi::avcodec_open2(context, codec, ptr::null_mut()) };
        if code < 0 {
            return Err(ScreenshotError::Encode {
                format,
                detail: format!(
                    "the encoder would not open for {width}x{height}: {}",
                    describe(code)
                ),
            });
        }
        Ok(encoder)
    }

    /// Encodes one picture and returns the file's bytes.
    fn encode(
        &mut self,
        format: ScreenshotFormat,
        picture: &mut Frame,
    ) -> Result<Vec<u8>, ScreenshotError> {
        // SAFETY: the picture is live and owned by the caller. `quality` is
        // what a qscale-mode encoder reads per frame; `global_quality` alone is
        // not enough, and a frame left at zero encodes at the best possible
        // quality and several times the intended size.
        unsafe {
            (*picture.raw).quality = (*self.context).global_quality;
            (*picture.raw).pts = 0;
        }

        // SAFETY: the context is live and open, and the picture is of the size
        // and layout it was opened for.
        let code = unsafe { ffi::avcodec_send_frame(self.context, picture.raw) };
        if code < 0 {
            return Err(ScreenshotError::Encode {
                format,
                detail: format!("the picture could not be encoded: {}", describe(code)),
            });
        }
        // SAFETY: as above. A null frame is the documented end of the stream,
        // which for a single-picture encode is immediately.
        let _ = unsafe { ffi::avcodec_send_frame(self.context, ptr::null_mut()) };

        let packet = Packet::allocate(format)?;
        // SAFETY: the context is live and open, and the packet is live and
        // holds nothing.
        let code = unsafe { ffi::avcodec_receive_packet(self.context, packet.raw) };
        if code < 0 {
            return Err(ScreenshotError::Encode {
                format,
                detail: format!("the encoder produced nothing: {}", describe(code)),
            });
        }

        // SAFETY: the packet was just filled in, so `data` holds `size` bytes
        // that live until the packet is unreferenced, which happens when it is
        // dropped at the end of this function.
        let bytes = unsafe {
            let size = usize::try_from((*packet.raw).size).unwrap_or(0);
            if (*packet.raw).data.is_null() || size == 0 {
                Vec::new()
            } else {
                core::slice::from_raw_parts((*packet.raw).data, size).to_vec()
            }
        };

        if bytes.is_empty() {
            return Err(ScreenshotError::Encode {
                format,
                detail: "the encoder reported success and produced an empty picture".to_owned(),
            });
        }
        Ok(bytes)
    }
}

impl Drop for StillEncoder {
    fn drop(&mut self) {
        if !self.context.is_null() {
            // SAFETY: allocated by `avcodec_alloc_context3` and not used again.
            unsafe { ffi::avcodec_free_context(&raw mut self.context) };
        }
    }
}

/// An `AVPacket`, freed on drop.
#[derive(Debug)]
struct Packet {
    raw: *mut ffi::AVPacket,
}

impl Packet {
    fn allocate(format: ScreenshotFormat) -> Result<Self, ScreenshotError> {
        // SAFETY: allocates a packet, or returns null.
        let raw = unsafe { ffi::av_packet_alloc() };
        if raw.is_null() {
            return Err(ScreenshotError::Encode {
                format,
                detail: "a packet could not be allocated".to_owned(),
            });
        }
        Ok(Self { raw })
    }
}

impl Drop for Packet {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            // SAFETY: allocated by `av_packet_alloc` and not used again;
            // `av_packet_free` unreferences whatever it holds first.
            unsafe { ffi::av_packet_free(&raw mut self.raw) };
        }
    }
}

/// What FFmpeg says about an error code.
fn describe(code: c_int) -> String {
    let mut buffer = [0 as c_char; 256];
    // SAFETY: the buffer is a live local of the length given, and
    // `av_strerror` writes a NUL-terminated string of at most that length.
    let written = unsafe { ffi::av_strerror(code, buffer.as_mut_ptr(), buffer.len()) };
    if written < 0 {
        return format!("FFmpeg error {code}");
    }
    // SAFETY: `av_strerror` returned success, so the buffer holds a
    // NUL-terminated string within its own length.
    let text = unsafe { core::ffi::CStr::from_ptr(buffer.as_ptr()) };
    text.to_string_lossy().into_owned()
}
