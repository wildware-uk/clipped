//! Stopping a recording without damaging it.
//!
//! A recording that is killed part-way through is a recording with no index and
//! no duration. Matroska survives that better than most containers
//! ([ADR 0001](../../../docs/adr/0001-mkv-archival-container.md)), but
//! "survives" is not "is finished properly": the encoder still holds frames it
//! has not emitted, and the muxer still has a header to rewrite. AGENTS.md
//! section 17 puts the user's recording above almost everything else, so the
//! process must be given the chance to close the file rather than be stopped
//! where it stands.
//!
//! This module is that chance, and it is the seam the capture pipeline plugs
//! into. Two pieces:
//!
//! - [`ShutdownSignal`] — a "stop when you can" flag that a Ctrl+C handler, or
//!   any other thread, can raise, and that the recording loop polls between
//!   frames. Nothing is interrupted mid-write; the loop decides when it is safe
//!   to stop.
//! - [`run_until_shutdown`] — runs the recording and then the finalisation
//!   hook, guaranteeing the hook runs exactly once whatever happened: normal
//!   end, error, Ctrl+C, or a panic on the way through.
//!
//! # What is here and what is not
//!
//! The signal, the hook and the guarantee are implemented and tested. There is
//! no capture pipeline to put between them yet — `crates/capture`,
//! `crates/encoder` and `crates/muxer` are documentation-only stubs — so today
//! the recorder's own use of this module runs a body that reports capture is
//! not implemented. Acceptance criterion 3 of
//! [issue #9](https://github.com/wildware-uk/clipped/issues/9), "Ctrl+C during
//! a recording produces a playable file", is therefore only half met: the
//! shutdown half exists and is exercised, and the recording half arrives with
//! the pipeline in M1.
//!
//! # Threading
//!
//! [`ShutdownSignal`] is `Send + Sync` and cheap to clone; every clone refers
//! to the same state. The Ctrl+C handler runs on a thread the operating system
//! provides, so all it does is set an atomic and wake the waiters — no
//! allocation, no logging, no lock held across anything slow.

use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// A request to stop, shared between the thread that asks and the thread that
/// is recording.
///
/// Clone it freely: every clone observes and raises the same request.
#[derive(Debug, Clone, Default)]
pub struct ShutdownSignal {
    state: Arc<SignalState>,
}

/// The state behind every clone of a [`ShutdownSignal`].
#[derive(Debug, Default)]
struct SignalState {
    /// The fast path, read by a recording loop between frames.
    requested: AtomicBool,
    /// The slow path, so a waiting thread does not have to spin. Holds the same
    /// value as `requested`; the atomic exists so that polling costs an atomic
    /// load rather than a lock (AGENTS.md section 20).
    requested_under_lock: Mutex<bool>,
    /// Woken when a request is raised.
    changed: Condvar,
}

impl ShutdownSignal {
    /// A signal that has not been raised.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks whatever is running to stop at its next safe point.
    ///
    /// Idempotent: raising an already-raised signal does nothing.
    pub fn request(&self) {
        self.state.requested.store(true, Ordering::SeqCst);
        if let Ok(mut requested) = self.state.requested_under_lock.lock() {
            *requested = true;
        }
        // Notified whether or not the lock was taken. A poisoned mutex means
        // some other thread panicked while holding it, which must not be
        // allowed to leave a recording running with no way to stop it.
        self.state.changed.notify_all();
    }

    /// Whether a stop has been asked for.
    ///
    /// This is what a capture loop calls between frames: one atomic load, no
    /// lock and no allocation, so polling it every frame costs nothing worth
    /// measuring (AGENTS.md section 18).
    #[must_use]
    pub fn is_requested(&self) -> bool {
        self.state.requested.load(Ordering::SeqCst)
    }

