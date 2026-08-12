//! One answer from `GET /liveclientdata/allgamedata`, read into the four things
//! this plugin needs and nothing else.
//!
//! # Why the whole payload, and not `/eventdata`
//!
//! The events are available on their own, and they are the smaller request. The
//! reason for asking for everything is that an event's time is **match
//! relative** — `"EventTime": 213.4` is seconds since the match began — and the
//! recording's timeline is not. Turning one into the other needs the match
//! clock *as it was when that list was produced*, which is `gameData.gameTime`.
//! Two requests would give a list from one instant and a clock from another,
//! and the gap between them would be an error in every event's position that
//! nothing downstream could see. One request gives both from the same instant.
//!
//! The cost is a body of tens of kilobytes rather than a few, once a second, on
//! a machine that is also running a game (AGENTS.md section 18). That is
//! measured in microseconds of `serde_json`, and it buys away a whole class of
//! timing error.
//!
//! # Reading it as leniently as it can be read
//!
//! A patch can add a field, a section or a kind of event at any time, and a
//! plugin that failed on one would stop reporting the events it *does*
//! understand — for a game that updates every fortnight. So:
//!
//! - Unknown fields and unknown sections are ignored, which is `serde`'s
//!   default and is the whole reason nothing here is `deny_unknown_fields`.
//! - Unknown *event names* are ignored one at a time, by [`watch`](crate::watch).
//! - An entry in the event list that cannot be read at all is skipped and
//!   **counted** ([`GameSnapshot::unreadable_entries`]), rather than failing the
//!   payload it is in. One event nobody can interpret should not cost the nine
//!   beside it (AGENTS.md section 15: it is counted and said out loud, not
//!   swallowed).
//! - What is *not* optional is `gameData.gameTime`. Without it an event has no
//!   position on any timeline, and a body that has none is not a snapshot —
//!   which is exactly how the endpoint's own "no active game" answer is
//!   refused rather than read as an empty match.

use core::fmt;

use serde::Deserialize;
use serde_json::{Map, Value};

/// Everything one poll of the Live Client Data API tells this plugin.
#[derive(Debug, Clone, PartialEq)]
pub struct GameSnapshot {
    /// Who the person at the keyboard is, when the payload says.
    active_player: Option<PlayerIdentity>,
    /// The match clock, in seconds, at the moment this payload was produced.
    game_time: f64,
    /// Every event the match has produced so far, in the order it arrived.
    events: Vec<LiveEvent>,
    /// How many entries in the event list could not be read at all.
    unreadable_entries: usize,
}

impl GameSnapshot {
    /// Reads one `allgamedata` body.
    ///
    /// # Errors
    ///
    /// [`SnapshotError`] when the body is not JSON, or is JSON that carries no
    /// match clock. Both mean "this is not a snapshot"; the caller counts them
    /// and decides when a run of them is worth telling the user about
    /// ([`LeagueWatch`](crate::LeagueWatch)).
    pub fn parse(body: &str) -> Result<Self, SnapshotError> {
        let raw: RawSnapshot =
            serde_json::from_str(body).map_err(|source| SnapshotError::Unreadable { source })?;

        if !raw.game_data.game_time.is_finite() || raw.game_data.game_time < 0.0 {
            return Err(SnapshotError::ImpossibleClock {
                game_time: raw.game_data.game_time,
            });
        }

        let mut events = Vec::with_capacity(raw.events.events.len());
        let mut unreadable_entries = 0;
        for entry in raw.events.events {
            match LiveEvent::read(entry) {
                Some(event) => events.push(event),
                None => unreadable_entries += 1,
            }
        }

        Ok(Self {
            active_player: raw.active_player.and_then(PlayerIdentity::from_payload),
            game_time: raw.game_data.game_time,
            events,
            unreadable_entries,
        })
    }

    /// Who the payload says is playing, if it says.
    ///
    /// Absent when spectating, and absent on a client that stopped reporting
    /// the fields this build reads. Without it a kill cannot be told from a
    /// death, which is why [`watch`](crate::watch) says so out loud rather than
    /// reporting nothing and leaving the user to wonder.
    #[must_use]
    pub fn active_player(&self) -> Option<&PlayerIdentity> {
        self.active_player.as_ref()
    }

