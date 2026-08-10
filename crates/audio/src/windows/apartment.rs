//! The COM apartment WASAPI activation needs, and why nothing here undoes it.

use std::sync::OnceLock;

use windows::core::HRESULT;
use windows::Win32::System::Com::CoIncrementMTAUsage;

/// Ensures this process has a multi-threaded COM apartment, once.
///
/// `CoCreateInstance` for the `MMDeviceEnumerator` fails with
/// `CO_E_NOTINITIALIZED` on a thread with no apartment, and the audio capture
/// thread is a thread this crate does not create and cannot assume anything
/// about. `CoIncrementMTAUsage` creates the multi-threaded apartment if there is
/// not one and keeps it alive; a thread that has not initialised COM for itself
/// is then treated as belonging to it.
///
/// # Why multi-threaded
///
/// The device enumerator delivers `IMMNotificationClient` callbacks on its own
/// thread. In a single-threaded apartment those would be marshalled through a
/// message loop, so a capture thread would have to pump messages to learn that
/// its endpoint had been unplugged — and a capture thread that pumps messages
/// is a capture thread that stalls whenever something else posts to it
/// (AGENTS.md section 20). In the multi-threaded apartment the callbacks arrive
/// directly on an audio-service thread, which is what `notifications.rs` is
/// written for.
///
/// # Why nothing releases it
///
/// The same reason `clipped-capture`'s apartment module gives, and it is worth
/// repeating rather than cross-referencing because the consequence of getting
/// it wrong is a crash: windows-rs caches activation factories in process-wide
/// statics, and uninitialising the last apartment in the process leaves those
/// pointers dangling. So the apartment is treated as process-wide
/// infrastructure — created once, one reference for the life of the process,
/// never given back — rather than as something a stopped recording takes away
/// from the rest of the program.
///
/// That this crate and `clipped-capture` each take one reference is deliberate
/// and costs one extra increment on a counter for the life of the process.
/// Sharing a single helper would mean one of two layer-1 crates depending on
/// the other, which the layering in README.md forbids.
///
/// # Errors
///
/// The `HRESULT` from `CoIncrementMTAUsage`, which means COM itself is
/// unavailable and no audio capture is possible. The answer is cached, so a
/// second caller gets the same one without asking again.
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
    use windows::Win32::System::Com::{
        CoGetApartmentType, APTTYPE, APTTYPEQUALIFIER, APTTYPEQUALIFIER_IMPLICIT_MTA, APTTYPE_MTA,
    };

    use super::*;

    #[test]
    fn a_thread_that_never_initialised_com_belongs_to_the_apartment() {
        // Asked of COM directly rather than inferred from an activation
        // succeeding: `CoGetApartmentType` reports the state of the process,
        // where a successful activation might only report that some library
        // was resourceful about a missing apartment.
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

        assert_eq!(apartment, (APTTYPE_MTA, APTTYPEQUALIFIER_IMPLICIT_MTA));
    }
}
