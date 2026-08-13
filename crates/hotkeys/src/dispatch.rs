//! What happens between a key going down and a handler running, and the threads
//! it happens on.
//!
//! # The rule this module exists for
//!
//! **A hotkey press must never make another thread wait.** Saving a replay
//! writes a file, which is a syscall a capture loop must not be behind
//! (AGENTS.md section 20), and the press arrives on a Windows message loop that
//! has to stay responsive to the *next* press. So the only thing the thread
//! receiving a press does is [`Dispatcher::press`], which is a lookup, an atomic
//! increment and a non-blocking send — never a handler.
//!
//! # The threading model, stated plainly
//!
//! ```text
//!  Windows message loop (crate::service)
//!     │  WM_HOTKEY
//!     ▼
//!  Dispatcher::press  ── never blocks, never runs a handler ──┐
//!     │                                                       │
//!     │ try_send (bounded, one queue per action)               │ try_send
//!     ▼                                                       ▼
//!  one worker thread per handled action                  HotkeyEvent
//!     │                                                  to the caller
//!     ▼
//!  the handler
//! ```
//!
//! - **One worker thread per action that has a handler**, created when the
//!   service starts and never after: the thread count is bounded by
//!   [`crate::ACTIONS`] and is usually two or three (AGENTS.md section 20 forbids
//!   uncontrolled thread creation).
//! - **Across actions, handlers are concurrent.** A save that takes a second
//!   does not delay a bookmark; they are different threads.
//! - **Within one action, handlers are serial and in order.** Two saves at once
//!   would be two writers for one buffer, which is worse than a queue.
//! - **Nothing shared is locked.** The only object two threads touch is the
//!   queue, and the only operation the press side performs on it is
//!   `try_send`.
//!
//! # What is dropped, and why that is the right answer
//!
//! A queue holds [`PRESSES_PER_ACTION`] presses *waiting* behind the one the
//! handler is already running. Beyond that a press is *reported* as
//! [`PressOutcome::Dropped`] rather than queued or waited for.
//! Somebody hammering the save key eight times wants one clip and a responsive
//! machine, not eight clips and a hotkey thread stuck in `send`. The drop is
//! counted, logged and delivered to the caller — it is never silent
//! (AGENTS.md section 15).

use core::fmt;
use core::sync::atomic::{AtomicUsize, Ordering};
use std::collections::BTreeMap;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Instant;

use crate::action::{HotkeyAction, PlannedSubsystem};
use crate::hotkey::Hotkey;

/// How many presses of one action may be waiting for that action's handler.
///
/// Four is a person leaning on a key for a moment while the handler is busy,
/// not a buffer: the point of a bound is that the memory and the backlog behind
/// a stuck handler are both finite, and that a press past the bound is reported
/// rather than absorbed.
///
/// This bounds the *waiting*, not the total. A busy handler has already taken
/// its own press off the queue, so one stuck handler absorbs five presses — the
/// one it is running plus four waiting — and the sixth is the first that can be
/// dropped. `presses_past_the_queue_are_reported_rather_than_waited_for` sends
/// seven and asserts at least two are dropped rather than exactly two, because
/// a worker that has not yet reached its first `recv` leaves room for only
/// four.
pub const PRESSES_PER_ACTION: usize = 4;

/// How many press events may be waiting for the caller to read them.
///
/// The caller is expected to drain the receiver
/// [`crate::HotkeyService::start`] hands back; this bound
/// is what happens when it does not. Sixteen presses of anything is well past
/// the point at which the caller has a problem of its own, and dropping the
/// seventeenth *event* costs a notification, never a handler.
const EVENT_QUEUE: usize = 16;

/// One press of a bound combination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotkeyPress {
    action: HotkeyAction,
    hotkey: Hotkey,
    at: Instant,
}

impl HotkeyPress {
    /// Records a press of `hotkey`, bound to `action`, at `at`.
    #[must_use]
    pub(crate) const fn new(action: HotkeyAction, hotkey: Hotkey, at: Instant) -> Self {
        Self { action, hotkey, at }
    }

    /// What the user asked for.
    #[must_use]
    pub const fn action(&self) -> HotkeyAction {
        self.action
    }

    /// The combination they pressed.
    #[must_use]
    pub const fn hotkey(&self) -> Hotkey {
        self.hotkey
    }

