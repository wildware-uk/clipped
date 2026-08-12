//! Reading one frame of a finished recording and turning it into a JPEG.
//!
//! # What this does, and what it deliberately does not
//!
//! It opens a file that has already been written, seeks to a few places in it,
//! decodes a handful of frames, keeps the best of them ([`super::choose`]),
//! scales it down and encodes it as a JPEG. It opens no capture device, no
//! encoder session and no GPU adapter; it never touches a recording that is
//! still being written. That is what makes thumbnail generation safe to run
//! beside a game — provided it is also kept out of the way, which is
//! [`super::service`]'s job rather than this module's.
//!
//! It is deliberately **not** the muxer. `clipped-muxer` owns the safe wrappers
//! over the container API and sits at layer 2, above this crate, so depending on
//! it would invert the dependency direction the workspace asserts
//! (README, "Dependency direction"). ADR 0004 permits naming the binding
//! directly in exactly that case, and `crates/encoder` and `crates/waveform` are
//! the other two places that do.
//!
//! # Ownership
//!
//! Five FFmpeg resources are held here, and each has exactly one owner.
//!
//! | Resource | Owner | Released by |
//! | --- | --- | --- |
//! | `AVFormatContext` | [`Demuxer`] | its `Drop`, with `avformat_close_input` |
//! | `AVCodecContext` (decoder and encoder) | [`Decoder`], [`JpegEncoder`] | their `Drop` |
//! | `AVFrame` | [`Frame`] | its `Drop` |
//! | `AVPacket` | [`Packet`] | its `Drop` |
//! | `SwsContext` | [`Scaler`] | its `Drop` |
//!
//! Every one of them is null or live and never anything else, so `Drop` is
//! correct on every path out of every constructor, including the ones that fail
//! part way (AGENTS.md section 58).
//!
//! # Why the frame is scaled before it is judged
//!
//! [`super::choose::score`] reads an 8-bit luma plane. A recording's own frames
//! are whatever the decoder produces — 8-bit 4:2:0 usually, 10-bit 4:2:0 for an
//! HDR capture, RGB for a screen recording from elsewhere — and scoring each of
//! those correctly would mean a branch per pixel format. Scaling first costs
//! about a millisecond a frame, removes every one of those branches, and has the
//! useful property that what is judged is exactly the picture that becomes the
//! thumbnail.

use core::ffi::{c_char, c_int, CStr};
use core::ptr;
use core::time::Duration;
use std::ffi::CString;
use std::path::Path;

use rusty_ffmpeg::ffi;
use tracing::debug;

use super::choose::{candidate_offsets, score, BLANK, FRAMES_PER_CANDIDATE, GOOD_ENOUGH};
use super::source::SourceIdentity;
use super::ThumbnailError;

/// FFmpeg's `AVERROR_EOF`, which is `FFERRTAG('E','O','F',' ')`.
///
/// The binding does not carry it: it is a macro over another macro, and
/// `bindgen` expands neither. Spelled the same way `crates/encoder` and
/// `crates/waveform` spell it.
const AVERROR_EOF: c_int = -0x20_46_4F_45;

/// FFmpeg's `AVERROR(EAGAIN)`: more input is needed before there is output.
#[allow(clippy::cast_possible_wrap)]
const AVERROR_EAGAIN: c_int = -(ffi::EAGAIN as c_int);

/// FFmpeg's `AV_NOPTS_VALUE`, the timestamp that means "there isn't one".
///
/// Written out rather than taken from the binding because `bindgen` renders it
/// as an unsigned constant, and comparing it against a signed `pts` would be a
/// cast at every use.
const NO_TIMESTAMP: i64 = i64::MIN;

/// `SWS_BILINEAR`, the scaling filter.
///
/// The same one `crates/encoder` uses, and for the same reason: it averages the
/// source pixels behind each destination one, where `SWS_POINT` would keep one
/// and discard the rest. A thumbnail is a large downscale, so point sampling
/// would alias thin bright detail — a game's interface — into noise.
const SWS_BILINEAR: c_int = 2;

/// How many container packets are read between two [`Pace::checkpoint`] calls.
///
/// Small enough that suspending generation takes effect immediately, large
/// enough that the check is not a measurable part of the work. Most thumbnails
/// never reach it: a candidate is a seek and a frame or two.
const PACKETS_PER_CHECKPOINT: u32 = 32;

/// How many packets are read at one candidate before it is given up on.
///
/// A seek lands on a keyframe, so the first packet of the stream after it
/// normally decodes to a picture. The bound is what stops a container whose
/// index is wrong from turning one candidate into a full read of the file.
const PACKETS_PER_CANDIDATE: u32 = 512;

/// Whether generation should carry on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Continue {
    /// Carry on.
    Yes,
    /// Stop, and report [`ThumbnailError::Cancelled`].
    Stop,
}

/// What the renderer asks, periodically, before reading more of the file.
///
/// This is the hook that keeps thumbnail generation out of a game's way. An
/// implementation may block — that is the point:
/// [`ThumbnailService`](super::ThumbnailService)'s blocks for as long as a
/// recording is running, so a library scan that started before a game launched
/// stops within a few packets and resumes when the recording ends, rather than
/// being abandoned or running through it.
pub trait Pace: Send + Sync {
    /// Called every so often while reading. May block; may ask for a stop.
    fn checkpoint(&self) -> Continue;
}

/// The pace of a caller with nothing better to do, which never waits and never
/// stops.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unpaced;

impl Pace for Unpaced {
    fn checkpoint(&self) -> Continue {
        Continue::Yes
    }
}

/// How wide a thumbnail is, in pixels, unless the caller says otherwise.
///
/// Six hundred and forty. A library tile is drawn at about 320 logical pixels
/// (`docs/desktop-ui.md`), which is 640 device pixels on the 200%-scaled
/// displays most gaming machines run, so this is the smallest size that is sharp
/// where a thumbnail is actually shown. `docs/thumbnails.md` records what the
/// alternatives cost and why one stored size is enough.
pub const DEFAULT_WIDTH: u32 = 640;

/// The JPEG quantiser scale a thumbnail is encoded at, unless the caller says
/// otherwise.
///
/// MJPEG's scale runs from 1 (largest, best) to 31 (smallest, worst). Four is
/// visually indistinguishable from the source at this size and lands around
/// 40 kB a picture; `docs/thumbnails.md` has the measured sizes across the
/// range.
pub const DEFAULT_QUALITY: u32 = 4;

