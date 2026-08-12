//! Turning a sequence of Dota 2 states into the events the application knows.
//!
//! Game State Integration describes *now*. An event is a **difference** between
//! two of those descriptions, and this module is where that subtraction
//! happens. What each difference is worth — a standard kind, this plugin's own
//! namespace, or nothing at all — is the table in [`super`].
//!
//! # The three rules that keep the subtraction honest
//!
//! 1. **The first payload of a match reports nothing.** Attaching to a game
//!    that is already running is normal — a user enables the plugin mid-match,
//!    or Clipped starts recording after the horn — and a plugin that reported
//!    the difference between "nothing" and "seven kills" would put seven marks
//!    on a timeline for one moment none of them happened at. So a snapshot with
//!    no comparable predecessor is a baseline, and produces no events.
//! 2. **A different match is a different baseline.** `map.matchid` changing is
//!    a new game, and every counter in it starts again. Diffing across that
//!    boundary would report a match's worth of deaths as negative and the next
//!    match's first kill as its eighth.
//! 3. **A counter that goes backwards reports nothing.** It cannot happen
//!    inside one match, so if it does, the payload is out of order or Dota has
//!    changed what the number means. Both are answered by taking the new value
//!    as the truth and reporting nothing, which is what
//!    [`u64::saturating_sub`] does here.
//!
//! Timing is not this module's business. It produces *what happened*, and
//! `crate::gsi::Window` — which knows the interval the payloads arrived in —
//! decides where on the recording's timeline it goes and how precisely it is
//! known. That split is why every test below is a pure comparison of two JSON
//! documents with no clock in sight.

use clipped_events::{CustomName, EventKind};
use serde_json::{Map, Value};

use super::snapshot::{Counters, GameState, Snapshot};

/// The kill streak Dota itself calls a killing spree, and the point at which
/// this plugin reports one.
///
/// Below it, a streak is two kills, which `kill` events already describe. At it,
/// the game announces it — which is the moment a highlight rule has something
/// to cut around.
const KILL_STREAK_THRESHOLD: u64 = 3;

/// The most events one counter may produce from one payload.
///
/// A kill counter cannot realistically move by more than a couple between two
/// posts a tenth of a second apart, so this is never reached by a game
/// behaving as it does today. It exists because the alternative is unbounded: a
/// Dota update that changed a counter's meaning — to a total across matches,
/// say — would otherwise put thousands of marks on one second of a timeline and
/// fill the host's queue with them (`docs/plugin-api.md`, "What a misbehaving
/// plugin costs a recording"). The total is carried in every payload, so what
/// the counter actually said is not lost.
const MAX_EVENTS_PER_COUNTER: u64 = 8;

/// Something that happened, before it is given a position on a timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    /// What happened.
    pub kind: EventKind,
    /// Dota's own words about it. Nothing above this plugin interprets them.
    pub data: Map<String, Value>,
}

/// Something the user can act on, noticed while reading a payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    /// The payloads describe a game the player is watching rather than playing.
    Spectating,
}

impl Notice {
    /// The line the user is shown.
    ///
    /// Said once per attach rather than per payload: it is a fact about the
    /// session, and repeating it would be a plugin talking over a recording.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Spectating => {
                "Clipped can only report Dota 2 events for the player at this computer, so \
                 nothing is reported while you are watching somebody else's game."
            }
        }
    }
}

/// What one payload produced.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Observed {
    /// The events, in a fixed order, all describing the same interval.
    pub reports: Vec<Report>,
    /// A message for the user, the first time it applies.
    pub notice: Option<Notice>,
}

/// Reads a stream of Dota 2 states and reports what changed between them.
#[derive(Debug, Default)]
pub struct Watcher {
    previous: Option<Snapshot>,
    told_about_spectating: bool,
}

impl Watcher {
    /// A watcher that has seen nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads one payload and reports what it changed.
    pub fn observe(&mut self, payload: &Value) -> Observed {
        let next = Snapshot::read(payload);

        let notice = if next.spectating && !self.told_about_spectating {
            self.told_about_spectating = true;
            Some(Notice::Spectating)
        } else {
            None
        };

        let reports = self
            .previous
            .take()
            .filter(|previous| previous.match_id == next.match_id)
            .map(|previous| difference(&previous, &next))
            .unwrap_or_default();

        self.previous = Some(next);
        Observed { reports, notice }
    }
}

