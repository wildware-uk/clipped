//! The `libopenh264` encoding session, driven through libavcodec.
//!
//! # Why this encoder and not x264
//!
//! Because of the project's licence, not because of its quality. x264 is the
//! software H.264 encoder everyone reaches for, and it is **GPL**: linking it
//! would require every binary Clipped distributes to be GPL as well, which is a
//! decision about what this project is rather than a choice of encoder
//! (`docs/adr/0004-ffmpeg-dependency-strategy.md`). The FFmpeg build Clipped
//! links is configured `--disable-libx264 --disable-libx265` for that reason,
//! and carries Cisco's `libopenh264` — **BSD-2-Clause** — instead.
//! `crates/muxer/tests/ffmpeg_linkage.rs` asserts it is still there, so a build
//! that dropped it fails a test rather than this backend.
//!
//! What it costs: `libopenh264` compresses less well than x264 at the same bit
//! rate, has no B-frames, and offers a smaller set of rate control modes. None
//! of that matters much for a fallback nobody should be using unless their
//! machine has no encoder in it.
//!
//! Patent licensing is a separate question from copyright licensing and is
//! untouched by any of this: distributing something that encodes H.264 has
//! patent-pool implications whoever wrote the encoder. ADR 0004 says so, and it
//! is out of scope here.
//!
//! # Ownership
//!
//! [`CodecSession`] owns the codec context, the picture it fills in and the
//! packet it receives into, and frees all three in `Drop`. Every one of them is
//! null or live and never anything else, so `Drop` is correct on every path out
//! of [`CodecSession::open`], including the ones that fail half way.

use core::ffi::{c_int, CStr};
use core::ptr;
use core::time::Duration;
use std::time::Instant;

use rusty_ffmpeg::ffi;

use crate::codec::{Codec, Resolution};
use crate::config::{
    ColourSpace, EncodePreset, EncoderConfig, KeyframeInterval, QualityTarget, RateControl,
};
use crate::error::EncodeErrorKind;
use crate::packet::PictureKind;

/// The software H.264 encoder in the pinned LGPL build.
pub(super) const ENCODER: &CStr = c"libopenh264";

/// FFmpeg's `AVERROR_EOF`, which is `FFERRTAG('E','O','F',' ')`.
///
/// The binding does not carry it: it is a macro over another macro, and
/// `bindgen` expands neither.
const AVERROR_EOF: c_int = -0x20_46_4F_45;

/// FFmpeg's `AVERROR(EAGAIN)`: the encoder has taken the frame and has no
/// packet ready yet.
#[allow(clippy::cast_possible_wrap)]
const AVERROR_EAGAIN: c_int = -(ffi::EAGAIN as c_int);

/// The time base every timestamp crossing this interface is in.
///
/// A nanosecond, rather than one frame, because a presentation time comes from
/// the capture clock and is not a whole number of frames: a capture at 60 frames
/// a second delivers at whatever intervals the compositor managed, and rounding
/// each one to the nearest frame would write the recorder's idea of the frame
/// rate into the file instead of what was captured (AGENTS.md section 22).
///
/// A nanosecond specifically, rather than the microsecond or the 1/90000 that
/// containers usually use, because that is the unit a
/// [`Duration`](core::time::Duration) counts in: at any coarser base the
/// timestamp a frame went in with is not the one that comes out, and the
/// interface promises it is. It costs nothing — an `i64` of nanoseconds is 292
/// years — and what a container stores is the muxer's decision rather than this
/// one.
const TIME_BASE_HZ: i64 = 1_000_000_000;

/// How many slices the picture may be split into.
///
/// `libopenh264` parallelises by cutting each picture into horizontal slices
/// and encoding one per thread, which costs compression efficiency — each slice
/// starts its own entropy context and cannot predict across the boundary — and
/// takes cores from the game being recorded. Four is the cap because this is
/// the fallback on a machine that could not afford a hardware encoder, and a
/// recorder that takes every core it can find is a recorder that makes the game
/// unplayable in order to record it well (AGENTS.md section 18).
const MAX_SLICES: usize = 4;

/// The bits per pixel per frame the default quality level is worth.
///
/// [`RateControl::Quality`] asks for a quantisation level, which is a knob
/// `libopenh264` does not have: it targets a bit rate. So a quality level
/// becomes a bit rate here, and this is the anchor — 0.09 bits per pixel at
/// [`QualityTarget::DEFAULT`], which is 20 Mbit/s at 2560x1440 and 60 frames a
/// second, in the region where game footage stops showing obvious blocking.
const BITS_PER_PIXEL_AT_DEFAULT_QUALITY: f64 = 0.09;

