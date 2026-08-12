//! What one layer says about one kind of event, and the table Clipped ships.
//!
//! Every field of [`HighlightRule`] is an `Option`, for the reason
//! `crate::config::Preferences`' fields are: *this layer says nothing* has to be
//! distinguishable from *this layer sets the value the layer below already
//! had*, or a game that inherited fifteen seconds of lead would stop following
//! the global setting the moment the user changed it (AGENTS.md section 30).
//!
//! The inheritance is per field and not per rule. A game that wants five more
//! seconds after a kill sets `trail` and nothing else, and keeps following the
//! global lead; a rule that inherited as a whole would make the user restate
//! every field to change one.

use core::time::Duration;
use std::collections::BTreeMap;

use clipped_events::{Confidence, EventKind};
use serde_json::Value;

use super::error::{with_kind, HighlightRuleError, RuleSetting};

/// The most of the recording before an event a rule may keep.
///
/// Five minutes, which is [`DEFAULT_REPLAY_WINDOW`](crate::config::DEFAULT_REPLAY_WINDOW)
/// — the point of the bound is that a lead longer than the buffer is a clip
/// that cannot be produced from a replay buffer at all
/// (`docs/replay-buffer.md`), and a setting that asks for footage nobody has is
/// a control that silently does nothing (AGENTS.md section 27). A recording of
/// the whole session can serve a longer lead, but a rule set is shared between
/// both and the smaller of the two limits is the honest one.
pub const MAXIMUM_LEAD: Duration = Duration::from_secs(5 * 60);

/// The most of the recording after an event a rule may keep.
///
/// The same five minutes. A trail is bounded for a different reason from a
/// lead — the footage always exists, because it has not happened yet — but a
/// highlight that runs for a quarter of an hour is not a highlight, and the
/// symmetry is one fewer number for a user to remember.
pub const MAXIMUM_TRAIL: Duration = Duration::from_secs(5 * 60);

/// The most a merge gap may be.
///
/// Two minutes. A gap this wide already joins two firefights either side of a
/// lull into one clip; wider than that and the setting stops meaning "the same
/// moment" and starts meaning "the same match", which is what
/// [`MAXIMUM_LENGTH`](RuleSetting::MaximumLength) bounds instead.
pub const MAXIMUM_MERGE_GAP: Duration = Duration::from_secs(2 * 60);

/// The shortest a maximum highlight length may be set to.
///
/// Ten seconds, which is under the trail of every rule Clipped ships. Below
/// that the ceiling would be shorter than a single event's own window, and
/// since a window is never truncated (see
/// [`ResolvedHighlightRules`](super::ResolvedHighlightRules)) the setting would
/// have no effect a user could observe.
pub const SHORTEST_MAXIMUM_LENGTH: Duration = Duration::from_secs(10);

/// The longest a maximum highlight length may be set to.
pub const LONGEST_MAXIMUM_LENGTH: Duration = Duration::from_secs(30 * 60);

/// How close two windows have to be to become one highlight, when nobody has
/// said.
///
/// Five seconds. Long enough that a kill, the death that answers it and the
/// kill after that are one clip rather than three, and short enough that two
/// separate fights a lull apart stay separate. It is the smaller half of the
/// answer to "a firefight must not produce twenty near-identical clips": the
/// larger half is that overlapping windows merge whatever the gap says.
pub const DEFAULT_MERGE_GAP: Duration = Duration::from_secs(5);

/// The longest a highlight may become by merging across a gap, when nobody has
/// said.
///
/// Two minutes: long enough for a sustained firefight or an objective push, and
/// short enough that a clip is still a clip. It is a bound on what merging
/// *adds*, never a truncation of what a rule asked for — see
/// [`ResolvedHighlightRules`](super::ResolvedHighlightRules).
pub const DEFAULT_MAXIMUM_LENGTH: Duration = Duration::from_secs(2 * 60);

/// How sure a source has to be before an event counts, when nobody has said.
///
/// A half: the source has to think the event more likely to have happened than
/// not. It exists because [`Confidence`] does: an integration reading an
/// authoritative feed reports [`Confidence::CERTAIN`] and sails past this, and
/// a detector watching the screen for a kill feed reports the score it
/// computed. Filtering on that number is the difference between a library of
/// moments and a library of things that might have been moments.
#[must_use]
pub fn default_minimum_confidence() -> Confidence {
    Confidence::new(0.5).expect("a half is within 0..=1")
}

/// What one layer says about events of one kind.
///
/// A rule on its own is *not* checked, and that is deliberate: the builders
/// below take values and the checking happens where the rule is filed under a
/// kind, in [`HighlightRules::set_rule`](super::HighlightRules::set_rule),
/// which is what lets a refusal say `kill's lead_seconds` and what keeps one
/// definition of "acceptable" rather than one per entry point. So a rule that
/// is *in* a [`HighlightRules`](super::HighlightRules) is one whose values are
/// in range and whose durations survive a round trip through the settings
/// file; one held in a local variable on the way there may not be.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HighlightRule {
    enabled: Option<bool>,
    lead: Option<Duration>,
    trail: Option<Duration>,
    minimum_confidence: Option<Confidence>,
    /// Keys this build does not recognise, kept exactly as they were read
    /// (AGENTS.md section 56).
    unknown: BTreeMap<String, Value>,
}

