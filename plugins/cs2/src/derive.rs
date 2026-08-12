//! Turning state snapshots into events, without inventing any.
//!
//! Game State Integration reports **state**, not events. A kill is the
//! difference between two payloads: `match_stats.kills` was 8, and now it is 9.
//! Everything in this module follows from that one sentence, and so does every
//! way it could go wrong.
//!
//! # The rule
//!
//! > **An event is reported only for a transition this plugin observed
//! > directly, between two payloads it accepted.**
//!
//! Four things follow, and each of them is a test below.
//!
//! **The first payload produces nothing.** It is a baseline. A plugin attached
//! to a game that is already in the third round of a match knows the score, and
//! knows nothing about how it got there; reporting `match_started` at the
//! moment it happened to look would put a mark on a timeline at a moment
//! nothing happened. The recording keeps whatever it sees from then on.
//!
//! **A payload older than the last one accepted is discarded.** Each post is a
//! separate TCP connection to a loopback port, so two of them can arrive out of
//! order, and a difference measured against a *newer* baseline is a negative
//! number of kills — or, once the next payload arrives, the same kills counted
//! twice. `provider.timestamp` is the only ordering information a payload
//! carries, so a payload stamped earlier than the last one accepted is dropped
//! whole. Payloads stamped in the same second are accepted in arrival order,
//! because within a second there is nothing to order them by, and pretending
//! otherwise would be a guess.
//!
//! **A counter that goes backwards is not a negative event.** Rejoining a
//! match, a new match on the same map, a warm-up ending: all reset the match
//! statistics. A decrease means this plugin's baseline is wrong, so the
//! baseline is replaced and nothing is reported for the step. Subtracting in
//! the other direction would be worse than useless.
//!
//! **A payload about somebody else is not about the player.** After dying, the
//! camera follows a teammate and the `player` block follows the camera
//! ([`GsiPayload::describes_the_local_player`]). Their kills are not the
//! player's, and are neither reported nor taken as a baseline — taking them as
//! a baseline would produce a spurious decrease the moment the camera came
//! back.
//!
//! # Where an event sits in time, and how precisely
//!
//! A payload says a counter changed; it does not say when. What this plugin
//! knows is that the change happened after the previous payload it accepted and
//! no later than this one. So the moment reported is the **middle of that
//! window** and the precision is **half of it**, which is exactly the claim the
//! event model asks a source to make (`docs/plugin-api.md`, "Timing"): `at` in
//! the middle of the window it is sure about, `precision` covering the rest.
//!
//! With the throttle `crate::integration` configures, that window is a tenth of
//! a second while a round is being played, so `precision` is around 50 ms. It
//! widens to the heartbeat interval when nothing at all is changing, which is
//! honest rather than convenient: a kill after two minutes of a still main menu
//! is not a thing that happens.
//!
//! Every event derived from one payload shares that moment. Two kills in one
//! step really are two kills the plugin cannot separate, and giving them
//! different times would be inventing an order.
//!
//! # What this deliberately does not report
//!
//! **A weapon.** The obvious `data` field for a kill, and it is not derivable:
//! the payload carries the weapon the player is *holding when it arrives*,
//! which after a kill is very often the next one they switched to. Reporting it
//! would be a plausible-looking guess, which is worse than an absent field
//! (AGENTS.md section 27).
//!
//! **A `match_ended` for a match this plugin never saw end.** Leaving a match
//! and joining another produces a `match_started` for the new one and nothing
//! for the old, because the moment the old one ended is not a moment this
//! plugin observed.
//!
//! **A `win` or `loss` per round.** Those two kinds are how it ended for the
//! player, and a round is not how anything ended; `round_ended` carries the
//! winning side in its payload, where a consumer that cares can find it.

use core::time::Duration;
use std::time::Instant;

use clipped_events::EventKind;
use serde_json::{Map, Value};

use crate::payload::{GsiPayload, MapPhase, MapState, MatchStats, RoundPhase, Team};

/// How sure this plugin is that an event it reports happened.
///
/// One. Game State Integration is the game telling the truth about itself: it
/// is an authoritative feed, not a detector, so there is nothing to be unsure
/// about (`docs/plugin-api.md`, "Confidence, and what it is not"). What *is*
/// uncertain is when, and that is `precision`'s job.
pub const CONFIDENCE: f32 = 1.0;

/// An event this plugin derived, before it is a report.
///
/// [`at`](Self::at) is a reading of this process's own clock, because that is
/// what a plugin owns: the moment the report is written, the host is told how
/// long *ago* this was, and it places the event on the recording's timeline
/// (`docs/plugin-api.md`, "How long ago, not when").
#[derive(Debug, Clone, PartialEq)]
pub struct DerivedEvent {
    /// What happened.
    pub kind: EventKind,
    /// The middle of the window it happened in.
    pub at: Instant,
    /// Half that window: how far either side of `at` the truth may lie.
    pub precision: Duration,
    /// Counter-Strike's own words, for whoever knows what they mean.
    pub data: Map<String, Value>,
}

/// Something worth saying about a payload that is not an event.
///
/// These are why a payload produced fewer events than somebody expected, and
/// each of them is a case that would otherwise be a silent gap (AGENTS.md
/// section 15). The plugin writes them to its standard error, which is the
/// host's, and the tests assert them: "no events" and "no events, because the
/// payload was stale" are not the same result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepNote {
    /// The first payload of a session, taken as the baseline.
    Baselined,
    /// A payload stamped earlier than the last one accepted, discarded whole.
    Stale {
        /// The timestamp it carried.
        stamped: i64,
        /// The timestamp of the last payload accepted.
        last_accepted: i64,
    },
    /// A payload with no `provider.timestamp`, which cannot be ordered.
    ///
    /// Accepted, because the alternative is discarding every payload from a
    /// configuration that did not subscribe to the provider block, and said out
    /// loud, because it means the ordering guard is not protecting this
    /// session.
    NotOrderable,
    /// The `player` block described somebody else, so it was ignored.
    AboutAnotherPlayer,
    /// A match counter decreased. The baseline was replaced and nothing was
    /// reported for the step.
    CountersReset,
}

