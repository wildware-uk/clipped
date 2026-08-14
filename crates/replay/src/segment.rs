//! A segment: the unit the buffer keeps, evicts and hands to a save.
//!
//! # Why segments rather than packets
//!
//! A replay buffer that evicted individual packets would be simpler and wrong.
//! Coded pictures reference the pictures before them, so a stream can only be
//! cut immediately before a keyframe; dropping the oldest packet of a group of
//! pictures leaves every later packet in that group undecodable while still
//! occupying memory. Grouping packets into segments that *begin* on a keyframe
//! makes the buffer's unit the same as the stream's unit of independence: every
//! segment held is a segment that can be decoded on its own, and evicting one
//! costs nothing that is still usable.
//!
//! # Storage
//!
//! One [`Vec<u8>`] per segment holds every packet's bytes end to end, and
//! [`BufferedPacket`] indexes into it. The alternative — a `Vec<u8>` per packet
//! — is one allocation per frame, which at 60 frames a second for thirty
//! minutes is 108,000 of them for one buffer's window (AGENTS.md section 18).
//! The bytes are copied in, once, because
//! [`clipped_encoder::EncodedPacket`] borrows the encoder's own output buffer
//! and is released by the next call to `next_packet`.
//!
//! # Ownership
//!
//! A sealed segment is immutable and is held behind an [`Arc`](std::sync::Arc).
//! That is what makes eviction safe while a save is reading: the buffer drops
//! its reference, the save keeps its own, and the bytes go when the last
//! reference does. `crate::buffer` describes the whole lifetime.

use core::fmt;
use core::time::Duration;

use std::io::{self, Read, Write};

use clipped_encoder::EncodedPacket;
use clipped_muxer::TrackId;

use crate::range::TimeRange;

/// What a spilled segment file begins with, so that a file which is not one is
/// refused rather than read as nonsense.
const MAGIC: u32 = 0x434c_5253;

/// The layout version, bumped when the fields below change.
///
/// A file written by a different version is refused rather than reinterpreted:
/// spill files do not outlive the process that wrote them
/// ([issue #36](https://github.com/wildware-uk/clipped/issues/36)), so there is
/// nothing to migrate and everything to lose by guessing.
const VERSION: u16 = 1;

/// Which segment this is, counted from the first one the buffer opened.
///
/// Monotonic for the life of a buffer and never reused, so a save that logs the
/// segments it read names something that cannot come back as a different
/// segment later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SegmentId(u64);

impl SegmentId {
    /// An identifier a test can name, for the modules that store and reload
    /// segments without a buffer to get one from.
    #[cfg(test)]
    pub(crate) const fn for_test(value: u64) -> Self {
        Self(value)
    }

    /// The number behind the identifier, for logs and reports.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for SegmentId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "segment-{:04}", self.0)
    }
}

/// Where one packet sits in its segment, and what it is.
#[derive(Debug, Clone, Copy)]
struct BufferedPacket {
    offset: usize,
    length: usize,
    presentation: Duration,
    decode: Duration,
    keyframe: bool,
}

/// One packet, read back out of a segment.
///
/// The same shape as [`clipped_encoder::EncodedPacket`], and deliberately not
/// that type: an encoder packet borrows the encoder's output buffer for one
/// call, and this borrows a segment that will outlive the encoder session
/// entirely.
#[derive(Debug, Clone, Copy)]
pub struct SegmentPacket<'segment> {
    data: &'segment [u8],
    presentation: Duration,
    decode: Duration,
    keyframe: bool,
}

impl<'segment> SegmentPacket<'segment> {
    /// The coded bytes.
    #[must_use]
    pub const fn data(&self) -> &'segment [u8] {
        self.data
    }

    /// When this picture should be shown, in media time.
    #[must_use]
    pub const fn presentation_time(&self) -> Duration {
        self.presentation
    }

    /// When this picture must be decoded, in media time.
    #[must_use]
    pub const fn decode_time(&self) -> Duration {
        self.decode
    }

    /// Whether a stream can be cut immediately before this packet.
    #[must_use]
    pub const fn is_keyframe(&self) -> bool {
        self.keyframe
    }
}