    /// Blocks until a stop is asked for, returning at once if one already was.
    pub fn wait(&self) {
        let Ok(mut requested) = self.state.requested_under_lock.lock() else {
            // The mutex is only ever held for a bool assignment, so poisoning
            // means an unrelated panic. Fall back to the atomic rather than
            // propagating a panic into a shutdown path.
            while !self.is_requested() {
                std::thread::sleep(Duration::from_millis(10));
            }
            return;
        };
        while !*requested {
            let Ok(next) = self.state.changed.wait(requested) else {
                return;
            };
            requested = next;
        }
    }
}

/// Why a run ended.
///
/// Passed to the finalisation hook, because what a muxer should do differs:
/// a completed recording gets its index written, an interrupted one gets the
/// same treatment because the user still wants the file, and a panicked one
/// gets whatever can be salvaged without trusting the process further.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The recording ended on its own — the target closed, or a requested
    /// duration elapsed.
    Completed,
    /// A stop was asked for, normally by Ctrl+C.
    Interrupted,
    /// The recording returned an error.
    Failed,
    /// The recording panicked. The hook runs during unwinding, so it should do
    /// as little as it can get away with.
    Panicked,
}

impl fmt::Display for StopReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Completed => "completed",
            Self::Interrupted => "interrupted",
            Self::Failed => "failed",
            Self::Panicked => "panicked",
        };
        formatter.write_str(name)
    }
}

/// Runs `recording`, then `finalise`, whatever happens in between.
///
/// `recording` is given the signal and is expected to poll
/// [`ShutdownSignal::is_requested`] at a point where stopping is safe — between
/// frames, not between the two halves of a write. When it returns, or unwinds,
/// `finalise` runs exactly once with the reason, and is where the encoder is
/// flushed and the container closed.
///
/// The hook runs during a panic unwind as well, which is the case that matters
/// most: a bug in the pipeline should still leave a file that plays.
///
/// # Panics
///
/// Never on its own account. A panic from `recording` runs `finalise` and then
/// continues unwinding; a panic from `finalise` itself is not caught.
pub fn run_until_shutdown<T, E, R, F>(
    signal: &ShutdownSignal,
    recording: R,
    finalise: F,
) -> Result<T, E>
where
    R: FnOnce(&ShutdownSignal) -> Result<T, E>,
    F: FnOnce(StopReason),
{
    // The guard is what makes the hook unwind-safe: if `recording` panics, the
    // guard is dropped during unwinding and the hook still runs, with the
    // reason it was last told.
    let mut guard = FinalisationGuard {
        finalise: Some(finalise),
        reason: StopReason::Panicked,
    };

    let outcome = recording(signal);
    guard.reason = match &outcome {
        Ok(_) if signal.is_requested() => StopReason::Interrupted,
        Ok(_) => StopReason::Completed,
        Err(_) => StopReason::Failed,
    };
    drop(guard);

    outcome
}

/// Runs the finalisation hook when dropped, including during a panic unwind.
struct FinalisationGuard<F: FnOnce(StopReason)> {
    finalise: Option<F>,
    reason: StopReason,
}

impl<F: FnOnce(StopReason)> Drop for FinalisationGuard<F> {
    fn drop(&mut self) {
        if let Some(finalise) = self.finalise.take() {
            finalise(self.reason);
        }
    }
}

/// Why a Ctrl+C handler could not be installed.
#[derive(Debug)]
pub struct CtrlCError(ctrlc::Error);

impl fmt::Display for CtrlCError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "the Ctrl+C handler could not be installed, so stopping the recorder \
             would leave the recording unfinished: {}",
            self.0
        )
    }
}

impl Error for CtrlCError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.0)
    }
}

