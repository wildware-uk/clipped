//! Turning "the state changed between these two payloads" into a moment and an
//! uncertainty.
//!
//! Game State Integration is a *polled* source dressed up as a push: the game
//! posts when something changed, but no more often than the throttle it was
//! configured with, and it says nothing about when the change happened. So all
//! a plugin honestly knows about a change it found by comparing two payloads is
//! that it happened **somewhere between them**.
//!
//! `docs/plugin-api.md` says exactly what to do with that:
//!
//! > A source polled every two seconds knows the moment to within a second: it
//! > places `at` in the middle of the window it is sure about and says
//! > `precision` is one second.
//!
//! Which is this module, and nothing else. The window is measured rather than
//! assumed — it is the interval that actually elapsed between the previous
//! payload and this one, not the interval the configuration file asked for —
//! because a game that has been paused, or a machine that has been busy, posts
//! when it posts, and claiming the configured precision then would be claiming
//! a precision this plugin does not have.
//!
//! Nothing here is capped. A twelve-second gap produces a six-second precision
//! and an event placed six seconds ago, which reads oddly and is true;
//! flattening it to something tidier would be inventing a precision (AGENTS.md
//! section 27, `crates/events`).

use core::time::Duration;
use std::time::Instant;

use clipped_events::EventKind;
use clipped_plugins::ReportedEvent;
use serde_json::{Map, Value};

/// The interval a change was found in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    ago: Duration,
    precision: Duration,
}

impl Window {
    /// How long before the report the event is placed.
    #[must_use]
    pub const fn ago(&self) -> Duration {
        self.ago
    }

    /// How far either side of that the truth may lie.
    #[must_use]
    pub const fn precision(&self) -> Duration {
        self.precision
    }

    /// An event found in this window, in the shape the host reads.
    ///
    /// `confidence` is `1.0` and is not a parameter. Game State Integration is
    /// the game itself saying what happened, so there is nothing to be unsure
    /// *about*: what a plugin over it can be unsure of is **when**, and that is
    /// [`precision`](Self::precision). `crates/events` keeps the two apart
    /// precisely so that a certain-but-imprecise source does not have to
    /// pretend to be an uncertain one.
    #[must_use]
    pub fn report(&self, kind: EventKind, data: Map<String, Value>) -> ReportedEvent {
        ReportedEvent {
            kind,
            ago_ns: nanos(self.ago),
            precision_ns: nanos(self.precision),
            confidence: 1.0,
            data,
        }
    }
}

/// How often payloads have been arriving.
#[derive(Debug, Clone, Copy)]
pub struct Cadence {
    previous: Instant,
}

impl Cadence {
    /// A cadence whose first window opens now.
    ///
    /// `opened` is when this plugin started being able to hear anything —
    /// in practice the moment the listener was bound. It exists so that the
    /// first payload has an honest window rather than an assumed one: whatever
    /// it describes could have happened at any point since the plugin could
    /// first have heard about it.
    #[must_use]
    pub const fn opened_at(opened: Instant) -> Self {
        Self { previous: opened }
    }

    /// The window a payload that arrived at `received` closes.
    #[must_use]
    pub fn observe(&mut self, received: Instant) -> Window {
        let interval = received.saturating_duration_since(self.previous);
        self.previous = received;
        let half = interval / 2;
        Window {
            ago: half,
            precision: half,
        }
    }
}

/// Nanoseconds, saturating.
///
/// A `Duration` counts further than a `u64` of nanoseconds can — 584 years,
/// against `Duration`'s 584 billion — so the conversion has an unrepresentable
/// case. It is reached only by a plugin that has been running for six
/// centuries, and saturating is the right answer for it anyway.
fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_is_placed_in_the_middle_of_the_window_it_was_found_in() {
        // The rule `docs/plugin-api.md` states, asserted rather than described:
        // a change found by comparing a payload with the one two seconds before
        // it is placed one second ago, give or take one second.
        let start = Instant::now();
        let mut cadence = Cadence::opened_at(start);

        let window = cadence.observe(start + Duration::from_secs(2));
        assert_eq!(window.ago(), Duration::from_secs(1));
        assert_eq!(window.precision(), Duration::from_secs(1));

        // And the window that follows is measured from the previous payload,
        // not from the start: 100 ms later means 50 ms ago, not 1.05 s.
        let window = cadence.observe(start + Duration::from_millis(2100));
        assert_eq!(window.ago(), Duration::from_millis(50));
        assert_eq!(window.precision(), Duration::from_millis(50));
    }

    #[test]
    fn a_long_silence_widens_the_precision_rather_than_being_tidied_away() {
        // A paused game, or a plugin that was not listening for a while, knows
        // less about when something happened. Saying so is the whole difference
        // between a mark a user can trust and one they cannot.
        let start = Instant::now();
        let mut cadence = Cadence::opened_at(start);
        let window = cadence.observe(start + Duration::from_secs(120));
        assert_eq!(window.precision(), Duration::from_secs(60));
    }

    #[test]
    fn a_reported_event_is_certain_about_what_and_unsure_only_about_when() {
        let start = Instant::now();
        let mut cadence = Cadence::opened_at(start);
        let window = cadence.observe(start + Duration::from_millis(200));

        let report = window.report(EventKind::Kill, Map::new());
        assert_eq!(report.kind, EventKind::Kill);
        assert_eq!(report.ago_ns, 100_000_000);
        assert_eq!(report.precision_ns, 100_000_000);
        assert!(
            (report.confidence - 1.0).abs() < f32::EPSILON,
            "the game said it happened; there is nothing to be unsure about"
        );
    }

    #[test]
    fn a_clock_that_does_not_move_produces_no_claim_of_exactness() {
        // Two payloads at the same instant is a game posting twice in one tick.
        // The window is empty, so `ago` is zero — which is honest — and so is
        // `precision`, which is the claim "I timed this exactly". It is the one
        // case where that claim is true: the plugin heard both states at the
        // same reading of its own clock.
        let start = Instant::now();
        let mut cadence = Cadence::opened_at(start);
        let window = cadence.observe(start);
        assert_eq!(window.ago(), Duration::ZERO);
        assert_eq!(window.precision(), Duration::ZERO);
    }
}
