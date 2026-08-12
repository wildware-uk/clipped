//! Asking a running recording for one of the frames it already has.
//!
//! # The problem this solves
//!
//! Somebody presses the screenshot key. The press arrives on a connection
//! thread inside the recorder (`docs/ipc.md`), and the frames are on the
//! capture thread, where they are borrowed from the backend, may not outlive
//! the acquisition that produced them and may not leave the thread at all
//! (`docs/capture-pipeline.md`). The two cannot simply be introduced.
//!
//! So this is a rendezvous with an explicit division of labour. The connection
//! thread leaves a request and waits. The capture loop notices it between two
//! frames, spends one texture copy on it, and hands back **owned pixels**. The
//! connection thread wakes up, encodes and writes the file, and answers the
//! command with the path.
//!
//! # What the capture thread pays
//!
//! One `CopyResource` on the frame the key was pressed on, and one memory copy
//! a frame or two later when the GPU has finished it
//! (`clipped_capture::windows::D3d11StillCopier`). It never waits for the GPU,
//! never encodes, never allocates in the steady state and never touches a
//! disk — which is AGENTS.md section 20's list of what a capture thread may not
//! do, item by item.
//!
//! # What happens when there is no frame
//!
//! The waiter times out and says so. A window that has stopped drawing produces
//! no frames at all, a recording can end between the request and the next
//! acquisition, and both must produce a refusal rather than a wait with no end
//! — a screenshot key that hangs the tray menu is worse than one that says it
//! could not take the picture (AGENTS.md sections 16 and 45).
//!
//! # Threading
//!
//! One mutex and one condition variable, and the mutex is never held across
//! anything slow: the capture thread's longest visit is a move of a `Vec` into
//! the slot. Several threads may wait at once — the tray and a hotkey pressed
//! together — and each gets its own request and its own answer, because a
//! screenshot is cheap enough not to be worth sharing and sharing one would mean
//! deciding whose timeout applies.

use core::time::Duration;
use std::sync::{Arc, Condvar, Mutex};

use clipped_capture::StillFrame;

use super::ScreenshotError;

/// How long a request waits for a frame when the caller does not say.
///
/// Two seconds. A recording at 30 fps produces a frame every 33 ms, so this is
/// two orders of magnitude more than the wait should ever be; what it is
/// actually sized for is the case where the answer is *no* — a window that has
/// stopped drawing — and there, two seconds is long enough that a stutter does
/// not become a refusal and short enough that a person gets an answer while
/// they are still looking at the screen.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);

/// One frame that a recording handed over, and where it sat in the recording.
#[derive(Debug)]
pub struct ServedStill {
    /// The pixels, owned.
    pub still: StillFrame,
    /// How far into the recording the frame was, on the recording's own media
    /// clock.
    ///
    /// [`None`] when the recording had not yet put a frame in its file, which
    /// is the same distinction [`crate::RecordingProgress`] draws and for the
    /// same reason: a marker at zero would point at the start of a file that
    /// does not contain the moment.
    pub position: Option<Duration>,
}

/// The channel between whoever presses the screenshot key and the recording.
///
/// Cheap to clone — it is an [`Arc`] — so the thread that starts a recording can
/// keep one while the recording keeps another, exactly as
/// [`crate::RecordingProgress`] is shared.
#[derive(Debug, Clone, Default)]
pub struct ScreenshotRequests {
    shared: Arc<Shared>,
}

#[derive(Debug, Default)]
struct Shared {
    state: Mutex<State>,
    /// Signalled when a request is answered.
    answered: Condvar,
}

#[derive(Debug, Default)]
struct State {
    /// The identifier the next request takes. Never reused, which is what lets
    /// an answer be matched to the request that asked for it.
    next_id: u64,
    /// Requests made and not yet claimed by a capture loop, oldest first.
    waiting: Vec<u64>,
    /// Answers to claimed requests, by identifier.
    ///
    /// A `Vec` rather than a map because there is never more than a handful:
    /// one per thread that pressed a key in the last couple of seconds.
    answers: Vec<(u64, Result<ServedStill, String>)>,
}

