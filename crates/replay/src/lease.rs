//! A read lease: the segments a save is reading, held against eviction.
//!
//! # The problem this type exists for
//!
//! A save reads segments while the encoder is still producing new ones and the
//! buffer is still evicting old ones. The three happen on different threads and
//! at different rates, so between choosing the segments a clip needs and
//! finishing reading them, the buffer may well have evicted every one of them —
//! a thirty-second buffer at two-second segments turns over completely in
//! thirty seconds, and writing a five-minute clip to a slow disk takes longer
//! than that.
//!
//! The answer is a reference count rather than a rule. Each sealed segment
//! lives behind an [`Arc`], the buffer holds one reference and a lease holds
//! another, and eviction drops only the buffer's. A segment the buffer has
//! evicted while a lease holds it is alive, unchanged and complete until the
//! lease is dropped; there is no window in which it could be otherwise, because
//! the lease's references are cloned under the same lock eviction takes.
//!
//! The cost is stated rather than hidden: memory a lease is holding still
//! counts against the buffer's ceiling
//! ([`ReplayStats::bytes_held`](crate::ReplayStats::bytes_held)), so a save
//! that takes a very long time shortens the window rather than growing the
//! process. `crate::buffer` describes the eviction that enforces it.

use core::time::Duration;
use std::sync::Arc;

use crate::range::TimeRange;
use crate::segment::{Segment, SegmentAudio, SegmentPacket};

/// The segments covering a requested range, held against eviction.
///
/// Dropping it releases them. Everything it reports was decided when it was
/// taken and does not change afterwards.
#[derive(Debug)]
pub struct SegmentLease {
    segments: Vec<Arc<Segment>>,
    requested: TimeRange,
    requested_length: Duration,
    covered: TimeRange,
}

impl SegmentLease {
    /// Builds a lease over `segments`, which must be in order and non-empty.
    ///
    /// `requested_length` is what the caller asked for, which is not always
    /// `requested.length()`: "the last thirty seconds" of a recording four
    /// seconds old is a four-second range, because media time begins at zero,
    /// and the buffer still has to be able to say it produced twenty-six
    /// seconds less than was wanted.
    pub(crate) fn new(
        segments: Vec<Arc<Segment>>,
        requested: TimeRange,
        requested_length: Duration,
    ) -> Self {
        debug_assert!(!segments.is_empty(), "a lease over nothing is not a lease");
        let covered = TimeRange::new(
            segments
                .first()
                .map_or(Duration::ZERO, |segment| segment.start()),
            segments
                .last()
                .map_or(Duration::ZERO, |segment| segment.last_presentation()),
        );

        Self {
            segments,
            requested,
            requested_length,
            covered,
        }
    }

    /// What was asked for.
    #[must_use]
    pub const fn requested(&self) -> TimeRange {
        self.requested
    }

    /// How much video was asked for.
    #[must_use]
    pub const fn requested_length(&self) -> Duration {
        self.requested_length
    }

    /// What the leased segments actually cover.
    ///
    /// Never narrower than [`requested`](Self::requested) at an end the buffer
    /// held, because a segment is taken whole: it begins on a keyframe, and
    /// cutting it anywhere else would produce a clip whose first pictures
    /// cannot be decoded.
    #[must_use]
    pub const fn covered(&self) -> TimeRange {
        self.covered
    }

    /// Whether the buffer held the whole of the requested range.
    ///
    /// `false` when the recording is younger than the requested duration, or
    /// when the range reaches back past material that has already been evicted.
    /// The lease is still valid and still worth saving — it is simply shorter
    /// than was asked for, which is what [`shortfall`](Self::shortfall) says.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.covered.start() <= self.requested.start()
            && self.covered.end() >= self.requested.end()
            && self.covered.length() >= self.requested_length
    }

    /// How much of the requested video the buffer did not hold.
    #[must_use]
    pub fn shortfall(&self) -> Duration {
        let before = self.covered.start().saturating_sub(self.requested.start());
        let after = self.requested.end().saturating_sub(self.covered.end());
        (before + after).max(self.requested_length.saturating_sub(self.covered.length()))
    }

    /// Extra material before the requested start, kept because the segment
    /// containing that instant begins earlier.
    ///
    /// Between zero and one segment, and it is not trimmed off: a clip that
    /// began at the requested instant would open with pictures nothing can
    /// decode, so a saved clip begins here and says it did
    /// ([`SavedClip::leading_slack`](crate::SavedClip::leading_slack)).
    #[must_use]
    pub fn leading_slack(&self) -> Duration {
        self.requested.start().saturating_sub(self.covered.start())
    }

    /// Extra material after the requested end, kept for the same reason.
    ///
    /// This end *is* trimmed when the clip is written, because nothing after
    /// the requested end has to be there for what precedes it to decode
    /// (`crate::save`). A lease still holds it, so that the decision belongs to
    /// the save rather than to the selection.
    #[must_use]
    pub fn trailing_slack(&self) -> Duration {
        self.covered.end().saturating_sub(self.requested.end())
    }

    /// How many segments are held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.segments.len()
    }

    /// Whether it holds no segments, which a lease taken from the buffer never
    /// does.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// How many bytes of coded video are held.
    #[must_use]
    pub fn byte_len(&self) -> usize {
        self.segments.iter().map(|segment| segment.byte_len()).sum()
    }

    /// The segments, oldest first.
    pub fn segments(&self) -> impl ExactSizeIterator<Item = &Segment> + '_ {
        self.segments.iter().map(AsRef::as_ref)
    }

    /// Every packet in every segment, in the order the encoder produced them.
    ///
    /// This is what [`save_clip`](crate::save_clip) writes, minus whatever falls
    /// after the requested end. The first packet is a keyframe, which is what
    /// makes the result decodable on its own.
    pub fn packets(&self) -> impl Iterator<Item = SegmentPacket<'_>> + '_ {
        self.segments.iter().flat_map(|segment| segment.packets())
    }

    /// Every block of captured audio in every segment.
    ///
    /// Unfiltered, and in the order it arrived rather than in timestamp order:
    /// audio is appended to whichever segment is open when it arrives, so a
    /// block near a segment boundary can carry a timestamp belonging to its
    /// neighbour. [`save_clip`](crate::save_clip) selects by timestamp for that
    /// reason, and nothing here pretends the two clocks were locked together.
    pub fn audio(&self) -> impl Iterator<Item = SegmentAudio<'_>> + '_ {
        self.segments.iter().flat_map(|segment| segment.audio())
    }
}
