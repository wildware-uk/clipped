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
//! On **one** thread that this service creates, and nowhere else.
//!
//! - One, not a pool. The work is a seek and a handful of decoded frames, and a
//!   second thread would double the disk queue depth of a job whose whole
//!   purpose is to stay out of the way.
//! - Created here rather than borrowed from the caller, so that the priority
//!   below applies to a thread this crate owns (AGENTS.md section 20 — no
//!   uncontrolled thread creation, and nothing hidden inside a caller's thread).
//!
//! The intended host is the recorder process, which is the process that already
//! knows when a recording is running and can therefore call
//! [`suspend_for_recording`](ThumbnailService::suspend_for_recording)
//! truthfully. Nothing hosts it yet: the library screen
//! ([issue #52](https://github.com/wildware-uk/clipped/issues/52)) is the
//! consumer and it is not built.
//!
//! # What bounds it
//!
//! | Bound | Value | Why |
//! | --- | --- | --- |
//! | Threads | 1 | above |
//! | Queue | [`DEFAULT_QUEUE_CAPACITY`] paths | a library scan must not turn into an unbounded allocation |
//! | Work per recording | one seek and at most 12 decoded frames | `super::choose` |
//! | Thread priority | `THREAD_PRIORITY_LOWEST` | run only when nothing else wants the processor |
//! | I/O priority | `THREAD_MODE_BACKGROUND_BEGIN` | reads must not take disk bandwidth from a recording |
//! | Suspension | while a recording is running | see below |
//!
//! Priority is not the whole answer, because a background-priority thread still
//! runs when a game is waiting on something other than the processor. So the
//! service can also be **suspended**, which is the "deferred until the session
//! ends" half of the requirement: while it is, the worker stops between packets
//! and does not resume until it is told to. A host suspends it when a recording
//! starts and resumes when the recording ends. Suspension is a real stop, not a
//! hint: the worker blocks, and [`is_suspended`](ThumbnailService::is_suspended)
//! reports it.
//!
//! What the thread priority actually became is reported by
//! [`worker_priority`](ThumbnailService::worker_priority), read back from
//! Windows rather than assumed, because a control that silently does nothing is
//! worse than no control (AGENTS.md section 27).
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

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;

use tracing::{debug, info, warn};

use super::cache::{ThumbnailCache, ThumbnailState};
use super::render::{render_paced, Continue, Pace, ThumbnailOptions};
use super::source::SourceIdentity;
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

/// What happened to a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestOutcome {
    /// Waiting to be made.
    Queued,
    /// Already waiting; nothing was added.
    AlreadyQueued,
    /// Waiting, and the oldest request was dropped to make room.
    QueuedInPlaceOf(PathBuf),
    /// The service is shutting down and took nothing.
    Stopped,
}

/// One finished attempt, handed to the callback on the worker thread.
#[derive(Debug)]
pub struct Completion {
    /// The recording that was looked at.
    pub recording: PathBuf,
    /// [`ThumbnailState::Ready`] or [`ThumbnailState::Unavailable`]; never
    /// [`ThumbnailState::Pending`], because this is what "no longer pending"
    /// looks like.
    pub state: ThumbnailState,
}

/// What the worker thread's priority actually became.
///
/// Read back from the operating system rather than inferred from the calls that
/// were made, so that "thumbnail generation runs at background priority" is
/// something this crate can be asked to prove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerPriority {
    lowest: bool,
    background_mode: bool,
    observed: i32,
}

impl WorkerPriority {
    pub(super) fn new(lowest: bool, background_mode: bool, observed: i32) -> Self {
        Self {
            lowest,
            background_mode,
            observed,
        }
    }

    /// Whether the thread took the lowest scheduling priority, as the operating
    /// system reported it back.
    #[must_use]
    pub fn is_lowest(&self) -> bool {
        self.lowest
    }

    /// Whether the thread entered background mode, which lowers its disk I/O
    /// priority as well as its scheduling priority.
    #[must_use]
    pub fn background_mode(&self) -> bool {
        self.background_mode
    }

