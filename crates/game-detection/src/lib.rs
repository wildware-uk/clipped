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
//! edit. The **process watcher**
//! ([issue #41](https://github.com/wildware-uk/clipped/issues/41)) reports what
//! started and what stopped, debounced so that a launcher handing over to a
//! game is one launch rather than three, and without polling for it.
//!
//! Nothing yet joins them: the watcher does not consult the catalogue, and the
//! launcher providers that would supply a launcher identity are
//! [#43](https://github.com/wildware-uk/clipped/issues/43) onwards.
//! `docs/game-detection.md` is the subsystem document.

pub mod catalogue;
mod process_watcher;

pub use process_watcher::{
    EventSource, LaunchGroup, LaunchId, Next, ProcessExit, ProcessSnapshot, SourceError,
    WatchConfig, WatchError, WatchEvent,
};

#[cfg(windows)]
pub use process_watcher::ProcessWatcher;