/// The narrowest and widest thumbnail that may be asked for.
///
/// The lower bound is a picture too small to recognise anything in; the upper is
/// where a "thumbnail" has become a copy of the frame and the cache budget
/// stops meaning anything.
const WIDTH_BOUNDS: core::ops::RangeInclusive<u32> = 64..=1_920;

/// How a thumbnail is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThumbnailOptions {
    width: u32,
    quality: u32,
}

impl ThumbnailOptions {
    /// [`DEFAULT_WIDTH`] at [`DEFAULT_QUALITY`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            width: DEFAULT_WIDTH,
            quality: DEFAULT_QUALITY,
        }
    }

    /// How wide the picture should be, clamped to what this module will make.
    ///
    /// The height follows from the recording's aspect ratio, and a recording
    /// narrower than this is never scaled *up*: a thumbnail is a smaller copy of
    /// a frame, and enlarging one only makes a soft picture and a larger file.
    #[must_use]
    pub fn with_width(mut self, width: u32) -> Self {
        self.width = width.clamp(*WIDTH_BOUNDS.start(), *WIDTH_BOUNDS.end());
        self
    }

    /// The JPEG quantiser scale, 1 (best) to 31 (worst), clamped.
    #[must_use]
    pub fn with_quality(mut self, quality: u32) -> Self {
        self.quality = quality.clamp(1, 31);
        self
    }

    /// How wide a picture this makes.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// The quantiser scale this encodes at.
    #[must_use]
    pub const fn quality(&self) -> u32 {
        self.quality
    }
}

impl Default for ThumbnailOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// A JPEG made from one frame of a recording, before it is written anywhere.
#[derive(Debug, Clone)]
pub struct RenderedThumbnail {
    source: SourceIdentity,
    jpeg: Vec<u8>,
    width: u32,
    height: u32,
    at: Duration,
    score: f32,
}

impl RenderedThumbnail {
    /// The recording this frame came from, as it was when the frame was taken.
    #[must_use]
    pub fn source(&self) -> &SourceIdentity {
        &self.source
    }

    /// The encoded JPEG.
    #[must_use]
    pub fn jpeg(&self) -> &[u8] {
        &self.jpeg
    }

    /// How wide the picture is, in pixels.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// How tall the picture is, in pixels.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// How far into the recording the frame was taken from.
    #[must_use]
    pub fn at(&self) -> Duration {
        self.at
    }

    /// How the chosen frame scored ([`super::choose::score`]).
    ///
    /// Below [`BLANK`] means every candidate looked at was a flat colour, and
    /// the picture is a black or white rectangle. It is still stored — a tile
    /// with a dark picture is closer to the truth than a tile with none — and
    /// the number is here so that a caller can say so.
    #[must_use]
    pub fn score(&self) -> f32 {
        self.score
    }

    /// Whether every candidate frame was a flat colour.
    #[must_use]
    pub fn is_blank(&self) -> bool {
        self.score < BLANK
    }
}

/// Makes a thumbnail of the recording at `path`.
///
/// # Errors
///
/// When the file cannot be stat-ed or opened, when FFmpeg cannot make sense of
/// the container, when there is no video stream in it, or when no frame could be
/// decoded at all.
pub fn render(
    path: impl AsRef<Path>,
    options: ThumbnailOptions,
) -> Result<RenderedThumbnail, ThumbnailError> {
    render_paced(path, options, &Unpaced)
}

/// [`render`], asking `pace` whether to carry on as it reads.
///
/// # Errors
///
/// As [`render`], and [`ThumbnailError::Cancelled`] when `pace` asks for a stop.
pub fn render_paced(
    path: impl AsRef<Path>,
    options: ThumbnailOptions,
    pace: &dyn Pace,
) -> Result<RenderedThumbnail, ThumbnailError> {
    let path = path.as_ref();
    // Read first, so that a missing file is reported as the missing file it is
    // rather than as whatever libavformat says about it, and so that the
    // identity stored with the picture is the one the picture was taken from.
    let source = SourceIdentity::of(path).map_err(|cause| ThumbnailError::Unreadable {
        path: clipped_logging::RedactedPath::new(path),
        cause,
    })?;

    let mut demuxer = Demuxer::open(path)?;
    let stream = demuxer.best_video_stream(path)?;
    let mut decoder = Decoder::open(path, stream.parameters, stream.time_base)?;

    let mut search = Search::new(path, options);
    let mut packet = Packet::allocate(path)?;

    for offset in candidate_offsets(demuxer.duration()) {
        if pace.checkpoint() == Continue::Stop {
            return Err(ThumbnailError::Cancelled);
        }
        if offset > Duration::ZERO {
            demuxer.seek(&stream, offset);
            decoder.flush_buffers();
        }
        search.at_candidate(&mut demuxer, &mut decoder, &mut packet, &stream, pace)?;
        if search.is_satisfied() {
            break;
        }
    }

    search.finish(source, &decoder, &stream)
}

/// The best frame seen so far, and the machinery for looking at more.
struct Search<'a> {
    path: &'a Path,
    options: ThumbnailOptions,
    scaler: Option<Scaler>,
    /// The frame being looked at, scaled to thumbnail size.
    candidate: Option<Frame>,
    /// The best frame seen so far, scaled to thumbnail size.
    best: Option<Frame>,
    best_score: f32,
    best_timestamp: i64,
    /// Frames that were decoded but could not be scaled or scored, so that
    /// "nothing was decodable" and "nothing was worth keeping" are different
    /// answers.
    decoded: u32,
}

impl<'a> Search<'a> {
    fn new(path: &'a Path, options: ThumbnailOptions) -> Self {
        Self {
            path,
            options,
            scaler: None,
            candidate: None,
            best: None,
            best_score: f32::MIN,
            best_timestamp: NO_TIMESTAMP,
            decoded: 0,
        }
    }

    /// Whether the best frame so far is good enough to stop looking.
    fn is_satisfied(&self) -> bool {
        self.best_score >= GOOD_ENOUGH
    }

