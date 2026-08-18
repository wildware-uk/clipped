//! Reading a finished recording, packet by packet, without changing it.
//!
//! # Why this links FFmpeg rather than going through `clipped-muxer`
//!
//! `clipped-muxer` owns this workspace's FFmpeg linkage and its safe wrappers,
//! and this crate depends on it — but what it offers is *writing*: a container
//! writer and a whole-file remux. Its input context is private to that crate
//! and there is no public way to read a container's packets. `clipped-waveform`
//! names the binding directly for the same reason and cites the amendment issue
//! #155 made to `docs/adr/0004-ffmpeg-dependency-strategy.md`: a crate with no
//! lower-layer route to what it needs may name the binding.
//!
//! What is *not* duplicated is the half that matters. The output side of an
//! export is `clipped_muxer::MkvWriter` and nothing else — there is no second
//! muxer here (AGENTS.md section 55, `docs/exporting.md`).
//!
//! # The recording is opened for reading and nothing else
//!
//! `avformat_open_input` opens for reading, and nothing here opens a source any
//! other way, so an export — including one that fails half-way and one that is
//! cancelled — leaves the recording byte for byte as it found it (AGENTS.md
//! sections 56 and 57).
//!
//! # Ownership and threading
//!
//! One `AVFormatContext` and one `AVPacket` per [`SourceMedia`], each released
//! in `Drop` (AGENTS.md section 58). The packet is allocated once and reused,
//! because a five-minute recording is tens of thousands of them. The value is
//! `Send` and deliberately not `Sync`: libavformat's contract is that one
//! context is used by one thread at a time, which `&mut self` on every read
//! enforces.

use core::ptr::{self, NonNull};
use core::time::Duration;
use std::ffi::{c_int, CStr, CString};
use std::path::{Path, PathBuf};

use clipped_muxer::{AudioCodec, AvError, FrameRate, VideoCodec};
use rusty_ffmpeg::ffi;

use crate::error::ExportError;
use crate::source::{
    AudioPacket, AudioPacketIndex, IndexedFrame, SourceProfile, SourceStream, StreamFormat,
    VideoFrameIndex,
};

/// FFmpeg's `AVERROR_EOF`, which is `FFERRTAG('E','O','F',' ')`.
///
/// The binding does not carry it: it is a macro over another macro, and
/// `bindgen` expands neither. The same constant, for the same reason, as
/// `crates/muxer/src/remux.rs`.
const AVERROR_EOF: c_int = -0x2046_4F45;

/// FFmpeg's `AV_NOPTS_VALUE`, the timestamp that means "there is none".
const AV_NOPTS_VALUE: i64 = i64::MIN;

/// The unit `AVFormatContext::start_time` and a whole-file seek are counted in.
const AV_TIME_BASE_Q: ffi::AVRational = ffi::AVRational {
    num: 1,
    den: 1_000_000,
};

/// Nanoseconds, which is what an edit document counts in.
const NANOSECONDS: ffi::AVRational = ffi::AVRational {
    num: 1,
    den: 1_000_000_000,
};

/// How far before a wanted moment a seek aims.
///
/// A seek lands on the keyframe at or before the target, and the demuxer
/// resumes at that file position for *every* stream — so audio whose packets
/// were interleaved slightly ahead of that video keyframe would be missed. A
/// second of slack is far more than any container interleaves across, and the
/// packets it brings back are discarded by the same filter that discards the
/// rest of the material before the cut.
const SEEK_SLACK: Duration = Duration::from_secs(1);

/// One packet of a recording, as it was read.
///
/// The payload is not in here: it is in the reader's own packet slot until the
/// next read, and [`SourceMedia::packet_data`] hands it out. That keeps the
/// borrow immutable, so a caller can look at the rest of the recording's
/// description while it holds the bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PacketInfo {
    /// Which stream of the container it belongs to.
    pub(crate) stream: usize,
    /// When it is presented, in nanoseconds on the recording's own timeline.
    pub(crate) presentation_nanos: i64,
    /// When it is decoded, on the same timeline.
    pub(crate) decode_nanos: i64,
    /// How long it occupies, where the container records that.
    pub(crate) duration_nanos: i64,
    /// Whether a decoder can start here.
    pub(crate) keyframe: bool,
    /// How many bytes of coded media it carries.
    ///
    /// Read from the packet's own header rather than from its payload, so that
    /// the indexing pass can record it without touching the bytes.
    pub(crate) bytes: u32,
}

