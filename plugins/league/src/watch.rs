//! Snapshots in, reports out: the whole of what this plugin decides.
//!
//! [`LeagueWatch::observe`] is a pure function of a poll and a clock reading,
//! deliberately, and for the same reason `PluginSupervisor::poll` is
//! (`docs/plugin-api.md`): everything here is about *time* — how long the API
//! has been unreachable, how long ago an event happened, how often something
//! is worth saying — and time-based behaviour that a test cannot supply a clock
//! to is time-based behaviour a test has to wait for. Nothing in this module
//! reads a clock, opens a socket or sleeps.
//!
//! # Polled, and lossless anyway
//!
//! League's feed is polled rather than pushed, and the obvious worry about a
//! polled feed is that a poll which arrives late misses whatever happened in
//! between. It cannot happen here, and the reason is the shape of the API
//! rather than anything clever: the event list is **cumulative and indexed**.
//! Every poll returns the whole match, so the state needed is the identifier
//! after the last one reported ([`LeagueWatch`]'s cursor), and the match clock
//! that says when the cursor belongs to a different match. A poll that took ten
//! seconds returns ten seconds of events, and none of them is lost.
//!
//! What a slow poll costs is therefore **latency and nothing else** — how long
//! after a kill the recording hears about it — because the *position* of an
//! event comes from the game's own match clock rather than from when this
//! process noticed. That is the whole argument for [`POLL_INTERVAL`] being a
//! second rather than a tenth of one.
//!
//! # Cumulative, which cuts both ways
//!
//! The same property that makes a slow poll lossless makes a *fresh* watch
//! dangerous: the first payload it reads carries the whole match, and a cursor
//! that starts at zero would report every kill of it. That is not a hypothetical
//! — the host restarts a plugin that exited or went silent
//! (`docs/plugin-api.md`, "Supervision and restart"), and the replacement is a
//! new process with a new cursor attached to the same recording. Reporting what
//! it finds would put a second copy of every kill, death, assist and
//! `match_started` on a timeline the attachment before it had already marked.
//!
//! So the cursor is not the only thing that decides what is reported. An event
//! is reported only if it happened **after this watch started observing**, which
//! [`LeagueWatch::observe`] is told on every call. The match clock in the
//! payload says how long ago each event was, so this is a comparison of two
//! numbers and needs no memory of a previous process — and it answers the other
//! form of the same question at the same time: a session that starts recording
//! part way through a match must not have the first ten minutes of that match
//! drawn onto the ten seconds of video it has.
//!
//! In the ordinary case it costs nothing at all, and the reason is worth having
//! written down. This plugin is attached to `League of Legends.exe`, and that
//! process starts *before* the match clock does — a loading screen is a minute
//! of it. So for an attachment that saw its match begin, `ago ≤ gameTime ≤ at`
//! holds for every event, and the rule never fires. It fires exactly when the
//! match was already under way, which is exactly when firing is right.
//!
//! What that trades is stated rather than glossed (AGENTS.md section 54): the
//! events that happen during the second or two a restart takes are **lost**,
//! because nothing was watching for them and nothing can prove afterwards
//! whether they were reported. A missing mark is the better direction to fail
//! in than a duplicated one — a timeline with two of every kill is wrong in a
//! way a viewer cannot repair, and the events are still in the recording.
//!
//! # What is reported, and what is not
//!
//! | League says | This reports |
//! | --- | --- |
//! | `GameStart` | `match_started` |
//! | `GameEnd` | `match_ended`, and `win` or `loss` from its `Result` |
//! | `ChampionKill` | `kill`, `death` or `assist`, depending on which name in it is the player's |
//! | `MinionsSpawning`, `FirstBrick`, `TurretKilled`, `InhibKilled`, `DragonKill`, `HeraldKill`, `BaronKill`, `Multikill`, `Ace`, `FirstBlood`, `InhibRespawningSoon`, `InhibRespawned`, and anything a later patch adds | nothing |
//!
//! The second row of that table is the one worth defending, because "an
//! objective this player took" is obviously worth clipping. It is left out
//! because issue #72's scope is kills, deaths, assists, match start and end,
//! and win and loss, and because each of the others is a decision about the
//! shared vocabulary rather than a line of code: a dragon is not a `goal` in
//! the sense any other game's plugin would mean, and inventing
//! `league-of-legends.dragon_killed` commits the project to a custom name
//! before anybody has asked for one (`docs/plugin-api.md`, "Custom events").
//! The events are already read, indexed and timed here; adding one is a match
//! arm and a test.

use core::time::Duration;