    /// Reads and scores frames from wherever the demuxer is now.
    fn at_candidate(
        &mut self,
        demuxer: &mut Demuxer,
        decoder: &mut Decoder,
        packet: &mut Packet,
        stream: &VideoStream,
        pace: &dyn Pace,
    ) -> Result<(), ThumbnailError> {
        let mut packets = 0u32;
        let mut frames = 0u32;

        while packets < PACKETS_PER_CANDIDATE && frames < FRAMES_PER_CANDIDATE {
            packets += 1;
            if packets % PACKETS_PER_CHECKPOINT == 0 && pace.checkpoint() == Continue::Stop {
                return Err(ThumbnailError::Cancelled);
            }

            match demuxer.read_into(packet) {
                Read::Packet(index) if index == stream.index => {
                    decoder.send(packet.raw());
                    packet.unreference();
                }
                Read::Packet(_) => {
                    packet.unreference();
                    continue;
                }
                Read::EndOfFile => {
                    // Tell the decoder so, and take what it was still holding.
                    // A short recording reaches this on its first candidate.
                    decoder.send(ptr::null_mut());
                    self.take_frames(decoder, FRAMES_PER_CANDIDATE - frames)?;
                    return Ok(());
                }
                Read::Failed(code) => {
                    // A damaged container, or one truncated by a crash. What has
                    // already been decoded is still a frame of the recording, so
                    // this candidate ends rather than the whole attempt.
                    debug!(
                        error = %describe(code),
                        "reading a packet failed; this thumbnail candidate ends here"
                    );
                    return Ok(());
                }
            }

            frames += self.take_frames(decoder, FRAMES_PER_CANDIDATE - frames)?;
            if self.is_satisfied() {
                return Ok(());
            }
        }
        Ok(())
    }

    /// Takes up to `wanted` frames from the decoder, scoring each.
    fn take_frames(&mut self, decoder: &mut Decoder, wanted: u32) -> Result<u32, ThumbnailError> {
        let mut taken = 0;
        while taken < wanted {
            match decoder.receive() {
                Received::Frame => {}
                Received::More | Received::EndOfStream => break,
                Received::Failed(code) => {
                    debug!(error = %describe(code), "a decoded frame could not be read");
                    break;
                }
            }
            taken += 1;
            self.decoded += 1;
            self.consider(decoder)?;
            decoder.unreference_frame();
            if self.is_satisfied() {
                break;
            }
        }
        Ok(taken)
    }

    /// Scales the decoder's current frame and keeps it if it is the best so far.
    fn consider(&mut self, decoder: &Decoder) -> Result<(), ThumbnailError> {
        let source = decoder.frame_geometry();
        // The first frame, or a stream that changed resolution mid-file — which
        // a recording of a game that changed its display mode does. Both are
        // answered by building the converter the frames in hand actually need,
        // and by starting the comparison again against pictures of the new size.
        if !self
            .scaler
            .as_ref()
            .is_some_and(|scaler| scaler.accepts(&source))
        {
            let target = target_size(&source, self.options.width());
            self.candidate = Some(Frame::for_picture(self.path, target)?);
            self.best = Some(Frame::for_picture(self.path, target)?);
            self.best_score = f32::MIN;
            self.scaler = Some(Scaler::open(self.path, &source, target)?);
        }

        let (Some(scaler), Some(candidate), Some(best)) = (
            self.scaler.as_mut(),
            self.candidate.as_mut(),
            self.best.as_mut(),
        ) else {
            // All three are set together, immediately above.
            return Ok(());
        };

        // SAFETY: the decoder's frame is live and was just filled in, and the
        // candidate is a live picture of the size this scaler was built for.
        unsafe { scaler.scale(decoder.frame(), candidate.raw()) }.map_err(|detail| {
            ThumbnailError::Undecodable {
                path: clipped_logging::RedactedPath::new(self.path),
                detail,
            }
        })?;

        let scored = candidate.score_luma();
        if scored > self.best_score {
            self.best_score = scored;
            self.best_timestamp = decoder.frame_timestamp();
            core::mem::swap(candidate, best);
        }
        Ok(())
    }

    /// Encodes the best frame found, or reports that there was not one.
    fn finish(
        mut self,
        source: SourceIdentity,
        decoder: &Decoder,
        stream: &VideoStream,
    ) -> Result<RenderedThumbnail, ThumbnailError> {
        let redacted = clipped_logging::RedactedPath::new(self.path);
        let Some(best) = self.best.take().filter(|_| self.best_score > f32::MIN) else {
            return Err(ThumbnailError::Undecodable {
                path: redacted,
                detail: format!(
                    "no frame could be decoded from the video stream ({} were offered to the \
                     decoder)",
                    self.decoded
                ),
            });
        };

        let (width, height) = best.size();
        let jpeg = JpegEncoder::open(self.path, width, height, self.options.quality())?
            .encode(self.path, best.raw())?;
        let at = seconds_of(self.best_timestamp, stream);

        debug!(
            recording = %redacted,
            at_seconds = at.as_secs_f64(),
            score = self.best_score,
            width,
            height,
            bytes = jpeg.len(),
            decoded = self.decoded,
            codec = decoder.codec_name(),
            "chose a frame for a thumbnail"
        );

        Ok(RenderedThumbnail {
            source,
            jpeg,
            width,
            height,
            at,
            score: self.best_score,
        })
    }
}

/// Where in the recording a timestamp is.
///
/// Zero when the frame carried no timestamp, which is what a container with no
/// timing information produces; the picture is still the picture.
fn seconds_of(timestamp: i64, stream: &VideoStream) -> Duration {
    if timestamp == NO_TIMESTAMP || stream.time_base.num <= 0 || stream.time_base.den <= 0 {
        return Duration::ZERO;
    }
    let ticks = timestamp.saturating_sub(if stream.start_time == NO_TIMESTAMP {
        0
    } else {
        stream.start_time
    });
    if ticks <= 0 {
        return Duration::ZERO;
    }
    let seconds = ticks as f64 * f64::from(stream.time_base.num) / f64::from(stream.time_base.den);
    Duration::try_from_secs_f64(seconds).unwrap_or(Duration::ZERO)
}

/// The size of a decoded picture, and its format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PictureFormat {
    format: c_int,
    width: c_int,
    height: c_int,
    full_range: bool,
    colorspace: c_int,
}

/// How large a thumbnail of `source` should be, at most `width` wide.
///
/// The aspect ratio is the picture's own. Both dimensions are even, because
/// 4:2:0 chroma is shared between pairs of pixels and an odd dimension is a
/// half-populated chroma row. Never larger than the source, in either axis.
fn target_size(source: &PictureFormat, width: u32) -> (u32, u32) {
    let source_width = u32::try_from(source.width).unwrap_or(0).max(2);
    let source_height = u32::try_from(source.height).unwrap_or(0).max(2);
    let scaled_width = width.min(source_width);
    let scaled_height =
        (u64::from(scaled_width) * u64::from(source_height) / u64::from(source_width)) as u32;
    (even(scaled_width), even(scaled_height.min(source_height)))
}

