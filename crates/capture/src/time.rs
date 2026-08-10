//! Capture timestamps, and the reason they cannot be read from a clock.

use core::fmt;
use core::num::NonZeroU64;
use core::time::Duration;

/// Which clock a [`CaptureTimestamp`] counts on.
///
/// A backend has to name one, and the only names available are monotonic
/// clocks that a *frame source* stamps frames with. There is deliberately no
/// variant for wall-clock time, so a backend that reaches for
/// `SystemTime::now()` has nowhere to declare the result and the mistake stops
/// at the type rather than at a desynchronised recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SourceClock {
    /// The Windows high-resolution performance counter.
    ///
    /// This is what both Windows capture APIs stamp frames with — Windows
    /// Graphics Capture through `Direct3D11CaptureFrame::SystemRelativeTime`,
    /// Desktop Duplication through `DXGI_OUTDUPL_FRAME_INFO::LastPresentTime`
    /// — and what Windows audio capture reports positions against, so video
    /// and audio timestamps can be compared without a conversion nobody can
    /// check.
    PerformanceCounter,
    /// A POSIX `CLOCK_MONOTONIC`-equivalent counter, for a future non-Windows
    /// backend. Nothing produces this today.
    Monotonic,
}

impl fmt::Display for SourceClock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::PerformanceCounter => "performance counter",
            Self::Monotonic => "monotonic",
        })
    }
}

/// When a frame was produced, according to whatever produced it.
///
/// # Why there is no `now()`
///
/// The obvious implementation of a capture timestamp — read the clock when the
/// frame arrives — is wrong, and wrong in a way that is invisible until someone
/// watches the recording. The moment a frame reaches this process is the moment
/// a compositor, a driver, a thread scheduler and an encoder queue have all
/// finished with it, and the delay each adds varies frame to frame and grows
/// under load. Timestamps taken at receipt therefore encode this recorder's
/// jitter rather than the game's frame pacing: the video drifts against audio
/// captured with its own device-clock positions, and the drift is worst exactly
/// when the machine is busiest, which is during a game.
///
/// So this type cannot be built from a clock. [`from_source`](Self::from_source)
/// and [`from_performance_counter`](Self::from_performance_counter) are the only
/// constructors, both take a value the frame arrived with, and both make the
/// backend name the [`SourceClock`] it came from. Nothing here can stop a
/// determined backend from calling `QueryPerformanceCounter` itself and passing
/// the result — the type system cannot know where a number came from — but the
/// accidental path is closed, and a review has one line to look at.
///
/// # Comparing
///
/// Two timestamps are comparable only if they name the same [`SourceClock`];
/// [`duration_since`](Self::duration_since) returns [`None`] otherwise rather
/// than subtracting two unrelated counters. Timestamps are stored in
/// nanoseconds so that a comparison never depends on a counter frequency the
/// caller does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CaptureTimestamp {
    clock: SourceClock,
    nanos: u64,
}

impl CaptureTimestamp {
    /// Takes the timestamp a frame arrived with, in nanoseconds on `clock`.
    ///
    /// `nanos` must be the value the capture API attached to this frame. It is
    /// not "close enough" to read a clock here: see the type documentation.
    #[must_use]
    pub const fn from_source(clock: SourceClock, nanos: u64) -> Self {
        Self { clock, nanos }
    }

    /// Converts a raw performance-counter reading into a timestamp.
    ///
    /// `ticks` is the counter value the frame arrived with and `frequency` is
    /// the counter's ticks per second, from `QueryPerformanceFrequency` — fixed
    /// for the lifetime of the system, so a backend reads it once at
    /// initialisation.
    ///
    /// The multiplication is done in 128-bit arithmetic because
    /// `ticks * 1_000_000_000` overflows a `u64` after about six seconds at a
    /// 10 MHz counter, and a performance counter counts from boot, not from
    /// when the recording started.
    #[must_use]
    pub const fn from_performance_counter(ticks: u64, frequency: NonZeroU64) -> Self {
        let nanos = (ticks as u128 * 1_000_000_000) / frequency.get() as u128;
        Self {
            clock: SourceClock::PerformanceCounter,
            // A u128 nanosecond count only exceeds u64 after ~584 years of
            // uptime, so this cannot truncate in practice; saturating rather
            // than wrapping keeps ordering sane if it somehow did.
            nanos: if nanos > u64::MAX as u128 {
                u64::MAX
            } else {
                nanos as u64
            },
        }
    }

