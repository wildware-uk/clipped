//! What can go wrong when watching for processes.

use core::fmt;
use std::error::Error;

/// A platform call the watcher depends on failed.
///
/// Deliberately not a Windows type. The watcher's public surface stays platform
/// neutral so that the debounce and the matching above it can be compiled and
/// tested anywhere, and so that a caller rendering this on a diagnostics screen
/// does not have to link the Windows crate to do it. `operation` is the call as
/// it is spelled in the SDK, so a report can be searched for it; `detail` is
/// what the platform said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceError {
    /// The call or step that failed, spelled as the Windows SDK spells it.
    pub operation: &'static str,

    /// What the platform said about it.
    pub detail: String,
}

impl SourceError {
    /// Builds a failure report for `operation`.
    pub(crate) fn new(operation: &'static str, detail: impl fmt::Display) -> Self {
        Self {
            operation,
            detail: detail.to_string(),
        }
    }
}

impl fmt::Display for SourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} failed: {}", self.operation, self.detail)
    }
}

impl Error for SourceError {}

/// Why a [`ProcessWatcher`](super::ProcessWatcher) could not be started.
///
/// Both variants mean the same thing to a caller — no process events will
/// arrive — and they are separate because the answer differs. A baseline that
/// cannot be read is a machine in trouble; a source that cannot be established
/// carries *two* failures, because the fallback was tried as well, and a report
/// that named only one of them would send somebody looking in the wrong place.
#[derive(Debug)]
#[non_exhaustive]
pub enum WatchError {
    /// The process table could not be read, so there is no starting point.
    ///
    /// The watcher needs one snapshot before it can begin: without it, an exit
    /// reported for a process that was already running has nothing to name.
    Baseline(SourceError),

    /// Neither the event subscription nor the fallback poller could be started.
    NoSource {
        /// Why the WMI notification subscription could not be established.
        subscription: SourceError,
        /// Why the snapshot poller could not be started either.
        fallback: SourceError,
    },
}

impl fmt::Display for WatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Baseline(source) => {
                write!(
                    formatter,
                    "the running processes could not be listed: {source}"
                )
            }
            Self::NoSource {
                subscription,
                fallback,
            } => write!(
                formatter,
                "no process event source could be started: the WMI subscription ({subscription}), \
                 and the snapshot fallback ({fallback})"
            ),
        }
    }
}

impl Error for WatchError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Baseline(source) => Some(source),
            // Neither of the two is "the" cause, and reporting one of them
            // would hide the other. `Display` names both.
            Self::NoSource { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_failure_names_the_call_and_the_reason() {
        let error = SourceError::new("ExecNotificationQuery", "0x80041003");

        assert_eq!(
            error.to_string(),
            "ExecNotificationQuery failed: 0x80041003"
        );
    }

    #[test]
    fn a_failure_to_start_names_both_attempts() {
        let error = WatchError::NoSource {
            subscription: SourceError::new("ConnectServer", "the service is not running"),
            fallback: SourceError::new("CreateToolhelp32Snapshot", "access denied"),
        };

        let rendered = error.to_string();
        assert!(rendered.contains("ConnectServer"), "{rendered}");
        assert!(rendered.contains("CreateToolhelp32Snapshot"), "{rendered}");
    }
}