use clipped_events::EventKind;
use clipped_plugins::{PluginReport, ReportedEvent};

use crate::snapshot::{GameSnapshot, LiveEvent, PlayerIdentity};

/// How long this plugin waits between polls.
///
/// A second, on a machine that is also running League of Legends (AGENTS.md
/// section 18). What that costs and what it buys, stated so that whoever wants
/// to change it knows what they are trading:
///
/// - **It does not affect where an event is drawn.** An event's position comes
///   from the match clock in the same payload, not from when this process
///   noticed, so polling twice as slowly does not make a mark twice as wrong.
///   This is the one real difference from a pushed feed such as
///   Counter-Strike 2's, and it is in League's favour.
/// - **It does affect how quickly anything can react**, which for a replay
///   buffer measured in minutes (`docs/replay-buffer.md`) is not close to
///   mattering, and it bounds an event's `latency` — the field that tells a
///   consumer whether reacting was possible at all.
/// - **It costs one HTTPS request and one JSON parse of a few tens of
///   kilobytes**, in another process, on a core the game is not using.
///
/// Which is to say: the interval is chosen for the reporting to feel prompt,
/// and it could be several times longer without losing an event.
pub const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// How far either side of a match-relative time the truth is assumed to lie.
///
/// An event's `at` is `now - (gameTime - EventTime)`, and three things blur it:
/// the request's round trip, which is measured and added to this; the
/// resolution the two clock readings are reported at; and the fact that both
/// are produced somewhere inside the request rather than at a known instant.
///
/// **This number is assumed, not measured.** No amount of reading a payload
/// establishes how precisely the client rounds what it prints, and this plugin
/// was written without the game to measure it against — so it is a deliberate
/// over-estimate. `precision` is a claim about how wrong an event's position
/// may be, and the safe direction to be wrong in is claiming less precision
/// than there is (`docs/plugin-api.md`, "Timing"). Somebody who measures it can
/// lower it; nobody should raise the claim without measuring.
pub const REPORTED_TIME_RESOLUTION: Duration = Duration::from_millis(100);

/// How long the API may be unreachable before the user is told.
///
/// The plugin is only started because League's own executable is running, and
/// that process serves this API. A minute of nothing listening is therefore not
/// a slow loading screen: it is the API not being there, which is worth one
/// line to somebody wondering why their match produced no marks (AGENTS.md
/// section 45).
const UNREACHABLE_NOTICE: Duration = Duration::from_secs(60);

/// How far the match clock may appear to go backwards without it meaning a
/// different match.
///
/// Inside a match it does not go backwards at all, so this is not a tolerance
/// for the game: it is a tolerance for two readings produced at two moments and
/// rounded. A second is far below the gap between one match's clock and the
/// next match's, which starts at zero.
const CLOCK_JITTER: f64 = 1.0;

/// How many unreadable answers in a row are worth telling the user about.
///
/// One is a hiccup. Five in a row, from an endpoint that answered, is the
/// payload having changed shape — which is what a game patched every fortnight
/// does eventually, and which nobody can act on if it is only ever counted.
const UNREADABLE_RUN_NOTICE: u32 = 5;

/// What one poll produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollResult<'a> {
    /// The API answered, with a body and a measured round trip.
    Answered {
        /// What it said.
        body: &'a str,
        /// How long the request took, start to finish. The match clock inside
        /// the body was read somewhere in that window, so this is the width of
        /// the uncertainty about when.
        round_trip: Duration,
    },
    /// The API is there and says there is no match: the game is loading, or
    /// finishing, or the player is somewhere other than in a game.
    NoMatch,
    /// Nothing answered. Not an error on its own — it is what the endpoint does
    /// before the match has loaded and after it has ended.
    Unreachable,
}

/// The state one attached session needs: who is playing, and what has already
/// been reported.
///
/// Everything else about the match is in the payload, which is why this is
/// three fields and a cursor rather than a model of a game of League.
#[derive(Debug, Default)]
pub struct LeagueWatch {
    /// Who the person at the keyboard is, from the last payload that said.
    identity: Option<PlayerIdentity>,
    /// The identifier the next unreported event will have.
    next_event_id: u64,
    /// The match clock in the last payload read, in seconds.
    last_game_time: f64,
    /// When the current run of unreachable polls began.
    unreachable_since: Option<Duration>,
    /// Whether the current run has already been reported.
    said_it_is_unreachable: bool,
    /// How many answers in a row could not be read.
    unreadable_run: u32,
    /// Whether the current run has already been reported.
    said_it_is_unreadable: bool,
    /// Whether the user has been told that kills cannot be attributed.
    said_it_does_not_know_the_player: bool,
    /// Whether the user has been told that entries could not be read.
    said_entries_could_not_be_read: bool,
    /// Whether the log has been told that this watch began part way through a
    /// match, and what it therefore left alone.
    said_it_began_mid_match: bool,
}

