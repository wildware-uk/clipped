//! What Dota 2 says about itself, reduced to the parts an event can be derived
//! from.
//!
//! A Game State Integration payload is a description of *now*: who the player
//! is, how many kills they have, which game rules state the match is in. It is
//! not a list of things that happened, which is why [`super::Watcher`] exists —
//! but everything that comparison needs is here, and nothing else is.
//!
//! # Reading it is deliberately lenient
//!
//! Every field is optional and nothing here can fail. A payload arrives from a
//! game that is in a menu, in a draft, in a match, watching a replay or being
//! updated by Valve next Tuesday, and the shapes differ:
//!
//! - In a menu there is no `map` and no `hero`.
//! - While **spectating**, `player` and `hero` are not the player's own blocks
//!   at all: they are keyed by team and slot, because there are ten of them.
//!   That is the one shape worth *detecting* rather than merely ignoring, and
//!   [`Snapshot::spectating`] is how, because a plugin that silently reported
//!   nothing during a whole spectated game would look exactly like a broken
//!   one.
//! - A future Dota adds a component, renames a state, or reports a number as a
//!   string.
//!
//! A missing field means "this payload does not say", which [`super::Watcher`]
//! treats as "nothing changed" rather than as a change to nothing. The
//! alternative — a strict parser — turns a Dota update into a plugin that
//! reports no events at all, which is the failure mode `crates/events`'
//! compatibility policy exists to avoid, applied one layer out.

use serde_json::Value;

/// Which side of the map a player is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Team {
    /// The Radiant.
    Radiant,
    /// The Dire.
    Dire,
}

impl Team {
    /// The team Dota named, if it named one.
    ///
    /// `"none"`, which is what `win_team` says while a match is still being
    /// played, is not a team.
    #[must_use]
    pub fn read(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "radiant" => Some(Self::Radiant),
            "dire" => Some(Self::Dire),
            _ => None,
        }
    }

    /// The name Dota uses, which is also what an event payload carries.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Radiant => "radiant",
            Self::Dire => "dire",
        }
    }
}

/// Where a match has got to.
///
/// Two of Dota's game rules states are named and the rest are one variant, and
/// that is a decision rather than an omission. This plugin needs to know when a
/// match **starts being played** and when it **is over**; hero selection,
/// strategy time, the pre-game horn and the several loading states differ from
/// each other in ways nothing here acts on. Keeping them as
/// [`Other`](Self::Other) also means a state Valve adds or renames is carried
/// through as itself rather than being read as one of these two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameState {
    /// `DOTA_GAMERULES_STATE_GAME_IN_PROGRESS`: the match is being played.
    InProgress,
    /// `DOTA_GAMERULES_STATE_POST_GAME`: it is over, and the scoreboard is up.
    PostGame,
    /// Anything else, kept as Dota spelled it.
    Other(String),
}

impl GameState {
    /// The state Dota named.
    #[must_use]
    pub fn read(name: &str) -> Self {
        match name {
            "DOTA_GAMERULES_STATE_GAME_IN_PROGRESS" => Self::InProgress,
            "DOTA_GAMERULES_STATE_POST_GAME" => Self::PostGame,
            other => Self::Other(other.to_owned()),
        }
    }
}

/// The player's own running totals.
///
/// All four are counters that only ever go up within a match, which is what
/// makes a difference between two payloads an event. A counter that went *down*
/// is a payload from a different match or one that arrived out of order, and
/// [`super::Watcher`] treats it as a new baseline rather than as negative
/// kills.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counters {
    /// Heroes the player has killed.
    pub kills: u64,
    /// Times the player has died.
    pub deaths: u64,
    /// Kills the player helped with.
    pub assists: u64,
    /// How many kills the player has made since last dying — Dota's own
    /// killing spree counter.
    pub kill_streak: u64,
}

/// Everything one payload says that this plugin acts on.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Snapshot {
    /// Dota's identifier for the match, as a string, which is how it is sent.
    ///
    /// The one field that says *which* match everything else describes. A
    /// change in it is a different game, and every counter starts again.
    pub match_id: Option<String>,
    /// Where the match has got to.
    pub state: Option<GameState>,
    /// Which team has won, once one has.
    pub win_team: Option<Team>,
    /// The match clock in seconds, negative before the horn.
    ///
    /// Carried into event payloads so that a mark on a video timeline can be
    /// matched against a replay, a scoreboard or a friend's description of the
    /// same fight. Nothing above the plugin interprets it.
    pub clock_time: Option<i64>,
    /// Which side the player is on.
    pub team: Option<Team>,
    /// The player's running totals, when the payload is about one player.
    pub counters: Option<Counters>,
    /// The hero the player is playing, as Dota names it.
    pub hero: Option<String>,
    /// Whether this payload describes a game the player is watching rather than
    /// playing.
    ///
    /// Detected by shape rather than by a flag: while spectating, `player`
    /// holds a block per team and slot instead of the player's own counters.
    pub spectating: bool,
}

