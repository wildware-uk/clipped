//! One layer of highlight rules: the global set, or one game's.
//!
//! The same type is used for both, so per-game inheritance is a fold over a
//! list of layers rather than a special case per setting — exactly as
//! `crate::config::Preferences` is used for both the global settings and each
//! game's.

use core::time::Duration;
use std::collections::BTreeMap;

use clipped_events::EventKind;
use serde_json::Value;

use super::error::{HighlightRuleError, RuleSetting};
use super::resolve::ResolvedHighlightRules;
use super::rule::{
    check_duration, HighlightRule, LONGEST_MAXIMUM_LENGTH, MAXIMUM_MERGE_GAP,
    SHORTEST_MAXIMUM_LENGTH,
};
use crate::config::Scope;

/// What one layer says about which moments are worth a highlight.
///
/// A layer that says nothing about a kind, or about one field of a kind's rule,
/// passes the layer below it through. That is the whole of what "per-game rules
/// inherit from the global ones" means, and it is why every field is optional:
/// a per-game layer holding the *effective* window could not tell "inherited
/// fifteen seconds" from "set to fifteen seconds", and the first change to the
/// global setting would silently stop propagating (AGENTS.md section 30).
///
/// Valid by construction, as `crate::config::Preferences` is: every way of
/// putting a value in validates it, so a layer that exists is one whose values
/// are in range and whose durations survive a round trip through the settings
/// file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HighlightRules {
    kinds: BTreeMap<EventKind, HighlightRule>,
    merge_gap: Option<Duration>,
    maximum_length: Option<Duration>,
    /// Keys this build does not recognise, kept exactly as they were read.
    unknown: BTreeMap<String, Value>,
}

impl HighlightRules {
    /// A layer that says nothing: every rule inherited.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Whether this layer says nothing at all, unrecognised keys included.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.kinds.values().all(HighlightRule::is_empty)
            && self.merge_gap.is_none()
            && self.maximum_length.is_none()
            && self.unknown.is_empty()
    }

    /// What this layer says about `kind`, if it says anything.
    #[must_use]
    pub fn rule(&self, kind: &EventKind) -> Option<&HighlightRule> {
        self.kinds.get(kind)
    }

    /// Sets, or with `None` clears, what this layer says about `kind`.
    ///
    /// # Errors
    ///
    /// Whatever [`HighlightRule`]'s own builders would refuse, with the kind
    /// filled in so that the message says `kill's lead_seconds`. A rule built
    /// through those builders cannot fail here; this is what keeps the
    /// invariant true whichever way the value arrived, including from the
    /// settings file.
    pub fn set_rule(
        &mut self,
        kind: EventKind,
        rule: Option<HighlightRule>,
    ) -> Result<(), HighlightRuleError> {
        match rule {
            Some(rule) => {
                rule.check(&kind)?;
                self.kinds.insert(kind, rule);
            }
            None => {
                self.kinds.remove(&kind);
            }
        }
        Ok(())
    }

    /// Every kind this layer says something about, in the vocabulary's own
    /// order — which is a stable one, so that a settings file does not
    /// reorder itself on every save.
    pub fn iter(&self) -> impl Iterator<Item = (&EventKind, &HighlightRule)> {
        self.kinds.iter()
    }

    /// How close two windows have to be to become one highlight, if this layer
    /// says.
    #[must_use]
    pub const fn merge_gap(&self) -> Option<Duration> {
        self.merge_gap
    }

    /// Sets, or with `None` clears, the merge gap.
    ///
    /// # Errors
    ///
    /// [`HighlightRuleError::OutOfRange`] above [`MAXIMUM_MERGE_GAP`], or for a
    /// duration that is not a whole number of seconds. Zero is accepted and
    /// means what it says: merge only what actually overlaps.
    pub fn set_merge_gap(&mut self, gap: Option<Duration>) -> Result<(), HighlightRuleError> {
        if let Some(gap) = gap {
            check_duration(
                RuleSetting::MergeGap,
                gap,
                Duration::ZERO,
                MAXIMUM_MERGE_GAP,
            )?;
        }
        self.merge_gap = gap;
        Ok(())
    }

    /// The longest a highlight may become by merging across a gap, if this
    /// layer says.
    #[must_use]
    pub const fn maximum_length(&self) -> Option<Duration> {
        self.maximum_length
    }

    /// Sets, or with `None` clears, the maximum length.
    ///
    /// # Errors
    ///
    /// [`HighlightRuleError::OutOfRange`] outside
    /// [`SHORTEST_MAXIMUM_LENGTH`]..=[`LONGEST_MAXIMUM_LENGTH`], or for a
    /// duration that is not a whole number of seconds.
    pub fn set_maximum_length(
        &mut self,
        length: Option<Duration>,
    ) -> Result<(), HighlightRuleError> {
        if let Some(length) = length {
            check_duration(
                RuleSetting::MaximumLength,
                length,
                SHORTEST_MAXIMUM_LENGTH,
                LONGEST_MAXIMUM_LENGTH,
            )?;
        }
        self.maximum_length = length;
        Ok(())
    }

    /// The keys from a newer build that were read and are being kept.
    pub fn unrecognised_keys(&self) -> impl Iterator<Item = &str> {
        self.unknown.keys().map(String::as_str)
    }

    /// Records a key this build does not understand (AGENTS.md section 56).
    pub(super) fn keep_unrecognised(&mut self, key: String, value: Value) {
        self.unknown.insert(key, value);
    }

    /// The kept keys, for writing back out.
    pub(super) const fn unrecognised(&self) -> &BTreeMap<String, Value> {
        &self.unknown
    }

    /// Folds the shipped defaults, the global layer and — for a game scope —
    /// that game's layer, in that order.
    ///
    /// This is the single resolution point for highlight rules, and it is the
    /// same three layers, in the same order, with the same meaning of "says
    /// nothing" as `crate::config`'s. Every consumer reaches a rule through
    /// here, so that "does this game keep longer clips of a kill" has one
    /// answer rather than one per call site (AGENTS.md section 30).
    ///
    /// `game` is ignored for [`Scope::Global`]: resolving the global page must
    /// never show a value a game set, or the user would edit the global rules
    /// and watch a game's number change under their hands.
    #[must_use]
    pub fn resolve(scope: Scope, global: &Self, game: Option<&Self>) -> ResolvedHighlightRules {
        ResolvedHighlightRules::fold(scope, global, game)
    }
}
