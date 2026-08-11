//! Which combination is pointed at which action.
//!
//! This is the half of "conflict detection" that needs no operating system:
//! two Clipped actions bound to one combination is a mistake nobody should have
//! to discover from Windows, and it is caught here, before anything is
//! registered. The other half — a combination another *application* already
//! owns — can only be discovered by asking Windows, and lives in
//! [`crate::HotkeyService`].

use core::fmt;
use std::collections::BTreeMap;

use crate::action::HotkeyAction;
use crate::hotkey::Hotkey;

/// The combination bound to each action, and nothing else.
///
/// Deliberately a plain value with no threads and no Windows in it: the
/// configuration UI ([issue
/// #54](https://github.com/wildware-uk/clipped/issues/54)) can build one,
/// validate it and show conflicts without a service running.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Bindings {
    entries: BTreeMap<HotkeyAction, Hotkey>,
}

impl Bindings {
    /// No action bound to anything.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// The defaults from SPEC.md sections 7 and 25: save replay on `Ctrl+F10`
    /// (section 7, "Manual / Replay Buffer") and bookmark on `Ctrl+F9`
    /// (section 25, "Manual Bookmarks").
    ///
    /// The other five actions start unbound. Binding all seven by default would
    /// mean taking five more combinations away from every other application on
    /// the machine before the user has asked for any of them, and the two that
    /// are bound are the two the specification names.
    ///
    /// # Panics
    ///
    /// Never: both combinations are literals this file controls, and
    /// `the_defaults_are_the_ones_the_specification_names` is what holds them
    /// to what SPEC.md says.
    #[must_use]
    pub fn defaults() -> Self {
        let mut bindings = Self::empty();
        for (action, text) in [
            (HotkeyAction::SaveReplay, "Ctrl+F10"),
            (HotkeyAction::AddBookmark, "Ctrl+F9"),
        ] {
            let hotkey = text.parse().expect("a default this file writes down");
            bindings
                .bind(action, hotkey)
                .expect("the two defaults do not collide");
        }
        bindings
    }

    /// Points `action` at `hotkey`, replacing whatever it was bound to.
    ///
    /// # Errors
    ///
    /// [`BindError::AlreadyBound`] if a *different* action already holds that
    /// combination. Rebinding an action to the combination it already has
    /// succeeds and changes nothing.
    pub fn bind(&mut self, action: HotkeyAction, hotkey: Hotkey) -> Result<(), BindError> {
        if let Some(owner) = self.action_for(hotkey) {
            if owner != action {
                return Err(BindError::AlreadyBound {
                    hotkey,
                    action: owner,
                });
            }
        }
        self.entries.insert(action, hotkey);
        Ok(())
    }

    /// Removes the binding for `action`, returning what it was.
    pub fn unbind(&mut self, action: HotkeyAction) -> Option<Hotkey> {
        self.entries.remove(&action)
    }

    /// What `action` is bound to, if anything.
    #[must_use]
    pub fn binding(&self, action: HotkeyAction) -> Option<Hotkey> {
        self.entries.get(&action).copied()
    }

    /// Which action `hotkey` is bound to, if any.
    #[must_use]
    pub fn action_for(&self, hotkey: Hotkey) -> Option<HotkeyAction> {
        self.entries
            .iter()
            .find(|(_, bound)| **bound == hotkey)
            .map(|(action, _)| *action)
    }

    /// Every binding, in action order.
    pub fn iter(&self) -> impl Iterator<Item = (HotkeyAction, Hotkey)> + '_ {
        self.entries
            .iter()
            .map(|(action, hotkey)| (*action, *hotkey))
    }

    /// How many actions are bound.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is bound at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Why a binding was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindError {
    /// Another Clipped action already holds that combination.
    AlreadyBound {
        /// The combination that is already spoken for.
        hotkey: Hotkey,
        /// The action that holds it.
        action: HotkeyAction,
    },
}

impl fmt::Display for BindError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyBound { hotkey, action } => write!(
                formatter,
                "{hotkey} is already Clipped's shortcut for {action}. \
                 Choose another combination, or clear that one first",
            ),
        }
    }
}

impl core::error::Error for BindError {}

#[cfg(test)]
mod tests {
    use super::{BindError, Bindings};
    use crate::action::HotkeyAction;
    use crate::hotkey::Hotkey;

    fn hotkey(text: &str) -> Hotkey {
        text.parse()
            .unwrap_or_else(|_| panic!("{text} is a hotkey"))
    }

    /// SPEC.md sections 7 and 25 name these two exactly: `Ctrl + F10` under
    /// "Manual / Replay Buffer" and `Ctrl + F9` under "Manual Bookmarks".
    #[test]
    fn the_defaults_are_the_ones_the_specification_names() {
        let defaults = Bindings::defaults();

        assert_eq!(
            defaults.binding(HotkeyAction::SaveReplay),
            Some(hotkey("Ctrl+F10")),
        );
        assert_eq!(
            defaults.binding(HotkeyAction::AddBookmark),
            Some(hotkey("Ctrl+F9")),
        );
        assert_eq!(
            defaults.len(),
            2,
            "nothing else is bound without the user asking",
        );
    }

    #[test]
    fn a_combination_two_actions_want_is_refused_naming_the_action_that_has_it() {
        let mut bindings = Bindings::defaults();

        let error = bindings
            .bind(HotkeyAction::TakeScreenshot, hotkey("Ctrl+F10"))
            .expect_err("Ctrl+F10 already saves a replay");

        assert_eq!(
            error,
            BindError::AlreadyBound {
                hotkey: hotkey("Ctrl+F10"),
                action: HotkeyAction::SaveReplay,
            },
        );
        let message = error.to_string();
        assert!(message.contains("Ctrl+F10"), "{message}");
        assert!(message.contains("Save replay"), "{message}");
        assert_eq!(
            bindings.binding(HotkeyAction::TakeScreenshot),
            None,
            "the refused binding must not have been made anyway",
        );
    }

    #[test]
    fn rebinding_an_action_to_what_it_already_has_is_not_a_conflict_with_itself() {
        let mut bindings = Bindings::defaults();

        assert!(bindings
            .bind(HotkeyAction::SaveReplay, hotkey("Ctrl+F10"))
            .is_ok());
    }

    #[test]
    fn a_combination_is_free_once_the_action_holding_it_gives_it_up() {
        let mut bindings = Bindings::defaults();

        assert_eq!(
            bindings.unbind(HotkeyAction::SaveReplay),
            Some(hotkey("Ctrl+F10")),
        );
        assert!(bindings
            .bind(HotkeyAction::TakeScreenshot, hotkey("Ctrl+F10"))
            .is_ok());
        assert_eq!(
            bindings.action_for(hotkey("Ctrl+F10")),
            Some(HotkeyAction::TakeScreenshot),
        );
    }
}
