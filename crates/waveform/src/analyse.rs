//! Reading a finished recording and summarising every audio track in it.
//!
//! # What this does, and what it deliberately does not
//!
//! It opens a file that has already been written, demuxes it, decodes the audio
//! streams and reduces them to peaks. It opens no capture device, no audio
//! endpoint and no encoder; it never touches a recording that is still being
//! written. That is what makes waveform generation safe to run while a game is
//! being recorded — provided it is also kept out of the way, which is
//! [`crate::service`]'s job rather than this module's.
//!
//! # Ownership
//!
//! Three FFmpeg resources are held here, and each has exactly one owner.
//!
//! | Resource | Owner | Released by |
//! | --- | --- | --- |
//! | `AVFormatContext` | [`Demuxer`] | its `Drop`, with `avformat_close_input` |
//! | `AVCodecContext` | [`TrackDecoder`] | its `Drop`, with `avcodec_free_context` |
//! | `AVFrame`, `AVPacket` | [`TrackDecoder`], [`Packet`] | their `Drop` |
//!
//! Every one of them is null or live and never anything else, so `Drop` is
//! correct on every path out of every constructor, including the ones that fail
//! part way (AGENTS.md section 58).
//!
//! # Costs
//!
//! Demuxing reads the whole file, video packets included: the audio of a
//! recording is interleaved with its video, so there is no way to reach the last
//! second of audio without reading past the last second of video. That makes
//! this as much a disk job as a processor one, which is why the service asks
//! Windows to lower its **I/O** priority and not only its thread priority.
//! Measured throughput for this machine is in `docs/waveforms.md`.

use core::ffi::{c_char, c_int, CStr};
use core::ptr;
use core::time::Duration;
use std::ffi::CString;
use std::path::Path;

use clipped_background::{Continue, Pace, SourceIdentity, Unpaced};
use rusty_ffmpeg::ffi;
use tracing::{debug, warn};

use crate::peaks::{PeakAccumulator, BASE_BUCKET, MAX_BASE_BUCKETS};
use crate::samples::{SampleKind, SampleLayout};
use crate::waveform::{TrackDescriptor, TrackWaveform, Waveform};
use crate::WaveformError;

/// FFmpeg's `AVERROR_EOF`, which is `FFERRTAG('E','O','F',' ')`.
///
/// The binding does not carry it: it is a macro over another macro, and
/// `bindgen` expands neither. Spelled the same way `crates/encoder` spells it.
const AVERROR_EOF: c_int = -0x20_46_4F_45;

/// FFmpeg's `AVERROR(EAGAIN)`: the decoder wants another packet before it has a
/// frame to give.
#[allow(clippy::cast_possible_wrap)]
const AVERROR_EAGAIN: c_int = -(ffi::EAGAIN as c_int);

/// FFmpeg's `AV_NOPTS_VALUE`, the timestamp that means "there isn't one".
///
/// Written out rather than taken from the binding because `bindgen` renders it
/// as an unsigned constant, and comparing it against `AVFrame::pts` — which is
/// signed — would be a cast at every use.
const NO_TIMESTAMP: i64 = i64::MIN;

/// How many container packets are read between two [`Pace::checkpoint`] calls.
///
/// Small enough that suspending generation takes effect in well under a
/// millisecond of audio, large enough that the check is not a measurable part of
/// the work.
const PACKETS_PER_CHECKPOINT: u32 = 64;

/// How many times one packet is offered to a decoder before it is given up on.
///
/// libavcodec's contract is that a single drain is enough, so the second attempt
/// is the one that succeeds and anything beyond it is a decoder misbehaving.
/// Bounded because an unbounded retry would spin the worker thread on one packet
/// rather than reporting a problem.
const SEND_ATTEMPTS: u32 = 4;

/// What became of one packet offered to a decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delivery {
    /// The decoder took it.
    Accepted,
    /// The decoder refused it outright, saying this.
    Refused(c_int),
    /// The decoder kept asking for its output to be read and never took the
    /// packet, which contradicts libavcodec's own contract.
    NeverAccepted,
}

