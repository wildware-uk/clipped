//! How long a buffer keeps, how finely it keeps it, and what that costs.
//!
//! # The ceiling is arithmetic, not a guess
//!
//! A rolling buffer of encoded video occupies its bitrate multiplied by its
//! duration, and nothing else of consequence. That makes the ceiling
//! computable before a single frame is captured, which is the property this
//! whole design exists for: at 1080p60 a raw BGRA frame is 8.3 MB and thirty
//! minutes of them is 834 GiB, whereas thirty minutes of the same footage at the
//! bitrate a recording actually uses is a number a machine can be asked for in
//! advance (SPEC.md section 16).
//!
//! [`ReplayConfig::expected_bytes`] is that number and
//! [`ReplayConfig::memory_ceiling`] is the limit derived from it. The buffer
//! never exceeds the ceiling: it evicts oldest-first to stay under it, which
//! shortens the window rather than growing the process, and says so
//! ([`ReplayStats::segments_evicted_over_ceiling`](crate::ReplayStats::segments_evicted_over_ceiling)).
//! Documented behaviour under pressure beats an allocation that fails at the
//! worst possible moment (AGENTS.md section 16).
//!
//! One term sits outside the ceiling, and is stated rather than hidden: a save
//! in progress holds the segments it is reading alive, so while a clip is being
//! written the process holds the ceiling *plus that clip*. It is bounded by the
//! clip's own length — a save of the whole window at most doubles the figure —
//! and it is not counted against the ceiling because evicting the buffer's
//! history to pay for a save would leave nothing for the next one
//! (`crate::buffer`).
//!
//! # Granularity
//!
//! The segment length is the granularity of a save. A segment begins on a
//! keyframe so that it can be decoded on its own, so the buffer cannot start a
//! segment anywhere the encoder did not put one: the real granularity is the
//! larger of [`ReplayConfig::segment_duration`] and the encoder's keyframe
//! interval. A save therefore keeps up to one segment of extra material before
//! the requested start and up to one after the requested end;
//! [`SegmentLease`](crate::SegmentLease) reports both as slack, and trimming
//! them off is [issue #37](https://github.com/wildware-uk/clipped/issues/37).
//!
//! Shorter segments cost compression — every keyframe is several times the size
//! of a predicted picture — and buy precision. Two seconds is the default here
//! because it is what `clipped_encoder::KeyframeInterval::DEFAULT` already
//! configures a recording with, and a buffer whose segments were shorter than
//! the keyframe interval would not produce shorter segments, only confusing
//! ones.

use core::fmt;
use core::time::Duration;

use clipped_encoder::BitRate;

use crate::error::ConfigError;

/// The shortest replay window that may be configured (SPEC.md section 16).
pub const MINIMUM_WINDOW: Duration = Duration::from_secs(30);

/// The longest replay window that may be configured (SPEC.md section 16).
///
/// Thirty minutes in memory is expensive — see `docs/replay-buffer.md` for the
/// measured figures — and spilling to disk beyond a threshold is
/// [issue #36](https://github.com/wildware-uk/clipped/issues/36).
pub const MAXIMUM_WINDOW: Duration = Duration::from_secs(30 * 60);

/// The default segment length, matching `clipped_encoder::KeyframeInterval`'s
/// own default.
pub const DEFAULT_SEGMENT: Duration = Duration::from_secs(2);

/// How much the default ceiling allows above what the window needs, as a
/// percentage.
///
/// A recording is configured for a constant bitrate and no encoder holds to one
/// exactly: a scene change costs more than the rate control allocated and is
/// paid back over the following second. Fifty per cent is generous for that
/// and, more importantly, is the difference between a ceiling that trims a
/// buffer's oldest seconds during a firefight and one that never bites in
/// ordinary use.
const CEILING_HEADROOM_PERCENT: u64 = 150;

/// What a buffer keeps and what it is allowed to spend keeping it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayConfig {
    window: Duration,
    segment: Duration,
    bitrate: BitRate,
    ceiling: Option<u64>,
}

impl ReplayConfig {
    /// A buffer holding `window` of video encoded at `bitrate`.
    ///
    /// The bitrate is asked for rather than discovered because it is what every
    /// number here is derived from, and the caller always knows it: it is the
    /// rate control the encoder session was opened with.
    ///
    /// # Errors
    ///
    /// [`ConfigError::WindowOutOfRange`] if `window` is outside
    /// [`MINIMUM_WINDOW`] to [`MAXIMUM_WINDOW`].
    pub fn new(window: Duration, bitrate: BitRate) -> Result<Self, ConfigError> {
        if window < MINIMUM_WINDOW || window > MAXIMUM_WINDOW {
            return Err(ConfigError::WindowOutOfRange {
                requested: window,
                minimum: MINIMUM_WINDOW,
                maximum: MAXIMUM_WINDOW,
            });
        }

        Ok(Self {
            window,
            segment: DEFAULT_SEGMENT,
            bitrate,
            ceiling: None,
        })
    }