impl ScreenshotRequests {
    /// A channel nobody has asked anything of.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks the recording for a frame and waits up to [`DEFAULT_TIMEOUT`].
    ///
    /// # Errors
    ///
    /// As [`take_within`](Self::take_within).
    pub fn take(&self) -> Result<ServedStill, ScreenshotError> {
        self.take_within(DEFAULT_TIMEOUT)
    }

    /// Asks the recording for a frame and waits up to `timeout`.
    ///
    /// Blocks the calling thread, which must not be a capture thread. Returns
    /// as soon as the recording hands a frame over.
    ///
    /// # Errors
    ///
    /// [`ScreenshotError::NoFrame`] if nothing produced a frame in time —
    /// including the case where no recording is running at all, because a
    /// channel nobody is reading looks exactly like a window that has stopped
    /// drawing — and [`ScreenshotError::Copy`] if the recording tried and the
    /// copy failed.
    pub fn take_within(&self, timeout: Duration) -> Result<ServedStill, ScreenshotError> {
        let id = {
            let mut state = self.lock();
            let id = state.next_id;
            state.next_id = state.next_id.wrapping_add(1);
            state.waiting.push(id);
            id
        };

        let deadline = std::time::Instant::now() + timeout;
        let mut state = self.lock();
        loop {
            if let Some(position) = state.answers.iter().position(|(held, _)| *held == id) {
                let (_, answer) = state.answers.remove(position);
                return answer.map_err(|detail| ScreenshotError::NotCaptured { detail });
            }

            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                // Withdrawn on the way out, so that a capture loop does not
                // later copy a frame for a request nobody is waiting on. An
                // answer that had already been claimed is dropped when it
                // arrives, by `serve`.
                state.waiting.retain(|held| *held != id);
                state.answers.retain(|(held, _)| *held != id);
                return Err(ScreenshotError::NoFrame { waited: timeout });
            }

            let (guarded, _) = self
                .shared
                .answered
                .wait_timeout(state, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state = guarded;
        }
    }

    /// The oldest request nobody has started serving, if there is one.
    ///
    /// Called by the capture loop between frames. The identifier it returns
    /// must eventually be passed to [`serve`](Self::serve), with a frame or
    /// with a reason — a claimed request that is never answered is a caller
    /// waiting for its whole timeout for no reason.
    pub(crate) fn claim(&self) -> Option<u64> {
        let mut state = self.lock();
        if state.waiting.is_empty() {
            return None;
        }
        Some(state.waiting.remove(0))
    }

    /// Whether anything is waiting to be served.
    ///
    /// One lock and one `is_empty`, which is what the capture loop calls per
    /// frame in the overwhelmingly common case where the answer is no.
    pub(crate) fn is_waiting(&self) -> bool {
        !self.lock().waiting.is_empty()
    }

    /// Answers a claimed request.
    ///
    /// An answer to a request whose waiter has already given up is dropped
    /// here rather than accumulating: nothing will ever collect it.
    pub(crate) fn serve(&self, id: u64, answer: Result<ServedStill, String>) {
        {
            let mut state = self.lock();
            state.answers.push((id, answer));
        }
        // Every waiter is woken because a condition variable cannot wake one
        // *particular* waiter, and each checks for its own identifier. The
        // wasted wake-ups are bounded by how many people are holding down a
        // screenshot key at once.
        self.shared.answered.notify_all();
    }

    /// The state, recovering from a panic in another holder.
    ///
    /// A poisoned lock here means a thread panicked while holding it, which
    /// costs at most one screenshot. Refusing every screenshot afterwards
    /// because of it would be a recording-long failure caused by a moment's
    /// one, and this data has no invariant a panic could have broken halfway
    /// (AGENTS.md section 16).
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