/// Where one block of captured audio sits in its segment.
#[derive(Debug, Clone, Copy)]
struct BufferedAudio {
    offset: usize,
    length: usize,
    track: TrackId,
    at: Duration,
}

/// One block of captured audio, read back out of a segment.
///
/// Interleaved `f32` samples, which is what the capture produced and what
/// [`clipped_muxer::AudioTrackWriter`] takes. They are held in that form rather
/// than converted to PCM on the way in so that a clip and the recording it was
/// taken from go through **one** conversion, in one place: a buffer that
/// encoded its own samples would be a second implementation of the thing the
/// muxer already does (AGENTS.md section 55).
#[derive(Debug, Clone, Copy)]
pub struct SegmentAudio<'segment> {
    samples: &'segment [f32],
    track: TrackId,
    at: Duration,
}

impl<'segment> SegmentAudio<'segment> {
    /// The interleaved samples.
    #[must_use]
    pub const fn samples(&self) -> &'segment [f32] {
        self.samples
    }

    /// Which audio track they belong to.
    #[must_use]
    pub const fn track(&self) -> TrackId {
        self.track
    }

    /// When the first frame in the block was captured, in media time.
    #[must_use]
    pub const fn at(&self) -> Duration {
        self.at
    }
}

/// A sealed run of packets beginning on a keyframe.
///
/// Immutable once built. Everything that reads one — a save, a report, a test —
/// sees the same bytes for as long as it holds a reference, whatever the buffer
/// does next.
#[derive(Debug)]
pub struct Segment {
    id: SegmentId,
    bytes: Vec<u8>,
    packets: Vec<BufferedPacket>,
    samples: Vec<f32>,
    audio: Vec<BufferedAudio>,
    start: Duration,
    last_presentation: Duration,
}

impl Segment {
    /// Which segment this is.
    #[must_use]
    pub const fn id(&self) -> SegmentId {
        self.id
    }

    /// The presentation time of the keyframe this segment begins with.
    #[must_use]
    pub const fn start(&self) -> Duration {
        self.start
    }

    /// The presentation time of the last picture in this segment.
    #[must_use]
    pub const fn last_presentation(&self) -> Duration {
        self.last_presentation
    }

    /// The media time this segment covers.
    #[must_use]
    pub fn span(&self) -> TimeRange {
        TimeRange::new(self.start, self.last_presentation)
    }

    /// How many packets are in it.
    #[must_use]
    pub fn len(&self) -> usize {
        self.packets.len()
    }