/// A recording, open for reading.
pub(crate) struct SourceMedia {
    context: NonNull<ffi::AVFormatContext>,
    packet: NonNull<ffi::AVPacket>,
    path: PathBuf,
    streams: Vec<SourceStream>,
    /// The unit each container stream counts time in, indexed by stream index.
    time_bases: Vec<ffi::AVRational>,
    /// Where the recording's own timeline begins, in nanoseconds, as the
    /// container declares it.
    ///
    /// Subtracted from every timestamp so that "nanoseconds from that
    /// recording's first frame" — which is what a `SourceTime` in an edit
    /// document means — is what a caller gets. Zero for anything Clipped
    /// recorded, because `MkvWriter` rebases onto its first packet already.
    origin_nanos: i64,
}

// SAFETY: a `SourceMedia` exclusively owns its `AVFormatContext` and its
// `AVPacket`; no pointer to either escapes and neither is shared. FFmpeg
// attaches no thread affinity to a format context — the contract is that one
// context is used by one thread at a time, which `&mut self` on every read
// enforces — so moving a reader to the thread that will run the export is
// sound. It is deliberately not `Sync`.
unsafe impl Send for SourceMedia {}

impl SourceMedia {
    /// Opens `path` for reading and describes its streams.
    ///
    /// # Errors
    ///
    /// [`ExportError::SourceNotRepresentable`] for a path FFmpeg cannot be
    /// given, and [`ExportError::SourceUnreadable`] for a file that cannot be
    /// opened or described.
    pub(crate) fn open(path: &Path) -> Result<Self, ExportError> {
        let Some(text) = path.to_str() else {
            return Err(ExportError::SourceNotRepresentable {
                source: path.to_path_buf(),
            });
        };
        // `file:` rather than the bare path so that the file protocol is used
        // whatever the path looks like, for the reason `MkvWriter::create`
        // gives: FFmpeg picks a protocol from the text before the first colon.
        let Ok(url) = CString::new(format!("file:{text}")) else {
            return Err(ExportError::SourceNotRepresentable {
                source: path.to_path_buf(),
            });
        };

        let mut context: *mut ffi::AVFormatContext = ptr::null_mut();
        // SAFETY: `avformat_open_input` allocates a context and writes it
        // through its first argument, which is a live local. `url` is
        // NUL-terminated and outlives the call. Passing null for the format and
        // the options asks it to probe the file and to take no options. On
        // failure it leaves the pointer null, which is why the code is checked
        // first.
        let code = unsafe {
            ffi::avformat_open_input(&mut context, url.as_ptr(), ptr::null(), ptr::null_mut())
        };
        if code < 0 {
            return Err(ExportError::SourceUnreadable {
                source: path.to_path_buf(),
                error: AvError::new(code),
            });
        }
        let Some(context) = NonNull::new(context) else {
            return Err(ExportError::SourceUnreadable {
                source: path.to_path_buf(),
                error: AvError::new(-(ffi::EINVAL as i32)),
            });
        };

        // SAFETY: `av_packet_alloc` takes no arguments and returns either null
        // or a packet this value then owns. Allocated before the early returns
        // below so that every path out of this function either owns both
        // resources or has released the one it took.
        let packet = unsafe { ffi::av_packet_alloc() };
        let Some(packet) = NonNull::new(packet) else {
            let mut context = context.as_ptr();
            // SAFETY: the context was opened above and nothing else holds it.
            unsafe { ffi::avformat_close_input(&mut context) };
            return Err(ExportError::SourceUnreadable {
                source: path.to_path_buf(),
                error: AvError::new(-(ffi::ENOMEM as i32)),
            });
        };

        let mut media = Self {
            context,
            packet,
            path: path.to_path_buf(),
            streams: Vec::new(),
            time_bases: Vec::new(),
            origin_nanos: 0,
        };

        // SAFETY: the context is live and owned by `media`, which closes it on
        // every path from here. `avformat_find_stream_info` reads far enough
        // into the file to fill in each stream's parameters and takes no
        // options.
        let code = unsafe { ffi::avformat_find_stream_info(media.as_ptr(), ptr::null_mut()) };
        if code < 0 {
            return Err(ExportError::SourceUnreadable {
                source: path.to_path_buf(),
                error: AvError::new(code),
            });
        }

        media.describe();
        Ok(media)
    }

