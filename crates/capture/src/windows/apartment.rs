//! The COM apartment WinRT activation needs, and why nothing here undoes it.

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
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
            #[cfg(test)]
            REFERENCES_TAKEN.fetch_add(1, Ordering::Relaxed);

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

/// How many references this process has taken on the multi-threaded apartment.
///
/// The whole point of the design is that the answer is one, for ever, however
/// many recordings start and stop — and "one, for ever" is not something the
/// two calls returning `Ok` can demonstrate. It is only compiled into tests
/// because there is nothing in a recorder that should ever read it.
#[cfg(test)]
static REFERENCES_TAKEN: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
mod tests {
    use windows::Win32::System::Com::{
        CoGetApartmentType, APTTYPE, APTTYPEQUALIFIER, APTTYPEQUALIFIER_IMPLICIT_MTA, APTTYPE_MTA,
    };

    use super::*;

    #[test]
    fn a_thread_that_never_initialised_com_belongs_to_the_apartment() {
        // The property the whole design rests on, asked of COM directly rather
        // than inferred from an activation succeeding. Activating a WinRT type
        // proves nothing here: windows-core's factory cache catches
        // `CO_E_NOTINITIALIZED`, calls `CoIncrementMTAUsage` itself and retries
        // (`imp/factory_cache.rs`), so that test passes whether or not this
        // module does anything at all. `CoGetApartmentType` has no such
        // fallback — on a thread with no apartment in a process with no MTA it
        // returns `CO_E_NOTINITIALIZED` — so it reports the state of the
        // process rather than the resourcefulness of a library.
        ensure_multi_threaded_apartment().expect("the process can have an MTA");

        let apartment = std::thread::spawn(|| {
            let mut kind = APTTYPE::default();
            let mut qualifier = APTTYPEQUALIFIER::default();
            // SAFETY: both out parameters are live locals of the types the
            // signature names, and the call reads nothing else.
            unsafe { CoGetApartmentType(&mut kind, &mut qualifier) }.map(|()| (kind, qualifier))
        })
        .join()
        .expect("the apartment-reading thread did not panic")
        .expect("a thread in a process with an MTA has an apartment type");

        assert_eq!(
            apartment,
            (APTTYPE_MTA, APTTYPEQUALIFIER_IMPLICIT_MTA),
            "a thread that never initialised COM should be an implicit member of \
             the process's multi-threaded apartment"
        );
    }

    #[test]
    fn the_apartment_is_referenced_once_however_many_times_it_is_asked_for() {
        // The reference is never given back, so taking a second one would be a
        // leak that grows with every recording a user starts. `get_or_init`
        // runs its body once; this is the only way to observe that it did,
        // which is why `REFERENCES_TAKEN` exists.
        assert!(ensure_multi_threaded_apartment().is_ok());
        assert!(ensure_multi_threaded_apartment().is_ok());

        assert_eq!(
            REFERENCES_TAKEN.load(Ordering::Relaxed),
            1,
            "the process's apartment reference is taken once for the life of the \
             process, no matter how many captures ask for it"
        );
    }
}
