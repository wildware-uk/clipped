//! Writing a recording into Matroska, incrementally, as the packets arrive.
//!
//! MKV was chosen for what happens when things go wrong (ADR 0001): a
//! recording killed halfway through has to remain a playable recording of
//! everything up to the kill, because the alternative is telling somebody that
//! the two hours they cannot recreate are an unreadable file. That single
//! requirement decides most of what this module does, and `docs/muxing.md`
//! sets out the whole design; what follows is the reasoning attached to the
//! code it explains.
//!
//! # What is written when
//!
//! Nothing important waits for the end of the session:
//!
//! - Packets go to `av_interleaved_write_frame`, which orders them across
//!   tracks and hands each one to the Matroska muxer as soon as it is safe to
//!   write. The muxer accumulates a cluster and emits it at the latest when
//!   [`CLUSTER_TIME_LIMIT_MS`] of media has gone into it — sooner at a keyframe
//!   or a size limit — so at most that much is in memory rather than on disk.
//! - `AVFMT_FLAG_FLUSH_PACKETS` makes libavformat flush its own 32 KiB I/O
//!   buffer to the operating system after every packet, so a cluster that has
//!   been emitted is a cluster that has reached the file rather than one
//!   sitting in a buffer that a killed process takes with it.
//! - The header carries every track's codec, name and language, so a truncated
//!   file still describes itself.
//!
//! What is *not* written until [`MkvWriter::finish`] is the segment length, the
//! duration and the cue index. A file missing those still opens: the segment
//! reads as unknown-length, which is the same construct a live stream is
//! written with. That is verified with FFmpeg's own demuxer and no other —
//! `crates/muxer/tests/abrupt_termination.rs` kills a writer mid-recording and
//! decodes what is left with the pinned build's `ffprobe`. What other players
//! do with such a file has not been tested here.
//!
//! # Ownership
//!
//! One `AVFormatContext`, one `AVIOContext` and one `AVPacket` per writer, each
//! owned by a Rust value whose `Drop` releases it (AGENTS.md section 58). The
//! writer is not `Sync`, and is `Send` so that a session can create it on one
//! thread and run it on the muxing thread.

use core::fmt;
use core::ptr::{self, NonNull};
use core::time::Duration;
use std::ffi::{c_int, CString};
use std::path::{Path, PathBuf};

use clipped_logging::RedactedPath;
use rusty_ffmpeg::ffi;
use tracing::{debug, info, warn};

use crate::error::{AvError, MuxError};
use crate::linkage;
use crate::packet::EncodedPacket;
use crate::timeline::{TimeBase, TimestampFixes, TrackTimeline};
use crate::track::{AudioCodec, AudioTrack, RecordingLayout, TrackId, VideoCodec, VideoTrack};

/// FFmpeg's name for the Matroska muxer, as `ffmpeg -muxers` lists it.
///
/// Named explicitly rather than left to be guessed from the file extension:
/// what container a recording is written in is a decision (ADR 0001), not a
/// consequence of what somebody typed after the dot. One spelling, used both to
/// check the linked build has the muxer and to allocate the context, so the two
/// cannot drift into asking about one container and writing another.
const MATROSKA_MUXER: &core::ffi::CStr = c"matroska";

/// How much media the Matroska muxer may accumulate in one cluster, in
/// milliseconds.
///
/// This is the size of the window an abrupt termination costs, and it is the
/// most consequential number in this file. A cluster does not reach the disk
/// until it is closed, so whatever is in the open one when the process dies is
/// lost whole.
///
/// FFmpeg's Matroska muxer closes a cluster at the first of three things: a
/// keyframe on the video track, `cluster_size_limit` bytes, or
/// `cluster_time_limit` milliseconds of media. Both limits are `-1` in the
/// option table and are replaced with 5 MB and 5000 ms while the header is
/// written, so the default window is *not* the keyframe interval alone — it is
/// bounded above at five seconds. Setting this to 1000 moves that ceiling to
/// one second and takes the encoder's keyframe interval out of the answer
/// entirely, which is what makes the loss a property of the recorder rather
/// than of whatever the encoder was tuned for.
///
/// Measured on the pinned build, counting Matroska cluster elements in a
/// 30-second file at 30 fps with keyframes 10 seconds apart:
///
/// | `cluster_time_limit` | Clusters | Starting at (ms) |
/// | --- | --- | --- |
/// | FFmpeg's default | 6 | 0, 5033, 10000, 15033, 20000, 25033 |
/// | 1000 | 30 | 0, 1033, 2067, 3100, … one a second |
///
/// One second is the compromise. The cost is a cluster header — a few dozen
/// bytes — every second rather than every five, which against a recording
/// running at tens of megabits does not register. `docs/muxing.md` records the
/// commands, and what a killed recorder actually recovers with each setting.
const CLUSTER_TIME_LIMIT_MS: i64 = 1000;

/// A container context, and the file it is writing to.
///
/// Both are released here, in one place, so that every early return in
/// [`MkvWriter::create`] cleans up by returning rather than by remembering to.
struct FormatContext(NonNull<ffi::AVFormatContext>);

