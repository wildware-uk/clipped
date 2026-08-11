//! How the watcher's threads are told to finish.

use std::sync::{Condvar, Mutex};
use std::time::Duration;

/// A latch a source thread waits on and the watcher raises once.
///
/// Two threads need to stop, and they wait in different ways: the poller is
/// asleep between intervals, and the WMI threads are blocked inside a COM call
/// that cannot be interrupted. So this offers both — a sleep that ends early
/// when the latch is raised, and a cheap check for a thread that can only look
/// between calls.
///
/// It never lowers. A watcher that has been dropped stays dropped.
#[derive(Debug, Default)]
pub(crate) struct Stop {
    stopped: Mutex<bool>,
    raised: Condvar,
}

impl Stop {
    /// Raises the latch and wakes everything waiting on it.
    pub(crate) fn raise(&self) {
        let mut stopped = self.stopped.lock().unwrap_or_else(|poison| {
            // A source thread panicking while holding this lock must not stop
            // the watcher from shutting the others down; the value behind it is
            // a `bool` that cannot be left inconsistent.
            poison.into_inner()
        });
        *stopped = true;
        self.raised.notify_all();
    }

    /// Whether the latch has been raised.
    pub(crate) fn is_raised(&self) -> bool {
        *self
            .stopped
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Waits up to `duration`, ending early if the latch is raised.
    ///
    /// Reports whether it was raised, so a loop reads
    /// `while !stop.sleep(interval)`.
    pub(crate) fn sleep(&self, duration: Duration) -> bool {
        let stopped = self
            .stopped
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let (stopped, _timeout) = self
            .raised
            .wait_timeout_while(stopped, duration, |stopped| !*stopped)
            .unwrap_or_else(|poison| poison.into_inner());
        *stopped
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Instant;

    use super::*;

    #[test]
    fn a_sleep_that_is_not_interrupted_lasts_its_duration() {
        let stop = Stop::default();
        let start = Instant::now();

        assert!(!stop.sleep(Duration::from_millis(50)));
        assert!(start.elapsed() >= Duration::from_millis(50));
    }

    #[test]
    fn raising_the_latch_ends_a_sleep_early() {
        let stop = Arc::new(Stop::default());
        let sleeper = Arc::clone(&stop);

        let start = Instant::now();
        let waiting = std::thread::spawn(move || sleeper.sleep(Duration::from_secs(30)));
        // Raised from another thread while the sleeper is waiting; if the
        // condition variable were not woken this test would take 30 seconds.
        std::thread::sleep(Duration::from_millis(20));
        stop.raise();

        assert!(waiting.join().expect("the sleeping thread did not panic"));
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "the sleep waited out its full duration"
        );
    }

    #[test]
    fn a_raised_latch_never_lowers() {
        let stop = Stop::default();
        assert!(!stop.is_raised());

        stop.raise();

        assert!(stop.is_raised());
        assert!(
            stop.sleep(Duration::from_secs(30)),
            "and never sleeps again"
        );
    }
}
