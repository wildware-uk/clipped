//! Telling Windows that the display has to stay on, for as long as something is
//! capturing it.
//!
//! Windows turns the displays off on an idle timer, and a display that is off
//! is not a display that is merely dark: it stops being a source. Desktop
//! Duplication delivers *nothing at all* from an output whose display Windows
//! has powered down — not a slow trickle, not a repeated last frame, zero
//! frames — while `DuplicateOutput` still succeeds, `AttachedToDesktop` is still
//! true and every `AcquireNextFrame` answers `DXGI_ERROR_WAIT_TIMEOUT`, which is
//! the same answer an idle desktop gives
//! ([issue #461](https://github.com/wildware-uk/clipped/issues/461)). Windows
//! Graphics Capture is not a refuge either: the desktop compositor drops to
//! about 4 Hz in the same state, measured in `docs/capture-pipeline.md`.
//!
//! Measured on this project's development machine with
//! `cargo run -p clipped-capture --example duplication_probe`, which drives raw
//! DXGI and takes this crate out of the path, over one three-second pass per
//! output. The only thing that changed between the two runs was one synthetic
//! mouse-move event, about a minute apart:
//!
//! ```text
//! displays off (idle 5,765 s, AC timeout 900 s)
//!   DISPLAY2  idle desktop      -> 0 frames, 12 timeouts
//!   DISPLAY2  window repainting -> 0 frames, 12 timeouts
//!   DISPLAY1  idle desktop      -> 0 frames, 12 timeouts
//!   DISPLAY1  window repainting -> 0 frames, 12 timeouts
//!
//! displays on (one SendInput mouse move later)
//!   DISPLAY2  idle desktop      -> 496 frames, 0 timeouts
//!   DISPLAY2  window repainting -> 542 frames, 0 timeouts
//!   DISPLAY1  idle desktop      -> 3 frames, 12 timeouts
//!   DISPLAY1  window repainting -> 541 frames, 0 timeouts
//! ```
//!
//! The two idle-desktop rows with the displays *on* are the ordinary behaviour
//! this must not be confused with: a screen where nothing is changing produces
//! timeouts, and that is correct. The rows that matter are the repainting ones,
//! because a window drawing in alternating colours is a real present: 0 with the
//! display off and 541 with it on, from the same binary on the same machine.
//!
//! # What this does and does not do
//!
//! `SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED)` tells Windows
//! that the calling thread needs the display, which holds the idle timer off
//! until it is cleared. `docs/capture-pipeline.md` records a sweep in which one
//! such call held both displays on well past this machine's fifteen-minute
//! timeout.
//!
//! **It cannot wake a display that is already off.** Neither can
//! `WM_SYSCOMMAND`/`SC_MONITORPOWER`; only a real input event does that, which
//! is what the measurement above had to use. So this prevents a capture from
//! going dark and does nothing for one that started dark — which is why the
//! recorder also *reports* a source that is producing no frames rather than
//! relying on this alone (`docs/capture-pipeline.md`, ADR 0015).
//!
//! # Threading
//!
//! The state is **per thread**, and Windows drops it when that thread ends. So
//! the requirement has to be set on a thread that lives for the whole capture
//! and cleared on that same thread. [`DisplayAwake`](crate::DisplayAwake) is
//! what makes that hard to get wrong, and these two functions go no further than
//! this crate, because a `require` without a matching `release` leaves somebody's
//! monitors lit until the process exits.

use windows::Win32::System::Power::{
    SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, EXECUTION_STATE,
};

/// Asks Windows to keep the display on until [`release`] is called on this
/// thread.
///
/// Returns whether Windows accepted it. A refusal is reported rather than
/// ignored so that the caller can say the display was not held, instead of a
/// recording going dark with nothing to explain it — but it is not an error that
/// stops a recording, because a recording of a lit screen is exactly what a
/// refusal still allows.
pub(crate) fn require() -> bool {
    // SAFETY: the call takes a bitmask by value, returns one by value, touches
    // nothing this process owns, and is documented as callable from any thread.
    // `ES_CONTINUOUS` is what makes the requirement persist rather than being a
    // one-shot reset of the idle timer; without it the flags below would nudge
    // the timer once and expire, which is the classic way of writing this and
    // getting nothing.
    let previous = unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED) };

    // Zero is the documented failure. The function does not use `GetLastError`,
    // so this is the whole of what Windows says about it.
    previous != EXECUTION_STATE(0)
}

/// Gives the display back, and says whether this thread was really holding it.
///
/// `SetThreadExecutionState` returns the state it replaced, so clearing the
/// requirement is also the only way to observe it: the answer here is read from
/// that return value rather than from a flag this module kept, which is what
/// makes "the display was held" a measurement instead of a claim.
pub(crate) fn release() -> bool {
    // SAFETY: as above. `ES_CONTINUOUS` on its own is the documented way to
    // clear every continuous requirement this thread has set.
    let previous = unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
    previous.contains(ES_DISPLAY_REQUIRED)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each case runs on a thread of its own because the state under test is
    /// thread-local: a test that set it on the harness's thread would leak the
    /// requirement into every test that ran after it on the same thread, and one
    /// that read it there would be reading whatever the previous test left.
    fn on_a_fresh_thread<T: Send + 'static>(body: impl FnOnce() -> T + Send + 'static) -> T {
        std::thread::spawn(body)
            .join()
            .expect("the body of this test does not panic")
    }

    #[test]
    fn a_thread_that_never_asked_is_not_holding_the_display() {
        // The half that stops `release` being written as `true`. Windows reports
        // the state that was actually replaced, and on a thread that has asked
        // for nothing that state contains no display requirement.
        assert!(
            !on_a_fresh_thread(release),
            "a thread that never called require must not report that it held the display"
        );
    }

    #[test]
    fn the_requirement_is_held_until_it_is_released_and_not_afterwards() {
        let (required, first_release, second_release) = on_a_fresh_thread(|| {
            let required = require();
            (required, release(), release())
        });

        assert!(
            required,
            "SetThreadExecutionState refused ES_CONTINUOUS | ES_DISPLAY_REQUIRED, which it is \
             documented to accept from any thread"
        );
        assert!(
            first_release,
            "the display requirement require() set was not there when release() looked, so \
             nothing was holding the display on for the recording"
        );
        assert!(
            !second_release,
            "releasing twice reported a second hold; the requirement was cleared by the first \
             release and there was nothing left to give back"
        );
    }
}