impl LeagueWatch {
    /// A watch that has seen nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads one poll and says what should be reported.
    ///
    /// `at` is how long this watch has been running, measured by the caller
    /// against a monotonic clock. Two things read it: how long a run of
    /// failures has gone on, and — the reason it has to be honest rather than
    /// merely increasing — how far back this attachment reaches, which is what
    /// decides whether an event in the payload is this watch's to report or the
    /// attachment before it (see this module's documentation).
    ///
    /// The reports come out in the order they should be written: the events of
    /// the match in the order the match produced them, then anything wrong.
    pub fn observe(&mut self, poll: PollResult<'_>, at: Duration) -> Vec<PluginReport> {
        match poll {
            PollResult::Unreachable => self.nothing_answered(at),
            PollResult::NoMatch => {
                // The API is there, so it is not unreachable, whatever it says
                // about a match. Anything else would report a plugin attached
                // during a loading screen as broken.
                self.answered();
                Vec::new()
            }
            PollResult::Answered { body, round_trip } => {
                self.answered();
                self.read(body, round_trip, at)
            }
        }
    }

    /// What is reported when nothing answered.
    fn nothing_answered(&mut self, at: Duration) -> Vec<PluginReport> {
        let since = *self.unreachable_since.get_or_insert(at);
        let unreachable_for = at.saturating_sub(since);
        if unreachable_for < UNREACHABLE_NOTICE || self.said_it_is_unreachable {
            return Vec::new();
        }
        self.said_it_is_unreachable = true;
        vec![problem(
            "League's Live Client Data API has not answered on 127.0.0.1:2999 for a minute. \
             Kills and deaths will not be marked while that lasts.",
        )]
    }

    /// Ends a run of unreachable polls, so that a later outage is reported
    /// again rather than swallowed by the first one having been mentioned.
    fn answered(&mut self) {
        self.unreachable_since = None;
        self.said_it_is_unreachable = false;
    }

    /// What is reported for a body that answered.
    fn read(&mut self, body: &str, round_trip: Duration, at: Duration) -> Vec<PluginReport> {
        let snapshot = match GameSnapshot::parse(body) {
            Ok(snapshot) => snapshot,
            Err(error) => return self.could_not_read(&error),
        };
        self.unreadable_run = 0;
        self.said_it_is_unreadable = false;

        if let Some(identity) = snapshot.active_player() {
            self.identity = Some(identity.clone());
        }
        self.rewind_if_this_is_another_match(&snapshot);

        let mut reports = Vec::new();
        let mut wanted_the_player = false;
        let mut older_than_this_attachment = 0_usize;
        for event in snapshot.events() {
            if event.id() < self.next_event_id {
                continue;
            }
            // Past the cursor whether or not it is reported: an event left to
            // an earlier attachment must not come back when the next poll
            // carries it again.
            self.next_event_id = event.id().saturating_add(1);

            let ago = ago(snapshot.game_time(), event.time());
            if ago > at {
                older_than_this_attachment += 1;
                continue;
            }

            let kinds = self.kinds_of(event, &mut wanted_the_player);
            for kind in kinds {
                reports.push(PluginReport::Event(ReportedEvent {
                    kind,
                    ago_ns: nanos(ago),
                    precision_ns: nanos(round_trip.saturating_add(REPORTED_TIME_RESOLUTION)),
                    // The Live Client Data API is the game reporting its own
                    // events. There is nothing to be unsure of: what is
                    // uncertain is *when*, and that is `precision`
                    // (`docs/plugin-api.md`, "Confidence, and what it is not").
                    confidence: 1.0,
                    data: event.payload(),
                }));
            }
        }

        self.say_what_was_left_to_an_earlier_attachment(older_than_this_attachment);
        reports.extend(self.say_what_could_not_be_read(&snapshot));

        if wanted_the_player && !self.said_it_does_not_know_the_player {
            self.said_it_does_not_know_the_player = true;
            reports.push(problem(
                "League's Live Client Data API did not say who is playing, so kills, deaths and \
                 assists cannot be told apart. The match itself is still marked.",
            ));
        }
        reports
    }