impl FormatContext {
    /// Allocates an output context for the Matroska muxer.
    fn allocate_matroska() -> Result<Self, MuxError> {
        let mut context: *mut ffi::AVFormatContext = ptr::null_mut();

        // SAFETY: `avformat_alloc_output_context2` writes an allocated context
        // through its first argument, which is a live local. The format is
        // named by the NUL-terminated literal rather than by a filename, so the
        // remaining two arguments are legitimately null. On failure it leaves
        // the pointer null, which is checked below before it is used.
        let code = unsafe {
            ffi::avformat_alloc_output_context2(
                &mut context,
                ptr::null(),
                MATROSKA_MUXER.as_ptr(),
                ptr::null(),
            )
        };

        match NonNull::new(context) {
            // A negative code with a context allocated should not happen, but
            // trusting the pointer over the code would be a leak at best.
            Some(context) if code >= 0 => Ok(Self(context)),
            Some(context) => {
                // SAFETY: `context` was allocated by the call above and has not
                // been handed to anything else, so freeing it here is the only
                // release of it.
                unsafe { ffi::avformat_free_context(context.as_ptr()) };
                Err(MuxError::Ffmpeg {
                    operation: "allocating a Matroska output context",
                    source: AvError::new(code),
                })
            }
            None => Err(MuxError::Ffmpeg {
                operation: "allocating a Matroska output context",
                source: AvError::new(code),
            }),
        }
    }

    /// The context, for passing to FFmpeg.
    fn as_ptr(&self) -> *mut ffi::AVFormatContext {
        self.0.as_ptr()
    }

    /// Opens `url` for writing and attaches it to the context.
    fn open_output(&mut self, url: &str) -> Result<(), MuxError> {
        let Ok(url) = CString::new(url) else {
            // Only reachable from a path containing a NUL, which no filesystem
            // permits; the caller has a byte string that is not a path.
            return Err(MuxError::Ffmpeg {
                operation: "opening the output file",
                source: AvError::new(-(ffi::EINVAL as i32)),
            });
        };

        // SAFETY: `avio_open` writes the opened context through its first
        // argument, which points at the context's own `pb` field, and reads
        // `url` as a NUL-terminated string that outlives the call. `pb` is null
        // beforehand — nothing else assigns it — so nothing is overwritten and
        // leaked. Ownership of what it writes passes to this value, which
        // closes it in `Drop`.
        let code = unsafe {
            ffi::avio_open(
                &mut (*self.as_ptr()).pb,
                url.as_ptr(),
                ffi::AVIO_FLAG_WRITE as c_int,
            )
        };

        if code < 0 {
            return Err(MuxError::Ffmpeg {
                operation: "opening the output file",
                source: AvError::new(code),
            });
        }
        Ok(())
    }
}

impl Drop for FormatContext {
    fn drop(&mut self) {
        // SAFETY: this value owns both resources and nothing else has a pointer
        // to either. `avio_closep` flushes and closes the file and nulls the
        // field, so a context whose output was never opened — an error between
        // allocating and opening — skips it and frees only the context.
        unsafe {
            let context = self.as_ptr();
            if !(*context).pb.is_null() {
                ffi::avio_closep(&mut (*context).pb);
            }
            ffi::avformat_free_context(context);
        }
    }
}

/// The one packet structure a writer reuses for every packet it writes.
///
/// A recorder writes one of these per frame for hours, so it is allocated once
/// (AGENTS.md section 18). It never owns a payload: the bytes belong to the
/// caller for the length of the call, and FFmpeg takes its own reference to
/// them, as [`MkvWriter::write_packet`] explains.
struct PacketSlot(NonNull<ffi::AVPacket>);

impl PacketSlot {
    fn allocate() -> Result<Self, MuxError> {
        // SAFETY: `av_packet_alloc` takes no arguments and returns either null
        // or a packet this value then owns.
        let packet = unsafe { ffi::av_packet_alloc() };
        NonNull::new(packet).map(Self).ok_or(MuxError::Ffmpeg {
            operation: "allocating a packet",
            source: AvError::new(-(ffi::ENOMEM as i32)),
        })
    }

    fn as_ptr(&self) -> *mut ffi::AVPacket {
        self.0.as_ptr()
    }
}

impl Drop for PacketSlot {
    fn drop(&mut self) {
        let mut packet = self.as_ptr();
        // SAFETY: this value owns the packet and nothing else holds a pointer
        // to it. `av_packet_free` unreferences whatever the packet holds and
        // nulls the local pointer.
        unsafe { ffi::av_packet_free(&mut packet) };
    }
}

/// One track of the file being written.
struct TrackState {
    id: TrackId,
    stream_index: c_int,
    timeline: TrackTimeline,
}

/// What a finished recording turned out to contain.
///
/// Worth returning rather than logging alone: the session that owns the
/// recording writes some of this into the library, and the media tests assert
/// on it (AGENTS.md section 22).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RecordingSummary {
    /// How many packets were written, across every track.
    pub packets: u64,
    /// The span between the first and last timestamps written.
    ///
    /// Not quite the playing time — the last packet also has a duration — but
    /// the number to compare against how long the session ran.
    pub duration: Duration,
    /// Packets whose timestamps preceded the start of the file and were
    /// clamped to it.
    pub timestamps_clamped_to_start: u64,
    /// Packets whose decode timestamps did not advance and were forced
    /// forward. Anything above a handful means something upstream is
    /// reordering or a clock has stepped.
    pub timestamps_forced_monotonic: u64,
    /// Packets that would have been presented before they were decoded.
    pub timestamps_presented_before_decoded: u64,
}

