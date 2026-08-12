//! What Counter-Strike 2 posts, as much of it as this plugin reads.
//!
//! Game State Integration sends a **snapshot of state**, not a list of things
//! that happened. There is no "kill" message: there is a match statistics block
//! whose `kills` was 8 a moment ago and is 9 now. Turning that into an event is
//! `crate::derive`'s job; this module's job is to read the snapshot, and to
//! keep reading it after Valve next changes it.
//!
//! # Everything is optional, and unknown fields are ignored
//!
//! This is deliberately the **opposite** rule to the plugin manifest's, which
//! refuses a file carrying a field it has not learned (`clipped_plugins`). The
//! reasoning is the same reasoning in both places, applied to two different
//! kinds of document:
//!
//! - A manifest is a *permission document*, written by whoever wants the plugin
//!   to run. Reading it loosely means running a plugin under a declaration
//!   narrower than the one it was written to, which is a permission nobody
//!   granted.
//! - A payload is *another program's output*, arriving from a game that updates
//!   itself without asking. It carries no permission and grants nothing. A
//!   payload whose `map` block has grown a field this build has never seen
//!   still says, correctly, that the player's kill count went up — and refusing
//!   the whole document over the new field would lose that.
//!
//! So every block is an `Option`, every field inside it is an `Option`, and
//! `serde`'s default of ignoring unknown fields is left alone. What this build
//! cannot find, it does not derive events from; what it can find still works.
//! The same applies to the three enumerations below: an unrecognised phase is
//! kept as [`MapPhase::Other`] rather than failing the payload, so a phase
//! added in a future update is a transition this build declines to interpret
//! rather than a plugin that stops working the day the game updates.
//!
//! # What is deliberately not read
//!
//! `previously` and `added` — the blocks Game State Integration adds to say
//! which values changed since **the payload it last sent**. They are the
//! obvious way to derive a difference and this plugin does not use them, for
//! one reason: they are relative to a payload we may never have received. A
//! post that was dropped, refused for a bad token or overtaken by the next one
//! makes `previously` describe a baseline this plugin never held, and a
//! difference measured against the wrong baseline is a kill nobody got. The
//! last payload this plugin *accepted* is a baseline it can be sure of, so that
//! is what `crate::derive` compares against.

use serde::Deserialize;

/// One Game State Integration payload.
///
/// Every block is optional because Counter-Strike sends only what it has: in
/// the main menu there is no `map` and no `round`, and a payload that arrives
/// while the player is dead and spectating still carries a `player` block —
/// describing somebody else. See [`PlayerState::steam_id`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct GsiPayload {
    /// The game that sent this, and when it says it sent it.
    pub provider: Option<Provider>,
    /// The map and the match on it.
    pub map: Option<MapState>,
    /// The round inside that match.
    pub round: Option<RoundState>,
    /// The player this payload describes, who is not always the local one.
    pub player: Option<PlayerState>,
    /// The token from the configuration file this plugin installed.
    pub auth: Option<Auth>,
}

impl GsiPayload {
    /// Reads one payload.
    ///
    /// # Errors
    ///
    /// The `serde_json` failure. A payload that is not JSON at all is a
    /// protocol fault worth counting; a payload that is JSON in a shape this
    /// build does not recognise is not, and reads as a payload with nothing in
    /// it.
    pub fn parse(json: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(json)
    }

    /// The token the payload carries, if it carries one.
    #[must_use]
    pub fn token(&self) -> Option<&str> {
        self.auth.as_ref()?.token.as_deref()
    }

    /// The second the game stamped this payload with.
    ///
    /// Unix seconds, and the only ordering information a payload carries: there
    /// is no sequence number. `crate::derive` uses it to refuse a payload older
    /// than the last one it accepted.
    #[must_use]
    pub fn timestamp(&self) -> Option<i64> {
        self.provider.as_ref()?.timestamp
    }

    /// Whether the `player` block describes the person running the game.
    ///
    /// `provider.steamid` is whoever the game is running as; `player.steamid`
    /// is whoever the payload is about, and after dying in a competitive match
    /// those differ for the rest of the round, because the camera has moved to
    /// a teammate. Counting a teammate's kills as the local player's is the
    /// most obvious way this plugin could invent events, so both identifiers
    /// have to be present *and* equal before anything in the `player` block is
    /// believed. Missing either is answered `false`: an unknown identity is not
    /// a matching one.
    #[must_use]
    pub fn describes_the_local_player(&self) -> bool {
        let provider = self.provider.as_ref().and_then(|it| it.steam_id.as_deref());
        let player = self.player.as_ref().and_then(|it| it.steam_id.as_deref());
        match (provider, player) {
            (Some(provider), Some(player)) => provider == player,
            _ => false,
        }
    }
}