/// Every event between two snapshots of the same match.
fn difference(previous: &Snapshot, next: &Snapshot) -> Vec<Report> {
    let mut reports = Vec::new();
    let context = || {
        let mut data = Map::new();
        if let Some(match_id) = &next.match_id {
            data.insert("match_id".to_owned(), Value::from(match_id.clone()));
        }
        if let Some(clock_time) = next.clock_time {
            data.insert("clock_time".to_owned(), Value::from(clock_time));
        }
        data
    };

    if previous.state != next.state {
        match &next.state {
            Some(GameState::InProgress) => reports.push(Report {
                kind: EventKind::MatchStarted,
                data: context(),
            }),
            Some(GameState::PostGame) => reports.push(Report {
                kind: EventKind::MatchEnded,
                data: context(),
            }),
            // Hero selection, strategy time, the pre-game horn, a reconnect:
            // states this plugin does not claim to have an event for.
            Some(GameState::Other(_)) | None => {}
        }
    }

    if let (Some(counters), Some(before)) = (next.counters, previous.counters) {
        reports.extend(counted(
            EventKind::Kill,
            "kills",
            before.kills,
            counters.kills,
            &next.hero,
            &context,
        ));
        reports.extend(counted(
            EventKind::Death,
            "deaths",
            before.deaths,
            counters.deaths,
            &next.hero,
            &context,
        ));
        reports.extend(counted(
            EventKind::Assist,
            "assists",
            before.assists,
            counters.assists,
            &next.hero,
            &context,
        ));
        reports.extend(streak(&before, &counters, &next.hero, &context));
    }

    // Reported once, when a team first appears in `win_team`. The player's own
    // team decides which of the two kinds it is; without it — a payload that
    // says who won but not who the player is — nothing is reported, because
    // guessing would put a `win` on the timeline of somebody who lost.
    if previous.win_team.is_none() {
        if let (Some(winner), Some(team)) = (next.win_team, next.team) {
            let mut data = context();
            data.insert("team".to_owned(), Value::from(team.as_str()));
            data.insert("winning_team".to_owned(), Value::from(winner.as_str()));
            reports.push(Report {
                kind: if winner == team {
                    EventKind::Win
                } else {
                    EventKind::Loss
                },
                data,
            });
        }
    }

    reports
}

/// One event per step a counter took, up to [`MAX_EVENTS_PER_COUNTER`].
fn counted(
    kind: EventKind,
    total_name: &str,
    before: u64,
    now: u64,
    hero: &Option<String>,
    context: &impl Fn() -> Map<String, Value>,
) -> Vec<Report> {
    let steps = now.saturating_sub(before).min(MAX_EVENTS_PER_COUNTER);
    (0..steps)
        .map(|_| {
            let mut data = context();
            data.insert(total_name.to_owned(), Value::from(now));
            if let Some(hero) = hero {
                data.insert("hero".to_owned(), Value::from(hero.clone()));
            }
            Report {
                kind: kind.clone(),
                data,
            }
        })
        .collect()
}

/// The killing spree, if this payload started or extended one.
fn streak(
    before: &Counters,
    now: &Counters,
    hero: &Option<String>,
    context: &impl Fn() -> Map<String, Value>,
) -> Option<Report> {
    if now.kill_streak <= before.kill_streak || now.kill_streak < KILL_STREAK_THRESHOLD {
        return None;
    }
    let mut data = context();
    data.insert("streak".to_owned(), Value::from(now.kill_streak));
    if let Some(hero) = hero {
        data.insert("hero".to_owned(), Value::from(hero.clone()));
    }
    Some(Report {
        kind: kill_streak_kind(),
        data,
    })
}

/// This plugin's own name for a killing spree.
///
/// Namespaced, as `docs/plugin-api.md` requires of anything the project's
/// vocabulary does not define: a plugin may not claim a word the application
/// has not defined, and the namespace is the plugin's own identifier so that an
/// unexplained mark on a timeline is traceable to what made it.
///
/// # Panics
///
/// Never: the name is a constant that obeys the syntax, which
/// `a_custom_name_this_plugin_reports_is_one_the_host_accepts` asserts rather
/// than assumes.
#[must_use]
pub fn kill_streak_kind() -> EventKind {
    EventKind::Custom(
        CustomName::new(KILL_STREAK_NAME).expect("this plugin's own event name is well formed"),
    )
}

