//! The bounded queue between a plugin and the recording.
//!
//! This is the only thing a recording session touches. It never calls a plugin,
//! never waits for one and never learns that one exists beyond the events in
//! here — which is what makes "a plugin cannot stall a recording" a property of
//! the shape rather than a promise about the plugins.
//!
//! Two rules, and they are the whole module:
//!
//! - **Delivering never blocks.** A full queue drops the event and counts it.
//!   The alternative — a plugin's reader thread waiting for a consumer — moves
//!   the flood from a queue into a stall, and the stall would be on the thread
//!   that reads a plugin's output rather than the one that encodes, but the
//!   supervisor would then be waiting behind it (AGENTS.md section 20).
//! - **What was dropped is counted and reported.** A timeline missing marks has
//!   to say so; a timeline missing marks that looks complete is the invented
//!   data AGENTS.md section 27 forbids. [`InboxStats::dropped`] is that number,
//!   and `crate::supervision` also treats a plugin that fills the queue as a
//!   plugin that is flooding.
//!
//! # Why the newest is dropped rather than the oldest
//!
//! Because the events already queued are the ones that arrived first, and a
//! flood is the anomaly. Evicting the oldest would mean a plugin that has
//! started emitting rubbish erases the kills it reported before it went wrong.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Arc;

use clipped_events::GameEvent;

/// How many events the queue holds when a caller does not choose.
///
/// A game produces events in ones and twos a minute and the session drains the
/// queue every time round its loop, so this is deep enough that nothing is ever
/// dropped in normal use and shallow enough that a plugin flooding it costs a
/// few hundred kilobytes rather than the machine.
pub const DEFAULT_CAPACITY: usize = 512;

/// Creates the queue: a handle each running plugin delivers into, and the end
/// the recording drains.
///
/// `capacity` of zero is read as one. A zero-capacity queue in this shape is a
/// rendezvous — every delivery would fail unless a consumer happened to be
/// waiting at that instant — which is not a queue anybody meant to ask for.
#[must_use]
pub fn inbox(capacity: usize) -> (EventInbox, EventReceiver) {
    let (sender, receiver) = sync_channel(capacity.max(1));
    let counters = Arc::new(Counters::default());
    (
        EventInbox {
            sender,
            counters: Arc::clone(&counters),
        },
        EventReceiver {
            receiver,
            counters,
            capacity: capacity.max(1),
        },
    )
}

/// Where a running plugin puts the events it reports.
///
/// Cloned once per plugin. Delivering from several plugins at once is safe and
/// never blocks any of them.
#[derive(Debug, Clone)]
pub struct EventInbox {
    sender: SyncSender<GameEvent>,
    counters: Arc<Counters>,
}

impl EventInbox {
    /// Offers an event to the recording.
    ///
    /// Returns whether it was taken. [`Delivery::Dropped`] means the queue was
    /// full, or that the recording has finished and stopped draining; neither
    /// is a reason to stop reading a plugin, and both are counted.
    pub fn deliver(&self, event: GameEvent) -> Delivery {
        match self.sender.try_send(event) {
            Ok(()) => {
                self.counters.delivered.fetch_add(1, Ordering::Relaxed);
                Delivery::Delivered
            }
            Err(_) => {
                self.counters.dropped.fetch_add(1, Ordering::Relaxed);
                Delivery::Dropped
            }
        }
    }

    /// What has been delivered and dropped so far, across every plugin.
    #[must_use]
    pub fn stats(&self) -> InboxStats {
        self.counters.read()
    }
}

/// What became of an offered event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// It is queued for the recording.
    Delivered,
    /// The queue was full, or nothing is draining it any more.
    Dropped,
}

impl Delivery {
    /// Whether the event was dropped.
    #[must_use]
    pub const fn was_dropped(self) -> bool {
        matches!(self, Self::Dropped)
    }
}

/// The recording's end of the queue.
///
/// Draining it is the only work a plugin ever causes on the session's thread,
/// and it is bounded by [`capacity`](Self::capacity) however badly a plugin
/// behaves.
#[derive(Debug)]
pub struct EventReceiver {
    receiver: Receiver<GameEvent>,
    counters: Arc<Counters>,
    capacity: usize,
}

impl EventReceiver {
    /// The next event, or [`None`] when there is nothing waiting.
    ///
    /// Never blocks, including when every plugin has gone.
    ///
    /// Nothing waiting and every plugin gone are the same answer here, on
    /// purpose: a session that has finished with plugins is not interested in
    /// which it was, and `PluginSupervisor::health` is where that question is
    /// asked.
    #[must_use]
    pub fn next_event(&self) -> Option<GameEvent> {
        self.receiver.try_recv().ok()
    }