    /// What `GetThreadPriority` reports for the thread now.
    ///
    /// Not the same number as `THREAD_PRIORITY_LOWEST` once background mode has
    /// been entered: Windows then reports the background priority, which is
    /// lower still. [`is_lowest`](Self::is_lowest) is the answer to "did the
    /// scheduling priority take"; this is the raw value, for a log.
    #[must_use]
    pub fn observed(&self) -> i32 {
        self.observed
    }
}

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

/// Everything the worker and its callers share.
#[derive(Debug)]
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    capacity: usize,
}

#[derive(Debug, Default)]
struct State {
    queue: VecDeque<PathBuf>,
    /// What the worker is looking at right now, so a second request for it is
    /// recognised as a duplicate rather than queued behind itself.
    working_on: Option<PathBuf>,
    suspended: bool,
    stopping: bool,
    /// What the worker's priority turned out to be, once it has started.
    priority: Option<WorkerPriority>,
    /// How many attempts have finished, successfully or not. Diagnostics, and
    /// what a test waits on.
    finished: u64,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // A poisoned lock means a previous holder panicked. Nothing here is a
        // half-updated invariant — a queue of paths and four flags — and
        // refusing to make thumbnails for the rest of the process's life would
        // be a much worse outcome than carrying on.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The [`Pace`] the renderer checks: suspension and shutdown, as one question.
impl Pace for Shared {
    fn checkpoint(&self) -> Continue {
        let mut state = self.lock();
        while state.suspended && !state.stopping {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        if state.stopping {
            Continue::Stop
        } else {
            Continue::Yes
        }
    }
}

/// Makes thumbnails in the background, at a priority a game does not notice.
///
/// See the module documentation for where it runs and what bounds it.
#[derive(Debug)]
pub struct ThumbnailService {
    shared: Arc<Shared>,
    cache: ThumbnailCache,
    worker: Option<JoinHandle<()>>,
}

impl ThumbnailService {
    /// Starts the worker.
    #[must_use]
    pub fn start(cache: ThumbnailCache, options: ServiceOptions) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            capacity: options.queue_capacity,
        });

        let worker_shared = Arc::clone(&shared);
        let worker_cache = cache.clone();
        let thumbnail = options.thumbnail;
        let on_finished = options.on_finished;
        let worker = std::thread::Builder::new()
            .name(WORKER_THREAD_NAME.to_owned())
            .spawn(move || run(&worker_shared, &worker_cache, thumbnail, on_finished))
            .map_err(|error| {
                // A machine that cannot start a thread is in serious trouble,
                // and it is still not worth failing over: every method below
                // degrades to "nothing is ever made", which is the state a
                // library screen already has to handle.
                warn!(
                    error = %error,
                    "the thumbnail worker could not be started; thumbnails will not be generated"
                );
            })
            .ok();

        Self {
            shared,
            cache,
            worker,
        }
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
        let recording = recording.as_ref().to_path_buf();
        let outcome = {
            let mut state = self.shared.lock();
            if state.stopping {
                RequestOutcome::Stopped
            } else if state.working_on.as_ref() == Some(&recording)
                || state.queue.contains(&recording)
            {
                RequestOutcome::AlreadyQueued
            } else {
                let dropped = if state.queue.len() >= self.shared.capacity {
                    state.queue.pop_front()
                } else {
                    None
                };
                state.queue.push_back(recording);
                dropped.map_or(RequestOutcome::Queued, RequestOutcome::QueuedInPlaceOf)
            }
        };

