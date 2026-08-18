//! What a recording is capturing with, for anybody who is not the capture
//! thread.
//!
//! # Why this exists
//!
//! [`CaptureFallback`](clipped_capture::CaptureFallback) knows which backend a
//! recording asked for, which one it started with and every replacement since —
//! and it belongs to the capture thread, like the backends it creates. Its
//! [`CaptureStatus`] borrows its change list, so the reading cannot outlive the
//! frame it was taken in, let alone reach a connection thread in another part of
//! the process.
//!
//! That was the whole of why the desktop application could not report the
//! capture backend ([issue #302](https://github.com/wildware-uk/clipped/issues/302)):
//! not that nothing knew, but that what knew could not be asked. This module is
//! the answer — an **owned** reading, taken once on the capture thread and left
//! somewhere any thread can read it.
//!
//! # Threading
//!
//! One [`Mutex`] around a small value, and the discipline is the one
//! `RecordingState::watching` documents in `apps/recorder`: it is held for a
//! clone or a store and never across a capture, a file or an event. The writer
//! is the recording thread and it writes **once**, after its backend is open and
//! before the first frame is asked for; the readers are connection threads
//! answering a command. Nothing on the frame loop touches it, which is what
//! AGENTS.md sections 17 and 20 require of anything a diagnostics screen wants.
//!
//! Falling back *during* a recording would write it again, and this is shaped for
//! that: it is a lock rather than a
//! [`OnceLock`](std::sync::OnceLock) precisely so that the second writer costs
//! nothing to add. Nothing calls
//! [`CaptureFallback::recover`](clipped_capture::CaptureFallback::recover) yet —
//! see `docs/diagnostics.md`.

use std::sync::{Arc, Mutex, PoisonError};

use clipped_capture::{CaptureMethod, CaptureMethodSetting, CaptureStatus, MethodChange};

/// An owned reading of [`CaptureStatus`], taken on the capture thread.
///
/// The same four facts, with the change list copied rather than borrowed. The
/// copy is what makes it a value a window can be told about, and it costs one
/// small `Vec` per recording — the list is empty on every machine where the
/// preferred backend starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureAccount {
    setting: CaptureMethodSetting,
    started_with: CaptureMethod,
    current: CaptureMethod,
    changes: Vec<MethodChange>,
}

impl CaptureAccount {
    /// A reading, from the four facts that make one up.
    ///
    /// Public so that this module is testable without a graphics device: the
    /// path that matters — a reading taken on one thread and read on another —
    /// is the same whether the four values came from a real
    /// [`CaptureFallback`](clipped_capture::CaptureFallback) or from a test.
    /// What comes from a real one is [`From<CaptureStatus>`](Self::from).
    #[must_use]
    pub const fn new(
        setting: CaptureMethodSetting,
        started_with: CaptureMethod,
        current: CaptureMethod,
        changes: Vec<MethodChange>,
    ) -> Self {
        Self {
            setting,
            started_with,
            current,
            changes,
        }
    }

    /// What the recording asked for: `Automatic`, or a method that was pinned.
    #[must_use]
    pub const fn setting(&self) -> CaptureMethodSetting {
        self.setting
    }

    /// The method this recording started with.
    #[must_use]
    pub const fn started_with(&self) -> CaptureMethod {
        self.started_with
    }

    /// The method capturing when this reading was taken.
    #[must_use]
    pub const fn current(&self) -> CaptureMethod {
        self.current
    }

    /// Every replacement and restart up to this reading, in the order they
    /// happened.
    #[must_use]
    pub fn changes(&self) -> &[MethodChange] {
        &self.changes
    }
}

impl From<CaptureStatus<'_>> for CaptureAccount {
    /// Copies a borrowed reading into one that can outlive the capture thread.
    ///
    /// The copy is the whole point: [`CaptureStatus`] borrows the fallback's
    /// change list, so without it there is nothing to hand to another thread.
    fn from(status: CaptureStatus<'_>) -> Self {
        Self {
            setting: status.setting(),
            started_with: status.initial_method(),
            current: status.current_method(),
            changes: status.changes().to_vec(),
        }
    }
}

