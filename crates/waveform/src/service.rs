//! Where waveform generation runs, and what stops it competing with a
//! recording.
//!
//! # The requirement
//!
//! AGENTS.md section 18: this application runs alongside games. Generating
//! waveforms for a library of recordings is the archetypal background job — it
//! reads whole files from the same disk a recording is being written to, and it
//! decodes audio on the same processor the game is running on. Issue #66 asks
//! for it to happen "asynchronously and at low priority after recording", and
//! for that to be measured rather than asserted.
//!
//! # Where it runs
//!
//! On the one low-priority thread [`clipped_background::Worker`] owns, reading
//! from the bounded queue it holds — the same worker the thumbnail module of
//! `clipped-library` runs on
//! ([issue #293](https://github.com/wildware-uk/clipped/issues/293)).
//! What is specific to a waveform is the `process` closure
//! [`WaveformService::start`] hands it: reading a [`SourceIdentity`], calling
//! [`analyse_paced`], writing the `.cwf` cache, and deciding what to log.
//! `crates/background/src/lib.rs` documents the queue bound, the suspension
//! contract and the thread priority the worker applies; this module does not
//! repeat that.
//!
//! The intended host is the recorder process, which is the process that already
//! knows when a recording is running and can therefore call
//! [`suspend_for_recording`](WaveformService::suspend_for_recording) truthfully.
//! Nothing hosts it yet: the timeline (issue #65) and the clip editor (issue
//! #83) are the consumers, and neither exists.
//!
//! # Threading model
//!
//! [`WaveformService`] is `Send + Sync` and every method may be called from any
//! thread. The worker thread owns the FFmpeg resources and the callback; nothing
//! else touches them. The callback runs **on the worker thread**, so a host that
//! wants to touch a user interface from it must post to its own loop — and a
//! callback that blocks blocks waveform generation, not the caller.
//!
//! Dropping the service stops the worker and waits for it, so no thread outlives
//! the value that created it.

use std::path::{Path, PathBuf};

use clipped_background::{Outcome, RequestOutcome, SourceIdentity, Worker, WorkerPriority};
use tracing::{debug, info, warn};

use crate::analyse::analyse_paced;
use crate::cache::WaveformCache;
use crate::waveform::WaveformState;
use crate::WaveformError;

/// How many recordings may be waiting at once.
///
/// A library scan can offer thousands; this is what keeps the queue a fixed
/// cost. When it is full the **oldest** waiting request is dropped, because the
/// newest is the recording somebody just looked at, and because a dropped
/// request costs nothing — asking again is one call, and the peaks were never
/// there to lose.
pub const DEFAULT_QUEUE_CAPACITY: usize = 64;

/// The name the worker thread carries in a debugger and in a crash dump.
const WORKER_THREAD_NAME: &str = "clipped-waveform";

/// One finished attempt, handed to the callback on the worker thread.
pub type Completion = clipped_background::Completion<WaveformState>;

/// How a [`WaveformService`] is set up.
pub struct ServiceOptions {
    queue_capacity: usize,
    on_finished: Option<Box<dyn FnMut(Completion) + Send>>,
}

impl ServiceOptions {
    /// The defaults: [`DEFAULT_QUEUE_CAPACITY`] and no callback.
    #[must_use]
    pub fn new() -> Self {
        Self {
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            on_finished: None,
        }
    }

    /// How many recordings may be waiting at once. Never zero.
    #[must_use]
    pub fn with_queue_capacity(mut self, capacity: usize) -> Self {
        self.queue_capacity = capacity.max(1);
        self
    }

    /// Called on the worker thread when a recording has been analysed.
    ///
    /// Optional: a host that polls [`WaveformService::waveform`] needs no
    /// callback at all.
    #[must_use]
    pub fn on_finished(mut self, callback: impl FnMut(Completion) + Send + 'static) -> Self {
        self.on_finished = Some(Box::new(callback));
        self
    }
}

impl Default for ServiceOptions {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Debug for ServiceOptions {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("ServiceOptions")
            .field("queue_capacity", &self.queue_capacity)
            .field("on_finished", &self.on_finished.is_some())
            .finish()
    }
}

/// Generates waveforms in the background, at a priority a game does not notice.
///
/// See the module documentation for where it runs and what bounds it.
#[derive(Debug)]
pub struct WaveformService {
    worker: Worker,
    cache: WaveformCache,
}

