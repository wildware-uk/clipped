//! The capture backend interface: what a backend declares, and what it does.

use core::fmt;
use core::time::Duration;

use crate::{
    CaptureError, CaptureMethod, CaptureTarget, CapturedFrame, FrameFormat, FrameSize, TargetKind,
    TargetProperties,
};

/// The fixed facts about a backend, true before it is pointed at anything.
///
/// These are *declarations*, not measurements. A backend states them as
/// constants; [`select`](crate::select) and the diagnostics screen read them,
/// and neither has to start a capture to find out.
///
/// Kept to the questions that actually differ between the methods in
/// [`CaptureMethod::PREFERENCE_ORDER`]. Anything a backend can determine for
/// itself once it has the target belongs in
/// [`BackendDeclaration::availability`] instead, where it can look.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BackendCapabilities {
    captures_windows: bool,
    captures_monitors: bool,
    occlusion_independent: bool,
    cursor_optional: bool,
}

impl BackendCapabilities {
    /// Declares a backend that can capture the given kinds of target.
    ///
    /// The remaining capabilities default to the conservative answer — no, it
    /// is not occlusion independent, and no, the cursor cannot be turned off —
    /// so a backend author states the good news explicitly rather than
    /// inheriting it.
    ///
    /// # Panics
    ///
    /// If both are false. A backend that can capture neither a window nor a
    /// display has no reason to exist, and [`select`](crate::select) would pass
    /// over it for every target without ever saying why.
    ///
    /// Unlike [`FrameSize::new`], which returns [`Option`] for the analogous
    /// invariant, this is a panic, because the two invariants are broken by
    /// different people. A frame size comes from the platform at run time and a
    /// caller has to cope with a minimised window; capabilities are a constant
    /// the backend's author writes, so this is a `const fn` and the assertion
    /// fires while the crate is being compiled — the declaration in the
    /// doctest below stops the build rather than a recording.
    ///
    /// ```compile_fail
    /// # use clipped_capture::BackendCapabilities;
    /// const NOTHING: BackendCapabilities = BackendCapabilities::new(false, false);
    /// ```
    #[must_use]
    pub const fn new(captures_windows: bool, captures_monitors: bool) -> Self {
        assert!(
            captures_windows || captures_monitors,
            "a capture backend must capture windows, displays or both"
        );
        Self {
            captures_windows,
            captures_monitors,
            occlusion_independent: false,
            cursor_optional: false,
        }
    }

    /// Declares that the backend captures the target's own content even when
    /// something is drawn over it.
    ///
    /// True for a window captured through Windows Graphics Capture, which reads
    /// the compositor's copy of that window. False for Desktop Duplication,
    /// which duplicates what is on the display: a notification, an overlay or
    /// another window lands in the recording. This is the main reason the
    /// preference order is the order it is.
    #[must_use]
    pub const fn with_occlusion_independent(mut self, independent: bool) -> Self {
        self.occlusion_independent = independent;
        self
    }

    /// Declares that [`CaptureConfig::capture_cursor`] is honoured.
    ///
    /// When false the backend produces whatever it produces and the setting is
    /// ignored, which the settings UI needs to know so that it can say so
    /// instead of offering a switch that does nothing.
    #[must_use]
    pub const fn with_cursor_optional(mut self, optional: bool) -> Self {
        self.cursor_optional = optional;
        self
    }

    /// Whether a single window can be captured.
    #[must_use]
    pub const fn captures_windows(&self) -> bool {
        self.captures_windows
    }

    /// Whether a whole display can be captured.
    #[must_use]
    pub const fn captures_monitors(&self) -> bool {
        self.captures_monitors
    }

    /// Whether the target's own content is captured even when it is covered.
    #[must_use]
    pub const fn is_occlusion_independent(&self) -> bool {
        self.occlusion_independent
    }

    /// Whether the cursor can be excluded.
    #[must_use]
    pub const fn is_cursor_optional(&self) -> bool {
        self.cursor_optional
    }

