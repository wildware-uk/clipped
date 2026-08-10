//! The COM apartment WinRT activation needs, and why nothing here undoes it.

use std::sync::OnceLock;

use windows::core::HRESULT;
use windows::Win32::System::Com::CoIncrementMTAUsage;

/// Ensures this process has a multi-threaded COM apartment, once.
///
/// Activating a WinRT runtime class — which is what
/// `Direct3D11CaptureFramePool::CreateFreeThreaded` and `GraphicsCaptureItem`
/// creation both are — fails with `CO_E_NOTINITIALIZED` in a process with no
/// apartment. `CoIncrementMTAUsage` creates the multi-threaded apartment if
/// there is not one and keeps it alive, and a thread that has not initialised
/// COM for itself is then treated as belonging to it, which is exactly what a
/// capture thread wants: no message loop, no dispatcher queue, no per-thread
/// bookkeeping.
///
/// # Why multi-threaded
///
/// The frame pool is created free-threaded, so `FrameArrived` is raised on a
/// thread-pool thread rather than pumped through a message loop. A capture
/// thread that had to pump messages to receive frames would be a capture thread
/// that stalls whenever something else posts to it, and AGENTS.md section 20
/// puts hidden blocking on a capture thread near the top of what to avoid.
///
/// # Why nothing releases it
///
/// This is the one native resource in the crate with no deterministic release,
/// and that is a decision rather than an oversight of AGENTS.md section 58.
///
/// The obvious design — `RoInitialize` on the capture thread, `RoUninitialize`
/// from a guard's `Drop` — was written first, and it crashes. windows-rs caches
/// activation factories in process-wide statics and keeps the raw pointers for
/// the life of the program; when the last thread in the apartment uninitialises,
/// the apartment goes with it and every cached pointer is left dangling. Here
/// that showed up as `STATUS_ACCESS_VIOLATION` in CI, in a test run where one
/// thread's guard dropped while another thread was activating a WinRT type —
/// intermittently, because it depends on which test finishes first. A recorder
/// that stops a recording while its audio or encoder threads are mid-activation
/// is the same race with a user's session attached to it.
///
/// So the apartment is treated as what it is: process-wide infrastructure, like
/// a loaded DLL, rather than a per-capture resource. It is created once, it
/// costs one reference for the life of the process, and it is not something a
/// stopped recording should take away from the rest of the program. A backend
/// that started and stopped capture a thousand times still holds exactly one.
///
/// # Errors
///
/// The `HRESULT` from `CoIncrementMTAUsage`, which means COM itself is
/// unavailable and no capture is possible. The answer is cached, so a second
/// caller gets the same one without asking again.
pub(super) fn ensure_multi_threaded_apartment() -> Result<(), windows::core::Error> {
    /// The result of the one and only attempt. The cookie
    /// `CoIncrementMTAUsage` returns is deliberately dropped: it is useful only
    /// for `CoDecrementMTAUsage`, which is the call this function exists in
    /// order not to make.
    static APARTMENT: OnceLock<Result<(), HRESULT>> = OnceLock::new();

    APARTMENT
        .get_or_init(|| {
            // SAFETY: `CoIncrementMTAUsage` takes no arguments and no pointers.
            // Its only obligation is that a matching `CoDecrementMTAUsage` may
            // be called with the cookie it returns, which this crate never does
            // and never should — see "Why nothing releases it" above.
            unsafe { CoIncrementMTAUsage() }
                .map(|_cookie| ())
                .map_err(|error| error.code())
        })
        .map_err(windows::core::Error::from_hresult)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_apartment_is_available_to_a_thread_that_never_asked_for_one() {
        // The property the whole design rests on: a thread which has not
        // initialised COM for itself can still activate a WinRT type once the
        // process has a multi-threaded apartment. If that were untrue, every
        // capture would need per-thread initialisation, and the release problem
        // this function exists to avoid would come straight back.
        ensure_multi_threaded_apartment().expect("the process can have an MTA");

        let activated = std::thread::spawn(|| {
            windows::Foundation::Uri::CreateUri(&windows::core::HSTRING::from(
                "https://github.com/wildware-uk/clipped",
            ))
            .is_ok()
        })
        .join()
        .expect("the activation thread did not panic");

        assert!(
            activated,
            "a thread with no apartment of its own should still be able to \
             activate a WinRT type"
        );
    }

    #[test]
    fn asking_twice_is_the_same_answer_and_not_a_second_reference() {
        // `get_or_init` runs the body once, so a second call cannot raise the
        // process's apartment count again. Two calls returning the same `Ok` is
        // the observable half of that; the half that matters is that a recorder
        // starting and stopping capture all day holds one reference, not one
        // per session.
        assert!(ensure_multi_threaded_apartment().is_ok());
        assert!(ensure_multi_threaded_apartment().is_ok());
    }
}
