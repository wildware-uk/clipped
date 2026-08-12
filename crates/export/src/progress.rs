//! Saying how far an export has got, and stopping it.
//!
//! An export is the longest thing a user waits for in Clipped, which makes both
//! of these part of the feature rather than decoration (AGENTS.md section 59):
//! a progress bar that does not move and a cancel button that does nothing are
//! two of the same bug.
//!
//! # Threading
//!
//! [`Cancellation`] is an `Arc` around an atomic flag, so the thread running
//! the export and the thread the user clicked on are not the same thread and do
//! not need to be. Setting it is one relaxed store; the export reads it between
//! packets, which bounds how long a cancel takes at one packet's write.
//!
//! The progress callback runs **on the exporting thread**. It is called at most
//! once per [`ExportOptions::progress_interval`] of output written, which for
//! the default is a few times a second rather than once per frame — a callback
//! that took a lock would otherwise be taking it sixty times a second for the
//! length of the clip (AGENTS.md section 20).

use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use std::sync::Arc;

/// How far an export has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportProgress {
    /// How much of the clip's own timeline has been written, in nanoseconds.
    pub written_nanos: u64,
    /// How long the finished clip will be, in nanoseconds.
    pub total_nanos: u64,
    /// How many packets have been written, across every track.
    pub packets: u64,
}

impl ExportProgress {
    /// How far through, between zero and one.
    ///
    /// Zero for a clip with no length, rather than a division by zero: an empty
    /// document is a valid document (`EditDocument::validate` accepts one) and
    /// an export of it finishes immediately.
    #[must_use]
    pub fn fraction(&self) -> f64 {
        if self.total_nanos == 0 {
            return 0.0;
        }
        // Clamped because the last packet of a segment can carry a duration
        // that runs a little past the end of the clip, and a progress bar that
        // reads 101 % is a bug report.
        (self.written_nanos as f64 / self.total_nanos as f64).clamp(0.0, 1.0)
    }
}

/// A shared "stop" flag an export checks between packets.
///
/// Cloning it shares the flag: the clone the caller keeps and the clone the
/// export holds are the same switch.
#[derive(Debug, Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    /// A cancellation that has not been asked for.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks the export to stop.
    ///
    /// It stops at the next packet boundary, removes what it had written, and
    /// returns [`ExportError::Cancelled`](crate::ExportError::Cancelled).
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// Whether a stop has been asked for.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// What a caller wants told, and how it stops the export.
///
/// Deliberately not `Clone` and deliberately not `Debug`-derived: it holds a
/// callback, and the export borrows it for the length of the run.
pub struct ExportOptions<'callback> {
    progress: Option<&'callback (dyn Fn(ExportProgress) + Sync)>,
    progress_interval: Duration,
    cancellation: Cancellation,
}

/// How much output is written between progress reports by default.
///
/// A quarter of a second of the *clip*, not of wall clock: a copy runs many
/// times faster than real time, so this is a handful of reports for a short
/// clip and a few hundred for a long one, which is what a progress bar wants.
pub const DEFAULT_PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

impl Default for ExportOptions<'_> {
    fn default() -> Self {
        Self {
            progress: None,
            progress_interval: DEFAULT_PROGRESS_INTERVAL,
            cancellation: Cancellation::new(),
        }
    }
}

impl<'callback> ExportOptions<'callback> {
    /// Nothing reported and nothing cancelling it.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reports progress to `callback`, on the exporting thread.
    #[must_use]
    pub fn reporting_to(mut self, callback: &'callback (dyn Fn(ExportProgress) + Sync)) -> Self {
        self.progress = Some(callback);
        self
    }

    /// Reports at most once per `interval` of output written.
    ///
    /// Zero means every packet, which is what a test that wants to count
    /// reports asks for and what an interface should not.
    #[must_use]
    pub const fn every(mut self, interval: Duration) -> Self {
        self.progress_interval = interval;
        self
    }

    /// Stops when `cancellation` is set.
    #[must_use]
    pub fn cancelled_by(mut self, cancellation: Cancellation) -> Self {
        self.cancellation = cancellation;
        self
    }

    /// The cancellation this export watches.
    #[must_use]
    pub const fn cancellation(&self) -> &Cancellation {
        &self.cancellation
    }

    /// How much output is written between reports.
    #[must_use]
    pub const fn progress_interval(&self) -> Duration {
        self.progress_interval
    }

    /// Calls the callback, if there is one.
    pub(crate) fn report(&self, progress: ExportProgress) {
        if let Some(callback) = self.progress {
            callback(progress);
        }
    }

    /// Whether a callback was given at all, so the caller of this can skip the
    /// bookkeeping when nobody is listening.
    pub(crate) const fn reports(&self) -> bool {
        self.progress.is_some()
    }
}

impl core::fmt::Debug for ExportOptions<'_> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("ExportOptions")
            .field("reports_progress", &self.progress.is_some())
            .field("progress_interval", &self.progress_interval)
            .field("cancelled", &self.cancellation.is_cancelled())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancellation_is_one_switch_however_many_clones_there_are() {
        let cancellation = Cancellation::new();
        let held_by_the_export = cancellation.clone();

        assert!(!held_by_the_export.is_cancelled());
        cancellation.cancel();
        assert!(
            held_by_the_export.is_cancelled(),
            "the export would carry on after the user pressed cancel"
        );
    }

    #[test]
    fn progress_is_a_fraction_that_cannot_leave_its_range() {
        let progress = |written: u64, total: u64| {
            ExportProgress {
                written_nanos: written,
                total_nanos: total,
                packets: 0,
            }
            .fraction()
        };

        assert!((progress(0, 4_000) - 0.0).abs() < f64::EPSILON);
        assert!((progress(1_000, 4_000) - 0.25).abs() < f64::EPSILON);
        assert!((progress(4_000, 4_000) - 1.0).abs() < f64::EPSILON);
        // The last packet of a segment can carry a duration that runs past the
        // end of the clip.
        assert!((progress(4_100, 4_000) - 1.0).abs() < f64::EPSILON);
        // An empty clip, which the document model accepts.
        assert!((progress(0, 0) - 0.0).abs() < f64::EPSILON);
    }
}
