//! What is being captured, described without naming a platform.

use core::fmt;

use crate::FrameSize;

/// The kind of thing a capture is pointed at.
///
/// Backends differ in which kinds they can address at all — Desktop
/// Duplication owns a whole display and reaches a window only by cropping —
/// so this is the first question [`select`](crate::select) asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TargetKind {
    /// A single top-level window belonging to some application.
    Window,
    /// An entire display output.
    Monitor,
}

impl fmt::Display for TargetKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Window => "window",
            Self::Monitor => "monitor",
        })
    }
}

/// An opaque reference to the window or display being captured.
///
/// This crate never interprets the value. On Windows it is the `HWND` or
/// `HMONITOR` that enumeration returned, carried as a plain integer so that the
/// interface itself stays free of platform types; the module that produced it
/// is the only code allowed to turn it back into a handle, and that module
/// lives in `clipped-windows` or in a `windows/` submodule of this crate
/// (AGENTS.md section 5).
///
/// It is a *reference*, not ownership: the window can close, and the display
/// can be unplugged, while this value still exists. A backend that finds the
/// target gone reports [`CaptureError::TargetLost`](crate::CaptureError::TargetLost).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TargetHandle(u64);

impl TargetHandle {
    /// Wraps a platform handle that a platform module has already validated.
    #[must_use]
    pub const fn from_raw(handle: u64) -> Self {
        Self(handle)
    }

    /// The platform handle, for the platform module that understands it.
    #[must_use]
    pub const fn as_raw(self) -> u64 {
        self.0
    }
}

/// The facts about a target that decide which backends can capture it.
///
/// Everything here is observed once, before capture starts, by whichever
/// platform module enumerated the target. [`select`](crate::select) reasons
/// over this and over declared [`BackendCapabilities`](crate::BackendCapabilities)
/// and nothing else, which is what makes the choice reproducible and testable
/// on a machine with no GPU.
///
/// Deliberately small. A field belongs here only when some backend's answer to
/// "can you capture this?" depends on it; anything a backend can look up for
/// itself at initialisation time does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TargetProperties {
    kind: TargetKind,
    size: FrameSize,
    content_protected: bool,
}

impl TargetProperties {
    /// Describes a target of `kind` currently `size` pixels, whose content is
    /// not protected.
    #[must_use]
    pub const fn new(kind: TargetKind, size: FrameSize) -> Self {
        Self {
            kind,
            size,
            content_protected: false,
        }
    }

    /// Marks the target as one the system refuses to let anything capture.
    ///
    /// On Windows this is a window that called `SetWindowDisplayAffinity`, as
    /// DRM video players and some launchers do. A backend is expected to
    /// declare itself [`Unavailable`](crate::Unavailable) for such a target
    /// rather than record a black rectangle: SPEC.md section 8 promises the
    /// user a working recording or an explanation, and
    /// [issue #97](https://github.com/wildware-uk/clipped/issues/97) makes
    /// "never silently produce a black recording" a requirement.
    #[must_use]
    pub const fn with_content_protected(mut self, protected: bool) -> Self {
        self.content_protected = protected;
        self
    }

    /// Whether this is a window or a whole display.
    #[must_use]
    pub const fn kind(&self) -> TargetKind {
        self.kind
    }

    /// The target's size when it was enumerated.
    ///
    /// A starting point, not a promise: windows are resized and displays change
    /// mode mid-recording, which is what
    /// [`Acquisition::SizeChanged`](crate::Acquisition::SizeChanged) and
    /// [`CaptureBackend::resize`](crate::CaptureBackend::resize) exist for.
    #[must_use]
    pub const fn size(&self) -> FrameSize {
        self.size
    }

    /// Whether the system refuses to let this target be captured.
    #[must_use]
    pub const fn is_content_protected(&self) -> bool {
        self.content_protected
    }
}

/// A target a backend can be pointed at: which one, and what is known about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CaptureTarget {
    handle: TargetHandle,
    properties: TargetProperties,
}

impl CaptureTarget {
    /// Pairs a platform handle with the properties observed for it.
    ///
    /// The two must describe the same thing. Enumeration produces both together
    /// and is the only place that should call this.
    #[must_use]
    pub const fn new(handle: TargetHandle, properties: TargetProperties) -> Self {
        Self { handle, properties }
    }

    /// The platform handle for the window or display.
    #[must_use]
    pub const fn handle(&self) -> TargetHandle {
        self.handle
    }

    /// What is known about the target.
    #[must_use]
    pub const fn properties(&self) -> &TargetProperties {
        &self.properties
    }
}
