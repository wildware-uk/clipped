//! The service on a platform with no global hotkeys.
//!
//! The workspace compiles and unit-tests off Windows so that a contributor's
//! other machine is not useless to them, and the parts of this crate with the
//! interesting logic in them — parsing a combination, refusing one that would
//! swallow typing, the dispatch rules and every threading guarantee — are all
//! platform-independent and run there. What cannot exist is the registration,
//! so it says so plainly rather than being quietly absent, which is the choice
//! `clipped-ipc`'s transport makes for the same reason (AGENTS.md section 54).

use super::{HotkeyError, RegistrationOutcome};
use crate::bindings::Bindings;
use crate::dispatch::Dispatcher;

/// A loop that cannot be started on this platform.
#[derive(Debug)]
pub(super) struct HotkeyLoop(());

impl HotkeyLoop {
    /// Always fails.
    ///
    /// # Errors
    ///
    /// Always [`HotkeyError::Unsupported`].
    pub(super) fn start(
        _bindings: &Bindings,
        _dispatcher: Dispatcher,
    ) -> Result<(Self, Vec<RegistrationOutcome>), HotkeyError> {
        Err(HotkeyError::Unsupported)
    }

    /// Does nothing; there is no loop.
    pub(super) fn stop(self) {}
}