impl RecordingSummary {
    /// How many packets needed a timestamp changed to keep the file valid.
    #[must_use]
    pub const fn timestamps_corrected(&self) -> u64 {
        self.timestamps_clamped_to_start
            + self.timestamps_forced_monotonic
            + self.timestamps_presented_before_decoded
    }
}

impl fmt::Display for RecordingSummary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} packets over {:.3}s, {} timestamps corrected",
            self.packets,
            self.duration.as_secs_f64(),
            self.timestamps_corrected()
        )
    }
}

/// Writes encoded packets into an MKV file as they arrive.
///
/// # Example
///
/// ```no_run
/// use std::path::Path;
/// use clipped_muxer::{
///     AudioCodec, AudioTrack, EncodedPacket, Language, MkvWriter, PacketTimestamp,
///     RecordingLayout, TrackId, VideoCodec, VideoTrack,
/// };
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let layout = RecordingLayout::new(
///     VideoTrack::new(VideoCodec::H264, 2560, 1440).with_codec_private(sequence_header()),
/// )
/// .with_audio_track(
///     AudioTrack::new(AudioCodec::PcmS16Le, 48_000, 2)
///         .with_name("Game")
///         .with_language(Language::UNDETERMINED),
/// );
///
/// let mut writer = MkvWriter::create(Path::new("recording.mkv"), &layout)?;
/// writer.write_packet(
///     &EncodedPacket::new(TrackId::Video, PacketTimestamp::from_nanos(0), &frame())
///         .with_keyframe(true),
/// )?;
/// let summary = writer.finish()?;
/// println!("{summary}");
/// # Ok(())
/// # }
/// # fn sequence_header() -> Vec<u8> { Vec::new() }
/// # fn frame() -> Vec<u8> { Vec::new() }
/// ```
pub struct MkvWriter {
    format: FormatContext,
    packet: PacketSlot,
    tracks: Vec<TrackState>,
    audio_tracks: usize,
    path: PathBuf,
    /// The timestamp everything in the file is relative to: the earliest one
    /// the first packet carried. `None` until that packet arrives.
    origin_nanos: Option<i64>,
    last_presentation_nanos: i64,
    summary: RecordingSummary,
    warned: TimestampFixes,
    finished: bool,
}

// SAFETY: an `MkvWriter` exclusively owns its `AVFormatContext`, its
// `AVIOContext` and its `AVPacket`; no pointer to any of them escapes, and none
// of them is shared with another writer. FFmpeg attaches no thread affinity to
// a format context — libavformat's contract is that one context is used by one
// thread at a time, which `&mut self` on every operation enforces — so moving a
// writer to the thread that will run it is sound. It is deliberately not
// `Sync`: two threads writing packets into one context concurrently is exactly
// what libavformat does not support.
unsafe impl Send for MkvWriter {}

impl MkvWriter {
    /// Creates `path` and writes the container header for `layout`.
    ///
    /// The tracks are fixed here. Matroska's track entries live in the header,
    /// so a track cannot appear later in the file, and a routing change
    /// mid-session is a new recording rather than a new track.
    ///
    /// # Errors
    ///
    /// [`MuxError::OutputExists`] rather than truncating anything that is
    /// already there (AGENTS.md section 56), [`MuxError::InvalidTrack`] for a
    /// track the container cannot carry, and [`MuxError::Ffmpeg`] for anything
    /// FFmpeg or the filesystem refuses — a directory that does not exist, a
    /// disk with no room, a path with no permission.
    ///
    /// A failure leaves nothing behind: if the output was created before the
    /// failure, it is removed again — and if it cannot be removed, that is
    /// logged at `warn` — so that a caller retrying the same name is not told
    /// it is about to overwrite a recording that is really an empty file from
    /// its own last attempt.
    pub fn create(path: &Path, layout: &RecordingLayout) -> Result<Self, MuxError> {
        // `to_str` cannot fail: the constant is an ASCII literal. Written as a
        // fallback rather than an unwrap so that a recording never ends in a
        // panic (AGENTS.md section 15); an unreadable muxer name is reported as
        // the container being unavailable, which is what it would mean.
        let Ok(matroska) = MATROSKA_MUXER.to_str() else {
            return Err(MuxError::ContainerUnsupported);
        };
        if !linkage::muxer_available(matroska) {
            return Err(MuxError::ContainerUnsupported);
        }

        // Checked rather than left to the open, because the flags that create a
        // file also truncate one, and there is no combination of avio flags
        // that means "create, but fail if it exists". A race between this check
        // and the open is possible and is not what the check is for: the
        // failure being prevented is a session pointed at the name of an
        // existing recording, which is a decision made seconds earlier, not
        // nanoseconds.
        if matches!(path.try_exists(), Ok(true)) {
            return Err(MuxError::OutputExists {
                path: path.to_path_buf(),
            });
        }

        let Some(path_text) = path.to_str() else {
            return Err(MuxError::PathNotRepresentable {
                path: path.to_path_buf(),
            });
        };

        let mut format = FormatContext::allocate_matroska()?;

        let mut tracks = Vec::with_capacity(1 + layout.audio_tracks().len());
        let video_index = add_video_stream(&format, layout.video())?;
        tracks.push((TrackId::Video, video_index));
        for (index, track) in layout.audio_tracks().iter().enumerate() {
            let id = TrackId::Audio(u16::try_from(index).map_err(|_| MuxError::InvalidTrack {
                track: TrackId::Audio(u16::MAX),
                reason: "a recording cannot have more than 65,536 audio tracks",
            })?);
            tracks.push((id, add_audio_stream(&format, id, track)?));
        }

        // `file:` rather than the bare path so that the file protocol is used
        // whatever the path looks like. FFmpeg picks a protocol from the text
        // before the first colon, and while it knows to leave a Windows drive
        // letter alone, a path that begins with something longer would be read
        // as a URL rather than as a file.
        format.open_output(&format!("file:{path_text}"))?;

        // The file exists from here on, so a failure has to take it away again.
        // `create` refuses an output that already exists rather than truncating
        // it, so an empty file left behind by a failed attempt would make that
        // name permanently unusable — a session retrying after a transient
        // failure would be told it was about to overwrite a recording.
        Self::write_header_and_tracks(format, tracks, path, layout).inspect_err(|_| {
            // The context, and with it the open file, was dropped by the call
            // above; Windows would refuse to remove it otherwise.
            if let Err(error) = std::fs::remove_file(path) {
                warn!(
                    path = %RedactedPath::new(path),
                    %error,
                    "a recording could not be started and the empty file left behind could \
                     not be removed; that name cannot be recorded to until it is"
                );
            }
        })
    }

    /// Everything between the output being opened and a usable writer.
    ///
    /// Separated from [`create`](Self::create) only so that every failure after
    /// the file exists leaves through one place, which is where it is removed
    /// again. `format` is taken by value so that a failure here drops it, and
    /// so closes the file, before the caller tries to delete it.
    fn write_header_and_tracks(
        format: FormatContext,
        tracks: Vec<(TrackId, c_int)>,
        path: &Path,
        layout: &RecordingLayout,
    ) -> Result<Self, MuxError> {
        // SAFETY: `format` owns a live context and this is the only reference
        // to it. `flags` is a plain integer field, documented as settable by
        // the caller before `avformat_write_header`.
        unsafe {
            (*format.as_ptr()).flags |= ffi::AVFMT_FLAG_FLUSH_PACKETS as c_int;
        }

        write_header(&format)?;

        let tracks = tracks
            .into_iter()
            .map(|(id, stream_index)| {
                let time_base =
                    stream_time_base(&format, stream_index).ok_or(MuxError::InvalidTrack {
                        track: id,
                        reason: "the container declared a time base that is not a positive \
                                 fraction of a second",
                    })?;
                Ok(TrackState {
                    id,
                    stream_index,
                    timeline: TrackTimeline::new(time_base),
                })
            })
            .collect::<Result<Vec<_>, MuxError>>()?;

        info!(
            path = %RedactedPath::new(path),
            video_codec = layout.video().codec().ffmpeg_name(),
            width = layout.video().width(),
            height = layout.video().height(),
            audio_tracks = layout.audio_tracks().len(),
            "recording file opened"
        );

        Ok(Self {
            format,
            packet: PacketSlot::allocate()?,
            audio_tracks: tracks.len() - 1,
            tracks,
            path: path.to_path_buf(),
            origin_nanos: None,
            last_presentation_nanos: 0,
            summary: RecordingSummary::default(),
            warned: TimestampFixes::default(),
            finished: false,
        })
    }

    /// The file being written.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// What has been written so far.
    ///
    /// The same figures [`finish`](Self::finish) returns, readable during a
    /// session so that a recorder can report progress without ending it.
    #[must_use]
    pub fn summary(&self) -> RecordingSummary {
        let mut summary = self.summary;
        summary.duration = self.duration();
        summary
    }

    /// Writes one encoded packet.
    ///
    /// The first packet written establishes the file's origin: every timestamp
    /// after it is stored relative to the earlier of that packet's decode and
    /// presentation timestamps. That is what lets a caller pass performance
    /// counter readings — nanoseconds since the machine booted — without the
    /// recording starting several years in.
    ///
    /// Timestamps are converted into the container's own units and forced to
    /// increase; [`crate::timeline`] documents what is corrected and why
    /// nothing is dropped.
    ///
    /// # Errors
    ///
    /// [`MuxError::UnknownTrack`] for a track the layout did not declare,
    /// [`MuxError::EmptyPacket`] for a packet with no bytes, and
    /// [`MuxError::Ffmpeg`] when the write itself fails — which for a recorder
    /// running for hours usually means the disk filled up or the drive was
    /// disconnected (AGENTS.md section 16).
    pub fn write_packet(&mut self, packet: &EncodedPacket<'_>) -> Result<(), MuxError> {
        let data = packet.data();
        if data.is_empty() {
            return Err(MuxError::EmptyPacket {
                track: packet.track(),
            });
        }
        let size = c_int::try_from(data.len()).map_err(|_| MuxError::PacketTooLarge {
            track: packet.track(),
            bytes: data.len(),
        })?;

        let presentation_nanos = packet.presentation().as_nanos();
        let decode_nanos = packet.decode().as_nanos();
        let origin = *self
            .origin_nanos
            .get_or_insert(presentation_nanos.min(decode_nanos));

        let track = self.track_mut(packet.track())?;
        let stamps = track.timeline.stamp(
            presentation_nanos.saturating_sub(origin),
            decode_nanos.saturating_sub(origin),
            packet
                .duration()
                // A duration too large for 64 bits of nanoseconds is 584 years
                // of packet, so this saturates rather than growing a variant
                // for it; the timeline rescales whatever it is given and the
                // container's own limit catches the rest.
                .map(|duration| u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)),
        );
        let stream_index = track.stream_index;

        let slot = self.packet.as_ptr();
        // SAFETY: `slot` is this writer's own live packet. It is unreferenced
        // first so that nothing from the previous write survives into this one,
        // then filled with plain scalar fields and a borrowed payload. The
        // payload pointer is cast to `*mut` because that is the field's type;
        // `av_interleaved_write_frame` does not write through it, and the
        // borrow lives until the end of this function, which is past the call.
        unsafe {
            ffi::av_packet_unref(slot);
            (*slot).data = data.as_ptr().cast_mut();
            (*slot).size = size;
            (*slot).stream_index = stream_index;
            (*slot).pts = stamps.presentation;
            (*slot).dts = stamps.decode;
            (*slot).duration = stamps.duration;
            (*slot).flags = if packet.is_keyframe() {
                ffi::AV_PKT_FLAG_KEY as c_int
            } else {
                0
            };
        }

        // SAFETY: both pointers are live and exclusively owned. This call takes
        // ownership of the packet's contents: because the packet is not
        // reference counted — its payload is borrowed from the caller —
        // libavformat copies the bytes into a buffer of its own before it
        // queues them, which is the one copy per packet this design accepts in
        // exchange for callers not having to allocate FFmpeg buffers. The
        // packet is left unreferenced whether the call succeeds or fails, and
        // unreferencing a packet whose payload it does not own frees nothing.
        let code = unsafe { ffi::av_interleaved_write_frame(self.format.as_ptr(), slot) };
        if code < 0 {
            return Err(MuxError::Ffmpeg {
                operation: "writing a packet",
                source: AvError::new(code),
            });
        }

        self.summary.packets += 1;
        self.last_presentation_nanos = self
            .last_presentation_nanos
            .max(presentation_nanos.saturating_sub(origin));
        self.record_fixes(packet.track(), stamps.fixes);

        Ok(())
    }

    /// Finishes the file: flushes what is queued and writes the trailer.
    ///
    /// The trailer is where the segment length, the duration and the cue index
    /// go. A recording that never reaches this point is still playable — that
    /// is the whole reason for the container — but it is this call that makes
    /// it seekable and gives it a duration a library can read without scanning
    /// it.
    ///
    /// # Errors
    ///
    /// [`MuxError::Ffmpeg`] when the trailer cannot be written, which at this
    /// stage means the disk filled or the drive went away. The file is closed
    /// either way, and what was written before the failure remains.
    pub fn finish(mut self) -> Result<RecordingSummary, MuxError> {
        let summary = self.summary();

        // SAFETY: the context is live, the header was written when this value
        // was created, and this runs at most once — `finished` stops `Drop`
        // from repeating it.
        let code = unsafe { ffi::av_write_trailer(self.format.as_ptr()) };
        self.finished = true;

        if code < 0 {
            return Err(MuxError::Ffmpeg {
                operation: "writing the container trailer",
                source: AvError::new(code),
            });
        }

        info!(
            path = %RedactedPath::new(&self.path),
            packets = summary.packets,
            duration_ms = summary.duration.as_millis(),
            timestamps_corrected = summary.timestamps_corrected(),
            "recording file finished"
        );

        Ok(summary)
    }

    /// The span between the first and the last timestamp written.
    fn duration(&self) -> Duration {
        Duration::from_nanos(self.last_presentation_nanos.max(0).unsigned_abs())
    }

    /// The track a packet named, or an error naming what the file does have.
    fn track_mut(&mut self, id: TrackId) -> Result<&mut TrackState, MuxError> {
        let audio_tracks = self.audio_tracks;
        self.tracks
            .iter_mut()
            .find(|track| track.id == id)
            .ok_or(MuxError::UnknownTrack {
                track: id,
                audio_tracks,
            })
    }

    /// Counts a correction, and says so the first time each kind happens.
    ///
    /// Logged once rather than per packet because the pathological case is
    /// thousands in a row, and a recorder that spends a session writing a log
    /// line per frame has become the problem it was reporting. The count in the
    /// summary is what says how bad it was.
    fn record_fixes(&mut self, track: TrackId, fixes: TimestampFixes) {
        if !fixes.any() {
            return;
        }

        if fixes.before_start {
            self.summary.timestamps_clamped_to_start += 1;
            if !self.warned.before_start {
                self.warned.before_start = true;
                warn!(
                    %track,
                    "a packet was timed before the start of the recording and was clamped \
                     to it; the tracks did not start together"
                );
            }
        }

        if fixes.not_monotonic {
            self.summary.timestamps_forced_monotonic += 1;
            if !self.warned.not_monotonic {
                self.warned.not_monotonic = true;
                warn!(
                    %track,
                    "a packet's decode timestamp did not advance and was forced forward; \
                     something upstream reordered packets or stepped its clock"
                );
            }
        }

        if fixes.presented_before_decoded {
            self.summary.timestamps_presented_before_decoded += 1;
            if !self.warned.presented_before_decoded {
                self.warned.presented_before_decoded = true;
                warn!(
                    %track,
                    "a packet would have been presented before it was decoded; its \
                     presentation timestamp was raised to match"
                );
            }
        }
    }
}

