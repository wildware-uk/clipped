//! The `.cwf` sidecar: how a computed waveform is written down.
//!
//! AGENTS.md section 32 allows application metadata in SQLite, in a documented
//! sidecar format, or in the container. Peaks are none of those three things by
//! nature — they are derived data that can always be recomputed from the
//! recording — so they are written to a documented sidecar in a **cache**
//! directory rather than into the database or into the recording. The format is
//! specified in `docs/waveforms.md`; this module is the implementation of that
//! specification and the two must be changed together.
//!
//! # Why a file and not the database
//!
//! Three reasons, in order of weight.
//!
//! - Section 31 says not to store large media blobs in SQLite. A three-track
//!   hour is about 4 MB of peaks, which is exactly the kind of blob that means.
//! - Losing it costs a recomputation. The database holds things that cannot be
//!   recovered — bookmarks, favourites, per-game settings — and mixing
//!   throwaway data into it means every backup, migration and integrity check
//!   carries the throwaway data too.
//! - It does not need the database to exist. Issue #55 is building the schema
//!   as this is written, and a feature that had to wait for a table could not
//!   be finished at all.
//!
//! **How it would move.** If the peaks ever belong in SQLite, the *index* moves
//! and the payload does not: a `waveform` table keyed on recording id, holding
//! the source identity and the entry's file name, replaces the "find the file
//! named after the digest of the path" step in [`crate::WaveformCache`]. The
//! bytes this module writes stay a file, because of section 31. Nothing else in
//! the crate would change, which is why the identity is written into the entry
//! rather than being implied by where the entry is.
//!
//! # Endianness and forwards compatibility
//!
//! Everything is little-endian, because every machine this runs on is. A
//! version byte pair leads the file and a reader refuses a version it does not
//! know, which is what makes the base bucket resolution changeable later: an
//! entry written by an older build is not misread, it is recomputed.

use core::time::Duration;
use std::path::PathBuf;

use clipped_background::SourceIdentity;

use crate::peaks::{Level, Peak, MAX_BASE_BUCKETS};
use crate::waveform::{TrackDescriptor, TrackWaveform, Waveform};

/// What every entry starts with.
pub(crate) const MAGIC: [u8; 8] = *b"CLIPWAVE";

/// The format this build writes, and the only one it reads.
pub(crate) const VERSION: u16 = 1;

/// The flag that says an entry records why there is no waveform rather than a
/// waveform.
///
/// A recording that cannot be decoded has to be written down, or every lookup
/// misses and asks for the whole file to be read again ([`crate::WaveformCache::remember_failure`]).
/// It is a flag rather than a second file name so that one entry per recording
/// stays the rule: invalidation, pruning and the budget all work on it unchanged.
pub(crate) const FLAG_UNAVAILABLE: u16 = 0x0001;

/// The longest failure reason written into an entry.
///
/// Reasons are this crate's own messages, which are a line long; the bound is
/// against a pathological FFmpeg string rather than against anything expected.
const MAX_REASON_BYTES: usize = 1_024;

/// The most audio tracks one entry may describe.
///
/// A bound on what a corrupt header can make this allocate. Far above the
/// handful SPEC.md section 11 describes, and far below anything that would hurt.
const MAX_TRACKS: usize = 64;

/// The most pyramid levels one track may have.
///
/// Levels halve, so this is 2^64 base buckets — unreachable, which is the point:
/// it bounds a corrupt count without constraining a real one.
const MAX_LEVELS: usize = 64;

/// Why an entry could not be read.
///
/// Never surfaced to a user. A cache entry that cannot be read is a cache miss
/// and a recomputation; the variant exists so the log says which kind of broken
/// it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Corrupt {
    /// The file does not begin with [`MAGIC`].
    NotAWaveformFile,
    /// Written by a different version of this format.
    Version(u16),
    /// The file ends in the middle of something.
    Truncated,
    /// A count in the file is larger than this build will allocate for.
    Implausible(&'static str),
    /// A string in the file is not UTF-8.
    NotText,
}

impl core::fmt::Display for Corrupt {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotAWaveformFile => formatter.write_str("not a waveform cache entry"),
            Self::Version(version) => {
                write!(
                    formatter,
                    "written in format version {version}, not {VERSION}"
                )
            }
            Self::Truncated => formatter.write_str("ends part way through"),
            Self::Implausible(field) => write!(formatter, "declares an implausible {field}"),
            Self::NotText => formatter.write_str("holds a name that is not UTF-8"),
        }
    }
}

