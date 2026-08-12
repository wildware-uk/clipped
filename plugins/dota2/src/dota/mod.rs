//! Dota 2's own vocabulary, and the decision about how much of it becomes an
//! event.
//!
//! Nothing in this module opens a socket, writes a file or reads a clock. It
//! takes the JSON Dota posts and says what changed (`crate::gsi` does the rest),
//! which is what makes every rule below testable against a committed sample
//! payload rather than against an installed game.
//!
//! # What maps to the project's vocabulary
//!
//! `crates/events` defines a closed set of kinds, and a variant of it is a
//! concept the *application* acts on rather than one a particular game happens
//! to have (`docs/plugin-api.md`). These are the ones Dota's state supports:
//!
//! | Event | Derived from | Note |
//! | --- | --- | --- |
//! | `match_started` | `map.game_state` becoming `DOTA_GAMERULES_STATE_GAME_IN_PROGRESS` | The horn, not the draft |
//! | `match_ended` | it becoming `DOTA_GAMERULES_STATE_POST_GAME` | |
//! | `kill` | `player.kills` increasing | One event per step it took |
//! | `death` | `player.deaths` increasing | |
//! | `assist` | `player.assists` increasing | |
//! | `win` / `loss` | `map.win_team` naming a team, read against `player.team_name` | Neither is reported when the payload does not say which team the player is on |
//!
//! # What is reported under this plugin's own name
//!
//! One thing, and the shortness of this list is the point. `Custom` exists for
//! what a universal vocabulary should not pretend to know, and a plugin that
//! answered every question with a custom name would turn a shared model into a
//! bag of strings (`docs/plugin-api.md`, "Custom events").
//!
//! | Event | Derived from | Why it is not one of the above |
//! | --- | --- | --- |
//! | `dota-2.kill_streak` | `player.kill_streak` reaching three | It is not `score`, which is points, and not `achievement`, which is something the game awarded. It is Dota's killing spree, and Dota is the only game here that has one |
//!
//! # What does not map, and is therefore not reported
//!
//! This is the more useful half of the table, because it is the half a reader
//! would otherwise assume was an oversight.
//!
//! | Not reported | Why |
//! | --- | --- |
//! | `round_started`, `round_ended` | **Dota has no rounds.** A lane phase is not a round and neither is a fight. Mapping something onto them so that the row is filled in would put marks on a timeline that mean nothing consistent |
//! | `score`, `goal` | Last hits, denies and net worth are not points scored, and nothing in the subscribed components says an objective was completed |
//! | `achievement` | Dota does not award anything through this interface |
//! | `game_started`, `game_ended` | The application already reports these from the process watcher (issues #41 and #46), and this plugin cannot do better: Game State Integration starts talking well after the process does |
//! | **Roshan, the Aegis, towers, barracks, wards, runes, first blood** | Not in the components this plugin subscribes to. See below |
//!
//! ## Roshan, towers and wards, specifically
//!
//! The issue that asked for this plugin named them, and they are exactly the
//! Dota-shaped events a highlight reel wants. They are **not reported**, and
//! the reason is worth stating plainly rather than leaving as an absence:
//!
//! - `provider`, `map`, `player` and `hero` are what this plugin subscribes to
//!   and what its tests are written against. None of them carries a building,
//!   a ward, a rune or Roshan.
//! - Inferring them from what *is* there — a fight near the pit, a sudden gold
//!   gain — would be a guess presented as a fact. `crates/events` has a
//!   `confidence` field for sources that guess, and a plugin over an
//!   authoritative feed reporting a guessed Roshan kill at `1.0` would be
//!   lying; reporting it at `0.4` would be inventing a number (AGENTS.md
//!   section 27).
//! - Reading them out of the game's memory, or out of a log the game does not
//!   publish, is what AGENTS.md section 34 forbids and is not on the table at
//!   any price.
//!
//! What would change that is a component that carries them. If a Dota build
//! posts buildings, runes or an event list to an integration, the mapping is
//! already decided: a tower or a barracks is a `goal` — an objective completed
//! — and Roshan, the Aegis and a ward are `dota-2.` names, because no other
//! game has them. Adding them then is a subscription, a diff and a row in this
//! table.

pub mod installation;
pub mod snapshot;
pub mod watch;

pub use installation::{configuration_directory, InstallationError, APP_ID, CONFIG_FILE};
pub use snapshot::{Counters, GameState, Snapshot, Team};
pub use watch::{kill_streak_kind, Notice, Observed, Report, Watcher, KILL_STREAK_NAME};

/// The components this plugin asks Dota to include in its payloads.
///
/// Deliberately the smallest set the rules above need. Every component is data
/// the game gathers and posts while it is drawing frames, and one this plugin
/// would not read is one it has asked the game to do work for nothing (SPEC.md
/// section 2: background analysis must never interfere with the game).
///
/// `provider` is here despite nothing being derived from it because it is how a
/// payload says which game and which build sent it, which is the first thing
/// anybody diagnosing an integration wants to see.
pub const COMPONENTS: [&str; 4] = ["provider", "map", "player", "hero"];
