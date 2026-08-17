//! The queue, the thread and the suspension mechanism a background reader
//! runs behind.
//!
//! See the crate documentation for why this exists and what it deliberately
//! does not do: everything domain-specific — reading a [`crate::SourceIdentity`],
//! generating a waveform or a thumbnail, writing a cache, deciding what to log
//! — is the `process` closure [`Worker::start`] takes, supplied by
//! `clipped-waveform` and by the thumbnail module of `clipped-library`.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;

use tracing::{debug, warn};

use crate::pace::{Continue, Pace};

/// What happened to a request to work on one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestOutcome {
    /// Waiting to be processed.
    Queued,
    /// Already waiting; nothing was added.
    AlreadyQueued,
    /// Waiting, and the oldest request was dropped to make room.
    QueuedInPlaceOf(PathBuf),
    /// The worker is shutting down and took nothing.
    Stopped,
}

/// One finished attempt, handed to a caller's own callback.
///
/// Generic over `S`, what "finished" means to the caller — a waveform crate's
/// `WaveformState` or the thumbnail module's `ThumbnailState`, neither of
/// which this crate can name — because this crate does not know what kind of
/// work it is running. The shape, which path and what came of it, is the one
/// thing every caller's callback needs regardless.
#[derive(Debug)]
pub struct Completion<S> {
    /// The recording that was worked on.
    pub recording: PathBuf,
    /// What became of it. Never the caller's own "pending" state, because
    /// this is what "no longer pending" looks like.
    pub state: S,
}

/// What the worker thread's priority actually became.
///
/// Read back from the operating system rather than inferred from the calls
/// that were made, so that "this runs at background priority" is something a
/// caller can be asked to prove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerPriority {
    lowest: bool,
    background_mode: bool,
    observed: i32,
}

impl WorkerPriority {
    pub(crate) fn new(lowest: bool, background_mode: bool, observed: i32) -> Self {
        Self {
            lowest,
            background_mode,
            observed,
        }
    }

    /// Whether the thread took the lowest scheduling priority, as the
    /// operating system reported it back.
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
    /// Not the same number as `THREAD_PRIORITY_LOWEST` once background mode
    /// has been entered: Windows then reports the background priority, which
    /// is lower still. [`is_lowest`](Self::is_lowest) is the answer to "did
    /// the scheduling priority take"; this is the raw value, for a log.
    #[must_use]
    pub fn observed(&self) -> i32 {
        self.observed
    }
}

/// What one item of work produced, from [`Worker::start`]'s `process`
/// closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The item was processed to completion, successfully or not. Any
    /// reporting specific to it — caching the result, calling a caller's own
    /// callback — has already happened inside the closure.
    Finished,
    /// The worker was told to stop while this item was being processed.
    /// Nothing is reported for it, because nothing finished; the worker loop
    /// ends without taking anything else off the queue.
    Cancelled,
}

/// Everything the worker thread and its callers share.
#[derive(Debug)]
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    capacity: usize,
}

