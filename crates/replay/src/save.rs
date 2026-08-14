//! Turning a lease into a playable file.
//!
//! This is what the rest of the crate exists for. [`ReplayBuffer::lease`] and
//! [`ReplayBuffer::lease_last`] pick the segments a clip needs and hold them
//! against eviction; [`save_clip`] writes them.
//!
//! [`ReplayBuffer::lease`]: crate::ReplayBuffer::lease
//! [`ReplayBuffer::lease_last`]: crate::ReplayBuffer::lease_last
//!
//! # There is no second muxer here
//!
//! A saved clip is written by `clipped_muxer::MkvWriter`, the same writer a
//! recording is written by, given the same encoded packets. The buffer holds
//! what came out of the encoder, and that is exactly what the writer takes, so
//! a save is a loop over a lease and not a container implementation (AGENTS.md
//! section 55). Everything the container does for a recording — rebasing
//! timestamps onto the first packet, forcing decode timestamps to increase,
//! bounding what an interrupted write costs — a clip therefore gets for free.
//!
//! # What the user is actually given
//!
//! A range somebody asks for lands in the middle of a segment at both ends, and
//! the two ends are not symmetrical:
//!
//! ```text
//!  segments      │▓▓▓▓▓▓▓▓│▓▓▓▓▓▓▓▓│▓▓▓▓▓▓▓▓│▓▓▓▓▓▓▓▓│
//!  requested          ├──────────────────┤
//!  written        ├──────────────────────┤
//!                 ↑                      ↑
//!         the keyframe at or        the last picture
//!         before the request        at or before the
//!                                   requested end
//! ```
//!
//! - **At the front the clip is longer than was asked for.** A coded picture
//!   references the pictures before it, so a stream can only be cut immediately
//!   before a keyframe: a clip that began at the requested instant would open
//!   with pictures nothing can decode. The clip therefore begins at the keyframe
//!   at or before the requested start, which is up to one segment early —
//!   [`DEFAULT_SEGMENT`](crate::DEFAULT_SEGMENT) is two seconds, so up to two
//!   seconds early. [`SavedClip::leading_slack`] reports exactly how much.
//! - **At the end it is trimmed to the request.** Nothing after the requested
//!   end depends on being written, so the packets after it are simply not, and
//!   the trim is made *in decode order*: everything up to and including the last
//!   packet whose presentation time falls at or before the requested end is
//!   written, and nothing after it. Cutting in decode order is what keeps this
//!   safe for an encoder that reorders — every picture a written packet
//!   references was decoded before it, so it was written too.
//!
//! So a saved clip is **never shorter than was asked for and never more than one
//! segment longer**, and the extra is at the front. That is the whole of the
//! keyframe-boundary behaviour, and `docs/replay-buffer.md` states it as the
//! tolerance a caller may rely on.
//!
//! A clip the buffer could not fill — a hotkey pressed ten seconds into a
//! session asking for the last thirty — is still written, and
//! [`SavedClip::is_complete`] and [`SavedClip::shortfall`] say what was missing.
//! Refusing would be worse: there is a clip to be had, and it is the clip
//! somebody asked for.
//!
//! # Where the work happens
//!
//! Not on the capture thread, and nothing here makes that true by itself — the
//! division of labour does:
//!
//! ```text
//!  capture + encode thread        the thread a save runs on
//!  ───────────────────────        ─────────────────────────
//!  ReplayBuffer::push  ─ lock ─▶
//!    (one memcpy)                 ReplayBuffer::lease  ─ lock ─▶
//!                                   (0.77 ms for a 5-minute window)
//!                                 save_clip(&lease, …)   no lock at all
//!                                   (a file write, for as long as it takes)
//! ```
//!
//! [`save_clip`] never touches the buffer. It reads a lease, whose segments are
//! immutable and held alive by their own reference count (`crate::lease`), so
//! the encoder can fill the buffer and the buffer can evict every segment the
//! clip is being written from while the write is in progress. The one bounded
//! moment under the buffer's lock is taking the lease, which is measured in
//! `docs/replay-buffer.md`.
//!
//! What that asks of a caller is one thing: **take the lease wherever is
//! convenient and call [`save_clip`] on a thread that is not capturing**
//! (AGENTS.md section 20). `crates/replay/tests/save_clip.rs` is what holds it
//! to that — a recording carries on being captured, buffered and written to its
//! own file while a clip is saved out of the same packets, and every frame of it
//! survives.
//!
//! # Audio, and the two clocks
//!
//! A clip carries every track the recording does: the compatibility mix and
//! each isolated source, declared from the
//! [`RecordingLayout`](clipped_muxer::RecordingLayout) the caller passes
//! ([issue #40](https://github.com/wildware-uk/clipped/issues/40)).
//!
//! Two things about that are worth stating, because both are easy to get
//! subtly wrong and neither shows up as anything louder than drift.
//!
//! **A clip begins on a keyframe, and audio has none.** The video written
//! starts at the keyframe at or before the requested start, which is earlier
//! than what was asked for. The audio written is the audio belonging to *that*
//! range and not to the requested one — selected against the video actually
//! written, so the two ends agree. Selecting against the request instead would
//! produce a clip whose audio led its video by up to a segment, which looks
//! like an alignment bug and is a selection bug.
//!
//! **Blocks are written whole.** A block straddling the first keyframe is
//! written with its own timestamp rather than trimmed, so a clip's audio may
//! begin a few milliseconds before its video. That is the same thing the
//! recording path does with the first block of a recording, and trimming would
//! mean this module resampling — a second implementation of something the muxer
//! owns (AGENTS.md section 55).
//!
//! The two are then **merged by timestamp**, not written one after the other.
//! [`MkvWriter`] forces a timestamp that goes backwards to be monotonic and
//! counts it, so appending every audio block after every video packet would not
//! produce a badly interleaved file — it would produce one whose audio had been
//! silently dragged forward to the end of the video.
//!
//! # Naming, and two saves at once
//!
//! Nothing here invents a file name. Two clips saved a second apart go to two
//! paths the caller chose, and a path that is already taken is refused
//! ([`MuxError::OutputExists`](clipped_muxer::MuxError::OutputExists)) rather
//! than overwritten (AGENTS.md section 56). Deciding what a clip should be
//! called belongs to the layer that knows what it is of, which is
//! [issue #38](https://github.com/wildware-uk/clipped/issues/38).
//!
//! Concurrent saves need nothing from this module: two leases are two
//! independent sets of `Arc`s, and two writers are two files.