/// The narrowest bit rate a derived one is allowed to be.
const MINIMUM_DERIVED_RATE: f64 = 1_000_000.0;

/// The widest bit rate a derived one is allowed to be.
///
/// A CPU encoder that is asked for 500 Mbit/s spends the whole machine writing
/// a file nothing can play back smoothly.
const MAXIMUM_DERIVED_RATE: f64 = 200_000_000.0;

/// A live `libopenh264` encoding session.
pub(super) struct CodecSession {
    context: *mut ffi::AVCodecContext,
    frame: *mut ffi::AVFrame,
    packet: *mut ffi::AVPacket,
    /// Whether [`receive`](Self::receive) handed out a packet that has not been
    /// unreferenced yet. The bytes belong to the session until then, which is
    /// the contract [`EncodedPacket`](crate::EncodedPacket) states.
    packet_held: bool,
    /// The sequence and picture parameter sets, from the context's `extradata`.
    parameter_sets: Vec<u8>,
    /// Where a keyframe is assembled when the encoder did not put the parameter
    /// sets in front of it. See [`receive`](Self::receive).
    repeated: Vec<u8>,
    elapsed: Duration,
    frames: u64,
}

impl CodecSession {
    /// Opens the encoder for `config`.
    pub(super) fn open(config: &EncoderConfig) -> Result<Self, EncodeErrorKind> {
        // SAFETY: reads the NUL-terminated name and returns either null or a
        // pointer to a static descriptor inside libavcodec.
        let codec = unsafe { ffi::avcodec_find_encoder_by_name(ENCODER.as_ptr()) };
        if codec.is_null() {
            return Err(EncodeErrorKind::Configuration {
                detail: format!(
                    "the FFmpeg this build is linked against has no {} encoder, so there is no \
                     software encoder to fall back to; run scripts/fetch-ffmpeg.ps1 to install \
                     the pinned build (docs/ffmpeg.md)",
                    ENCODER.to_string_lossy()
                ),
            });
        }

        // SAFETY: `codec` is a live descriptor. This returns either null or a
        // context this value owns from here on, so every failure below releases
        // it through `Drop`.
        let context = unsafe { ffi::avcodec_alloc_context3(codec) };
        let mut session = Self {
            context,
            frame: ptr::null_mut(),
            packet: ptr::null_mut(),
            packet_held: false,
            parameter_sets: Vec::new(),
            repeated: Vec::new(),
            elapsed: Duration::ZERO,
            frames: 0,
        };
        if session.context.is_null() {
            return Err(EncodeErrorKind::OutOfMemory);
        }

        session.configure(config)?;
        session.open_codec(codec, config)?;
        session.allocate_buffers(config.resolution())?;
        Ok(session)
    }

    /// Fills in everything libavcodec has to be told before the codec is
    /// opened.
    fn configure(&mut self, config: &EncoderConfig) -> Result<(), EncodeErrorKind> {
        let resolution = config.resolution();
        let (average, peak) = rates(config);
        let colour = config.colour_space();

        // SAFETY: the context is live, exclusively owned and not yet open, and
        // every field written is one libavcodec documents as the caller's to
        // set before `avcodec_open2`.
        unsafe {
            let context = self.context;
            (*context).width = dimension(resolution.width);
            (*context).height = dimension(resolution.height);
            (*context).pix_fmt = ffi::AV_PIX_FMT_YUV420P;
            (*context).time_base = ffi::AVRational {
                num: 1,
                den: dimension(u32::try_from(TIME_BASE_HZ).unwrap_or(u32::MAX)),
            };
            (*context).framerate = ffi::AVRational {
                num: dimension(config.frame_rate().numerator()),
                den: dimension(config.frame_rate().denominator()),
            };
            (*context).bit_rate = average;
            (*context).rc_max_rate = peak.unwrap_or(0);
            (*context).gop_size = gop_size(config.keyframe_interval());
            // `libopenh264` produces none, and the interface promises a decode
            // timestamp equal to the presentation timestamp because of it.
            (*context).max_b_frames = 0;
            (*context).thread_count = threads();
            // What the samples mean, written into the stream's VUI. The
            // conversion that produces those samples is set from the same
            // values in `super::convert`, which is what keeps the description
            // and the pixels in agreement.
            (*context).color_primaries = colour_primaries(colour);
            (*context).color_trc = transfer(colour);
            (*context).colorspace = matrix(colour);
            (*context).color_range = range(colour);
            // Puts the parameter sets in `extradata`, where a container wants
            // them, and where this session can read them before a single frame
            // has been encoded. They are put back in front of every keyframe by
            // `receive`, so an elementary stream is still decodable from any
            // keyframe.
            (*context).flags |= dimension(ffi::AV_CODEC_FLAG_GLOBAL_HEADER);
        }
        Ok(())
    }

