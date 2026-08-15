//! What a replay buffer refuses, and why.
//!
//! There are only two kinds of refusal here, and keeping them apart matters
//! because they need different answers from a caller. A configuration error is
//! a mistake made before anything was recorded and is fixed by asking for
//! something else. A lease error is a fact about what the buffer happens to
//! hold at this instant — the recording only started four seconds ago, the
//! requested range is older than anything still held — and is answered by
//! saving what there is, or by saying how much there was (AGENTS.md section 45).

use core::fmt;
use core::time::Duration;

use crate::range::TimeRange;

/// A window, segment length or bitrate a buffer cannot be built from.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConfigError {
    /// The requested window is outside the supported range.
    ///
    /// SPEC.md section 16 asks for 30 seconds to 30 minutes;
    /// [`MINIMUM_WINDOW`](crate::MINIMUM_WINDOW) and
    /// [`MAXIMUM_WINDOW`](crate::MAXIMUM_WINDOW) are those bounds.
    WindowOutOfRange {
        /// What was asked for.
        requested: Duration,
        /// The shortest window a buffer may be configured with.
        minimum: Duration,
        /// The longest window a buffer may be configured with.
        maximum: Duration,
    },
    /// The segment length is zero, or is longer than the window itself.
    ///
    /// A segment longer than the window would mean a buffer that cannot hold
    /// even one complete segment of history, which is a buffer that can never
    /// satisfy the duration it was configured with.
    SegmentOutOfRange {
        /// What was asked for.
        requested: Duration,
        /// The window it has to fit inside.
        window: Duration,
    },
    /// The memory ceiling is smaller than the window needs at the configured
    /// bitrate, so the window could never be filled.
    ///
    /// Refused rather than accepted, because a buffer that silently keeps ten
    /// seconds when it was asked for five minutes is worse than one that says
    /// the numbers do not fit (AGENTS.md section 54).
    CeilingBelowWindow {
        /// The ceiling that was asked for, in bytes.
        requested: u64,
        /// What the window needs at the configured bitrate, in bytes.
        needed: u64,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WindowOutOfRange {
                requested,
                minimum,
                maximum,
            } => write!(
                formatter,
                "a replay buffer of {} is outside the supported range of {} to {}",
                seconds(*requested),
                seconds(*minimum),
                seconds(*maximum)
            ),
            Self::SegmentOutOfRange { requested, window } => write!(
                formatter,
                "a segment length of {} does not fit in a {} replay buffer",
                seconds(*requested),
                seconds(*window)
            ),
            Self::CeilingBelowWindow { requested, needed } => write!(
                formatter,
                "a memory ceiling of {} cannot hold the configured window, which needs {}",
                mebibytes(*requested),
                mebibytes(*needed)
            ),
        }
    }
}

impl core::error::Error for ConfigError {}

/// Why a range could not be leased for a save.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LeaseError {
    /// Nothing has been buffered yet.
    ///
    /// Either no packet has arrived, or none of the packets that arrived was a
    /// keyframe, so no segment could be started: a segment that does not begin
    /// on a keyframe cannot be decoded on its own, so there is nothing to save.
    Empty,
    /// The requested range ends before the oldest packet still held, or begins
    /// after the newest.
    ///
    /// A range that merely *overhangs* what is held is not this: it is leased,
    /// and [`SegmentLease::is_complete`](crate::SegmentLease::is_complete)
    /// reports the shortfall. This is a range with no overlap at all.
    OutsideBuffer {
        /// What was asked for.
        requested: TimeRange,
        /// What the buffer holds.
        held: TimeRange,
    },
    /// A segment the buffer had spilled to disk could not be read back.
    ///
    /// The lease is refused rather than returned short. A clip written from
    /// the segments that *did* read would be missing the middle of itself and
    /// would say nothing about it, which is the one outcome worse than no clip
    /// (AGENTS.md section 22).
    Unreadable {
        /// The segment that would not read.
        segment: crate::SegmentId,
        /// What kind of failure it was.
        ///
        /// The kind and the message rather than the `io::Error` itself, because
        /// this type is `Clone` and comparable and an `io::Error` is neither —
        /// and a caller that wants to tell "the drive went away" from "the file
        /// is corrupt" needs the kind rather than the object.
        kind: std::io::ErrorKind,
        /// What the filesystem said.
        detail: String,
    },
}

impl fmt::Display for LeaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter
                .write_str("the replay buffer is empty: no encoded keyframe has reached it yet"),
            Self::OutsideBuffer { requested, held } => write!(
                formatter,
                "the replay buffer holds {held} and nothing of the requested {requested}"
            ),
            Self::Unreadable {
                segment, detail, ..
            } => write!(
                formatter,
                "the replay buffer had spilled {segment} to disk and could not read it back: \
                 {detail}"
            ),
        }
    }
}

impl core::error::Error for LeaseError {}

/// A duration, to a tenth of a second, for a message a user may read.
fn seconds(duration: Duration) -> String {
    format!("{:.1}s", duration.as_secs_f64())
}

/// A byte count, in mebibytes, for a message a user may read.
///
/// 1024², labelled MiB, because that is what the figure is; Windows Task
/// Manager shows the same quantity and calls it MB, which is exactly the
/// confusion worth not adding to.
fn mebibytes(bytes: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let mebibytes = bytes as f64 / (1024.0 * 1024.0);
    format!("{mebibytes:.0} MiB")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_refusal_names_the_supported_range() {
        let error = ConfigError::WindowOutOfRange {
            requested: Duration::from_secs(3600),
            minimum: Duration::from_secs(30),
            maximum: Duration::from_secs(1800),
        };

        assert_eq!(
            error.to_string(),
            "a replay buffer of 3600.0s is outside the supported range of 30.0s to 1800.0s"
        );
    }

    #[test]
    fn a_ceiling_refusal_says_what_the_window_needs() {
        let error = ConfigError::CeilingBelowWindow {
            requested: 64 * 1024 * 1024,
            needed: 704 * 1024 * 1024,
        };

        assert_eq!(
            error.to_string(),
            "a memory ceiling of 64 MiB cannot hold the configured window, which needs 704 MiB"
        );
    }

    #[test]
    fn an_empty_buffer_says_what_is_missing_rather_than_that_it_is_empty() {
        // "Empty" alone would send somebody looking for a bug in the buffer.
        // The reason a recording that has been running for a second has nothing
        // to save is that the first keyframe has not come out of the encoder.
        assert!(LeaseError::Empty.to_string().contains("keyframe"));
    }
}