use core::fmt;
use core::time::Duration;
use std::error::Error;
use std::path::{Path, PathBuf};

use clipped_logging::RedactedPath;
use clipped_muxer::{
    AudioTrackWriter, EncodedPacket, MkvWriter, MuxError, PacketTimestamp, RecordingLayout, TrackId,
};
use tracing::info;

use crate::lease::SegmentLease;
use crate::range::TimeRange;

/// Writes the video a lease holds to `destination` as a Matroska clip.
///
/// The clip begins on the keyframe at or before the requested start and ends at
/// the requested end; the module documentation sets out why the two ends differ
/// and what the caller is therefore given.
///
/// This blocks for as long as the write takes and must not be called on a
/// capture thread. It reads only the lease, which is what makes handing one to
/// another thread safe (`crate::lease`).
///
/// # Errors
///
/// [`SaveError::Create`] when the file could not be created — a path that is
/// already taken, a directory that does not exist, a track the container cannot
/// carry — in which case nothing is left behind. [`SaveError::Write`] when the
/// write failed part-way, which for a clip of any size means the disk filled or
/// the drive went away (AGENTS.md section 16); what had been written by then
/// remains, and is playable.
pub fn save_clip(
    lease: &SegmentLease,
    destination: &Path,
    layout: &RecordingLayout,
) -> Result<SavedClip, SaveError> {
    let requested = lease.requested();
    let to_write = packets_to_write(lease);
    let video = layout.video();
    // The caller's frame rate, or nothing. Matroska stores no duration for an
    // ordinary block, so this is the hint that decides the duration of the
    // *last* one, which is otherwise taken to be zero and makes a file read as
    // ending one frame early. With a frame rate declared on the track FFmpeg's
    // Matroska muxer infers the same value and the finished file is identical
    // either way — passing it is what keeps that true for a track that has none
    // to infer from. It is never invented when the caller did not supply one
    // (AGENTS.md section 19).
    let frame_interval = video
        .frame_rate()
        .map(clipped_muxer::FrameRate::frame_interval);

    let mut writer =
        MkvWriter::create(destination, layout).map_err(|source| SaveError::Create {
            destination: destination.to_path_buf(),
            source,
        })?;

    // One writer per declared track, in the order the layout declares them,
    // which is the order the container numbered them.
    let mut audio_writers = Vec::with_capacity(layout.audio_tracks().len());
    for (index, declared) in layout.audio_tracks().iter().enumerate() {
        let track = TrackId::Audio(u16::try_from(index).unwrap_or(u16::MAX));
        let writer =
            AudioTrackWriter::new(track, declared).map_err(|source| SaveError::Create {
                destination: destination.to_path_buf(),
                source,
            })?;
        audio_writers.push(writer);
    }

    // The audio that belongs to the video actually being written, in timestamp
    // order. Collected before anything is written because the selection depends
    // on where the video ends up starting, and because a lease hands audio back
    // in arrival order rather than in time order.
    let mut audio = audio_to_write(lease, to_write);
    let mut next_audio = 0;

    let mut packets = 0;
    let mut bytes = 0;
    let mut first = None;
    let mut last = Duration::ZERO;

    for packet in lease.packets().take(to_write) {
        // Everything captured at or before this picture goes first, so the file
        // is interleaved rather than sorted afterwards by a writer that would
        // rather move a timestamp than fail.
        while let Some(block) = audio.get(next_audio) {
            if block.at > packet.decode_time() {
                break;
            }
            write_audio_block(&mut writer, &mut audio_writers, block, destination, packets)?;
            next_audio += 1;
        }

        let mut written = EncodedPacket::new(
            TrackId::Video,
            timestamp(packet.presentation_time()),
            packet.data(),
        )
        .with_decode_timestamp(timestamp(packet.decode_time()))
        .with_keyframe(packet.is_keyframe());
        if let Some(interval) = frame_interval {
            written = written.with_duration(interval);
        }

        writer
            .write_packet(&written)
            .map_err(|source| SaveError::Write {
                destination: destination.to_path_buf(),
                packets_written: packets,
                source,
            })?;

        packets += 1;
        bytes += packet.data().len() as u64;
        first.get_or_insert(packet.presentation_time());
        // `max` rather than assignment: an encoder that reorders emits packets
        // in decode order, so the last one written need not carry the latest
        // presentation time.
        last = last.max(packet.presentation_time());
    }

    // Whatever was captured after the last picture but still inside the clip.
    for block in audio.drain(next_audio..) {
        write_audio_block(
            &mut writer,
            &mut audio_writers,
            &block,
            destination,
            packets,
        )?;
    }

    writer.finish().map_err(|source| SaveError::Write {
        destination: destination.to_path_buf(),
        packets_written: packets,
        source,
    })?;

    let covered = TimeRange::new(first.unwrap_or(Duration::ZERO), last);
    let clip = SavedClip {
        path: destination.to_path_buf(),
        requested,
        requested_length: lease.requested_length(),
        covered,
        packets,
        bytes,
        complete: lease.is_complete(),
        shortfall: lease.shortfall(),
    };

    info!(
        path = %RedactedPath::new(destination),
        packets = clip.packets,
        seconds = clip.duration().as_secs_f64(),
        leading_slack_seconds = clip.leading_slack().as_secs_f64(),
        complete = clip.complete,
        "replay clip saved"
    );

    Ok(clip)
}

