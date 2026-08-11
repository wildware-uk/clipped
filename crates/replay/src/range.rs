//! Ranges of media time, and what a range means here.
//!
//! Every time in this crate is *media time*: nanoseconds from the epoch the
//! recording's capture clock fixed at its first frame, which is the same zero
//! the encoder's packet timestamps and the container's timestamps use
//! (`docs/av-sync.md`). Nothing here reads a wall clock, so a buffer behaves
//! identically in a test that pushes an hour of packets in a millisecond and in
//! a recording that takes an hour to produce them (AGENTS.md section 25).
//!
//! A range is measured between *presentation* timestamps and is inclusive at
//! both ends: `TimeRange::new(0s, 2s)` covers the frame shown at two seconds.
//! It deliberately does not include that frame's display duration, because the
//! buffer is never told the frame rate — the encoder does not stamp a duration
//! on a packet, and inventing one would put a number in a report that nothing
//! measured (AGENTS.md section 19).

use core::cmp::Ordering;
use core::fmt;
use core::time::Duration;

/// A span of media time, inclusive at both ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimeRange {
    start: Duration,
    end: Duration,
}

impl TimeRange {
    /// A range from `start` to `end`.
    ///
    /// The two are swapped if they arrive the wrong way round rather than
    /// refused: every caller of this is converting a user's "the last minute"
    /// into a pair of instants, and a range that is its own reverse is the same
    /// range.
    #[must_use]
    pub fn new(start: Duration, end: Duration) -> Self {
        match start.cmp(&end) {
            Ordering::Greater => Self {
                start: end,
                end: start,
            },
            _ => Self { start, end },
        }
    }

    /// The `length` of media time ending at `end`.
    ///
    /// What "save the last 30 seconds" means, and it saturates at zero: a
    /// recording four seconds old asked for the last thirty yields the four
    /// seconds there are.
    #[must_use]
    pub fn ending_at(end: Duration, length: Duration) -> Self {
        Self {
            start: end.saturating_sub(length),
            end,
        }
    }

    /// The first instant in the range.
    #[must_use]
    pub const fn start(&self) -> Duration {
        self.start
    }

    /// The last instant in the range.
    #[must_use]
    pub const fn end(&self) -> Duration {
        self.end
    }

    /// How long the range is.
    #[must_use]
    pub fn length(&self) -> Duration {
        self.end.saturating_sub(self.start)
    }

    /// Whether `at` falls within the range, inclusive at both ends.
    #[must_use]
    pub fn contains(&self, at: Duration) -> bool {
        self.start <= at && at <= self.end
    }

    /// Whether any instant belongs to both ranges.
    #[must_use]
    pub fn overlaps(&self, other: Self) -> bool {
        self.start <= other.end && other.start <= self.end
    }

    /// This range extended to include `other`.
    #[must_use]
    pub fn union(&self, other: Self) -> Self {
        Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

impl fmt::Display for TimeRange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:.3}s to {:.3}s",
            self.start.as_secs_f64(),
            self.end.as_secs_f64()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }

    #[test]
    fn a_reversed_range_is_the_range_it_describes() {
        assert_eq!(
            TimeRange::new(secs(9), secs(4)),
            TimeRange::new(secs(4), secs(9))
        );
    }

    #[test]
    fn the_last_thirty_seconds_of_a_four_second_recording_is_four_seconds() {
        // The hotkey pressed early in a session. Saturating rather than
        // underflowing is what makes "save the last 30 seconds" answerable at
        // any point in a recording.
        let range = TimeRange::ending_at(secs(4), secs(30));

        assert_eq!(range.start(), Duration::ZERO);
        assert_eq!(range.end(), secs(4));
        assert_eq!(range.length(), secs(4));
    }

    #[test]
    fn ranges_that_touch_at_one_instant_overlap() {
        // A segment ending exactly where the requested range begins holds the
        // keyframe that range has to be decoded from, so it is selected.
        let earlier = TimeRange::new(secs(0), secs(2));
        let later = TimeRange::new(secs(2), secs(4));

        assert!(earlier.overlaps(later));
        assert!(later.overlaps(earlier));
    }

    #[test]
    fn ranges_that_do_not_touch_do_not_overlap() {
        let earlier = TimeRange::new(secs(0), secs(2));
        let later = TimeRange::new(secs(2) + Duration::from_nanos(1), secs(4));

        assert!(!earlier.overlaps(later));
    }

    #[test]
    fn a_range_reads_as_the_seconds_it_covers() {
        assert_eq!(
            TimeRange::new(Duration::from_millis(1500), secs(31)).to_string(),
            "1.500s to 31.000s"
        );
    }
}