/// `value` rounded down to an even number, and never below two.
fn even(value: u32) -> u32 {
    (value & !1).max(2)
}

/// One video stream of a container.
#[derive(Debug, Clone, Copy)]
struct VideoStream {
    index: u32,
    raw_index: c_int,
    parameters: *const ffi::AVCodecParameters,
    time_base: ffi::AVRational,
    start_time: i64,
}

/// An open container.
struct Demuxer {
    context: *mut ffi::AVFormatContext,
}

impl Demuxer {
    /// Opens `path` and reads enough of it to describe its streams.
    fn open(path: &Path) -> Result<Self, ThumbnailError> {
        let redacted = clipped_logging::RedactedPath::new(path);
        let text = CString::new(path.to_string_lossy().into_owned()).map_err(|_| {
            ThumbnailError::Undecodable {
                path: redacted.clone(),
                detail: "the path contains a NUL byte, which no file has".to_owned(),
            }
        })?;

        let mut context: *mut ffi::AVFormatContext = ptr::null_mut();
        // SAFETY: `context` is a live local initialised to null, which
        // `avformat_open_input` documents as "allocate one". `text` is a
        // NUL-terminated string that outlives the call, and the two option
        // arguments are documented as nullable.
        let code = unsafe {
            ffi::avformat_open_input(
                &raw mut context,
                text.as_ptr(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if code < 0 {
            // On failure `avformat_open_input` frees the context and nulls the
            // pointer, so there is nothing to release here.
            return Err(ThumbnailError::Undecodable {
                path: redacted,
                detail: format!("the container could not be opened: {}", describe(code)),
            });
        }

        let demuxer = Self { context };
        // SAFETY: the context is live and exclusively owned; the options
        // argument is documented as nullable.
        let code = unsafe { ffi::avformat_find_stream_info(demuxer.context, ptr::null_mut()) };
        if code < 0 {
            return Err(ThumbnailError::Undecodable {
                path: redacted,
                detail: format!(
                    "the container's streams could not be read: {}",
                    describe(code)
                ),
            });
        }
        Ok(demuxer)
    }

    /// How long the container says it is, where it says.
    fn duration(&self) -> Option<Duration> {
        // SAFETY: the context is live; `duration` is in `AV_TIME_BASE` units and
        // is `AV_NOPTS_VALUE` or negative when the container does not say.
        let micros = unsafe { (*self.context).duration };
        if micros == NO_TIMESTAMP || micros <= 0 {
            return None;
        }
        Some(Duration::from_micros(micros.unsigned_abs()))
    }

    /// The video stream a viewer would play.
    fn best_video_stream(&mut self, path: &Path) -> Result<VideoStream, ThumbnailError> {
        // SAFETY: the context is live; a null decoder-out argument is
        // documented as "do not report the decoder", and the flags argument is
        // documented as reserved and passed as zero.
        let index = unsafe {
            ffi::av_find_best_stream(
                self.context,
                ffi::AVMEDIA_TYPE_VIDEO,
                -1,
                -1,
                ptr::null_mut(),
                0,
            )
        };
        if index < 0 {
            return Err(ThumbnailError::NoVideo {
                path: clipped_logging::RedactedPath::new(path),
            });
        }

        // SAFETY: `av_find_best_stream` returned an index below `nb_streams`,
        // and every entry of `streams` up to that is a live stream owned by the
        // context, which outlives every value derived from it here.
        let stream = unsafe { *(*self.context).streams.add(index as usize) };
        // SAFETY: a live stream always has parameters, a time base and a start
        // time.
        let (parameters, time_base, start_time) = unsafe {
            (
                (*stream).codecpar,
                (*stream).time_base,
                (*stream).start_time,
            )
        };

        Ok(VideoStream {
            index: index.unsigned_abs(),
            raw_index: index,
            parameters,
            time_base,
            start_time,
        })
    }

    /// Moves to the keyframe at or before `offset`.
    ///
    /// A failure is not reported: a container with no index — a recording
    /// truncated by a power cut, which AGENTS.md section 16 says to expect —
    /// simply carries on reading from wherever it already was, and the frame
    /// that produces is still a frame of the recording.
    fn seek(&mut self, stream: &VideoStream, offset: Duration) {
        let micros = i64::try_from(offset.as_micros()).unwrap_or(i64::MAX);
        let time_base = ffi::AVRational {
            num: 1,
            den: 1_000_000,
        };
        // SAFETY: both rationals are live locals with positive denominators.
        let timestamp = unsafe { ffi::av_rescale_q(micros, time_base, stream.time_base) };
        // SAFETY: the context is live and the stream index came from it.
        // `AVSEEK_FLAG_BACKWARD` asks for the keyframe at or before the
        // timestamp, which is the one that can be decoded without reading what
        // came before it.
        let code = unsafe {
            ffi::av_seek_frame(
                self.context,
                stream.raw_index,
                timestamp,
                ffi::AVSEEK_FLAG_BACKWARD as c_int,
            )
        };
        if code < 0 {
            debug!(
                offset_seconds = offset.as_secs_f64(),
                error = %describe(code),
                "a thumbnail candidate could not be seeked to; reading on from here instead"
            );
        }
    }

    /// Reads the next packet of the container into `packet`.
    fn read_into(&mut self, packet: &mut Packet) -> Read {
        // SAFETY: both the context and the packet are live and exclusively
        // owned. On success the packet references data the caller must
        // unreference, which `Packet::unreference` does.
        let code = unsafe { ffi::av_read_frame(self.context, packet.raw()) };
        if code == AVERROR_EOF {
            return Read::EndOfFile;
        }
        if code < 0 {
            return Read::Failed(code);
        }
        // SAFETY: the packet is live and was just filled in.
        Read::Packet(unsafe { (*packet.raw()).stream_index.unsigned_abs() })
    }
}

impl Drop for Demuxer {
    fn drop(&mut self) {
        if !self.context.is_null() {
            // SAFETY: allocated by `avformat_open_input`, not used again; the
            // call also nulls the pointer.
            unsafe { ffi::avformat_close_input(&raw mut self.context) };
        }
    }
}

/// What one call to [`Demuxer::read_into`] produced.
enum Read {
    /// A packet, belonging to this stream.
    Packet(u32),
    /// There are no more.
    EndOfFile,
    /// FFmpeg reported an error.
    Failed(c_int),
}

/// What one call to [`Decoder::receive`] produced.
enum Received {
    /// A picture, now in the decoder's frame.
    Frame,
    /// The decoder wants another packet first.
    More,
    /// The decoder has been drained.
    EndOfStream,
    /// FFmpeg reported an error.
    Failed(c_int),
}

/// A reusable packet.
struct Packet {
    raw: *mut ffi::AVPacket,
}

impl Packet {
    fn allocate(path: &Path) -> Result<Self, ThumbnailError> {
        // SAFETY: allocates a packet, or returns null.
        let raw = unsafe { ffi::av_packet_alloc() };
        if raw.is_null() {
            return Err(ThumbnailError::Undecodable {
                path: clipped_logging::RedactedPath::new(path),
                detail: "a packet could not be allocated".to_owned(),
            });
        }
        Ok(Self { raw })
    }

    fn raw(&mut self) -> *mut ffi::AVPacket {
        self.raw
    }

    /// Releases the data the last read attached, leaving the packet reusable.
    fn unreference(&mut self) {
        // SAFETY: the packet is live and exclusively owned; `av_packet_unref` is
        // documented as safe on a packet holding nothing.
        unsafe { ffi::av_packet_unref(self.raw) };
    }
}

impl Drop for Packet {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            // SAFETY: allocated by `av_packet_alloc` and not used again.
            unsafe { ffi::av_packet_free(&raw mut self.raw) };
        }
    }
}

/// An `AVFrame` and, where it owns one, its picture buffer.
struct Frame {
    raw: *mut ffi::AVFrame,
}

impl Frame {
    /// An empty frame, for a decoder to fill in.
    fn empty(path: &Path) -> Result<Self, ThumbnailError> {
        // SAFETY: allocates a frame, or returns null.
        let raw = unsafe { ffi::av_frame_alloc() };
        if raw.is_null() {
            return Err(ThumbnailError::Undecodable {
                path: clipped_logging::RedactedPath::new(path),
                detail: "a frame could not be allocated".to_owned(),
            });
        }
        Ok(Self { raw })
    }

