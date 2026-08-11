//! The durations and bounds automatic recording is decided by.
//!
//! Every one of them is a judgement somebody will want to revisit with a real
//! game in front of them, so they are settings rather than constants (AGENTS.md
//! section 30), and every default below is explained by the failure it exists
//! to avoid rather than by taste.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long after a game exits its session stays open for the same game to come
/// back.
///
/// A player who alt-F4s and relaunches has had one sitting, and the two files
/// they produced belong to it. A minute is comfortably longer than the
/// alt-F4-and-back-in case and comfortably shorter than a break, and the
/// consequence of being wrong either way is one grouping decision rather than a
/// lost recording.
pub const DEFAULT_RESTART_GRACE: Duration = Duration::from_secs(60);

/// How large a jump in the wall clock is read as the machine having slept.
///
/// The manager is driven by a loop that calls it at least once a second, so any
/// gap of this size is either a suspend or a machine so overloaded that it may
/// as well have been one. It is deliberately far above the second the loop
/// promises and far below the length of any real break.
pub const DEFAULT_SUSPEND_GAP: Duration = Duration::from_secs(90);

/// How long the manager waits before recording the same game again after one of
/// its recordings ended by itself.
///
/// This is not politeness, it is a race. A recording ends the instant the
/// game's window goes, and the *process* exiting reaches the manager through
/// the watcher up to `notification_interval + exit_settle_period` later — three
/// seconds with the shipped `WatchConfig`
/// (`clipped_game_detection::WatchConfig`). Restarting sooner than that would
/// have the manager go looking for the window of a game it does not yet know
/// has exited, on every ordinary quit.
pub const DEFAULT_RECORDING_RESTART_DELAY: Duration = Duration::from_secs(5);

/// How many recordings one session may contain.
///
/// A loop guard, not a product limit. The manager records the game again
/// whenever a recording ends while the game is still running, which is what
/// carries a session across a window being destroyed and recreated; a game that
/// somehow ends a recording immediately every time would otherwise spin for as
/// long as it ran. A hundred is far above any real session and far below a
/// number that would fill a disk unnoticed.
pub const DEFAULT_MAX_RECORDINGS_PER_SESSION: u32 = 100;

/// What automatic recording was told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomaticSettings {
    directory: PathBuf,
    restart_grace: Duration,
    suspend_gap: Duration,
    recording_restart_delay: Duration,
    max_recordings_per_session: u32,
}

impl AutomaticSettings {
    /// Settings that write into `directory`, with every default above.
    #[must_use]
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            restart_grace: DEFAULT_RESTART_GRACE,
            suspend_gap: DEFAULT_SUSPEND_GAP,
            recording_restart_delay: DEFAULT_RECORDING_RESTART_DELAY,
            max_recordings_per_session: DEFAULT_MAX_RECORDINGS_PER_SESSION,
        }
    }

    /// Sets how long a session stays open for the same game to come back.
    #[must_use]
    pub const fn with_restart_grace(mut self, grace: Duration) -> Self {
        self.restart_grace = grace;
        self
    }

    /// Sets how large a wall-clock jump is read as a suspend.
    #[must_use]
    pub const fn with_suspend_gap(mut self, gap: Duration) -> Self {
        self.suspend_gap = gap;
        self
    }

    /// Sets how long the manager waits before recording the same game again.
    #[must_use]
    pub const fn with_recording_restart_delay(mut self, delay: Duration) -> Self {
        self.recording_restart_delay = delay;
        self
    }

    /// Sets the cap on recordings in one session.
    #[must_use]
    pub const fn with_max_recordings_per_session(mut self, maximum: u32) -> Self {
        self.max_recordings_per_session = maximum;
        self
    }

    /// Where recordings and session sidecars are written.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// How long a session stays open for the same game to come back.
    #[must_use]
    pub const fn restart_grace(&self) -> Duration {
        self.restart_grace
    }

    /// How large a wall-clock jump is read as a suspend.
    #[must_use]
    pub const fn suspend_gap(&self) -> Duration {
        self.suspend_gap
    }

    /// How long the manager waits before recording the same game again.
    #[must_use]
    pub const fn recording_restart_delay(&self) -> Duration {
        self.recording_restart_delay
    }

    /// How many recordings one session may contain.
    #[must_use]
    pub const fn max_recordings_per_session(&self) -> u32 {
        self.max_recordings_per_session
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_restart_delay_outlasts_the_watchers_worst_case_exit_latency() {
        // Not a style preference. A recording ends when the window goes; the
        // process exiting reaches the manager up to
        // `notification_interval + exit_settle_period` later, and a shorter
        // delay would restart a recording of a game that has already quit on
        // every ordinary exit.
        let watcher = clipped_game_detection::WatchConfig::default();
        let worst_case = watcher.notification_interval + watcher.exit_settle_period;

        assert!(
            DEFAULT_RECORDING_RESTART_DELAY > worst_case,
            "the restart delay is {DEFAULT_RECORDING_RESTART_DELAY:?} and an exit can take \
             {worst_case:?} to arrive"
        );
    }

    #[test]
    fn the_suspend_gap_is_far_longer_than_the_loop_that_drives_the_manager() {
        // The manager is polled at least once a second, so anything near that
        // is not a suspend. A gap threshold below the restart grace would also
        // make an ordinary quiet minute look like a resume.
        assert!(DEFAULT_SUSPEND_GAP > DEFAULT_RESTART_GRACE);
        assert!(DEFAULT_SUSPEND_GAP > Duration::from_secs(30));
    }

    #[test]
    fn every_setting_is_carried_through_to_the_getters() {
        // A setting that is accepted and then ignored is a control that
        // silently does nothing (AGENTS.md section 27), and this mapping is
        // the only place one could be lost.
        let settings = AutomaticSettings::new(PathBuf::from(r"D:\clips"))
            .with_restart_grace(Duration::from_secs(7))
            .with_suspend_gap(Duration::from_secs(11))
            .with_recording_restart_delay(Duration::from_secs(13))
            .with_max_recordings_per_session(17);

        assert_eq!(settings.directory(), Path::new(r"D:\clips"));
        assert_eq!(settings.restart_grace(), Duration::from_secs(7));
        assert_eq!(settings.suspend_gap(), Duration::from_secs(11));
        assert_eq!(settings.recording_restart_delay(), Duration::from_secs(13));
        assert_eq!(settings.max_recordings_per_session(), 17);
    }
}
