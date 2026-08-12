//! Launcher providers: asking a shop which of its games a process is.
//!
//! # Why this exists at all
//!
//! [`crate::catalogue`] has a rung above every other in its precedence order —
//! [`MatchStrength::LauncherIdentity`](crate::catalogue::MatchStrength::LauncherIdentity),
//! the one that identifies a game whose process is called `launcher.exe` — and
//! reaching it needs somebody to say "this process is application 730". Nothing
//! did. This module is where that comes from.
//!
//! The shape is one submodule per launcher, which is SPEC.md section 6's
//! "provider-based so that support for a new launcher is an addition rather than
//! a change to shared logic". [`steam`] is the first
//! ([#43](https://github.com/wildware-uk/clipped/issues/43)); Epic, Xbox,
//! Battle.net, EA, Ubisoft, Riot and GOG are
//! [#44](https://github.com/wildware-uk/clipped/issues/44) and are deliberately
//! not stubbed here — an empty provider that always answers "no" is a control
//! that silently does nothing (AGENTS.md section 27), and
//! [`LauncherKind`](crate::catalogue::LauncherKind) already carries the
//! vocabulary they will need.
//!
//! # What every provider is expected to do, and not do
//!
//! - **Read local files only.** No network. A launcher's own metadata is on the
//!   machine, and a detector that needed the internet would stop working on the
//!   train.
//! - **Report, never decide.** A provider answers "which application is this
//!   path?" and hands back a
//!   [`ProcessCandidate`](crate::catalogue::ProcessCandidate). Whether that is a
//!   game worth recording is the catalogue's answer, and what to do about it is
//!   `clipped_session`'s.
//! - **Name the file when something is wrong.** These are files somebody else's
//!   installer wrote, so they will be missing, half-written and occasionally
//!   nonsense; every failure says which file (AGENTS.md section 15).
//!
//! There is no `trait LauncherProvider` yet, on purpose. One implementation is
//! not enough to know what the trait's shape should be, and #44 brings three
//! launchers whose metadata lives in three different kinds of place —
//! `.item` manifests, the package registry, a product database. Writing the
//! abstraction now would be guessing at all three (AGENTS.md, "Do not
//! over-engineer").

mod keyvalues;
pub mod steam;