/// One block of audio, chosen for the clip.
///
/// Owned rather than borrowed from the lease because the selection is sorted,
/// and sorting borrowed slices of several segments would otherwise pin the
/// lease's layout into this function's signature.
struct PlannedAudio {
    track: TrackId,
    at: Duration,
    samples: Vec<f32>,
}

/// The audio belonging to the video that is about to be written, in time order.
///
/// The window is the video's own: from the keyframe the clip opens on to the
/// last picture written. See the module documentation for why it is not the
/// requested range.
fn audio_to_write(lease: &SegmentLease, to_write: usize) -> Vec<PlannedAudio> {
    let mut first = None;
    let mut last = Duration::ZERO;
    for packet in lease.packets().take(to_write) {
        first.get_or_insert(packet.presentation_time());
        last = last.max(packet.presentation_time());
    }
    let Some(first) = first else {
        return Vec::new();
    };

    let mut planned: Vec<PlannedAudio> = lease
        .audio()
        .filter(|block| block.at() >= first && block.at() <= last)
        .map(|block| PlannedAudio {
            track: block.track(),
            at: block.at(),
            samples: block.samples().to_vec(),
        })
        .collect();
    // Stable, so two blocks captured in the same instant on different tracks
    // keep the order they arrived in rather than swapping between saves.
    planned.sort_by_key(|block| block.at);
    planned
}