    /// Logs what this watch found had already happened when it began.
    ///
    /// Once per watch, and on standard error rather than as a `problem`: it is
    /// the normal shape of a restart and of a recording started mid-match, and
    /// it needs no action from the user. What it must not be is *silent* — a
    /// timeline missing the first half of a match, with nothing anywhere saying
    /// why, is the failure AGENTS.md section 15 is about.
    fn say_what_was_left_to_an_earlier_attachment(&mut self, how_many: usize) {
        if how_many == 0 || self.said_it_began_mid_match {
            return;
        }
        self.said_it_began_mid_match = true;
        eprintln!(
            "league plugin: {how_many} events had already happened when this attachment began, \
             and are left to whatever was watching then rather than marked twice"
        );
    }

    /// Says out loud that entries of the event list could not be read.
    ///
    /// [`GameSnapshot`] skips an entry it cannot read so that one costs one
    /// rather than costing the payload it arrived in — and the skip is only
    /// defensible while somebody hears about it. Once per watch, because the
    /// cause is a patch having changed the shape of one kind of entry, which is
    /// one fact however many entries carry it.
    fn say_what_could_not_be_read(&mut self, snapshot: &GameSnapshot) -> Option<PluginReport> {
        if snapshot.unreadable_entries() == 0 || self.said_entries_could_not_be_read {
            return None;
        }
        self.said_entries_could_not_be_read = true;
        eprintln!(
            "league plugin: {} entries of the event list could not be read and were skipped",
            snapshot.unreadable_entries()
        );
        Some(problem(
            "Some of what League's Live Client Data API reported could not be read, so parts of \
             this match may not be marked. A League patch may have changed it.",
        ))
    }

    /// What is reported for a body that could not be read.
    fn could_not_read(&mut self, error: &crate::snapshot::SnapshotError) -> Vec<PluginReport> {
        self.unreadable_run = self.unreadable_run.saturating_add(1);
        if self.unreadable_run < UNREADABLE_RUN_NOTICE || self.said_it_is_unreadable {
            return Vec::new();
        }
        self.said_it_is_unreadable = true;
        // The error itself is not put in front of the user: it is a `serde`
        // message about a JSON path, which is diagnostics rather than something
        // to act on (AGENTS.md section 15). It goes to standard error, which is
        // the host's, and the user gets the sentence that has an action in it.
        eprintln!("league plugin: the Live Client Data API could not be read: {error}");
        vec![problem(
            "League's Live Client Data API is answering with something this version of Clipped \
             cannot read. A League patch may have changed it.",
        )]
    }

    /// Starts the cursor again when the payload is describing another match.
    ///
    /// The event list is cumulative *within a match*. A new match starts it
    /// again from zero, and this process outlives that: League's executable is
    /// one process for one match, but a plugin whose game is a custom game
    /// followed by another sees two matches through one attachment. Without
    /// this, the second match's events would all sit below the cursor and
    /// none of them would ever be reported.
    ///
    /// Two signals rather than one, because the obvious one is not enough. The
    /// event list going backwards is what a new match usually looks like — but
    /// only while the new match has fewer events than the old one had, which is
    /// a race the plugin does not control: a first poll that lands a few
    /// minutes into the second match would see identifiers past the cursor and
    /// quietly skip everything below them. **The match clock cannot go
    /// backwards inside a match**, so a clock that has is a different match
    /// whatever the identifiers say.
    fn rewind_if_this_is_another_match(&mut self, snapshot: &GameSnapshot) {
        let clock_went_backwards = snapshot.game_time() + CLOCK_JITTER < self.last_game_time;
        self.last_game_time = snapshot.game_time();
        if self.next_event_id == 0 {
            return;
        }

        let highest = snapshot.events().iter().map(LiveEvent::id).max();
        let list_went_backwards = match highest {
            Some(highest) => highest.saturating_add(1) < self.next_event_id,
            // A match that has produced no events at all yet, when this watch
            // has already reported some, is a match that started again.
            None => true,
        };
        if list_went_backwards || clock_went_backwards {
            self.next_event_id = 0;
            self.said_it_does_not_know_the_player = false;
        }
    }