/// Offers one packet to a decoder, reading the decoder's output first whenever
/// it asks for that, until the packet is taken or refused.
///
/// `AVERROR(EAGAIN)` from `avcodec_send_packet` is documented as "output must be
/// read before this packet is accepted". It is not a bad packet: the answer is
/// to drain and offer **the same packet** again. Answering it the way a
/// rejection is answered drops the audio in that packet from the waveform with
/// nothing in the log to say so — a gap indistinguishable from silence, which is
/// precisely the outcome [`crate::WaveformError`] documents this crate as
/// avoiding.
///
/// Separated from the FFI, and taking the two operations as closures, so that
/// the retry can be tested: a libavcodec audio decoder cannot be made to return
/// `EAGAIN` here on demand, so a test that drove a real decoder would assert
/// nothing about the path this exists for.
fn offer_packet(
    mut send: impl FnMut() -> c_int,
    mut drain: impl FnMut() -> Result<(), WaveformError>,
) -> Result<Delivery, WaveformError> {
    for _ in 0..SEND_ATTEMPTS {
        let code = send();
        if code >= 0 || code == AVERROR_EOF {
            // `AVERROR_EOF` means the decoder has already been told the stream
            // ended and will take nothing more. Re-sending would not help;
            // reading what it still holds would.
            drain()?;
            return Ok(Delivery::Accepted);
        }
        if code != AVERROR_EAGAIN {
            return Ok(Delivery::Refused(code));
        }
        drain()?;
    }
    Ok(Delivery::NeverAccepted)
}

/// The most audio streams one recording is analysed for.
///
/// SPEC.md section 11 describes four; a file from elsewhere can declare more,
/// and each one costs a decoder and an accumulator. This is the bound that keeps
/// a hostile or broken file from turning into hundreds of open decoders.
const MAX_TRACKS: usize = 64;

/// Summarises every audio track of the recording at `path`.
///
/// Reads the file to the end. For a long recording that is seconds of work, so
/// a caller that is not already on a background thread wants
/// [`crate::WaveformService`] instead of this.
///
/// # Errors
///
/// When the file cannot be opened or read, when FFmpeg cannot make sense of the
/// container, or when the audio is longer than [`MAX_BASE_BUCKETS`] covers. A
/// recording with no audio at all is not an error: it is a [`Waveform`] with no
/// tracks (issue #180).
pub fn analyse(path: impl AsRef<Path>) -> Result<Waveform, WaveformError> {
    analyse_paced(path, &Unpaced)
}