/// Writes one planned block through the writer for its track.
///
/// A block for a track the layout did not declare is dropped rather than
/// written somewhere else: it means the caller passed a layout that disagrees
/// with what the buffer was fed, and putting a microphone into the game track
/// is worse than leaving it out.
fn write_audio_block(
    writer: &mut MkvWriter,
    audio_writers: &mut [AudioTrackWriter],
    block: &PlannedAudio,
    destination: &Path,
    packets_written: u64,
) -> Result<(), SaveError> {
    let TrackId::Audio(index) = block.track else {
        return Ok(());
    };
    let Some(track) = audio_writers.get_mut(usize::from(index)) else {
        return Ok(());
    };
    track
        .write_samples(writer, timestamp(block.at), &block.samples)
        .map_err(|source| SaveError::Write {
            destination: destination.to_path_buf(),
            packets_written,
            source,
        })
}

/// How many of a lease's packets belong in the clip, counted in decode order.
///
/// Everything up to and including the last packet presented at or before the
/// requested end, which is the trim described in the module documentation. At
/// least one packet always: a lease's first packet is the keyframe the clip
/// opens on, and an empty file is not a better answer than a very short one.
fn packets_to_write(lease: &SegmentLease) -> usize {
    let end = lease.requested().end();

    lease
        .packets()
        .enumerate()
        .filter(|(_, packet)| packet.presentation_time() <= end)
        .map(|(index, _)| index + 1)
        .last()
        .unwrap_or(1)
}

/// A media time as the container's clock reading.
///
/// Saturating rather than wrapping: 2^63 nanoseconds is 292 years of media
/// time, so this is unreachable, and a silent wrap would put one picture at the
/// far end of a clip's timeline.
fn timestamp(at: Duration) -> PacketTimestamp {
    PacketTimestamp::from_nanos(i64::try_from(at.as_nanos()).unwrap_or(i64::MAX))
}

/// A clip that was written, and how it compares with what was asked for.
///
/// Worth returning rather than logging: the caller is what tells somebody their
/// clip is ready, and "the last 30 seconds" that turned out to be 12 is a
/// different message from one that did not (AGENTS.md section 45).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedClip {
    path: PathBuf,
    requested: TimeRange,
    requested_length: Duration,
    covered: TimeRange,
    packets: u64,
    bytes: u64,
    complete: bool,
    shortfall: Duration,
}

impl SavedClip {
    /// The file that was written.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The range that was asked for.
    #[must_use]
    pub const fn requested(&self) -> TimeRange {
        self.requested
    }

    /// How much video was asked for.
    ///
    /// Not always [`requested`](Self::requested)`.length()`: "the last thirty
    /// seconds" of a recording four seconds old is a four-second range, and the
    /// clip still has to be able to say twenty-six seconds were wanted and not
    /// there.
    #[must_use]
    pub const fn requested_length(&self) -> Duration {
        self.requested_length
    }

    /// The media time the clip covers, measured between the presentation
    /// timestamps of its first and last pictures.
    #[must_use]
    pub const fn covered(&self) -> TimeRange {
        self.covered
    }

    /// How long the clip is.
    ///
    /// The span between its first and last pictures. The file's playing time is
    /// this plus the last picture's own duration, which the container knows only
    /// when the caller gave the track a frame rate.
    #[must_use]
    pub fn duration(&self) -> Duration {
        self.covered.length()
    }

    /// Video kept before the requested start, because the clip has to begin on
    /// a keyframe.
    ///
    /// Between zero and one segment. This is the part a caller may want to
    /// mention: the clip is slightly longer at the front than was asked for.
    #[must_use]
    pub fn leading_slack(&self) -> Duration {
        self.requested.start().saturating_sub(self.covered.start())
    }

