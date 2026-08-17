//! Detection of running games and their launchers.
//!
//! Detection is provider-based so that support for a new launcher is an
//! addition rather than a change to shared logic (SPEC.md section 6).
//!
//! # Responsibilities
//!
//! - Watching for process start and exit.
//! - Matching processes against the known-game database and launcher installs.
//! - User overrides: manual registration, renaming and exclusion.
//!
//! # Not responsible for
//!
//! Starting or stopping recordings; it reports what is running and
//! `clipped-session` decides what to do about it.
//!
//! # Position in the architecture
//!
//! Sits above `clipped-windows` and below `clipped-session`.
//!
//! # What exists today
//!
//! Two of the three parts this crate will have. The **game catalogue**
//! ([issue #42](https://github.com/wildware-uk/clipped/issues/42)) answers
//! "given a running process, which game is this?" from data a contributor can
//! edit — and, since
//! [#45](https://github.com/wildware-uk/clipped/issues/45), data the user can
//! edit too: [`catalogue::Overlay`] registers a game Clipped does not know,
//! renames one, and excludes one, and
//! [`catalogue::Catalogue::explain_process`] says why a process matched what it
//! did. The **process watcher**
//! ([issue #41](https://github.com/wildware-uk/clipped/issues/41)) reports what
//! started and what stopped, debounced so that a launcher handing over to a
//! game is one launch rather than three, and without polling for it.
//!
//! Nothing in *this* crate joins them, deliberately: the watcher does not
//! consult the catalogue. What asks one about the other, and starts a
//! recording, is `clipped_session::automatic`
//! ([#46](https://github.com/wildware-uk/clipped/issues/46),
//! `docs/sessions.md`), which is where that decision belongs — this crate
//! reports what is running and nothing more.
//!
//! The third part is the **launcher providers**
//! ([#43](https://github.com/wildware-uk/clipped/issues/43) onwards), which
//! supply the launcher identity the catalogue's strongest matching rung needs.
//! [`launcher::steam`] is the first of them: it reads Steam's own library index
//! and application manifests off the local disk and answers "which Steam
//! application is this executable?", which is what turns a process called
//! `launcher.exe` into a game with a name. #44 adds the other shops.
//!
//! `docs/game-detection.md` is the subsystem document.

pub mod catalogue;
pub mod launcher;
mod process_watcher;

/// The scratch directory the tests in this crate build fixtures in. Never built
/// into the library.
#[cfg(test)]
pub(crate) mod test_support;

pub use process_watcher::{
    EventSource, LaunchGroup, LaunchId, Next, ProcessExit, ProcessSnapshot, SourceError,
    WatchConfig, WatchError, WatchEvent,
};

#[cfg(windows)]
pub use process_watcher::ProcessWatcher;