    /// A frame owning a writable 8-bit 4:2:0 picture of the given size.
    fn for_picture(path: &Path, (width, height): (u32, u32)) -> Result<Self, ThumbnailError> {
        let frame = Self::empty(path)?;
        // SAFETY: the frame is live, exclusively owned and holds no buffer yet.
        // These four fields are what `av_frame_get_buffer` reads to decide what
        // to allocate.
        unsafe {
            (*frame.raw).format = ffi::AV_PIX_FMT_YUV420P;
            (*frame.raw).width = c_int::try_from(width).unwrap_or(2);
            (*frame.raw).height = c_int::try_from(height).unwrap_or(2);
            // A JPEG's samples are full range, and the encoder is told the same
            // thing. The two disagreeing is a picture that plays back washed
            // out with nothing reporting a problem (AGENTS.md section 22).
            (*frame.raw).color_range = ffi::AVCOL_RANGE_JPEG;
        }
        // SAFETY: the frame is live and describes a picture; zero asks
        // libavutil for its own default alignment.
        let code = unsafe { ffi::av_frame_get_buffer(frame.raw, 0) };
        if code < 0 {
            return Err(ThumbnailError::Undecodable {
                path: clipped_logging::RedactedPath::new(path),
                detail: format!(
                    "a {width}x{height} picture could not be allocated: {}",
                    describe(code)
                ),
            });
        }
        Ok(frame)
    }

    fn raw(&self) -> *mut ffi::AVFrame {
        self.raw
    }

    /// The picture's size.
    fn size(&self) -> (u32, u32) {
        // SAFETY: the frame is live.
        unsafe {
            (
                (*self.raw).width.unsigned_abs(),
                (*self.raw).height.unsigned_abs(),
            )
        }
    }

    /// How much variety there is in this picture ([`super::choose::score`]).
    fn score_luma(&self) -> f32 {
        // SAFETY: the frame is live and holds a YUV 4:2:0 picture, so plane 0 is
        // one byte of luma per pixel with `linesize[0]` bytes a row.
        let (plane, stride, width, height) = unsafe {
            (
                (*self.raw).data[0],
                (*self.raw).linesize[0],
                (*self.raw).width,
                (*self.raw).height,
            )
        };
        let (Ok(stride), Ok(width), Ok(height)) = (
            usize::try_from(stride),
            usize::try_from(width),
            usize::try_from(height),
        ) else {
            return 0.0;
        };
        if plane.is_null() || stride == 0 || height == 0 {
            return 0.0;
        }
        // SAFETY: `av_frame_get_buffer` allocated `stride` bytes for each of
        // `height` rows of plane 0, and nothing has freed it: this frame owns
        // the buffer for its whole life. The slice is read and not held.
        let luma = unsafe { core::slice::from_raw_parts(plane, stride * height) };
        score(luma, stride, width, height)
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            // SAFETY: allocated by `av_frame_alloc` and not used again;
            // `av_frame_free` releases the buffer with it.
            unsafe { ffi::av_frame_free(&raw mut self.raw) };
        }
    }
}

/// The video decoder, and the frame it decodes into.
struct Decoder {
    context: *mut ffi::AVCodecContext,
    frame: Frame,
    codec_name: String,
}

impl Decoder {
    /// Opens a decoder for one video stream.
    fn open(
        path: &Path,
        parameters: *const ffi::AVCodecParameters,
        time_base: ffi::AVRational,
    ) -> Result<Self, ThumbnailError> {
        let redacted = clipped_logging::RedactedPath::new(path);
        // SAFETY: a live stream always has parameters.
        let codec_id = unsafe { (*parameters).codec_id };
        // SAFETY: reads a static table inside libavcodec and returns null or a
        // pointer into it.
        let codec = unsafe { ffi::avcodec_find_decoder(codec_id) };
        if codec.is_null() {
            return Err(ThumbnailError::Undecodable {
                path: redacted,
                detail: format!("this build has no decoder for video codec {codec_id}"),
            });
        }
        // SAFETY: `codec` is a live descriptor with a NUL-terminated name.
        let codec_name = unsafe { CStr::from_ptr((*codec).name) }
            .to_string_lossy()
            .into_owned();

        // SAFETY: `codec` is a live descriptor; this returns null or a context
        // this value owns from here on.
        let context = unsafe { ffi::avcodec_alloc_context3(codec) };
        if context.is_null() {
            return Err(ThumbnailError::Undecodable {
                path: redacted,
                detail: "a decoder context could not be allocated".to_owned(),
            });
        }
        let decoder = Self {
            context,
            frame: Frame::empty(path)?,
            codec_name,
        };

        // SAFETY: the context is live, exclusively owned and not yet open, and
        // the parameters belong to the stream it is being opened for.
        let code = unsafe { ffi::avcodec_parameters_to_context(decoder.context, parameters) };
        if code < 0 {
            return Err(ThumbnailError::Undecodable {
                path: redacted,
                detail: format!("the decoder could not be configured: {}", describe(code)),
            });
        }
        // SAFETY: the context is live and not yet open. `pkt_timebase` is
        // documented as the caller's to set, and some decoders need it to
        // produce sensible frame timestamps.
        unsafe { (*decoder.context).pkt_timebase = time_base };

        // SAFETY: the context is live and configured; the options argument is
        // documented as nullable.
        let code = unsafe { ffi::avcodec_open2(decoder.context, codec, ptr::null_mut()) };
        if code < 0 {
            return Err(ThumbnailError::Undecodable {
                path: redacted,
                detail: format!("the decoder could not be opened: {}", describe(code)),
            });
        }
        Ok(decoder)
    }

