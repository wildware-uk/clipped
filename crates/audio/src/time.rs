//! Audio timestamps, and the clock they are obliged to come from.

use core::fmt;
use core::num::NonZeroU64;
use core::time::Duration;

/// When a block of audio was heard, according to the audio device.
///
/// # Why there is no `now()`
///
/// The same reason `clipped_capture::CaptureTimestamp` has none. The moment a
/// buffer reaches this process is the moment the audio engine, a driver and a
/// thread scheduler have all finished with it, and the delay each adds varies
/// from packet to packet and grows under load. A timestamp taken at receipt
/// therefore encodes this recorder's jitter rather than the endpoint's sample
/// clock, and audio stamped that way drifts against video stamped from the
/// compositor's clock — worst exactly when the machine is busiest, which is
/// during a game.
///
/// So the only constructors take a value the audio stack produced:
/// [`from_hundred_nanos`](Self::from_hundred_nanos) for the QPC position that
/// arrives with every captured packet, and
/// [`from_performance_counter`](Self::from_performance_counter) for a raw
/// counter reading. There is one place in this crate that reads the counter
/// itself — synthesising silence for a period the device said nothing about,
/// where there is no device reading to use — and it is documented at the call
/// site.
///
/// # Why this is not `clipped_capture::CaptureTimestamp`
///
/// It is the same clock, the same units and very nearly the same type, and one
/// of them would be better than two. `clipped-capture` and `clipped-audio` are
/// both layer 1 in the dependency table in README.md, so neither may depend on
/// the other, and there is no shared vocabulary crate below them to put a
/// timestamp in. Inventing one for a single type would be a larger change than
/// this issue, so the duplication is deliberate and bounded: both count
/// nanoseconds on the Windows performance counter, both are produced only from
/// a value their source supplied, and
/// [`as_nanos`](Self::as_nanos) on either can be compared with the other
/// directly. Merging them is work for A/V synchronisation
/// ([issue #22](https://github.com/wildware-uk/clipped/issues/22)), which is
/// the first code that holds both at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AudioTimestamp {
    nanos: u64,
}

impl AudioTimestamp {
    /// Takes a position in 100-nanosecond units, which is what WASAPI reports.
    ///
    /// `IAudioCaptureClient::GetBuffer` hands its QPC position back already
    /// converted to 100-nanosecond units, so there is no counter frequency to
    /// read here. The value it counts is still a performance-counter reading,
    /// which is what makes the result comparable with a video frame's
    /// timestamp.
    #[must_use]
    pub const fn from_hundred_nanos(ticks: u64) -> Self {
        Self {
            nanos: ticks.saturating_mul(100),
        }
    }

    /// Converts a raw performance-counter reading into a timestamp.
    ///
    /// `ticks` is a `QueryPerformanceCounter` value and `frequency` is
    /// `QueryPerformanceFrequency`, which is fixed for the lifetime of the
    /// system and so is read once.
    ///
    /// The multiplication is done in 128-bit arithmetic because
    /// `ticks * 1_000_000_000` overflows a `u64` after about six seconds at a
    /// 10 MHz counter, and the counter counts from boot rather than from when
    /// the recording started.
    #[must_use]
    pub const fn from_performance_counter(ticks: u64, frequency: NonZeroU64) -> Self {
        let nanos = (ticks as u128 * 1_000_000_000) / frequency.get() as u128;
        Self {
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

    /// Takes a reading already expressed in nanoseconds on the same clock.
    #[must_use]
    pub const fn from_nanos(nanos: u64) -> Self {
        Self { nanos }
    }

    /// The reading, in nanoseconds.
    ///
    /// The zero point is the performance counter's own, which is system boot,
    /// so only differences mean anything.
    #[must_use]
    pub const fn as_nanos(&self) -> u64 {
        self.nanos
    }

    /// How much later this is than `earlier`.
    ///
    /// [`None`] when `earlier` is actually later. A source reporting positions
    /// that go backwards is a fault to report, not a negative duration to
    /// average away.
    #[must_use]
    pub fn duration_since(&self, earlier: Self) -> Option<Duration> {
        self.nanos
            .checked_sub(earlier.nanos)
            .map(Duration::from_nanos)
    }
}

impl fmt::Display for AudioTimestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}ns (performance counter)", self.nanos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The performance-counter frequency Windows reports on every machine
    /// since Windows 10: 10 MHz, so one tick is 100 nanoseconds — the same unit
    /// WASAPI reports positions in.
    const TEN_MHZ: u64 = 10_000_000;

    fn ten_mhz() -> NonZeroU64 {
        NonZeroU64::new(TEN_MHZ).expect("10 MHz is not zero")
    }

    #[test]
    fn a_wasapi_position_and_a_counter_reading_land_on_the_same_number() {
        // The property A/V synchronisation depends on: the position WASAPI
        // attaches to a packet and a reading this process takes itself are the
        // same clock in the same units, so one can be subtracted from the
        // other. Ten seconds of uptime, expressed both ways.
        let ticks = 10 * TEN_MHZ;
        assert_eq!(
            AudioTimestamp::from_hundred_nanos(ticks),
            AudioTimestamp::from_performance_counter(ticks, ten_mhz())
        );
        assert_eq!(
            AudioTimestamp::from_hundred_nanos(ticks).as_nanos(),
            10_000_000_000
        );
    }

    #[test]
    fn conversion_does_not_overflow_at_realistic_uptimes() {
        // Ten days at 10 MHz. `ticks * 1_000_000_000` is about 8.6e21, which
        // does not fit in a u64: computing this in 64 bits silently produces
        // nonsense, and every timestamp in a session started on a machine that
        // has been up for a while would be wrong.
        let ticks = 10 * 24 * 60 * 60 * TEN_MHZ;
        assert_eq!(
            AudioTimestamp::from_performance_counter(ticks, ten_mhz()).as_nanos(),
            864_000_000_000_000
        );
    }

    #[test]
    fn a_position_going_backwards_is_reported_rather_than_hidden() {
        let earlier = AudioTimestamp::from_nanos(1_000);
        let later = AudioTimestamp::from_nanos(2_000);
        assert_eq!(earlier.duration_since(later), None);
        assert_eq!(
            later.duration_since(earlier),
            Some(Duration::from_micros(1))
        );
    }
}