impl HighlightRule {
    /// A rule that says nothing: every field inherited.
    #[must_use]
    pub fn unset() -> Self {
        Self::default()
    }

    /// Whether this rule says nothing at all, unrecognised keys included.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.enabled.is_none()
            && self.lead.is_none()
            && self.trail.is_none()
            && self.minimum_confidence.is_none()
            && self.unknown.is_empty()
    }

    /// Whether this layer says events of this kind are worth a highlight.
    #[must_use]
    pub const fn enabled(&self) -> Option<bool> {
        self.enabled
    }

    /// Sets, or with `None` clears, whether the kind is worth a highlight.
    #[must_use]
    pub const fn with_enabled(mut self, enabled: Option<bool>) -> Self {
        self.enabled = enabled;
        self
    }

    /// How much of the recording before the event this layer keeps.
    #[must_use]
    pub const fn lead(&self) -> Option<Duration> {
        self.lead
    }

    /// Sets, or with `None` clears, the lead.
    ///
    /// Nothing is refused here. A rule is checked where it is filed, by
    /// [`HighlightRules::set_rule`](super::HighlightRules::set_rule), because
    /// that is where the event kind is known and `kill's lead_seconds is 900
    /// seconds` is a message a user can act on where `lead_seconds is 900
    /// seconds` is not. One check point rather than two also means the builder
    /// and the file reader cannot come to disagree about what is acceptable.
    #[must_use]
    pub const fn with_lead(mut self, lead: Option<Duration>) -> Self {
        self.lead = lead;
        self
    }

    /// How much of the recording after the event this layer keeps.
    #[must_use]
    pub const fn trail(&self) -> Option<Duration> {
        self.trail
    }

    /// Sets, or with `None` clears, the trail. Checked as [`Self::with_lead`]
    /// is.
    #[must_use]
    pub const fn with_trail(mut self, trail: Option<Duration>) -> Self {
        self.trail = trail;
        self
    }

    /// How sure the source has to be before an event of this kind counts.
    #[must_use]
    pub const fn minimum_confidence(&self) -> Option<Confidence> {
        self.minimum_confidence
    }

    /// Sets, or with `None` clears, the minimum confidence. Checked as
    /// [`Self::with_lead`] is.
    #[must_use]
    pub const fn with_minimum_confidence(mut self, minimum: Option<Confidence>) -> Self {
        self.minimum_confidence = minimum;
        self
    }

    /// Clears one field, so that this layer inherits it again.
    #[must_use]
    pub fn without(mut self, setting: RuleSetting) -> Self {
        match setting {
            RuleSetting::Enabled => self.enabled = None,
            RuleSetting::Lead => self.lead = None,
            RuleSetting::Trail => self.trail = None,
            RuleSetting::MinimumConfidence => self.minimum_confidence = None,
            // Neither belongs to a kind; a rule holds neither, so clearing one
            // here is not an error and not a change.
            RuleSetting::MergeGap | RuleSetting::MaximumLength => {}
        }
        self
    }

    /// Whether this layer sets `setting` at all.
    ///
    /// The question a Reset control asks, without knowing the type behind each
    /// field. False for the two settings that belong to the set rather than to
    /// a kind.
    #[must_use]
    pub const fn is_set(&self, setting: RuleSetting) -> bool {
        match setting {
            RuleSetting::Enabled => self.enabled.is_some(),
            RuleSetting::Lead => self.lead.is_some(),
            RuleSetting::Trail => self.trail.is_some(),
            RuleSetting::MinimumConfidence => self.minimum_confidence.is_some(),
            RuleSetting::MergeGap | RuleSetting::MaximumLength => false,
        }
    }

    /// The keys from a newer build that were read and are being kept.
    pub fn unrecognised_keys(&self) -> impl Iterator<Item = &str> {
        self.unknown.keys().map(String::as_str)
    }

    /// Records a key this build does not understand.
    pub(super) fn keep_unrecognised(&mut self, key: String, value: Value) {
        self.unknown.insert(key, value);
    }

    /// The kept keys, for writing back out.
    pub(super) const fn unrecognised(&self) -> &BTreeMap<String, Value> {
        &self.unknown
    }

    /// Every check a rule has to pass, named for the kind it is filed under.
    ///
    /// The one place a rule's values are judged, whether they arrived from a
    /// settings screen or from the file (AGENTS.md section 30). It is
    /// `set_rule` that calls it, because the kind is what turns `lead_seconds
    /// is 900 seconds` into something a user with fourteen rules in their file
    /// can act on.
    pub(super) fn check(&self, kind: &EventKind) -> Result<(), HighlightRuleError> {
        let named = |error| with_kind(error, kind);
        if let Some(lead) = self.lead {
            check_duration(RuleSetting::Lead, lead, Duration::ZERO, MAXIMUM_LEAD).map_err(named)?;
        }
        if let Some(trail) = self.trail {
            check_duration(RuleSetting::Trail, trail, Duration::ZERO, MAXIMUM_TRAIL)
                .map_err(named)?;
        }
        if let Some(minimum) = self.minimum_confidence {
            // A `Confidence` read back from a stored event is kept verbatim and
            // may be outside 0..=1, and a threshold no event could ever meet is
            // a rule that switches its kind off while appearing to be on.
            if !minimum.is_usable() {
                return Err(named(HighlightRuleError::OutOfRange {
                    setting: RuleSetting::MinimumConfidence,
                    kind: None,
                    value: minimum.to_string(),
                    accepted: "a certainty between 0 and 1".to_owned(),
                }));
            }
        }
        Ok(())
    }
}