    /// The context, for passing to FFmpeg.
    fn as_ptr(&self) -> *mut ffi::AVFormatContext {
        self.context.as_ptr()
    }

    /// Which container stream is the picture, when there is one.
    pub(crate) fn video_stream_index(&self) -> Option<usize> {
        self.streams
            .iter()
            .find(|stream| stream.is_video())
            .map(SourceStream::index)
    }

    /// Reads every stream's description out of the open context.
    fn describe(&mut self) {
        // SAFETY: the context is live and `nb_streams` and `start_time` are
        // plain fields filled in while the file was opened.
        let (count, start_time) = unsafe {
            (
                (*self.as_ptr()).nb_streams as usize,
                (*self.as_ptr()).start_time,
            )
        };

        self.origin_nanos = if start_time == AV_NOPTS_VALUE {
            0
        } else {
            rescale(start_time, AV_TIME_BASE_Q, NANOSECONDS)
        };

        self.streams = Vec::with_capacity(count);
        self.time_bases = Vec::with_capacity(count);

        for index in 0..count {
            // SAFETY: `index` is below `nb_streams`, so it indexes a stream the
            // live context owns.
            let stream = unsafe { *(*self.as_ptr()).streams.add(index) };
            self.time_bases.push(
                // SAFETY: as above; `time_base` is a plain field of a live
                // stream.
                unsafe { (*stream).time_base },
            );
            self.streams.push(describe_stream(index, stream));
        }
    }

    /// Reads the whole file once, without decoding, to find out where the
    /// pictures are.
    ///
    /// This is the pass that makes a plan possible: whether a cut lands on a
    /// keyframe is a property of the bitstream and there is no cheaper way to
    /// ask. Nothing is decoded, so the cost is reading the container's packet
    /// headers.
    ///
    /// The same headers carry each packet's coded size, which is what lets a
    /// plan say how large a copy would be, so it is recorded here rather than
    /// paid for with a second pass.
    ///
    /// # Errors
    ///
    /// [`ExportError::SourceRead`] when the file stops being readable, and
    /// [`ExportError::SourceUnreadable`] when it cannot be rewound afterwards.
    pub(crate) fn profile(&mut self) -> Result<SourceProfile, ExportError> {
        self.seek_before_nanos(0)?;

        let video = self.video_stream_index();
        // The container index of each sound track, in the order a document
        // numbers them: 0 for the first audio stream, not for the video one.
        let audio_streams: Vec<usize> = self
            .streams
            .iter()
            .filter(|stream| stream.is_audio())
            .map(SourceStream::index)
            .collect();

        let mut frames = Vec::new();
        let mut audio = vec![Vec::new(); audio_streams.len()];

        while let Some(packet) = self.read()? {
            let presentation = clipped_edit::SourceTime::from_nanos(
                packet.presentation_nanos.max(0).unsigned_abs(),
            );

            if Some(packet.stream) == video {
                frames.push(IndexedFrame {
                    presentation,
                    decode: clipped_edit::SourceTime::from_nanos(
                        packet.decode_nanos.max(0).unsigned_abs(),
                    ),
                    keyframe: packet.keyframe,
                    bytes: packet.bytes,
                });
            } else if let Some(track) = audio_streams
                .iter()
                .position(|stream| *stream == packet.stream)
            {
                audio[track].push(AudioPacket {
                    presentation,
                    bytes: packet.bytes,
                });
            }
        }

        Ok(
            SourceProfile::new(self.streams.clone(), VideoFrameIndex::new(frames))
                .with_audio_packets(audio.into_iter().map(AudioPacketIndex::new).collect()),
        )
    }

