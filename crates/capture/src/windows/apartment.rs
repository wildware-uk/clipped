//! The COM apartment a capture thread needs before it can activate WinRT types.

use std::thread::{self, ThreadId};

use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED};

/// A multi-threaded COM apartment, held for as long as this value lives.
///
/// Activating a WinRT runtime class — which is what
/// `Direct3D11CaptureFramePool::CreateFreeThreaded` and `GraphicsCaptureItem`
/// creation both are — fails with `CO_E_NOTINITIALIZED` on a thread that has no
/// apartment. A capture thread is created by this process and starts with none,
/// so a backend has to enter one before it touches anything, and leave it when
/// it is finished.
///
/// # Why multi-threaded
///
/// The frame pool is created free-threaded, so `FrameArrived` is raised on a
/// thread-pool thread rather than pumped through a message loop. A capture
/// thread that had to pump messages to receive frames would be a capture thread
/// that stalls whenever something else posts to it, and AGENTS.md section 20
/// puts hidden blocking on a capture thread near the top of what to avoid. The
/// multi-threaded apartment is also what makes the interfaces agile, which is
/// the premise of the `Send` argument on the backend itself.
///
/// # Ownership
///
/// One `Apartment` per thread that needs one, released in [`Drop`] on the same
/// thread that entered it. `RoUninitialize` is per thread: calling it from
/// another thread would decrement *that* thread's apartment count and leave
/// this one's raised, which is a leak and a corruption at once.
///
/// The type is [`Send`] anyway, because the backend that holds it has to be —
/// `CaptureBackend` is `Send` so that a session can build a backend and move it
/// to the capture thread. So the rule is enforced at run time instead:
/// [`enter`](Self::enter) records the thread it ran on, and [`drop`](Drop::drop)
/// releases only if it is running on that same thread. Being dropped elsewhere
/// is a contract violation — one backend belongs to one capture thread — and
/// the response is to log it and leave the apartment raised, because leaking a
/// reference on a thread that is going away is recoverable and unbalancing a
/// live thread's apartment is not.
#[derive(Debug)]
pub(super) struct Apartment {
    /// Whether this value's own `RoInitialize` succeeded, and therefore whether
    /// it owes an `RoUninitialize`.
    ///
    /// False when the thread was already in an incompatible apartment, which is
    /// not an error: something else on this thread entered it first and owns
    /// leaving it. Balancing somebody else's initialisation would tear the
    /// apartment down underneath them.
    owns_initialisation: bool,
    /// The thread `enter` ran on, which is the only thread that may release.
    thread: ThreadId,
}

impl Apartment {
    /// Enters the multi-threaded apartment on the calling thread.
    ///
    /// # Errors
    ///
    /// The `HRESULT` from `RoInitialize`, except for `RPC_E_CHANGED_MODE`,
    /// which means the thread is already in a single-threaded apartment. That
    /// is reported as success with nothing owned: the caller's WinRT
    /// activations will work, they will simply be marshalled, and refusing to
    /// capture because the host process happens to have entered an STA would be
    /// a worse answer than a slightly slower event delivery.
    pub(super) fn enter() -> Result<Self, windows::core::Error> {
        // SAFETY: `RoInitialize` takes no pointers and has no precondition
        // beyond being called on the thread whose apartment it sets, which is
        // this one. Its effect is undone by `RoUninitialize` in `Drop`, on the
        // same thread, because `Apartment` is `!Send`.
        let result = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };

        let thread = thread::current().id();
        match result {
            Ok(()) => Ok(Self {
                owns_initialisation: true,
                thread,
            }),
            Err(error) if error.code() == RPC_E_CHANGED_MODE => Ok(Self {
                owns_initialisation: false,
                thread,
            }),
            // `S_FALSE` reaches here as a success `HRESULT` rather than an
            // error and is handled by the `Ok` arm above: the thread was
            // already in the multi-threaded apartment, and this initialisation
            // still has to be balanced, which `owns_initialisation` records.
            Err(error) => Err(error),
        }
    }

    /// Whether this value will call `RoUninitialize` when it is dropped.
    ///
    /// Exists for the test below and for a diagnostic log line; a caller has
    /// nothing to decide from it.
    pub(super) const fn owns_initialisation(&self) -> bool {
        self.owns_initialisation
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if !self.owns_initialisation() {
            return;
        }
        if thread::current().id() != self.thread {
            tracing::error!(
                "a capture backend was dropped on a different thread from the one that \
                 initialised it; the COM apartment it entered has been left raised rather \
                 than unbalancing another thread's. One backend belongs to one capture \
                 thread (docs/capture-pipeline.md)"
            );
            return;
        }
        // SAFETY: balances exactly one successful `RoInitialize` performed by
        // `enter` on this thread — the check above establishes that this is the
        // thread `enter` ran on — and `owns_initialisation` guarantees there
        // was one and that it has not already been balanced, because `Drop`
        // runs once.
        unsafe { RoUninitialize() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entering_twice_on_one_thread_leaves_the_apartment_standing() {
        // The nesting rule is the whole reason `owns_initialisation` exists: a
        // backend created on a thread that is already in an apartment must not
        // tear that apartment down when it shuts down, because whatever entered
        // it first is still using it. Both of these initialise successfully —
        // the second returns `S_FALSE` — so both owe an uninitialisation, and
        // the thread stays in the apartment until the outer one drops.
        let outer = Apartment::enter().expect("a fresh test thread has no apartment");
        assert!(outer.owns_initialisation());

        {
            let inner = Apartment::enter().expect("re-entering the same mode succeeds");
            assert!(
                inner.owns_initialisation(),
                "a nested RoInitialize returns S_FALSE and still has to be balanced"
            );
        }

        // The outer guard is still valid here: if the inner drop had called
        // `RoUninitialize` without owning an initialisation, the apartment
        // would be gone and this activation would fail with
        // `CO_E_NOTINITIALIZED`.
        let activated = windows::Foundation::Uri::CreateUri(&windows::core::HSTRING::from(
            "https://github.com/wildware-uk/clipped",
        ));
        assert!(
            activated.is_ok(),
            "the apartment should still be usable: {activated:?}"
        );
        drop(outer);
    }
}
