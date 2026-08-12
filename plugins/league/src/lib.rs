//! The League of Legends highlight plugin: what the game's own local API says,
//! turned into `clipped_events` and nothing else.
//!
//! The prose version of everything here is `docs/plugin-api.md`, "The League of
//! Legends plugin". The short version:
//!
//! League serves a **Live Client Data API** over HTTPS on `127.0.0.1:2999`
//! while a match is running. It is Riot's own documented interface, it is read
//! by polling it, and it is the only thing this plugin touches — no injection,
//! no reading the game's memory, nothing that resembles a cheat (AGENTS.md
//! section 34). A user's account is worth more than a highlight.
//!
//! # The shape of it
//!
//! | Module | What it is |
//! | --- | --- |
//! | [`snapshot`] | One `GET /liveclientdata/allgamedata` body, read into the four things this plugin needs from it |
//! | [`watch`] | The whole derivation: which snapshots produce which events, when, and what is said when the API misbehaves |
//! | `live_api` | The HTTPS client. Windows only, and the only part of this crate that touches a socket |
//!
//! The split is deliberate and it is the reason there are tests at all. League
//! is not installed on the machine this was written on, and a machine that is
//! also running a game is not somewhere to spin up a TLS server for a test
//! (AGENTS.md section 25). Everything above the socket is a pure function of a
//! payload and a clock reading, so `tests/` drives the real derivation with the
//! real payloads and nothing has to be mocked.
//!
//! The socket is not entirely untested either: `live_api`'s own tests stand
//! plain listeners on ephemeral loopback ports and hold the two properties that
//! do not need TLS to be true — that a redirect is not followed, and that a
//! listener dripping bytes cannot hold the poll loop. What remains untested is
//! the **successful HTTPS request**, which has never run: no match has been
//! played through this plugin. `docs/plugin-api.md` says so plainly rather than
//! implying otherwise (AGENTS.md section 54).
//!
//! # Threading
//!
//! The binary (`src/main.rs`) runs one loop on the main thread — poll, report,
//! sleep — and one thread reading standard input so that `detach` is noticed
//! while the loop is sleeping. Nothing here is shared between them but an
//! [`AtomicBool`](std::sync::atomic::AtomicBool). This crate's library half
//! owns no threads at all and blocks on nothing.

#[cfg(windows)]
pub mod live_api;
pub mod snapshot;
pub mod watch;

pub use snapshot::{GameSnapshot, LiveEvent, PlayerIdentity, SnapshotError};
pub use watch::{LeagueWatch, PollResult, POLL_INTERVAL, REPORTED_TIME_RESOLUTION};