    /// Positions the reader a little before `nanos` on the recording's
    /// timeline.
    ///
    /// Backwards, so the demuxer resumes at a keyframe at or before the target;
    /// the caller discards what it does not want. A failure is reported rather
    /// than ignored, because carrying on from wherever the reader happened to
    /// be would write a segment of the wrong material.
    ///
    /// # Errors
    ///
    /// [`ExportError::SourceRead`] when FFmpeg cannot seek there.
    pub(crate) fn seek_before_nanos(&mut self, nanos: u64) -> Result<(), ExportError> {
        let target = i64::try_from(nanos.saturating_sub(SEEK_SLACK.as_nanos() as u64))
            .unwrap_or(i64::MAX)
            .saturating_add(self.origin_nanos);
        let target = rescale(target, NANOSECONDS, AV_TIME_BASE_Q);

        // SAFETY: the context is live. A stream index of -1 asks for the
        // timestamp to be interpreted in `AV_TIME_BASE` units over the whole
        // file, which is what `target` was just converted to.
        // `AVSEEK_FLAG_BACKWARD` asks for the keyframe at or before it.
        let code = unsafe {
            ffi::av_seek_frame(
                self.as_ptr(),
                -1,
                target,
                ffi::AVSEEK_FLAG_BACKWARD as c_int,
            )
        };
        if code < 0 {
            return Err(ExportError::SourceRead {
                source: self.path.clone(),
                error: AvError::new(code),
            });
        }
        Ok(())
    }

    /// Reads the next packet, or [`None`] at the end of the file.
    ///
    /// The bytes are in this reader's own packet until the next call; read them
    /// with [`packet_data`](Self::packet_data).
    ///
    /// # Errors
    ///
    /// [`ExportError::SourceRead`] when the file stops being readable.
    pub(crate) fn read(&mut self) -> Result<Option<PacketInfo>, ExportError> {
        // SAFETY: both pointers are live and exclusively owned. `av_read_frame`
        // unreferences whatever the packet held and fills it with a reference
        // of its own, which is released by the next read or by `Drop`.
        let code = unsafe { ffi::av_read_frame(self.as_ptr(), self.packet.as_ptr()) };
        if code == AVERROR_EOF {
            return Ok(None);
        }
        if code < 0 {
            return Err(ExportError::SourceRead {
                source: self.path.clone(),
                error: AvError::new(code),
            });
        }

        // SAFETY: the packet is live and filled in; every field read is a plain
        // scalar.
        let (index, presentation, decode, duration, flags, size) = unsafe {
            let packet = self.packet.as_ptr();
            (
                (*packet).stream_index,
                (*packet).pts,
                (*packet).dts,
                (*packet).duration,
                (*packet).flags,
                (*packet).size,
            )
        };

        let stream = usize::try_from(index).unwrap_or(usize::MAX);
        let time_base = self.time_bases.get(stream).copied().unwrap_or(NANOSECONDS);

        // A packet with only one of the two timestamps is normal — Matroska
        // stores no decode timestamp for a stream that does not reorder — so
        // the missing one is the other rather than a guess, exactly as
        // `crates/muxer/src/remux.rs` does it.
        let presentation = first_timestamp(presentation, decode).unwrap_or(0);
        let decode = first_timestamp(decode, presentation).unwrap_or(0);

        Ok(Some(PacketInfo {
            stream,
            presentation_nanos: rescale(presentation, time_base, NANOSECONDS) - self.origin_nanos,
            decode_nanos: rescale(decode, time_base, NANOSECONDS) - self.origin_nanos,
            duration_nanos: rescale(duration.max(0), time_base, NANOSECONDS),
            keyframe: (flags & ffi::AV_PKT_FLAG_KEY as c_int) != 0,
            bytes: u32::try_from(size).unwrap_or(0),
        }))
    }