/// The result of one payload.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Step {
    /// What to report, in the order it is reported: what opened, then what
    /// happened inside it, then what closed.
    pub events: Vec<DerivedEvent>,
    /// What else is worth knowing about the payload.
    pub notes: Vec<StepNote>,
}

impl Step {
    /// The kinds derived, in order. Convenience for tests and for logging.
    #[must_use]
    pub fn kinds(&self) -> Vec<&EventKind> {
        self.events.iter().map(|event| &event.kind).collect()
    }
}

/// Everything the last accepted payload said, so that the next one can be
/// compared against it.
///
/// It is deliberately *not* a copy of the payload: it is the handful of values
/// a difference is taken over, so that adding a field to the payload types
/// cannot silently start affecting what is reported.
#[derive(Debug, Default)]
struct Baseline {
    stamped: Option<i64>,
    received: Option<Instant>,
    map_name: Option<String>,
    map_phase: Option<MapPhase>,
    round_phase: Option<RoundPhase>,
    /// Only ever the local player's, and only when the payload proved it.
    stats: Option<MatchStats>,
    round_kills: Option<u32>,
    round_headshot_kills: Option<u32>,
}

/// Follows a match across payloads and reports what changed.
///
/// One per attached session. It holds no clock of its own: the caller supplies
/// the moment each payload arrived, which is what makes every case below
/// testable without waiting for anything.
#[derive(Debug, Default)]
pub struct MatchTracker {
    baseline: Baseline,
}

impl MatchTracker {
    /// A tracker that has seen nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads one payload and reports what it says happened.
    ///
    /// `received` is when this process took delivery of it.
    pub fn observe(&mut self, payload: &GsiPayload, received: Instant) -> Step {
        let mut step = Step::default();

        let stamped = payload.timestamp();
        match (stamped, self.baseline.stamped) {
            (Some(stamped), Some(last_accepted)) if stamped < last_accepted => {
                // Discarded whole, and the baseline is left exactly as it was.
                // A stale payload is not evidence about anything: believing any
                // part of it would move the baseline backwards, and the next
                // payload would then report the difference a second time.
                step.notes.push(StepNote::Stale {
                    stamped,
                    last_accepted,
                });
                return step;
            }
            (None, _) => step.notes.push(StepNote::NotOrderable),
            _ => {}
        }

        let Some(window) = self.window_ending_at(received) else {
            // The first payload. Everything about it becomes the baseline and
            // none of it becomes an event.
            self.adopt(payload, received, stamped, &step);
            step.notes.push(StepNote::Baselined);
            return step;
        };

        // In the order a reader would tell it: what opened, then what happened
        // inside it, then what closed. Every one of them carries the same
        // moment, because one payload cannot separate them.
        self.derive_match(payload, window, &mut step);
        let (opened, closed) = self.round_transition(payload, window);
        step.events.extend(opened);
        self.derive_player(payload, window, &mut step);
        step.events.extend(closed);
        self.close_match(payload, window, &mut step);

        self.adopt(payload, received, stamped, &step);
        step
    }

    /// The window this payload's changes must have happened in.
    ///
    /// `None` before there is a previous payload to bound it.
    fn window_ending_at(&self, received: Instant) -> Option<Window> {
        let previous = self.baseline.received?;
        Some(Window {
            previous,
            // A monotonic clock cannot go backwards, but `received` is supplied
            // by a caller, so the subtraction is saturating rather than
            // trusting.
            span: received.saturating_duration_since(previous),
        })
    }

    /// `match_started`: a map this plugin was not already watching, or the same
    /// map starting again.
    ///
    /// The second half matters more than it looks. Two matches in a row on one
    /// map — a rematch, or a queue that lands on it twice — never change
    /// `map.name`, so a rule about the name alone reports `match_ended` for the
    /// second match and nothing for its beginning. The map phase leaving
    /// `gameover` is that beginning, and it is a transition observed directly
    /// between two payloads, which is the only kind this plugin reports.
    fn derive_match(&mut self, payload: &GsiPayload, window: Window, step: &mut Step) {
        let map = payload.map.as_ref();
        let name = map.and_then(|map| map.name.as_deref());
        let Some(name) = name else {
            return;
        };
        if self.baseline.map_name.as_deref() == Some(name) && !self.left_game_over(map) {
            return;
        }

        let mut data = Map::new();
        data.insert("map".to_owned(), Value::String(name.to_owned()));
        if let Some(mode) = map.and_then(|map| map.mode.as_deref()) {
            data.insert("mode".to_owned(), Value::String(mode.to_owned()));
        }
        step.events
            .push(window.event(EventKind::MatchStarted, data));

        // A new match means new counters. Adopting them without reporting is
        // what stops the first payload of a fresh match reporting the previous
        // match's kills all over again.
        self.baseline.stats = None;
        self.baseline.round_kills = None;
        self.baseline.round_headshot_kills = None;
    }

    /// Whether the map has just come out of `gameover`.
    ///
    /// Counter-Strike keeps posting while the end-of-match scoreboard is up, so
    /// the interesting moment is the phase changing to something that is not
    /// `gameover` — warm-up, or straight into a live round.
    ///
    /// A payload whose map carries no phase at all is not a transition and
    /// reports nothing here. It is also not evidence that the phase changed, so
    /// `adopt` leaves the baseline phase exactly as it was rather than clearing
    /// it: the payload is passed over, not acted on. Clearing it would decline
    /// this payload *and* the next one, because the `gameover` that the next
    /// payload's warm-up has to be a transition from would be gone — one
    /// phase-less payload in the wrong place would swallow a `match_started`
    /// with nothing in the log to say so.
    fn left_game_over(&self, map: Option<&MapState>) -> bool {
        if self.baseline.map_phase.as_ref() != Some(&MapPhase::GameOver) {
            return false;
        }
        map.and_then(|map| map.phase.as_ref())
            .is_some_and(|phase| *phase != MapPhase::GameOver)
    }