    /// Whether this backend can address a target of `kind` at all.
    #[must_use]
    pub const fn captures(&self, kind: TargetKind) -> bool {
        match kind {
            TargetKind::Window => self.captures_windows,
            TargetKind::Monitor => self.captures_monitors,
        }
    }
}

/// Whether a backend is willing to capture a particular target right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[must_use]
pub enum Availability {
    /// The backend expects to be able to capture this target.
    ///
    /// A declaration, not a guarantee — a driver can still refuse at
    /// initialisation — but a backend that answers this and then fails
    /// systematically is a bug in the backend.
    Available,
    /// The backend declines, for a reason worth showing a user.
    Unavailable(Unavailable),
}

/// Why a backend declined a target.
///
/// These reach the user through the diagnostics screen
/// ([issue #101](https://github.com/wildware-uk/clipped/issues/101)), so they
/// explain rather than blame: "this build has no Game Capture backend" is
/// useful, "unavailable" is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Unavailable {
    /// The method is recognised but nothing in this build implements it.
    ///
    /// [`CaptureMethod::GameCapture`] is the standing example: SPEC.md section
    /// 8 prefers it, AGENTS.md section 34 rules out the usual way of doing it,
    /// and so no candidate for it is registered at all.
    NotImplemented,
    /// The operating system does not provide what this backend needs.
    UnsupportedSystem {
        /// What is missing, as in `"Windows 10 build 1903 or later"`.
        requirement: &'static str,
    },
    /// The API is present but cannot capture this particular target.
    UnsupportedTarget {
        /// Why, as in `"the window has opted out of being captured"`.
        reason: &'static str,
    },
}

impl fmt::Display for Unavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotImplemented => f.write_str("not implemented in this build"),
            Self::UnsupportedSystem { requirement } => write!(f, "requires {requirement}"),
            Self::UnsupportedTarget { reason } => f.write_str(reason),
        }
    }
}

/// What [`select`](crate::select) is allowed to know about a backend.
///
/// Everything here is cheap, side-effect free and safe to call on any thread
/// with no GPU work in flight. That is why choosing a backend is a pure
/// function that can be unit tested without a display.
///
/// It is a separate trait from [`CaptureBackendFactory`] on purpose: the
/// selector is handed `&dyn BackendDeclaration` and therefore *cannot* start a
/// capture as a side effect of choosing one, however tempting a future change
/// makes it.
///
/// # Threading
///
/// `Send + Sync`: declarations live in a registry shared by whichever thread
/// starts a session.
pub trait BackendDeclaration: fmt::Debug + Send + Sync {
    /// Which capture method this backend implements.
    ///
    /// Unique within a candidate list — [`select`](crate::select) rejects a
    /// list containing two candidates for the same method, because the report
    /// it produces would be ambiguous about which one ran.
    fn method(&self) -> CaptureMethod;

    /// What the backend can do, regardless of target.
    fn capabilities(&self) -> BackendCapabilities;

    /// Whether this backend expects to be able to capture `target`.
    ///
    /// Called once per session, before anything is initialised. It must not
    /// allocate GPU resources, block, or take longer than a system query:
    /// every candidate ahead of the winner is asked, and the user is waiting
    /// for a recording to start.
    ///
    /// It does not need to re-check the target kind. [`select`](crate::select)
    /// has already compared it against [`capabilities`](Self::capabilities) and
    /// will not ask a backend about a kind it declared it cannot capture.
    fn availability(&self, target: &TargetProperties) -> Availability;
}

/// Creates capture backends of one method.
///
/// Splitting creation from [`BackendDeclaration`] keeps selection pure; this is
/// the half the session actually calls, once, on the backend selection chose.
///
/// The returned backend is *not* initialised. Two steps rather than one because
/// the backend has to be moved onto the capture thread before it touches a
/// device: `create` runs wherever the session starts, and
/// [`CaptureBackend::initialise`] runs on the thread that will own every frame.
pub trait CaptureBackendFactory: BackendDeclaration {
    /// Builds an uninitialised backend.
    ///
    /// Fails only if the backend cannot be constructed at all. Anything that
    /// depends on the target is [`CaptureBackend::initialise`]'s problem.
    fn create(&self) -> Result<Box<dyn CaptureBackend>, CaptureError>;
}