/// What one cache entry holds.
#[derive(Debug)]
pub(crate) enum Entry {
    /// Peaks.
    Ready(Waveform),
    /// The recording was analysed and produced none, and this is what the
    /// attempt said.
    Failed {
        /// Which version of which recording failed.
        source: SourceIdentity,
        /// What the failed attempt reported.
        reason: String,
    },
}

/// Writes a waveform as the bytes of a cache entry.
pub(crate) fn encode(waveform: &Waveform) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(estimate(waveform));
    write_header(
        &mut bytes,
        waveform.source(),
        0,
        narrow_u16(waveform.tracks().len()),
    );

    for track in waveform.tracks() {
        let descriptor = track.descriptor();
        // A track name comes from a container tag, so it is somebody else's
        // string. Clipped's own are a word long; anything approaching 64 kB is
        // not a label, and cutting it on a character boundary keeps the field
        // and its declared length in agreement.
        let name = truncate(descriptor.name().unwrap_or_default(), u16::MAX as usize).as_bytes();
        bytes.extend_from_slice(&descriptor.stream_index().to_le_bytes());
        bytes.extend_from_slice(&descriptor.sample_rate().to_le_bytes());
        bytes.extend_from_slice(&descriptor.channels().to_le_bytes());
        bytes.extend_from_slice(&narrow_u16(name.len()).to_le_bytes());
        bytes.extend_from_slice(name);
        bytes.extend_from_slice(&duration_nanos(track.duration()).to_le_bytes());
        bytes.extend_from_slice(&narrow_u16(track.levels().len()).to_le_bytes());

        for level in track.levels() {
            bytes.extend_from_slice(&level.bucket_nanos().to_le_bytes());
            bytes.extend_from_slice(&narrow_u32(level.peaks().len()).to_le_bytes());
            for peak in level.peaks() {
                bytes.push(peak.minimum().to_le_bytes()[0]);
                bytes.push(peak.maximum().to_le_bytes()[0]);
            }
        }
    }

    bytes
}

/// Writes an entry that records why a recording produced no waveform.
pub(crate) fn encode_failure(source: &SourceIdentity, reason: &str) -> Vec<u8> {
    let reason = truncate(reason, MAX_REASON_BYTES).as_bytes();
    let mut bytes = Vec::with_capacity(reason.len() + 512);
    write_header(&mut bytes, source, FLAG_UNAVAILABLE, 0);
    bytes.extend_from_slice(&narrow_u16(reason.len()).to_le_bytes());
    bytes.extend_from_slice(reason);
    bytes
}

/// Writes the part of an entry that does not depend on what kind of entry it is.
///
/// Identical for both kinds by design: pruning reads the identity out of the
/// front of every entry without knowing or caring which kind it found.
fn write_header(bytes: &mut Vec<u8>, source: &SourceIdentity, flags: u16, tracks: u16) {
    let path = source.path().to_string_lossy().into_owned();
    let path = path.as_bytes();

    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(&VERSION.to_le_bytes());
    // Flags. Bit 0 is [`FLAG_UNAVAILABLE`]; the rest are unused, and a reader
    // ignores them, so the next one defined has to be a change an older reader
    // is right to ignore.
    bytes.extend_from_slice(&flags.to_le_bytes());
    bytes.extend_from_slice(&tracks.to_le_bytes());
    // A 32-bit length, unlike every other string here, because a path is the
    // one field whose length this crate does not choose. Truncating it to fit a
    // `u16` would write a length that disagreed with the bytes that followed,
    // which is the one kind of corruption a reader cannot detect.
    bytes.extend_from_slice(&narrow_u32(path.len()).to_le_bytes());
    bytes.extend_from_slice(&source.size().to_le_bytes());
    bytes.extend_from_slice(&source.modified_nanos().to_le_bytes());
    bytes.extend_from_slice(path);
}

/// How large the encoding will be, so that it is built in one allocation.
fn estimate(waveform: &Waveform) -> usize {
    let peaks: usize = waveform
        .tracks()
        .iter()
        .flat_map(|track| track.levels())
        .map(|level| level.peaks().len() * 2 + 12)
        .sum();
    peaks + waveform.tracks().len() * 24 + 512
}