impl Drop for MkvWriter {
    /// Finishes the file if [`finish`](MkvWriter::finish) did not.
    ///
    /// A session that fails or panics still drops its writer, and a recording
    /// finalised on the way out is strictly better for the person who made it
    /// than one left without an index (AGENTS.md section 17). Failure here can
    /// only be logged, which is the reason `finish` exists as well.
    fn drop(&mut self) {
        if self.finished {
            return;
        }

        // SAFETY: as in `finish`; the context is live and the trailer is
        // written at most once.
        let code = unsafe { ffi::av_write_trailer(self.format.as_ptr()) };
        if code < 0 {
            warn!(
                path = %RedactedPath::new(&self.path),
                error = %AvError::new(code),
                "the recording was dropped without being finished and its trailer could \
                 not be written; the file remains playable but has no index"
            );
        } else {
            debug!(
                path = %RedactedPath::new(&self.path),
                "the recording was finished while being dropped"
            );
        }
    }
}

impl fmt::Debug for MkvWriter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The path is deliberately absent: it contains the account name, and a
        // `Debug` string ends up in logs and panic messages (docs/logging.md).
        formatter
            .debug_struct("MkvWriter")
            .field("tracks", &self.tracks.len())
            .field("packets", &self.summary.packets)
            .field("finished", &self.finished)
            .finish()
    }
}