    /// The bytes of the packet the last [`read`](Self::read) returned.
    pub(crate) fn packet_data(&self) -> &[u8] {
        // SAFETY: the packet is live. After a successful `av_read_frame` its
        // `data` points at `size` readable bytes owned by the packet's own
        // reference, which lives until the next read or until `Drop`; the
        // returned slice borrows `self`, so neither can happen while it is
        // held. A packet with no payload has a null pointer and a zero size,
        // which is returned as an empty slice rather than a null one.
        unsafe {
            let packet = self.packet.as_ptr();
            let size = usize::try_from((*packet).size).unwrap_or(0);
            if (*packet).data.is_null() || size == 0 {
                return &[];
            }
            core::slice::from_raw_parts((*packet).data, size)
        }
    }
}

impl Drop for SourceMedia {
    fn drop(&mut self) {
        let mut packet = self.packet.as_ptr();
        let mut context = self.as_ptr();
        // SAFETY: this value owns both and nothing else holds a pointer to
        // either. `av_packet_free` unreferences whatever the packet holds and
        // nulls the local pointer; `avformat_close_input` closes the file,
        // frees the context and nulls its own.
        unsafe {
            ffi::av_packet_free(&mut packet);
            ffi::avformat_close_input(&mut context);
        }
    }
}

impl core::fmt::Debug for SourceMedia {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // The path is deliberately absent: it contains the account name, and a
        // `Debug` string ends up in logs and panic messages (docs/logging.md).
        formatter
            .debug_struct("SourceMedia")
            .field("streams", &self.streams.len())
            .field("origin_nanos", &self.origin_nanos)
            .finish()
    }
}

/// Reads one stream's description out of the container.
fn describe_stream(index: usize, stream: *mut ffi::AVStream) -> SourceStream {
    // SAFETY: `stream` came from the context's own array and points at a stream
    // that context owns and outlives this call. `codecpar` is allocated with
    // the stream and is never null; every field read here is a plain scalar.
    let (media_type, codec_id, width, height, sample_rate, channels, default) = unsafe {
        let parameters = (*stream).codecpar;
        (
            (*parameters).codec_type,
            (*parameters).codec_id,
            (*parameters).width,
            (*parameters).height,
            (*parameters).sample_rate,
            (*parameters).ch_layout.nb_channels,
            ((*stream).disposition & ffi::AV_DISPOSITION_DEFAULT as c_int) != 0,
        )
    };

    let format = match media_type {
        ffi::AVMEDIA_TYPE_VIDEO => StreamFormat::Video {
            codec: video_codec(codec_id),
            width: width.unsigned_abs(),
            height: height.unsigned_abs(),
            // SAFETY: as above; `avg_frame_rate` is a plain field.
            frame_rate: frame_rate(unsafe { (*stream).avg_frame_rate }),
        },
        ffi::AVMEDIA_TYPE_AUDIO => StreamFormat::Audio {
            codec: audio_codec(codec_id),
            sample_rate: sample_rate.unsigned_abs(),
            channels: u16::try_from(channels).unwrap_or(0),
        },
        _ => StreamFormat::Other,
    };

    let mut described = SourceStream::new(index, format, codec_name(codec_id))
        .with_extradata(extradata(stream))
        .with_default_flag(default);
    if let Some(name) = metadata(stream, c"title") {
        described = described.with_name(name);
    }
    if let Some(language) = metadata(stream, c"language") {
        described = described.with_language(language);
    }
    described
}

/// The video codec `clipped-muxer` knows this identifier as, if it knows one.
fn video_codec(codec_id: ffi::AVCodecID) -> Option<VideoCodec> {
    [VideoCodec::H264, VideoCodec::Hevc, VideoCodec::Av1]
        .into_iter()
        .find(|codec| codec.ffmpeg_name() == codec_name(codec_id))
}