    /// What one entry of the event list becomes, if anything.
    ///
    /// `wanted_the_player` is set when an entry could only be interpreted by
    /// knowing who is playing and this build does not.
    fn kinds_of(&self, event: &LiveEvent, wanted_the_player: &mut bool) -> Vec<EventKind> {
        match event.name() {
            "GameStart" => vec![EventKind::MatchStarted],
            "GameEnd" => {
                let mut kinds = vec![EventKind::MatchEnded];
                // `Result` is from the active player's point of view, which is
                // why it is read even when nobody could be identified: it is
                // the game answering "did you win", not "who won".
                match event.result() {
                    Some("Win") => kinds.push(EventKind::Win),
                    Some("Lose") => kinds.push(EventKind::Loss),
                    // A result this build does not recognise leaves the match
                    // ending without a verdict, which is honest: `win` and
                    // `loss` are the two the vocabulary has, and guessing
                    // between them from a word nobody has seen would be
                    // inventing the outcome of somebody's match.
                    Some(_) | None => {}
                }
                kinds
            }
            "ChampionKill" => {
                let Some(identity) = &self.identity else {
                    *wanted_the_player = true;
                    return Vec::new();
                };
                let is_me = |name: Option<&str>| name.is_some_and(|name| identity.matches(name));
                if is_me(event.killer()) {
                    vec![EventKind::Kill]
                } else if is_me(event.victim()) {
                    vec![EventKind::Death]
                } else if event
                    .assisters()
                    .iter()
                    .any(|assister| identity.matches(assister))
                {
                    vec![EventKind::Assist]
                } else {
                    // Somebody else's kill, on the other side of the map.
                    Vec::new()
                }
            }
            // Everything else, including every name a later patch invents. See
            // the table in this module's documentation for what that leaves out
            // and why.
            _ => Vec::new(),
        }
    }
}

/// How long before the match clock's current reading an event happened.
///
/// Clamped at zero: an event in the future of the clock in the same payload is
/// not something to report as having happened before the recording started.
fn ago(game_time: f64, event_time: f64) -> Duration {
    let seconds = game_time - event_time;
    if !seconds.is_finite() || seconds <= 0.0 {
        return Duration::ZERO;
    }
    // Fails only for a value that cannot be a duration at all, which the guard
    // above has already excluded every reachable case of.
    Duration::try_from_secs_f64(seconds).unwrap_or(Duration::ZERO)
}

/// A duration as the nanoseconds the wire carries, saturating rather than
/// wrapping: 584 years is not a number a match produces, and wrapping would put
/// an event at the wrong end of a timeline.
fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