    /// When the press was taken off the message queue.
    ///
    /// Taken on the thread that received it and before anything else happens to
    /// it, so the difference between this and the moment a handler starts is
    /// the dispatch latency and nothing else.
    #[must_use]
    pub const fn at(&self) -> Instant {
        self.at
    }
}

/// What became of a press.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PressOutcome {
    /// Handed to the action's handler, which is running or about to.
    Dispatched,
    /// Nothing in this process handles that action. See [`Unhandled`].
    Unhandled(Unhandled),
    /// The action's queue was full: its handler is still busy with earlier
    /// presses.
    Dropped {
        /// How many presses of this action were already waiting.
        waiting: usize,
    },
    /// The action's handler thread is gone, which means the handler panicked.
    ///
    /// Every later press of that action ends here too. A panicking handler is a
    /// defect in the handler, and this is how it stops being invisible.
    HandlerStopped,
}

impl fmt::Display for PressOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dispatched => formatter.write_str("dispatched"),
            Self::Unhandled(unhandled) => write!(formatter, "{unhandled}"),
            Self::Dropped { waiting } => write!(
                formatter,
                "ignored: the previous {waiting} presses have not been dealt with yet",
            ),
            Self::HandlerStopped => {
                formatter.write_str("ignored: this action's handler stopped after an error")
            }
        }
    }
}

/// A press of an action nothing in this process can perform.
///
/// This is the honest answer, and it is the whole reason the type exists: a
/// hotkey for something Clipped cannot do yet must say so, to the caller and
/// then to the user, rather than being swallowed (AGENTS.md section 54). It is
/// the same shape `clipped-recorder serve` refuses an unbuilt command with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unhandled {
    action: HotkeyAction,
    planned: Option<PlannedSubsystem>,
}

impl Unhandled {
    /// The sentence for an action this process has no handler for.
    ///
    /// Public so that it can be said *before* anybody presses the key. A
    /// configuration screen listing an action nothing performs has to give the
    /// same reason a press would, and two copies of that sentence would drift
    /// apart the moment a milestone landed (AGENTS.md section 55) — which is a
    /// drift nobody would notice, because the stale one still reads plausibly.
    #[must_use]
    pub const fn for_action(action: HotkeyAction) -> Self {
        Self {
            action,
            planned: action.planned_subsystem(),
        }
    }

    /// The action nobody handled.
    #[must_use]
    pub const fn action(&self) -> HotkeyAction {
        self.action
    }

    /// The subsystem that would handle it and does not exist yet, if that is
    /// the reason. [`None`] means the subsystem exists and this process was
    /// simply not given a handler for it.
    #[must_use]
    pub const fn planned_subsystem(&self) -> Option<PlannedSubsystem> {
        self.planned
    }
}

impl fmt::Display for Unhandled {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.planned {
            Some(planned) => write!(
                formatter,
                "{} is not in this build: {planned}",
                self.action.label(),
            ),
            None => write!(
                formatter,
                "nothing in this process handles {}",
                self.action.label(),
            ),
        }
    }
}

/// A press and what became of it, as the caller is told about it.
///
/// Produced on the thread that received the press, before the handler has done
/// anything, so a caller reading these sees presses at the rate they happen
/// however slow the handlers are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyEvent {
    press: HotkeyPress,
    outcome: PressOutcome,
}

impl HotkeyEvent {
    /// The press.
    #[must_use]
    pub const fn press(&self) -> &HotkeyPress {
        &self.press
    }

    /// What became of it.
    #[must_use]
    pub const fn outcome(&self) -> &PressOutcome {
        &self.outcome
    }
}

impl fmt::Display for HotkeyEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} ({}): {}",
            self.press.action.label(),
            self.press.hotkey,
            self.outcome,
        )
    }
}

/// What to run when an action is pressed.
///
/// A handler runs on its action's own thread and may take as long as it needs
/// to; saving a replay is slow and nothing waits for it. What it must not do is
/// outlive the service without saying so — a panic ends that action's thread,
/// and every later press of it is reported as
/// [`PressOutcome::HandlerStopped`].
#[derive(Default)]
pub struct Handlers {
    handlers: BTreeMap<HotkeyAction, Handler>,
}

/// The boxed closure behind one action.
type Handler = Box<dyn FnMut(HotkeyPress) + Send + 'static>;

