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
//! ([#43](https://github.com/wildware-uk/clipped/issues/43)), [`epic`] the
//! second, [`ubisoft`] the third, [`xbox`] the fourth, [`battlenet`] the fifth
//! and [`riot`] the sixth
//! ([#44](https://github.com/wildware-uk/clipped/issues/44), which asks for one
//! pull request per launcher and is still open for EA).
//!
//! EA and GOG are deliberately **not** stubbed
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
//! There is still no `trait LauncherProvider`, and the fourth implementation is
//! the one that was expected to settle it. It settled it the other way: Xbox
//! reads a **two-level** registry key whose entries are not all installations,
//! and its identifier has to be *derived* from a package full name rather than
//! read out of a field. The four agree on the same three methods — `discover`,
//! `candidate_for`, `problems` — and agree on nothing else: Steam follows a
//! registry key to a library index to a manifest per application across several
//! drives, Epic reads one directory of JSON, Ubisoft enumerates a registry key
//! and reads a name out of somebody else's, Xbox enumerates two, and Battle.net
//! reads its identifier out of a **command line**.
//!
//! What the later ones *did* change is what is demonstrably shared, which is now
//! shared rather than repeated (AGENTS.md section 55):
//!
//! - [`registry`] — reading a value and enumerating subkeys, for Steam, Ubisoft
//!   and Xbox. Extracted from Steam when Ubisoft needed the same two-call
//!   sizing; Xbox needed the subkey enumeration twice over.
//! - [`claim`] — which installation directory owns a running executable.
//!   Extracted from Epic when Ubisoft needed the same rule, with the same two
//!   details that are easy to get subtly wrong. Xbox uses it unchanged, which is
//!   the first evidence that the extraction was the right shape rather than a
//!   convenient one.
//!
//! That is the case for waiting, not against it: each extraction happened when a
//! second caller appeared and named exactly what the two had in common, which a
//! trait written in advance would have had to guess at.
//!
//! What remains of #44 is EA, Riot and GOG. Riot is the one to leave
//! alone: its `RiotClientInstalls.json` publishes no per-game identifier, and
//! only one of eight `Metadata` directories on a real installation carried an
//! install path, so a provider would find one game out of seven products (#44
//! records the measurements). Writing it now would still be guessing (AGENTS.md,
//! "Do not over-engineer").

pub mod battlenet;
mod claim;
pub mod epic;
mod keyvalues;
mod registry;
pub mod riot;
pub mod steam;
pub mod ubisoft;
pub mod xbox;