    /// Which clock this counts on.
    #[must_use]
    pub const fn clock(&self) -> SourceClock {
        self.clock
    }

    /// The reading, in nanoseconds on [`clock`](Self::clock).
    ///
    /// The zero point is the clock's own, which for a performance counter is
    /// system boot. Only differences mean anything.
    #[must_use]
    pub const fn as_nanos(&self) -> u64 {
        self.nanos
    }

    /// How much later this frame is than `earlier`.
    ///
    /// [`None`] when the two name different clocks, or when `earlier` is
    /// actually later — a source that reports timestamps going backwards is a
    /// fault to report, not a negative duration to average away.
    #[must_use]
    pub fn duration_since(&self, earlier: Self) -> Option<Duration> {
        if self.clock != earlier.clock {
            return None;
        }
        self.nanos
            .checked_sub(earlier.nanos)
            .map(Duration::from_nanos)
    }
}

impl fmt::Display for CaptureTimestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}ns ({})", self.nanos, self.clock)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The performance-counter frequency Windows reports on every machine
    /// since Windows 10: 10 MHz, so one tick is 100 nanoseconds.
    fn ten_mhz() -> NonZeroU64 {
        NonZeroU64::new(10_000_000).expect("10 MHz is not zero")
    }

    #[test]
    fn performance_counter_ticks_convert_to_nanoseconds() {
        let timestamp = CaptureTimestamp::from_performance_counter(1, ten_mhz());
        assert_eq!(timestamp.as_nanos(), 100);
        assert_eq!(timestamp.clock(), SourceClock::PerformanceCounter);
    }

    #[test]
    fn conversion_does_not_overflow_at_realistic_uptimes() {
        // Ten days of uptime at 10 MHz. `ticks * 1_000_000_000` is about
        // 8.6e21, which does not fit in a u64: computing this in 64 bits
        // silently produces nonsense, and every frame timestamp in a session
        // started on a machine that has been up for a while would be wrong.
        let ticks = 10 * 24 * 60 * 60 * 10_000_000;
        let timestamp = CaptureTimestamp::from_performance_counter(ticks, ten_mhz());
        assert_eq!(timestamp.as_nanos(), 864_000_000_000_000);
    }

    #[test]
    fn frame_intervals_come_out_of_the_difference() {
        // Two frames one 60 Hz interval apart on a 10 MHz counter.
        let first = CaptureTimestamp::from_performance_counter(1_000_000, ten_mhz());
        let second = CaptureTimestamp::from_performance_counter(1_166_666, ten_mhz());
        let interval = second
            .duration_since(first)
            .expect("both readings are on the same clock");
        assert_eq!(interval, Duration::from_nanos(16_666_600));
    }

    #[test]
    fn timestamps_on_different_clocks_are_not_comparable() {
        let counter = CaptureTimestamp::from_source(SourceClock::PerformanceCounter, 2_000);
        let monotonic = CaptureTimestamp::from_source(SourceClock::Monotonic, 1_000);
        assert_eq!(
            counter.duration_since(monotonic),
            None,
            "subtracting readings from two unrelated clocks must not produce a duration"
        );
    }

    #[test]
    fn a_timestamp_going_backwards_is_reported_rather_than_hidden() {
        let earlier = CaptureTimestamp::from_source(SourceClock::PerformanceCounter, 1_000);
        let later = CaptureTimestamp::from_source(SourceClock::PerformanceCounter, 2_000);
        assert_eq!(earlier.duration_since(later), None);
        assert_eq!(
            later.duration_since(earlier),
            Some(Duration::from_micros(1))
        );
    }
}