#[derive(Debug, Default)]
struct State {
    queue: VecDeque<PathBuf>,
    /// What the worker is processing right now, so a second request for it is
    /// recognised as a duplicate rather than queued behind itself.
    working_on: Option<PathBuf>,
    suspended: bool,
    stopping: bool,
    /// What the worker's priority turned out to be, once it has started.
    priority: Option<WorkerPriority>,
    /// How many items have finished, successfully or not. Diagnostics, and
    /// what a test waits on.
    finished: u64,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // A poisoned lock means a previous holder panicked. Nothing here is a
        // half-updated invariant — a queue of paths and four flags — and
        // refusing to process anything for the rest of the process's life
        // would be a much worse outcome than carrying on.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The [`Pace`] a `process` closure is handed: suspension and shutdown, as
/// one question.
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

/// A single low-priority background thread, reading paths off a bounded
/// queue that drops the oldest when full, suspendable for the duration of a
/// recording.
///
/// See the crate documentation for the requirement this meets and for why
/// what to do with each path is the caller's `process` closure rather than
/// anything this type knows about.
///
/// # Threading model
///
/// [`Worker`] is `Send + Sync` and every method may be called from any
/// thread. The `process` closure runs **on the worker thread**, and nothing
/// else calls it — so a caller that wants to touch a user interface from
/// inside it must post to its own loop, and a closure that blocks blocks this
/// worker, not the caller.
///
/// Dropping a [`Worker`] stops the thread and waits for it, so no thread
/// outlives the value that created it.
#[derive(Debug)]
pub struct Worker {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    /// Starts the worker thread.
    ///
    /// `thread_name` is what the thread carries in a debugger and a crash
    /// dump, and what identifies it in the log line reporting the priority it
    /// took. `queue_capacity` is clamped to never be zero. `process` runs on
    /// the worker thread for every path taken off the queue: it receives the
    /// path and the [`Pace`] this worker itself implements — which blocks a
    /// checkpoint while suspended — and reports whether the item finished or
    /// the worker was told to stop mid-item.
    #[must_use]
    pub fn start(
        thread_name: impl Into<String>,
        queue_capacity: usize,
        mut process: impl FnMut(PathBuf, &dyn Pace) -> Outcome + Send + 'static,
    ) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            capacity: queue_capacity.max(1),
        });

        let name = thread_name.into();
        let worker_shared = Arc::clone(&shared);
        let run_name = name.clone();
        let error_name = name.clone();
        let thread = std::thread::Builder::new()
            .name(name)
            .spawn(move || run(&worker_shared, &run_name, &mut process))
            .map_err(|error| {
                // A machine that cannot start a thread is in serious trouble,
                // and it is still not worth failing over: every method below
                // degrades to "nothing is ever processed", which is a state
                // every caller of this crate already has to handle for an
                // empty cache.
                warn!(
                    error = %error,
                    worker = %error_name,
                    "a background worker could not be started; its queue will never be drained"
                );
            })
            .ok();

        Self { shared, thread }
    }

    /// How many paths may be waiting at once. Never zero.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.shared.capacity
    }

    /// Asks for a path to be processed.
    ///
    /// When the queue is full the **oldest** waiting request is dropped: the
    /// newest is the recording somebody just looked at, and a dropped request
    /// costs nothing to ask for again.
    pub fn request(&self, recording: impl AsRef<Path>) -> RequestOutcome {
        let recording = recording.as_ref().to_path_buf();
        let mut state = self.shared.lock();
        let outcome = if state.stopping {
            RequestOutcome::Stopped
        } else if state.working_on.as_ref() == Some(&recording) || state.queue.contains(&recording)
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
        };
        drop(state);
        self.shared.changed.notify_all();
        outcome
    }

    /// Stops processing until [`resume`](Self::resume) is called.
    ///
    /// Takes effect inside the current item: work in progress is paused, not
    /// abandoned. Nesting is not counted — this is a state, not a lock — so a
    /// caller that calls it twice resumes with one call.
    ///
    /// Returns whether this call is what suspended it, so a caller can log
    /// the transition in its own words rather than on every call.
    pub fn suspend_for_recording(&self) -> bool {
        let mut state = self.shared.lock();
        let already = state.suspended;
        state.suspended = true;
        drop(state);
        self.shared.changed.notify_all();
        !already
    }

    /// Lets processing carry on.
    ///
    /// Returns whether it was suspended, for the same reason
    /// [`suspend_for_recording`](Self::suspend_for_recording) does.
    pub fn resume(&self) -> bool {
        let mut state = self.shared.lock();
        let was = state.suspended;
        state.suspended = false;
        drop(state);
        self.shared.changed.notify_all();
        was
    }

    /// Whether processing is suspended.
    #[must_use]
    pub fn is_suspended(&self) -> bool {
        self.shared.lock().suspended
    }

    /// How many paths are waiting.
    #[must_use]
    pub fn queued(&self) -> usize {
        self.shared.lock().queue.len()
    }

    /// How many items have finished, successfully or not.
    #[must_use]
    pub fn finished(&self) -> u64 {
        self.shared.lock().finished
    }

    /// What the worker thread's priority actually became, once it has
    /// started.
    ///
    /// [`None`] until the worker has run its first instruction, and on a
    /// platform where no priority was applied.
    #[must_use]
    pub fn worker_priority(&self) -> Option<WorkerPriority> {
        self.shared.lock().priority
    }

    /// Stops the worker and waits for it.
    ///
    /// Equivalent to dropping the value; both exist because a caller that
    /// wants to know when the thread has gone should not have to rely on a
    /// drop.
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
        if let Some(thread) = self.thread.take() {
            // A panicking worker is reported by the thread itself; joining it
            // here is about not outliving it.
            let _ = thread.join();
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The worker thread.
fn run(shared: &Arc<Shared>, name: &str, process: &mut dyn FnMut(PathBuf, &dyn Pace) -> Outcome) {
    let priority = enter_background();
    {
        let mut state = shared.lock();
        state.priority = Some(priority);
    }
    shared.changed.notify_all();
    debug!(
        worker = %name,
        lowest = priority.is_lowest(),
        background_mode = priority.background_mode(),
        observed = priority.observed(),
        "a background worker is running"
    );

    while let Some(recording) = next(shared) {
        match process(recording, shared.as_ref()) {
            Outcome::Finished => {
                let mut locked = shared.lock();
                locked.working_on = None;
                locked.finished += 1;
                drop(locked);
                shared.changed.notify_all();
            }
            Outcome::Cancelled => {
                // Shutdown: `Pace::checkpoint` only ever returns `Stop` while
                // `stopping` is set, so the queue is about to be cleared and
                // the loop below would find nothing left worth taking. Ending
                // here rather than looping again is what keeps a cancelled
                // item unreported: nothing finished, so nothing is logged as
                // having finished.
                break;
            }
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
        crate::windows::priority::enter()
    }
    #[cfg(not(windows))]
    {
        // Clipped targets Windows (README, "Supported platforms"); everything
        // above the `windows` module still builds and tests elsewhere, and
        // says plainly that it lowered nothing rather than claiming it did.
        WorkerPriority::new(false, false, 0)
    }
}

/// Undoes [`enter_background`].
fn leave_background() {
    #[cfg(windows)]
    {
        crate::windows::priority::leave();
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
            capacity: 64,
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
    fn a_checkpoint_blocks_while_processing_is_suspended() {
        // The mechanism a `process` closure calls between packets. If this
        // returned straight away, suspension would be a flag nobody reads and
        // a library scan would run right through a recording.
        let shared = shared();
        shared.lock().suspended = true;
        let answers = checkpoint_in_the_background(&shared);

        assert_eq!(
            answers.recv_timeout(Duration::from_millis(500)),
            Err(mpsc::RecvTimeoutError::Timeout),
            "a checkpoint returned while processing was suspended"
        );

        shared.lock().suspended = false;
        shared.changed.notify_all();
        assert_eq!(
            answers
                .recv_timeout(Duration::from_secs(10))
                .expect("the checkpoint returns once processing resumes"),
            Continue::Yes
        );
    }

    #[test]
    fn a_checkpoint_asks_for_a_stop_when_the_worker_is_shutting_down() {
        let shared = shared();
        assert_eq!(shared.checkpoint(), Continue::Yes);

        // Shutting down while suspended has to break the wait rather than
        // deadlock inside it, which is the case a host hits every time it
        // quits during a recording.
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
                .expect("the checkpoint returns when the worker stops"),
            Continue::Stop
        );
    }

    #[test]
    fn the_worker_takes_nothing_off_the_queue_while_processing_is_suspended() {
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
            "the worker took work while processing was suspended"
        );

        shared.lock().suspended = false;
        shared.changed.notify_all();
        assert_eq!(
            taken.join().expect("the worker thread"),
            Some(PathBuf::from("waiting.mkv"))
        );
    }

    #[test]
    fn the_queue_is_bounded_and_drops_the_oldest_waiting_request() {
        let worker = Worker::start("clipped-background-tests", 2, |_, _| Outcome::Finished);
        // Suspended so the worker takes nothing off the queue while this runs.
        worker.suspend_for_recording();

        let first = PathBuf::from("first.mkv");
        let second = PathBuf::from("second.mkv");
        let third = PathBuf::from("third.mkv");

        assert_eq!(worker.request(&first), RequestOutcome::Queued);
        assert_eq!(worker.request(&second), RequestOutcome::Queued);
        assert_eq!(worker.request(&second), RequestOutcome::AlreadyQueued);
        assert_eq!(worker.queued(), 2);

        assert_eq!(
            worker.request(&third),
            RequestOutcome::QueuedInPlaceOf(first.clone())
        );
        assert_eq!(worker.queued(), 2);
    }

    #[test]
    fn shutting_down_stops_the_worker_rather_than_waiting_for_the_queue() {
        let worker = Worker::start("clipped-background-tests", 8, |_, pace| {
            if pace.checkpoint() == Continue::Stop {
                return Outcome::Cancelled;
            }
            Outcome::Finished
        });
        worker.suspend_for_recording();
        worker.request(PathBuf::from("match.mkv"));

        // Suspended with work outstanding: shutdown has to break the wait,
        // not deadlock behind it. The test hanging is the failure.
        worker.shutdown();
    }

    #[test]
    fn suspend_and_resume_report_whether_they_changed_anything() {
        // A caller uses this to decide whether to log a transition, rather
        // than logging on every call regardless of whether anything changed.
        let worker = Worker::start("clipped-background-tests", 8, |_, _| Outcome::Finished);
        assert!(
            !worker.resume(),
            "resuming an unsuspended worker changed something"
        );

        assert!(
            worker.suspend_for_recording(),
            "the first suspend did not report a transition"
        );
        assert!(
            !worker.suspend_for_recording(),
            "suspending twice reported a second transition"
        );
        assert!(
            worker.resume(),
            "resuming a suspended worker did not report a transition"
        );
        assert!(
            !worker.resume(),
            "resuming twice reported a second transition"
        );
    }
}