/// Why a video track with no out-of-band header cannot be written.
///
/// Matroska's track entry for each of these carries a mandatory `CodecPrivate`
/// describing the bitstream, so a recording without it is not decodable from
/// anywhere but the very start of a stream that happens to repeat its headers
/// in band.
const fn video_header_missing(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => {
            "an H.264 track needs its sequence and picture parameter sets (SPS and PPS) as \
             the codec's out-of-band header, and none was given"
        }
        VideoCodec::Hevc => {
            "an HEVC track needs its video, sequence and picture parameter sets (VPS, SPS \
             and PPS) as the codec's out-of-band header, and none was given"
        }
        VideoCodec::Av1 => {
            "an AV1 track needs its sequence header as the codec's out-of-band header, and \
             none was given"
        }
    }
}

/// Why an audio track with no out-of-band header cannot be written.
const fn audio_header_missing(codec: AudioCodec) -> &'static str {
    match codec {
        AudioCodec::Aac => {
            "an AAC track needs its AudioSpecificConfig as the codec's out-of-band header, \
             and none was given"
        }
        AudioCodec::Opus => {
            "an Opus track needs its identification header as the codec's out-of-band \
             header, and none was given"
        }
        // Uncompressed audio needs nothing, so `requires_codec_private` keeps
        // this arm unreachable. A general message rather than a panic: getting
        // here would mean the two functions had drifted apart, and a slightly
        // vague error is a better outcome than a recording that ends in a
        // panic.
        AudioCodec::PcmS16Le => {
            "this audio track needs the codec's out-of-band header, and none was given"
        }
    }
}