    /// Opens the codec, with the options that are not `AVCodecContext` fields.
    fn open_codec(
        &mut self,
        codec: *const ffi::AVCodec,
        config: &EncoderConfig,
    ) -> Result<(), EncodeErrorKind> {
        let mut options: *mut ffi::AVDictionary = ptr::null_mut();
        // The first option that could not be *offered*, as opposed to one the
        // encoder declined. `av_dict_set` fails only by failing to allocate,
        // and dropping that on the floor would open the session with
        // `libopenh264`'s default for whatever never reached it — the same
        // failure the `unrecognised` warning below exists to prevent, which
        // catches only the options that did reach it (AGENTS.md section 15).
        // The first is kept rather than the last because the later calls are
        // meaningless once allocation is failing.
        let mut rejected: Option<(&'static CStr, i32)> = None;
        let mut set = |key: &'static CStr, value: &CStr| {
            // SAFETY: `options` is a live local holding either null — which
            // `av_dict_set` documents as "allocate a new dictionary" — or a
            // dictionary allocated by an earlier call, and both strings are
            // NUL-terminated literals which the call copies.
            let code =
                unsafe { ffi::av_dict_set(&raw mut options, key.as_ptr(), value.as_ptr(), 0) };
            if code < 0 && rejected.is_none() {
                rejected = Some((key, code));
            }
        };

        let (rate_control, entropy_coder, profile) = match config.rate_control() {
            // `libopenh264`'s bitrate mode holds an average, which is what a
            // recorder writing to a disk with a size budget wants. Its default
            // is quality mode, which ignores the target rate almost entirely.
            RateControl::Bitrate { .. } => (c"bitrate", entropy(config.preset()), profile(config)),
            RateControl::Quality { .. } => (c"quality", entropy(config.preset()), profile(config)),
        };
        set(c"rc_mode", rate_control);
        set(c"coder", entropy_coder);
        set(c"profile", profile);
        // Dropping frames to hold a bit rate is the wrong trade for a recorder:
        // a missing frame is a visible stutter in a clip, and a bit rate
        // overshoot is a slightly larger file (SPEC.md section 9).
        set(c"allow_skip_frames", c"0");

        if let Some((key, code)) = rejected {
            // SAFETY: `options` is either null or a dictionary from the calls
            // above, and is not used after this — the function returns here.
            unsafe { ffi::av_dict_free(&raw mut options) };
            // The key is in the log rather than in the error because
            // `av_dict_set` fails by failing to allocate, and the error that
            // reaches a user for that is `OutOfMemory`, which carries no
            // operation. Which option was lost is a diagnostic, and diagnostics
            // are where the deeper detail belongs (AGENTS.md section 15).
            tracing::error!(
                encoder = "software_h264",
                option = key.to_string_lossy().as_ref(),
                code,
                "the software encoder's options could not be assembled"
            );
            return Err(failure("av_dict_set", code));
        }

        // SAFETY: the context was allocated for this codec and has not been
        // opened; `options` is a live local, and libavcodec replaces it with
        // whatever it did not recognise.
        let code = unsafe { ffi::avcodec_open2(self.context, codec, &raw mut options) };

        // SAFETY: `options` is either null or the dictionary above, and is not
        // used after this.
        let unrecognised = unsafe {
            let count = ffi::av_dict_count(options);
            ffi::av_dict_free(&raw mut options);
            count
        };
        if unrecognised > 0 {
            // Not a failure: the encoder still works, with its own default for
            // whatever it did not take. It is worth a line in the log, because
            // an option silently ignored is how a build ends up encoding with
            // settings nobody chose.
            tracing::warn!(
                encoder = "software_h264",
                unrecognised,
                "the software encoder did not recognise every option it was given"
            );
        }

        if code < 0 {
            return Err(failure("avcodec_open2", code));
        }

        // SAFETY: the context is live and open. `extradata` is null or points
        // at `extradata_size` bytes owned by the context; the bytes are copied
        // out here and the pointer is not kept.
        self.parameter_sets = unsafe {
            let size = usize::try_from((*self.context).extradata_size).unwrap_or(0);
            if (*self.context).extradata.is_null() || size == 0 {
                Vec::new()
            } else {
                core::slice::from_raw_parts((*self.context).extradata, size).to_vec()
            }
        };
        if self.parameter_sets.is_empty() {
            return Err(EncodeErrorKind::Configuration {
                detail: "the software encoder opened without producing the parameter sets a \
                         container needs before the first frame"
                    .to_owned(),
            });
        }
        Ok(())
    }