    /// The match clock when this payload was produced, in seconds.
    #[must_use]
    pub const fn game_time(&self) -> f64 {
        self.game_time
    }

    /// Every event of the match so far, as the client reported them.
    #[must_use]
    pub fn events(&self) -> &[LiveEvent] {
        &self.events
    }

    /// How many entries of the event list this build could not read.
    ///
    /// Zero in every payload seen so far. It is kept because the alternative to
    /// counting is discarding quietly, and a mark missing from a timeline with
    /// nothing anywhere saying why is the failure AGENTS.md section 15 is
    /// about.
    #[must_use]
    pub const fn unreadable_entries(&self) -> usize {
        self.unreadable_entries
    }
}

/// One entry of the match's event list.
///
/// The fields the events this plugin reports need, and the whole original
/// object underneath — see [`Self::payload`].
#[derive(Debug, Clone, PartialEq)]
pub struct LiveEvent {
    /// The fields this build reads.
    read: RawEvent,
    /// The entry exactly as it arrived, including everything above and anything
    /// a later patch added.
    ///
    /// This is what an event's payload is built from, so a field this build has
    /// never heard of still reaches the recording rather than being dropped on
    /// the way through (`docs/plugin-api.md`: `data` is opaque above the
    /// plugin, and a plugin that filtered it to the fields it happened to know
    /// would be deciding what a future build may see).
    original: Map<String, Value>,
}

impl LiveEvent {
    /// Reads one entry, or `None` when it is not one this build can use.
    ///
    /// An entry with no identifier cannot be told apart from the next poll's
    /// copy of itself, and an entry with no usable time cannot be placed, so
    /// neither is an event — but neither is a reason to lose the payload it
    /// arrived in, which is why this is an `Option` rather than an error the
    /// caller has to decide to swallow.
    fn read(entry: Value) -> Option<Self> {
        let Value::Object(original) = entry else {
            return None;
        };
        let read: RawEvent = serde_json::from_value(Value::Object(original.clone())).ok()?;
        if !read.event_time.is_finite() {
            return None;
        }
        Some(Self { read, original })
    }

    /// The match's own index for this event, counting from zero.
    ///
    /// This is what makes a *polled* feed lossless: the list is cumulative and
    /// this number never moves, so a client that missed three polls asks for
    /// everything above the last identifier it saw and gets exactly the events
    /// it missed, in order, once.
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.read.event_id
    }

    /// What the game calls it: `ChampionKill`, `GameEnd`, `DragonKill`.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.read.event_name
    }

    /// When it happened, in seconds since the match began.
    #[must_use]
    pub const fn time(&self) -> f64 {
        self.read.event_time
    }

    /// Who the game says did it.
    #[must_use]
    pub fn killer(&self) -> Option<&str> {
        self.read.killer_name.as_deref()
    }

    /// Who the game says it happened to.
    #[must_use]
    pub fn victim(&self) -> Option<&str> {
        self.read.victim_name.as_deref()
    }

    /// Who the game says helped.
    #[must_use]
    pub fn assisters(&self) -> &[String] {
        &self.read.assisters
    }

    /// How the match ended for the active player, on a `GameEnd`.
    #[must_use]
    pub fn result(&self) -> Option<&str> {
        self.read.result.as_deref()
    }

    /// What an event reported from this entry carries as its payload.
    ///
    /// The entry's own fields, minus the two the [`EventKind`] and the
    /// recording's timeline already say:
    ///
    /// - `EventID` is an index into a list that only exists inside League, and
    ///   it means nothing to a timeline.
    /// - `EventName` is what the kind was derived *from*. Keeping it would
    ///   invite a consumer to switch on it, which is a game's protocol reaching
    ///   above the plugin boundary — the thing `docs/plugin-api.md` says the
    ///   answer to is a new kind, never a special case.
    ///
    /// `EventTime` stays, because it is the one fact here that the envelope
    /// genuinely cannot carry: where the event sits in *the match*, which is
    /// not where it sits in the recording.
    ///
    /// [`EventKind`]: clipped_events::EventKind
    #[must_use]
    pub fn payload(&self) -> Map<String, Value> {
        let mut payload = self.original.clone();
        payload.remove("EventID");
        payload.remove("EventName");
        payload
    }
}