impl WaveformService {
    /// Starts the worker.
    #[must_use]
    pub fn start(cache: WaveformCache, options: ServiceOptions) -> Self {
        let worker_cache = cache.clone();
        let mut on_finished = options.on_finished;

        let worker = Worker::start(
            WORKER_THREAD_NAME,
            options.queue_capacity,
            move |recording: PathBuf, pace| {
                let redacted = clipped_logging::RedactedPath::new(&recording);
                // Read before the analysis rather than after, so that a
                // recording rewritten while it was being read is not
                // remembered as a failure of the file that replaced it. The
                // entry then describes the version that failed,
                // `still_describes` refuses it, and the new file is analysed.
                let before = SourceIdentity::of(&recording).ok();
                let state = match analyse_paced(&recording, pace) {
                    Ok(waveform) => {
                        if let Err(error) = worker_cache.store(&waveform) {
                            // Not fatal, and not the caller's problem: the
                            // peaks in hand are still correct, they just have
                            // to be computed again next time.
                            warn!(recording = %redacted, error = %error, "a waveform could not be cached");
                        }
                        debug!(
                            recording = %redacted,
                            tracks = waveform.tracks().len(),
                            "generated a waveform"
                        );
                        WaveformState::Ready(waveform)
                    }
                    Err(WaveformError::Cancelled) => {
                        // Shutdown. The request is gone with the queue;
                        // nothing is reported, because nothing finished.
                        debug!(recording = %redacted, "waveform generation was stopped");
                        return Outcome::Cancelled;
                    }
                    Err(error) => {
                        debug!(recording = %redacted, error = %error, "no waveform for this recording");
                        // Written down, or this file is re-read from end to
                        // end on every lookup for ever: `waveform`
                        // re-requests anything the cache calls `Pending`, and
                        // an entry that was never stored is `Pending` for
                        // good. Only when the recording could be stat-ed — an
                        // entry keyed on nothing would match nothing.
                        if let Some(source) = &before {
                            if let Err(cause) = worker_cache.remember_failure(source, &error) {
                                warn!(
                                    recording = %redacted,
                                    error = %cause,
                                    "a failed analysis could not be cached, so it will be attempted again"
                                );
                            }
                        }
                        WaveformState::Unavailable(error)
                    }
                };

                if let Some(callback) = on_finished.as_mut() {
                    callback(Completion { recording, state });
                }
                Outcome::Finished
            },
        );

        Self { worker, cache }
    }

    /// The waveform of a recording, generating it if there is not one.
    ///
    /// This is the call a timeline makes. It never fails and never blocks on the
    /// worker: [`WaveformState::Pending`] means "not yet, and it is being worked
    /// on", which a timeline draws as a track with no waveform in it.
    #[must_use]
    pub fn waveform(&self, recording: impl AsRef<Path>) -> WaveformState {
        let recording = recording.as_ref();
        let state = self.cache.lookup(recording);
        if matches!(state, WaveformState::Pending) {
            self.request(recording);
        }
        state
    }

    /// Asks for a recording to be analysed, without reading the cache first.
    ///
    /// For a library scan, which has thousands of files and does not want the
    /// peaks of any of them yet.
    pub fn request(&self, recording: impl AsRef<Path>) -> RequestOutcome {
        let outcome = self.worker.request(recording);
        if let RequestOutcome::QueuedInPlaceOf(dropped) = &outcome {
            debug!(
                dropped = %clipped_logging::RedactedPath::new(dropped),
                capacity = self.worker.capacity(),
                "the waveform queue was full, so the oldest waiting recording was dropped"
            );
        }
        outcome
    }

    /// Stops generation until [`resume`](Self::resume) is called.
    ///
    /// Takes effect inside the current packet: a recording being analysed is
    /// paused, not abandoned. Nesting is not counted — this is a state, not a
    /// lock — so a host that calls it twice resumes with one call.
    pub fn suspend_for_recording(&self) {
        if self.worker.suspend_for_recording() {
            info!("waveform generation suspended while a recording is running");
        }
    }

    /// Lets generation carry on.
    pub fn resume(&self) {
        if self.worker.resume() {
            info!("waveform generation resumed");
        }
    }

    /// Whether generation is suspended.
    #[must_use]
    pub fn is_suspended(&self) -> bool {
        self.worker.is_suspended()
    }

    /// How many recordings are waiting.
    #[must_use]
    pub fn queued(&self) -> usize {
        self.worker.queued()
    }

    /// How many analyses have finished, successfully or not.
    #[must_use]
    pub fn finished(&self) -> u64 {
        self.worker.finished()
    }

    /// What the worker thread's priority actually became, once it has started.
    ///
    /// [`None`] until the worker has run its first instruction, and on a
    /// platform where no priority was applied.
    #[must_use]
    pub fn worker_priority(&self) -> Option<WorkerPriority> {
        self.worker.worker_priority()
    }

    /// The cache this service reads and writes.
    #[must_use]
    pub fn cache(&self) -> &WaveformCache {
        &self.cache
    }

    /// Stops the worker and waits for it.
    ///
    /// Equivalent to dropping the service; both exist because a host that wants
    /// to know when the thread has gone should not have to rely on a drop.
    pub fn shutdown(self) {
        self.worker.shutdown();
    }
}
