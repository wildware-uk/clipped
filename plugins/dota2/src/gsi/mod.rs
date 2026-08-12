//! Valve's Game State Integration, with no game in it.
//!
//! Counter-Strike 2, Dota 2 and Valve's other titles share one mechanism: a
//! KeyValues file in the game's own directory names a local address, a set of
//! components and a token, and the game POSTs a JSON description of what it can
//! see to that address whenever it changes. Everything about that sentence
//! except *what is in the JSON* is the same for every game, and everything in
//! this module is the part that is the same.
//!
//! | Module | What it does | What it would have to be told to serve another game |
//! | --- | --- | --- |
//! | [`config`] | Writes the configuration file, safely | its title, its address and its component names |
//! | [`secret`] | Generates and remembers the auth token | nothing |
//! | [`listener`] | Binds loopback, authenticates, hands over the state | nothing |
//! | [`cadence`] | Turns an interval between payloads into a moment and a precision | nothing |
//!
//! # Why it is here rather than in a crate of its own
//!
//! Because it is not the only copy. The Counter-Strike 2 integration
//! ([#70](https://github.com/wildware-uk/clipped/issues/70)) is on `main` and
//! implements the same plumbing independently — a KeyValues writer, a loopback
//! listener with request bounds, a shared secret checked on every payload —
//! which makes two, and two is when AGENTS.md section 55 says this belongs in a
//! crate both plugin binaries link (`crates/gsi`) rather than being copied.
//!
//! **It is deliberately not extracted here.** Moving a module out from under
//! two branches that were both open would have conflicted with whichever of
//! them merged second, and the extraction is worth doing once, against both
//! callers, by somebody who can see what they actually have in common. It is
//! proposed with a recommendation on
//! [#69](https://github.com/wildware-uk/clipped/issues/69), which owns the
//! plugin contract, and the deferral is recorded there rather than left as a
//! comment nobody is looking for.
//!
//! Everything here is written to make that extraction a *move*: no type in here
//! names Dota, reads a Dota field or imports [`crate::dota`], its tests do not
//! either, and the two places a game's name could have leaked in — the
//! diagnostics on standard error and the header of the rendered configuration
//! file — take it from the caller or do without it.
//!
//! It could not live in `clipped-plugins`. That crate is the *host* side and is
//! linked into the recorder, and a listening socket and a writer of files
//! inside a game's installation are two things ADR 0002 keeps out of the
//! process that is recording.
//!
//! # What it deliberately does not do
//!
//! **Find the game.** Where a game is installed is a question about a launcher,
//! not about Game State Integration, and `clipped-game-detection` already
//! answers it from Steam's own files on disk (`crate::dota::installation`).
//! Keeping that out of here is what stops this module growing a second copy of
//! Steam library parsing (AGENTS.md section 55).

pub mod cadence;
pub mod config;
pub mod listener;
pub mod secret;

pub use cadence::{Cadence, Window};
pub use config::{ConfigError, Installation, Installed, Integration, Timings};
pub use listener::{Complaints, GameStateListener, Payload, Refusal};
pub use secret::{remembered_token, AuthToken, TokenError};
