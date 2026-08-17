//! Where thumbnail generation runs, and what stops it competing with a
//! recording.
//!
//! # The requirement
//!
//! AGENTS.md section 18: this application runs alongside games. Generating
//! thumbnails for a library of recordings is the archetypal background job — it
//! seeks about in files on the same disk a recording is being written to, and it
//! decodes video on the same processor the game is running on. Issue #57 asks
//! for it "asynchronously and at low priority", never "during active capture at
//! a priority that affects gameplay", and "deferred until the session ends where
//! practical".
//!
//! # Where it runs
//!
//! On the one low-priority thread [`clipped_background::Worker`] owns, reading
//! from the bounded queue it holds — the same worker `clipped-waveform` runs on
//! ([issue #293](https://github.com/wildware-uk/clipped/issues/293)). What is
//! specific to a thumbnail is the `process` closure
//! [`ThumbnailService::start`] hands it: reading a [`SourceIdentity`], calling
//! [`render_paced`], writing the JPEG-and-JSON cache, and deciding what to log.
//! `crates/background/src/lib.rs` documents the queue bound, the suspension
//! contract and the thread priority the worker applies; this module does not
//! repeat that.
//!
//! The intended host is the recorder process, which is the process that already
//! knows when a recording is running and can therefore call
//! [`suspend_for_recording`](ThumbnailService::suspend_for_recording)
//! truthfully. Nothing hosts it yet: the library screen
//! ([issue #52](https://github.com/wildware-uk/clipped/issues/52)) is the
//! consumer and it is not built.
//!
//! # Threading model
//!
//! [`ThumbnailService`] is `Send + Sync` and every method may be called from any
//! thread. The worker thread owns the FFmpeg resources and the callback; nothing
//! else touches them. The callback runs **on the worker thread**, so a host that
//! wants to touch a user interface from it must post to its own loop — and a
//! callback that blocks blocks thumbnail generation, not the caller.
//!
//! Dropping the service stops the worker and waits for it, so no thread outlives
//! the value that created it.

use std::path::{Path, PathBuf};

use clipped_background::{Outcome, RequestOutcome, SourceIdentity, Worker, WorkerPriority};
use tracing::{debug, info, warn};

use super::cache::{ThumbnailCache, ThumbnailState};
use super::render::{render_paced, ThumbnailOptions};
use super::ThumbnailError;

/// How many recordings may be waiting at once.
///
/// A library scan can offer thousands; this is what keeps the queue a fixed
/// cost. When it is full the **oldest** waiting request is dropped, because the
/// newest is the recording somebody just scrolled to, and because a dropped
/// request costs nothing — asking again is one call, and the picture was never
/// there to lose.
pub const DEFAULT_QUEUE_CAPACITY: usize = 128;

/// The name the worker thread carries in a debugger and in a crash dump.
const WORKER_THREAD_NAME: &str = "clipped-thumbnails";

/// One finished attempt, handed to the callback on the worker thread.
pub type Completion = clipped_background::Completion<ThumbnailState>;

/// How a [`ThumbnailService`] is set up.
pub struct ServiceOptions {
    queue_capacity: usize,
    thumbnail: ThumbnailOptions,
    on_finished: Option<Box<dyn FnMut(Completion) + Send>>,
}

impl ServiceOptions {
    /// The defaults: [`DEFAULT_QUEUE_CAPACITY`], [`ThumbnailOptions::new`] and
    /// no callback.
    #[must_use]
    pub fn new() -> Self {
        Self {
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            thumbnail: ThumbnailOptions::new(),
            on_finished: None,
        }
    }

    /// How many recordings may be waiting at once. Never zero.
    #[must_use]
    pub fn with_queue_capacity(mut self, capacity: usize) -> Self {
        self.queue_capacity = capacity.max(1);
        self
    }

    /// How the pictures are made.
    #[must_use]
    pub fn with_thumbnails(mut self, options: ThumbnailOptions) -> Self {
        self.thumbnail = options;
        self
    }

    /// Called on the worker thread when a recording has been looked at.
    ///
    /// Optional: a host that polls [`ThumbnailService::thumbnail`] needs no
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
            .field("thumbnail", &self.thumbnail)
            .field("on_finished", &self.on_finished.is_some())
            .finish()
    }
}

/// Makes thumbnails in the background, at a priority a game does not notice.
///
/// See the module documentation for where it runs and what bounds it.
#[derive(Debug)]
pub struct ThumbnailService {
    worker: Worker,
    cache: ThumbnailCache,
}