    /// What libavcodec calls this decoder, for a log line.
    fn codec_name(&self) -> &str {
        &self.codec_name
    }

    /// Offers a packet, or a null one to say the stream has ended.
    ///
    /// A refusal is not reported: a damaged packet is skipped, and the search
    /// carries on with the frames that do decode. Every path out of this module
    /// still ends in either a picture or an error naming how many frames were
    /// offered.
    fn send(&mut self, packet: *mut ffi::AVPacket) {
        // SAFETY: the context is live and open. A null packet is the documented
        // way to signal the end of the stream, and libavcodec copies what it
        // needs from a non-null one.
        let code = unsafe { ffi::avcodec_send_packet(self.context, packet) };
        if code < 0 && code != AVERROR_EOF && code != AVERROR_EAGAIN {
            debug!(error = %describe(code), "a video packet could not be decoded and was skipped");
        }
    }

    /// Takes one frame from the decoder, if it has one.
    fn receive(&mut self) -> Received {
        // SAFETY: both the context and the frame are live and exclusively
        // owned. On success the frame holds a reference the caller releases
        // with `unreference_frame` before asking for another.
        let code = unsafe { ffi::avcodec_receive_frame(self.context, self.frame.raw()) };
        match code {
            0 => Received::Frame,
            AVERROR_EAGAIN => Received::More,
            AVERROR_EOF => Received::EndOfStream,
            code => Received::Failed(code),
        }
    }

    /// Releases the picture the last [`receive`](Self::receive) attached.
    fn unreference_frame(&mut self) {
        // SAFETY: the frame is live; unreferencing is required before it is
        // reused and is safe whether or not it holds anything.
        unsafe { ffi::av_frame_unref(self.frame.raw()) };
    }

    /// Throws away everything buffered, which a seek invalidates.
    fn flush_buffers(&mut self) {
        // SAFETY: the context is live and open.
        unsafe { ffi::avcodec_flush_buffers(self.context) };
    }

    fn frame(&self) -> *const ffi::AVFrame {
        self.frame.raw()
    }

    /// The format and size of the frame currently held.
    fn frame_geometry(&self) -> PictureFormat {
        // SAFETY: the frame is live and was just filled in by
        // `avcodec_receive_frame`.
        unsafe {
            PictureFormat {
                format: (*self.frame.raw()).format,
                width: (*self.frame.raw()).width,
                height: (*self.frame.raw()).height,
                full_range: (*self.frame.raw()).color_range == ffi::AVCOL_RANGE_JPEG,
                colorspace: (*self.frame.raw()).colorspace,
            }
        }
    }

    /// The presentation timestamp of the frame currently held.
    fn frame_timestamp(&self) -> i64 {
        // SAFETY: as above.
        unsafe { (*self.frame.raw()).pts }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        if !self.context.is_null() {
            // SAFETY: allocated by `avcodec_alloc_context3` and not used again.
            unsafe { ffi::avcodec_free_context(&raw mut self.context) };
        }
    }
}

/// The downscale from a decoded frame to a thumbnail-sized 4:2:0 picture.
struct Scaler {
    context: *mut ffi::SwsContext,
    source: PictureFormat,
    target: (u32, u32),
}

impl Scaler {
    fn open(
        path: &Path,
        source: &PictureFormat,
        target: (u32, u32),
    ) -> Result<Self, ThumbnailError> {
        // SAFETY: the formats come from a live frame and from the binding, the
        // dimensions are positive, and the three filter arguments are documented
        // as optional and passed as null. Ownership of the context passes to
        // this value, which frees it in `Drop`.
        let context = unsafe {
            ffi::sws_getContext(
                source.width,
                source.height,
                source.format,
                c_int::try_from(target.0).unwrap_or(c_int::MAX),
                c_int::try_from(target.1).unwrap_or(c_int::MAX),
                ffi::AV_PIX_FMT_YUV420P,
                SWS_BILINEAR,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            )
        };
        if context.is_null() {
            return Err(ThumbnailError::Undecodable {
                path: clipped_logging::RedactedPath::new(path),
                detail: format!(
                    "no conversion from pixel format {} at {}x{} to a {}x{} thumbnail",
                    source.format, source.width, source.height, target.0, target.1
                ),
            });
        }

        let scaler = Self {
            context,
            source: *source,
            target,
        };
        scaler.set_colour();
        Ok(scaler)
    }

    /// Tells swscale what the source samples mean and what the destination's
    /// should.
    ///
    /// The destination is a JPEG, whose samples are full range by definition and
    /// which the encoder is separately told is full range. Without this call
    /// swscale assumes limited range on both sides, and every thumbnail comes
    /// out slightly grey with nothing anywhere reporting a problem — the same
    /// silent failure `crates/encoder`'s converter documents.
    ///
    /// A failure is ignored deliberately: `sws_setColorspaceDetails` refuses
    /// RGB sources, for which swscale's own defaults are already right.
    fn set_colour(&self) {
        // SAFETY: `sws_getCoefficients` reads a static table inside swscale for
        // any input and never returns null.
        let coefficients = unsafe { ffi::sws_getCoefficients(self.coefficient_table()) };
        // SAFETY: the context is live, both tables are the static ones swscale
        // just returned, and the last three arguments are the documented
        // neutral values for brightness, contrast and saturation.
        let code = unsafe {
            ffi::sws_setColorspaceDetails(
                self.context,
                coefficients,
                c_int::from(self.source.full_range),
                coefficients,
                1,
                0,
                1 << 16,
                1 << 16,
            )
        };
        if code < 0 {
            debug!(
                format = self.source.format,
                "swscale would not take a colour range for this source; using its defaults"
            );
        }
    }

