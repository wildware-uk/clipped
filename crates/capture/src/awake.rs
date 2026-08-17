//! Holding the display on for as long as something is capturing it.
//!
//! A capture backend reads what is on a screen, so the screen's power state is
//! an input to capture rather than somebody else's business. When Windows turns
//! the displays off on its idle timer, Desktop Duplication delivers no frames at
//! all — not from a still desktop, which is ordinary, but from a window actively
//! repainting, which is not — and Windows Graphics Capture drops to about 4 Hz
//! because the compositor stops composing. Neither backend reports anything
//! wrong, because from the API's point of view nothing is: the answer is
//! `DXGI_ERROR_WAIT_TIMEOUT`, which is also the honest answer for a screen where
//! nothing is happening.
//!
//! The measurements are in
//! [`windows::display_required`](crate::windows) and in
//! `docs/capture-pipeline.md`; the decision to hold the display, the
//! alternatives to it and what it costs are in
//! [ADR 0015](../../../docs/adr/0015-capture-holds-the-display-awake.md).
//!
//! # What a caller gets, and what it does not
//!
//! [`DisplayAwake::hold`] returns a value that keeps the display on until it is
//! dropped. It is the whole interface, because there is nothing else a caller
//! can usefully decide: the requirement is either held for the length of the
//! capture or it is not held at all, and a capture that hands it back halfway is
//! a capture that goes dark halfway.
//!
//! Two things it deliberately does not do.
//!
//! **It does not wake a display that is already off.** Nothing a background
//! process can call does; only a real input event turns a powered-down display
//! back on. A capture that starts into a dark screen therefore stays dark, and
//! the recorder's answer to that is to *say so* — `clipped_session` counts how
//! long the source produced nothing and puts it on the recording's report — not
//! to pretend this prevented it.
//!
//! **It does not keep the machine out of sleep.** `ES_SYSTEM_REQUIRED` is not
//! set. A recorder that stopped somebody's computer from suspending would be
//! taking a much larger decision than keeping a monitor lit, and a machine that
//! suspends ends the capture anyway, which is a thing the recording can report
//! rather than a thing it has to survive.
//!
//! # Off Windows
//!
//! There is nothing to hold: this build has no capture backends at all
//! ([`registered_backends`](crate::registered_backends) is empty), so
//! [`DisplayAwake::hold`] succeeds at doing nothing and
//! [`is_held`](DisplayAwake::is_held) says false rather than claiming a hold
//! that does not exist.

use core::fmt;

/// The display, kept on for as long as this value is alive.
///
/// # Threading
///
/// **Bound to the thread that created it.** Windows tracks the requirement per
/// thread and drops it when that thread ends, so this type is deliberately not
/// [`Send`]: the compiler will not let a caller take the hold on one thread and
/// release it on another, and it will not let one be parked somewhere its owning
/// thread outlives. A recording takes it on the thread that runs the capture
/// loop, which is the thread that exists for exactly as long as the hold should.
///
/// The call itself is a bitmask handed to the kernel. It allocates nothing,
/// blocks on nothing and reaches no device, so taking it on the capture thread
/// costs that thread nothing (AGENTS.md section 20).
#[must_use = "the display is released again as soon as this value is dropped"]
pub struct DisplayAwake {
    held: bool,
    /// Makes the type neither [`Send`] nor [`Sync`], so that the hold cannot be
    /// released on a thread that never took it. A raw pointer is the ordinary
    /// way of saying that and costs nothing at run time.
    _not_send: core::marker::PhantomData<*const ()>,
}

impl DisplayAwake {
    /// Asks the operating system to keep the display on until this value is
    /// dropped.
    ///
    /// Never fails. A refusal — or a platform with no way to ask — produces a
    /// hold that reports [`is_held`](Self::is_held) as false, because the
    /// alternative is a capture that refuses to start over a power setting.
    /// What a caller owes a refusal is a log line, not an abandoned recording.
    pub fn hold() -> Self {
        #[cfg(not(windows))]
        let held = false;

        #[cfg(windows)]
        let held = {
            let held = crate::windows::require_display();
            if held {
                tracing::debug!(
                    "the display is being held on for the length of this capture; a display \
                     Windows has powered down delivers no frames (ADR 0015)"
                );
            } else {
                tracing::warn!(
                    "the display could not be held on for this capture; if the screen powers \
                     down during it, the recording will contain nothing from that point until \
                     it comes back"
                );
            }
            held
        };

        Self {
            held,
            _not_send: core::marker::PhantomData,
        }
    }

    /// Whether the display is actually being held on.
    ///
    /// False on a platform that cannot ask and on one that refused. Worth
    /// reading before concluding anything about why a recording went dark.
    #[must_use]
    pub const fn is_held(&self) -> bool {
        self.held
    }
}

impl fmt::Debug for DisplayAwake {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DisplayAwake")
            .field("held", &self.held)
            .finish()
    }
}

impl Drop for DisplayAwake {
    fn drop(&mut self) {
        // Nothing was taken off a platform that cannot ask, or after a refusal,
        // so there is nothing to give back. Off Windows this is the whole body.
        if !self.held {
            return;
        }

        #[cfg(windows)]
        if crate::windows::release_display() {
            tracing::debug!("the display is no longer being held on by this capture");
        } else {
            // Not a leak — the requirement is gone either way — but it means
            // something outside this type cleared it, and the stretch of
            // recording after that point was not protected. Worth a line,
            // because the symptom is a recording that goes dark for no reason
            // anybody can see.
            tracing::warn!(
                "the display hold this capture took had already been cleared by something \
                 else; the screen was free to power down for part of this recording"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hold_reports_whether_it_actually_holds_anything() {
        let awake = DisplayAwake::hold();

        // Split by platform because the *fact* differs by platform and a test
        // that accepted either answer everywhere would pass on a Windows build
        // that had quietly stopped asking.
        #[cfg(windows)]
        assert!(
            awake.is_held(),
            "a Windows build must actually hold the display; a hold that reports false here \
             means SetThreadExecutionState refused, and every capture on this machine is one \
             idle timeout away from recording nothing"
        );
        #[cfg(not(windows))]
        assert!(
            !awake.is_held(),
            "a build with no capture backends must not claim to be holding a display on"
        );
    }

    #[test]
    fn the_hold_is_released_when_it_is_dropped() {
        // The observation is the platform's own: `release_display` reads the
        // state it replaced, so a second hold taken after the first was dropped
        // finds nothing left over. Two holds that overlapped would leave the
        // requirement set after both had gone.
        drop(DisplayAwake::hold());

        #[cfg(windows)]
        assert!(
            !crate::windows::release_display(),
            "dropping a hold must give the display back; something was still holding it"
        );
    }

    #[test]
    fn the_debug_form_says_whether_the_display_is_held() {
        let awake = DisplayAwake::hold();
        assert_eq!(
            format!("{awake:?}"),
            format!("DisplayAwake {{ held: {} }}", awake.is_held())
        );
    }
}