impl Snapshot {
    /// Reads a payload. Never fails; a payload that says nothing produces a
    /// snapshot that says nothing.
    #[must_use]
    pub fn read(payload: &Value) -> Self {
        let map = payload.get("map");
        let player = payload.get("player");
        let hero = payload.get("hero");

        let counters = player.and_then(read_counters);
        Self {
            match_id: map.and_then(|map| map.get("matchid")).and_then(read_text),
            state: map
                .and_then(|map| map.get("game_state"))
                .and_then(Value::as_str)
                .map(GameState::read),
            win_team: map
                .and_then(|map| map.get("win_team"))
                .and_then(Value::as_str)
                .and_then(Team::read),
            clock_time: map
                .and_then(|map| map.get("clock_time"))
                .and_then(Value::as_i64),
            team: player
                .and_then(|player| player.get("team_name"))
                .and_then(Value::as_str)
                .and_then(Team::read),
            counters,
            hero: hero
                .and_then(|hero| hero.get("name"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            spectating: counters.is_none() && player.is_some_and(is_keyed_by_slot),
        }
    }
}

/// The player's counters, when the block is one player's.
fn read_counters(player: &Value) -> Option<Counters> {
    // `kills` is the field that decides whether this is a player block at all:
    // present and numeric means the payload is about the person at this
    // machine, and everything else is read on that basis.
    let kills = player.get("kills").and_then(Value::as_u64)?;
    Some(Counters {
        kills,
        deaths: player.get("deaths").and_then(Value::as_u64).unwrap_or(0),
        assists: player.get("assists").and_then(Value::as_u64).unwrap_or(0),
        kill_streak: player
            .get("kill_streak")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    })
}

/// Whether a block holds other blocks rather than a player's own fields, which
/// is the shape Dota sends while a game is being watched rather than played.
fn is_keyed_by_slot(player: &Value) -> bool {
    player
        .as_object()
        .is_some_and(|teams| teams.values().any(Value::is_object))
}

/// A value Dota may send as a string or as a number, as text.
///
/// `matchid` is a string in every payload seen while this was written, and it
/// is a sixty-four-bit number that JSON cannot carry exactly, so a build that
/// started sending it as a number would be a build whose match identifiers this
/// plugin could not compare. Reading both costs one match arm.
fn read_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_payload_from_a_match_reads_as_the_player_it_is_about() {
        let snapshot = Snapshot::read(&json!({
            "map": {
                "matchid": "8421997461",
                "game_state": "DOTA_GAMERULES_STATE_GAME_IN_PROGRESS",
                "win_team": "none",
                "clock_time": 903
            },
            "player": {"team_name": "radiant", "kills": 3, "deaths": 1, "assists": 4,
                       "kill_streak": 2},
            "hero": {"name": "npc_dota_hero_lina", "level": 11}
        }));

        assert_eq!(snapshot.match_id.as_deref(), Some("8421997461"));
        assert_eq!(snapshot.state, Some(GameState::InProgress));
        assert_eq!(snapshot.win_team, None, "`none` is not a team");
        assert_eq!(snapshot.clock_time, Some(903));
        assert_eq!(snapshot.team, Some(Team::Radiant));
        assert_eq!(
            snapshot.counters,
            Some(Counters {
                kills: 3,
                deaths: 1,
                assists: 4,
                kill_streak: 2
            })
        );
        assert_eq!(snapshot.hero.as_deref(), Some("npc_dota_hero_lina"));
        assert!(!snapshot.spectating);
    }

    #[test]
    fn a_payload_with_nothing_in_it_says_nothing_rather_than_failing() {
        // The menu, a loading screen, and a Dota that has changed its mind
        // about a field name all arrive here. None of them is an error, and
        // none of them may be read as "the player now has zero kills".
        for payload in [
            json!({}),
            json!({"provider": {"name": "Dota 2", "appid": 570}}),
            json!({"map": {}, "player": {}, "hero": {}}),
            json!({"map": {"game_state": "DOTA_GAMERULES_STATE_HERO_SELECTION"}}),
        ] {
            let snapshot = Snapshot::read(&payload);
            assert_eq!(snapshot.counters, None, "no counters in {payload}");
            assert!(!snapshot.spectating, "not spectating in {payload}");
        }

        assert_eq!(
            Snapshot::read(&json!({"map": {"game_state": "DOTA_GAMERULES_STATE_PRE_GAME"}})).state,
            Some(GameState::Other("DOTA_GAMERULES_STATE_PRE_GAME".to_owned())),
            "a state this plugin does not act on is carried, not discarded"
        );
    }

    #[test]
    fn a_spectated_game_is_recognised_rather_than_misread() {
        // While watching, `player` holds a block per team and slot. Reading
        // one of those as "the player" would attribute somebody else's kills to
        // whoever is at this machine, which is worse than reporting nothing.
        let snapshot = Snapshot::read(&json!({
            "map": {"matchid": "8421997461"},
            "player": {
                "team2": {"player0": {"kills": 9, "deaths": 0, "team_name": "radiant"}},
                "team3": {"player5": {"kills": 1, "deaths": 4, "team_name": "dire"}}
            }
        }));

        assert!(snapshot.spectating);
        assert_eq!(snapshot.counters, None);
        assert_eq!(snapshot.team, None);
    }

    #[test]
    fn a_match_identifier_is_compared_as_text_whichever_way_it_arrives() {
        let text = Snapshot::read(&json!({"map": {"matchid": "8421997461"}}));
        let number = Snapshot::read(&json!({"map": {"matchid": 8_421_997_461_u64}}));
        assert_eq!(text.match_id, number.match_id);
    }
}