    /// Which `SWS_CS_*` table describes the source.
    ///
    /// From the frame's own tag where the container carried one. Where it did
    /// not, from the picture's height, which is the same guess every player
    /// makes: standard definition was BT.601 and high definition is BT.709.
    fn coefficient_table(&self) -> c_int {
        /// `SWS_CS_ITU709`.
        const BT709: c_int = 1;
        /// `SWS_CS_ITU601`, which swscale also calls `SWS_CS_DEFAULT`.
        const BT601: c_int = 5;
        /// `SWS_CS_BT2020`.
        const BT2020: c_int = 9;
        /// `AVCOL_SPC_BT709`.
        const AVCOL_SPC_BT709: c_int = 1;
        /// `AVCOL_SPC_BT470BG` and `AVCOL_SPC_SMPTE170M`, the two spellings of
        /// BT.601 a container may carry.
        const AVCOL_SPC_BT470BG: c_int = 5;
        const AVCOL_SPC_SMPTE170M: c_int = 6;
        /// `AVCOL_SPC_BT2020_NCL`.
        const AVCOL_SPC_BT2020_NCL: c_int = 9;

        match self.source.colorspace {
            AVCOL_SPC_BT709 => BT709,
            AVCOL_SPC_BT470BG | AVCOL_SPC_SMPTE170M => BT601,
            AVCOL_SPC_BT2020_NCL => BT2020,
            _ if self.source.height >= 720 => BT709,
            _ => BT601,
        }
    }

    /// Whether this converter was built for frames of this shape.
    fn accepts(&self, source: &PictureFormat) -> bool {
        self.source == *source
    }

    /// Scales one decoded frame into `destination`.
    ///
    /// # Safety
    ///
    /// `frame` must be a live picture of the format and size this converter was
    /// opened for, and `destination` a live writable 4:2:0 picture of the target
    /// size.
    unsafe fn scale(
        &mut self,
        frame: *const ffi::AVFrame,
        destination: *mut ffi::AVFrame,
    ) -> Result<(), String> {
        // SAFETY: the context is live; the source planes and strides are the
        // caller's frame, which the caller guarantees matches what this context
        // was built for; the destination planes are the caller's picture, of the
        // size this context produces. swscale writes only inside those planes.
        let rows = unsafe {
            ffi::sws_scale(
                self.context,
                (*frame).data.as_ptr().cast::<*const u8>(),
                (*frame).linesize.as_ptr(),
                0,
                self.source.height,
                (*destination).data.as_ptr(),
                (*destination).linesize.as_ptr(),
            )
        };
        let expected = c_int::try_from(self.target.1).unwrap_or(c_int::MAX);
        if rows != expected {
            return Err(format!(
                "scaling a frame to {}x{} produced {rows} of {expected} rows",
                self.target.0, self.target.1
            ));
        }
        Ok(())
    }
}

impl Drop for Scaler {
    fn drop(&mut self) {
        // SAFETY: this value owns the context, it is freed exactly once, and
        // `sws_freeContext` accepts null — which the context never is, because
        // `open` fails rather than returning one.
        unsafe { ffi::sws_freeContext(self.context) };
    }
}

/// The still-image encoder a thumbnail is written with.
///
/// JPEG rather than PNG or WebP. A frame of a game is photographic, so PNG is
/// roughly ten times the bytes for a difference nobody can see at this size;
/// WebP and AVIF are smaller still but need encoders this pinned FFmpeg build
/// does not carry, and JPEG is the one format every webview, file manager and
/// image viewer on Windows opens without being asked
/// (`docs/thumbnails.md`, "Format").
struct JpegEncoder {
    context: *mut ffi::AVCodecContext,
}

impl JpegEncoder {
    fn open(path: &Path, width: u32, height: u32, quality: u32) -> Result<Self, ThumbnailError> {
        let redacted = clipped_logging::RedactedPath::new(path);
        // SAFETY: reads a static table inside libavcodec and returns null or a
        // pointer into it.
        let codec = unsafe { ffi::avcodec_find_encoder(ffi::AV_CODEC_ID_MJPEG) };
        if codec.is_null() {
            return Err(ThumbnailError::Undecodable {
                path: redacted,
                detail: "this build has no JPEG encoder".to_owned(),
            });
        }
        // SAFETY: `codec` is a live descriptor; this returns null or a context
        // this value owns from here on.
        let context = unsafe { ffi::avcodec_alloc_context3(codec) };
        if context.is_null() {
            return Err(ThumbnailError::Undecodable {
                path: redacted,
                detail: "a JPEG encoder context could not be allocated".to_owned(),
            });
        }
        let encoder = Self { context };

        // SAFETY: the context is live, exclusively owned and not yet open, and
        // every field written here is one libavcodec documents as the caller's
        // to set before `avcodec_open2`.
        unsafe {
            (*context).width = c_int::try_from(width).unwrap_or(2);
            (*context).height = c_int::try_from(height).unwrap_or(2);
            (*context).pix_fmt = ffi::AV_PIX_FMT_YUV420P;
            // A JPEG's samples are full range. Saying so is what lets the
            // encoder take `AV_PIX_FMT_YUV420P` at all — it refuses limited
            // range as non-standard — and it is the same thing the scaler was
            // told, so the picture and its description agree.
            (*context).color_range = ffi::AVCOL_RANGE_JPEG;
            // A still image, so the time base is arbitrary; one second a frame
            // keeps the numbers small.
            (*context).time_base = ffi::AVRational { num: 1, den: 1 };
            // MJPEG's quality is a quantiser scale rather than a bitrate, and
            // this flag is what makes libavcodec read it.
            (*context).flags |= ffi::AV_CODEC_FLAG_QSCALE as c_int;
            (*context).global_quality =
                c_int::try_from(quality * ffi::FF_QP2LAMBDA).unwrap_or(c_int::MAX);
        }

        // SAFETY: the context is live and configured; the options argument is
        // documented as nullable.
        let code = unsafe { ffi::avcodec_open2(encoder.context, codec, ptr::null_mut()) };
        if code < 0 {
            return Err(ThumbnailError::Undecodable {
                path: redacted,
                detail: format!(
                    "the JPEG encoder would not open for {width}x{height}: {}",
                    describe(code)
                ),
            });
        }
        Ok(encoder)
    }