    /// The round phase changing: a round that opened, and a round that closed.
    ///
    /// Two return values rather than one so that the caller can put a
    /// `round_started` before the kills inside it and a `round_ended` after
    /// them. Only one of the two is ever `Some`; a phase change is one
    /// transition.
    fn round_transition(
        &self,
        payload: &GsiPayload,
        window: Window,
    ) -> (Option<DerivedEvent>, Option<DerivedEvent>) {
        let Some(phase) = payload
            .round
            .as_ref()
            .and_then(|round| round.phase.as_ref())
        else {
            return (None, None);
        };
        if self.baseline.round_phase.as_ref() == Some(phase) {
            return (None, None);
        }

        let mut data = Map::new();
        if let Some(round) = payload.map.as_ref().and_then(|map| map.round) {
            data.insert("round".to_owned(), Value::from(round));
        }

        match phase {
            RoundPhase::Live => (Some(window.event(EventKind::RoundStarted, data)), None),
            RoundPhase::Over => {
                if let Some(winner) = payload
                    .round
                    .as_ref()
                    .and_then(|round| round.win_team.as_ref())
                {
                    data.insert(
                        "win_team".to_owned(),
                        Value::String(winner.as_str().to_owned()),
                    );
                }
                (None, Some(window.event(EventKind::RoundEnded, data)))
            }
            // Freeze time is the gap between one round ending and the next
            // starting, and neither of those is it. An unrecognised phase from
            // a future update is declined for the same reason: this build does
            // not know what it means, and a mark on a timeline it cannot
            // explain is worse than a missing one.
            RoundPhase::FreezeTime | RoundPhase::Other(_) => (None, None),
        }
    }

    /// `kill`, `death` and `assist`, from the match counters moving.
    fn derive_player(&mut self, payload: &GsiPayload, window: Window, step: &mut Step) {
        if payload.player.is_none() {
            return;
        }
        if !payload.describes_the_local_player() {
            step.notes.push(StepNote::AboutAnotherPlayer);
            return;
        }
        let Some(stats) = payload
            .player
            .as_ref()
            .and_then(|player| player.match_stats)
        else {
            return;
        };
        let Some(previous) = self.baseline.stats else {
            // First sight of the local player's counters: a baseline, not a
            // diff against zero.
            return;
        };

        let moves = (
            delta(previous.kills, stats.kills),
            delta(previous.assists, stats.assists),
            delta(previous.deaths, stats.deaths),
        );
        let (Some(kills), Some(assists), Some(deaths)) = moves else {
            step.notes.push(StepNote::CountersReset);
            return;
        };

        let headshot = self.headshot_of_a_single_kill(payload, kills);
        for _ in 0..kills {
            let mut data = Map::new();
            if let Some(headshot) = headshot {
                data.insert("headshot".to_owned(), Value::Bool(headshot));
            }
            step.events.push(window.event(EventKind::Kill, data));
        }
        for _ in 0..deaths {
            step.events.push(window.event(EventKind::Death, Map::new()));
        }
        for _ in 0..assists {
            step.events
                .push(window.event(EventKind::Assist, Map::new()));
        }
    }