/// Where a recording publishes how it is capturing.
///
/// Handed to a recording through
/// [`RecordingOutputs::capture`](crate::RecordingOutputs::capture) and kept by
/// whoever started it, in the way
/// [`RecordingProgress`](crate::RecordingProgress) is. A recording given none
/// publishes nothing and is otherwise identical.
#[derive(Debug, Clone, Default)]
pub struct CaptureAccounting {
    shared: Arc<Mutex<Option<CaptureAccount>>>,
}

impl CaptureAccounting {
    /// A handle for a recording that has not chosen a backend yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How the recording is capturing, or [`None`] while it has not said.
    ///
    /// [`None`] is the honest answer for the moments between a recording being
    /// asked for and its backend opening, and for a recording that failed before
    /// it opened one. It is not "the default backend": which method a recording
    /// will use is not known until [`select`](clipped_capture::select) has run
    /// and the backend has initialised, and reporting a guess is the mistake
    /// AGENTS.md section 27 is about.
    #[must_use]
    pub fn account(&self) -> Option<CaptureAccount> {
        self.shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Records how the recording is capturing.
    ///
    /// Called from the capture thread, once, after the backend is open and
    /// before the first frame is asked for. Taking the reading there rather than
    /// per frame is the whole point: a capture thread may not wait on anything a
    /// window does (AGENTS.md section 20), and this is the one moment the fact
    /// changes.
    pub fn publish(&self, account: CaptureAccount) {
        *self.shared.lock().unwrap_or_else(PoisonError::into_inner) = Some(account);
    }
}

#[cfg(test)]
mod tests {
    use clipped_capture::{CaptureMethod, CaptureMethodSetting};

    use super::{CaptureAccount, CaptureAccounting};

    fn reading() -> CaptureAccount {
        CaptureAccount::new(
            CaptureMethodSetting::Automatic,
            CaptureMethod::WindowsGraphicsCapture,
            CaptureMethod::WindowsGraphicsCapture,
            Vec::new(),
        )
    }

    #[test]
    fn nothing_is_published_before_a_recording_has_opened_a_backend() {
        let accounting = CaptureAccounting::new();

        assert_eq!(
            accounting.account(),
            None,
            "a recording that has not chosen a backend must report none rather than a default: \n             \"not chosen yet\" and \"chose this\" are different facts, and only one of them is a \n             measurement"
        );
    }

    #[test]
    fn a_reading_taken_on_one_thread_is_read_from_another() {
        // The property this module exists for, and the one the borrowed
        // `CaptureStatus` cannot have. A test that published and read on one
        // thread would pass against a type that could not cross.
        let accounting = CaptureAccounting::new();
        let writer = accounting.clone();

        std::thread::spawn(move || writer.publish(reading()))
            .join()
            .expect("the publishing thread does not panic");

        let account = accounting.account().expect("the reading crossed");
        assert_eq!(
            account.started_with(),
            CaptureMethod::WindowsGraphicsCapture
        );
        assert_eq!(account.current(), CaptureMethod::WindowsGraphicsCapture);
        assert!(
            account.changes().is_empty(),
            "an empty change list is what says the backend has never been replaced, and it must \n             survive the crossing as an empty list rather than as an absence"
        );
    }

    #[test]
    fn a_later_reading_replaces_the_one_before_it() {
        // Not reachable today — nothing calls `CaptureFallback::recover` — and
        // asserted anyway, because the shape is what makes wiring that up a
        // one-line change rather than a redesign (`docs/diagnostics.md`).
        let accounting = CaptureAccounting::new();
        accounting.publish(reading());
        accounting.publish(CaptureAccount::new(
            CaptureMethodSetting::Automatic,
            CaptureMethod::WindowsGraphicsCapture,
            CaptureMethod::DesktopDuplication,
            Vec::new(),
        ));

        let account = accounting.account().expect("something was published");
        assert_eq!(
            account.current(),
            CaptureMethod::DesktopDuplication,
            "what is capturing now is the latest reading, not the first"
        );
        assert_eq!(
            account.started_with(),
            CaptureMethod::WindowsGraphicsCapture,
            "what the recording started with does not change when the backend does"
        );
    }
}