/// Who the person at the keyboard is, in every name the client gives for them.
///
/// League has spent several years moving from summoner names to Riot IDs, and
/// which of them the event list uses has changed with it. Rather than pick one
/// and be wrong on some patch, this holds every name the payload offered and
/// asks whether a name in an event is one of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerIdentity {
    /// `Rosalind#EU1`, `Rosalind`, and whatever else the payload offered, with
    /// the empty ones left out.
    aliases: Vec<String>,
    /// Whether any of them carries a `#tag`.
    tagged: bool,
}

impl PlayerIdentity {
    /// An identity from the names a payload gave, or `None` if it gave none.
    fn from_payload(player: RawActivePlayer) -> Option<Self> {
        Self::from_names([
            player.riot_id,
            player.riot_id_game_name,
            player.summoner_name,
        ])
    }

    /// An identity from a list of names, ignoring the empty ones.
    #[must_use]
    pub fn from_names<I: IntoIterator<Item = String>>(names: I) -> Option<Self> {
        let mut aliases: Vec<String> = names.into_iter().filter(|name| !name.is_empty()).collect();
        aliases.dedup();
        if aliases.is_empty() {
            return None;
        }
        let tagged = aliases.iter().any(|alias| alias.contains('#'));
        Some(Self { aliases, tagged })
    }

    /// Whether a name in an event is this player.
    ///
    /// Exact against every alias, and that is nearly the whole rule. The one
    /// piece of leniency is for a client that reports the player as
    /// `Rosalind` while its events say `Rosalind#EU1`: when *no* alias carries
    /// a tag, a tagged name is compared without it.
    ///
    /// That exception is deliberately not the other way round. Two players in
    /// one match can share a game name and differ only by tag, so stripping the
    /// tag off an event's name when this build has been given the player's full
    /// Riot ID would be trading a certain answer for an ambiguous one — and the
    /// event it got wrong would be a kill attributed to the person who died.
    #[must_use]
    pub fn matches(&self, name: &str) -> bool {
        if self.aliases.iter().any(|alias| alias == name) {
            return true;
        }
        if self.tagged {
            return false;
        }
        let untagged = name.split('#').next().unwrap_or(name);
        self.aliases.iter().any(|alias| alias == untagged)
    }

    /// Every name this player is known by here.
    #[must_use]
    pub fn aliases(&self) -> &[String] {
        &self.aliases
    }
}

/// Why a body was not a snapshot.
#[derive(Debug)]
pub enum SnapshotError {
    /// It was not JSON this build could read.
    Unreadable {
        /// What `serde_json` made of it.
        source: serde_json::Error,
    },
    /// It carried a match clock that cannot be a match clock.
    ImpossibleClock {
        /// What it said.
        game_time: f64,
    },
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable { source } => write!(
                formatter,
                "the Live Client Data API answered with something this build could not read: \
                 {source}"
            ),
            Self::ImpossibleClock { game_time } => write!(
                formatter,
                "the Live Client Data API reported a match clock of {game_time} seconds, which is \
                 not a moment an event can be placed against"
            ),
        }
    }
}

impl core::error::Error for SnapshotError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Unreadable { source } => Some(source),
            Self::ImpossibleClock { .. } => None,
        }
    }
}

/// The payload, as far as this plugin reads it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSnapshot {
    #[serde(default)]
    active_player: Option<RawActivePlayer>,
    #[serde(default)]
    events: RawEventLog,
    /// The one required section: an event with no match clock to measure it
    /// against is not an event this plugin can place.
    game_data: RawGameData,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawActivePlayer {
    #[serde(default)]
    riot_id: String,
    #[serde(default)]
    riot_id_game_name: String,
    #[serde(default)]
    summoner_name: String,
}

