//! Where a running recording has reached on its own timeline.
//!
//! One number, published by the recording and read by anything that needs to
//! name a moment *inside* the file while it is still being written. Manual
//! bookmarks are the reason it exists
//! ([issue #64](https://github.com/wildware-uk/clipped/issues/64)): a bookmark
//! is an offset into a recording, and the only honest source for that offset is
//! the recording's own media clock.
//!
//! # Why not simply time the recording from outside
//!
//! Because the two clocks are not the same, and the difference is not small.
//! `crate::record` selects a backend, initialises it, waits for a frame in order
//! to find the device it lives on, opens an encoder against that device and
//! creates the file — and only *then* does the first frame that reaches the
//! container fix the recording's epoch (`docs/av-sync.md`). A caller measuring
//! from the moment it called `record` would be ahead of the file by however long
//! all of that took, which is hundreds of milliseconds on a warm machine and
//! seconds on a cold one. A bookmark a second out is a bookmark pointing at the
//! wrong thing.
//!
//! So the recording says where it is, in the same units and from the same epoch
//! as the timestamps it writes into the container.
//!
//! # Threading
//!
//! One relaxed atomic store per encoded frame on the capture thread, and a
//! relaxed load on whichever thread is asking. There is no lock, no allocation
//! and nothing to wait on, which is what AGENTS.md section 20 requires of
//! anything the capture thread touches: a bookmark taken during a recording must
//! not be able to delay a frame.
//!
//! Relaxed is enough because the value is a single `u64` that only ever moves
//! forward and is read for its own sake rather than to order anything else. A
//! reader can see a position one frame stale — 16.7 ms at 60 fps — and that is
//! the tolerance `docs/bookmarks.md` states rather than a fault to be fixed with
//! a stronger ordering.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use core::time::Duration;
use std::sync::Arc;

/// A handle on how far a recording has got.
///
/// Cheap to clone — it is an [`Arc`] — so the thread that starts a recording can
/// keep one while the recording keeps another.
#[derive(Debug, Clone, Default)]
pub struct RecordingProgress {
    shared: Arc<Shared>,
}

#[derive(Debug, Default)]
struct Shared {
    /// The media timestamp of the most recent frame submitted to the encoder,
    /// in nanoseconds from the recording's epoch.
    position_nanos: AtomicU64,
    /// Whether any frame has been submitted at all.
    ///
    /// Separate from the position because zero is a real position — the first
    /// frame of every recording is at zero — and "at the very beginning" and
    /// "nothing has been recorded yet" are answers a caller must be able to
    /// tell apart.
    started: AtomicBool,
}

impl RecordingProgress {
    /// A handle for a recording that has not produced a frame yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How far into the recording the last encoded frame was, or [`None`] while
    /// no frame has reached the encoder.
    ///
    /// [`None`] is the honest answer during the setup a recording does before
    /// its first frame, and it is why a bookmark asked for in that window is
    /// refused rather than placed at zero.
    #[must_use]
    pub fn position(&self) -> Option<Duration> {
        if !self.shared.started.load(Ordering::Relaxed) {
            return None;
        }
        Some(Duration::from_nanos(
            self.shared.position_nanos.load(Ordering::Relaxed),
        ))
    }

    /// Whether the recording has put anything in its file yet.
    #[must_use]
    pub fn has_started(&self) -> bool {
        self.shared.started.load(Ordering::Relaxed)
    }

    /// Publishes the position of a frame that has just reached the file.
    ///
    /// [`crate::record_into`] calls this from the capture thread, once per
    /// encoded frame. Frames that were skipped for the frame rate or dropped
    /// because the writer was behind are deliberately not published: a bookmark
    /// must name a moment that is actually in the file.
    ///
    /// Public because the handle is one half of a contract with two ends, and
    /// because it is what lets everything downstream of a recording position —
    /// the recorder's bookmark path, most of all — be exercised without a GPU,
    /// a window and a real encoder (AGENTS.md section 26).
    pub fn reached(&self, position: Duration) {
        let nanos = u64::try_from(position.as_nanos()).unwrap_or(u64::MAX);
        self.shared.position_nanos.store(nanos, Ordering::Relaxed);
        self.shared.started.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_has_a_position_until_a_frame_has_been_encoded() {
        // The distinction this holds is the one a bookmark depends on: a
        // recording that has been asked for but has not yet captured anything
        // has no position, and answering "zero" would put a bookmark at the
        // start of a file that does not contain the moment yet.
        let progress = RecordingProgress::new();
        assert_eq!(progress.position(), None);
        assert!(!progress.has_started());

        progress.reached(Duration::ZERO);
        assert_eq!(progress.position(), Some(Duration::ZERO));
        assert!(progress.has_started());
    }

    #[test]
    fn the_position_is_the_most_recent_frame_and_is_shared_by_every_handle() {
        let progress = RecordingProgress::new();
        let watcher = progress.clone();

        progress.reached(Duration::from_nanos(16_666_667));
        assert_eq!(
            watcher.position(),
            Some(Duration::from_nanos(16_666_667)),
            "a clone must see what the recording published, or a bookmark reads its own copy"
        );

        progress.reached(Duration::from_millis(2_500));
        assert_eq!(watcher.position(), Some(Duration::from_millis(2_500)));
    }
}