    /// Whether it holds no packets.
    ///
    /// Never true of a segment reachable through the buffer: a segment is
    /// opened by its keyframe, so it has a packet before it exists.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.packets.is_empty()
    }

    /// How many bytes of coded video it holds.
    ///
    /// The payload, not the allocation. [`resident_bytes`](Self::resident_bytes)
    /// is what it costs.
    #[must_use]
    pub fn byte_len(&self) -> usize {
        self.bytes.len()
    }

    /// How much memory it occupies: the payload, its allocation's spare
    /// capacity, and the packet index.
    ///
    /// This is the number the buffer's ceiling is enforced against, because a
    /// ceiling checked against payload alone is a ceiling that is quietly
    /// exceeded (AGENTS.md section 19).
    #[must_use]
    pub fn resident_bytes(&self) -> usize {
        self.bytes.capacity()
            + self.packets.capacity() * size_of::<BufferedPacket>()
            + self.samples.capacity() * size_of::<f32>()
            + self.audio.capacity() * size_of::<BufferedAudio>()
            + size_of::<Self>()
    }

    /// Writes this segment where it can be read back by [`read_from`](Self::read_from).
    ///
    /// The format is this crate's own and deliberately not a general one: it is
    /// read only by the process that wrote it, within one run
    /// ([issue #36](https://github.com/wildware-uk/clipped/issues/36)), so it
    /// carries no compatibility promise and nothing that would need one. Fixed
    /// little-endian fields and then the two payloads, so reading is one
    /// allocation each rather than one per packet.
    ///
    /// # Errors
    ///
    /// Whatever the writer reports, which for a spill file means the disk
    /// filled or the drive went away.
    pub(crate) fn write_to(&self, into: &mut impl Write) -> io::Result<()> {
        into.write_all(&MAGIC.to_le_bytes())?;
        into.write_all(&VERSION.to_le_bytes())?;
        put_u64(into, nanos(self.start))?;
        put_u64(into, nanos(self.last_presentation))?;
        put_u64(into, self.packets.len() as u64)?;
        put_u64(into, self.audio.len() as u64)?;
        put_u64(into, self.bytes.len() as u64)?;
        put_u64(into, self.samples.len() as u64)?;

        for packet in &self.packets {
            put_u64(into, packet.offset as u64)?;
            put_u64(into, packet.length as u64)?;
            put_u64(into, nanos(packet.presentation))?;
            put_u64(into, nanos(packet.decode))?;
            into.write_all(&[u8::from(packet.keyframe)])?;
        }
        for block in &self.audio {
            put_u64(into, block.offset as u64)?;
            put_u64(into, block.length as u64)?;
            put_u64(into, track_number(block.track))?;
            put_u64(into, nanos(block.at))?;
        }

        into.write_all(&self.bytes)?;
        for sample in &self.samples {
            into.write_all(&sample.to_le_bytes())?;
        }
        Ok(())
    }

    /// Reads back what [`write_to`](Self::write_to) wrote.
    ///
    /// The identifier is supplied rather than stored, because it belongs to the
    /// buffer that is putting the segment back rather than to the file.
    ///
    /// # Errors
    ///
    /// [`io::ErrorKind::InvalidData`] for a file that is not one of these or is
    /// truncated, and whatever the reader reports otherwise.
    pub(crate) fn read_from(id: SegmentId, from: &mut impl Read) -> io::Result<Self> {
        let mut magic = [0_u8; 4];
        from.read_exact(&mut magic)?;
        let mut version = [0_u8; 2];
        from.read_exact(&mut version)?;
        if u32::from_le_bytes(magic) != MAGIC || u16::from_le_bytes(version) != VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "this is not a replay segment written by this build",
            ));
        }

        let start = Duration::from_nanos(get_u64(from)?);
        let last_presentation = Duration::from_nanos(get_u64(from)?);
        let packet_count = usize::try_from(get_u64(from)?).unwrap_or(usize::MAX);
        let audio_count = usize::try_from(get_u64(from)?).unwrap_or(usize::MAX);
        let byte_count = usize::try_from(get_u64(from)?).unwrap_or(usize::MAX);
        let sample_count = usize::try_from(get_u64(from)?).unwrap_or(usize::MAX);

        let mut packets = Vec::with_capacity(packet_count.min(1 << 20));
        for _ in 0..packet_count {
            let offset = usize::try_from(get_u64(from)?).unwrap_or(usize::MAX);
            let length = usize::try_from(get_u64(from)?).unwrap_or(usize::MAX);
            let presentation = Duration::from_nanos(get_u64(from)?);
            let decode = Duration::from_nanos(get_u64(from)?);
            let mut keyframe = [0_u8; 1];
            from.read_exact(&mut keyframe)?;
            packets.push(BufferedPacket {
                offset,
                length,
                presentation,
                decode,
                keyframe: keyframe[0] != 0,
            });
        }

        let mut audio = Vec::with_capacity(audio_count.min(1 << 20));
        for _ in 0..audio_count {
            let offset = usize::try_from(get_u64(from)?).unwrap_or(usize::MAX);
            let length = usize::try_from(get_u64(from)?).unwrap_or(usize::MAX);
            let track = track_of(get_u64(from)?);
            let at = Duration::from_nanos(get_u64(from)?);
            audio.push(BufferedAudio {
                offset,
                length,
                track,
                at,
            });
        }

        let mut bytes = vec![0_u8; byte_count];
        from.read_exact(&mut bytes)?;
        let mut samples = vec![0_f32; sample_count];
        for sample in &mut samples {
            let mut raw = [0_u8; 4];
            from.read_exact(&mut raw)?;
            *sample = f32::from_le_bytes(raw);
        }

        // A file whose index points outside its own payload would panic on the
        // first read, a long way from here. Refusing it now names the fault.
        let sound = packets
            .iter()
            .all(|packet| packet.offset.saturating_add(packet.length) <= bytes.len())
            && audio
                .iter()
                .all(|block| block.offset.saturating_add(block.length) <= samples.len());
        if !sound {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "a spilled segment's index points outside the bytes it was stored with",
            ));
        }

        Ok(Self {
            id,
            bytes,
            packets,
            samples,
            audio,
            start,
            last_presentation,
        })
    }

    /// Every block of captured audio in it, in the order it arrived.
    ///
    /// Audio is appended to whichever segment is open when it arrives, and it
    /// arrives on its own thread, so a block's timestamp is **not** guaranteed
    /// to lie inside its segment's span. Nothing here tries to correct that:
    /// selection is by timestamp (`crate::save`), which is the only thing that
    /// is true of audio captured beside a video stream it was never locked to.
    pub fn audio(&self) -> impl ExactSizeIterator<Item = SegmentAudio<'_>> + '_ {
        self.audio.iter().map(|block| SegmentAudio {
            samples: &self.samples[block.offset..block.offset + block.length],
            track: block.track,
            at: block.at,
        })
    }

    /// Every packet, in the order the encoder produced them, which is decode
    /// order.
    pub fn packets(&self) -> impl ExactSizeIterator<Item = SegmentPacket<'_>> + '_ {
        self.packets.iter().map(|packet| SegmentPacket {
            data: &self.bytes[packet.offset..packet.offset + packet.length],
            presentation: packet.presentation,
            decode: packet.decode,
            keyframe: packet.keyframe,
        })
    }
}

