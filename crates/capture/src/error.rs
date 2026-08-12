//! What a backend reports when capture goes wrong.

use core::fmt;
use std::error::Error;

use crate::{CaptureMethod, TargetKind};

/// A capture operation failed.
///
/// Every variant names the [`CaptureMethod`] that produced it, because the
/// first question asked of a capture failure is always "which backend?", and by
/// the time the error reaches a log or a support bundle the caller that knew is
/// several layers away.
///
/// The variants are split by *what the caller can do*, not by which Windows API
/// returned which `HRESULT`:
///
/// - [`NotInitialised`](Self::NotInitialised) is a programming error in the
///   caller.
/// - [`UnsupportedTarget`](Self::UnsupportedTarget) means try a different
///   backend, and ideally means [`select`](crate::select) should have known
///   better.
/// - [`TargetLost`](Self::TargetLost) means the recording is over.
/// - [`Interrupted`](Self::Interrupted) means reinitialise this backend and
///   carry on.
/// - [`Backend`](Self::Backend) is everything a backend could not classify.
///
/// That split is what automatic fallback reads: [`response_to`](crate::response_to)
/// turns these variants into "the recording is over", "restart this backend" or
/// "try the next one", and [`CaptureFallback`](crate::CaptureFallback) acts on
/// the answer ([issue #97](https://github.com/wildware-uk/clipped/issues/97)).
/// A new variant here therefore has to decide which of those three it is, and
/// the exhaustive `match` in `response_to` will not compile until it has.
#[derive(Debug)]
#[non_exhaustive]
pub enum CaptureError {
    /// A frame was requested before
    /// [`initialise`](crate::CaptureBackend::initialise) succeeded, or after
    /// [`shut_down`](crate::CaptureBackend::shut_down).
    NotInitialised {
        /// The backend that was asked.
        method: CaptureMethod,
    },
    /// [`initialise`](crate::CaptureBackend::initialise) was called on a
    /// backend that is already running.
    AlreadyInitialised {
        /// The backend that was asked.
        method: CaptureMethod,
    },
    /// This backend cannot capture this target.
    ///
    /// [`select`](crate::select) exists to make this unreachable: a backend
    /// should decline through
    /// [`BackendDeclaration::availability`](crate::BackendDeclaration::availability)
    /// before anything touches the GPU. Seeing it means a declaration is
    /// lying, or a caller bypassed selection.
    UnsupportedTarget {
        /// The backend that refused.
        method: CaptureMethod,
        /// What it was pointed at.
        target: TargetKind,
        /// Why it cannot, phrased for a diagnostics screen.
        reason: &'static str,
    },
    /// The window closed, or the display was disconnected.
    ///
    /// There is nothing left to capture, and no backend would do better. The
    /// session ends and the recording is finalised (AGENTS.md section 17).
    TargetLost {
        /// The backend that noticed.
        method: CaptureMethod,
    },
    /// Capture stopped for a reason that reinitialising this same backend is
    /// expected to fix.
    ///
    /// A GPU driver reset, a display mode change, or Desktop Duplication's
    /// `DXGI_ERROR_ACCESS_LOST` when a full-screen application takes the
    /// output. The target still exists.
    Interrupted {
        /// The backend that was interrupted.
        method: CaptureMethod,
        /// What happened, phrased for a diagnostics screen.
        reason: &'static str,
    },
    /// The platform API failed in a way this backend could not classify.
    Backend {
        /// The backend that failed.
        method: CaptureMethod,
        /// What it was doing, phrased so the message reads as a sentence:
        /// `"starting the Windows Graphics Capture frame pool"`.
        operation: &'static str,
        /// The platform error underneath.
        source: Box<dyn Error + Send + Sync>,
    },
}

impl CaptureError {
    /// Which backend produced this error.
    #[must_use]
    pub const fn method(&self) -> CaptureMethod {
        match self {
            Self::NotInitialised { method }
            | Self::AlreadyInitialised { method }
            | Self::UnsupportedTarget { method, .. }
            | Self::TargetLost { method }
            | Self::Interrupted { method, .. }
            | Self::Backend { method, .. } => *method,
        }
    }
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotInitialised { method } => {
                write!(f, "{method} has not been initialised")
            }
            Self::AlreadyInitialised { method } => {
                write!(f, "{method} is already capturing")
            }
            Self::UnsupportedTarget {
                method,
                target,
                reason,
            } => write!(f, "{method} cannot capture a {target}: {reason}"),
            Self::TargetLost { method } => {
                write!(f, "the target {method} was capturing no longer exists")
            }
            Self::Interrupted { method, reason } => {
                write!(f, "{method} was interrupted: {reason}")
            }
            Self::Backend {
                method,
                operation,
                source,
            } => write!(f, "{method} failed while {operation}: {source}"),
        }
    }
}

impl Error for CaptureError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Backend { source, .. } => Some(source.as_ref()),
            Self::NotInitialised { .. }
            | Self::AlreadyInitialised { .. }
            | Self::UnsupportedTarget { .. }
            | Self::TargetLost { .. }
            | Self::Interrupted { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_name_the_backend_and_what_it_was_doing() {
        let error = CaptureError::UnsupportedTarget {
            method: CaptureMethod::DesktopDuplication,
            target: TargetKind::Window,
            reason: "it duplicates a whole display and cannot follow a window that moves \
                     between them",
        };
        assert_eq!(
            error.to_string(),
            "Desktop Duplication cannot capture a window: it duplicates a whole display \
             and cannot follow a window that moves between them"
        );
        assert_eq!(error.method(), CaptureMethod::DesktopDuplication);
    }

    #[test]
    fn a_platform_failure_keeps_its_cause() {
        let error = CaptureError::Backend {
            method: CaptureMethod::WindowsGraphicsCapture,
            operation: "creating the frame pool",
            source: Box::new(std::io::Error::other("DXGI_ERROR_DEVICE_REMOVED")),
        };
        assert_eq!(
            error.to_string(),
            "Windows Graphics Capture failed while creating the frame pool: \
             DXGI_ERROR_DEVICE_REMOVED"
        );
        assert!(
            error.source().is_some(),
            "the platform error must stay reachable through Error::source"
        );
    }
}
