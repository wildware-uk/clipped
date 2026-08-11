//! The two kinds of time an edit is written in, and the arithmetic between
//! them.
//!
//! Both count **nanoseconds**, which is not an arbitrary unit: the encoder
//! stamps packets in nanoseconds (`clipped-encoder`'s software backend uses a
//! `1/1_000_000_000` time base) and `clipped-muxer` rescales them into whatever
//! the container asks for. A position in this document is therefore the same
//! number the recorder already wrote, with no conversion in between to round.
//!
//! Nanoseconds also stay exact on the other side of the IPC boundary, where
//! the editor is JavaScript and integers are exact only below 2^53. That is a
//! hundred and four days of nanoseconds. A recording is hours.
//!
//! # Why not frames
//!
//! A frame index would be smaller and would make [issue
//! #84](https://github.com/wildware-uk/clipped/issues/84)'s "frame-accurate"
//! boundaries look automatic. It would also assume a constant frame rate,
//! which a recording made while a game dropped frames does not have, and it
//! would make a document meaningless if the same edit ever referred to two
//! recordings at different rates ([issue
//! #88](https://github.com/wildware-uk/clipped/issues/88)). So the document
//! stores time and the exporter snaps to frames, rather than the document
//! storing a frame count nobody can interpret without opening the file.

use core::time::Duration;

use serde::{Deserialize, Serialize};

/// A position in one source recording's own timeline.
///
/// Counted in nanoseconds from that recording's first frame, which is the
/// timeline the recorder wrote and a decoder seeks in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceTime {
    nanos: u64,
}

/// A position on the edited timeline.
///
/// Counted in nanoseconds from the start of the clip: the playhead, an
/// overlay's timing range, and the timeline of the file an export will write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OutputTime {
    nanos: u64,
}

/// Generates the identical, tiny surface both time types have.
///
/// A macro rather than a shared generic `Time<Kind>`, because the entire point
/// of having two types is that neither can be passed where the other is
/// expected; a generic would have made them one type with a phantom label and
/// left `SourceTime + OutputTime` expressible with a turbofish.
macro_rules! time_type {
    ($name:ident, $what:literal) => {
        impl $name {
            #[doc = concat!("The start of ", $what, ".")]
            pub const ZERO: Self = Self { nanos: 0 };

            #[doc = concat!("A position ", $what, ", in nanoseconds.")]
            #[must_use]
            pub const fn from_nanos(nanos: u64) -> Self {
                Self { nanos }
            }

            /// The position in nanoseconds.
            #[must_use]
            pub const fn as_nanos(self) -> u64 {
                self.nanos
            }

            /// The position as a [`Duration`], for callers that speak in them.
            #[must_use]
            pub const fn as_duration(self) -> Duration {
                Duration::from_nanos(self.nanos)
            }

            /// The position `nanos` later, or `None` if that overflows.
            ///
            /// Overflow is unreachable with real recordings — `u64` nanoseconds
            /// is five hundred and eighty-four years — but a document is user
            /// data that a build of Clipped this old may never have written, so
            /// it is refused rather than wrapped.
            #[must_use]
            pub const fn checked_add_nanos(self, nanos: u64) -> Option<Self> {
                match self.nanos.checked_add(nanos) {
                    Some(nanos) => Some(Self { nanos }),
                    None => None,
                }
            }

            /// How far `self` is beyond `earlier`, or `None` if it is before it.
            #[must_use]
            pub const fn nanos_since(self, earlier: Self) -> Option<u64> {
                self.nanos.checked_sub(earlier.nanos)
            }
        }
    };
}

time_type!(SourceTime, "a source recording");
time_type!(OutputTime, "the edited timeline");