impl fmt::Debug for Handlers {
    /// Closures have no useful `Debug`, so this lists what is handled — which
    /// is the question anybody debugging a dead hotkey is actually asking.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Handlers")
            .field(
                "handled",
                &self.handlers.keys().map(|a| a.name()).collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl Handlers {
    /// No handlers at all: every press is [`PressOutcome::Unhandled`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs `handler` when `action` is pressed, replacing any handler already
    /// registered for it.
    #[must_use]
    pub fn on(
        mut self,
        action: HotkeyAction,
        handler: impl FnMut(HotkeyPress) + Send + 'static,
    ) -> Self {
        self.handlers.insert(action, Box::new(handler));
        self
    }

    /// Which actions have handlers.
    pub fn handled(&self) -> impl Iterator<Item = HotkeyAction> + '_ {
        self.handlers.keys().copied()
    }

    /// Runs the handler registered for `action`, on the calling thread, as
    /// though `hotkey` had been pressed.
    ///
    /// This is the only way to reach a registered handler without a keyboard.
    /// [`handled`](Self::handled) says *that* an action has one, and
    /// [`crate::HotkeyService::start`] consumes the whole set into worker
    /// threads, so without this a closure is opaque from the moment it is
    /// registered: the process wiring the handlers up can assert which actions
    /// it registered, and nothing whatsoever about what those handlers do.
    ///
    /// That gap is worth an API because of the specific way it fails. A handler
    /// registered against the wrong action is not a dead key — it is a key that
    /// does something other than what its row in the settings screen says, and
    /// the screenshot key stopping a recording is a worse outcome than the
    /// screenshot key doing nothing.
    ///
    /// Unlike a real press this runs the handler **here**, not on that action's
    /// thread, and so it neither queues nor drops: the concurrency rules this
    /// module documents belong to [`Dispatcher`] and are tested against it.
    ///
    /// # Errors
    ///
    /// [`Unhandled`] when no handler is registered for `action`, carrying the
    /// same sentence a real press of it would be reported with.
    pub fn press(&mut self, action: HotkeyAction, hotkey: Hotkey) -> Result<(), Unhandled> {
        let Some(handler) = self.handlers.get_mut(&action) else {
            return Err(Unhandled::for_action(action));
        };

        handler(HotkeyPress::new(action, hotkey, Instant::now()));
        Ok(())
    }
}

/// One action's queue, thread and backlog counter.
#[derive(Debug)]
struct Worker {
    presses: SyncSender<HotkeyPress>,
    waiting: Arc<AtomicUsize>,
    handle: Option<JoinHandle<()>>,
}

/// The press side of the service: everything that happens between a press being
/// received and a handler being handed it.
///
/// Split out from the Windows message loop so that the threading rules above
/// can be tested on any machine, with no keyboard and no desktop, by calling
/// [`press`](Self::press) directly.
#[derive(Debug)]
pub(crate) struct Dispatcher {
    workers: BTreeMap<HotkeyAction, Worker>,
    events: Option<SyncSender<HotkeyEvent>>,
}

impl Dispatcher {
    /// Starts one thread per handled action.
    ///
    /// Returns the dispatcher and the receiver the caller reads
    /// [`HotkeyEvent`]s from.
    pub(crate) fn start(handlers: Handlers) -> (Self, Receiver<HotkeyEvent>) {
        let (events, incoming) = mpsc::sync_channel(EVENT_QUEUE);

        let workers = handlers
            .handlers
            .into_iter()
            .map(|(action, handler)| (action, Worker::start(action, handler)))
            .collect();

        (
            Self {
                workers,
                events: Some(events),
            },
            incoming,
        )
    }

    /// Hands one press to its action's handler.
    ///
    /// **This never blocks and never runs a handler**, which is the property
    /// the message loop depends on. It performs a map lookup, an atomic
    /// increment and two non-blocking sends.
    pub(crate) fn press(&self, press: HotkeyPress) -> PressOutcome {
        let outcome = self.queue(press);

        match &outcome {
            PressOutcome::Dispatched => {
                tracing::debug!(action = press.action.name(), hotkey = %press.hotkey, "hotkey pressed");
            }
            // Not a debug line: each of these is a press the user made and did
            // not get, and is the first thing a report of "the hotkey does
            // nothing" needs to show (AGENTS.md section 15).
            other => {
                tracing::warn!(
                    action = press.action.name(),
                    hotkey = %press.hotkey,
                    outcome = %other,
                    "hotkey press was not acted on",
                );
            }
        }

        if let Some(events) = &self.events {
            let event = HotkeyEvent {
                press,
                outcome: outcome.clone(),
            };
            if let Err(TrySendError::Full(event)) = events.try_send(event) {
                tracing::warn!(%event, "nobody is reading hotkey events, so this one was dropped");
            }
        }

        outcome
    }

