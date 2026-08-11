//! Watching for processes starting and stopping.
//!
//! This module answers one question — *what started, and what stopped?* — and
//! deliberately not the next one. Deciding that a process is a game is
//! [`crate::catalogue`] and the launcher integrations after it; deciding what to
//! do about it is `clipped_session::automatic`
//! ([issue #46](https://github.com/wildware-uk/clipped/issues/46)). What arrives
//! here is a launch and an executable, and nothing in this module knows what a
//! game is.
//!
//! # The shape of it: observe, then judge
//!
//! ```text
//! WMI notifications ─┐
//!                    ├─ needs Windows ──▶ SourceEvent  (started / exited)
//! snapshot polling  ─┘                          │
//!                                       LaunchDebouncer
//!                                ── pure, and where the rules live ──▶
//!                                               │
//!                              WatchEvent  (launched / exited / source news)
//! ```
//!
//! The split is the same one `clipped-windows` makes between enumerating the
//! desktop and resolving a target, and for the same reason. "What did Windows
//! say?" can only be answered by Windows and has almost no logic in it; "is
//! that one launch or three?" is all logic and needs no machine at all, so it
//! is a state machine over `(event, now)` and is tested against constructed
//! process trees ([`debounce`]).
//!
//! # Why a game launch is not one process
//!
//! Steam starts a launcher, the launcher starts the game, an anti-cheat wrapper
//! may sit between them, and some titles re-execute themselves once. Treating
//! each of those as a launch would have the session layer start and abandon
//! three recordings in two seconds. So starts are collected into a launch by
//! following the parent chain — which is why every event carries a parent — and
//! reported once the chain goes quiet. A process that appears and disappears
//! inside that window is never reported at all, which is what makes a launcher
//! that hands over and exits invisible.
//!
//! # What it costs
//!
//! The watcher does not poll. It blocks, and Windows wakes it. The measured
//! idle cost, the measured detection latency, and the fallback that applies
//! when WMI is unavailable are all in `docs/game-detection.md`.

mod config;
mod debounce;
mod error;
mod process;
mod source;

#[cfg(windows)]
mod watcher;
#[cfg(windows)]
mod windows;

pub use config::WatchConfig;
pub use error::{SourceError, WatchError};
pub use process::{LaunchGroup, LaunchId, ProcessExit, ProcessSnapshot};
pub use source::EventSource;

/// What a wait for the next event found.
///
/// There are three answers and not two, because "nothing happened in the last
/// second" and "nothing will ever happen again" call for opposite responses:
/// the first is the normal state of a machine nobody is playing anything on,
/// and the second means detection is over and the user has to be told. An
/// [`Option`] would spell them the same way, and a caller who did not already
/// know to look for [`WatchEvent::Stopped`] would loop forever on the answer
/// that never changes.
///
/// Deliberately not `#[non_exhaustive]`: the point of the type is that a caller
/// cannot write a loop which forgets [`Self::Finished`], and a wildcard arm
/// would let them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Next {
    /// Something happened, and this is it.
    Event(WatchEvent),

    /// Nothing happened within the timeout.
    ///
    /// The usual answer, and not a failure: most seconds on most machines have
    /// no process events in them that anybody wants.
    Idle,

    /// The watcher has finished. Nothing further will ever be reported.
    ///
    /// [`WatchEvent::Stopped`] is delivered once, with the reason; this is the
    /// answer to every call after it. The wait still costs its timeout, so a
    /// loop that does not break on this answer idles rather than spinning on a
    /// processor for the life of the process.
    Finished,
}

#[cfg(windows)]
pub use watcher::ProcessWatcher;

/// Something the watcher has to report.
///
/// The first two are the ticket: what started, and what stopped. The other two
/// are the watcher talking about itself, and they exist because a detector that
/// has quietly stopped working looks exactly like a machine where nobody is
/// playing anything (AGENTS.md sections 16 and 45).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum WatchEvent {
    /// One launch, with every process the debounce collected into it.
    Launched(LaunchGroup),

    /// A process that had been reported is gone.
    ///
    /// Within one batch these arrive children first, so a consumer tearing down
    /// a session sees the leaf of a process tree go before its parent.
    Exited(ProcessExit),

    /// The watcher changed to a different event source, and why.
    ///
    /// Detection continues, with the latency and cost of the new source. This
    /// is worth surfacing rather than only logging: a machine that has fallen
    /// back to polling is behaving differently from one that has not.
    SourceChanged {
        /// The source that stopped working.
        from: EventSource,
        /// The source now in use.
        to: EventSource,
        /// What went wrong with the old one.
        reason: SourceError,
    },

    /// No source remains. Nothing further will be reported.
    ///
    /// The watcher is finished; a caller that wants detection back has to build
    /// a new one, and should tell the user that automatic detection has stopped
    /// rather than leave them wondering why their game was not recorded.
    Stopped(SourceError),
}
