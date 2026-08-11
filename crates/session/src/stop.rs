//! Asking a recording to stop.

use core::fmt;

/// A "stop when you can" request, polled by the recording loop between frames.
///
/// This is a trait rather than a concrete flag because the thing that raises it
/// belongs to whoever started the recording: today it is the recorder's Ctrl+C
/// handler (`apps/recorder/src/shutdown.rs`), and in M5 it will be an IPC
/// request from the desktop application. Both already own a shared flag of
/// their own, and a second one here would be a second piece of state to keep in
/// step (AGENTS.md section 55).
///
/// # What an implementation owes the caller
///
/// [`is_requested`](Self::is_requested) is called once per acquisition — tens
/// of times a second for the whole of a recording — on the thread that is also
/// capturing and encoding. It must not allocate, lock anything a slow operation
/// holds, or block (AGENTS.md section 20). An atomic load is the intended
/// implementation.
pub trait StopSignal: fmt::Debug + Sync {
    /// Whether a stop has been asked for.
    fn is_requested(&self) -> bool;
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    #[derive(Debug, Default)]
    struct Flag(AtomicBool);

    impl StopSignal for Flag {
        fn is_requested(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
    }

    #[test]
    fn a_signal_is_observed_through_a_shared_reference() {
        // The shape the recording loop uses: it holds `&dyn StopSignal` and
        // never a mutable borrow, because the thread raising the signal holds
        // one at the same time.
        let flag = Flag::default();
        let signal: &dyn StopSignal = &flag;
        assert!(!signal.is_requested());
        flag.0.store(true, Ordering::SeqCst);
        assert!(signal.is_requested());
    }
}