    /// Whether one kill was a headshot, when that can be said at all.
    ///
    /// `round_killhs` counts headshot kills this round, so a headshot is
    /// attributable only when the step contains exactly one kill *and* the
    /// round counters moved by exactly one alongside it. Two kills in a step
    /// with one headshot between them says nothing about which; a round that
    /// reset in between says nothing at all. Both answer `None`, and the field
    /// is left off rather than guessed.
    fn headshot_of_a_single_kill(&self, payload: &GsiPayload, kills: u32) -> Option<bool> {
        if kills != 1 {
            return None;
        }
        // Every one of these is required rather than defaulted. A configuration
        // that does not subscribe to `player_state`, or a round that reset in
        // between, leaves the field off — which is the honest answer, and the
        // reason `delta`'s "absent counts as no movement" is not used here.
        let state = payload.player.as_ref()?.state.as_ref()?;
        let round_kills = state.round_kills?.checked_sub(self.baseline.round_kills?)?;
        if round_kills != 1 {
            return None;
        }
        let headshots = state
            .round_headshot_kills?
            .checked_sub(self.baseline.round_headshot_kills?)?;
        match headshots {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }

    /// `match_ended`, and how it ended for the player.
    fn close_match(&mut self, payload: &GsiPayload, window: Window, step: &mut Step) {
        let Some(map) = payload.map.as_ref() else {
            return;
        };
        if map.phase.as_ref() != Some(&MapPhase::GameOver) {
            return;
        }
        if self.baseline.map_phase.as_ref() == Some(&MapPhase::GameOver) {
            return;
        }

        step.events
            .push(window.event(EventKind::MatchEnded, Map::new()));
        if let Some(kind) = outcome_for_the_player(payload, map) {
            step.events.push(window.event(kind, Map::new()));
        }
    }

    /// Replaces the baseline with what this payload said.
    fn adopt(
        &mut self,
        payload: &GsiPayload,
        received: Instant,
        stamped: Option<i64>,
        step: &Step,
    ) {
        self.baseline.received = Some(received);
        if let Some(stamped) = stamped {
            self.baseline.stamped = Some(stamped);
        }

        if let Some(map) = payload.map.as_ref() {
            // Field by field, and only when the field is there. A map block is
            // not a complete map: Game State Integration sends what the
            // configuration subscribed to, and a component this build does not
            // recognise parses to `None` as well. Replacing the baseline with
            // an absence would make the *next* payload the first sight of a
            // value that never went away — which for `phase` destroys the
            // `gameover` the match's beginning is derived from, so a single
            // phase-less payload between the scoreboard and the next warm-up
            // costs a `match_started` permanently. "Says nothing" and "says
            // nothing is there" are different payloads.
            if map.name.is_some() {
                self.baseline.map_name.clone_from(&map.name);
            }
            if map.phase.is_some() {
                self.baseline.map_phase.clone_from(&map.phase);
            }
        } else {
            // Back to the main menu, which is the whole map block being absent
            // rather than a field of it. The next map is a new match.
            self.baseline.map_name = None;
            self.baseline.map_phase = None;
        }
        self.baseline.round_phase = payload.round.as_ref().and_then(|round| round.phase.clone());

        // A payload about somebody else says nothing about the player's
        // counters, so it must not replace them: adopting a teammate's kills
        // would produce a decrease — and a `CountersReset` — the moment the
        // camera came back.
        if step.notes.contains(&StepNote::AboutAnotherPlayer) {
            return;
        }
        if let Some(player) = payload.player.as_ref() {
            if let Some(stats) = player.match_stats {
                self.baseline.stats = Some(stats);
            }
            if let Some(state) = player.state.as_ref() {
                self.baseline.round_kills = state.round_kills;
                self.baseline.round_headshot_kills = state.round_headshot_kills;
            }
        }
    }
}

/// The span one payload's changes must have happened in.
#[derive(Debug, Clone, Copy)]
struct Window {
    previous: Instant,
    span: Duration,
}

impl Window {
    /// An event in the middle of the window, precise to half of it.
    fn event(self, kind: EventKind, data: Map<String, Value>) -> DerivedEvent {
        let half = self.span / 2;
        DerivedEvent {
            kind,
            at: self.previous + half,
            precision: half,
            data,
        }
    }
}

/// How far a counter moved.
///
/// `None` means it moved **backwards**, which is not a difference and is every
/// caller's reason to rebaseline rather than report. A counter missing from
/// either payload has not been seen to move at all and answers `Some(0)`: a
/// configuration that did not subscribe to a block should report nothing from
/// it, not treat every payload as a reset.
fn delta(previous: Option<u32>, current: Option<u32>) -> Option<u32> {
    match (previous, current) {
        (Some(previous), Some(current)) => current.checked_sub(previous),
        _ => Some(0),
    }
}

/// `win` or `loss`, when the payload says enough to tell.
///
/// A draw, a missing score or a player on neither side gives `None`, and
/// `match_ended` is reported on its own. There is no `draw` kind, and picking
/// one of `win` or `loss` anyway would be a coin toss recorded as a fact.
fn outcome_for_the_player(payload: &GsiPayload, map: &MapState) -> Option<EventKind> {
    let counter_terrorists = map.team_ct.as_ref()?.score?;
    let terrorists = map.team_t.as_ref()?.score?;
    if counter_terrorists == terrorists {
        return None;
    }

    let winners = if counter_terrorists > terrorists {
        Team::CounterTerrorist
    } else {
        Team::Terrorist
    };
    match payload.player.as_ref()?.team.as_ref()? {
        Team::Other(_) => None,
        side if *side == winners => Some(EventKind::Win),
        _ => Some(EventKind::Loss),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCAL: &str = "76561198000000001";

    fn at(seconds: u64) -> Instant {
        // A fixed origin plus an offset, so that every window in a test is a
        // number the test states rather than however long the test took.
        origin() + Duration::from_secs(seconds)
    }

    fn origin() -> Instant {
        // One reading, taken once, shared by every `at` in a test.
        static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        *ORIGIN.get_or_init(Instant::now)
    }

    /// A payload builder, so that each test states only what it is about.
    fn payload(json: Value) -> GsiPayload {
        GsiPayload::parse(json.to_string().as_bytes()).expect("the test payload reads")
    }

    fn live(stamp: i64, kills: u32, deaths: u32, assists: u32) -> GsiPayload {
        payload(serde_json::json!({
            "provider": {"steamid": LOCAL, "timestamp": stamp},
            "map": {"name": "de_dust2", "mode": "competitive", "phase": "live", "round": 3,
                    "team_ct": {"score": 2}, "team_t": {"score": 1}},
            "round": {"phase": "live"},
            "player": {"steamid": LOCAL, "team": "CT",
                       "state": {"round_kills": 0, "round_killhs": 0},
                       "match_stats": {"kills": kills, "deaths": deaths, "assists": assists}}
        }))
    }

    #[test]
    fn the_first_payload_is_a_baseline_and_reports_nothing() {
        // A plugin attached to a game already in a match knows the score and
        // nothing about how it got there. Reporting `match_started` here would
        // put a mark at a moment nothing happened.
        let mut tracker = MatchTracker::new();
        let step = tracker.observe(&live(10, 8, 6, 3), at(0));

        assert!(
            step.events.is_empty(),
            "the first payload described a match already underway: {:?}",
            step.kinds()
        );
        assert_eq!(step.notes, vec![StepNote::Baselined]);
    }

    #[test]
    fn a_kill_is_the_difference_between_two_payloads() {
        let mut tracker = MatchTracker::new();
        tracker.observe(&live(10, 8, 6, 3), at(0));
        let step = tracker.observe(&live(11, 9, 6, 3), at(2));

        assert_eq!(step.kinds(), vec![&EventKind::Kill]);
        let kill = &step.events[0];
        assert_eq!(
            kill.at,
            at(1),
            "the moment reported is the middle of the window the change happened in"
        );
        assert_eq!(
            kill.precision,
            Duration::from_secs(1),
            "and the precision is half the window, which is the rest of the claim"
        );
    }

    #[test]
    fn several_kills_in_one_payload_are_several_events_at_one_moment() {
        let mut tracker = MatchTracker::new();
        tracker.observe(&live(10, 0, 0, 0), at(0));
        let step = tracker.observe(&live(11, 3, 1, 2), at(1));

        assert_eq!(
            step.kinds(),
            vec![
                &EventKind::Kill,
                &EventKind::Kill,
                &EventKind::Kill,
                &EventKind::Death,
                &EventKind::Assist,
                &EventKind::Assist,
            ]
        );
        assert!(
            step.events
                .iter()
                .all(|event| event.at == step.events[0].at),
            "one payload cannot separate them, so neither may the plugin"
        );
    }

    #[test]
    fn a_payload_from_an_earlier_second_is_discarded_whole() {
        // Each post is its own connection to a loopback port, so two can arrive
        // out of order. This is the case the ordering guard exists for: without
        // it, the stale payload reports minus one kill and the next payload
        // reports the same kill a second time.
        let mut tracker = MatchTracker::new();
        tracker.observe(&live(10, 8, 6, 3), at(0));
        let overtaken = tracker.observe(&live(11, 9, 6, 3), at(1));
        assert_eq!(overtaken.kinds(), vec![&EventKind::Kill]);

        let stale = tracker.observe(&live(10, 8, 6, 3), at(2));
        assert!(
            stale.events.is_empty(),
            "a payload from before the baseline invented: {:?}",
            stale.kinds()
        );
        assert_eq!(
            stale.notes,
            vec![StepNote::Stale {
                stamped: 10,
                last_accepted: 11
            }]
        );

        // And the baseline it did not move: the next payload reports one kill,
        // not two.
        let next = tracker.observe(&live(12, 10, 6, 3), at(3));
        assert_eq!(next.kinds(), vec![&EventKind::Kill]);
    }

    #[test]
    fn payloads_stamped_in_the_same_second_are_both_accepted() {
        // The guard refuses what it can prove is old. Within one second there
        // is nothing to order by, and refusing an equal stamp would drop most
        // of a live round.
        let mut tracker = MatchTracker::new();
        tracker.observe(&live(10, 8, 6, 3), at(0));
        let first = tracker.observe(&live(10, 9, 6, 3), at(1));
        let second = tracker.observe(&live(10, 10, 6, 3), at(2));

        assert_eq!(first.kinds(), vec![&EventKind::Kill]);
        assert_eq!(second.kinds(), vec![&EventKind::Kill]);
    }

    #[test]
    fn a_payload_with_no_timestamp_is_accepted_and_says_so() {
        let mut tracker = MatchTracker::new();
        let unordered = payload(serde_json::json!({
            "player": {"steamid": LOCAL},
            "map": {"name": "de_nuke"}
        }));

        let first = tracker.observe(&unordered, at(0));
        assert!(first.notes.contains(&StepNote::NotOrderable));
        assert!(first.notes.contains(&StepNote::Baselined));
    }

    #[test]
    fn a_counter_going_backwards_rebaselines_rather_than_reporting() {
        let mut tracker = MatchTracker::new();
        tracker.observe(&live(10, 8, 6, 3), at(0));
        let reset = tracker.observe(&live(11, 0, 0, 0), at(1));

        assert!(
            reset.events.is_empty(),
            "a reset counter is not a negative event: {:?}",
            reset.kinds()
        );
        assert_eq!(reset.notes, vec![StepNote::CountersReset]);

        // Rebaselined, so the next kill after the reset is one kill.
        let next = tracker.observe(&live(12, 1, 0, 0), at(2));
        assert_eq!(next.kinds(), vec![&EventKind::Kill]);
    }

    #[test]
    fn a_spectated_teammates_kills_are_neither_reported_nor_believed() {
        let mut tracker = MatchTracker::new();
        tracker.observe(&live(10, 8, 6, 3), at(0));

        let teammate = payload(serde_json::json!({
            "provider": {"steamid": LOCAL, "timestamp": 11},
            "map": {"name": "de_dust2", "mode": "competitive", "phase": "live", "round": 3,
                    "team_ct": {"score": 2}, "team_t": {"score": 1}},
            "round": {"phase": "live"},
            "player": {"steamid": "76561198000000002", "team": "CT",
                       "state": {"round_kills": 4, "round_killhs": 3},
                       "match_stats": {"kills": 21, "deaths": 5, "assists": 4}}
        }));
        let spectating = tracker.observe(&teammate, at(1));

        assert!(
            spectating.events.is_empty(),
            "thirteen of somebody else's kills were reported as the player's: {:?}",
            spectating.kinds()
        );
        assert_eq!(spectating.notes, vec![StepNote::AboutAnotherPlayer]);

        // And the teammate's counters were not adopted: coming back to the
        // player's own view is one kill, not a reset from twenty-one.
        let back = tracker.observe(&live(12, 9, 6, 3), at(2));
        assert_eq!(back.kinds(), vec![&EventKind::Kill]);
        assert!(back.notes.is_empty());
    }

    #[test]
    fn a_headshot_is_reported_only_when_one_kill_can_be_attributed() {
        let with_round_counters = |stamp: i64, kills: u32, round: u32, headshots: u32| {
            payload(serde_json::json!({
                "provider": {"steamid": LOCAL, "timestamp": stamp},
                "map": {"name": "de_dust2", "phase": "live", "round": 3},
                "round": {"phase": "live"},
                "player": {"steamid": LOCAL, "team": "CT",
                           "state": {"round_kills": round, "round_killhs": headshots},
                           "match_stats": {"kills": kills, "deaths": 0, "assists": 0}}
            }))
        };

        let mut tracker = MatchTracker::new();
        tracker.observe(&with_round_counters(10, 0, 0, 0), at(0));

        let headshot = tracker.observe(&with_round_counters(11, 1, 1, 1), at(1));
        assert_eq!(headshot.events[0].data["headshot"], Value::Bool(true));

        let body = tracker.observe(&with_round_counters(12, 2, 2, 1), at(2));
        assert_eq!(body.events[0].data["headshot"], Value::Bool(false));

        // Two kills in one step, one headshot between them: which of the two it
        // was is not in the payload, so the field is absent rather than guessed.
        let ambiguous = tracker.observe(&with_round_counters(13, 4, 4, 2), at(3));
        assert_eq!(ambiguous.kinds(), vec![&EventKind::Kill, &EventKind::Kill]);
        assert!(
            ambiguous
                .events
                .iter()
                .all(|event| !event.data.contains_key("headshot")),
            "a headshot was attributed to one of two kills it could not be attributed to"
        );

        // The case the round counters alone cannot see. A new round resets
        // them, so after this payload the baseline is zero…
        let new_round = tracker.observe(&with_round_counters(14, 5, 0, 0), at(4));
        assert_eq!(new_round.kinds(), vec![&EventKind::Kill]);

        // …and now two kills arrive together, of which the round says one was a
        // headshot. The round counters agree perfectly — one kill, one headshot
        // — and they are describing a different number of kills from the match
        // total, so neither event may claim it.
        let across_a_round = tracker.observe(&with_round_counters(15, 7, 1, 1), at(5));
        assert_eq!(
            across_a_round.kinds(),
            vec![&EventKind::Kill, &EventKind::Kill]
        );
        assert!(
            across_a_round
                .events
                .iter()
                .all(|event| !event.data.contains_key("headshot")),
            "the round counters and the match total disagreed about how many kills there were, \
             and a headshot was attributed anyway"
        );
    }

    #[test]
    fn a_kill_never_claims_a_weapon() {
        // The payload carries the weapon held when it arrived, which after a
        // kill is very often the next one. Stated as a test because it is a
        // decision, not an omission.
        let mut tracker = MatchTracker::new();
        tracker.observe(&live(10, 0, 0, 0), at(0));
        let step = tracker.observe(&live(11, 1, 0, 0), at(1));

        assert!(!step.events[0].data.contains_key("weapon"));
    }

    #[test]
    fn a_new_map_starts_a_match_and_resets_the_counters() {
        let mut tracker = MatchTracker::new();
        tracker.observe(&live(10, 8, 6, 3), at(0));

        let next_map = payload(serde_json::json!({
            "provider": {"steamid": LOCAL, "timestamp": 20},
            "map": {"name": "de_nuke", "mode": "competitive", "phase": "warmup", "round": 0},
            "player": {"steamid": LOCAL, "team": "T",
                       "match_stats": {"kills": 0, "deaths": 0, "assists": 0}}
        }));
        let step = tracker.observe(&next_map, at(1));

        assert_eq!(step.kinds(), vec![&EventKind::MatchStarted]);
        assert_eq!(step.events[0].data["map"], Value::String("de_nuke".into()));
        assert_eq!(
            step.events[0].data["mode"],
            Value::String("competitive".into())
        );
        assert!(
            step.notes.is_empty(),
            "a new match's counters starting from zero is not a reset to report: {:?}",
            step.notes
        );
    }

    #[test]
    fn joining_a_match_already_underway_reports_it_starting_and_nothing_that_happened_before() {
        // The counters a new match starts with are usually zero, which hides
        // this: a plugin that treated a missing baseline as zero would look
        // correct on every ordinary map change and would report nineteen kills
        // the first time somebody joined a match in progress.
        let mut tracker = MatchTracker::new();
        tracker.observe(&live(10, 8, 6, 3), at(0));

        let underway = payload(serde_json::json!({
            "provider": {"steamid": LOCAL, "timestamp": 20},
            "map": {"name": "de_nuke", "mode": "competitive", "phase": "live", "round": 14,
                    "team_ct": {"score": 9}, "team_t": {"score": 5}},
            "player": {"steamid": LOCAL, "team": "T",
                       "match_stats": {"kills": 19, "deaths": 11, "assists": 4}}
        }));
        let step = tracker.observe(&underway, at(1));

        assert_eq!(
            step.kinds(),
            vec![&EventKind::MatchStarted],
            "the match this plugin joined is reported starting; what happened in it before is \
             not something this plugin saw"
        );

        // And the counters it joined at are the baseline, so the next kill is
        // one kill rather than twenty.
        let next = payload(serde_json::json!({
            "provider": {"steamid": LOCAL, "timestamp": 21},
            "map": {"name": "de_nuke", "mode": "competitive", "phase": "live", "round": 14,
                    "team_ct": {"score": 9}, "team_t": {"score": 5}},
            "player": {"steamid": LOCAL, "team": "T",
                       "match_stats": {"kills": 20, "deaths": 11, "assists": 4}}
        }));
        assert_eq!(
            tracker.observe(&next, at(2)).kinds(),
            vec![&EventKind::Kill]
        );
    }

    #[test]
    fn a_match_this_plugin_never_saw_end_produces_no_match_ended() {
        // Leaving one match for another is not a moment this plugin observed
        // the first one ending, and `match_ended` at the moment the second one
        // was noticed would be a mark in the wrong place.
        let mut tracker = MatchTracker::new();
        tracker.observe(&live(10, 8, 6, 3), at(0));

        let elsewhere = payload(serde_json::json!({
            "provider": {"steamid": LOCAL, "timestamp": 20},
            "map": {"name": "de_nuke", "phase": "warmup"},
            "player": {"steamid": LOCAL}
        }));
        let step = tracker.observe(&elsewhere, at(1));

        assert_eq!(step.kinds(), vec![&EventKind::MatchStarted]);
    }

    #[test]
    fn game_over_ends_the_match_once_and_says_how_it_went() {
        let over = |team: &str, ct: u32, t: u32| {
            payload(serde_json::json!({
                "provider": {"steamid": LOCAL, "timestamp": 30},
                "map": {"name": "de_dust2", "phase": "gameover", "round": 20,
                        "team_ct": {"score": ct}, "team_t": {"score": t}},
                "player": {"steamid": LOCAL, "team": team,
                           "match_stats": {"kills": 8, "deaths": 6, "assists": 3}}
            }))
        };

        let mut winning = MatchTracker::new();
        winning.observe(&live(10, 8, 6, 3), at(0));
        let won = winning.observe(&over("CT", 13, 7), at(1));
        assert_eq!(won.kinds(), vec![&EventKind::MatchEnded, &EventKind::Win]);

        // The same payload from the other side of the scoreboard.
        let mut losing = MatchTracker::new();
        losing.observe(&live(10, 8, 6, 3), at(0));
        let lost = losing.observe(&over("T", 13, 7), at(1));
        assert_eq!(lost.kinds(), vec![&EventKind::MatchEnded, &EventKind::Loss]);

        // Game over does not keep ending: the phase is already `gameover`, and
        // Counter-Strike keeps posting while the scoreboard is up.
        let again = winning.observe(&over("CT", 13, 7), at(2));
        assert!(
            again.events.is_empty(),
            "the match ended twice: {:?}",
            again.kinds()
        );
    }

    #[test]
    fn a_second_match_on_the_same_map_is_reported_starting() {
        // Two matches in a row on one map is an ordinary evening, and
        // `map.name` never changes across it. A rule about the name alone gives
        // the second match a `match_ended` and no beginning — a timeline with
        // two endings and one start on it.
        let phase = |stamp: i64, phase: &str, kills: u32| {
            payload(serde_json::json!({
                "provider": {"steamid": LOCAL, "timestamp": stamp},
                "map": {"name": "de_dust2", "mode": "competitive", "phase": phase, "round": 20,
                        "team_ct": {"score": 13}, "team_t": {"score": 7}},
                "player": {"steamid": LOCAL, "team": "CT",
                           "match_stats": {"kills": kills, "deaths": 6, "assists": 3}}
            }))
        };

        let mut tracker = MatchTracker::new();
        tracker.observe(&live(10, 8, 6, 3), at(0));

        let ended = tracker.observe(&phase(30, "gameover", 8), at(1));
        assert_eq!(ended.kinds(), vec![&EventKind::MatchEnded, &EventKind::Win]);

        // The scoreboard is up and the game keeps posting: still one ending.
        assert!(tracker
            .observe(&phase(31, "gameover", 8), at(2))
            .events
            .is_empty());

        // And now the next match begins on the same map.
        let started = tracker.observe(&phase(40, "warmup", 0), at(3));
        assert_eq!(
            started.kinds(),
            vec![&EventKind::MatchStarted],
            "the match that started after the last one ended was never reported starting"
        );
        assert_eq!(
            started.events[0].data["map"],
            Value::String("de_dust2".into())
        );
        assert!(
            started.notes.is_empty(),
            "a new match's counters starting from zero is not a reset to report: {:?}",
            started.notes
        );

        // The counters it starts from are the new match's, so the first kill of
        // it is one kill rather than the whole of the last match again.
        let first_kill = tracker.observe(&phase(41, "live", 1), at(4));
        assert_eq!(first_kill.kinds(), vec![&EventKind::Kill]);
    }

    #[test]
    fn a_map_block_without_a_phase_does_not_erase_the_game_over_the_next_match_starts_from() {
        // The sequence the guard above depends on, with one payload inserted
        // into it that carries a map block and no `phase`: a configuration that
        // does not subscribe to the whole of `map`, a component this build does
        // not recognise, or a truncated post.
        //
        // Declining to report anything for *that* payload is right. Taking it
        // as evidence that the phase went away is not, and it is the more
        // expensive mistake by far: the `gameover` the next payload's warm-up
        // has to be a transition from is gone, so the second match gets an
        // ending and no beginning — which is exactly the defect
        // `a_second_match_on_the_same_map_is_reported_starting` exists to stop,
        // arriving by a different door and leaving no note behind.
        let phase = |stamp: i64, phase: &str| {
            payload(serde_json::json!({
                "provider": {"steamid": LOCAL, "timestamp": stamp},
                "map": {"name": "de_dust2", "mode": "competitive", "phase": phase, "round": 20,
                        "team_ct": {"score": 13}, "team_t": {"score": 7}},
                "player": {"steamid": LOCAL, "team": "CT",
                           "match_stats": {"kills": 8, "deaths": 6, "assists": 3}}
            }))
        };
        let no_phase = payload(serde_json::json!({
            "provider": {"steamid": LOCAL, "timestamp": 35},
            "map": {"name": "de_dust2", "mode": "competitive", "round": 20,
                    "team_ct": {"score": 13}, "team_t": {"score": 7}},
            "player": {"steamid": LOCAL, "team": "CT",
                       "match_stats": {"kills": 8, "deaths": 6, "assists": 3}}
        }));

        let mut tracker = MatchTracker::new();
        tracker.observe(&live(10, 8, 6, 3), at(0));

        let ended = tracker.observe(&phase(30, "gameover"), at(1));
        assert_eq!(ended.kinds(), vec![&EventKind::MatchEnded, &EventKind::Win]);

        let passed_over = tracker.observe(&no_phase, at(2));
        assert!(
            passed_over.events.is_empty(),
            "a payload that says nothing about the phase is not a transition: {:?}",
            passed_over.kinds()
        );

        let started = tracker.observe(&phase(40, "warmup"), at(3));
        assert_eq!(
            started.kinds(),
            vec![&EventKind::MatchStarted],
            "one phase-less payload between the scoreboard and the next warm-up swallowed the \
             beginning of the match: the baseline phase was cleared rather than kept"
        );
    }

    #[test]
    fn a_map_block_without_a_name_does_not_restart_the_match_it_is_silent_about() {
        // The same rule, on the field beside it, where the cost is a spurious
        // event rather than a missing one: clearing `map_name` makes the next
        // payload the first sight of a map that never changed, and
        // `derive_match` reports a match starting in the middle of one.
        let named = |stamp: i64| {
            payload(serde_json::json!({
                "provider": {"steamid": LOCAL, "timestamp": stamp},
                "map": {"name": "de_dust2", "phase": "live", "round": 5},
                "player": {"steamid": LOCAL, "team": "CT",
                           "match_stats": {"kills": 3, "deaths": 2, "assists": 1}}
            }))
        };
        let nameless = payload(serde_json::json!({
            "provider": {"steamid": LOCAL, "timestamp": 21},
            "map": {"phase": "live", "round": 5},
            "player": {"steamid": LOCAL, "team": "CT",
                       "match_stats": {"kills": 3, "deaths": 2, "assists": 1}}
        }));

        let mut tracker = MatchTracker::new();
        tracker.observe(&named(20), at(0));
        assert!(tracker.observe(&nameless, at(1)).events.is_empty());

        let same_map = tracker.observe(&named(22), at(2));
        assert!(
            same_map.events.is_empty(),
            "the map never changed, and a match was reported starting inside one: {:?}",
            same_map.kinds()
        );
    }

    #[test]
    fn a_match_that_has_not_ended_is_not_restarted_by_a_phase_change() {
        // The guard above is about coming out of `gameover` and nothing else.
        // Warm-up giving way to a live round is the same match, and reporting
        // it starting again would put a second beginning on the timeline.
        let phase = |stamp: i64, phase: &str| {
            payload(serde_json::json!({
                "provider": {"steamid": LOCAL, "timestamp": stamp},
                "map": {"name": "de_dust2", "phase": phase, "round": 0},
                "player": {"steamid": LOCAL, "team": "CT",
                           "match_stats": {"kills": 0, "deaths": 0, "assists": 0}}
            }))
        };

        let mut tracker = MatchTracker::new();
        tracker.observe(&phase(10, "warmup"), at(0));
        assert!(tracker.observe(&phase(11, "live"), at(1)).events.is_empty());
        assert!(tracker
            .observe(&phase(12, "intermission"), at(2))
            .events
            .is_empty());
        assert!(tracker.observe(&phase(13, "live"), at(3)).events.is_empty());
    }

    #[test]
    fn a_drawn_match_ends_without_claiming_a_result() {
        // There is no `draw` kind, and picking `win` or `loss` would be a coin
        // toss recorded as a fact.
        let drawn = payload(serde_json::json!({
            "provider": {"steamid": LOCAL, "timestamp": 30},
            "map": {"name": "de_dust2", "phase": "gameover",
                    "team_ct": {"score": 12}, "team_t": {"score": 12}},
            "player": {"steamid": LOCAL, "team": "CT",
                       "match_stats": {"kills": 8, "deaths": 6, "assists": 3}}
        }));

        let mut tracker = MatchTracker::new();
        tracker.observe(&live(10, 8, 6, 3), at(0));
        assert_eq!(
            tracker.observe(&drawn, at(1)).kinds(),
            vec![&EventKind::MatchEnded]
        );
    }

    #[test]
    fn round_phases_produce_a_start_and_an_end_and_nothing_in_between() {
        let with_phase = |stamp: i64, phase: &str, winner: Option<&str>| {
            let mut round = serde_json::json!({"phase": phase});
            if let Some(winner) = winner {
                round["win_team"] = Value::String(winner.to_owned());
            }
            payload(serde_json::json!({
                "provider": {"steamid": LOCAL, "timestamp": stamp},
                "map": {"name": "de_dust2", "phase": "live", "round": 4},
                "round": round,
                "player": {"steamid": LOCAL, "team": "CT",
                           "match_stats": {"kills": 0, "deaths": 0, "assists": 0}}
            }))
        };

        let mut tracker = MatchTracker::new();
        tracker.observe(&with_phase(10, "freezetime", None), at(0));

        assert_eq!(
            tracker
                .observe(&with_phase(11, "live", None), at(1))
                .kinds(),
            vec![&EventKind::RoundStarted]
        );
        let ended = tracker.observe(&with_phase(12, "over", Some("T")), at(2));
        assert_eq!(ended.kinds(), vec![&EventKind::RoundEnded]);
        assert_eq!(ended.events[0].data["win_team"], Value::String("T".into()));
        assert_eq!(ended.events[0].data["round"], Value::from(4));

        // Back to freeze time: not a start, not an end.
        assert!(tracker
            .observe(&with_phase(13, "freezetime", None), at(3))
            .events
            .is_empty());
    }

    #[test]
    fn a_round_phase_this_build_does_not_know_is_declined_rather_than_guessed() {
        let paused = payload(serde_json::json!({
            "provider": {"steamid": LOCAL, "timestamp": 11},
            "map": {"name": "de_dust2", "phase": "live"},
            "round": {"phase": "tactical_pause"},
            "player": {"steamid": LOCAL, "match_stats": {"kills": 0, "deaths": 0, "assists": 0}}
        }));

        let mut tracker = MatchTracker::new();
        tracker.observe(&live(10, 0, 0, 0), at(0));
        assert!(tracker.observe(&paused, at(1)).events.is_empty());
    }
}