/// Adds the video stream to the context, returning its index.
fn add_video_stream(format: &FormatContext, track: &VideoTrack) -> Result<c_int, MuxError> {
    if track.width() == 0 || track.height() == 0 {
        return Err(MuxError::InvalidTrack {
            track: TrackId::Video,
            reason: "a video track with no width or no height cannot be described to the \
                     container",
        });
    }
    if track.codec_private().is_empty() {
        // FFmpeg refuses this too, with `Invalid data found when processing
        // input` from `avformat_write_header` — a message that names neither
        // the track nor the missing header, and that arrives after the file has
        // been created. Every hardware encoder can be asked for its header
        // separately from the bitstream, so this is a caller that has not asked
        // (AGENTS.md section 15).
        return Err(MuxError::InvalidTrack {
            track: TrackId::Video,
            reason: video_header_missing(track.codec()),
        });
    }
    let width = c_int::try_from(track.width()).map_err(|_| MuxError::InvalidTrack {
        track: TrackId::Video,
        reason: "the picture is wider than any codec allows",
    })?;
    let height = c_int::try_from(track.height()).map_err(|_| MuxError::InvalidTrack {
        track: TrackId::Video,
        reason: "the picture is taller than any codec allows",
    })?;

    let stream = new_stream(format, TrackId::Video)?;

    // SAFETY: `stream` points at a stream owned by the context, which outlives
    // it, and `codecpar` is allocated with it and never null. Every field
    // assigned below is a plain scalar the caller is expected to fill in before
    // `avformat_write_header`.
    unsafe {
        let parameters = (*stream).codecpar;
        (*parameters).codec_type = ffi::AVMEDIA_TYPE_VIDEO;
        (*parameters).codec_id = track.codec().av_codec_id();
        (*parameters).width = width;
        (*parameters).height = height;

        // A hint: the muxer replaces it with the unit Matroska counts in, and
        // the writer reads back what it chose rather than assuming.
        (*stream).time_base = ffi::AVRational {
            num: 1,
            den: 1_000_000_000,
        };

        if let Some(frame_rate) = track.frame_rate() {
            (*stream).avg_frame_rate = frame_rate.as_rational();
            (*parameters).framerate = frame_rate.as_rational();
        }
    }

    set_codec_private(stream, TrackId::Video, track.codec_private())?;
    set_track_metadata(
        stream,
        TrackId::Video,
        track.name(),
        track.language(),
        false,
    )?;

    // SAFETY: as above; `index` is filled in by `avformat_new_stream`.
    Ok(unsafe { (*stream).index })
}

