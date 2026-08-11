//! The four durations that decide what the watcher costs and how fast it is.

use std::time::Duration;

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
    /// The `WITHIN` interval, in whole seconds, never below one.
    ///
    /// WQL takes the polling interval as a number of seconds. Sub-second values
    /// are legal and are exactly the "high-frequency polling" this watcher
    /// exists to avoid, so the floor is enforced here rather than trusted to
    /// the caller.
    #[must_use]
    pub(crate) fn notification_interval_seconds(self) -> u32 {
        let seconds = self.notification_interval.as_secs();
        u32::try_from(seconds).unwrap_or(u32::MAX).max(1)
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

        assert_eq!(config.notification_interval_seconds(), 1);
    }

    #[test]
    fn a_longer_interval_is_passed_through() {
        let config = WatchConfig {
            notification_interval: Duration::from_secs(5),
            ..WatchConfig::default()
        };

        assert_eq!(config.notification_interval_seconds(), 5);
    }
}