/// One line the user can act on.
fn problem(message: &str) -> PluginReport {
    PluginReport::Problem {
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use clipped_plugins::MAX_PROBLEM_BYTES;

    use super::*;

    /// A snapshot with one event list and one match clock, written the way the
    /// API writes them.
    fn snapshot(game_time: f64, events: &str) -> String {
        format!(
            r#"{{"activePlayer":{{"riotId":"Rosalind#EU1","riotIdGameName":"Rosalind"}},
                 "events":{{"Events":[{events}]}},
                 "gameData":{{"gameMode":"CLASSIC","gameTime":{game_time}}}}}"#
        )
    }

    fn answered(body: &str) -> PollResult<'_> {
        PollResult::Answered {
            body,
            round_trip: Duration::from_millis(4),
        }
    }

    /// How long a watch that saw this match begin has been running by the time
    /// the match clock reads `game_time`.
    ///
    /// Not zero, and the difference matters. This plugin is attached to `League
    /// of Legends.exe`, which starts before the match clock does — a loading
    /// screen is a minute of it — so an attachment that saw its match begin is
    /// always older than the match, and passing a reading that says otherwise
    /// would be testing the derivation against a state no machine is ever in.
    /// It also keeps these tests about the cursor: the rule that a watch does
    /// not report what happened before it started is
    /// `a_restarted_plugin_reports_nothing_the_attachment_before_it_reported`'s
    /// to prove, and nothing else's to trip over.
    fn watching_since_before(game_time: f64) -> Duration {
        Duration::from_secs_f64(game_time + 90.0)
    }

    fn kinds(reports: &[PluginReport]) -> Vec<String> {
        reports
            .iter()
            .filter_map(|report| match report {
                PluginReport::Event(event) => Some(event.kind.as_str().to_owned()),
                _ => None,
            })
            .collect()
    }

    fn problems(reports: &[PluginReport]) -> Vec<String> {
        reports
            .iter()
            .filter_map(|report| match report {
                PluginReport::Problem { message } => Some(message.clone()),
                _ => None,
            })
            .collect()
    }

    const KILL: &str = r#"{"EventID":1,"EventName":"ChampionKill","EventTime":213.4,
                           "KillerName":"Rosalind#EU1","VictimName":"Kestrel#EUW","Assisters":[]}"#;
    const DEATH: &str = r#"{"EventID":2,"EventName":"ChampionKill","EventTime":470.9,
                            "KillerName":"Marlowe#EUW","VictimName":"Rosalind#EU1",
                            "Assisters":[]}"#;
    const START: &str = r#"{"EventID":0,"EventName":"GameStart","EventTime":0.03}"#;

    #[test]
    fn an_event_is_reported_once_however_many_polls_carry_it() {
        // The whole reason a polled feed can be lossless: every poll returns
        // the whole match, and the cursor is what stops the same kill being
        // three marks on a timeline.
        let mut watch = LeagueWatch::new();
        let body = snapshot(300.0, &format!("{START},{KILL}"));
        let at = watching_since_before(300.0);
        assert_eq!(
            kinds(&watch.observe(answered(&body), at)),
            vec!["match_started", "kill"]
        );
        assert!(kinds(&watch.observe(answered(&body), at + POLL_INTERVAL)).is_empty());
        assert!(kinds(&watch.observe(answered(&body), at + POLL_INTERVAL * 2)).is_empty());
    }

    #[test]
    fn a_restarted_plugin_reports_nothing_the_attachment_before_it_reported() {
        // The host restarts a plugin that exited or went silent
        // (`docs/plugin-api.md`, "Supervision and restart"). The replacement is
        // a new process with a new cursor, attached to the same recording — and
        // League's event list is cumulative, so its first poll carries the whole
        // match. Reporting it would put a second copy of every kill on a
        // timeline that already has one.
        let body = snapshot(900.0, &format!("{START},{KILL},{DEATH}"));

        let mut before = LeagueWatch::new();
        assert_eq!(
            kinds(&before.observe(answered(&body), watching_since_before(900.0))),
            vec!["match_started", "kill", "death"],
            "the attachment that saw the match begin marks it"
        );

        // Its replacement, a moment old, reading the same match.
        let mut after = LeagueWatch::new();
        assert!(
            after
                .observe(answered(&body), Duration::from_millis(200))
                .is_empty(),
            "a restarted plugin marks nothing that happened before it was there"
        );

        // And it is not mute afterwards: what happens next is still its to
        // report, which is the difference between this and a plugin that gave
        // up on the match.
        let next = r#"{"EventID":3,"EventName":"ChampionKill","EventTime":903.0,
                       "KillerName":"Rosalind#EU1","VictimName":"Marlowe#EUW","Assisters":[]}"#;
        let later = snapshot(904.0, &format!("{START},{KILL},{DEATH},{next}"));
        assert_eq!(
            kinds(&after.observe(answered(&later), Duration::from_secs(5))),
            vec!["kill"],
            "the kill that happened while it was watching"
        );
    }

    #[test]
    fn a_poll_that_missed_a_window_reports_everything_that_happened_in_it() {
        // The failure this is guarding: a plugin that only looked at the last
        // entry, or that assumed one event per poll, would lose the kill and
        // the death that happened while a slow request was outstanding.
        let mut watch = LeagueWatch::new();
        let at = watching_since_before(10.0);
        assert_eq!(
            kinds(&watch.observe(answered(&snapshot(10.0, START)), at)),
            vec!["match_started"]
        );

        let missed = snapshot(500.0, &format!("{START},{KILL},{DEATH}"));
        assert_eq!(
            kinds(&watch.observe(answered(&missed), at + Duration::from_secs(490))),
            vec!["kill", "death"],
            "the events between two polls arrive in match order, once each"
        );
    }

    #[test]
    fn a_second_match_through_one_attachment_starts_the_cursor_again() {
        let mut watch = LeagueWatch::new();
        let at = watching_since_before(500.0);
        let first = snapshot(500.0, &format!("{START},{KILL},{DEATH}"));
        assert_eq!(watch.observe(answered(&first), at).len(), 3);

        // A new match: the identifiers begin at zero again.
        let second = snapshot(6.0, START);
        assert_eq!(
            kinds(&watch.observe(answered(&second), at + Duration::from_secs(400))),
            vec!["match_started"],
            "the second match's events are below the first match's cursor"
        );
    }

    #[test]
    fn a_second_match_already_under_way_is_noticed_by_its_clock() {
        // The identifiers are not enough on their own, and this is the case
        // that shows it: a first poll of the second match that lands after it
        // has produced more events than the first match did. Every identifier
        // is above the cursor, so nothing looks backwards — except the match
        // clock, which cannot go backwards inside a match.
        let mut watch = LeagueWatch::new();
        let at = watching_since_before(500.0);
        let first = snapshot(500.0, &format!("{START},{KILL}"));
        assert_eq!(watch.observe(answered(&first), at).len(), 2);

        let later = r#"{"EventID":7,"EventName":"ChampionKill","EventTime":211.0,
                        "KillerName":"Rosalind#EU1","VictimName":"Kestrel#EUW","Assisters":[]}"#;
        let second = snapshot(240.0, &format!("{START},{KILL},{later}"));
        assert_eq!(
            kinds(&watch.observe(answered(&second), at + Duration::from_secs(600))),
            vec!["match_started", "kill", "kill"],
            "a match whose clock has gone back is another match, whatever its identifiers say"
        );
    }

    #[test]
    fn where_an_event_sits_comes_from_the_match_clock_and_not_from_the_poll() {
        let mut watch = LeagueWatch::new();
        let reports = watch.observe(
            PollResult::Answered {
                body: &snapshot(300.0, KILL),
                round_trip: Duration::from_millis(40),
            },
            Duration::from_secs(1_000),
        );
        let [PluginReport::Event(kill)] = reports.as_slice() else {
            panic!("expected one event, got {reports:?}");
        };

        assert_eq!(
            Duration::from_nanos(kill.ago_ns),
            Duration::from_secs_f64(300.0 - 213.4),
            "the kill happened 86.6 seconds before the clock in the same payload"
        );
        assert_eq!(
            Duration::from_nanos(kill.precision_ns),
            Duration::from_millis(40) + REPORTED_TIME_RESOLUTION,
            "the round trip is the width of the window the clock was read in"
        );
        assert!((kill.confidence - 1.0).abs() < f32::EPSILON);
        assert_eq!(kill.data["VictimName"], "Kestrel#EUW");
    }

    #[test]
    fn a_kill_a_death_and_an_assist_are_told_apart_by_who_is_playing() {
        let assist = r#"{"EventID":3,"EventName":"ChampionKill","EventTime":631.2,
                         "KillerName":"Bramble#EU1","VictimName":"Marlowe#EUW",
                         "Assisters":["Rosalind#EU1"]}"#;
        let elsewhere = r#"{"EventID":4,"EventName":"ChampionKill","EventTime":700.0,
                            "KillerName":"Kestrel#EUW","VictimName":"Bramble#EU1",
                            "Assisters":["Marlowe#EUW"]}"#;
        let mut watch = LeagueWatch::new();
        let body = snapshot(800.0, &format!("{KILL},{DEATH},{assist},{elsewhere}"));

        assert_eq!(
            kinds(&watch.observe(answered(&body), watching_since_before(800.0))),
            vec!["kill", "death", "assist"],
            "somebody else's kill on the other side of the map is not a mark on this recording"
        );
    }

    #[test]
    fn a_match_that_ends_says_how_it_ended() {
        for (result, expected) in [
            (r#""Win""#, vec!["match_ended", "win"]),
            (r#""Lose""#, vec!["match_ended", "loss"]),
            // A verdict this build does not know is left out rather than
            // guessed at: the alternative is inventing the outcome of
            // somebody's match.
            (r#""Surrendered""#, vec!["match_ended"]),
            ("null", vec!["match_ended"]),
        ] {
            let mut watch = LeagueWatch::new();
            let ended = format!(
                r#"{{"EventID":0,"EventName":"GameEnd","EventTime":1799.8,"Result":{result}}}"#
            );
            assert_eq!(
                kinds(&watch.observe(
                    answered(&snapshot(1800.0, &ended)),
                    watching_since_before(1800.0)
                )),
                expected,
                "for a result of {result}"
            );
        }
    }

    #[test]
    fn a_client_that_does_not_say_who_is_playing_says_so_once() {
        let body = format!(
            r#"{{"events":{{"Events":[{START},{KILL}]}},"gameData":{{"gameTime":300.0}}}}"#
        );
        let mut watch = LeagueWatch::new();
        let at = watching_since_before(300.0);
        let first = watch.observe(answered(&body), at);

        assert_eq!(
            kinds(&first),
            vec!["match_started"],
            "the events that do not need a name are still reported"
        );
        assert_eq!(problems(&first).len(), 1, "{first:?}");
        assert!(problems(&first)[0].contains("who is playing"));

        // The kill is past the cursor now, so a second identical poll produces
        // nothing at all — and a match full of kills nobody can attribute must
        // not produce a line per kill either.
        let more = r#"{"EventID":2,"EventName":"ChampionKill","EventTime":400.0,
                       "KillerName":"Marlowe#EUW","VictimName":"Kestrel#EUW","Assisters":[]}"#;
        let again = format!(
            r#"{{"events":{{"Events":[{START},{KILL},{more}]}},"gameData":{{"gameTime":500.0}}}}"#
        );
        assert!(
            problems(&watch.observe(answered(&again), at + Duration::from_secs(200))).is_empty(),
            "the plugin says it once, not once a kill"
        );
    }

    #[test]
    fn an_api_that_never_answers_is_reported_once_a_minute_after_it_stops() {
        let mut watch = LeagueWatch::new();
        assert!(watch
            .observe(PollResult::Unreachable, Duration::ZERO)
            .is_empty());
        assert!(
            watch
                .observe(PollResult::Unreachable, Duration::from_secs(59))
                .is_empty(),
            "a game that has not finished loading is not a fault"
        );

        let told = watch.observe(PollResult::Unreachable, Duration::from_secs(60));
        assert_eq!(problems(&told).len(), 1, "{told:?}");
        assert!(problems(&told)[0].contains("127.0.0.1:2999"));
        assert!(
            watch
                .observe(PollResult::Unreachable, Duration::from_secs(120))
                .is_empty(),
            "and not once a second for the rest of the match"
        );
    }

    #[test]
    fn an_api_that_comes_back_and_goes_again_is_reported_again() {
        let mut watch = LeagueWatch::new();
        assert!(watch
            .observe(PollResult::Unreachable, Duration::ZERO)
            .is_empty());
        assert_eq!(
            problems(&watch.observe(PollResult::Unreachable, Duration::from_secs(60))).len(),
            1
        );

        // Answering — even to say there is no match — ends the outage.
        assert!(watch
            .observe(PollResult::NoMatch, Duration::from_secs(61))
            .is_empty());
        assert!(watch
            .observe(PollResult::Unreachable, Duration::from_secs(62))
            .is_empty());
        assert_eq!(
            problems(&watch.observe(PollResult::Unreachable, Duration::from_secs(130))).len(),
            1,
            "a second outage is a second thing worth telling somebody about"
        );
    }

    #[test]
    fn a_payload_that_stopped_making_sense_is_reported_after_a_run_of_them() {
        let mut watch = LeagueWatch::new();
        for poll in 0..u64::from(UNREADABLE_RUN_NOTICE) - 1 {
            assert!(
                watch
                    .observe(answered("<html>not json</html>"), Duration::from_secs(poll))
                    .is_empty(),
                "one unreadable answer is a hiccup"
            );
        }
        let told = watch.observe(answered("<html>not json</html>"), Duration::from_secs(5));
        assert_eq!(problems(&told).len(), 1, "{told:?}");
        assert!(problems(&told)[0].contains("patch"));

        // And a run that ends resets, so the next one is reported too.
        let readable = watching_since_before(10.0);
        assert!(
            watch
                .observe(answered(&snapshot(10.0, START)), readable)
                .len()
                == 1
        );
        for poll in 1..u64::from(UNREADABLE_RUN_NOTICE) {
            assert!(watch
                .observe(
                    answered("still not json"),
                    readable + Duration::from_secs(poll)
                )
                .is_empty());
        }
        assert_eq!(
            problems(&watch.observe(
                answered("still not json"),
                readable + Duration::from_secs(20)
            ))
            .len(),
            1
        );
    }

    #[test]
    fn every_problem_this_plugin_can_report_fits_in_the_line_it_is_shown_on() {
        // The host bounds a problem message because it is rendered
        // (`clipped_plugins::MAX_PROBLEM_BYTES`). A message written past that
        // bound is a message the user never reads the end of.
        let mut watch = LeagueWatch::new();
        watch.observe(PollResult::Unreachable, Duration::ZERO);
        let mut messages = problems(&watch.observe(PollResult::Unreachable, UNREACHABLE_NOTICE));
        for poll in 0..u64::from(UNREADABLE_RUN_NOTICE) {
            messages.extend(problems(&watch.observe(
                answered("not json"),
                UNREACHABLE_NOTICE + Duration::from_secs(poll + 1),
            )));
        }

        let named_nobody =
            format!(r#"{{"events":{{"Events":[{KILL}]}},"gameData":{{"gameTime":300}}}}"#);
        messages.extend(problems(
            &watch.observe(answered(&named_nobody), watching_since_before(300.0)),
        ));

        // An entry with no identifier: skipped by `GameSnapshot`, and said out
        // loud here rather than counted and forgotten.
        let unreadable_entry = r#"{"events":{"Events":[
             {"EventName":"AnEntryWithNoIdentifier","EventTime":301.0}]},
             "gameData":{"gameTime":302}}"#;
        messages.extend(problems(
            &watch.observe(answered(unreadable_entry), watching_since_before(302.0)),
        ));

        assert_eq!(messages.len(), 4, "one of each: {messages:?}");
        for message in messages {
            assert!(
                message.len() <= MAX_PROBLEM_BYTES,
                "{} bytes is over the {MAX_PROBLEM_BYTES} a problem may carry: {message}",
                message.len()
            );
        }
    }
}