/// The fields of an event entry this build reads.
#[derive(Debug, Clone, PartialEq, Deserialize)]
struct RawEvent {
    #[serde(rename = "EventID")]
    event_id: u64,
    #[serde(rename = "EventName")]
    event_name: String,
    #[serde(rename = "EventTime")]
    event_time: f64,
    #[serde(rename = "KillerName", default)]
    killer_name: Option<String>,
    #[serde(rename = "VictimName", default)]
    victim_name: Option<String>,
    #[serde(rename = "Assisters", default)]
    assisters: Vec<String>,
    #[serde(rename = "Result", default)]
    result: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawEventLog {
    /// Read as values rather than as events, so that one entry this build
    /// cannot interpret costs one entry.
    #[serde(rename = "Events", default)]
    events: Vec<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawGameData {
    game_time: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_body_with_no_match_clock_is_not_a_snapshot() {
        // This is how the endpoint's own "there is no game" answer is refused.
        // Reading it as an empty match would be a plugin inventing a match that
        // is not being played.
        let refusal = GameSnapshot::parse(r#"{"errorCode":"RESOURCE_NOT_FOUND","httpStatus":404}"#)
            .expect_err("an error body is not a snapshot");
        assert!(matches!(refusal, SnapshotError::Unreadable { .. }));

        let refusal =
            GameSnapshot::parse(r#"{"gameData":{"gameTime":-4.0},"events":{"Events":[]}}"#)
                .expect_err("a negative match clock is not a match clock");
        assert!(matches!(refusal, SnapshotError::ImpossibleClock { .. }));
        assert!(refusal.to_string().contains("-4"), "{refusal}");
    }

    #[test]
    fn an_entry_that_cannot_be_read_costs_one_entry() {
        let snapshot = GameSnapshot::parse(
            r#"{"gameData":{"gameTime":10.0},"events":{"Events":[
                 {"EventID":0,"EventName":"GameStart","EventTime":0.03},
                 {"EventName":"NoIdentifier","EventTime":1.0},
                 {"EventID":2,"EventName":"MinionsSpawning","EventTime":65.0}]}}"#,
        )
        .expect("the payload is a snapshot");

        assert_eq!(snapshot.unreadable_entries(), 1);
        assert_eq!(
            snapshot
                .events()
                .iter()
                .map(LiveEvent::name)
                .collect::<Vec<_>>(),
            vec!["GameStart", "MinionsSpawning"],
            "the readable entries either side of it survive"
        );
    }

    #[test]
    fn an_events_payload_keeps_what_the_game_said_and_drops_what_the_envelope_says() {
        let snapshot = GameSnapshot::parse(
            r#"{"gameData":{"gameTime":220.0},"events":{"Events":[
                 {"EventID":2,"EventName":"ChampionKill","EventTime":213.4,
                  "KillerName":"Rosalind#EU1","VictimName":"Kestrel#EUW",
                  "Assisters":["Bramble#EU1"],"KillType":"SOMETHING_NEW"}]}}"#,
        )
        .expect("the payload is a snapshot");

        let payload = snapshot.events()[0].payload();
        assert_eq!(payload["KillerName"], Value::from("Rosalind#EU1"));
        assert_eq!(payload["EventTime"], Value::from(213.4));
        assert_eq!(
            payload["KillType"],
            Value::from("SOMETHING_NEW"),
            "a field this build has never heard of still reaches the recording"
        );
        assert!(!payload.contains_key("EventID"), "{payload:?}");
        assert!(!payload.contains_key("EventName"), "{payload:?}");
    }

    #[test]
    fn a_player_is_matched_by_any_name_the_client_gave_for_them() {
        let identity = PlayerIdentity::from_names([
            "Rosalind#EU1".to_owned(),
            "Rosalind".to_owned(),
            String::new(),
        ])
        .expect("a payload that names the player");

        assert!(identity.matches("Rosalind#EU1"));
        assert!(identity.matches("Rosalind"));
        assert!(!identity.matches("Kestrel#EUW"));
        assert!(
            !identity.matches("Rosalind#EUW"),
            "another player who shares the game name and not the tag is another player"
        );
    }

    #[test]
    fn a_client_that_gives_no_tag_still_matches_a_tagged_event() {
        let identity = PlayerIdentity::from_names(["Rosalind".to_owned()])
            .expect("a payload that names the player");
        assert!(identity.matches("Rosalind#EU1"));
        assert!(!identity.matches("Kestrel#EUW"));

        assert_eq!(
            PlayerIdentity::from_names([String::new(), String::new()]),
            None,
            "a payload that names nobody identifies nobody"
        );
    }
}
