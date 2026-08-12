//! The rules as they are written down, and what survives being read by the
//! wrong build.
//!
//! # The shape
//!
//! One section of the settings file `crate::config::document` owns:
//!
//! ```json
//! {
//!   "highlights": {
//!     "merge_gap_seconds": 5,
//!     "maximum_length_seconds": 120,
//!     "events": {
//!       "kill": { "lead_seconds": 20 },
//!       "death": { "enabled": false },
//!       "acme-cs2.flag_captured": { "enabled": true, "lead_seconds": 8 }
//!     }
//!   }
//! }
//! ```
//!
//! A layer says only what it changes. `kill` above keeps its shipped ten
//! seconds of trail and its shipped certainty, and follows them if a later
//! version of Clipped changes either.
//!
//! # Durations are whole seconds
//!
//! For the reason `replay_window_seconds` is: the file holds seconds, so a
//! fractional value would come back as something other than what was set, and a
//! setting that does not survive a save is worse than one that is refused. The
//! window a rule *produces* is not restricted this way — it is widened by the
//! event's own timing precision, which is a measurement rather than a setting.
//!
//! # Compatibility
//!
//! The three ways a file outlives the build that wrote it (AGENTS.md sections
//! 43 and 56), and how each is handled here:
//!
//! - **A key this build has never heard of** — a setting a later Clipped added
//!   — is kept, at either level, and written back out unchanged.
//! - **An event kind this build has never heard of** needs no special handling
//!   at all, and that is the point of `clipped_events::EventKind` being open:
//!   `objective_taken` from a newer build reads as
//!   [`Unrecognised`](clipped_events::EventKind::Unrecognised) and a plugin's
//!   `acme-cs2.flag_captured` as [`Custom`](clipped_events::EventKind::Custom),
//!   both keyed by the string they arrived as. A rule written for a kind this
//!   build cannot name still applies to events of that kind, because the match
//!   is on the same string the event carries.
//! - **A whole section this build has never heard of** is the case that needed
//!   no code: a build without highlight rules keeps `highlights` among the
//!   top-level keys it does not recognise and writes it back
//!   (`crate::config::Configuration::unrecognised_keys`).
//!
//! # The migration path
//!
//! There is nothing to migrate *from*: no build has ever written this section,
//! so the only older shape is its absence, and absence means every rule is
//! inherited — which is exactly what an unconfigured user gets. Reading a
//! settings file that predates the section therefore produces the shipped
//! defaults without touching the file, and a build that reads one and saves it
//! adds the section rather than rewriting anything.
//!
//! That the section carries no version of its own is deliberate: the settings
//! file has one version, `crate::config::SCHEMA_VERSION`, and a second version
//! number inside one of its sections would be a second answer to "how old is
//! this file" — the migration step that renames a key here belongs in
//! `crate::config::document`'s ladder with the rest of them.

use core::time::Duration;

use clipped_events::{Confidence, EventKind};
use serde_json::{Map, Value};

use super::error::{with_kind, HighlightRuleError, RuleSetting};
use super::rule::HighlightRule;
use super::rules::HighlightRules;

/// The key the per-kind rules are written under, inside the section.
const EVENTS: &str = "events";

impl HighlightRules {
    /// Reads one layer's rules from its section of the settings file.
    ///
    /// A key present with a `null` value means the same as an absent key: this
    /// layer says nothing. That is what a settings screen writes when the user
    /// presses Reset, and reading it as "set to nothing" would make Reset
    /// unrepresentable.
    ///
    /// # Errors
    ///
    /// [`HighlightRuleError`], naming the setting, the event kind it belongs to
    /// and what would have been accepted. Nothing is repaired and nothing is
    /// dropped: a rule this build cannot read is a rule the user wrote, and the
    /// caller's job is to refuse the file and leave it alone
    /// (`crate::config::ConfigurationError::WouldOverwrite`).
    pub fn read(section: &Map<String, Value>) -> Result<Self, HighlightRuleError> {
        let mut rules = Self::none();

        for (key, value) in section {
            if key == EVENTS {
                continue;
            }
            match RuleSetting::from_name(key) {
                Some(RuleSetting::MergeGap) if !value.is_null() => {
                    rules.set_merge_gap(Some(read_seconds(RuleSetting::MergeGap, value)?))?;
                }
                Some(RuleSetting::MaximumLength) if !value.is_null() => {
                    rules.set_maximum_length(Some(read_seconds(
                        RuleSetting::MaximumLength,
                        value,
                    )?))?;
                }
                // Anything else at this level is either a reset, or a per-kind
                // setting written where the set's settings go — which this
                // build has no meaning for. Both are kept rather than guessed
                // at.
                Some(RuleSetting::MergeGap | RuleSetting::MaximumLength) => {}
                Some(_) | None => rules.keep_unrecognised(key.clone(), value.clone()),
            }
        }

        match section.get(EVENTS) {
            None | Some(Value::Null) => {}
            Some(Value::Object(events)) => {
                for (name, value) in events {
                    let kind = EventKind::from(name.clone());
                    let Value::Object(object) = value else {
                        return Err(HighlightRuleError::Malformed {
                            detail: format!(
                                "{kind}'s rule is an object and this is {}",
                                kind_of(value)
                            ),
                        });
                    };
                    let rule = read_rule(&kind, object)?;
                    rules.set_rule(kind, Some(rule))?;
                }
            }
            Some(other) => {
                return Err(HighlightRuleError::Malformed {
                    detail: format!("\"{EVENTS}\" is an object and this is {}", kind_of(other)),
                })
            }
        }

        Ok(rules)
    }