/// Reads a cache entry.
pub(crate) fn decode(bytes: &[u8]) -> Result<Entry, Corrupt> {
    let mut reader = Reader::new(bytes);
    if reader.take(MAGIC.len())? != MAGIC {
        return Err(Corrupt::NotAWaveformFile);
    }
    let version = reader.u16()?;
    if version != VERSION {
        return Err(Corrupt::Version(version));
    }
    let flags = reader.u16()?;
    let track_count = usize::from(reader.u16()?);
    if track_count > MAX_TRACKS {
        return Err(Corrupt::Implausible("track count"));
    }
    let path_length = usize::try_from(reader.u32()?).map_err(|_| Corrupt::Truncated)?;
    let size = reader.u64()?;
    let modified = reader.i64()?;
    let path = reader.text(path_length)?;
    let source = SourceIdentity::from_parts(PathBuf::from(path), size, modified);

    if flags & FLAG_UNAVAILABLE != 0 {
        let reason_length = usize::from(reader.u16()?);
        return Ok(Entry::Failed {
            source,
            reason: reader.text(reason_length)?,
        });
    }

    let mut tracks = Vec::with_capacity(track_count);
    for _ in 0..track_count {
        tracks.push(decode_track(&mut reader)?);
    }
    Ok(Entry::Ready(Waveform::new(source, tracks)))
}

/// Reads just the identity from the front of an entry.
///
/// What pruning needs: whether the recording an entry belongs to still exists,
/// without reading however many megabytes of peaks follow. The caller passes the
/// first few kilobytes of the file rather than all of it, so a
/// [`Corrupt::Truncated`] here means "the header did not fit in what you read"
/// as well as "the file is short" — both of which are answered the same way, by
/// treating the entry as unreadable.
pub(crate) fn decode_identity(bytes: &[u8]) -> Result<SourceIdentity, Corrupt> {
    let mut reader = Reader::new(bytes);
    if reader.take(MAGIC.len())? != MAGIC {
        return Err(Corrupt::NotAWaveformFile);
    }
    let version = reader.u16()?;
    if version != VERSION {
        return Err(Corrupt::Version(version));
    }
    let _flags = reader.u16()?;
    let _tracks = reader.u16()?;
    let path_length = usize::try_from(reader.u32()?).map_err(|_| Corrupt::Truncated)?;
    let size = reader.u64()?;
    let modified = reader.i64()?;
    let path = reader.text(path_length)?;
    Ok(SourceIdentity::from_parts(
        PathBuf::from(path),
        size,
        modified,
    ))
}

fn decode_track(reader: &mut Reader<'_>) -> Result<TrackWaveform, Corrupt> {
    let stream_index = reader.u32()?;
    let sample_rate = reader.u32()?;
    let channels = reader.u16()?;
    let name_length = usize::from(reader.u16()?);
    let name = reader.text(name_length)?;
    let duration = Duration::from_nanos(reader.u64()?);
    let level_count = usize::from(reader.u16()?);
    if level_count > MAX_LEVELS {
        return Err(Corrupt::Implausible("level count"));
    }

    let mut levels = Vec::with_capacity(level_count);
    for _ in 0..level_count {
        let bucket_nanos = reader.u64()?;
        let bucket_count = usize::try_from(reader.u32()?).map_err(|_| Corrupt::Truncated)?;
        if bucket_count > MAX_BASE_BUCKETS {
            return Err(Corrupt::Implausible("bucket count"));
        }
        let raw = reader.take(bucket_count * 2)?;
        let peaks = raw
            .chunks_exact(2)
            .map(|pair| Peak::new(pair[0] as i8, pair[1] as i8))
            .collect();
        levels.push(Level::new(bucket_nanos, peaks));
    }

    Ok(TrackWaveform::from_levels(
        TrackDescriptor::new(stream_index, sample_rate, channels, name),
        duration,
        levels,
    ))
}