    /// Keeps segments of `segment` rather than [`DEFAULT_SEGMENT`].
    ///
    /// This is a *target*: a segment is sealed at the first keyframe at or
    /// after it, so an encoder with a longer keyframe interval produces longer
    /// segments and asking for a shorter one changes nothing.
    ///
    /// # Errors
    ///
    /// [`ConfigError::SegmentOutOfRange`] if it is zero or longer than the
    /// window.
    pub fn with_segment_duration(self, segment: Duration) -> Result<Self, ConfigError> {
        if segment.is_zero() || segment > self.window {
            return Err(ConfigError::SegmentOutOfRange {
                requested: segment,
                window: self.window,
            });
        }

        Ok(Self { segment, ..self })
    }

    /// Holds the buffer to `bytes` rather than to the default ceiling.
    ///
    /// # Errors
    ///
    /// [`ConfigError::CeilingBelowWindow`] if the ceiling is below what the
    /// configured window needs at the configured bitrate. A ceiling that cannot
    /// hold the window is a window that will never be reached, and discovering
    /// that from a shorter clip than expected is worse than being told now.
    pub fn with_memory_ceiling(self, bytes: u64) -> Result<Self, ConfigError> {
        let needed = self.expected_bytes();
        if bytes < needed {
            return Err(ConfigError::CeilingBelowWindow {
                requested: bytes,
                needed,
            });
        }

        Ok(Self {
            ceiling: Some(bytes),
            ..self
        })
    }

    /// How much history the buffer keeps.
    #[must_use]
    pub const fn window(&self) -> Duration {
        self.window
    }

    /// The target length of one segment.
    #[must_use]
    pub const fn segment_duration(&self) -> Duration {
        self.segment
    }

    /// The bitrate the sizing is derived from.
    #[must_use]
    pub const fn bitrate(&self) -> BitRate {
        self.bitrate
    }

    /// What the buffer is expected to hold when it is full, in bytes.
    ///
    /// The window plus one segment. Retention drops the oldest segment only
    /// once the ones behind it still cover the window (`crate::buffer`), so a
    /// full buffer holds between `window` and `window + segment_duration` —
    /// the extra being however much of the oldest segment is not yet needed.
    #[must_use]
    pub fn expected_bytes(&self) -> u64 {
        bytes_for(self.bitrate, self.window.saturating_add(self.segment))
    }

    /// The most memory the buffer's own segments may occupy, in bytes.
    ///
    /// [`expected_bytes`](Self::expected_bytes) plus headroom for a bitrate
    /// that overshoots, unless
    /// [`with_memory_ceiling`](Self::with_memory_ceiling) set it explicitly.
    /// A save in progress holds its clip alive on top of this; the module
    /// documentation says why that term is outside the ceiling rather than
    /// inside it.
    #[must_use]
    pub fn memory_ceiling(&self) -> u64 {
        self.ceiling.unwrap_or_else(|| {
            self.expected_bytes()
                .saturating_mul(CEILING_HEADROOM_PERCENT)
                / 100
        })
    }

    /// What one segment is expected to hold, in bytes.
    ///
    /// Reserved when a segment opens, so that filling it is one allocation
    /// rather than a run of them on the capture thread.
    #[must_use]
    pub fn expected_segment_bytes(&self) -> u64 {
        bytes_for(self.bitrate, self.segment)
    }
}

impl fmt::Display for ReplayConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        #[allow(clippy::cast_precision_loss)]
        let ceiling = self.memory_ceiling() as f64 / (1024.0 * 1024.0);

        write!(
            formatter,
            "{:.0}s of {} in {:.0}s segments, at most {ceiling:.0} MiB",
            self.window.as_secs_f64(),
            self.bitrate,
            self.segment.as_secs_f64(),
        )
    }
}