        if let RequestOutcome::QueuedInPlaceOf(dropped) = &outcome {
            debug!(
                dropped = %clipped_logging::RedactedPath::new(dropped),
                capacity = self.shared.capacity,
                "the thumbnail queue was full, so the oldest waiting recording was dropped"
            );
        }
        self.shared.changed.notify_all();
        outcome
    }

    /// Stops generation until [`resume`](Self::resume) is called.
    ///
    /// Takes effect inside the recording being looked at: it is paused, not
    /// abandoned. Nesting is not counted — this is a state, not a lock — so a
    /// host that calls it twice resumes with one call.
    pub fn suspend_for_recording(&self) {
        let already = {
            let mut state = self.shared.lock();
            let already = state.suspended;
            state.suspended = true;
            already
        };
        if !already {
            info!("thumbnail generation suspended while a recording is running");
        }
        self.shared.changed.notify_all();
    }

    /// Lets generation carry on.
    pub fn resume(&self) {
        let was = {
            let mut state = self.shared.lock();
            let was = state.suspended;
            state.suspended = false;
            was
        };
        if was {
            info!("thumbnail generation resumed");
        }
        self.shared.changed.notify_all();
    }

    /// Whether generation is suspended.
    #[must_use]
    pub fn is_suspended(&self) -> bool {
        self.shared.lock().suspended
    }

    /// How many recordings are waiting.
    #[must_use]
    pub fn queued(&self) -> usize {
        self.shared.lock().queue.len()
    }

    /// How many attempts have finished, successfully or not.
    #[must_use]
    pub fn finished(&self) -> u64 {
        self.shared.lock().finished
    }

    /// What the worker thread's priority actually became, once it has started.
    ///
    /// [`None`] until the worker has run its first instruction, and on a
    /// platform where no priority was applied.
    #[must_use]
    pub fn worker_priority(&self) -> Option<WorkerPriority> {
        self.shared.lock().priority
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
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        {
            let mut state = self.shared.lock();
            state.stopping = true;
            state.queue.clear();
        }
        self.shared.changed.notify_all();
        if let Some(worker) = self.worker.take() {
            // A panicking worker is reported by the thread itself; joining it
            // here is about not outliving it.
            let _ = worker.join();
        }
    }
}