/// A cursor that refuses to read past the end.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], Corrupt> {
        let end = self.at.checked_add(count).ok_or(Corrupt::Truncated)?;
        let slice = self.bytes.get(self.at..end).ok_or(Corrupt::Truncated)?;
        self.at = end;
        Ok(slice)
    }

    fn u16(&mut self) -> Result<u16, Corrupt> {
        Ok(u16::from_le_bytes(fixed(self.take(2)?)))
    }

    fn u32(&mut self) -> Result<u32, Corrupt> {
        Ok(u32::from_le_bytes(fixed(self.take(4)?)))
    }

    fn u64(&mut self) -> Result<u64, Corrupt> {
        Ok(u64::from_le_bytes(fixed(self.take(8)?)))
    }

    fn i64(&mut self) -> Result<i64, Corrupt> {
        Ok(i64::from_le_bytes(fixed(self.take(8)?)))
    }

    fn text(&mut self, length: usize) -> Result<String, Corrupt> {
        let bytes = self.take(length)?;
        core::str::from_utf8(bytes)
            .map(ToOwned::to_owned)
            .map_err(|_| Corrupt::NotText)
    }
}

/// The first `N` bytes of a slice the reader has already sized.
fn fixed<const N: usize>(bytes: &[u8]) -> [u8; N] {
    let mut buffer = [0u8; N];
    buffer.copy_from_slice(&bytes[..N]);
    buffer
}