/// What Clipped ships for one kind of event, before any layer says anything.
///
/// Every field is a real value rather than an `Option`, because this is the
/// bottom of the fold: something has to answer, and the alternative to a
/// default is a rule set that only works once a user has configured it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShippedRule {
    /// Whether the kind is worth a highlight without being configured.
    pub enabled: bool,
    /// How much of the recording before the event to keep.
    pub lead: Duration,
    /// How much after it.
    pub trail: Duration,
    /// How sure the source has to be.
    pub minimum_confidence: Confidence,
}

/// The rule Clipped ships for `kind`.
///
/// # What is on by default, and why
///
/// On: the things the player did that they would watch again — a kill, a death,
/// an assist, a score, an objective, an achievement, and winning. Off:
/// everything that is a boundary rather than a moment — a game or match or
/// round starting and ending — because a clip of a lobby is a clip nobody
/// asked for, and every one of them costs the user a row in their library to
/// scroll past.
///
/// A loss is off for the same reason a death is on: a death is fifteen seconds
/// of "how did that happen", and a lost match is a scoreboard.
///
/// [`Custom`](EventKind::Custom) and [`Unrecognised`](EventKind::Unrecognised)
/// are off, and that is the more important half of the table. A plugin can
/// invent a name (`docs/plugin-api.md`), and a plugin that could turn its own
/// invention into clips in a user's library by inventing it would be a plugin
/// deciding what the user's library contains. The window it *would* get is a
/// real one, so switching it on in the settings needs one change and not four.
///
/// The window sizes are SPEC.md section 7's worked example — fifteen seconds
/// before a kill and ten after — and issue #75's for a death, with the rest
/// following the same shape: a moment that is the *result* of what came before
/// gets a long lead, and a moment that is the *start* of something gets a long
/// trail.
#[must_use]
pub fn shipped_rule(kind: &EventKind) -> ShippedRule {
    let (enabled, lead, trail) = match kind {
        // Boundaries rather than moments. Each carries the window it would get
        // if a user switched it on, so that enabling it is one change.
        EventKind::GameStarted | EventKind::MatchStarted | EventKind::RoundStarted => {
            (false, 0, 15)
        }
        EventKind::GameEnded | EventKind::RoundEnded => (false, 15, 5),
        EventKind::MatchEnded | EventKind::Loss => (false, 20, 10),

        // The moments.
        EventKind::Kill => (true, 15, 10),
        EventKind::Death => (true, 10, 5),
        EventKind::Assist => (true, 12, 8),
        EventKind::Win => (true, 20, 10),
        EventKind::Score => (true, 12, 8),
        EventKind::Goal => (true, 15, 10),
        EventKind::Achievement => (true, 10, 10),

        // Somebody else's word for something. See the function documentation.
        EventKind::Custom(_) | EventKind::Unrecognised(_) => (false, 15, 10),
    };

    ShippedRule {
        enabled,
        lead: Duration::from_secs(lead),
        trail: Duration::from_secs(trail),
        minimum_confidence: default_minimum_confidence(),
    }
}

/// Refuses a duration outside the range, or one the settings file cannot hold.
pub(super) fn check_duration(
    setting: RuleSetting,
    value: Duration,
    minimum: Duration,
    maximum: Duration,
) -> Result<(), HighlightRuleError> {
    if !(minimum..=maximum).contains(&value) {
        return Err(HighlightRuleError::OutOfRange {
            setting,
            kind: None,
            value: format!("{} seconds", value.as_secs_f64()),
            accepted: format!("{}-{} seconds", minimum.as_secs(), maximum.as_secs()),
        });
    }
    if value.subsec_nanos() != 0 {
        return Err(HighlightRuleError::OutOfRange {
            setting,
            kind: None,
            value: format!("{} seconds", value.as_secs_f64()),
            accepted: "a whole number of seconds, which is what the settings file holds".to_owned(),
        });
    }
    Ok(())
}