/// The audio codec `clipped-muxer` knows this identifier as, if it knows one.
fn audio_codec(codec_id: ffi::AVCodecID) -> Option<AudioCodec> {
    [AudioCodec::PcmS16Le, AudioCodec::Aac, AudioCodec::Opus]
        .into_iter()
        .find(|codec| codec.ffmpeg_name() == codec_name(codec_id))
}

/// The name FFmpeg's own descriptor table has for a codec.
fn codec_name(codec_id: ffi::AVCodecID) -> String {
    // SAFETY: `avcodec_get_name` accepts any value of `AVCodecID` and returns a
    // pointer to a string constant inside libavcodec — the descriptor's name,
    // or a literal for an unknown identifier — which is NUL-terminated and
    // lives for the process.
    let name = unsafe { CStr::from_ptr(ffi::avcodec_get_name(codec_id)) };
    name.to_string_lossy().into_owned()
}

/// A stream's out-of-band header, copied out of the container.
fn extradata(stream: *mut ffi::AVStream) -> Vec<u8> {
    // SAFETY: `stream` is live and owns its parameters. `extradata` is either
    // null or points at `extradata_size` readable bytes the parameters own; the
    // bytes are copied here and the pointer is not kept.
    unsafe {
        let parameters = (*stream).codecpar;
        let size = usize::try_from((*parameters).extradata_size).unwrap_or(0);
        if (*parameters).extradata.is_null() || size == 0 {
            return Vec::new();
        }
        core::slice::from_raw_parts((*parameters).extradata, size).to_vec()
    }
}

/// One metadata entry of a stream, where it has one.
fn metadata(stream: *mut ffi::AVStream, key: &CStr) -> Option<String> {
    // SAFETY: `stream` is live and owns its metadata dictionary. `av_dict_get`
    // reads the NUL-terminated key and returns either null or a pointer to an
    // entry the dictionary owns, whose `value` is a NUL-terminated string. The
    // string is copied here and the entry is not kept.
    unsafe {
        let entry = ffi::av_dict_get((*stream).metadata, key.as_ptr(), ptr::null(), 0);
        if entry.is_null() || (*entry).value.is_null() {
            return None;
        }
        Some(
            CStr::from_ptr((*entry).value)
                .to_string_lossy()
                .into_owned(),
        )
    }
}

/// A frame rate FFmpeg declared, where it declared a real one.
fn frame_rate(rate: ffi::AVRational) -> Option<FrameRate> {
    let numerator = u32::try_from(rate.num).ok()?;
    let denominator = u32::try_from(rate.den).ok()?;
    FrameRate::new(numerator, denominator)
}

/// The first of two timestamps that is really there.
const fn first_timestamp(preferred: i64, fallback: i64) -> Option<i64> {
    if preferred != AV_NOPTS_VALUE {
        Some(preferred)
    } else if fallback != AV_NOPTS_VALUE {
        Some(fallback)
    } else {
        None
    }
}