/// Installs a Ctrl+C handler that raises `signal`.
///
/// The handler does nothing but raise the signal: it is called on a thread the
/// operating system creates for it, while the recording threads are still
/// running, so anything else it did would race with them.
///
/// # Errors
///
/// Returns an error if a handler is already installed — the operating system
/// allows one per process — or if the platform refuses to install one. That is
/// worth failing the command over: a recorder that cannot be stopped cleanly
/// would leave the file it is writing unfinished, which is the failure this
/// module exists to prevent.
pub fn install_ctrl_c_handler(signal: &ShutdownSignal) -> Result<(), CtrlCError> {
    let signal = signal.clone();
    ctrlc::set_handler(move || signal.request()).map_err(CtrlCError)
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;

    use super::*;

    #[test]
    fn a_fresh_signal_is_not_raised() {
        assert!(!ShutdownSignal::new().is_requested());
    }

    #[test]
    fn a_request_is_visible_through_every_clone() {
        let signal = ShutdownSignal::new();
        let clone = signal.clone();
        clone.request();
        assert!(signal.is_requested());
    }

    #[test]
    fn waiting_returns_when_another_thread_requests_a_stop() {
        let signal = ShutdownSignal::new();
        let waiter = signal.clone();
        let (started, has_started) = mpsc::channel();
        let (finished, has_finished) = mpsc::channel();

        thread::spawn(move || {
            started.send(()).expect("the test is still listening");
            waiter.wait();
            finished.send(()).expect("the test is still listening");
        });

        has_started
            .recv_timeout(Duration::from_secs(5))
            .expect("the waiting thread starts");
        assert!(
            has_finished
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "wait() returned before anything asked for a stop"
        );

        signal.request();
        has_finished
            .recv_timeout(Duration::from_secs(5))
            .expect("wait() returns once a stop is requested");
    }

    #[test]
    fn waiting_on_an_already_raised_signal_returns_immediately() {
        let signal = ShutdownSignal::new();
        signal.request();
        // Deadlocking here is the failure this asserts against; the test
        // harness's own timeout is what would catch it.
        signal.wait();
    }

    #[test]
    fn a_recording_that_ends_on_its_own_finalises_as_completed() {
        let signal = ShutdownSignal::new();
        let mut reason = None;

        let outcome: Result<u32, ()> =
            run_until_shutdown(&signal, |_| Ok(7), |stop| reason = Some(stop));

        assert_eq!(outcome, Ok(7));
        assert_eq!(reason, Some(StopReason::Completed));
    }

    #[test]
    fn a_recording_stopped_by_the_signal_finalises_as_interrupted() {
        let signal = ShutdownSignal::new();
        let mut reason = None;

        let outcome: Result<(), ()> = run_until_shutdown(
            &signal,
            |signal| {
                signal.request();
                Ok(())
            },
            |stop| reason = Some(stop),
        );

        assert_eq!(outcome, Ok(()));
        assert_eq!(reason, Some(StopReason::Interrupted));
    }

    #[test]
    fn a_recording_that_fails_still_finalises() {
        let signal = ShutdownSignal::new();
        let mut reason = None;

        let outcome: Result<(), &str> =
            run_until_shutdown(&signal, |_| Err("encoder gone"), |stop| reason = Some(stop));

        assert_eq!(outcome, Err("encoder gone"));
        assert_eq!(reason, Some(StopReason::Failed));
    }

    #[test]
    fn a_recording_that_panics_still_finalises() {
        let signal = ShutdownSignal::new();
        let reason = Arc::new(Mutex::new(None));
        let recorded = Arc::clone(&reason);

        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: Result<(), ()> = run_until_shutdown(
                &signal,
                |_| panic!("the encoder disappeared"),
                move |stop| {
                    *recorded.lock().expect("the hook holds the lock alone") = Some(stop);
                },
            );
        }));

        assert!(panicked.is_err(), "the panic should keep propagating");
        assert_eq!(
            *reason.lock().expect("the test holds the lock alone"),
            Some(StopReason::Panicked),
            "the finalisation hook must run while unwinding"
        );
    }

    #[test]
    fn the_finalisation_hook_runs_exactly_once() {
        let signal = ShutdownSignal::new();
        let mut runs = 0_u32;

        let outcome: Result<(), ()> = run_until_shutdown(
            &signal,
            |signal| {
                signal.request();
                Ok(())
            },
            |_| runs += 1,
        );

        assert_eq!(outcome, Ok(()));
        assert_eq!(runs, 1);
    }
}