/// Writes a `u64` little-endian.
fn put_u64(into: &mut impl Write, value: u64) -> io::Result<()> {
    into.write_all(&value.to_le_bytes())
}

/// Reads a `u64` little-endian.
fn get_u64(from: &mut impl Read) -> io::Result<u64> {
    let mut bytes = [0_u8; 8];
    from.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

/// A duration as nanoseconds, saturating rather than wrapping.
fn nanos(value: Duration) -> u64 {
    u64::try_from(value.as_nanos()).unwrap_or(u64::MAX)
}

/// A track as one number: zero is the video track, and an audio track is its
/// index plus one.
fn track_number(track: TrackId) -> u64 {
    match track {
        TrackId::Video => 0,
        TrackId::Audio(index) => u64::from(index) + 1,
    }
}

/// The inverse of [`track_number`].
fn track_of(number: u64) -> TrackId {
    match u16::try_from(number.saturating_sub(1)) {
        Ok(index) if number > 0 => TrackId::Audio(index),
        _ => TrackId::Video,
    }
}

/// How many packets the index grows by at a time.
///
/// Two seconds at 60 fps is 120 packets, so a segment of the default length
/// costs one growth beyond the initial reservation. See [`OpenSegment`] for why
/// the step is fixed rather than doubling.
const PACKET_INDEX_STEP: usize = 64;

/// The segment currently being written.
///
/// Separate from [`Segment`] rather than a mutable one because the distinction
/// is load-bearing: only a sealed segment may be shared, and the type system is
/// what says so. An open segment is owned by the buffer alone, so appending to
/// it cannot race with a save reading it.
///
/// # Growth
///
/// Both vectors grow in **fixed steps** rather than by `Vec`'s doubling, and
/// the step is a whole segment's worth of bytes. That is not a micro-optimism:
/// `crate::buffer` enforces the memory ceiling against
/// [`resident_bytes`](Self::resident_bytes), which counts the allocation and not
/// just the payload, and a capacity that doubled could carry a segment from just
/// under the ceiling to well over it inside a single push. A fixed step bounds
/// what one append can cost, which is what lets the buffer ask
/// [`resident_bytes_after`](Self::resident_bytes_after) what the next packet
/// will cost and refuse it before the memory is committed.
#[derive(Debug)]
pub(crate) struct OpenSegment {
    id: SegmentId,
    bytes: Vec<u8>,
    packets: Vec<BufferedPacket>,
    samples: Vec<f32>,
    audio: Vec<BufferedAudio>,
    start: Duration,
    last_presentation: Duration,
    /// How many bytes to add when the byte buffer is full.
    growth: usize,
}

impl OpenSegment {
    /// Opens a segment beginning with `keyframe`.
    ///
    /// `reserve` is what one segment is expected to hold at the configured
    /// bitrate, and is both the initial allocation and the step it grows by.
    ///
    /// What that buys is worth stating precisely, because the measurements in
    /// `docs/replay-buffer.md` do not support the stronger claim: NVENC
    /// achieved 24.03 Mbit/s against a configured 18.66, so **every** segment
    /// in those runs outgrew its reservation. It is one allocation for a
    /// segment the encoder produces at the bitrate it was configured for, and
    /// one more for each further segment's worth of overshoot — two, in those
    /// runs — rather than the run of doubling reallocations a `Vec` filled a
    /// packet at a time would perform on the thread that is also capturing
    /// (AGENTS.md section 18).
    pub(crate) fn open(id: SegmentId, keyframe: &EncodedPacket<'_>, reserve: usize) -> Self {
        let mut segment = Self {
            id,
            bytes: Vec::with_capacity(reserve.max(keyframe.data().len())),
            packets: Vec::with_capacity(PACKET_INDEX_STEP),
            // Not reserved ahead: a recording with no audio must not pay for a
            // pool it never fills, and a segment's worth of samples is far
            // larger than its coded video.
            samples: Vec::new(),
            audio: Vec::new(),
            start: keyframe.presentation_time(),
            last_presentation: keyframe.presentation_time(),
            growth: reserve.max(1),
        };
        segment.append(keyframe);
        segment
    }

    /// The capacities [`append`](Self::append) would grow to for a packet of
    /// `bytes`.
    ///
    /// One function, used by `append` and by
    /// [`resident_bytes_after`](Self::resident_bytes_after), so that the growth
    /// policy and the buffer's ceiling arithmetic cannot drift apart.
    fn capacities_after(&self, bytes: usize) -> (usize, usize) {
        let byte_capacity = if self.bytes.len() + bytes > self.bytes.capacity() {
            self.bytes.len() + bytes + self.growth
        } else {
            self.bytes.capacity()
        };
        let index_capacity = if self.packets.len() == self.packets.capacity() {
            self.packets.capacity() + PACKET_INDEX_STEP
        } else {
            self.packets.capacity()
        };

        (byte_capacity, index_capacity)
    }

    /// What this segment would occupy after appending a packet of `bytes`.
    ///
    /// The buffer asks before it copies, because a ceiling checked after the
    /// memory has been committed is not a ceiling (`crate::buffer`).
    pub(crate) fn resident_bytes_after(&self, bytes: usize) -> usize {
        let (byte_capacity, index_capacity) = self.capacities_after(bytes);
        byte_capacity + index_capacity * size_of::<BufferedPacket>() + size_of::<Segment>()
    }

    /// Adds one packet to the end.
    pub(crate) fn append(&mut self, packet: &EncodedPacket<'_>) {
        let (byte_capacity, index_capacity) = self.capacities_after(packet.data().len());
        // `reserve_exact` rather than `reserve`: the latter is the doubling
        // this type exists to avoid.
        if byte_capacity > self.bytes.capacity() {
            self.bytes.reserve_exact(byte_capacity - self.bytes.len());
        }
        if index_capacity > self.packets.capacity() {
            self.packets
                .reserve_exact(index_capacity - self.packets.len());
        }

        let offset = self.bytes.len();
        self.bytes.extend_from_slice(packet.data());
        self.packets.push(BufferedPacket {
            offset,
            length: packet.data().len(),
            presentation: packet.presentation_time(),
            decode: packet.decode_time(),
            keyframe: packet.is_keyframe(),
        });
        // `max` rather than assignment: an encoder that reorders pictures emits
        // them in decode order, so a later packet can carry an earlier
        // presentation time, and the segment's end is the latest picture in it.
        self.last_presentation = self.last_presentation.max(packet.presentation_time());
    }

    /// What this segment would occupy after appending `samples` of audio.
    ///
    /// The audio counterpart of
    /// [`resident_bytes_after`](Self::resident_bytes_after), and it exists for
    /// the same reason: the buffer's ceiling has to be checked before the
    /// memory is committed, and audio is the larger of the two per second of
    /// recording once several tracks are captured.
    pub(crate) fn resident_bytes_after_audio(&self, samples: usize) -> usize {
        let (sample_capacity, index_capacity) = self.audio_capacities_after(samples);
        self.bytes.capacity()
            + self.packets.capacity() * size_of::<BufferedPacket>()
            + sample_capacity * size_of::<f32>()
            + index_capacity * size_of::<BufferedAudio>()
            + size_of::<Segment>()
    }

    /// The capacities [`append_audio`](Self::append_audio) would grow to.
    ///
    /// One function for the same reason `capacities_after` is one function: the
    /// growth policy and the ceiling arithmetic must not drift apart.
    fn audio_capacities_after(&self, samples: usize) -> (usize, usize) {
        let sample_capacity = if self.samples.len() + samples > self.samples.capacity() {
            self.samples.len() + samples + self.growth
        } else {
            self.samples.capacity()
        };
        let index_capacity = if self.audio.len() == self.audio.capacity() {
            self.audio.capacity() + PACKET_INDEX_STEP
        } else {
            self.audio.capacity()
        };

        (sample_capacity, index_capacity)
    }

    /// Adds one block of captured audio.
    ///
    /// `at` is when the first frame of the block was captured, on the same
    /// media clock the video packets carry, which is what lets a save select
    /// audio for a range that begins on a keyframe.
    pub(crate) fn append_audio(&mut self, track: TrackId, at: Duration, samples: &[f32]) {
        let (sample_capacity, index_capacity) = self.audio_capacities_after(samples.len());
        if sample_capacity > self.samples.capacity() {
            self.samples
                .reserve_exact(sample_capacity - self.samples.len());
        }
        if index_capacity > self.audio.capacity() {
            self.audio.reserve_exact(index_capacity - self.audio.len());
        }

        let offset = self.samples.len();
        self.samples.extend_from_slice(samples);
        self.audio.push(BufferedAudio {
            offset,
            length: samples.len(),
            track,
            at,
        });
    }

    /// The presentation time of the keyframe it began with.
    pub(crate) const fn start(&self) -> Duration {
        self.start
    }

    /// The presentation time of the newest picture in it.
    pub(crate) const fn last_presentation(&self) -> Duration {
        self.last_presentation
    }

    /// How many packets are in it.
    pub(crate) fn len(&self) -> usize {
        self.packets.len()
    }

    /// How much memory it occupies now, including the spare capacity reserved
    /// for the packets still to come.
    pub(crate) fn resident_bytes(&self) -> usize {
        self.bytes.capacity()
            + self.packets.capacity() * size_of::<BufferedPacket>()
            + self.samples.capacity() * size_of::<f32>()
            + self.audio.capacity() * size_of::<BufferedAudio>()
            + size_of::<Segment>()
    }

    /// Closes it, releasing the capacity that was reserved and not used.
    ///
    /// The shrink is why the buffer's accounting can be exact. It copies at
    /// most one segment's bytes — a few megabytes, once every segment length —
    /// which at the two-second default is under three megabytes a second of
    /// memory traffic beside an encode that is producing them.
    pub(crate) fn seal(mut self) -> Segment {
        self.bytes.shrink_to_fit();
        self.packets.shrink_to_fit();
        self.samples.shrink_to_fit();
        self.audio.shrink_to_fit();
        Segment {
            id: self.id,
            bytes: self.bytes,
            packets: self.packets,
            samples: self.samples,
            audio: self.audio,
            start: self.start,
            last_presentation: self.last_presentation,
        }
    }

    /// A sealed copy of what is in it now, leaving it open.
    ///
    /// What a save reads for the newest material. The freshest packets are in
    /// the open segment — that is where the moment somebody just pressed a
    /// hotkey for lives — and they cannot be shared while the buffer is still
    /// appending to them, so a lease takes a copy. It costs one memcpy of at
    /// most one segment, once per save, and it is what stops a save from losing
    /// the last two seconds of the thing it was asked to save.
    pub(crate) fn snapshot(&self) -> Segment {
        Segment {
            id: self.id,
            bytes: self.bytes.clone(),
            packets: self.packets.clone(),
            samples: self.samples.clone(),
            audio: self.audio.clone(),
            start: self.start,
            last_presentation: self.last_presentation,
        }
    }
}

/// Builds the identifiers, so that nothing else can invent one.
#[derive(Debug, Default)]
pub(crate) struct SegmentIds(u64);

impl SegmentIds {
    /// The next identifier, which no earlier segment has had.
    pub(crate) fn next(&mut self) -> SegmentId {
        let id = SegmentId(self.0);
        self.0 += 1;
        id
    }
}

#[cfg(test)]
mod tests {
    use clipped_encoder::PictureKind;

    use super::*;

    fn packet<'a>(data: &'a [u8], at_ms: u64, keyframe: bool) -> EncodedPacket<'a> {
        EncodedPacket::new(
            data,
            Duration::from_millis(at_ms),
            Duration::from_millis(at_ms),
            if keyframe {
                PictureKind::Keyframe
            } else {
                PictureKind::Predicted
            },
        )
    }

    #[test]
    fn a_segment_reads_back_exactly_what_was_pushed_into_it() {
        // The whole point of the packed representation: one allocation, and
        // every packet still comes back with its own bytes and its own times.
        let mut open = OpenSegment::open(SegmentId(7), &packet(&[1, 2, 3], 0, true), 1024);
        open.append(&packet(&[4, 5], 16, false));
        open.append(&packet(&[6, 7, 8, 9], 33, false));
        let segment = open.seal();

        let read: Vec<(Vec<u8>, u64, bool)> = segment
            .packets()
            .map(|packet| {
                (
                    packet.data().to_vec(),
                    packet.presentation_time().as_millis() as u64,
                    packet.is_keyframe(),
                )
            })
            .collect();

        assert_eq!(
            read,
            vec![
                (vec![1, 2, 3], 0, true),
                (vec![4, 5], 16, false),
                (vec![6, 7, 8, 9], 33, false),
            ]
        );
        assert_eq!(segment.id(), SegmentId(7));
        assert_eq!(segment.byte_len(), 9);
        assert_eq!(segment.len(), 3);
    }

    #[test]
    fn a_segment_spans_from_its_keyframe_to_its_newest_picture() {
        let mut open = OpenSegment::open(SegmentId(0), &packet(&[0], 1000, true), 64);
        open.append(&packet(&[0], 2000, false));
        let segment = open.seal();

        assert_eq!(segment.start(), Duration::from_secs(1));
        assert_eq!(segment.last_presentation(), Duration::from_secs(2));
        assert_eq!(segment.span().length(), Duration::from_secs(1));
    }

    #[test]
    fn a_reordered_picture_does_not_pull_the_end_of_a_segment_backwards() {
        // An encoder emitting B-frames produces them in decode order, so a
        // packet can carry an earlier presentation time than the one before it.
        // Taking the last packet's time as the segment's end would make the
        // segment look shorter than it is, and a buffer would then hold less
        // history than it was asked for.
        let mut open = OpenSegment::open(SegmentId(0), &packet(&[0], 0, true), 64);
        open.append(&packet(&[0], 100, false));
        open.append(&packet(&[0], 50, false));
        let segment = open.seal();

        assert_eq!(segment.last_presentation(), Duration::from_millis(100));
    }

    #[test]
    fn sealing_releases_the_capacity_that_was_reserved_and_not_used() {
        // Reserving a whole segment up front is what keeps allocation off the
        // capture thread; giving it back at the seal is what keeps the buffer's
        // accounting of what it holds honest.
        let open = OpenSegment::open(SegmentId(0), &packet(&[0; 16], 0, true), 4 * 1024 * 1024);
        let reserved = open.resident_bytes();
        let segment = open.seal();

        assert!(reserved > 4 * 1024 * 1024, "{reserved}");
        assert!(
            segment.resident_bytes() < 4096,
            "a sealed segment of 16 bytes still occupies {}",
            segment.resident_bytes()
        );
    }

    #[test]
    fn a_segment_costs_exactly_what_it_said_the_next_packet_would_cost() {
        // The buffer refuses a packet before copying it in, using this
        // prediction (`crate::buffer`). A prediction that came in under what
        // the append really cost would be a ceiling that is quietly exceeded,
        // so it is checked against the outcome at every step — including the
        // ones that reallocate.
        let mut open = OpenSegment::open(SegmentId(0), &packet(&[0; 8], 0, true), 64);

        for index in 1..200u64 {
            let data = vec![0u8; 40];
            let predicted = open.resident_bytes_after(data.len());
            open.append(&packet(&data, index * 16, false));

            assert_eq!(
                open.resident_bytes(),
                predicted,
                "appending packet {index} cost more than the buffer was told it would"
            );
        }
    }

    #[test]
    fn an_open_segment_grows_in_fixed_steps_rather_than_doubling() {
        // A segment that outgrows its reservation must not double its way past
        // the memory ceiling between two of the buffer's checks. A thousand
        // times the reservation is fed in: grown in fixed steps the allocation
        // ends up just over the payload, and a doubling `Vec` would be holding
        // very nearly twice it.
        let reserve = 1024;
        let mut open = OpenSegment::open(SegmentId(0), &packet(&[0; 1024], 0, true), reserve);

        for index in 1..=1000u64 {
            open.append(&packet(&[0; 1024], index, false));
        }

        let payload = 1024 * 1001;
        assert!(
            open.resident_bytes() < payload + payload / 4,
            "a segment holding {payload} bytes of video occupies {}, which is the doubling this \
             type exists to avoid",
            open.resident_bytes()
        );
    }

    #[test]
    fn a_snapshot_of_an_open_segment_is_independent_of_what_is_appended_next() {
        // What a save reads for the newest material. If the copy shared the
        // buffer's allocation, appending would move it and the save would be
        // reading a segment that changed underneath it.
        let mut open = OpenSegment::open(SegmentId(3), &packet(&[9, 9], 0, true), 128);
        let snapshot = open.snapshot();
        open.append(&packet(&[1], 16, false));

        assert_eq!(snapshot.len(), 1);
        assert_eq!(
            snapshot.packets().next().expect("one packet").data(),
            &[9, 9]
        );
        assert_eq!(open.last_presentation(), Duration::from_millis(16));
    }

    #[test]
    fn identifiers_are_never_reused() {
        let mut ids = SegmentIds::default();
        let first = ids.next();
        let second = ids.next();

        assert_ne!(first, second);
        assert_eq!(first.to_string(), "segment-0000");
        assert_eq!(second.to_string(), "segment-0001");
    }
}