/// Generates a half-open span over one of the time types.
///
/// Half-open — `[start, end)` — throughout, so that the segment ending at
/// twelve seconds and the segment starting at twelve seconds do not both claim
/// the frame there. A duplicated frame at every cut is the classic way an
/// editor's preview and its export disagree.
macro_rules! span_type {
    ($name:ident, $time:ident, $what:literal) => {
        #[doc = concat!("A half-open range of ", $what, ": `[start, end)`.")]
        ///
        /// Constructed through [`new`](Self::new), which refuses an empty or
        /// backwards range. Deserialisation cannot go through a constructor, so
        /// a span read from a document is checked by
        /// [`EditDocument::validate`](crate::EditDocument::validate) instead —
        /// which every read and every write runs.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct $name {
            start: $time,
            end: $time,
        }

        impl $name {
            /// A span from `start` up to but not including `end`.
            ///
            /// `None` when `end` is not strictly after `start`: a zero-length
            /// span contributes nothing and is more likely a bug than an
            /// intention.
            #[must_use]
            pub const fn new(start: $time, end: $time) -> Option<Self> {
                if end.as_nanos() > start.as_nanos() {
                    Some(Self { start, end })
                } else {
                    None
                }
            }

            /// Where the span starts, inclusive.
            #[must_use]
            pub const fn start(self) -> $time {
                self.start
            }

            /// Where the span ends, exclusive.
            #[must_use]
            pub const fn end(self) -> $time {
                self.end
            }

            /// How long the span is, in nanoseconds; zero if it is not valid.
            #[must_use]
            pub const fn duration_nanos(self) -> u64 {
                self.end.as_nanos().saturating_sub(self.start.as_nanos())
            }

            /// How long the span is.
            #[must_use]
            pub const fn duration(self) -> Duration {
                Duration::from_nanos(self.duration_nanos())
            }

            /// Whether `at` falls inside the span, `end` excluded.
            #[must_use]
            pub const fn contains(self, at: $time) -> bool {
                at.as_nanos() >= self.start.as_nanos() && at.as_nanos() < self.end.as_nanos()
            }

            /// Whether the span is the right way round and not empty.
            #[must_use]
            pub const fn is_valid(self) -> bool {
                self.end.as_nanos() > self.start.as_nanos()
            }
        }
    };
}

span_type!(SourceSpan, SourceTime, "one source recording's timeline");
span_type!(OutputSpan, OutputTime, "the edited timeline");

/// How fast a segment plays, as an exact ratio.
///
/// `2/1` is double speed and half the output duration; `1/2` is half speed and
/// twice the output duration. A ratio rather than a float because the number
/// decides where every later frame of the clip lands: `0.1` is not a tenth in
/// binary floating point, and a preview and an export that round it at
/// different moments drift apart over a long clip. Two integers divide the
/// same way everywhere, for ever.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Speed {
    numerator: u32,
    denominator: u32,
}

impl Default for Speed {
    fn default() -> Self {
        Self::NORMAL
    }
}

impl Speed {
    /// Ordinary playback: one second of source is one second of output.
    pub const NORMAL: Self = Self {
        numerator: 1,
        denominator: 1,
    };

    /// A speed of `numerator / denominator`, or `None` if either is zero.
    ///
    /// Not reduced: `2/2` is stored as written and behaves exactly as `1/1`
    /// does. Reducing would mean a document that does not round-trip to the
    /// bytes it arrived as, for no benefit to anything that reads it.
    #[must_use]
    pub const fn new(numerator: u32, denominator: u32) -> Option<Self> {
        if numerator == 0 || denominator == 0 {
            return None;
        }
        Some(Self {
            numerator,
            denominator,
        })
    }

    /// The ratio's numerator.
    #[must_use]
    pub const fn numerator(self) -> u32 {
        self.numerator
    }

    /// The ratio's denominator.
    #[must_use]
    pub const fn denominator(self) -> u32 {
        self.denominator
    }

    /// Whether both halves are non-zero, which deserialisation cannot promise.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.numerator != 0 && self.denominator != 0
    }

    /// Whether this speed leaves the material at its recorded rate.
    ///
    /// True for any ratio equal to one, not only for [`NORMAL`](Self::NORMAL),
    /// because an exporter deciding whether it may copy the stream instead of
    /// re-encoding it cares about the value and not about how it was written.
    #[must_use]
    pub const fn is_normal(self) -> bool {
        self.numerator == self.denominator
    }

    /// How much output `source_nanos` of material produces at this speed.
    ///
    /// Truncating division, in 128-bit arithmetic so that hours of nanoseconds
    /// multiplied by a large numerator cannot wrap. Truncation rather than
    /// rounding because the alternative is a boundary that lands one
    /// nanosecond past the end of the material it came from; the leftover is
    /// under a nanosecond and the exporter snaps to a frame anyway.
    #[must_use]
    pub fn output_nanos(self, source_nanos: u64) -> Option<u64> {
        if !self.is_valid() {
            return None;
        }
        let scaled = u128::from(source_nanos) * u128::from(self.denominator);
        u64::try_from(scaled / u128::from(self.numerator)).ok()
    }

    /// How much material `output_nanos` of output consumes at this speed.
    ///
    /// The inverse of [`output_nanos`](Self::output_nanos), and the direction
    /// an exporter actually reads: given a position in the file being written,
    /// which position in the recording does it come from.
    #[must_use]
    pub fn source_nanos(self, output_nanos: u64) -> Option<u64> {
        if !self.is_valid() {
            return None;
        }
        let scaled = u128::from(output_nanos) * u128::from(self.numerator);
        u64::try_from(scaled / u128::from(self.denominator)).ok()
    }
}