impl Drop for ThumbnailService {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The worker thread.
fn run(
    shared: &Arc<Shared>,
    cache: &ThumbnailCache,
    options: ThumbnailOptions,
    mut on_finished: Option<Box<dyn FnMut(Completion) + Send>>,
) {
    let priority = enter_background();
    {
        let mut state = shared.lock();
        state.priority = Some(priority);
    }
    shared.changed.notify_all();
    debug!(
        lowest = priority.is_lowest(),
        background_mode = priority.background_mode(),
        observed = priority.observed(),
        "the thumbnail worker is running in the background"
    );

    while let Some(recording) = next(shared) {
        let redacted = clipped_logging::RedactedPath::new(&recording);
        // Read before the attempt rather than after, so that a recording
        // rewritten while it was being read is not remembered as a failure of
        // the file that replaced it. The entry then describes the version that
        // failed, `still_describes` refuses it, and the new file is attempted.
        let before = SourceIdentity::of(&recording).ok();
        let state = match render_paced(&recording, options, shared.as_ref()) {
            Ok(rendered) => match cache.store(&rendered) {
                Ok(thumbnail) => {
                    debug!(
                        recording = %redacted,
                        blank = thumbnail.is_blank(),
                        "generated a thumbnail"
                    );
                    ThumbnailState::Ready(thumbnail)
                }
                Err(error) => {
                    // Not fatal and not the caller's problem: the picture was
                    // made, it just could not be kept. It is reported as
                    // unavailable rather than ready because there is no file to
                    // hand a screen — a `Thumbnail` is a path, and this is the
                    // one path there is not.
                    warn!(recording = %redacted, error = %error, "a thumbnail could not be cached");
                    ThumbnailState::Unavailable(error)
                }
            },
            Err(ThumbnailError::Cancelled) => {
                // Shutdown. The request went with the queue; nothing is
                // reported, because nothing finished.
                debug!(recording = %redacted, "thumbnail generation was stopped");
                break;
            }
            Err(error) => {
                debug!(recording = %redacted, error = %error, "no thumbnail for this recording");
                // Written down, or this file is decoded again on every lookup
                // for ever: `thumbnail` re-requests anything the cache calls
                // `Pending`, and an entry that was never stored is `Pending` for
                // good. Only when the recording could be stat-ed — an entry
                // keyed on nothing would match nothing.
                if let Some(source) = &before {
                    if let Err(cause) = cache.remember_failure(source, &error) {
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

        {
            let mut locked = shared.lock();
            locked.working_on = None;
            locked.finished += 1;
        }
        shared.changed.notify_all();

        if let Some(callback) = on_finished.as_mut() {
            callback(Completion { recording, state });
        }
    }

    leave_background();
}

/// Waits for something to do, or for a shutdown.
fn next(shared: &Arc<Shared>) -> Option<PathBuf> {
    let mut state = shared.lock();
    loop {
        if state.stopping {
            return None;
        }
        if !state.suspended {
            if let Some(recording) = state.queue.pop_front() {
                state.working_on = Some(recording.clone());
                return Some(recording);
            }
        }
        state = shared
            .changed
            .wait(state)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
}

/// Lowers this thread's scheduling and I/O priority, where the platform has a
/// way to.
fn enter_background() -> WorkerPriority {
    #[cfg(windows)]
    {
        super::windows::priority::enter()
    }
    #[cfg(not(windows))]
    {
        // Clipped targets Windows (README, "Supported platforms"); everything
        // above the `windows` module still builds and tests elsewhere, and says
        // plainly that it lowered nothing rather than claiming it did.
        WorkerPriority::new(false, false, 0)
    }
}

/// Undoes [`enter_background`].
fn leave_background() {
    #[cfg(windows)]
    {
        super::windows::priority::leave();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::time::Duration;
    use std::sync::mpsc;

    fn shared() -> Arc<Shared> {
        Arc::new(Shared {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            capacity: DEFAULT_QUEUE_CAPACITY,
        })
    }

    /// Runs `checkpoint` on another thread and reports whether it came back.
    fn checkpoint_in_the_background(shared: &Arc<Shared>) -> mpsc::Receiver<Continue> {
        let (answered, answers) = mpsc::channel();
        let shared = Arc::clone(shared);
        std::thread::spawn(move || {
            let _ = answered.send(shared.checkpoint());
        });
        answers
    }

    #[test]
    fn a_checkpoint_blocks_while_generation_is_suspended() {
        // The mechanism the renderer calls between packets. If this returned
        // straight away, suspension would be a flag nobody reads and a library
        // scan would decode right through a recording.
        let shared = shared();
        shared.lock().suspended = true;
        let answers = checkpoint_in_the_background(&shared);

        assert_eq!(
            answers.recv_timeout(Duration::from_millis(500)),
            Err(mpsc::RecvTimeoutError::Timeout),
            "a checkpoint returned while generation was suspended"
        );

        shared.lock().suspended = false;
        shared.changed.notify_all();
        assert_eq!(
            answers
                .recv_timeout(Duration::from_secs(10))
                .expect("the checkpoint returns once generation resumes"),
            Continue::Yes
        );
    }

    #[test]
    fn a_checkpoint_asks_for_a_stop_when_the_service_is_shutting_down() {
        let shared = shared();
        assert_eq!(shared.checkpoint(), Continue::Yes);

        // Shutting down while suspended has to break the wait rather than
        // deadlock inside it, which is the case a host hits every time it quits
        // during a recording.
        shared.lock().suspended = true;
        let answers = checkpoint_in_the_background(&shared);
        {
            let mut state = shared.lock();
            state.stopping = true;
        }
        shared.changed.notify_all();

        assert_eq!(
            answers
                .recv_timeout(Duration::from_secs(10))
                .expect("the checkpoint returns when the service stops"),
            Continue::Stop
        );
    }

    #[test]
    fn the_worker_takes_nothing_off_the_queue_while_generation_is_suspended() {
        let shared = shared();
        {
            let mut state = shared.lock();
            state.suspended = true;
            state.queue.push_back(PathBuf::from("waiting.mkv"));
        }

        let taken = std::thread::spawn({
            let shared = Arc::clone(&shared);
            move || next(&shared)
        });
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            shared.lock().queue.len(),
            1,
            "the worker took work while generation was suspended"
        );

        shared.lock().suspended = false;
        shared.changed.notify_all();
        assert_eq!(
            taken.join().expect("the worker thread"),
            Some(PathBuf::from("waiting.mkv"))
        );
    }
}
