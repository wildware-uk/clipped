//! Which combination is pointed at which action, as configuration.
//!
//! # Why hotkeys are global and not per-game
//!
//! Because a per-game hotkey could not be honoured. `clipped_hotkeys` registers
//! a combination with Windows once, for the process, and Windows hands the
//! press to whoever registered it; there is no per-foreground-application
//! variant of that registration. A per-game hotkey override would therefore be
//! a control that silently does nothing (AGENTS.md section 27), so it is not
//! offered. SPEC.md section 31's list of per-game overrides does not include
//! hotkeys either.
//!
//! # Three states, not two
//!
//! [`clipped_hotkeys::Bindings`] answers "what is this action bound to" with
//! `Option<Hotkey>`, which is the right answer for the service. Configuration
//! needs one more state, for the same reason per-game settings do: *unset* has
//! to be distinguishable from *deliberately unbound*, so that a user who has
//! never touched Save Replay keeps following the default when the default
//! changes, and a user who cleared it stays cleared.
//!
//! So the three states are absent (follow the default), [`HotkeyOverride::Bound`]
//! and [`HotkeyOverride::Unbound`], and [`ResolvedHotkeys`] folds them onto
//! [`Bindings::defaults`] to produce the `Bindings` the service is given.

use std::collections::BTreeMap;

use clipped_hotkeys::{Bindings, Hotkey, HotkeyAction, ACTIONS};

use crate::config::error::SettingError;
use crate::config::value::{Resolved, SettingSource};

/// What the settings say about one action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyOverride {
    /// Bound to this combination.
    Bound(Hotkey),
    /// Deliberately bound to nothing, even if the default binds it.
    Unbound,
}

impl HotkeyOverride {
    /// The combination, if there is one.
    #[must_use]
    pub const fn hotkey(self) -> Option<Hotkey> {
        match self {
            Self::Bound(hotkey) => Some(hotkey),
            Self::Unbound => None,
        }
    }
}

/// The hotkey layer of the configuration: what the user has changed, and
/// nothing else.
///
/// Validated on every change, so a `HotkeyOverrides` that exists is one whose
/// resolution has no two actions on one combination.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HotkeyOverrides {
    entries: BTreeMap<HotkeyAction, HotkeyOverride>,
    unknown: BTreeMap<String, serde_json::Value>,
}

impl HotkeyOverrides {
    /// Nothing changed from the defaults.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// What the settings say about `action`, if anything.
    #[must_use]
    pub fn get(&self, action: HotkeyAction) -> Option<HotkeyOverride> {
        self.entries.get(&action).copied()
    }

    /// Sets what the settings say about `action`, or with `None` clears it so
    /// that the action follows the default again.
    ///
    /// # Errors
    ///
    /// [`SettingError::HotkeyConflict`] when the resulting set would point one
    /// combination at two actions — including at an action that is only bound
    /// by default, because that is the combination the user would find had
    /// stopped working.
    pub fn set(
        &mut self,
        action: HotkeyAction,
        binding: Option<HotkeyOverride>,
    ) -> Result<(), SettingError> {
        let mut candidate = self.clone();
        match binding {
            Some(binding) => candidate.entries.insert(action, binding),
            None => candidate.entries.remove(&action),
        };
        // Resolution is what a conflict is about: two *effective* bindings on
        // one combination. Checking the overrides alone would miss a user who
        // binds Take Screenshot to the combination Save Replay holds by
        // default.
        candidate.resolve()?;
        *self = candidate;
        Ok(())
    }

    /// Every action the settings say something about.
    pub fn iter(&self) -> impl Iterator<Item = (HotkeyAction, HotkeyOverride)> + '_ {
        self.entries
            .iter()
            .map(|(action, binding)| (*action, *binding))
    }

    /// Whether the settings say nothing about hotkeys at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.unknown.is_empty()
    }

    /// The effective binding of every action, and where each came from.
    ///
    /// # Errors
    ///
    /// [`SettingError::HotkeyConflict`] if the overrides and the defaults
    /// together point one combination at two actions. [`Self::set`] refuses to
    /// produce such a set, so this only fails for a set built by the file
    /// reader, which is exactly where it has to fail.
    pub fn resolve(&self) -> Result<ResolvedHotkeys, SettingError> {
        let defaults = Bindings::defaults();
        let mut bindings = Bindings::empty();
        let mut entries = BTreeMap::new();

        for action in ACTIONS {
            let (binding, source) = match self.entries.get(&action) {
                Some(override_) => (override_.hotkey(), SettingSource::Global),
                None => (defaults.binding(action), SettingSource::Default),
            };
            if let Some(hotkey) = binding {
                bindings.bind(action, hotkey).map_err(|error| match error {
                    clipped_hotkeys::BindError::AlreadyBound {
                        hotkey,
                        action: held_by,
                    } => SettingError::HotkeyConflict {
                        hotkey,
                        action,
                        held_by,
                    },
                })?;
            }
            entries.insert(
                action,
                Resolved::new(binding, source, SettingSource::Global),
            );
        }

        Ok(ResolvedHotkeys { entries, bindings })
    }

    /// The keys from a newer build that were read and are being kept.
    pub fn unrecognised_keys(&self) -> impl Iterator<Item = &str> {
        self.unknown.keys().map(String::as_str)
    }

    /// Records a key this build does not understand (AGENTS.md section 56).
    pub(crate) fn keep_unrecognised(&mut self, key: String, value: serde_json::Value) {
        self.unknown.insert(key, value);
    }

    /// The kept keys, for writing back out.
    pub(crate) const fn unrecognised(&self) -> &BTreeMap<String, serde_json::Value> {
        &self.unknown
    }

    /// Sets a binding without validating, for the file reader, which validates
    /// the whole set once it has read all of them.
    pub(crate) fn insert_unchecked(&mut self, action: HotkeyAction, binding: HotkeyOverride) {
        self.entries.insert(action, binding);
    }

    /// Takes on the unrecognised keys of the layer this one is replacing, as
    /// [`super::Preferences::adopt_unrecognised_from`].
    pub(crate) fn adopt_unrecognised_from(&mut self, previous: &Self) {
        for (key, value) in &previous.unknown {
            self.unknown
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }
    }
}