/// Adds one audio stream to the context, returning its index.
fn add_audio_stream(
    format: &FormatContext,
    id: TrackId,
    track: &AudioTrack,
) -> Result<c_int, MuxError> {
    if track.sample_rate() == 0 {
        return Err(MuxError::InvalidTrack {
            track: id,
            reason: "an audio track with no sampling rate cannot be described to the container",
        });
    }
    if track.channels() == 0 {
        return Err(MuxError::InvalidTrack {
            track: id,
            reason: "an audio track with no channels cannot be described to the container",
        });
    }
    if track.codec().requires_codec_private() && track.codec_private().is_empty() {
        return Err(MuxError::InvalidTrack {
            track: id,
            reason: audio_header_missing(track.codec()),
        });
    }
    let sample_rate = c_int::try_from(track.sample_rate()).map_err(|_| MuxError::InvalidTrack {
        track: id,
        reason: "the sampling rate is higher than any codec allows",
    })?;
    let channels = c_int::from(track.channels());

    let stream = new_stream(format, id)?;

    // SAFETY: as in `add_video_stream`. `av_channel_layout_default` fills the
    // parameters' own layout with the conventional arrangement for that many
    // channels; the layout it replaces is the zeroed one `avformat_new_stream`
    // left, which holds no allocation, and the context frees the result with
    // the stream.
    unsafe {
        let parameters = (*stream).codecpar;
        (*parameters).codec_type = ffi::AVMEDIA_TYPE_AUDIO;
        (*parameters).codec_id = track.codec().av_codec_id();
        (*parameters).sample_rate = sample_rate;
        ffi::av_channel_layout_default(&mut (*parameters).ch_layout, channels);

        if let Some(bits) = track.codec().bits_per_sample() {
            // Matroska records this as `BitDepth`, and for uncompressed audio a
            // player has no other way to know how wide a sample is.
            (*parameters).bits_per_coded_sample = c_int::from(bits);
            (*parameters).block_align = channels * c_int::from(bits) / 8;
        }

        (*stream).time_base = ffi::AVRational {
            num: 1,
            den: 1_000_000_000,
        };
    }

    set_codec_private(stream, id, track.codec_private())?;
    set_track_metadata(
        stream,
        id,
        track.name(),
        track.language(),
        track.is_default(),
    )?;

    // SAFETY: as above.
    Ok(unsafe { (*stream).index })
}

/// Adds a stream to the context.
fn new_stream(format: &FormatContext, id: TrackId) -> Result<*mut ffi::AVStream, MuxError> {
    // SAFETY: the context is live and owns whatever this returns; passing a
    // null codec is the documented way to add a stream that will be described
    // by its `codecpar` rather than by an encoder.
    let stream = unsafe { ffi::avformat_new_stream(format.as_ptr(), ptr::null()) };
    if stream.is_null() {
        return Err(MuxError::InvalidTrack {
            track: id,
            reason: "there was not enough memory to add the track to the container",
        });
    }
    Ok(stream)
}

/// Copies a codec's out-of-band header into the stream's parameters.
///
/// FFmpeg frees `extradata` with the stream, so the copy has to be made with
/// FFmpeg's allocator and handed over rather than borrowed from the layout.
/// The trailing padding is FFmpeg's own requirement: its bitstream readers read
/// in words and are allowed to run past the end.
fn set_codec_private(
    stream: *mut ffi::AVStream,
    id: TrackId,
    header: &[u8],
) -> Result<(), MuxError> {
    if header.is_empty() {
        return Ok(());
    }
    let size = c_int::try_from(header.len()).map_err(|_| MuxError::InvalidTrack {
        track: id,
        reason: "the codec's out-of-band header is implausibly large",
    })?;

    let padded = header.len() + ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize;

    // SAFETY: `av_malloc` returns either null or a block of at least `padded`
    // bytes, which is checked below. The copy writes exactly `header.len()`
    // bytes from a live slice into the start of that block, and the padding
    // after it is zeroed, so every byte handed to FFmpeg has been written.
    // Ownership passes to `codecpar`, which frees it with the stream.
    unsafe {
        let buffer = ffi::av_malloc(padded).cast::<u8>();
        if buffer.is_null() {
            return Err(MuxError::InvalidTrack {
                track: id,
                reason: "there was not enough memory for the codec's out-of-band header",
            });
        }
        ptr::copy_nonoverlapping(header.as_ptr(), buffer, header.len());
        ptr::write_bytes(
            buffer.add(header.len()),
            0,
            ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize,
        );

        let parameters = (*stream).codecpar;
        (*parameters).extradata = buffer;
        (*parameters).extradata_size = size;
    }

    Ok(())
}