/// Narrows a length that the writer's own bounds keep in range.
fn narrow_u16(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

/// Narrows a length that the writer's own bounds keep in range.
fn narrow_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// A duration in nanoseconds, saturating rather than wrapping.
fn duration_nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

/// The longest prefix of `text` that fits in `limit` bytes without splitting a
/// character.
fn truncate(text: &str, limit: usize) -> &str {
    if text.len() <= limit {
        return text;
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peaks::BASE_BUCKET;

    fn waveform() -> Waveform {
        let quiet = TrackWaveform::from_base(
            TrackDescriptor::new(1, 48_000, 2, "Game"),
            Duration::from_millis(2_500),
            BASE_BUCKET,
            (0..250)
                .map(|index| Peak::new(-(index % 100) as i8, (index % 100) as i8))
                .collect(),
        );
        let loud = TrackWaveform::from_base(
            TrackDescriptor::new(2, 44_100, 1, ""),
            Duration::from_millis(500),
            BASE_BUCKET,
            vec![Peak::new(-127, 127); 50],
        );
        Waveform::new(
            SourceIdentity::from_parts(PathBuf::from(r"C:\videos\match.mkv"), 1_234, -42),
            vec![quiet, loud],
        )
    }

    /// The peaks of an entry that is supposed to hold some.
    fn peaks_of(entry: Entry) -> Waveform {
        match entry {
            Entry::Ready(waveform) => waveform,
            Entry::Failed { reason, .. } => panic!("expected peaks, found a failure: {reason}"),
        }
    }

    #[test]
    fn an_entry_survives_a_round_trip_track_for_track_and_peak_for_peak() {
        let original = waveform();
        let decoded = peaks_of(decode(&encode(&original)).expect("the entry reads back"));

        assert_eq!(decoded.source(), original.source());
        assert_eq!(decoded.tracks().len(), 2);
        for (left, right) in decoded.tracks().iter().zip(original.tracks()) {
            assert_eq!(left.descriptor(), right.descriptor());
            assert_eq!(left.duration(), right.duration());
            assert_eq!(left.levels().len(), right.levels().len());
            for (from, to) in left.levels().iter().zip(right.levels()) {
                assert_eq!(from.bucket_nanos(), to.bucket_nanos());
                assert_eq!(from.peaks(), to.peaks());
            }
        }
    }

    #[test]
    fn a_recording_with_no_audio_round_trips_as_a_waveform_with_no_tracks() {
        let empty = Waveform::new(
            SourceIdentity::from_parts(PathBuf::from("silent.mkv"), 7, 8),
            Vec::new(),
        );
        let decoded = peaks_of(decode(&encode(&empty)).expect("the entry reads back"));
        assert!(decoded.is_silent());
        assert_eq!(decoded.source().size(), 7);
    }

    #[test]
    fn a_failure_entry_reads_back_as_a_failure_and_not_as_a_silent_recording() {
        // The distinction this flag exists for. A recording that could not be
        // decoded and a recording that genuinely has no audio are both "no
        // tracks" on the wire, and confusing them would tell a timeline that a
        // broken file is silent.
        let source = SourceIdentity::from_parts(PathBuf::from("truncated.mkv"), 99, 7);
        let bytes = encode_failure(&source, "the container could not be opened");

        match decode(&bytes).expect("the entry reads back") {
            Entry::Failed {
                source: read,
                reason,
            } => {
                assert_eq!(read, source);
                assert_eq!(reason, "the container could not be opened");
            }
            Entry::Ready(waveform) => {
                panic!(
                    "a failure entry decoded as {} tracks of peaks",
                    waveform.tracks().len()
                )
            }
        }

        // And pruning reads its identity out of the front exactly as it does
        // for peaks, so a failure entry is invalidated and swept like any other.
        assert_eq!(
            decode_identity(&bytes).expect("the header reads back"),
            source
        );
    }

    #[test]
    fn a_failure_entry_cut_short_is_refused_at_every_length() {
        let bytes = encode_failure(
            &SourceIdentity::from_parts(PathBuf::from("truncated.mkv"), 99, 7),
            "a reason of some length",
        );
        for length in 0..bytes.len() {
            assert!(
                decode(&bytes[..length]).is_err(),
                "a {length}-byte prefix of a {}-byte failure entry decoded",
                bytes.len()
            );
        }
        assert!(decode(&bytes).is_ok());
    }

    #[test]
    fn a_reason_longer_than_the_format_holds_is_cut_rather_than_mis_declared() {
        let source = SourceIdentity::from_parts(PathBuf::from("truncated.mkv"), 1, 1);
        let bytes = encode_failure(&source, &"é".repeat(MAX_REASON_BYTES));
        match decode(&bytes).expect("the entry still reads back") {
            Entry::Failed { reason, .. } => {
                assert!(reason.len() <= MAX_REASON_BYTES, "{}", reason.len());
                assert!(reason.chars().all(|character| character == 'é'));
            }
            Entry::Ready(_) => panic!("a failure entry decoded as peaks"),
        }
    }

    #[test]
    fn the_identity_can_be_read_without_the_peaks() {
        let bytes = encode(&waveform());
        let identity = decode_identity(&bytes).expect("the header reads back");
        assert_eq!(&identity, waveform().source());
        // And it does not need the whole file: 64 bytes is past the path here.
        assert_eq!(
            decode_identity(&bytes[..64]).expect("the header still reads back"),
            identity
        );
    }

    #[test]
    fn something_that_is_not_an_entry_is_refused_rather_than_interpreted() {
        assert_eq!(
            decode(b"not a waveform at all").unwrap_err(),
            Corrupt::NotAWaveformFile
        );
        assert_eq!(decode(&[]).unwrap_err(), Corrupt::Truncated);
    }

    #[test]
    fn an_entry_from_another_format_version_is_refused() {
        let mut bytes = encode(&waveform());
        bytes[8] = 99;
        assert_eq!(decode(&bytes).unwrap_err(), Corrupt::Version(99));
    }

    #[test]
    fn an_entry_cut_short_is_refused_at_every_length_rather_than_read_past() {
        // A power cut during a write, or a half-copied cache directory. Every
        // truncation has to be refused, not just the obvious ones: this is the
        // check that the reader's bounds are the file's and not the buffer's.
        let bytes = encode(&waveform());
        for length in 0..bytes.len() {
            assert!(
                decode(&bytes[..length]).is_err(),
                "a {length}-byte prefix of a {}-byte entry decoded",
                bytes.len()
            );
        }
        assert!(decode(&bytes).is_ok());
    }

    #[test]
    fn an_implausible_count_is_refused_before_it_is_allocated_for() {
        let mut bytes = encode(&waveform());
        // The track count, at offset 12.
        bytes[12..14].copy_from_slice(&1_000u16.to_le_bytes());
        assert_eq!(
            decode(&bytes).unwrap_err(),
            Corrupt::Implausible("track count")
        );
    }

    #[test]
    fn a_name_that_is_not_text_is_refused() {
        let mut bytes = encode(&waveform());
        // The path starts at offset 34; break its first byte.
        bytes[34] = 0xff;
        assert_eq!(decode(&bytes).unwrap_err(), Corrupt::NotText);
    }

    #[test]
    fn a_name_too_long_for_its_field_is_cut_on_a_character_boundary() {
        assert_eq!(truncate("Microphone", 64), "Microphone");
        // "é" is two bytes, so a limit that falls inside it takes neither half.
        assert_eq!(truncate("é", 1), "");
        assert_eq!(truncate("aé", 2), "a");
    }
}