    /// Writes one layer's rules as its section of the settings file.
    ///
    /// Only what the layer says. A rule that sets nothing is still written —
    /// `"kill": {}` — because a kind the user opened and reset every field on
    /// is one they may well come back to, and the file recording that is a few
    /// bytes.
    #[must_use]
    pub fn write(&self) -> Map<String, Value> {
        let mut section = Map::new();
        if let Some(gap) = self.merge_gap() {
            section.insert(
                RuleSetting::MergeGap.name().to_owned(),
                Value::from(gap.as_secs()),
            );
        }
        if let Some(length) = self.maximum_length() {
            section.insert(
                RuleSetting::MaximumLength.name().to_owned(),
                Value::from(length.as_secs()),
            );
        }

        let mut events = Map::new();
        for (kind, rule) in self.iter() {
            events.insert(kind.as_str().to_owned(), Value::Object(write_rule(rule)));
        }
        section.insert(EVENTS.to_owned(), Value::Object(events));

        for (key, value) in self.unrecognised() {
            section.insert(key.clone(), value.clone());
        }
        section
    }
}

fn read_rule(
    kind: &EventKind,
    object: &Map<String, Value>,
) -> Result<HighlightRule, HighlightRuleError> {
    let mut rule = HighlightRule::unset();
    for (key, value) in object {
        let Some(setting) = RuleSetting::from_name(key) else {
            rule.keep_unrecognised(key.clone(), value.clone());
            continue;
        };
        if value.is_null() {
            continue;
        }
        rule = read_setting(rule, setting, value).map_err(|error| with_kind(error, kind))?;
    }
    Ok(rule)
}

fn read_setting(
    rule: HighlightRule,
    setting: RuleSetting,
    value: &Value,
) -> Result<HighlightRule, HighlightRuleError> {
    match setting {
        RuleSetting::Enabled => {
            let Some(enabled) = value.as_bool() else {
                return Err(HighlightRuleError::WrongType {
                    setting,
                    kind: None,
                    expected: "true or false",
                    found: kind_of(value),
                });
            };
            Ok(rule.with_enabled(Some(enabled)))
        }
        RuleSetting::Lead => Ok(rule.with_lead(Some(read_seconds(setting, value)?))),
        RuleSetting::Trail => Ok(rule.with_trail(Some(read_seconds(setting, value)?))),
        RuleSetting::MinimumConfidence => {
            let Some(number) = value.as_f64() else {
                return Err(HighlightRuleError::WrongType {
                    setting,
                    kind: None,
                    expected: "a certainty between 0 and 1",
                    found: kind_of(value),
                });
            };
            // Narrowing to the width a `Confidence` holds. A certainty is one
            // significant figure of somebody's estimate; the digits lost here
            // are digits the source never had.
            let confidence =
                Confidence::new(number as f32).map_err(|_| HighlightRuleError::OutOfRange {
                    setting,
                    kind: None,
                    value: number.to_string(),
                    accepted: "a certainty between 0 and 1".to_owned(),
                })?;
            Ok(rule.with_minimum_confidence(Some(confidence)))
        }
        // Neither belongs to a kind. They are written where the set's settings
        // go, so one found here is a key this build has no meaning for; it is
        // kept rather than applied to every kind on a guess.
        RuleSetting::MergeGap | RuleSetting::MaximumLength => {
            let mut rule = rule;
            rule.keep_unrecognised(setting.name().to_owned(), value.clone());
            Ok(rule)
        }
    }
}

fn read_seconds(setting: RuleSetting, value: &Value) -> Result<Duration, HighlightRuleError> {
    value
        .as_u64()
        .map(Duration::from_secs)
        .ok_or(HighlightRuleError::WrongType {
            setting,
            kind: None,
            expected: "a whole number of seconds",
            found: kind_of(value),
        })
}

fn write_rule(rule: &HighlightRule) -> Map<String, Value> {
    let mut object = Map::new();
    if let Some(enabled) = rule.enabled() {
        object.insert(RuleSetting::Enabled.name().to_owned(), Value::from(enabled));
    }
    if let Some(lead) = rule.lead() {
        object.insert(
            RuleSetting::Lead.name().to_owned(),
            Value::from(lead.as_secs()),
        );
    }
    if let Some(trail) = rule.trail() {
        object.insert(
            RuleSetting::Trail.name().to_owned(),
            Value::from(trail.as_secs()),
        );
    }
    if let Some(minimum) = rule.minimum_confidence() {
        object.insert(
            RuleSetting::MinimumConfidence.name().to_owned(),
            Value::from(minimum.as_f32()),
        );
    }
    for (key, value) in rule.unrecognised() {
        object.insert(key.clone(), value.clone());
    }
    object
}

/// What a JSON value is, in the words an error message uses.
fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "nothing",
        Value::Bool(_) => "true or false",
        Value::Number(_) => "a number",
        Value::String(_) => "text",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}
