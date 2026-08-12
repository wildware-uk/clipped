//! Why a highlight rule refused a value.
//!
//! The same discipline as `crate::config::error`: every message names the
//! setting, the event kind it belongs to, the value that was offered and what
//! would have been accepted, because "invalid rule" is not something a user can
//! act on (AGENTS.md sections 15 and 45).
//!
//! It is a type of its own rather than a `crate::config::SettingError` because
//! that error names a [`SettingKey`](crate::config::SettingKey), and a
//! highlight rule's settings are per event kind — `kill`'s lead is a different
//! value from `death`'s, and a key enumeration with no room for the kind could
//! only say which of the two was wrong by putting it in the prose.

use core::fmt;

use clipped_events::EventKind;

/// One of the values a rule set is made of.
///
/// It exists as an enumeration for the two callers `crate::config::SettingKey`
/// exists for: an error names a setting without a hand-written string, and a
/// settings screen can list what there is to render without a second list that
/// goes stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuleSetting {
    /// Whether events of a kind are worth a highlight at all.
    Enabled,
    /// How much of the recording before the event to keep.
    Lead,
    /// How much after it to keep.
    Trail,
    /// How sure the source has to be before the event counts.
    MinimumConfidence,
    /// How far apart two windows may be and still become one highlight.
    MergeGap,
    /// The longest a highlight merging may build across a gap.
    MaximumLength,
}

impl RuleSetting {
    /// Every setting, in the order a settings screen should list them.
    pub const ALL: [Self; 6] = [
        Self::Enabled,
        Self::Lead,
        Self::Trail,
        Self::MinimumConfidence,
        Self::MergeGap,
        Self::MaximumLength,
    ];

    /// The settings that belong to one event kind rather than to the set.
    pub const PER_KIND: [Self; 4] = [
        Self::Enabled,
        Self::Lead,
        Self::Trail,
        Self::MinimumConfidence,
    ];

    /// The setting's key in the settings file, and its name in a diagnostic.
    ///
    /// These are the file format. Changing one is a change to a user's file and
    /// needs a migration (AGENTS.md section 43).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Lead => "lead_seconds",
            Self::Trail => "trail_seconds",
            Self::MinimumConfidence => "minimum_confidence",
            Self::MergeGap => "merge_gap_seconds",
            Self::MaximumLength => "maximum_length_seconds",
        }
    }

    /// The setting's name in the words a person reads (AGENTS.md section 28).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Enabled => "Clip this event",
            Self::Lead => "Keep before",
            Self::Trail => "Keep after",
            Self::MinimumConfidence => "Minimum certainty",
            Self::MergeGap => "Join clips closer than",
            Self::MaximumLength => "Longest clip",
        }
    }

    /// The setting a file key names, if it names one.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|setting| setting.name() == name)
    }
}

impl fmt::Display for RuleSetting {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// One value a highlight rule cannot take.
#[derive(Debug, Clone, PartialEq)]
pub enum HighlightRuleError {
    /// A value of the right kind, outside the range the setting allows.
    OutOfRange {
        /// Which setting.
        setting: RuleSetting,
        /// Which event kind's, when the setting belongs to one.
        kind: Option<EventKind>,
        /// What was offered, already phrased for a sentence.
        value: String,
        /// What would have been accepted.
        accepted: String,
    },
    /// A value of the wrong JSON type — a string where a number belongs.
    WrongType {
        /// Which setting.
        setting: RuleSetting,
        /// Which event kind's, when the setting belongs to one.
        kind: Option<EventKind>,
        /// What the setting is written as.
        expected: &'static str,
        /// What was found instead.
        found: &'static str,
    },
    /// The section itself is not the shape a rule set is written in.
    Malformed {
        /// What was wrong, phrased for a sentence.
        detail: String,
    },
}

impl HighlightRuleError {
    /// Which setting the problem is about, when it is about one.
    #[must_use]
    pub const fn setting(&self) -> Option<RuleSetting> {
        match self {
            Self::OutOfRange { setting, .. } | Self::WrongType { setting, .. } => Some(*setting),
            Self::Malformed { .. } => None,
        }
    }

    /// Which event kind's rule the problem is in, when it is in one.
    #[must_use]
    pub const fn kind(&self) -> Option<&EventKind> {
        match self {
            Self::OutOfRange { kind, .. } | Self::WrongType { kind, .. } => kind.as_ref(),
            Self::Malformed { .. } => None,
        }
    }
}

impl fmt::Display for HighlightRuleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfRange {
                setting,
                kind,
                value,
                accepted,
            } => write!(
                formatter,
                "{} is {value}; it accepts {accepted}",
                subject(*setting, kind.as_ref())
            ),
            Self::WrongType {
                setting,
                kind,
                expected,
                found,
            } => write!(
                formatter,
                "{} is written as {expected} and this one is {found}",
                subject(*setting, kind.as_ref())
            ),
            Self::Malformed { detail } => write!(formatter, "{detail} in the highlight rules"),
        }
    }
}

impl core::error::Error for HighlightRuleError {}

/// The same refusal, saying which event kind's rule it is about.
///
/// A rule is built before it is filed under a kind — `HighlightRule`'s builders
/// do not know which one they are for — so the kind is filled in by whoever
/// does know, rather than being threaded through every setter.
pub(super) fn with_kind(error: HighlightRuleError, of: &EventKind) -> HighlightRuleError {
    match error {
        HighlightRuleError::OutOfRange {
            setting,
            kind: None,
            value,
            accepted,
        } => HighlightRuleError::OutOfRange {
            setting,
            kind: Some(of.clone()),
            value,
            accepted,
        },
        HighlightRuleError::WrongType {
            setting,
            kind: None,
            expected,
            found,
        } => HighlightRuleError::WrongType {
            setting,
            kind: Some(of.clone()),
            expected,
            found,
        },
        already_named => already_named,
    }
}

/// `kill's lead_seconds`, or `merge_gap_seconds` for a setting of the whole
/// set.
fn subject(setting: RuleSetting, kind: Option<&EventKind>) -> String {
    match kind {
        Some(kind) => format!("{kind}'s {setting}"),
        None => setting.name().to_owned(),
    }
}