    /// Allocates the picture the caller's frames are converted into, and the
    /// packet coded pictures come back in.
    fn allocate_buffers(&mut self, resolution: Resolution) -> Result<(), EncodeErrorKind> {
        // SAFETY: both take no arguments and return null on failure; what they
        // return is owned by this value and freed in `Drop`.
        unsafe {
            self.frame = ffi::av_frame_alloc();
            self.packet = ffi::av_packet_alloc();
        }
        if self.frame.is_null() || self.packet.is_null() {
            return Err(EncodeErrorKind::OutOfMemory);
        }

        // SAFETY: the frame is live and empty, and the three fields set are the
        // ones `av_frame_get_buffer` reads to decide what to allocate. The
        // buffers it allocates belong to the frame and are freed with it.
        let code = unsafe {
            (*self.frame).width = dimension(resolution.width);
            (*self.frame).height = dimension(resolution.height);
            (*self.frame).format = ffi::AV_PIX_FMT_YUV420P;
            ffi::av_frame_get_buffer(self.frame, 0)
        };
        if code < 0 {
            return Err(failure("av_frame_get_buffer", code));
        }
        Ok(())
    }

    /// The parameter sets, available from the moment the session opens.
    pub(super) fn parameter_sets(&self) -> &[u8] {
        &self.parameter_sets
    }

    /// Makes the picture writable and hands it over to be filled in.
    ///
    /// The encoder keeps a reference to a frame it has been given, so this may
    /// allocate a replacement rather than hand back the same buffers.
    pub(super) fn writable_picture(&mut self) -> Result<*mut ffi::AVFrame, EncodeErrorKind> {
        // SAFETY: the frame is live and its buffers were allocated in
        // `allocate_buffers`. `av_frame_make_writable` replaces them if
        // anything else still holds a reference to them.
        let code = unsafe { ffi::av_frame_make_writable(self.frame) };
        if code < 0 {
            return Err(failure("av_frame_make_writable", code));
        }
        Ok(self.frame)
    }

    /// Hands the picture to the encoder, at `presentation` in the recording.
    pub(super) fn send(
        &mut self,
        presentation: Duration,
        force_keyframe: bool,
    ) -> Result<(), EncodeErrorKind> {
        let started = Instant::now();

        // SAFETY: the frame is live, was made writable and filled in by the
        // caller, and describes the picture the encoder was opened for.
        unsafe {
            (*self.frame).pts = timestamp(presentation);
            // The way libavcodec asks an encoder for a keyframe out of turn.
            // `libopenh264` maps it onto `ForceIntraFrame`.
            (*self.frame).pict_type = if force_keyframe {
                ffi::AV_PICTURE_TYPE_I
            } else {
                ffi::AV_PICTURE_TYPE_NONE
            };
        }

        // SAFETY: both pointers are live and the context is open for encoding.
        let code = unsafe { ffi::avcodec_send_frame(self.context, self.frame) };
        self.elapsed += started.elapsed();
        if code < 0 {
            return Err(failure("avcodec_send_frame", code));
        }
        self.frames += 1;
        Ok(())
    }

    /// Declares the end of the stream, so that anything held back comes out.
    pub(super) fn flush(&mut self) -> Result<(), EncodeErrorKind> {
        self.release_packet();
        // SAFETY: a null frame is the documented way to tell an encoder that no
        // more input is coming; the context is live and open.
        let code = unsafe { ffi::avcodec_send_frame(self.context, ptr::null()) };
        if code < 0 {
            return Err(failure("avcodec_send_frame (flush)", code));
        }
        Ok(())
    }

    /// Takes the next coded picture, if the encoder has one ready.
    ///
    /// The bytes belong to the session until the next call, which is what the
    /// `&mut self` borrow makes the compiler enforce for the
    /// [`EncodedPacket`](crate::EncodedPacket) built from them.
    pub(super) fn receive(&mut self) -> Result<Option<Coded<'_>>, EncodeErrorKind> {
        self.release_packet();