/// Serialises a [`Duration`] as whole nanoseconds.
///
/// Used for the fade lengths in [`crate::audio`]. `Duration`'s own
/// representation is a struct of seconds and nanoseconds, which would put two
/// numbers in the document for one quantity and read oddly beside every other
/// time in it.
pub(crate) mod duration_nanos {
    use core::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    /// Writes the duration as a nanosecond count, saturating at `u64::MAX`.
    pub(crate) fn serialize<S: Serializer>(
        value: &Duration,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let nanos = u64::try_from(value.as_nanos()).unwrap_or(u64::MAX);
        serializer.serialize_u64(nanos)
    }

    /// Reads a nanosecond count back.
    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Duration, D::Error> {
        u64::deserialize(deserializer).map(Duration::from_nanos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_span_has_to_end_after_it_starts() {
        let start = SourceTime::from_nanos(1_000);
        assert!(SourceSpan::new(start, SourceTime::from_nanos(1_001)).is_some());
        assert!(
            SourceSpan::new(start, start).is_none(),
            "an empty span contributes nothing to an edit"
        );
        assert!(SourceSpan::new(start, SourceTime::from_nanos(999)).is_none());
    }

    #[test]
    fn a_span_is_half_open_so_a_cut_does_not_duplicate_a_frame() {
        let span = OutputSpan::new(OutputTime::from_nanos(10), OutputTime::from_nanos(20))
            .expect("the span is valid");

        assert!(span.contains(OutputTime::from_nanos(10)));
        assert!(span.contains(OutputTime::from_nanos(19)));
        assert!(
            !span.contains(OutputTime::from_nanos(20)),
            "the end belongs to whatever comes next"
        );
        assert_eq!(span.duration_nanos(), 10);
    }

    #[test]
    fn speed_refuses_a_zero_on_either_side() {
        assert!(Speed::new(0, 1).is_none());
        assert!(Speed::new(1, 0).is_none());
        assert_eq!(
            Speed::new(2, 1).expect("two times is a speed").numerator(),
            2
        );
    }

    #[test]
    fn double_speed_halves_the_output_and_quarter_speed_quadruples_it() {
        let double = Speed::new(2, 1).expect("a valid speed");
        let quarter = Speed::new(1, 4).expect("a valid speed");

        assert_eq!(double.output_nanos(8_000_000_000), Some(4_000_000_000));
        assert_eq!(quarter.output_nanos(8_000_000_000), Some(32_000_000_000));
        // And back the other way, which is the direction an export reads.
        assert_eq!(double.source_nanos(4_000_000_000), Some(8_000_000_000));
        assert_eq!(quarter.source_nanos(32_000_000_000), Some(8_000_000_000));
    }

    #[test]
    fn speed_arithmetic_survives_an_intermediate_that_does_not_fit_in_u64() {
        let hour_nanos = 3_600_000_000_000_u64;
        // An hour of nanoseconds times ten million is 3.6e19, which is past
        // `u64::MAX` — but a millionth of that is 3.6e18, which is not. The
        // 128-bit intermediate is the whole difference between this answer and
        // a wrapped one.
        let slow = Speed::new(10, 10_000_000).expect("a valid speed");

        assert_eq!(
            slow.output_nanos(hour_nanos),
            Some(3_600_000_000_000_000_000)
        );
        assert_eq!(slow.source_nanos(hour_nanos), Some(3_600_000));
    }

    #[test]
    fn a_length_that_does_not_fit_is_reported_rather_than_wrapped() {
        let hour_nanos = 3_600_000_000_000_u64;
        let far_too_slow = Speed::new(1, 10_000_000).expect("a valid speed");

        assert_eq!(
            far_too_slow.output_nanos(hour_nanos),
            None,
            "an hour played ten million times slower is longer than an edit can \
             represent, and saying so beats wrapping"
        );
    }

    #[test]
    fn division_truncates_rather_than_rounding() {
        let third = Speed::new(3, 1).expect("a valid speed");
        assert_eq!(third.output_nanos(10), Some(3));
        assert_eq!(third.source_nanos(3), Some(9));
    }

    #[test]
    fn any_ratio_equal_to_one_is_normal_speed() {
        assert!(Speed::NORMAL.is_normal());
        assert!(Speed::new(2, 2).expect("a valid speed").is_normal());
        assert!(!Speed::new(2, 1).expect("a valid speed").is_normal());
    }

    #[test]
    fn a_time_reports_how_far_it_is_past_another() {
        let start = OutputTime::from_nanos(500);
        assert_eq!(OutputTime::from_nanos(800).nanos_since(start), Some(300));
        assert_eq!(OutputTime::from_nanos(400).nanos_since(start), None);
    }

    #[test]
    fn a_time_refuses_to_wrap() {
        assert_eq!(SourceTime::from_nanos(u64::MAX).checked_add_nanos(1), None);
    }
}