/// How many bytes `bitrate` produces in `duration`.
fn bytes_for(bitrate: BitRate, duration: Duration) -> u64 {
    let bits = u64::from(bitrate.as_bits_per_second());
    // Nanoseconds rather than seconds so that a fractional duration is not
    // rounded to nothing, and `u128` because bits-times-nanoseconds overflows
    // `u64` for any realistic rate.
    let bits_total = u128::from(bits) * duration.as_nanos() / 1_000_000_000;
    u64::try_from(bits_total / 8).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `clipped-session` gives a 1080p60 recording:
    /// 1920 x 1080 x 60 x 0.15 bits per pixel per frame.
    fn rate_1080p60() -> BitRate {
        BitRate::bits_per_second(18_662_400).expect("a real rate")
    }

    #[test]
    fn a_window_shorter_than_thirty_seconds_is_refused() {
        let error = ReplayConfig::new(Duration::from_secs(29), rate_1080p60())
            .expect_err("SPEC.md section 16 starts at thirty seconds");

        assert!(
            matches!(error, ConfigError::WindowOutOfRange { .. }),
            "{error}"
        );
    }

    #[test]
    fn a_window_longer_than_thirty_minutes_is_refused() {
        let error = ReplayConfig::new(Duration::from_secs(30 * 60 + 1), rate_1080p60())
            .expect_err("SPEC.md section 16 stops at thirty minutes");

        assert!(
            matches!(error, ConfigError::WindowOutOfRange { .. }),
            "{error}"
        );
    }

    #[test]
    fn both_ends_of_the_supported_range_are_accepted() {
        assert!(ReplayConfig::new(MINIMUM_WINDOW, rate_1080p60()).is_ok());
        assert!(ReplayConfig::new(MAXIMUM_WINDOW, rate_1080p60()).is_ok());
    }

    #[test]
    fn the_expected_size_is_the_bitrate_times_the_window_and_one_segment() {
        // 18.66 Mbit/s for 32 seconds is 71.2 MiB. This is the arithmetic the
        // whole design rests on, so it is checked rather than described: the
        // same 32 seconds of raw 1080p60 BGRA frames would be 15.9 GB.
        let config = ReplayConfig::new(Duration::from_secs(30), rate_1080p60())
            .expect("thirty seconds is in range");

        assert_eq!(config.expected_bytes(), 74_649_600);
    }

    #[test]
    fn a_thirty_minute_window_is_sized_in_gigabytes() {
        // The argument for issue #36. Nothing here is wrong; it is simply what
        // thirty minutes of 18.66 Mbit/s video weighs.
        let config = ReplayConfig::new(MAXIMUM_WINDOW, rate_1080p60()).expect("in range");

        assert_eq!(config.expected_bytes(), 4_203_705_600);
        assert_eq!(config.memory_ceiling(), 6_305_558_400);
    }

    #[test]
    fn the_default_ceiling_leaves_room_for_a_bitrate_that_overshoots() {
        let config = ReplayConfig::new(Duration::from_secs(60), rate_1080p60()).expect("in range");

        assert_eq!(config.memory_ceiling(), config.expected_bytes() * 3 / 2);
    }

    #[test]
    fn a_ceiling_that_cannot_hold_the_window_is_refused_with_both_numbers() {
        let config = ReplayConfig::new(Duration::from_secs(300), rate_1080p60()).expect("in range");
        let error = config
            .with_memory_ceiling(64 * 1024 * 1024)
            .expect_err("64 MiB cannot hold five minutes of 18.66 Mbit/s video");

        match error {
            ConfigError::CeilingBelowWindow { requested, needed } => {
                assert_eq!(requested, 64 * 1024 * 1024);
                assert_eq!(needed, config.expected_bytes());
            }
            other => panic!("expected a ceiling refusal, got {other}"),
        }
    }

    #[test]
    fn a_segment_longer_than_the_window_is_refused() {
        let config = ReplayConfig::new(Duration::from_secs(30), rate_1080p60()).expect("in range");
        let error = config
            .with_segment_duration(Duration::from_secs(31))
            .expect_err("a buffer cannot be one segment that does not fit in it");

        assert!(
            matches!(error, ConfigError::SegmentOutOfRange { .. }),
            "{error}"
        );
    }

    #[test]
    fn a_segment_of_zero_is_refused() {
        let config = ReplayConfig::new(Duration::from_secs(30), rate_1080p60()).expect("in range");

        assert!(config.with_segment_duration(Duration::ZERO).is_err());
    }

    #[test]
    fn changing_the_segment_length_moves_the_default_ceiling_with_it() {
        // The ceiling is derived rather than stored, so a longer segment — a
        // longer keyframe interval — does not silently overrun a ceiling that
        // was computed for the old one.
        let two = ReplayConfig::new(Duration::from_secs(60), rate_1080p60()).expect("in range");
        let ten = two
            .with_segment_duration(Duration::from_secs(10))
            .expect("ten seconds fits in a minute");

        assert!(ten.memory_ceiling() > two.memory_ceiling());
    }

    #[test]
    fn an_explicit_ceiling_survives_being_read_back() {
        let config = ReplayConfig::new(Duration::from_secs(30), rate_1080p60())
            .expect("in range")
            .with_memory_ceiling(200 * 1024 * 1024)
            .expect("200 MiB holds thirty seconds");

        assert_eq!(config.memory_ceiling(), 200 * 1024 * 1024);
    }

    #[test]
    fn a_configuration_reads_as_what_it_will_cost() {
        let config = ReplayConfig::new(Duration::from_secs(30), rate_1080p60()).expect("in range");

        assert_eq!(
            config.to_string(),
            "30s of 18.7 Mbit/s in 2s segments, at most 107 MiB"
        );
    }
}