        let started = Instant::now();
        // SAFETY: both pointers are live, and the packet was unreferenced
        // above, which is the state `avcodec_receive_packet` requires.
        let code = unsafe { ffi::avcodec_receive_packet(self.context, self.packet) };
        self.elapsed += started.elapsed();

        if code == AVERROR_EAGAIN || code == AVERROR_EOF {
            return Ok(None);
        }
        if code < 0 {
            return Err(failure("avcodec_receive_packet", code));
        }
        self.packet_held = true;

        // SAFETY: a packet returned by `avcodec_receive_packet` holds `size`
        // bytes at `data`, owned by the packet until it is unreferenced — which
        // is the next call to this method, after the borrow returned here has
        // ended.
        let (data, presentation, decode, keyframe) = unsafe {
            let packet = self.packet;
            let size = usize::try_from((*packet).size).unwrap_or(0);
            (
                core::slice::from_raw_parts((*packet).data, size),
                (*packet).pts,
                (*packet).dts,
                ((*packet).flags & dimension(ffi::AV_PKT_FLAG_KEY)) != 0,
            )
        };

        // A keyframe that no decoder can start on is not a cut point, and every
        // clip the replay buffer saves starts at one (SPEC.md section 7). With
        // the global header flag set, the parameter sets are in `extradata` and
        // an encoder is entitled to leave them out of the stream, so they are
        // put back — into a buffer of this session's, because the packet's is
        // libavcodec's and is exactly the size of what it coded.
        let data = if keyframe && !starts_with_parameter_sets(data, &self.parameter_sets) {
            self.repeated.clear();
            self.repeated
                .reserve(self.parameter_sets.len() + data.len());
            self.repeated.extend_from_slice(&self.parameter_sets);
            self.repeated.extend_from_slice(data);
            &self.repeated
        } else {
            data
        };

        Ok(Some(Coded {
            data,
            presentation: duration(presentation),
            // `libopenh264` reorders nothing, so the two are equal; taken from
            // the packet rather than assumed, because an encoder that did
            // reorder would otherwise be silently mistimed.
            decode: duration(if decode == i64::MIN {
                presentation
            } else {
                decode
            }),
            picture: if keyframe {
                PictureKind::Keyframe
            } else {
                PictureKind::Predicted
            },
        }))
    }

    /// Releases the packet handed out by the previous [`receive`](Self::receive).
    fn release_packet(&mut self) {
        if !self.packet_held {
            return;
        }
        self.packet_held = false;
        // SAFETY: the packet is live and the borrow of its payload has ended —
        // it was returned inside a `Coded` borrowing `&mut self`, so the
        // compiler will not let a caller still hold it here.
        unsafe { ffi::av_packet_unref(self.packet) };
    }

    /// How long has been spent inside libavcodec, and over how many frames.
    pub(super) const fn elapsed(&self) -> (Duration, u64) {
        (self.elapsed, self.frames)
    }
}

impl Drop for CodecSession {
    fn drop(&mut self) {
        self.release_packet();
        // SAFETY: this value owns all three, each is freed exactly once here,
        // and every one of these takes a pointer to the pointer and does
        // nothing when it is null — which is the state `open` leaves behind
        // when it fails part of the way through.
        unsafe {
            ffi::av_packet_free(&raw mut self.packet);
            ffi::av_frame_free(&raw mut self.frame);
            ffi::avcodec_free_context(&raw mut self.context);
        }
    }
}

impl core::fmt::Debug for CodecSession {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("CodecSession")
            .field("encoder", &ENCODER)
            .field("frames", &self.frames)
            .field("elapsed", &self.elapsed)
            .finish()
    }
}

// SAFETY: `VideoEncoder` is `Send` so that a session can be built on one thread
// and moved to the encoding thread, and this has to satisfy it.
//
// What crosses the boundary is three FFmpeg allocations. None of them is
// thread-bound: libavcodec documents an `AVCodecContext` as usable from any
// single thread at a time — its own frame-threading moves contexts between
// threads — and an `AVFrame` and an `AVPacket` are reference-counted buffers
// with no affinity at all.
//
// What is *not* claimed is that two threads may use one session at once. They
// may not, and nothing here makes it possible: every method takes `&mut self`,
// the type is not `Sync`, and a coded packet borrows the session it came from.
unsafe impl Send for CodecSession {}