    /// The queue half of [`press`](Self::press), without the reporting.
    fn queue(&self, press: HotkeyPress) -> PressOutcome {
        let Some(worker) = self.workers.get(&press.action) else {
            return PressOutcome::Unhandled(Unhandled::for_action(press.action));
        };

        // Counted before the send, like the muxer's queue, so that the depth a
        // full queue reports is never behind the queue itself.
        worker.waiting.fetch_add(1, Ordering::Relaxed);
        match worker.presses.try_send(press) {
            Ok(()) => PressOutcome::Dispatched,
            Err(TrySendError::Full(_)) => {
                let waiting = worker.waiting.fetch_sub(1, Ordering::Relaxed) - 1;
                PressOutcome::Dropped { waiting }
            }
            Err(TrySendError::Disconnected(_)) => {
                worker.waiting.fetch_sub(1, Ordering::Relaxed);
                PressOutcome::HandlerStopped
            }
        }
    }

    /// Closes every queue and waits for the handlers still running.
    ///
    /// This *does* wait, and deliberately: a replay being written when the user
    /// quits should finish being written (AGENTS.md section 17). It is called
    /// from whoever stops the service, never from the message loop and never
    /// from a capture thread.
    pub(crate) fn shutdown(mut self) {
        // Dropping the event sender first releases anything blocked reading
        // events, so a caller that is draining them is not still waiting when
        // the workers are joined below.
        drop(self.events.take());

        for (action, mut worker) in core::mem::take(&mut self.workers) {
            drop(worker.presses);
            if let Some(handle) = worker.handle.take() {
                if handle.join().is_err() {
                    tracing::warn!(
                        action = action.name(),
                        "this action's handler panicked; later presses of it were refused",
                    );
                }
            }
        }
    }
}

impl Drop for Dispatcher {
    /// A dispatcher dropped without [`shutdown`](Self::shutdown) still closes
    /// its queues and joins its threads, so no path — a panic included — leaves
    /// a handler thread running against state its owner has released
    /// (AGENTS.md section 58).
    fn drop(&mut self) {
        for (_, worker) in core::mem::take(&mut self.workers) {
            let mut worker = worker;
            drop(worker.presses);
            if let Some(handle) = worker.handle.take() {
                let _ = handle.join();
            }
        }
    }
}

impl Worker {
    /// Starts the thread that runs `handler`.
    fn start(action: HotkeyAction, mut handler: Handler) -> Self {
        let (presses, incoming) = mpsc::sync_channel(PRESSES_PER_ACTION);
        let waiting = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&waiting);

        let handle = thread::Builder::new()
            .name(format!("clipped-hotkey-{}", action.name()))
            .spawn(move || {
                for press in incoming {
                    counted.fetch_sub(1, Ordering::Relaxed);
                    handler(press);
                }
            })
            // A machine that cannot start a thread is a machine that cannot
            // record either, and there is nothing to fall back to.
            .expect("a hotkey handler needs a thread to run on");