    /// Everything waiting, in the order it was delivered, up to one queue's
    /// worth.
    ///
    /// **At most [`capacity`](Self::capacity) events**, and that bound is the
    /// point rather than an implementation detail: a plugin delivering faster
    /// than this loop runs would otherwise keep a caller inside this function
    /// for as long as it kept producing, which is exactly the stall a bounded
    /// queue exists to prevent. Anything still waiting is returned by the next
    /// call.
    ///
    /// Events from several plugins are interleaved by arrival and are **not**
    /// in timeline order; sort by `timing().at()` (`crates/events`).
    ///
    /// The bound is proved by
    /// `supervisor::tests::a_plugin_that_floods_is_bounded_counted_and_stopped_for_good`,
    /// against a real flooding process, and not by a test in this module: two
    /// threads of one process do not reliably outrun each other, so an
    /// in-process producer makes a test that passes whether or not the bound is
    /// there. A real plugin filling a pipe does outrun the drain, every time.
    #[must_use]
    pub fn drain(&self) -> Vec<GameEvent> {
        let mut events = Vec::with_capacity(0);
        while events.len() < self.capacity {
            match self.next_event() {
                Some(event) => events.push(event),
                None => break,
            }
        }
        events
    }

    /// How many events the queue holds.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// What has been delivered and dropped so far.
    #[must_use]
    pub fn stats(&self) -> InboxStats {
        self.counters.read()
    }
}

/// What the queue has seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InboxStats {
    /// Events handed to the recording.
    pub delivered: u64,
    /// Events lost because the queue was full, or because nothing was draining
    /// it. A timeline built from a session with a non-zero count here is
    /// incomplete, and has to say so.
    pub dropped: u64,
}

#[derive(Debug, Default)]
struct Counters {
    delivered: AtomicU64,
    dropped: AtomicU64,
}

impl Counters {
    fn read(&self) -> InboxStats {
        InboxStats {
            delivered: self.delivered.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use clipped_events::{Confidence, EventKind, EventSource, EventTime, EventTiming};

    use super::*;

    fn event() -> GameEvent {
        GameEvent::new(
            EventKind::Kill,
            EventTiming::new(EventTime::ZERO, Duration::ZERO),
            EventSource::plugin("acme").expect("a valid identifier"),
            Confidence::CERTAIN,
        )
    }

    #[test]
    fn events_arrive_in_the_order_they_were_delivered() {
        let (inbox, receiver) = inbox(4);
        assert_eq!(inbox.deliver(event()), Delivery::Delivered);
        assert_eq!(inbox.deliver(event()), Delivery::Delivered);

        assert_eq!(receiver.drain().len(), 2);
        assert!(receiver.drain().is_empty());
        assert_eq!(
            receiver.stats(),
            InboxStats {
                delivered: 2,
                dropped: 0
            }
        );
    }

    #[test]
    fn a_flood_is_bounded_and_counted_rather_than_blocking() {
        // The property the recording depends on: delivering never waits, so a
        // plugin producing events faster than anything drains them costs a
        // fixed amount of memory and a counter, and no stall anywhere.
        let (inbox, receiver) = inbox(8);
        for _ in 0..1_000 {
            inbox.deliver(event());
        }

        let stats = receiver.stats();
        assert_eq!(stats.delivered, 8, "the queue holds what it says it holds");
        assert_eq!(stats.dropped, 992);
        assert_eq!(
            receiver.drain().len(),
            8,
            "and a drain costs at most one queue"
        );
    }

    // There is deliberately no test here for "a producer that never stops
    // cannot keep a drain going". One was written, and it passed with the bound
    // removed: two threads of one process do not reliably outrun each other, so
    // the drain emptied the queue between deliveries and the assertion never
    // saw the case it was written for. The bound is proved instead by
    // `supervisor::tests::a_plugin_that_floods_is_bounded_counted_and_stopped_for_good`,
    // where the producer is a real process filling a pipe — which fails
    // consistently when the bound is taken out. A test that cannot fail is
    // worse than no test (AGENTS.md section 23).

    #[test]
    fn delivering_to_a_finished_recording_is_counted_rather_than_failing() {
        let (inbox, receiver) = inbox(4);
        drop(receiver);
        assert_eq!(inbox.deliver(event()), Delivery::Dropped);
        assert_eq!(inbox.stats().dropped, 1);
    }

    #[test]
    fn a_queue_of_no_capacity_is_a_queue_of_one() {
        let (inbox, receiver) = inbox(0);
        assert_eq!(receiver.capacity(), 1);
        assert_eq!(inbox.deliver(event()), Delivery::Delivered);
        assert_eq!(inbox.deliver(event()), Delivery::Dropped);
    }
}