/// Converts a timestamp from one time base into another.
///
/// FFmpeg's own rounding — to nearest, away from zero on a tie — so that a
/// timestamp read here is the one `ffmpeg` would have read, and so that
/// truncation cannot drag every timestamp towards the start of the file.
fn rescale(ticks: i64, from: ffi::AVRational, to: ffi::AVRational) -> i64 {
    // A time base that is not a positive fraction of a second is not a time
    // base, and would make the conversion below divide by zero. It comes from a
    // file somebody else wrote, so it is checked rather than trusted.
    if from.num <= 0 || from.den <= 0 || to.num <= 0 || to.den <= 0 {
        return ticks;
    }

    // SAFETY: `av_rescale_q_rnd` is pure arithmetic over the values it is given
    // and reads no state at all. Both fractions have just been checked to be
    // positive, so nothing divides by zero, and the rounding argument is a
    // combination of two of libavutil's own flags.
    unsafe {
        ffi::av_rescale_q_rnd(
            ticks,
            from,
            to,
            ffi::AV_ROUND_NEAR_INF | ffi::AV_ROUND_PASS_MINMAX,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_packet_missing_one_timestamp_borrows_the_other_rather_than_guessing() {
        // Matroska stores no decode timestamp for a stream that does not
        // reorder, so half the packets in a recording arrive this way. Treating
        // the absent one as zero would put every such packet at the start of
        // the recording, and every cut would then be measured against the wrong
        // moment.
        assert_eq!(first_timestamp(1_000, AV_NOPTS_VALUE), Some(1_000));
        assert_eq!(first_timestamp(AV_NOPTS_VALUE, 1_000), Some(1_000));
        assert_eq!(first_timestamp(AV_NOPTS_VALUE, AV_NOPTS_VALUE), None);
        // A negative timestamp is a real one — Opus carries its pre-skip as one
        // — and must not be mistaken for an absent one.
        assert_eq!(first_timestamp(-7, AV_NOPTS_VALUE), Some(-7));
    }

    #[test]
    fn timestamps_are_converted_into_the_nanoseconds_a_document_counts_in() {
        let milliseconds = ffi::AVRational { num: 1, den: 1000 };
        let sample_rate = ffi::AVRational {
            num: 1,
            den: 48_000,
        };

        assert_eq!(rescale(1, milliseconds, NANOSECONDS), 1_000_000);
        assert_eq!(rescale(48_000, sample_rate, NANOSECONDS), 1_000_000_000);
        // Half a tick rounds away from zero in both directions, which is what
        // `av_rescale_q` does and therefore what FFmpeg's own tools would read.
        assert_eq!(rescale(500_000, NANOSECONDS, milliseconds), 1);
        assert_eq!(rescale(-500_000, NANOSECONDS, milliseconds), -1);
    }

    #[test]
    fn a_time_base_that_is_not_a_fraction_of_a_second_leaves_the_timestamp_alone() {
        // Unreachable through a container FFmpeg opened, but the arithmetic
        // would divide by zero rather than fail, and an export must not end in
        // a panic (AGENTS.md section 15).
        let broken = ffi::AVRational { num: 0, den: 0 };
        assert_eq!(rescale(42, broken, NANOSECONDS), 42);
        assert_eq!(rescale(42, NANOSECONDS, broken), 42);
    }

    #[test]
    fn the_codecs_a_container_writer_knows_are_recognised_by_the_name_ffmpeg_gives_them() {
        // Matched against FFmpeg's own descriptor table rather than against a
        // second list of identifiers written down here, which would be a
        // mapping to get wrong in a way nothing would notice until an export
        // refused a recording Clipped had just made.
        assert_eq!(video_codec(ffi::AV_CODEC_ID_H264), Some(VideoCodec::H264));
        assert_eq!(video_codec(ffi::AV_CODEC_ID_HEVC), Some(VideoCodec::Hevc));
        assert_eq!(video_codec(ffi::AV_CODEC_ID_AV1), Some(VideoCodec::Av1));
        assert_eq!(video_codec(ffi::AV_CODEC_ID_VP9), None);

        assert_eq!(
            audio_codec(ffi::AV_CODEC_ID_PCM_S16LE),
            Some(AudioCodec::PcmS16Le)
        );
        assert_eq!(audio_codec(ffi::AV_CODEC_ID_OPUS), Some(AudioCodec::Opus));
        assert_eq!(audio_codec(ffi::AV_CODEC_ID_AAC), Some(AudioCodec::Aac));
        assert_eq!(audio_codec(ffi::AV_CODEC_ID_VORBIS), None);
    }

    #[test]
    fn a_frame_rate_the_container_did_not_declare_is_none_rather_than_zero() {
        assert_eq!(
            frame_rate(ffi::AVRational { num: 60, den: 1 }),
            FrameRate::per_second(60)
        );
        assert_eq!(frame_rate(ffi::AVRational { num: 0, den: 0 }), None);
        assert_eq!(frame_rate(ffi::AVRational { num: 30, den: 0 }), None);
    }
}
