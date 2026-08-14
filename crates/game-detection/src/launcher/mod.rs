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
//! ([#43](https://github.com/wildware-uk/clipped/issues/43)) and [`epic`] the
//! second ([#44](https://github.com/wildware-uk/clipped/issues/44), which asks
//! for one pull request per launcher and is still open for the rest).
//!
//! Xbox, Battle.net, EA, Ubisoft, Riot and GOG are deliberately **not** stubbed
//! here — an empty provider that always answers "no" is a control that silently
//! does nothing (AGENTS.md section 27), and
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
//! There is still no `trait LauncherProvider`, and two implementations have made
//! the case for waiting rather than weakened it. Steam and Epic agree on three
//! methods — `discover`, `candidate_for`, `problems` — and agree on nothing
//! else: Steam follows a registry key to a library index to a manifest per
//! application across several drives, and Epic reads one directory of JSON. The
//! part that would be shared is the part that is already shared, in
//! [`crate::catalogue`]: `normalise_path`, `path_segments` and
//! `ProcessCandidate`.
//!
//! What remains of #44 is Xbox, whose metadata is in the package registry rather
//! than in any file, and that is the one most likely to decide the trait's shape.
//! Writing it now would still be guessing (AGENTS.md, "Do not over-engineer").

pub mod epic;
mod keyvalues;
pub mod steam;
