//! The four durations that decide what the watcher costs and how fast it is.

use std::time::Duration;

/// The shortest interval any source will look at, however it is configured.
///
/// Below this the value is not honoured meaningfully by WMI's own scheduling,
/// and for the poller it is exactly the high-frequency process enumeration this
/// watcher exists to avoid (AGENTS.md section 18). Both sources ask
/// [`WatchConfig::source_interval`] rather than reading the field, so the floor
/// holds on both paths rather than only the one that happens to round to whole
/// seconds.
const MINIMUM_NOTIFICATION_INTERVAL: Duration = Duration::from_secs(1);

/// How the watcher trades latency against cost and against noise.
///
/// Every field is a duration, and every one of them is a decision somebody will
/// want to revisit with a real game in front of them, so they are settings
/// rather than constants (AGENTS.md section 30). [`Default`] is the answer for
/// a machine that is also running a game; the tests construct their own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WatchConfig {
    /// How often the event source is willing to look.
    ///
    /// This is the `WITHIN` clause of the WMI subscription, and the poll period
    /// of the fallback. It is the floor under detection latency: nothing can be
    /// noticed sooner than the interval it is looked for in.
    ///
    /// It is also the setting that decides what the watcher costs, and the
    /// relationship is close to inverse: the WMI service compares the whole
    /// process table with itself once per interval per subscription, so halving
    /// the interval roughly doubles the work. `docs/game-detection.md` has the
    /// measurements, and they are the reason the default is not one second.
    ///
    /// One second is the floor: below it the value is not honoured meaningfully
    /// by WMI's own scheduling, and it is exactly the high-frequency polling
    /// this watcher exists to avoid.
    pub notification_interval: Duration,

    /// How long a launch must stay quiet before it is reported.
    ///
    /// A game launch is not one process. Steam starts a launcher, the launcher
    /// starts the game, an anti-cheat wrapper may sit between them, and some
    /// titles re-execute themselves once. Reporting each of those as a separate
    /// launch would have the session layer start and abandon three recordings.
    ///
    /// So a launch is collected until nothing related to it has started for
    /// this long, and then reported once. It is deliberately longer than
    /// [`Self::notification_interval`]: with a source that looks once a second,
    /// a parent and its child can easily land in *different* batches, and a
    /// quiet period shorter than the interval could not span that gap.
    pub launch_quiet_period: Duration,

    /// The longest a launch may be collected for, however busy it is.
    ///
    /// Without this, a process that spawns a helper every second — a launcher
    /// that keeps a watchdog running, a browser-based storefront — would defer
    /// its launch for as long as it kept doing so, and the recording would
    /// never start. The cap is measured from the first member, so a launch is
    /// always reported eventually.
    pub max_launch_window: Duration,

    /// How long exits are gathered before they are reported.
    ///
    /// A source that looks once a second delivers a parent and its child dying
    /// together in one batch, in whatever order the provider chose. Gathering
    /// them briefly is what lets the watcher report them child-first, so a
    /// consumer tearing a session down sees the leaf go before its parent
    /// rather than in an order that changes between runs.
    pub exit_settle_period: Duration,
}

impl WatchConfig {
    /// How often a source may actually look, never more often than once a
    /// second.
    ///
    /// [`Self::notification_interval`] is a public field on a public type, so
    /// the value that arrives here is whatever a caller wrote — and a caller
    /// asking for fifty milliseconds would otherwise get a fifty-millisecond
    /// enumeration of every process on the machine out of the fallback poller.
    /// The floor is enforced here rather than trusted to the caller, and *both*
    /// sources go through this method to get it.
    #[must_use]
    pub(crate) fn source_interval(self) -> Duration {
        self.notification_interval
            .max(MINIMUM_NOTIFICATION_INTERVAL)
    }

    /// The `WITHIN` interval, in whole seconds, never below one.
    ///
    /// WQL takes the polling interval as a number of seconds, so this is
    /// [`Self::source_interval`] truncated; the floor is already applied there,
    /// which is why nothing clamps again here.
    #[must_use]
    pub(crate) fn notification_interval_seconds(self) -> u32 {
        let seconds = self.source_interval().as_secs();
        u32::try_from(seconds).unwrap_or(u32::MAX)
    }
}

impl Default for WatchConfig {
    /// The settings a recorder runs with, chosen from measurements.
    ///
    /// Worst-case latency from a process starting to a launch being reported is
    /// `notification_interval + launch_quiet_period`, so these values cost up
    /// to four and a half seconds, and about three and a half in practice. That
    /// is cheap against the ten to sixty seconds a game takes to reach anything
    /// worth recording, and it is deliberately bought: at a one-second interval
    /// the same watcher detects a launch a second sooner and costs the machine
    /// twice as much for every second it is not detecting anything.
    /// `docs/game-detection.md` records both measurements.
    fn default() -> Self {
        Self {
            notification_interval: Duration::from_secs(2),
            launch_quiet_period: Duration::from_millis(2_500),
            max_launch_window: Duration::from_secs(15),
            exit_settle_period: Duration::from_millis(500),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_quiet_period_outlasts_the_interval_it_watches() {
        let config = WatchConfig::default();

        // Not a style preference: a parent and its child can be reported in
        // consecutive batches, and a quiet period shorter than one batch could
        // not hold a launch open long enough to join them.
        assert!(
            config.launch_quiet_period > config.notification_interval,
            "a launch could not span two notification batches"
        );
        assert!(config.max_launch_window > config.launch_quiet_period);
    }

    #[test]
    fn the_polling_interval_never_drops_below_a_second() {
        let config = WatchConfig {
            notification_interval: Duration::from_millis(20),
            ..WatchConfig::default()
        };

        // Both halves of the floor: the number WQL is given, and the duration
        // the fallback poller sleeps for. A clamp on only one of them leaves a
        // caller who writes 20ms with a 20ms enumeration of the process table.
        assert_eq!(config.notification_interval_seconds(), 1);
        assert_eq!(config.source_interval(), Duration::from_secs(1));
    }

    #[test]
    fn a_longer_interval_is_passed_through() {
        let config = WatchConfig {
            notification_interval: Duration::from_secs(5),
            ..WatchConfig::default()
        };

        assert_eq!(config.notification_interval_seconds(), 5);
        assert_eq!(config.source_interval(), Duration::from_secs(5));
    }

    #[test]
    fn a_fractional_interval_above_the_floor_keeps_its_fraction() {
        let config = WatchConfig {
            notification_interval: Duration::from_millis(2_500),
            ..WatchConfig::default()
        };

        // The floor is a floor, not a rounding rule: the poller can sleep for
        // two and a half seconds even though WQL can only be told about two.
        assert_eq!(config.source_interval(), Duration::from_millis(2_500));
        assert_eq!(config.notification_interval_seconds(), 2);
    }
}