/// One coded picture, borrowed from the session that produced it.
#[derive(Debug)]
pub(super) struct Coded<'session> {
    /// The coded bytes.
    pub(super) data: &'session [u8],
    /// When the picture is shown.
    pub(super) presentation: Duration,
    /// When it must be decoded.
    pub(super) decode: Duration,
    /// What kind of picture it is.
    pub(super) picture: PictureKind,
}

/// Whether a coded picture already begins with the parameter sets.
///
/// Compared against the session's own copy rather than by parsing NAL headers:
/// the bytes an encoder would put in front of a keyframe are the bytes it put
/// in `extradata`, and a comparison cannot be wrong about a start code prefix
/// the way a parser can.
fn starts_with_parameter_sets(data: &[u8], parameter_sets: &[u8]) -> bool {
    !parameter_sets.is_empty() && data.starts_with(parameter_sets)
}

/// Turns a presentation time into the encoder's time base.
fn timestamp(presentation: Duration) -> i64 {
    i64::try_from(presentation.as_nanos()).unwrap_or(i64::MAX)
}

/// Turns a timestamp in the encoder's time base back into a duration.
fn duration(ticks: i64) -> Duration {
    Duration::from_nanos(ticks.max(0).unsigned_abs())
}

/// The average and peak bit rates to ask for.
///
/// [`RateControl::Bitrate`] is what it says. [`RateControl::Quality`] is not:
/// `libopenh264` has no constant-quality mode, so a quality level becomes a bit
/// rate here, from the picture size, the frame rate and
/// [`BITS_PER_PIXEL_AT_DEFAULT_QUALITY`]. Every six levels of the scale doubles
/// or halves it, which is the relationship a quantisation parameter has to bit
/// rate in every codec of this generation.
fn rates(config: &EncoderConfig) -> (i64, Option<i64>) {
    match config.rate_control() {
        RateControl::Bitrate { average, peak } => (
            i64::from(average.as_bits_per_second()),
            peak.map(|peak| i64::from(peak.as_bits_per_second())),
        ),
        RateControl::Quality { target, ceiling } => {
            let derived = derived_rate(config.resolution(), config.frame_rate().as_f64(), target);
            let ceiling = ceiling.map(|ceiling| i64::from(ceiling.as_bits_per_second()));
            let average = ceiling.map_or(derived, |ceiling| derived.min(ceiling));
            (average, ceiling)
        }
    }
}

/// The bit rate a quality level is worth at this size and frame rate.
fn derived_rate(resolution: Resolution, frames_per_second: f64, target: QualityTarget) -> i64 {
    let pixels = f64::from(resolution.width) * f64::from(resolution.height);
    let steps = f64::from(QualityTarget::DEFAULT.as_level()) - f64::from(target.as_level());
    let bits_per_pixel = BITS_PER_PIXEL_AT_DEFAULT_QUALITY * (steps / 6.0).exp2();
    let rate = (pixels * frames_per_second * bits_per_pixel)
        .clamp(MINIMUM_DERIVED_RATE, MAXIMUM_DERIVED_RATE);

    #[allow(clippy::cast_possible_truncation)]
    {
        rate as i64
    }
}

/// How many frames apart the keyframes are, as libavcodec counts them.
fn gop_size(interval: KeyframeInterval) -> c_int {
    match interval.as_frames() {
        Some(frames) => c_int::try_from(frames.get()).unwrap_or(c_int::MAX),
        None => c_int::MAX,
    }
    .max(1)
}

/// How many threads the encoder may use.
fn threads() -> c_int {
    let available = std::thread::available_parallelism().map_or(1, core::num::NonZeroUsize::get);
    c_int::try_from((available / 2).clamp(1, MAX_SLICES)).unwrap_or(1)
}

/// Which entropy coder a preset asks for.
///
/// CABAC costs a few per cent of encoding time and buys a few per cent of
/// compression; it is the only knob `libopenh264` exposes through FFmpeg that
/// trades one for the other in the direction a preset means.
const fn entropy(preset: EncodePreset) -> &'static CStr {
    match preset {
        EncodePreset::Speed => c"cavlc",
        EncodePreset::Balanced | EncodePreset::Quality => c"cabac",
    }
}

/// Which H.264 profile to restrict the stream to.
///
/// Constrained baseline for the speed preset because it is what CAVLC without
/// 8x8 transforms amounts to, and every decoder in existence takes it; high
/// otherwise, which is what the entropy coder and the transform sizes above it
/// need.
const fn profile(config: &EncoderConfig) -> &'static CStr {
    match config.preset() {
        EncodePreset::Speed => c"constrained_baseline",
        EncodePreset::Balanced | EncodePreset::Quality => c"high",
    }
}