/// Which game sent the payload, and when.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Provider {
    /// `Counter-Strike: Global Offensive`, which is what Counter-Strike 2 still
    /// calls itself here.
    pub name: Option<String>,
    /// Steam's application identifier: 730.
    pub appid: Option<u64>,
    /// Who the game is running as. See
    /// [`GsiPayload::describes_the_local_player`].
    #[serde(rename = "steamid")]
    pub steam_id: Option<String>,
    /// Unix seconds, as the game stamped it.
    pub timestamp: Option<i64>,
}

/// The map, and the match being played on it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct MapState {
    /// `de_dust2`.
    pub name: Option<String>,
    /// `competitive`, `casual`, `deathmatch`.
    pub mode: Option<String>,
    /// Where the match is up to.
    pub phase: Option<MapPhase>,
    /// How many rounds have been completed.
    pub round: Option<u32>,
    /// The Counter-Terrorist side.
    pub team_ct: Option<TeamState>,
    /// The Terrorist side.
    pub team_t: Option<TeamState>,
}

/// One side's standing in the match.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct TeamState {
    /// Rounds won.
    pub score: Option<u32>,
}

/// The round inside a match.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct RoundState {
    /// Where the round is up to.
    pub phase: Option<RoundPhase>,
    /// Which side won it, once it is over.
    pub win_team: Option<Team>,
    /// `planted`, `defused`, `exploded`.
    pub bomb: Option<String>,
}

/// The player a payload describes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct PlayerState {
    /// Who this is. Compared against the provider's, never assumed to be the
    /// local player.
    #[serde(rename = "steamid")]
    pub steam_id: Option<String>,
    /// Which side they are on, which is what turns a match result into a
    /// [`win`](clipped_events::EventKind::Win) or a
    /// [`loss`](clipped_events::EventKind::Loss).
    pub team: Option<Team>,
    /// Per-round counters, which reset when a round does.
    pub state: Option<PlayerRoundState>,
    /// Per-match counters, which do not.
    pub match_stats: Option<MatchStats>,
}

/// Counters that reset at the start of every round.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct PlayerRoundState {
    /// Kills so far this round.
    pub round_kills: Option<u32>,
    /// How many of them were headshots.
    #[serde(rename = "round_killhs")]
    pub round_headshot_kills: Option<u32>,
}

/// Counters that run for the whole match.
///
/// These are what a kill, a death and an assist are derived from, because they
/// are the only numbers in the payload that count the things this plugin
/// reports and do not reset between rounds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct MatchStats {
    /// Kills this match.
    pub kills: Option<u32>,
    /// Assists this match.
    pub assists: Option<u32>,
    /// Deaths this match.
    pub deaths: Option<u32>,
}

/// The token Counter-Strike echoes back from the configuration file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Auth {
    /// The shared secret `crate::integration` wrote into the game's
    /// configuration directory.
    pub token: Option<String>,
}

/// Where a match is up to.
///
/// [`Other`](Self::Other) is what keeps this readable after a game update: an
/// unrecognised phase is kept verbatim rather than failing the payload, and
/// `crate::derive` treats it as a phase it will not interpret rather than as an
/// error.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(from = "String")]
pub enum MapPhase {
    /// Before the match proper.
    Warmup,
    /// Being played.
    Live,
    /// Half time.
    Intermission,
    /// Finished.
    GameOver,
    /// Something this build has not been taught.
    Other(String),
}

impl From<String> for MapPhase {
    fn from(value: String) -> Self {
        match value.as_str() {
            "warmup" => Self::Warmup,
            "live" => Self::Live,
            "intermission" => Self::Intermission,
            "gameover" => Self::GameOver,
            _ => Self::Other(value),
        }
    }
}

/// Where a round is up to.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(from = "String")]
pub enum RoundPhase {
    /// Buy time, before anybody can move.
    FreezeTime,
    /// Being played.
    Live,
    /// Decided, with the next one not yet started.
    Over,
    /// Something this build has not been taught.
    Other(String),
}

impl From<String> for RoundPhase {
    fn from(value: String) -> Self {
        match value.as_str() {
            "freezetime" => Self::FreezeTime,
            "live" => Self::Live,
            "over" => Self::Over,
            _ => Self::Other(value),
        }
    }
}