    /// Video kept after the requested end.
    ///
    /// Zero for a stream in presentation order, because the clip is trimmed
    /// there. An encoder that reorders can leave up to its reordering depth,
    /// since the trim is made in decode order.
    #[must_use]
    pub fn trailing_slack(&self) -> Duration {
        self.covered.end().saturating_sub(self.requested.end())
    }

    /// Whether the buffer held the whole of what was asked for.
    ///
    /// `false` when the recording is younger than the requested duration, or
    /// when the request reached back past material already evicted. The clip is
    /// still a clip; [`shortfall`](Self::shortfall) says what is missing from
    /// it.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.complete
    }

    /// How much of the requested video the buffer did not hold.
    #[must_use]
    pub const fn shortfall(&self) -> Duration {
        self.shortfall
    }

    /// How many packets were written.
    #[must_use]
    pub const fn packets(&self) -> u64 {
        self.packets
    }

    /// How many bytes of coded video were written, before the container's own
    /// overhead.
    #[must_use]
    pub const fn byte_len(&self) -> u64 {
        self.bytes
    }
}

impl fmt::Display for SavedClip {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:.3}s in {} packets, {:.3}s of which is before the requested start",
            self.duration().as_secs_f64(),
            self.packets,
            self.leading_slack().as_secs_f64()
        )?;
        if !self.complete {
            write!(
                formatter,
                "; {:.3}s of what was asked for was not held",
                self.shortfall.as_secs_f64()
            )?;
        }
        Ok(())
    }
}

/// Writing a clip failed.
///
/// The two cases are worth telling apart because they leave the disk in
/// different states, and a caller reporting the failure has to say which
/// (AGENTS.md section 45).
#[derive(Debug)]
#[non_exhaustive]
pub enum SaveError {
    /// The clip could not be created, and nothing was written.
    Create {
        /// Where the clip was going.
        destination: PathBuf,
        /// What the container writer said.
        source: MuxError,
    },

    /// The clip was created and the write failed part-way through.
    ///
    /// What had been written remains and is playable: the writer finishes the
    /// file as it is dropped, so a save interrupted by a full disk leaves a
    /// shorter clip rather than an unreadable one (AGENTS.md sections 17 and
    /// 56). It is deliberately not deleted — the buffer will have moved on
    /// within seconds, and a short clip of the thing somebody asked for is worth
    /// more than a tidy directory.
    Write {
        /// Where the clip was going, and what is now there.
        destination: PathBuf,
        /// How many packets reached the file before the failure.
        packets_written: u64,
        /// What the container writer said.
        source: MuxError,
    },
}

impl fmt::Display for SaveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Create { destination, .. } => write!(
                formatter,
                "the replay clip {} could not be created",
                RedactedPath::new(destination)
            ),
            Self::Write {
                destination,
                packets_written,
                ..
            } => write!(
                formatter,
                "the replay clip {} failed after {packets_written} packets; what was written \
                 before the failure is still there",
                RedactedPath::new(destination)
            ),
        }
    }
}