/// [`analyse`], asking `pace` whether to carry on as it reads.
///
/// # Errors
///
/// As [`analyse`], and [`WaveformError::Cancelled`] when `pace` asks for a stop.
pub fn analyse_paced(path: impl AsRef<Path>, pace: &dyn Pace) -> Result<Waveform, WaveformError> {
    let path = path.as_ref();
    // Read first, so that a missing file is reported as the missing file it is
    // rather than as whatever libavformat says about it, and so that the
    // identity written into the cache is the one the peaks were taken from.
    let identity = SourceIdentity::of(path).map_err(|cause| WaveformError::Unreadable {
        path: clipped_logging::RedactedPath::new(path),
        cause,
    })?;

    let mut demuxer = Demuxer::open(path)?;
    let mut tracks = demuxer.open_audio_tracks(path)?;
    if tracks.is_empty() {
        debug!(
            recording = %clipped_logging::RedactedPath::new(identity.path()),
            "the recording has no audio track, so its waveform has none either"
        );
        return Ok(Waveform::new(identity, Vec::new()));
    }

    let mut packet = Packet::allocate(path)?;
    let mut since_checkpoint = 0u32;
    let mut scratch = Vec::new();

    loop {
        since_checkpoint += 1;
        if since_checkpoint >= PACKETS_PER_CHECKPOINT {
            since_checkpoint = 0;
            if pace.checkpoint() == Continue::Stop {
                return Err(WaveformError::Cancelled);
            }
        }

        match demuxer.read_into(&mut packet) {
            Read::Packet(stream_index) => {
                if let Some(track) = tracks
                    .iter_mut()
                    .find(|track| track.stream_index == stream_index)
                {
                    track.submit(packet.raw(), &mut scratch)?;
                }
                packet.unreference();
            }
            Read::EndOfFile => break,
            Read::Failed(code) => {
                return Err(WaveformError::Undecodable {
                    path: clipped_logging::RedactedPath::new(path),
                    detail: format!("reading a packet failed: {}", describe(code)),
                })
            }
        }
    }

    let mut summarised = Vec::with_capacity(tracks.len());
    for track in &mut tracks {
        track.flush(&mut scratch)?;
        summarised.push(track.finish());
    }
    Ok(Waveform::new(identity, summarised))
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

/// An open container.
struct Demuxer {
    context: *mut ffi::AVFormatContext,
}

impl Demuxer {
    /// Opens `path` and reads enough of it to describe its streams.
    fn open(path: &Path) -> Result<Self, WaveformError> {
        let redacted = clipped_logging::RedactedPath::new(path);
        let text = CString::new(path.to_string_lossy().into_owned()).map_err(|_| {
            WaveformError::Undecodable {
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
            // On failure `avformat_open_input` frees the context and sets the
            // pointer back to null, so there is nothing to release here.
            return Err(WaveformError::Undecodable {
                path: redacted,
                detail: format!("the container could not be opened: {}", describe(code)),
            });
        }

        let demuxer = Self { context };
        // SAFETY: the context is live and exclusively owned; the options
        // argument is documented as nullable.
        let code = unsafe { ffi::avformat_find_stream_info(demuxer.context, ptr::null_mut()) };
        if code < 0 {
            return Err(WaveformError::Undecodable {
                path: redacted,
                detail: format!(
                    "the container's streams could not be read: {}",
                    describe(code)
                ),
            });
        }
        Ok(demuxer)
    }

    /// Opens a decoder for every audio stream the container declares.
    ///
    /// A stream this build has no decoder for, or whose decoder will not open,
    /// is left out with a warning naming the codec: a track that silently
    /// produced a flat line would be indistinguishable from a silent one.
    fn open_audio_tracks(&mut self, path: &Path) -> Result<Vec<Track>, WaveformError> {
        // SAFETY: the context is live, and `nb_streams`/`streams` are the
        // documented way to walk it.
        let count = unsafe { (*self.context).nb_streams };
        let mut tracks = Vec::new();

        for index in 0..count {
            // SAFETY: `index` is below `nb_streams`, and every entry of
            // `streams` up to that is a live stream owned by the context.
            let stream = unsafe { *(*self.context).streams.add(index as usize) };
            // SAFETY: a live stream always has parameters.
            let parameters = unsafe { (*stream).codecpar };
            // SAFETY: as above.
            if unsafe { (*parameters).codec_type } != ffi::AVMEDIA_TYPE_AUDIO {
                continue;
            }
            if tracks.len() >= MAX_TRACKS {
                warn!(
                    recording = %clipped_logging::RedactedPath::new(path),
                    limit = MAX_TRACKS,
                    "the recording declares more audio streams than are summarised"
                );
                break;
            }

            match Track::open(stream, index) {
                Ok(track) => tracks.push(track),
                Err(reason) => warn!(
                    recording = %clipped_logging::RedactedPath::new(path),
                    stream = index,
                    reason = %reason,
                    "an audio track has no waveform because it could not be decoded"
                ),
            }
        }

        Ok(tracks)
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
        Read::Packet(unsafe { (*packet.raw()).stream_index as u32 })
    }
}

impl Drop for Demuxer {
    fn drop(&mut self) {
        if !self.context.is_null() {
            // SAFETY: the context was allocated by `avformat_open_input` and is
            // not used again; the call also nulls the pointer.
            unsafe { ffi::avformat_close_input(&raw mut self.context) };
        }
    }
}

/// A reusable packet.
struct Packet {
    raw: *mut ffi::AVPacket,
}

impl Packet {
    fn allocate(path: &Path) -> Result<Self, WaveformError> {
        // SAFETY: allocates a packet, or returns null.
        let raw = unsafe { ffi::av_packet_alloc() };
        if raw.is_null() {
            return Err(WaveformError::Undecodable {
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
        // SAFETY: the packet is live and exclusively owned. `av_packet_unref`
        // is documented as safe to call on a packet holding nothing.
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

/// One audio stream, its decoder, and the peaks accumulated from it so far.
struct Track {
    stream_index: u32,
    descriptor: TrackDescriptor,
    /// The stream's time base, for turning a packet timestamp into a sample
    /// position.
    time_base: ffi::AVRational,
    /// What the container says this stream's first timestamp is, subtracted so
    /// that a track which does not start at zero still lines up with the others.
    start_timestamp: i64,
    context: *mut ffi::AVCodecContext,
    frame: *mut ffi::AVFrame,
    accumulator: PeakAccumulator,
    /// Where the next frame goes when it carries no timestamp of its own.
    next_position: u64,
}

impl Track {
    /// Opens a decoder for one audio stream.
    fn open(stream: *mut ffi::AVStream, index: u32) -> Result<Self, String> {
        // SAFETY: the stream is live and owned by the format context, which
        // outlives every `Track` (both are dropped in `analyse_paced`, tracks
        // first).
        let (parameters, time_base, start_timestamp, metadata) = unsafe {
            (
                (*stream).codecpar,
                (*stream).time_base,
                (*stream).start_time,
                (*stream).metadata,
            )
        };
        // SAFETY: a live stream always has parameters.
        let (codec_id, sample_rate, channels) = unsafe {
            (
                (*parameters).codec_id,
                (*parameters).sample_rate,
                (*parameters).ch_layout.nb_channels,
            )
        };

        if sample_rate <= 0 {
            return Err(format!(
                "the stream declares a sample rate of {sample_rate}"
            ));
        }

        // SAFETY: reads a static table inside libavcodec and returns null or a
        // pointer into it.
        let codec = unsafe { ffi::avcodec_find_decoder(codec_id) };
        if codec.is_null() {
            return Err(format!("this build has no decoder for codec {codec_id}"));
        }

        // SAFETY: `codec` is a live descriptor; this returns null or a context
        // this value owns from here on.
        let context = unsafe { ffi::avcodec_alloc_context3(codec) };
        if context.is_null() {
            return Err("a decoder context could not be allocated".to_owned());
        }
        let mut track = Self {
            stream_index: index,
            descriptor: TrackDescriptor::new(
                index,
                u32::try_from(sample_rate).unwrap_or(0),
                u16::try_from(channels).unwrap_or(0),
                name_of(metadata),
            ),
            time_base,
            start_timestamp: if start_timestamp == NO_TIMESTAMP {
                0
            } else {
                start_timestamp
            },
            context,
            frame: ptr::null_mut(),
            accumulator: PeakAccumulator::new(u32::try_from(sample_rate).unwrap_or(0), BASE_BUCKET),
            next_position: 0,
        };

        // SAFETY: the context is live, exclusively owned and not yet open, and
        // the parameters belong to the stream it is being opened for.
        let code = unsafe { ffi::avcodec_parameters_to_context(track.context, parameters) };
        if code < 0 {
            return Err(format!(
                "the decoder could not be configured: {}",
                describe(code)
            ));
        }
        // SAFETY: the context is live and not yet open. `pkt_timebase` is a
        // field libavcodec documents as the caller's to set, and some decoders
        // need it to produce sensible frame timestamps.
        unsafe { (*track.context).pkt_timebase = time_base };

        // SAFETY: the context is live and configured; the options argument is
        // documented as nullable.
        let code = unsafe { ffi::avcodec_open2(track.context, codec, ptr::null_mut()) };
        if code < 0 {
            return Err(format!(
                "the decoder could not be opened: {}",
                describe(code)
            ));
        }

        // SAFETY: allocates a frame, or returns null.
        track.frame = unsafe { ffi::av_frame_alloc() };
        if track.frame.is_null() {
            return Err("a frame could not be allocated".to_owned());
        }
        Ok(track)
    }

    /// Decodes one packet of this stream, accumulating whatever comes out.
    fn submit(
        &mut self,
        packet: *mut ffi::AVPacket,
        scratch: &mut Vec<f32>,
    ) -> Result<(), WaveformError> {
        let context = self.context;
        let delivery = offer_packet(
            // SAFETY: the context is live and open, and the packet is live and
            // belongs to this stream. libavcodec copies what it needs.
            || unsafe { ffi::avcodec_send_packet(context, packet) },
            || self.drain(scratch),
        )?;

        match delivery {
            Delivery::Accepted => {}
            Delivery::Refused(code) => {
                // A packet a decoder refuses outright is a damaged one; the
                // rest of the track is still worth summarising, so this is
                // logged rather than abandoning the file.
                debug!(
                    stream = self.stream_index,
                    error = %describe(code),
                    "a packet could not be decoded and was skipped"
                );
            }
            Delivery::NeverAccepted => warn!(
                stream = self.stream_index,
                attempts = SEND_ATTEMPTS,
                "a decoder would not accept a packet even after its output was read, so the \
                 waveform is missing that audio"
            ),
        }
        Ok(())
    }

    /// Tells the decoder there are no more packets and accumulates what it was
    /// still holding.
    fn flush(&mut self, scratch: &mut Vec<f32>) -> Result<(), WaveformError> {
        // SAFETY: the context is live and open. A null packet is the documented
        // way to signal the end of the stream.
        unsafe { ffi::avcodec_send_packet(self.context, ptr::null_mut()) };
        self.drain(scratch)
    }

    /// Takes every frame the decoder currently has.
    fn drain(&mut self, scratch: &mut Vec<f32>) -> Result<(), WaveformError> {
        loop {
            // SAFETY: both the context and the frame are live and exclusively
            // owned. On success the frame holds a reference this loop releases
            // with `av_frame_unref` before asking for another.
            let code = unsafe { ffi::avcodec_receive_frame(self.context, self.frame) };
            if code == AVERROR_EAGAIN || code == AVERROR_EOF {
                return Ok(());
            }
            if code < 0 {
                debug!(
                    stream = self.stream_index,
                    error = %describe(code),
                    "a decoded frame could not be read"
                );
                return Ok(());
            }

            let outcome = self.accumulate(scratch);
            // SAFETY: the frame is live; unreferencing it is required before it
            // is reused, and is safe whether or not it holds anything.
            unsafe { ffi::av_frame_unref(self.frame) };
            outcome?;
        }
    }

    /// Merges the frame currently in `self.frame` into the accumulator.
    fn accumulate(&mut self, scratch: &mut Vec<f32>) -> Result<(), WaveformError> {
        // SAFETY: the frame is live and was just filled in by
        // `avcodec_receive_frame`.
        let (format, frames, channels, timestamp, planes) = unsafe {
            (
                (*self.frame).format,
                (*self.frame).nb_samples,
                (*self.frame).ch_layout.nb_channels,
                (*self.frame).pts,
                (*self.frame).extended_data,
            )
        };
        let frames = usize::try_from(frames).unwrap_or(0);
        let channels = usize::try_from(channels).unwrap_or(0);
        if frames == 0 || channels == 0 || planes.is_null() {
            return Ok(());
        }

        let Some(layout) = layout_of(format) else {
            return Err(WaveformError::UnsupportedSampleFormat {
                stream: self.stream_index,
                format: format_name(format),
            });
        };

        let position = self.position_of(timestamp);
        let plane_bytes = layout.plane_bytes(frames, channels);

        for channel in 0..channels {
            let plane_index = if layout.is_planar() { channel } else { 0 };
            // SAFETY: `extended_data` has one entry per plane, which for this
            // format and channel count is one per channel when planar and one
            // in total when not — `plane_index` is inside that either way.
            // libavcodec guarantees each plane holds at
            // least the `nb_samples` the frame declares, which is what
            // `plane_bytes` is computed from, so the slice is within the
            // allocation. It is read before the frame is unreferenced.
            let plane = unsafe {
                let pointer = *planes.add(plane_index);
                if pointer.is_null() {
                    continue;
                }
                core::slice::from_raw_parts(pointer, plane_bytes)
            };
            layout.read_channel(plane, channels, channel, frames, scratch);
            self.accumulator
                .add_run(position, scratch)
                .map_err(|_| WaveformError::TooLong {
                    limit: BASE_BUCKET * u32::try_from(MAX_BASE_BUCKETS).unwrap_or(u32::MAX),
                })?;
        }

        self.next_position = position + frames as u64;
        Ok(())
    }

    /// Where in the track a frame belongs, in samples.
    ///
    /// From the frame's own timestamp where it has one, so that a gap in the
    /// audio is a gap in the waveform rather than a shift of everything after
    /// it. A frame without a timestamp follows the previous one, which is what
    /// a decoder that does not carry timestamps through produces.
    fn position_of(&self, timestamp: i64) -> u64 {
        if timestamp == NO_TIMESTAMP || self.time_base.den <= 0 || self.time_base.num <= 0 {
            return self.next_position;
        }
        let ticks = i128::from(timestamp) - i128::from(self.start_timestamp);
        if ticks <= 0 {
            // Before the container's start: encoder priming, which belongs at
            // the beginning rather than at a negative position.
            return 0;
        }
        let samples =
            ticks * i128::from(self.descriptor.sample_rate()) * i128::from(self.time_base.num)
                / i128::from(self.time_base.den);
        u64::try_from(samples).unwrap_or(self.next_position)
    }

    /// The finished track.
    ///
    /// Takes `&mut self` rather than `self` because a `Track` owns FFmpeg
    /// resources and therefore has a `Drop`, which is what releases them; moving
    /// the accumulator out and leaving an empty one behind keeps the decoder's
    /// ownership intact right up to the end of the value's life.
    fn finish(&mut self) -> TrackWaveform {
        let sample_rate = u64::from(self.descriptor.sample_rate().max(1));
        let accumulator = self.take_accumulator();
        let duration = Duration::from_nanos(
            accumulator
                .length_in_samples()
                .saturating_mul(1_000_000_000)
                / sample_rate,
        );
        TrackWaveform::from_base(
            self.descriptor.clone(),
            duration,
            BASE_BUCKET,
            accumulator.finish(),
        )
    }

    /// The accumulator's contents, leaving an empty one behind.
    fn take_accumulator(&mut self) -> PeakAccumulator {
        core::mem::replace(
            &mut self.accumulator,
            PeakAccumulator::new(self.descriptor.sample_rate(), BASE_BUCKET),
        )
    }
}

impl Drop for Track {
    fn drop(&mut self) {
        if !self.frame.is_null() {
            // SAFETY: allocated by `av_frame_alloc` and not used again.
            unsafe { ffi::av_frame_free(&raw mut self.frame) };
        }
        if !self.context.is_null() {
            // SAFETY: allocated by `avcodec_alloc_context3` and not used again.
            unsafe { ffi::avcodec_free_context(&raw mut self.context) };
        }
    }
}

/// What a track is called, from the container's tags.
///
/// The `title` tag first — that is what the muxer writes for each track
/// (docs/muxing.md) — and the language if there is no title, which is what a
/// file from elsewhere usually carries. Empty when the container says neither.
fn name_of(metadata: *mut ffi::AVDictionary) -> String {
    for key in [c"title", c"language"] {
        // SAFETY: `metadata` is null or a live dictionary owned by the stream;
        // `av_dict_get` documents null as an empty dictionary. The key is a
        // NUL-terminated literal, and the returned entry is owned by the
        // dictionary and read before anything can free it.
        let entry = unsafe { ffi::av_dict_get(metadata, key.as_ptr(), ptr::null(), 0) };
        if entry.is_null() {
            continue;
        }
        // SAFETY: a non-null entry has a NUL-terminated value.
        let value = unsafe { CStr::from_ptr((*entry).value) };
        if let Ok(text) = value.to_str() {
            if !text.is_empty() {
                return text.to_owned();
            }
        }
    }
    String::new()
}

/// The layout of an `AVSampleFormat`, or [`None`] for one this build cannot
/// read.
fn layout_of(format: c_int) -> Option<SampleLayout> {
    let (kind, planar) = match format {
        format if format == ffi::AV_SAMPLE_FMT_U8 => (SampleKind::U8, false),
        format if format == ffi::AV_SAMPLE_FMT_S16 => (SampleKind::S16, false),
        format if format == ffi::AV_SAMPLE_FMT_S32 => (SampleKind::S32, false),
        format if format == ffi::AV_SAMPLE_FMT_S64 => (SampleKind::S64, false),
        format if format == ffi::AV_SAMPLE_FMT_FLT => (SampleKind::F32, false),
        format if format == ffi::AV_SAMPLE_FMT_DBL => (SampleKind::F64, false),
        format if format == ffi::AV_SAMPLE_FMT_U8P => (SampleKind::U8, true),
        format if format == ffi::AV_SAMPLE_FMT_S16P => (SampleKind::S16, true),
        format if format == ffi::AV_SAMPLE_FMT_S32P => (SampleKind::S32, true),
        format if format == ffi::AV_SAMPLE_FMT_S64P => (SampleKind::S64, true),
        format if format == ffi::AV_SAMPLE_FMT_FLTP => (SampleKind::F32, true),
        format if format == ffi::AV_SAMPLE_FMT_DBLP => (SampleKind::F64, true),
        _ => return None,
    };
    Some(SampleLayout::new(kind, planar))
}

/// What libavutil calls a sample format, for an error message.
fn format_name(format: c_int) -> String {
    // SAFETY: `av_get_sample_fmt_name` takes any value and returns null for one
    // it does not know.
    let name = unsafe { ffi::av_get_sample_fmt_name(format) };
    if name.is_null() {
        return format!("an unknown format ({format})");
    }
    // SAFETY: a non-null result is a NUL-terminated static string.
    unsafe { CStr::from_ptr(name) }
        .to_string_lossy()
        .into_owned()
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

    /// Drives [`offer_packet`] over a scripted sequence of decoder answers and
    /// reports what it did: the outcome, how many times the packet was sent,
    /// and how many times the decoder's output was read.
    fn offer(answers: &[c_int]) -> (Delivery, usize, usize) {
        let mut sent = 0usize;
        let mut drained = 0usize;
        let delivery = offer_packet(
            || {
                let code = answers.get(sent).copied().unwrap_or(0);
                sent += 1;
                code
            },
            || {
                drained += 1;
                Ok(())
            },
        )
        .expect("draining does not fail here");
        (delivery, sent, drained)
    }

    #[test]
    fn a_packet_a_decoder_is_not_ready_for_is_offered_again_rather_than_dropped() {
        // FFmpeg's contract for `AVERROR(EAGAIN)` from `avcodec_send_packet` is
        // "output must be read before this packet is accepted". `submit`'s
        // caller unreferences the packet the moment it returns, so a run that
        // reads the output and then moves on has thrown that audio away: a gap
        // in the waveform indistinguishable from silence.
        let (delivery, sent, drained) = offer(&[AVERROR_EAGAIN, AVERROR_EAGAIN, 0]);
        assert_eq!(delivery, Delivery::Accepted);
        assert_eq!(sent, 3, "the packet was not offered again after draining");
        assert_eq!(
            drained, 3,
            "the decoder's output was not read before each retry and after acceptance"
        );
    }

    #[test]
    fn a_packet_taken_first_time_costs_one_send_and_one_drain() {
        assert_eq!(offer(&[0]), (Delivery::Accepted, 1, 1));
        // A decoder already flushed will take nothing more, so re-sending would
        // not help; what it still holds is still worth reading.
        assert_eq!(offer(&[AVERROR_EOF]), (Delivery::Accepted, 1, 1));
    }

    #[test]
    fn a_packet_the_decoder_refuses_is_skipped_without_retrying_it() {
        // A damaged packet rather than a full decoder. Retrying it would read
        // the rest of the file more slowly for nothing.
        let einval = -(ffi::EINVAL as c_int);
        let (delivery, sent, drained) = offer(&[einval]);
        assert_eq!(delivery, Delivery::Refused(einval));
        assert_eq!(sent, 1);
        assert_eq!(drained, 0);
    }

    #[test]
    fn a_decoder_that_never_takes_a_packet_is_given_up_on_rather_than_spun_on() {
        // This contradicts libavcodec's own contract, so it should not happen;
        // an unbounded retry would hang the worker thread on one packet if it
        // ever did.
        let forever = vec![AVERROR_EAGAIN; 64];
        let (delivery, sent, drained) = offer(&forever);
        assert_eq!(delivery, Delivery::NeverAccepted);
        assert_eq!(sent, SEND_ATTEMPTS as usize);
        assert_eq!(drained, SEND_ATTEMPTS as usize);
    }
}