/// Writes the track's name, language and default flag into its metadata.
///
/// Matroska stores all three per track, which is what makes a multi-track
/// recording readable in an editor without a sidecar file (ADR 0001).
fn set_track_metadata(
    stream: *mut ffi::AVStream,
    id: TrackId,
    name: Option<&str>,
    language: crate::track::Language,
    default: bool,
) -> Result<(), MuxError> {
    if let Some(name) = name {
        let Ok(name) = CString::new(name) else {
            return Err(MuxError::InvalidTrack {
                track: id,
                reason: "the track name contains a NUL, which cannot be written to the \
                         container",
            });
        };
        set_metadata(stream, c"title", &name, id)?;
    }

    let Ok(language) = CString::new(language.as_str()) else {
        unreachable!("a language tag is three ASCII letters");
    };
    set_metadata(stream, c"language", &language, id)?;

    if default {
        // SAFETY: `stream` is live and `disposition` is a plain integer field.
        unsafe {
            (*stream).disposition |= ffi::AV_DISPOSITION_DEFAULT as c_int;
        }
    }

    Ok(())
}

/// Sets one metadata entry on a stream.
fn set_metadata(
    stream: *mut ffi::AVStream,
    key: &core::ffi::CStr,
    value: &core::ffi::CStr,
    id: TrackId,
) -> Result<(), MuxError> {
    // SAFETY: `stream` is live and owns its metadata dictionary, which
    // `av_dict_set` allocates on first use and the context frees with the
    // stream. Both strings are NUL-terminated and outlive the call, which
    // copies them.
    let code =
        unsafe { ffi::av_dict_set(&mut (*stream).metadata, key.as_ptr(), value.as_ptr(), 0) };
    if code < 0 {
        return Err(MuxError::InvalidTrack {
            track: id,
            reason: "the track's name or language could not be recorded in the container",
        });
    }
    Ok(())
}

/// Writes the container header, with the options that decide how much a crash
/// can cost.
fn write_header(format: &FormatContext) -> Result<(), MuxError> {
    let mut options: *mut ffi::AVDictionary = ptr::null_mut();
    let cluster_limit = CString::new(CLUSTER_TIME_LIMIT_MS.to_string()).expect("a decimal number");

    // SAFETY: `options` starts null, which `av_dict_set` treats as "allocate
    // one", and every string passed is NUL-terminated and outlives the call.
    // The dictionary is freed on every path below.
    let set = unsafe {
        let cluster = ffi::av_dict_set(
            &mut options,
            c"cluster_time_limit".as_ptr(),
            cluster_limit.as_ptr(),
            0,
        );
        // `passthrough`, the default, would leave every track's FlagDefault at
        // whatever the disposition said and mark nothing when the caller marked
        // nothing — which is a file whose player has to guess which of five
        // audio tracks to play. `infer_no_subs` marks what the caller marked,
        // and falls back to the first track of each kind when it marked
        // nothing.
        let default_mode = ffi::av_dict_set(
            &mut options,
            c"default_mode".as_ptr(),
            c"infer_no_subs".as_ptr(),
            0,
        );
        cluster.min(default_mode)
    };

    if set < 0 {
        // Only an allocation failure can get here, but failing quietly would
        // mean the option is simply absent from the dictionary: the muxer would
        // then recognise everything it was handed, `av_dict_count` would be
        // zero, and the warning below could not fire. The recording would lose
        // five seconds to a kill instead of one with nothing to say so
        // (AGENTS.md section 15).
        //
        // SAFETY: `options` is either null or a dictionary owned here; freeing
        // nulls the pointer.
        unsafe { ffi::av_dict_free(&mut options) };
        return Err(MuxError::Ffmpeg {
            operation: "setting the container options that bound what a crash costs",
            source: AvError::new(set),
        });
    }

    // SAFETY: the context is live with its output open and its streams
    // described. `avformat_write_header` consumes the dictionary it is given
    // and writes back one holding whatever it did not recognise, which is freed
    // below in both the success and the failure case.
    let code = unsafe { ffi::avformat_write_header(format.as_ptr(), &mut options) };

    // SAFETY: `options` is either null or a dictionary owned here.
    let unrecognised = unsafe { ffi::av_dict_count(options) };
    // SAFETY: as above; freeing nulls the pointer.
    unsafe { ffi::av_dict_free(&mut options) };

    if code < 0 {
        return Err(MuxError::Ffmpeg {
            operation: "writing the container header",
            source: AvError::new(code),
        });
    }

    if unrecognised > 0 {
        // Not fatal, but it means this build's Matroska muxer does not have an
        // option the resilience of the output depends on, which is worth
        // knowing before somebody wonders why a killed recording lost five
        // seconds instead of one.
        warn!(
            unrecognised,
            "the linked FFmpeg's Matroska muxer did not recognise every option Clipped \
             sets; recordings may lose more on an abrupt termination than expected"
        );
    }

    Ok(())
}

/// Reads back the time base the muxer fixed a stream at.
fn stream_time_base(format: &FormatContext, stream_index: c_int) -> Option<TimeBase> {
    // SAFETY: the context is live and `stream_index` came from
    // `avformat_new_stream` on this same context, so it indexes a stream the
    // context still owns. `nb_streams` is checked anyway, because a wrong index
    // here would read arbitrary memory rather than fail.
    let time_base = unsafe {
        let context = format.as_ptr();
        let index = usize::try_from(stream_index).ok()?;
        if index >= (*context).nb_streams as usize {
            return None;
        }
        (**(*context).streams.add(index)).time_base
    };

    TimeBase::new(i64::from(time_base.num), i64::from(time_base.den))
}