/// How a capture session should behave, as far as the backend is concerned.
///
/// Small on purpose: this is not the settings model. Everything a user
/// configures — resolution, frame rate, bitrate — belongs to the encoder or to
/// configuration ([issue #108](https://github.com/wildware-uk/clipped/issues/108)).
/// Only choices the *capture API itself* makes appear here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct CaptureConfig {
    capture_cursor: bool,
}

impl CaptureConfig {
    /// Includes or excludes the mouse cursor.
    ///
    /// Honoured only by a backend whose
    /// [`BackendCapabilities::is_cursor_optional`] is true; others ignore it
    /// and record whatever their API gives them.
    #[must_use]
    pub const fn with_capture_cursor(mut self, capture: bool) -> Self {
        self.capture_cursor = capture;
        self
    }

    /// Whether the cursor should appear in the recording.
    #[must_use]
    pub const fn capture_cursor(&self) -> bool {
        self.capture_cursor
    }
}

/// The outcome of asking a backend for a frame.
///
/// Not every acquisition produces a frame, and the two things that happen
/// instead are ordinary rather than exceptional: a game that is not rendering
/// produces nothing, and a window that has been resized produces a different
/// shape. Neither is an error, so neither is a
/// [`CaptureError`](crate::CaptureError).
#[derive(Debug)]
pub enum Acquisition<'frame> {
    /// A frame, borrowed from the backend until it is dropped.
    Frame(CapturedFrame<'frame>),
    /// Nothing arrived within the timeout.
    ///
    /// Expected and common: a source only produces a frame when its content
    /// changes, so an idle game, a paused video or a static menu can go for
    /// seconds without one. The caller repeats the acquisition. Deciding
    /// whether to fabricate a repeat frame to keep the encoder's rate steady is
    /// the encoder's business, not the backend's.
    Timeout,
    /// The target changed shape, and the frame that revealed it was discarded.
    ///
    /// The backend is now idle: it will keep reporting this until
    /// [`CaptureBackend::resize`] is called. It cannot simply carry on, because
    /// the caller has an encoder configured for the old size and every frame
    /// after a silent resize would be wrong.
    SizeChanged(FrameSize),
}

/// A live capture of one target.
///
/// # Lifecycle
///
/// ```text
/// CaptureBackendFactory::create
///          ↓
///     initialise  ────────────────┐
///          ↓                      │ Acquisition::SizeChanged
///     acquire ⇄ acquire ⇄ ...     │          ↓
///          ↓                      └───── resize
///     shut_down
/// ```
///
/// `initialise` once, `acquire` in a loop, `resize` when an acquisition says
/// so, `shut_down` at the end. Out of order is a programming error and returns
/// [`CaptureError::NotInitialised`] or
/// [`CaptureError::AlreadyInitialised`] rather than misbehaving quietly.
///
/// # Ownership
///
/// The backend owns every native resource involved for the whole of its life:
/// the device, the frame pool or duplication, and every texture. A caller never
/// owns a frame; it borrows one, and the borrow of `&mut self` in
/// [`acquire`](Self::acquire) means the compiler will not let it hold two at
/// once or keep one across the next acquisition. An implementation therefore
/// releases the previous frame back to the platform API at the start of
/// `acquire` and in `shut_down`, and it must also release everything in `Drop`,
/// because a panic on the capture thread must not leak a duplication that stops
/// every other application from capturing the display (AGENTS.md section 58).
///
/// # Threading
///
/// One backend belongs to one capture thread. It is [`Send`], so a session can
/// build it on one thread and move it to the capture thread, and deliberately
/// not `Sync`: nothing shares it. Every method here is called from that one
/// thread.
///
/// The corollary is a rule about what that thread may do. It must not block on
/// the UI, the database, thumbnails, the network or a plugin (AGENTS.md section
/// 20). It acquires a frame, hands the texture to the encoder, drops the frame
/// and acquires the next one; anything slower happens on another thread, fed by
/// encoded packets rather than by borrowed textures.
pub trait CaptureBackend: fmt::Debug + Send {
    /// Which capture method this is, for reporting and for logs.
    ///
    /// Matches the [`BackendDeclaration::method`] of the factory that created
    /// it.
    fn method(&self) -> CaptureMethod;

