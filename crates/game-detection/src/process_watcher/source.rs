//! Where process events come from, and what they look like before debouncing.

use core::fmt;

use super::error::SourceError;
use super::process::ProcessSnapshot;

/// Which mechanism is delivering process events.
///
/// The choice is made once when the watcher starts and reported, rather than
/// left implicit, because the two have materially different latency and cost
/// and a support report that does not say which one was running cannot be
/// interpreted. `docs/game-detection.md` gives the measurements for both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventSource {
    /// A WMI notification subscription over `Win32_Process`.
    ///
    /// The preferred source: this process blocks on a call and is woken when
    /// something happens, so it does not poll at all. The WMI service does,
    /// once per `WITHIN` interval, which is where the cost and the latency
    /// actually live.
    WmiNotification,

    /// A periodic `CreateToolhelp32Snapshot` diff — the documented fallback.
    ///
    /// Used when WMI cannot be reached: the service is stopped, its repository
    /// is corrupt, or policy forbids the connection. It polls, which is what
    /// the design is trying to avoid, but a low-frequency poll that works is
    /// better than a subscription that does not exist, and it is the only
    /// mechanism left that needs neither WMI nor elevation.
    SnapshotPolling,
}

impl EventSource {
    /// The name used in logs, as a closed vocabulary rather than free text.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WmiNotification => "wmi_notification",
            Self::SnapshotPolling => "snapshot_polling",
        }
    }
}

impl fmt::Display for EventSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A process event exactly as the platform reported it, before debouncing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SourceEvent {
    /// A process appeared.
    Started(ProcessSnapshot),

    /// A process disappeared.
    ///
    /// Only the identifier: whatever else was true of the process, it is no
    /// longer there to ask, and the watcher remembers the rest itself.
    Exited { pid: u32 },
}

/// What a source thread sends back to the watcher.
#[derive(Debug)]
pub(crate) enum SourceMessage {
    /// Something happened to a process.
    Event(SourceEvent),

    /// This source has stopped and will send nothing further.
    ///
    /// The WMI service can be restarted, and its repository can be rebuilt,
    /// underneath a subscription that was working perfectly. Recovery is
    /// explicit rather than hoped for (AGENTS.md section 16): the watcher
    /// starts the fallback and says so.
    Lost(SourceError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_source_has_a_distinct_log_name() {
        assert_eq!(EventSource::WmiNotification.as_str(), "wmi_notification");
        assert_eq!(EventSource::SnapshotPolling.as_str(), "snapshot_polling");
        assert_eq!(
            EventSource::SnapshotPolling.to_string(),
            EventSource::SnapshotPolling.as_str()
        );
    }
}