/// A dimension or a flag as libavcodec's `int`.
fn dimension<T: TryInto<c_int>>(value: T) -> c_int {
    value.try_into().unwrap_or(c_int::MAX)
}

/// The `AVColorPrimaries` for a colour space.
fn colour_primaries(colour: ColourSpace) -> ffi::AVColorPrimaries {
    dimension(colour.primaries().code_point())
}

/// The `AVColorTransferCharacteristic` for a colour space.
fn transfer(colour: ColourSpace) -> ffi::AVColorTransferCharacteristic {
    dimension(colour.transfer().code_point())
}

/// The `AVColorSpace` for a colour space.
fn matrix(colour: ColourSpace) -> ffi::AVColorSpace {
    dimension(colour.matrix().code_point())
}

/// The `AVColorRange` for a colour space.
fn range(colour: ColourSpace) -> ffi::AVColorRange {
    match colour.range() {
        crate::config::ColourRange::Limited => ffi::AVCOL_RANGE_MPEG,
        crate::config::ColourRange::Full => ffi::AVCOL_RANGE_JPEG,
    }
}

/// Turns an FFmpeg error code into something a caller can act on.
///
/// The three that mean something specific are recognised; everything else keeps
/// FFmpeg's own number and description, because a code this build has never
/// seen is exactly the one whose text matters (AGENTS.md section 15).
fn failure(operation: &'static str, code: c_int) -> EncodeErrorKind {
    #[allow(clippy::cast_possible_wrap)]
    let enomem = -(ffi::ENOMEM as c_int);
    #[allow(clippy::cast_possible_wrap)]
    let einval = -(ffi::EINVAL as c_int);

    if code == enomem {
        return EncodeErrorKind::OutOfMemory;
    }

    let detail = describe(code);
    if code == einval {
        return EncodeErrorKind::Configuration {
            detail: format!("{operation} refused what it was given: {detail}"),
        };
    }

    EncodeErrorKind::Api {
        operation,
        status: code,
        status_name: name(code),
        detail,
    }
}

/// FFmpeg's own name for the error codes this backend can produce.
fn name(code: c_int) -> &'static str {
    #[allow(clippy::cast_possible_wrap)]
    if code == AVERROR_EOF {
        "AVERROR_EOF"
    } else if code == AVERROR_EAGAIN {
        "AVERROR(EAGAIN)"
    } else if code == -(ffi::ENOMEM as c_int) {
        "AVERROR(ENOMEM)"
    } else if code == -(ffi::EINVAL as c_int) {
        "AVERROR(EINVAL)"
    } else {
        "an FFmpeg error"
    }
}