    /// Opens the capture and returns the shape of the frames it will produce.
    ///
    /// The format is returned rather than discovered from the first frame so
    /// that the encoder can be configured before capture starts: a session that
    /// has to wait for a frame to size its encoder loses the first fraction of
    /// a second of the recording, which is the fraction the user pressed the
    /// key for.
    ///
    /// Returns [`CaptureError::AlreadyInitialised`] if called twice.
    fn initialise(
        &mut self,
        target: &CaptureTarget,
        config: &CaptureConfig,
    ) -> Result<FrameFormat, CaptureError>;

    /// Waits up to `timeout` for the next frame.
    ///
    /// The returned frame borrows `self`, so it must be dropped before the next
    /// acquisition — the compiler enforces it:
    ///
    /// ```compile_fail
    /// # use clipped_capture::{Acquisition, CaptureBackend, CaptureError};
    /// # use core::time::Duration;
    /// fn hold_two(backend: &mut dyn CaptureBackend) -> Result<(), CaptureError> {
    ///     let first = backend.acquire(Duration::from_millis(16))?;
    ///     // error[E0499]: cannot borrow `*backend` as mutable more than once
    ///     let second = backend.acquire(Duration::from_millis(16))?;
    ///     drop((first, second));
    ///     Ok(())
    /// }
    /// ```
    ///
    /// One at a time is fine, which is the shape of a capture loop:
    ///
    /// ```
    /// # use clipped_capture::{Acquisition, CaptureBackend, CaptureError};
    /// # use core::time::Duration;
    /// fn pump(backend: &mut dyn CaptureBackend) -> Result<(), CaptureError> {
    ///     loop {
    ///         match backend.acquire(Duration::from_millis(16))? {
    ///             Acquisition::Frame(frame) => {
    ///                 // Submit `frame.texture()` to the encoder here, on this
    ///                 // thread, and let the frame drop before looping.
    ///                 let _ = frame.timestamp();
    ///             }
    ///             Acquisition::Timeout => continue,
    ///             Acquisition::SizeChanged(size) => {
    ///                 backend.resize(size)?;
    ///             }
    ///         }
    ///     }
    /// }
    /// ```
    ///
    /// `timeout` bounds the wait so that a capture thread stays responsive to
    /// a stop request; it is not a frame rate.
    fn acquire(&mut self, timeout: Duration) -> Result<Acquisition<'_>, CaptureError>;

    /// Reconfigures the capture for a new target size and returns the new
    /// frame format.
    ///
    /// Called after [`Acquisition::SizeChanged`], with the size that
    /// acquisition reported. The backend rebuilds whatever the platform
    /// requires — a Windows Graphics Capture frame pool, a Desktop Duplication
    /// output — and the caller reconfigures the encoder from the returned
    /// format.
    ///
    /// Every texture handed out before this call is invalid afterwards, which
    /// the borrow checker already guarantees: `&mut self` means no frame can be
    /// outstanding.
    fn resize(&mut self, size: FrameSize) -> Result<FrameFormat, CaptureError>;

    /// Releases everything and stops capturing.
    ///
    /// Infallible and idempotent by contract. Teardown reports nothing because
    /// there is nothing a caller could usefully do about a failure to release a
    /// resource, and a `Result` here would collect `let _ =` at every call site
    /// (AGENTS.md section 15). A backend that hits an error while shutting down
    /// logs it and carries on releasing the rest.
    ///
    /// An implementation must do the same work from `Drop`, so that an unwind
    /// or an early return cannot leak native resources.
    fn shut_down(&mut self);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "a capture backend must capture windows, displays or both")]
    fn a_backend_that_can_capture_nothing_is_refused() {
        // The compile-time half of this is the `compile_fail` doctest on
        // `new`; this is the same invariant reached through a value computed at
        // run time, which is the only other way to get here.
        let neither = [false, false];
        let _ = BackendCapabilities::new(neither[0], neither[1]);
    }
}