/// A side.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(from = "String")]
pub enum Team {
    /// Counter-Terrorists.
    CounterTerrorist,
    /// Terrorists.
    Terrorist,
    /// Something this build has not been taught.
    Other(String),
}

impl Team {
    /// The word the game used, which is what goes in an event's payload.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::CounterTerrorist => "CT",
            Self::Terrorist => "T",
            Self::Other(other) => other,
        }
    }
}

impl From<String> for Team {
    fn from(value: String) -> Self {
        match value.as_str() {
            "CT" => Self::CounterTerrorist,
            "T" => Self::Terrorist,
            _ => Self::Other(value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The payload a live competitive round produces, as
    /// `tests/payloads/live_round.json` holds it.
    const LIVE: &str = include_str!("../tests/payloads/live_round.json");

    #[test]
    fn a_payload_reads_into_the_blocks_the_derivation_needs() {
        let payload = GsiPayload::parse(LIVE.as_bytes()).expect("the fixture is a payload");

        assert_eq!(payload.timestamp(), Some(1_754_899_215));
        assert_eq!(payload.token(), Some("fixture-token-not-a-secret"));
        assert_eq!(
            payload.map.as_ref().and_then(|map| map.name.as_deref()),
            Some("de_dust2")
        );
        assert_eq!(
            payload.map.as_ref().and_then(|map| map.phase.clone()),
            Some(MapPhase::Live)
        );
        assert_eq!(
            payload.round.as_ref().and_then(|round| round.phase.clone()),
            Some(RoundPhase::Live)
        );
        assert_eq!(
            payload
                .player
                .as_ref()
                .and_then(|player| player.match_stats)
                .and_then(|stats| stats.kills),
            Some(8)
        );
        assert!(payload.describes_the_local_player());
    }

    #[test]
    fn a_field_this_build_has_never_seen_does_not_cost_the_payload() {
        // The rule this module exists to state. A game update that adds a block
        // and a field must not stop a kill being reported, so the new shape is
        // read for what this build understands and the rest is ignored.
        let json = br#"{
            "provider": {"steamid": "76561198000000001", "timestamp": 12, "future": "?"},
            "map": {"name": "de_nuke", "phase": "live", "hostage_rescue_count": 2},
            "player": {"steamid": "76561198000000001", "match_stats": {"kills": 3, "adr": 91.4}},
            "grenades": {"1": {"type": "smoke"}}
        }"#;

        let payload = GsiPayload::parse(json).expect("a payload with unknown fields still reads");
        assert_eq!(
            payload
                .player
                .and_then(|player| player.match_stats)
                .and_then(|stats| stats.kills),
            Some(3)
        );
    }

    #[test]
    fn an_unrecognised_phase_is_kept_rather_than_failing_the_payload() {
        let json = br#"{"map": {"phase": "surrender_vote"}, "round": {"phase": "paused"}}"#;
        let payload = GsiPayload::parse(json).expect("an unknown phase still reads");

        assert_eq!(
            payload.map.and_then(|map| map.phase),
            Some(MapPhase::Other("surrender_vote".to_owned())),
            "the word the game used is kept, so a log line can name it"
        );
        assert_eq!(
            payload.round.and_then(|round| round.phase),
            Some(RoundPhase::Other("paused".to_owned()))
        );
    }

    #[test]
    fn a_payload_about_a_spectated_teammate_is_not_about_the_local_player() {
        // Dying in a competitive match moves the camera to a teammate, and the
        // `player` block follows the camera. This is the check that stops their
        // kills becoming ours.
        let spectating: &str = include_str!("../tests/payloads/spectating_teammate.json");
        let payload = GsiPayload::parse(spectating.as_bytes()).expect("the fixture is a payload");

        assert!(!payload.describes_the_local_player());
        assert_eq!(
            payload
                .player
                .and_then(|player| player.match_stats)
                .and_then(|stats| stats.kills),
            Some(21),
            "the teammate's counters are read; what matters is that they are not believed"
        );
    }

    #[test]
    fn an_identity_that_is_missing_is_not_an_identity_that_matches() {
        let no_provider = br#"{"player": {"steamid": "76561198000000001"}}"#;
        assert!(!GsiPayload::parse(no_provider)
            .expect("it reads")
            .describes_the_local_player());

        let no_player = br#"{"provider": {"steamid": "76561198000000001"}}"#;
        assert!(!GsiPayload::parse(no_player)
            .expect("it reads")
            .describes_the_local_player());
    }
}
