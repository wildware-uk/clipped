//! What can go wrong when asking Windows about the desktop.

use core::fmt;
use std::error::Error;

use crate::{MonitorHandle, WindowHandle};

/// Why a query about the desktop failed.
///
/// The split is by what a caller can do about it. [`Self::WindowGone`] and
/// [`Self::MonitorGone`] mean the thing being asked about stopped existing,
/// which during a recording is news rather than a fault: a game closing is the
/// normal end of a session (AGENTS.md section 16). [`Self::Api`] means Windows
/// refused a call that should have worked, and carries the operation's name so
/// that the report says which one.
#[derive(Debug)]
pub enum WindowsError {
    /// The window no longer exists.
    ///
    /// A window handle is a weak reference: it is valid until the window is
    /// destroyed, and nothing warns the holder. Every query in this crate that
    /// takes one can return this, at any moment, and a caller mid-recording
    /// should treat it as "the recording is over" rather than as an error to
    /// retry.
    WindowGone {
        /// The handle that no longer names a window.
        handle: WindowHandle,
    },

    /// The monitor no longer exists, or its handle has been superseded.
    ///
    /// Windows invalidates every `HMONITOR` when displays are added, removed or
    /// rearranged, so a handle held across a display change is stale even
    /// though the physical monitor is still plugged in. Re-enumerate rather
    /// than treating it as hardware failure.
    MonitorGone {
        /// The handle that no longer names a monitor.
        handle: MonitorHandle,
    },

    /// The process cannot be opened, so nothing can be asked about it.
    ///
    /// Two situations spell the same way and neither is a fault. The process
    /// has exited — its identifier now names nothing, or something else
    /// entirely — or it runs at a higher integrity level than Clipped, which an
    /// unelevated application may not open at all. Anti-cheat services and
    /// most of Windows' own processes are the second case.
    ProcessUnavailable {
        /// The identifier that could not be opened.
        process_id: u32,
    },

    /// A Windows API call failed.
    Api {
        /// The function that failed, as it is spelled in the Windows SDK, so
        /// that a report can be searched for.
        operation: &'static str,
        /// What Windows said.
        source: windows::core::Error,
    },
}

impl WindowsError {
    /// Builds an [`Self::Api`] failure for `operation`.
    pub(crate) fn api(operation: &'static str, source: windows::core::Error) -> Self {
        Self::Api { operation, source }
    }
}

impl fmt::Display for WindowsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WindowGone { handle } => {
                write!(formatter, "the window {handle} has closed")
            }
            Self::MonitorGone { handle } => write!(
                formatter,
                "the monitor {handle} is no longer attached, or the displays have been \
                 rearranged since it was enumerated"
            ),
            Self::ProcessUnavailable { process_id } => write!(
                formatter,
                "process {process_id} cannot be opened: it has exited, or it runs at a higher \
                 integrity level than this application"
            ),
            Self::Api { operation, source } => {
                write!(formatter, "{operation} failed: {source}")
            }
        }
    }
}

impl Error for WindowsError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::WindowGone { .. }
            | Self::MonitorGone { .. }
            | Self::ProcessUnavailable { .. } => None,
            Self::Api { source, .. } => Some(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closed_window_reads_as_an_event_rather_than_a_fault() {
        let error = WindowsError::WindowGone {
            handle: WindowHandle::from_raw(0x1234),
        };
        assert_eq!(error.to_string(), "the window 0x00001234 has closed");
        assert!(error.source().is_none());
    }

    #[test]
    fn a_process_that_cannot_be_opened_says_which_and_offers_both_reasons() {
        let error = WindowsError::ProcessUnavailable { process_id: 4 };
        assert_eq!(
            error.to_string(),
            "process 4 cannot be opened: it has exited, or it runs at a higher integrity level \
             than this application"
        );
        assert!(error.source().is_none());
    }

    #[test]
    fn an_api_failure_names_the_function_that_failed() {
        let error = WindowsError::api("GetClientRect", windows::core::Error::empty());
        assert!(
            error.to_string().starts_with("GetClientRect failed:"),
            "unexpected message: {error}"
        );
        assert!(error.source().is_some(), "the cause should stay reachable");
    }
}