impl Error for SaveError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Create { source, .. } | Self::Write { source, .. } => Some(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use clipped_encoder::{BitRate, EncodedPacket as EncoderPacket, PictureKind};

    use super::*;
    use crate::buffer::ReplayBuffer;
    use crate::config::ReplayConfig;

    /// A buffer with a 30 second window and one second segments, fed 10 packets
    /// a second — the same arithmetic `crate::buffer`'s tests use, so that the
    /// segment a range lands in can be worked out by hand.
    fn buffer() -> ReplayBuffer {
        let config = ReplayConfig::new(
            Duration::from_secs(30),
            BitRate::bits_per_second(800_000).expect("a real rate"),
        )
        .expect("a supported window")
        .with_segment_duration(Duration::from_secs(1))
        .expect("one second fits");

        ReplayBuffer::new(config)
    }

    /// Feeds `seconds` of 10 fps video, a keyframe every second.
    fn fill(buffer: &ReplayBuffer, seconds: u64) {
        for frame in 0..seconds * 10 {
            let at = Duration::from_millis(frame * 100);
            buffer.push(&EncoderPacket::new(
                &[0u8; 100],
                at,
                at,
                if frame % 10 == 0 {
                    PictureKind::Keyframe
                } else {
                    PictureKind::Predicted
                },
            ));
        }
    }

    #[test]
    fn the_packets_after_the_requested_end_are_not_written() {
        // The trim. The lease runs to the end of the segment containing 43.5 s,
        // which is 43.9 s; the clip stops at 43.5 s, so the four pictures after
        // it are dropped rather than written.
        let buffer = buffer();
        fill(&buffer, 50);

        let lease = buffer
            .lease(TimeRange::new(
                Duration::from_millis(41_500),
                Duration::from_millis(43_500),
            ))
            .expect("the range is held");

        assert_eq!(lease.packets().count(), 30, "three one-second segments");
        assert_eq!(
            packets_to_write(&lease),
            26,
            "41.0 s to 43.5 s inclusive, at 10 pictures a second"
        );
    }

    #[test]
    fn nothing_is_trimmed_when_the_request_ends_at_the_newest_picture() {
        // What a replay hotkey asks for: `lease_last` resolves the end against
        // the newest picture in the buffer, so there is nothing after it to
        // trim, and the clip is the whole lease.
        let buffer = buffer();
        fill(&buffer, 50);

        let lease = buffer
            .lease_last(Duration::from_secs(10))
            .expect("ten seconds are held");

        assert_eq!(packets_to_write(&lease), lease.packets().count());
    }

    #[test]
    fn a_reordered_stream_keeps_every_picture_at_or_before_the_requested_end() {
        // The reason the trim is made in decode order. An encoder emitting
        // B-frames produces them out of presentation order, so a packet after
        // the cut in decode order can carry an earlier presentation time —
        // trimming on the first packet past the end would leave a hole at the
        // end of the clip, and would drop pictures a written packet references.
        let buffer = buffer();
        let mut times = Vec::new();
        for group in 0..30u64 {
            // Decode order 0, 200, 100 within each group of three: the middle
            // picture is presented between the two either side of it.
            for offset in [0, 200, 100] {
                times.push(group * 300 + offset);
            }
        }
        for (index, at) in times.iter().enumerate() {
            let at = Duration::from_millis(*at);
            let index = u64::try_from(index).expect("a test pushes a few hundred packets");
            buffer.push(&EncoderPacket::new(
                &[0u8; 100],
                at,
                Duration::from_millis(index * 100),
                if index % 30 == 0 {
                    PictureKind::Keyframe
                } else {
                    PictureKind::Predicted
                },
            ));
        }

        let lease = buffer
            .lease(TimeRange::new(Duration::ZERO, Duration::from_millis(3_400)))
            .expect("the range is held");
        let kept = packets_to_write(&lease);

        let presented: Vec<Duration> = lease
            .packets()
            .take(kept)
            .map(|packet| packet.presentation_time())
            .collect();
        let dropped: Vec<Duration> = lease
            .packets()
            .skip(kept)
            .map(|packet| packet.presentation_time())
            .collect();

        assert!(
            !dropped.is_empty(),
            "the trim removed nothing, so this proves nothing"
        );
        assert!(
            dropped.iter().all(|at| *at > Duration::from_millis(3_400)),
            "a picture at or before the requested end was dropped: {dropped:?}"
        );
        assert!(
            presented.contains(&Duration::from_millis(3_400)),
            "the picture at the requested end was not kept"
        );
    }

    #[test]
    fn a_clip_opens_on_the_keyframe_at_or_before_the_requested_start() {
        // The front of the clip, which is the end that cannot be trimmed. A
        // request beginning at 41.5 s is written from the 41 s keyframe,
        // because a clip beginning half a second later would open with pictures
        // nothing can decode.
        let buffer = buffer();
        fill(&buffer, 50);

        let lease = buffer
            .lease(TimeRange::new(
                Duration::from_millis(41_500),
                Duration::from_millis(43_500),
            ))
            .expect("the range is held");

        let opening = lease
            .packets()
            .next()
            .expect("a lease over a range that is held holds packets");
        assert!(opening.is_keyframe());
        assert_eq!(opening.presentation_time(), Duration::from_secs(41));
        assert_eq!(lease.leading_slack(), Duration::from_millis(500));
    }
}