    /// Encodes one picture and returns the JPEG.
    fn encode(&mut self, path: &Path, frame: *mut ffi::AVFrame) -> Result<Vec<u8>, ThumbnailError> {
        let redacted = clipped_logging::RedactedPath::new(path);
        // SAFETY: the frame is live and owned by the caller. `quality` is what a
        // qscale-mode encoder reads per frame; `global_quality` alone is not
        // enough, and a frame left at zero encodes at the best possible quality
        // and several times the intended size.
        unsafe {
            (*frame).quality = (*self.context).global_quality;
            (*frame).pts = 0;
        }

        // SAFETY: the context is live and open, and the frame is a live picture
        // of the size and format it was opened for.
        let code = unsafe { ffi::avcodec_send_frame(self.context, frame) };
        if code < 0 {
            return Err(ThumbnailError::Undecodable {
                path: redacted,
                detail: format!(
                    "the frame could not be encoded as a JPEG: {}",
                    describe(code)
                ),
            });
        }
        // SAFETY: as above. A null frame is the documented end of the stream,
        // which for a single-picture encode is immediately.
        let _ = unsafe { ffi::avcodec_send_frame(self.context, ptr::null_mut()) };

        let mut packet = Packet::allocate(path)?;
        // SAFETY: the context is live and open, and the packet is live and
        // holds nothing.
        let code = unsafe { ffi::avcodec_receive_packet(self.context, packet.raw()) };
        if code < 0 {
            return Err(ThumbnailError::Undecodable {
                path: redacted,
                detail: format!("the JPEG encoder produced nothing: {}", describe(code)),
            });
        }

        // SAFETY: the packet was just filled in, so `data` holds `size` bytes
        // that live until the packet is unreferenced, which happens after this
        // copy.
        let jpeg = unsafe {
            let raw = packet.raw();
            let size = usize::try_from((*raw).size).unwrap_or(0);
            if (*raw).data.is_null() || size == 0 {
                Vec::new()
            } else {
                core::slice::from_raw_parts((*raw).data, size).to_vec()
            }
        };
        packet.unreference();

        if jpeg.is_empty() {
            return Err(ThumbnailError::Undecodable {
                path: redacted,
                detail: "the JPEG encoder produced an empty picture".to_owned(),
            });
        }
        Ok(jpeg)
    }
}

impl Drop for JpegEncoder {
    fn drop(&mut self) {
        if !self.context.is_null() {
            // SAFETY: allocated by `avcodec_alloc_context3` and not used again.
            unsafe { ffi::avcodec_free_context(&raw mut self.context) };
        }
    }
}

/// What FFmpeg says about an error code.
fn describe(code: c_int) -> String {
    let mut buffer = [0 as c_char; 256];
    // SAFETY: the buffer is live, exclusively owned and exactly as long as the
    // length passed. `av_strerror` writes a NUL-terminated string into it, or
    // leaves it empty and returns non-zero.
    let written = unsafe { ffi::av_strerror(code, buffer.as_mut_ptr(), buffer.len()) };
    if written < 0 {
        return format!("FFmpeg error {code}");
    }
    // SAFETY: the buffer holds a NUL-terminated string written above.
    let text = unsafe { CStr::from_ptr(buffer.as_ptr()) };
    format!("{} ({code})", text.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(width: c_int, height: c_int) -> PictureFormat {
        PictureFormat {
            format: ffi::AV_PIX_FMT_YUV420P,
            width,
            height,
            full_range: false,
            colorspace: 0,
        }
    }

    #[test]
    fn a_thumbnail_keeps_the_recordings_shape() {
        // 16:9, 16:10 and 4:3, all scaled to the same width.
        assert_eq!(target_size(&source(1_920, 1_080), 640), (640, 360));
        assert_eq!(target_size(&source(2_560, 1_600), 640), (640, 400));
        assert_eq!(target_size(&source(1_024, 768), 640), (640, 480));
        // An ultrawide is short rather than cropped: cropping would throw away
        // the part of the picture a player was looking at.
        assert_eq!(target_size(&source(3_440, 1_440), 640), (640, 266));
    }

    #[test]
    fn both_dimensions_are_even_because_chroma_is_shared_between_pixels() {
        // An odd dimension in a 4:2:0 picture is a half-populated chroma row,
        // which encoders refuse or render wrongly.
        for height in 100..140 {
            let (width, scaled) = target_size(&source(1_000, height), 333);
            assert_eq!(width % 2, 0, "{width} is odd");
            assert_eq!(scaled % 2, 0, "{scaled} is odd for a {height}-row source");
        }
    }

    #[test]
    fn a_recording_smaller_than_a_thumbnail_is_never_enlarged() {
        // Enlarging costs bytes and produces a soft picture; the frame is
        // already smaller than the tile it will be drawn in.
        assert_eq!(target_size(&source(320, 180), 640), (320, 180));
        assert_eq!(target_size(&source(2, 2), 640), (2, 2));
    }

    #[test]
    fn a_requested_width_is_held_inside_what_this_module_will_make() {
        assert_eq!(ThumbnailOptions::new().width(), DEFAULT_WIDTH);
        assert_eq!(ThumbnailOptions::new().with_width(0).width(), 64);
        assert_eq!(ThumbnailOptions::new().with_width(9_999).width(), 1_920);
        assert_eq!(ThumbnailOptions::new().with_quality(0).quality(), 1);
        assert_eq!(ThumbnailOptions::new().with_quality(99).quality(), 31);
    }

    #[test]
    fn a_frame_with_no_timestamp_is_reported_at_the_start_rather_than_at_a_wild_time() {
        let stream = VideoStream {
            index: 0,
            raw_index: 0,
            parameters: ptr::null(),
            time_base: ffi::AVRational { num: 1, den: 1_000 },
            start_time: 0,
        };
        assert_eq!(seconds_of(NO_TIMESTAMP, &stream), Duration::ZERO);
        assert_eq!(seconds_of(-5, &stream), Duration::ZERO);
        assert_eq!(seconds_of(1_500, &stream), Duration::from_millis(1_500));

        // A container whose first timestamp is not zero — which Matroska with a
        // negative-offset edit list produces — is measured from its own start.
        let offset = VideoStream {
            start_time: 1_000,
            ..stream
        };
        assert_eq!(seconds_of(1_500, &offset), Duration::from_millis(500));
    }
}