/// FFmpeg's description of an error code.
fn describe(code: c_int) -> String {
    let mut buffer = [0i8; 128];
    // SAFETY: `buffer` is a live local of `buffer.len()` bytes, which is what
    // the length argument declares; `av_strerror` writes a NUL-terminated
    // string inside it and nothing outside it.
    let written = unsafe { ffi::av_strerror(code, buffer.as_mut_ptr(), buffer.len()) };
    if written < 0 {
        return format!("FFmpeg error {code}");
    }

    // SAFETY: the call above reported success, so the buffer holds a
    // NUL-terminated string within its own bounds.
    unsafe { CStr::from_ptr(buffer.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

/// Whether this backend can produce `codec`.
///
/// H.264 and nothing else. The pinned build also carries `libsvtav1`, so
/// software AV1 is available to a later change
/// ([#157](https://github.com/wildware-uk/clipped/issues/157)); it is not here
/// because AV1 costs a CPU many times what H.264 does, and this backend exists
/// for machines that have no encoding hardware to spare — and because
/// `crate::reference` claims H.264 and only H.264 for the software encoder, so
/// producing AV1 here would make the capability report wrong.
pub(super) const fn produces(codec: Codec) -> bool {
    matches!(codec, Codec::H264)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BitRate, FrameRate};

    fn config(rate_control: RateControl) -> EncoderConfig {
        EncoderConfig::new(
            Codec::H264,
            Resolution::new(2560, 1440),
            FrameRate::FPS_60,
            rate_control,
        )
    }

    #[test]
    fn a_bit_rate_is_passed_through_as_it_was_given() {
        let (average, peak) = rates(&config(RateControl::Bitrate {
            average: BitRate::megabits_per_second(20),
            peak: Some(BitRate::megabits_per_second(30)),
        }));

        assert_eq!(average, 20_000_000);
        assert_eq!(peak, Some(30_000_000));
    }

    #[test]
    fn a_quality_level_becomes_a_bit_rate_because_this_encoder_has_no_other_knob() {
        // The documented anchor: the default level at 1440p60 is about
        // 20 Mbit/s, which is the rate the same footage is given by default on
        // the hardware path.
        let (average, _) = rates(&config(RateControl::quality(QualityTarget::DEFAULT)));
        assert!(
            (19_000_000..=21_000_000).contains(&average),
            "the default quality level came out at {average} bit/s"
        );

        // Six levels better is twice the rate, six levels worse is half.
        let better = QualityTarget::level(QualityTarget::DEFAULT.as_level() - 6)
            .expect("17 is on the scale");
        let worse = QualityTarget::level(QualityTarget::DEFAULT.as_level() + 6)
            .expect("29 is on the scale");
        let (better, _) = rates(&config(RateControl::quality(better)));
        let (worse, _) = rates(&config(RateControl::quality(worse)));

        assert!(
            better > average * 19 / 10 && better < average * 21 / 10,
            "{better}"
        );
        assert!(
            worse > average * 9 / 20 && worse < average * 11 / 20,
            "{worse}"
        );
    }

    #[test]
    fn a_ceiling_is_a_ceiling_even_when_the_quality_asked_for_costs_more() {
        let (average, peak) = rates(&config(RateControl::Quality {
            target: QualityTarget::BEST,
            ceiling: Some(BitRate::megabits_per_second(8)),
        }));

        assert_eq!(
            average, 8_000_000,
            "the ceiling has to bind the average too"
        );
        assert_eq!(peak, Some(8_000_000));
    }

    #[test]
    fn a_derived_rate_stays_inside_what_a_cpu_can_write() {
        // 8K at 240 fps at the best quality on the scale is an absurd request,
        // and the answer to it is a large number rather than an overflow.
        let absurd = EncoderConfig::new(
            Codec::H264,
            Resolution::new(7680, 4320),
            FrameRate::whole(240),
            RateControl::quality(QualityTarget::BEST),
        );
        let (average, _) = rates(&absurd);
        assert_eq!(average, MAXIMUM_DERIVED_RATE as i64);

        let tiny = EncoderConfig::new(
            Codec::H264,
            Resolution::new(64, 64),
            FrameRate::whole(1),
            RateControl::quality(QualityTarget::WORST),
        );
        let (average, _) = rates(&tiny);
        assert_eq!(average, MINIMUM_DERIVED_RATE as i64);
    }

    #[test]
    fn a_keyframe_interval_of_never_is_a_gop_no_recording_reaches() {
        assert_eq!(gop_size(KeyframeInterval::Never), c_int::MAX);
        assert_eq!(
            gop_size(KeyframeInterval::every(
                Duration::from_secs(2),
                FrameRate::FPS_60
            )),
            120
        );
    }

    #[test]
    fn the_slice_count_never_takes_the_whole_machine() {
        let threads = threads();
        assert!(
            (1..=c_int::try_from(MAX_SLICES).expect("four fits in an int")).contains(&threads),
            "the software encoder asked for {threads} threads"
        );
    }

    #[test]
    fn a_timestamp_survives_the_trip_through_the_time_base_exactly() {
        // A capture timestamp is neither a whole number of frames nor a whole
        // number of microseconds: a sixtieth of a second is 16 666 666.67 ns.
        // This is what stops the encoder rounding it into either.
        for awkward in [
            Duration::from_nanos(16_666_666),
            Duration::from_nanos(116_666_666),
            Duration::from_secs(3600) + Duration::from_nanos(1),
        ] {
            assert_eq!(duration(timestamp(awkward)), awkward);
        }
    }

    #[test]
    fn parameter_sets_are_only_prepended_when_they_are_missing() {
        let sets = [0u8, 0, 0, 1, 0x67, 0x42];
        assert!(starts_with_parameter_sets(
            &[0, 0, 0, 1, 0x67, 0x42, 9],
            &sets
        ));
        assert!(!starts_with_parameter_sets(&[0, 0, 0, 1, 0x65], &sets));
        assert!(!starts_with_parameter_sets(&[0, 0, 0, 1, 0x65], &[]));
    }

    #[test]
    fn this_backend_produces_h264_and_says_so_about_the_rest() {
        assert!(produces(Codec::H264));
        assert!(!produces(Codec::Hevc));
        assert!(!produces(Codec::Av1));
    }
}