/// See [`kill_streak_kind`].
pub const KILL_STREAK_NAME: &str = "dota-2.kill_streak";

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A payload in the shape Dota posts, with the parts a test cares about.
    fn state(match_id: &str, game_state: &str, counters: (u64, u64, u64, u64)) -> Value {
        let (kills, deaths, assists, kill_streak) = counters;
        json!({
            "map": {"matchid": match_id, "game_state": game_state, "win_team": "none",
                    "clock_time": 600},
            "player": {"team_name": "radiant", "kills": kills, "deaths": deaths,
                       "assists": assists, "kill_streak": kill_streak},
            "hero": {"name": "npc_dota_hero_lina"}
        })
    }

    fn in_progress(counters: (u64, u64, u64, u64)) -> Value {
        state(
            "8421997461",
            "DOTA_GAMERULES_STATE_GAME_IN_PROGRESS",
            counters,
        )
    }

    fn kinds(observed: &Observed) -> Vec<String> {
        observed
            .reports
            .iter()
            .map(|report| report.kind.as_str().to_owned())
            .collect()
    }

    #[test]
    fn the_first_payload_of_a_match_is_a_baseline_and_not_seven_kills() {
        // Rule 1. Attaching to a game already in progress is the ordinary case,
        // not the exception: the recorder starts a session when a process
        // starts, and a user can enable a plugin at any point.
        let mut watcher = Watcher::new();
        let observed = watcher.observe(&in_progress((7, 2, 5, 1)));
        assert!(
            observed.reports.is_empty(),
            "a first payload has nothing to be a difference from: {:?}",
            kinds(&observed)
        );

        // And the baseline is real: the next payload is compared against it.
        let observed = watcher.observe(&in_progress((8, 2, 5, 2)));
        assert_eq!(kinds(&observed), vec!["kill"]);
    }

    #[test]
    fn a_kill_a_death_and_an_assist_are_one_event_each_carrying_dotas_own_words() {
        let mut watcher = Watcher::new();
        watcher.observe(&in_progress((0, 0, 0, 0)));
        let observed = watcher.observe(&in_progress((1, 1, 1, 0)));

        assert_eq!(kinds(&observed), vec!["kill", "death", "assist"]);
        let kill = &observed.reports[0];
        assert_eq!(kill.data["kills"], json!(1));
        assert_eq!(kill.data["hero"], json!("npc_dota_hero_lina"));
        assert_eq!(kill.data["match_id"], json!("8421997461"));
        assert_eq!(
            kill.data["clock_time"],
            json!(600),
            "the match clock is what lets a mark be found again in a replay"
        );
    }

    #[test]
    fn a_counter_that_moves_by_more_than_one_reports_each_step() {
        // Two kills in one posting interval is a double kill, and it is two
        // things that happened. Collapsing them would lose one; the moment they
        // share is what `precision` is for.
        let mut watcher = Watcher::new();
        watcher.observe(&in_progress((0, 0, 0, 0)));
        assert_eq!(
            kinds(&watcher.observe(&in_progress((2, 0, 0, 2)))),
            vec!["kill", "kill"]
        );
    }

    #[test]
    fn a_counter_that_goes_backwards_or_leaps_cannot_flood_the_timeline() {
        // Rule 3, and the bound. Neither can happen while Dota behaves as it
        // does today; both are what a plugin does when it stops.
        let mut watcher = Watcher::new();
        watcher.observe(&in_progress((9, 9, 9, 0)));
        assert!(
            watcher
                .observe(&in_progress((3, 3, 3, 0)))
                .reports
                .is_empty(),
            "a counter cannot go down inside a match, so a payload that says it did is not \
             three negative kills"
        );

        let mut watcher = Watcher::new();
        watcher.observe(&in_progress((0, 0, 0, 0)));
        let flood = watcher.observe(&in_progress((10_000, 0, 0, 0)));
        assert_eq!(
            flood.reports.len(),
            8,
            "one payload must not be able to fill the host's queue"
        );
        // Spelled out rather than compared against `MAX_EVENTS_PER_COUNTER`,
        // which is the mistake this assertion started life as: a test that
        // reads the constant it is checking moves with it, and would have
        // agreed just as happily with a bound of ten thousand.
        assert_eq!(usize::try_from(MAX_EVENTS_PER_COUNTER), Ok(8));
    }

    #[test]
    fn a_new_match_starts_again_rather_than_reporting_the_difference() {
        // Rule 2. The second match's counters are lower than the first's, and
        // its first kill is its first, not its ninth.
        let mut watcher = Watcher::new();
        watcher.observe(&in_progress((8, 3, 11, 0)));

        let second = state(
            "8421997999",
            "DOTA_GAMERULES_STATE_GAME_IN_PROGRESS",
            (0, 0, 0, 0),
        );
        assert!(
            watcher.observe(&second).reports.is_empty(),
            "a different match identifier is a different game"
        );

        let mut third = second.clone();
        third["player"]["kills"] = json!(1);
        assert_eq!(kinds(&watcher.observe(&third)), vec!["kill"]);
    }

    #[test]
    fn a_match_starts_and_ends_on_the_states_that_mean_it() {
        let mut watcher = Watcher::new();
        watcher.observe(&state(
            "1",
            "DOTA_GAMERULES_STATE_HERO_SELECTION",
            (0, 0, 0, 0),
        ));
        assert!(
            watcher
                .observe(&state(
                    "1",
                    "DOTA_GAMERULES_STATE_STRATEGY_TIME",
                    (0, 0, 0, 0)
                ))
                .reports
                .is_empty(),
            "the states before a match are not a match starting"
        );
        assert_eq!(
            kinds(&watcher.observe(&state(
                "1",
                "DOTA_GAMERULES_STATE_GAME_IN_PROGRESS",
                (0, 0, 0, 0)
            ))),
            vec!["match_started"]
        );
        assert_eq!(
            kinds(&watcher.observe(&state("1", "DOTA_GAMERULES_STATE_POST_GAME", (0, 0, 0, 0)))),
            vec!["match_ended"]
        );
    }

    #[test]
    fn who_won_is_read_against_the_players_own_team() {
        let mut ended = in_progress((5, 5, 5, 0));
        ended["map"]["win_team"] = json!("radiant");

        let mut watcher = Watcher::new();
        watcher.observe(&in_progress((5, 5, 5, 0)));
        let observed = watcher.observe(&ended);
        assert_eq!(kinds(&observed), vec!["win"]);
        assert_eq!(observed.reports[0].data["team"], json!("radiant"));

        // The same payload for a player on the other side is a loss, and it is
        // read from the player's own block rather than assumed.
        let mut theirs = ended.clone();
        theirs["player"]["team_name"] = json!("dire");
        let mut watcher = Watcher::new();
        let mut before = in_progress((5, 5, 5, 0));
        before["player"]["team_name"] = json!("dire");
        watcher.observe(&before);
        assert_eq!(kinds(&watcher.observe(&theirs)), vec!["loss"]);

        // Reported once. A run of payloads all saying Radiant won is one win.
        assert!(watcher.observe(&theirs).reports.is_empty());
    }

    #[test]
    fn a_win_is_not_guessed_when_the_payload_does_not_say_who_the_player_is() {
        let mut watcher = Watcher::new();
        let mut before = in_progress((0, 0, 0, 0));
        before["player"]
            .as_object_mut()
            .expect("a player block")
            .remove("team_name");
        let mut after = before.clone();
        after["map"]["win_team"] = json!("dire");

        watcher.observe(&before);
        assert!(
            watcher.observe(&after).reports.is_empty(),
            "a `win` on the timeline of somebody who lost is worse than no mark at all"
        );
    }

    #[test]
    fn a_killing_spree_is_reported_under_this_plugins_own_name() {
        let mut watcher = Watcher::new();
        watcher.observe(&in_progress((1, 0, 0, 1)));
        assert_eq!(
            kinds(&watcher.observe(&in_progress((2, 0, 0, 2)))),
            vec!["kill"],
            "two kills is not yet a spree, and Dota does not announce one"
        );
        assert_eq!(
            kinds(&watcher.observe(&in_progress((3, 0, 0, 3)))),
            vec!["kill", KILL_STREAK_NAME],
            "the third is the killing spree the game itself calls out"
        );

        // A streak that ends and one that stays put report nothing.
        assert!(watcher
            .observe(&in_progress((3, 1, 0, 0)))
            .reports
            .iter()
            .all(|report| report.kind != kill_streak_kind()));
    }

    #[test]
    fn a_custom_name_this_plugin_reports_is_one_the_host_accepts() {
        // The rule from `docs/plugin-api.md`: a namespaced name is a plugin's
        // own, an unnamespaced one would be claiming a word in the project's
        // vocabulary and is refused by the host. Asserting it here means a typo
        // in the constant is a failing test rather than an event silently
        // dropped at run time.
        assert_eq!(kill_streak_kind().as_str(), KILL_STREAK_NAME);
        assert!(
            KILL_STREAK_NAME.starts_with(&format!("{}.", crate::PLUGIN_ID)),
            "a plugin's own names carry its own identifier as their namespace"
        );
        assert!(CustomName::new(KILL_STREAK_NAME).is_ok());
    }

    #[test]
    fn a_spectated_game_reports_nothing_and_says_why_once() {
        let watching = json!({
            "map": {"matchid": "1", "game_state": "DOTA_GAMERULES_STATE_GAME_IN_PROGRESS"},
            "player": {"team2": {"player0": {"kills": 4, "team_name": "radiant"}}}
        });

        let mut watcher = Watcher::new();
        let first = watcher.observe(&watching);
        assert!(first.reports.is_empty());
        assert_eq!(first.notice, Some(Notice::Spectating));

        let second = watcher.observe(&watching);
        assert_eq!(
            second.notice, None,
            "a fact about the session is said once, not once per payload"
        );
    }
}