/// What every hotkey resolves to, and whether the user set it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedHotkeys {
    entries: BTreeMap<HotkeyAction, Resolved<Option<Hotkey>>>,
    bindings: Bindings,
}

impl ResolvedHotkeys {
    /// What `action` is bound to, and whether that came from the settings or
    /// from the default.
    ///
    /// # Panics
    ///
    /// Never: [`HotkeyOverrides::resolve`] fills every action in
    /// [`clipped_hotkeys::ACTIONS`], and `every_action_has_an_answer` is what
    /// holds that true when a variant is added.
    #[must_use]
    pub fn binding(&self, action: HotkeyAction) -> &Resolved<Option<Hotkey>> {
        self.entries
            .get(&action)
            .expect("resolution fills every action")
    }

    /// Every action, in the order a settings screen should list them.
    pub fn iter(&self) -> impl Iterator<Item = (HotkeyAction, &Resolved<Option<Hotkey>>)> + '_ {
        ACTIONS
            .into_iter()
            .map(|action| (action, self.binding(action)))
    }

    /// The bindings to hand to `clipped_hotkeys::HotkeyService`.
    #[must_use]
    pub const fn bindings(&self) -> &Bindings {
        &self.bindings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hotkey(text: &str) -> Hotkey {
        text.parse().expect("a combination this test writes down")
    }

    #[test]
    fn an_action_nobody_touched_follows_the_default() {
        let resolved = HotkeyOverrides::none()
            .resolve()
            .expect("the defaults do not conflict");
        let save = resolved.binding(HotkeyAction::SaveReplay);
        assert_eq!(*save.value(), Some(hotkey("Ctrl+F10")));
        assert_eq!(save.source(), SettingSource::Default);
        assert!(!save.is_overridden());
    }

    #[test]
    fn setting_an_action_to_the_value_it_already_had_is_still_an_override() {
        // The hotkey half of the distinction the whole ticket is about: a user
        // who pins Save Replay to Ctrl+F10 keeps Ctrl+F10 when the default
        // moves, and a user who never touched it follows.
        let mut overrides = HotkeyOverrides::none();
        overrides
            .set(
                HotkeyAction::SaveReplay,
                Some(HotkeyOverride::Bound(hotkey("Ctrl+F10"))),
            )
            .expect("binding an action to what it already had is not a conflict");

        let resolved = overrides.resolve().expect("no conflict");
        let save = resolved.binding(HotkeyAction::SaveReplay);
        assert_eq!(*save.value(), Some(hotkey("Ctrl+F10")));
        assert_eq!(save.source(), SettingSource::Global);
        assert!(save.is_overridden());
    }

    #[test]
    fn an_action_can_be_deliberately_unbound_and_that_is_not_the_same_as_unset() {
        let mut overrides = HotkeyOverrides::none();
        overrides
            .set(HotkeyAction::SaveReplay, Some(HotkeyOverride::Unbound))
            .expect("clearing a binding is not a conflict");

        let resolved = overrides.resolve().expect("no conflict");
        let save = resolved.binding(HotkeyAction::SaveReplay);
        assert_eq!(*save.value(), None);
        assert_eq!(save.source(), SettingSource::Global);
        assert!(save.is_overridden());
        assert_eq!(resolved.bindings().binding(HotkeyAction::SaveReplay), None);
    }

    #[test]
    fn taking_a_combination_a_default_holds_is_refused_and_changes_nothing() {
        // The failure this prevents: Take Screenshot silently stealing
        // Ctrl+F10, and Save Replay stopping working with no message.
        let mut overrides = HotkeyOverrides::none();
        let error = overrides
            .set(
                HotkeyAction::TakeScreenshot,
                Some(HotkeyOverride::Bound(hotkey("Ctrl+F10"))),
            )
            .expect_err("Ctrl+F10 is Save Replay's by default");

        let message = error.to_string();
        assert!(
            message.contains("Ctrl+F10")
                && message.contains("take_screenshot")
                && message.contains("save_replay"),
            "the refusal must name the combination and both actions: {message}"
        );
        assert!(
            overrides.is_empty(),
            "a refused change must leave the previous settings standing"
        );
    }

    #[test]
    fn a_combination_is_free_once_the_action_holding_it_lets_go() {
        let mut overrides = HotkeyOverrides::none();
        overrides
            .set(HotkeyAction::SaveReplay, Some(HotkeyOverride::Unbound))
            .expect("unbinding is allowed");
        overrides
            .set(
                HotkeyAction::TakeScreenshot,
                Some(HotkeyOverride::Bound(hotkey("Ctrl+F10"))),
            )
            .expect("Ctrl+F10 is nobody's now");

        let resolved = overrides.resolve().expect("no conflict");
        assert_eq!(
            resolved.bindings().action_for(hotkey("Ctrl+F10")),
            Some(HotkeyAction::TakeScreenshot)
        );
    }

    #[test]
    fn every_action_has_an_answer() {
        let resolved = HotkeyOverrides::none().resolve().expect("no conflict");
        for action in ACTIONS {
            // `binding` panics if an action is missing, which is what would
            // happen to the settings screen if a new variant were added and
            // resolution did not fill it.
            let _ = resolved.binding(action);
        }
        assert_eq!(resolved.iter().count(), ACTIONS.len());
    }
}