impl ThumbnailService {
    /// Starts the worker.
    #[must_use]
    pub fn start(cache: ThumbnailCache, options: ServiceOptions) -> Self {
        let worker_cache = cache.clone();
        let thumbnail = options.thumbnail;
        let mut on_finished = options.on_finished;

        let worker = Worker::start(
            WORKER_THREAD_NAME,
            options.queue_capacity,
            move |recording: PathBuf, pace| {
                let redacted = clipped_logging::RedactedPath::new(&recording);
                // Read before the attempt rather than after, so that a
                // recording rewritten while it was being read is not
                // remembered as a failure of the file that replaced it. The
                // entry then describes the version that failed,
                // `still_describes` refuses it, and the new file is
                // attempted.
                let before = SourceIdentity::of(&recording).ok();
                let state = match render_paced(&recording, thumbnail, pace) {
                    Ok(rendered) => match worker_cache.store(&rendered) {
                        Ok(thumbnail) => {
                            debug!(
                                recording = %redacted,
                                blank = thumbnail.is_blank(),
                                "generated a thumbnail"
                            );
                            ThumbnailState::Ready(thumbnail)
                        }
                        Err(error) => {
                            // Not fatal and not the caller's problem: the
                            // picture was made, it just could not be kept. It
                            // is reported as unavailable rather than ready
                            // because there is no file to hand a screen — a
                            // `Thumbnail` is a path, and this is the one path
                            // there is not.
                            warn!(recording = %redacted, error = %error, "a thumbnail could not be cached");
                            ThumbnailState::Unavailable(error)
                        }
                    },
                    Err(ThumbnailError::Cancelled) => {
                        // Shutdown. The request went with the queue; nothing
                        // is reported, because nothing finished.
                        debug!(recording = %redacted, "thumbnail generation was stopped");
                        return Outcome::Cancelled;
                    }
                    Err(error) => {
                        debug!(recording = %redacted, error = %error, "no thumbnail for this recording");
                        // Written down, or this file is decoded again on
                        // every lookup for ever: `thumbnail` re-requests
                        // anything the cache calls `Pending`, and an entry
                        // that was never stored is `Pending` for good. Only
                        // when the recording could be stat-ed — an entry
                        // keyed on nothing would match nothing.
                        if let Some(source) = &before {
                            if let Err(cause) = worker_cache.remember_failure(source, &error) {
                                warn!(
                                    recording = %redacted,
                                    error = %cause,
                                    "a failed attempt could not be cached, so it will be made again"
                                );
                            }
                        }
                        ThumbnailState::Unavailable(error)
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

    /// The thumbnail of a recording, making one if there is not one.
    ///
    /// This is the call a library screen makes per tile. It never fails and
    /// never blocks on the worker: [`ThumbnailState::Pending`] means "not yet,
    /// and it is being worked on", which a tile draws as a tile with no picture.
    #[must_use]
    pub fn thumbnail(&self, recording: impl AsRef<Path>) -> ThumbnailState {
        let recording = recording.as_ref();
        let state = self.cache.lookup(recording);
        if matches!(state, ThumbnailState::Pending) {
            self.request(recording);
        }
        state
    }

    /// Asks for a recording to be looked at, without reading the cache first.
    ///
    /// For a library scan, which has thousands of files and does not want the
    /// picture of any of them yet.
    pub fn request(&self, recording: impl AsRef<Path>) -> RequestOutcome {
        let outcome = self.worker.request(recording);
        if let RequestOutcome::QueuedInPlaceOf(dropped) = &outcome {
            debug!(
                dropped = %clipped_logging::RedactedPath::new(dropped),
                capacity = self.worker.capacity(),
                "the thumbnail queue was full, so the oldest waiting recording was dropped"
            );
        }
        outcome
    }

    /// Stops generation until [`resume`](Self::resume) is called.
    ///
    /// Takes effect inside the recording being looked at: it is paused, not
    /// abandoned. Nesting is not counted — this is a state, not a lock — so a
    /// host that calls it twice resumes with one call.
    pub fn suspend_for_recording(&self) {
        if self.worker.suspend_for_recording() {
            info!("thumbnail generation suspended while a recording is running");
        }
    }

    /// Lets generation carry on.
    pub fn resume(&self) {
        if self.worker.resume() {
            info!("thumbnail generation resumed");
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

    /// How many attempts have finished, successfully or not.
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
    pub fn cache(&self) -> &ThumbnailCache {
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