        Self {
            presses,
            waiting,
            handle: Some(handle),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{self, Receiver};
    use std::time::{Duration, Instant};

    use super::{
        Dispatcher, Handlers, HotkeyEvent, HotkeyPress, PressOutcome, Unhandled, PRESSES_PER_ACTION,
    };
    use crate::action::HotkeyAction;
    use crate::hotkey::Hotkey;

    /// A combination to attach to a press. Nothing here registers anything, so
    /// which one it is only matters to the message it appears in.
    fn hotkey() -> Hotkey {
        "Ctrl+F10".parse().expect("Ctrl+F10 is a hotkey")
    }

    fn press(action: HotkeyAction) -> HotkeyPress {
        HotkeyPress::new(action, hotkey(), Instant::now())
    }

    /// How long a "slow" handler in these tests blocks for.
    const SLOW: Duration = Duration::from_secs(1);

    /// How long anything that must *not* have waited for a slow handler is
    /// given. Generously more than the work involved and far less than [`SLOW`],
    /// so that a loaded CI runner does not fail it and a genuinely serialised
    /// dispatcher cannot pass it.
    const PROMPTLY: Duration = Duration::from_millis(400);

    fn drain(events: &Receiver<HotkeyEvent>) -> Vec<HotkeyEvent> {
        events.try_iter().collect()
    }

    #[test]
    fn a_press_reaches_the_handler_that_was_registered_for_it() {
        let (sender, handled) = mpsc::channel();
        let (dispatcher, _events) =
            Dispatcher::start(Handlers::new().on(HotkeyAction::AddBookmark, move |press| {
                sender.send(press.action()).expect("the test is listening");
            }));

        assert_eq!(
            dispatcher.press(press(HotkeyAction::AddBookmark)),
            PressOutcome::Dispatched,
        );

        assert_eq!(
            handled
                .recv_timeout(PROMPTLY)
                .expect("the handler runs on its own thread"),
            HotkeyAction::AddBookmark,
        );
    }

    /// The property every caller of `Handlers::press` is relying on: it runs
    /// the handler registered for the action it was given, and hands that
    /// handler that action.
    ///
    /// Registering two handlers and pressing one is what makes this an
    /// assertion rather than a tautology — a `press` that ran whichever handler
    /// came first would pass with only one registered.
    #[test]
    fn pressing_an_action_runs_the_handler_registered_for_that_action_and_no_other() {
        let (bookmarked, bookmarks) = mpsc::channel();
        let (shot, screenshots) = mpsc::channel();
        let mut handlers = Handlers::new()
            .on(HotkeyAction::AddBookmark, move |press| {
                bookmarked.send(press).expect("the test is listening");
            })
            .on(HotkeyAction::TakeScreenshot, move |press| {
                shot.send(press).expect("the test is listening");
            });

        handlers
            .press(HotkeyAction::TakeScreenshot, hotkey())
            .expect("a handler was registered for it");

        let ran = screenshots
            .try_recv()
            .expect("the screenshot handler is the one that should have run");
        assert_eq!(ran.action(), HotkeyAction::TakeScreenshot);
        assert_eq!(ran.hotkey(), hotkey());
        assert!(
            bookmarks.try_recv().is_err(),
            "pressing one action ran another action's handler",
        );
    }

    /// AGENTS.md section 54: pressing an action nothing handles has to say so
    /// rather than look like it worked.
    #[test]
    fn pressing_an_action_with_no_handler_is_refused_in_the_words_a_real_press_uses() {
        let mut handlers = Handlers::new().on(HotkeyAction::AddBookmark, |_| {});

        let unhandled = handlers
            .press(HotkeyAction::SaveReplay, hotkey())
            .expect_err("nothing was registered for it");

        assert_eq!(unhandled, Unhandled::for_action(HotkeyAction::SaveReplay));
    }

    /// AGENTS.md section 54: an action with nothing behind it says so.
    #[test]
    fn an_action_with_no_handler_reports_which_milestone_would_build_it() {
        let (dispatcher, events) = Dispatcher::start(Handlers::new());

        let outcome = dispatcher.press(press(HotkeyAction::OpenOverlay));

        let PressOutcome::Unhandled(unhandled) = outcome else {
            panic!("no build has an in-game overlay, so this must be Unhandled: {outcome}");
        };
        assert_eq!(unhandled.action(), HotkeyAction::OpenOverlay);
        let message = unhandled.to_string();
        assert!(message.contains("Open overlay"), "{message}");
        assert!(message.contains("M5"), "{message}");
        assert!(message.contains("#53"), "{message}");

        let reported = drain(&events);
        assert_eq!(reported.len(), 1, "the caller is told, not only the log");
        assert_eq!(reported[0].press().action(), HotkeyAction::OpenOverlay);
    }

    #[test]
    fn an_action_whose_subsystem_exists_but_has_no_handler_here_says_that_instead() {
        let (dispatcher, _events) = Dispatcher::start(Handlers::new());

        let outcome = dispatcher.press(press(HotkeyAction::ToggleRecording));

        let PressOutcome::Unhandled(unhandled) = outcome else {
            panic!("no handler was registered: {outcome}");
        };
        assert_eq!(unhandled.planned_subsystem(), None);
        assert_eq!(
            unhandled.to_string(),
            "nothing in this process handles Start or stop recording",
        );
    }

    /// The acceptance criterion, in the form it can be tested in: a handler
    /// that blocks for a second must not delay anything else.
    #[test]
    fn a_handler_that_blocks_for_a_second_delays_neither_the_caller_nor_another_action() {
        let (finished, finishes) = mpsc::channel();
        let bookmarked = finished.clone();

        let (dispatcher, _events) = Dispatcher::start(
            Handlers::new()
                .on(HotkeyAction::SaveReplay, move |_| {
                    std::thread::sleep(SLOW);
                    finished.send(HotkeyAction::SaveReplay).ok();
                })
                .on(HotkeyAction::AddBookmark, move |_| {
                    bookmarked.send(HotkeyAction::AddBookmark).ok();
                }),
        );

        let started = Instant::now();
        assert_eq!(
            dispatcher.press(press(HotkeyAction::SaveReplay)),
            PressOutcome::Dispatched,
        );
        let submitting = started.elapsed();

        // (1) The thread that received the press is free again immediately. On
        //     Windows that thread is the message loop, and the next press
        //     arrives on it.
        assert!(
            submitting < PROMPTLY,
            "handing a press to a slow handler took {submitting:?}, so the \
             receiving thread was made to wait for it",
        );

        // (2) A different action runs while the slow one is still going.
        assert_eq!(
            dispatcher.press(press(HotkeyAction::AddBookmark)),
            PressOutcome::Dispatched,
        );
        let first = finishes
            .recv_timeout(PROMPTLY)
            .expect("the bookmark handler must not be behind the save");
        assert_eq!(
            first,
            HotkeyAction::AddBookmark,
            "the bookmark finished after the save, so the two share a thread",
        );

        let second = finishes
            .recv_timeout(SLOW * 2)
            .expect("the save handler still finishes");
        assert_eq!(second, HotkeyAction::SaveReplay);
    }

    #[test]
    fn two_presses_of_one_action_run_one_after_the_other_and_in_order() {
        let (order, ran) = mpsc::channel();
        let (dispatcher, _events) =
            Dispatcher::start(Handlers::new().on(HotkeyAction::AddBookmark, move |press| {
                // Which press this is, and when its handler started.
                order
                    .send((press.at(), Instant::now()))
                    .expect("the test is listening");
                std::thread::sleep(Duration::from_millis(100));
            }));

        let first = press(HotkeyAction::AddBookmark);
        let second = HotkeyPress::new(
            HotkeyAction::AddBookmark,
            hotkey(),
            first.at() + Duration::from_millis(1),
        );
        dispatcher.press(first);
        dispatcher.press(second);

        let (ran_first, started_first) = ran.recv_timeout(PROMPTLY).expect("the first press runs");
        let (ran_second, started_second) = ran
            .recv_timeout(PROMPTLY * 2)
            .expect("the second press runs");
        assert_eq!(ran_first, first.at(), "the presses ran out of order");
        assert_eq!(ran_second, second.at(), "the presses ran out of order");
        assert!(
            started_second.duration_since(started_first) >= Duration::from_millis(100),
            "the second press started {:?} after the first, so the two ran at once",
            started_second.duration_since(started_first),
        );
    }

    #[test]
    fn presses_past_the_queue_are_reported_rather_than_waited_for() {
        let (dispatcher, events) = Dispatcher::start(
            Handlers::new().on(HotkeyAction::SaveReplay, move |_| std::thread::sleep(SLOW)),
        );

        // One press occupies the handler; PRESSES_PER_ACTION more fill the
        // queue behind it. Everything after that has nowhere to go.
        let started = Instant::now();
        let mut outcomes = Vec::new();
        for _ in 0..=PRESSES_PER_ACTION + 2 {
            outcomes.push(dispatcher.press(press(HotkeyAction::SaveReplay)));
        }
        let submitting = started.elapsed();

        assert!(
            submitting < PROMPTLY,
            "submitting {} presses to a busy handler took {submitting:?}: a full \
             queue made the caller wait",
            outcomes.len(),
        );
        let dropped = outcomes
            .iter()
            .filter(|outcome| matches!(outcome, PressOutcome::Dropped { .. }))
            .count();
        assert!(
            dropped >= 2,
            "the presses past the queue should have been reported as dropped: {outcomes:?}",
        );
        assert!(
            drain(&events)
                .iter()
                .any(|event| matches!(event.outcome(), PressOutcome::Dropped { .. })),
            "a dropped press must reach the caller, not only the log",
        );
    }

    /// Exactly where the bound falls, which
    /// `presses_past_the_queue_are_reported_rather_than_waited_for` cannot say:
    /// it races the worker to its first `recv`, so it can only assert "at least
    /// two". Waiting until the handler has demonstrably started removes the
    /// race, and then the count is exact — one press being run plus
    /// [`PRESSES_PER_ACTION`] waiting, and the next one dropped.
    ///
    /// This is the number `docs/hotkeys.md` states, held to the code rather
    /// than to a reading of it.
    #[test]
    fn a_stuck_handler_absorbs_one_press_plus_a_full_queue_before_one_is_dropped() {
        let (entered, starts) = mpsc::channel();
        let (release, blocked) = mpsc::channel::<()>();
        let (dispatcher, events) =
            Dispatcher::start(Handlers::new().on(HotkeyAction::SaveReplay, move |_| {
                // Not `expect`: once the test has returned nobody is listening,
                // and a handler thread panicking during shutdown would be noise
                // that means nothing.
                let _ = entered.send(());
                // Waits until the test drops `release` — but never longer than
                // `SLOW`. A plain `recv()` would read more tidily and would
                // hang the suite instead of failing it: dropping the
                // `Dispatcher` joins this thread, and on a failing assertion
                // that drop runs before `release` is dropped, so the join would
                // wait on a handler waiting on a sender nothing will drop. A
                // test that deadlocks when it should fail is no better than one
                // that cannot fail.
                let _ = blocked.recv_timeout(SLOW);
            }));

        assert_eq!(
            dispatcher.press(press(HotkeyAction::SaveReplay)),
            PressOutcome::Dispatched,
        );
        starts
            .recv_timeout(PROMPTLY)
            .expect("the handler starts on its own thread");

        // The handler is now inside the closure and cannot take anything else
        // off the queue, so these fill it and none of them may be refused.
        for waiting in 1..=PRESSES_PER_ACTION {
            assert_eq!(
                dispatcher.press(press(HotkeyAction::SaveReplay)),
                PressOutcome::Dispatched,
                "press {waiting} of {PRESSES_PER_ACTION} behind a busy handler should \
                 have been queued, not dropped",
            );
        }

        assert_eq!(
            dispatcher.press(press(HotkeyAction::SaveReplay)),
            PressOutcome::Dropped {
                waiting: PRESSES_PER_ACTION,
            },
            "the queue was full and the handler busy, so this press had nowhere to go",
        );
        assert!(
            drain(&events).iter().any(|event| matches!(
                event.outcome(),
                PressOutcome::Dropped {
                    waiting: PRESSES_PER_ACTION,
                },
            )),
            "the drop and how many were waiting must both reach the caller",
        );

        drop(release);
    }

    /// A panicking handler is a defect in the handler. What must not happen is
    /// that it becomes invisible, so the panic message on stderr during this
    /// test is expected.
    #[test]
    fn a_handler_that_panicked_is_reported_rather_than_leaving_a_dead_key() {
        let (dispatcher, _events) = Dispatcher::start(
            Handlers::new().on(HotkeyAction::TakeScreenshot, |_| panic!("a broken handler")),
        );

        assert_eq!(
            dispatcher.press(press(HotkeyAction::TakeScreenshot)),
            PressOutcome::Dispatched,
            "the first press is accepted; the handler has not run yet",
        );

        let deadline = Instant::now() + PROMPTLY;
        let outcome = loop {
            let outcome = dispatcher.press(press(HotkeyAction::TakeScreenshot));
            if outcome == PressOutcome::HandlerStopped || Instant::now() > deadline {
                break outcome;
            }
            std::thread::sleep(Duration::from_millis(10));
        };

        assert_eq!(outcome, PressOutcome::HandlerStopped);
        assert!(outcome.to_string().contains("handler stopped"), "{outcome}");
    }

    #[test]
    fn shutting_down_waits_for_the_handler_that_is_running() {
        let (finished, finishes) = mpsc::channel();
        let (dispatcher, _events) =
            Dispatcher::start(Handlers::new().on(HotkeyAction::SaveReplay, move |_| {
                std::thread::sleep(Duration::from_millis(200));
                finished.send(()).ok();
            }));

        dispatcher.press(press(HotkeyAction::SaveReplay));
        dispatcher.shutdown();

        assert!(
            finishes.try_recv().is_ok(),
            "a replay being written when the service stops must be finished, not abandoned",
        );
    }
}
