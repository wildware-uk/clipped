//! What the layers add up to, and what that says about one event.

use core::fmt;
use core::time::Duration;

use clipped_events::{Confidence, EventKind, EventTime, GameEvent};

use super::rule::{shipped_rule, HighlightRule, DEFAULT_MAXIMUM_LENGTH, DEFAULT_MERGE_GAP};
use super::rules::HighlightRules;
use crate::config::{Resolved, Scope, SettingSource};

/// What every field of one kind's rule resolves to, and where each came from.
///
/// The shape a settings screen renders: the value, which layer supplied it, and
/// whether the scope being edited set it — which is what a Reset control is
/// enabled by (`crate::config::Resolved`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedRule {
    enabled: Resolved<bool>,
    lead: Resolved<Duration>,
    trail: Resolved<Duration>,
    minimum_confidence: Resolved<Confidence>,
}

impl ResolvedRule {
    /// Whether events of this kind are worth a highlight.
    #[must_use]
    pub const fn enabled(&self) -> &Resolved<bool> {
        &self.enabled
    }

    /// How much of the recording before the event to keep.
    #[must_use]
    pub const fn lead(&self) -> &Resolved<Duration> {
        &self.lead
    }

    /// How much of the recording after it to keep.
    #[must_use]
    pub const fn trail(&self) -> &Resolved<Duration> {
        &self.trail
    }

    /// How sure the source has to be before the event counts.
    #[must_use]
    pub const fn minimum_confidence(&self) -> &Resolved<Confidence> {
        &self.minimum_confidence
    }
}

/// Why an event will not become a highlight.
///
/// A reason rather than a bare `false`, because every one of these is something
/// a user or a plugin author asks about — "why is nothing being clipped" has
/// four different answers and three different fixes — and because a session
/// that logs the count of each can say which without keeping the events
/// (AGENTS.md section 15).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SkipReason {
    /// The rules say this kind is not worth a highlight.
    Disabled,
    /// The source is less sure than the rule requires.
    Uncertain {
        /// What the source claimed.
        confidence: Confidence,
        /// What the rule requires.
        minimum: Confidence,
    },
    /// The event's confidence is not a number between 0 and 1.
    ///
    /// Only reachable for an event read back from storage: `Confidence::new`
    /// refuses such a value, and `clipped_events` keeps one already in a user's
    /// library verbatim rather than destroying the event over it. There is no
    /// honest comparison to make against it — the source's certainty is not
    /// known — so the event is skipped and said to have been, rather than
    /// having a number invented for it (AGENTS.md section 27).
    ConfidenceUnusable {
        /// What was stored.
        confidence: Confidence,
    },
    /// The rule keeps nothing either side of the event, so there is no clip.
    ///
    /// Reachable only across layers — the global rules set the lead to zero and
    /// a game sets the trail to zero — because neither value is refused on its
    /// own. A window of no length is not a clip, and offering one would be a
    /// highlight that plays nothing.
    EmptyWindow,
}

impl fmt::Display for SkipReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disabled => formatter.write_str("the rules do not clip this kind of event"),
            Self::Uncertain {
                confidence,
                minimum,
            } => write!(
                formatter,
                "the source is {confidence} sure and the rule requires {minimum}"
            ),
            Self::ConfidenceUnusable { confidence } => write!(
                formatter,
                "the stored certainty is {confidence}, which is not between 0 and 1"
            ),
            Self::EmptyWindow => {
                formatter.write_str("the rule keeps nothing either side of the event")
            }
        }
    }
}

/// What the rules say about one event.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Decision {
    /// The event is worth a highlight, of this much recording either side.
    ///
    /// The two durations are the rule's lead and trail **widened by the event's
    /// timing precision**. A source that is polled every two seconds knows the
    /// moment to within a second and says so
    /// (`clipped_events::EventTiming::precision`); a window built from the
    /// nominal time alone would cut a second before the kill it is a clip of.
    /// Confidence and precision are separate fields of an event for exactly
    /// this reason — a rule filters on the one and pads with the other.
    Include {
        /// How much of the recording before the event to keep.
        lead: Duration,
        /// How much after it.
        trail: Duration,
    },
    /// It is not, and this is why.
    Skip(SkipReason),
}

impl Decision {
    /// Whether this event becomes a highlight.
    #[must_use]
    pub const fn is_included(&self) -> bool {
        matches!(self, Self::Include { .. })
    }

    /// Why it does not, if it does not.
    #[must_use]
    pub const fn skipped(&self) -> Option<SkipReason> {
        match self {
            Self::Include { .. } => None,
            Self::Skip(reason) => Some(*reason),
        }
    }
}

/// Every rule that applies, for one game or for the global settings.
///
/// # What "resolved" does not mean
///
/// It does not mean a finished table of every event kind. The vocabulary is
/// open — a plugin may invent a namespaced name at any time
/// (`clipped_events::EventKind::Custom`) — so a rule is folded when it is asked
/// for, by [`rule_for`](Self::rule_for), rather than enumerated in advance. A
/// kind nobody has configured resolves to what Clipped ships for it, which for
/// a name Clipped has never seen is *off*.
///
/// # The maximum length is a bound on merging, never a cut
///
/// Two windows that actually overlap always become one highlight, whatever the
/// maximum says, because the alternative is two clips of the same footage —
/// which is the entire failure this ticket exists to prevent. The maximum
/// bounds what merging may build **across a gap**: a burst separated by a lull
/// stops being joined once the result would be longer than it allows.
///
/// So a rule whose own window is longer than the maximum produces that window
/// and merges nothing into it. Truncating it instead would be the recorder
/// deciding that the user's fifteen seconds of lead were really nine
/// (AGENTS.md section 27).
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedHighlightRules {
    scope: Scope,
    global: HighlightRules,
    game: Option<HighlightRules>,
    merge_gap: Resolved<Duration>,
    maximum_length: Resolved<Duration>,
}

impl ResolvedHighlightRules {
    /// Folds the shipped defaults, the global layer and one game's.
    ///
    /// The layers are kept rather than flattened, for the reason the type
    /// documentation gives: the set of kinds is open, so there is no complete
    /// table to flatten into. They are cloned rather than borrowed because this
    /// outlives the configuration it was read from — a session resolves once at
    /// the start and holds the answer — and two `BTreeMap`s of a handful of
    /// small rules is not a cost worth a lifetime parameter on every consumer.
    pub(super) fn fold(
        scope: Scope,
        global: &HighlightRules,
        game: Option<&HighlightRules>,
    ) -> Self {
        // A game's layer only applies to a game's scope, exactly as
        // `ResolvedSettings::fold` treats it.
        let game = game.filter(|_| scope.game().is_some()).cloned();
        let layer = scope.layer();
        let merge_gap = pick(
            DEFAULT_MERGE_GAP,
            layer,
            global.merge_gap(),
            game.as_ref().and_then(HighlightRules::merge_gap),
        );
        let maximum_length = pick(
            DEFAULT_MAXIMUM_LENGTH,
            layer,
            global.maximum_length(),
            game.as_ref().and_then(HighlightRules::maximum_length),
        );

        Self {
            scope,
            global: global.clone(),
            game,
            merge_gap,
            maximum_length,
        }
    }

    /// Which layer this was resolved for.
    #[must_use]
    pub const fn scope(&self) -> &Scope {
        &self.scope
    }

    /// How close two windows have to be to become one highlight.
    #[must_use]
    pub const fn merge_gap(&self) -> &Resolved<Duration> {
        &self.merge_gap
    }

    /// The longest a highlight may become by merging across a gap.
    #[must_use]
    pub const fn maximum_length(&self) -> &Resolved<Duration> {
        &self.maximum_length
    }

    /// The rule that applies to `kind`, and where each of its values came from.
    #[must_use]
    pub fn rule_for(&self, kind: &EventKind) -> ResolvedRule {
        let shipped = shipped_rule(kind);
        let layer = self.scope.layer();
        let global = self.global.rule(kind);
        let game = self.game.as_ref().and_then(|rules| rules.rule(kind));

        ResolvedRule {
            enabled: pick(
                shipped.enabled,
                layer,
                global.and_then(HighlightRule::enabled),
                game.and_then(HighlightRule::enabled),
            ),
            lead: pick(
                shipped.lead,
                layer,
                global.and_then(HighlightRule::lead),
                game.and_then(HighlightRule::lead),
            ),
            trail: pick(
                shipped.trail,
                layer,
                global.and_then(HighlightRule::trail),
                game.and_then(HighlightRule::trail),
            ),
            minimum_confidence: pick(
                shipped.minimum_confidence,
                layer,
                global.and_then(HighlightRule::minimum_confidence),
                game.and_then(HighlightRule::minimum_confidence),
            ),
        }
    }

    /// Every kind either layer says anything about, in the vocabulary's own
    /// order and without duplicates.
    ///
    /// What a settings screen lists as "configured", and what a diagnostic
    /// prints. It is not the set of kinds that produce highlights — that is
    /// open, and [`rule_for`](Self::rule_for) answers for any kind at all.
    pub fn configured_kinds(&self) -> impl Iterator<Item = &EventKind> {
        let global = self.global.iter().map(|(kind, _)| kind);
        let game = self
            .game
            .iter()
            .flat_map(|rules| rules.iter().map(|(kind, _)| kind));
        // Both layers are sorted, and a kind in both must appear once.
        let mut kinds: Vec<&EventKind> = global.chain(game).collect();
        kinds.sort_unstable();
        kinds.dedup();
        kinds.into_iter()
    }

    /// What the rules say about one event.
    ///
    /// This is the whole of the per-event judgement, and it is deliberately
    /// separable from merging: the Highlights Only capture mode
    /// ([issue #77](https://github.com/wildware-uk/clipped/issues/77)) has to
    /// decide as each event arrives, with no view of the events after it, and
    /// must not need a second copy of these rules to do so.
    #[must_use]
    pub fn decision_for(&self, event: &GameEvent) -> Decision {
        let rule = self.rule_for(event.kind());
        if !rule.enabled.get() {
            return Decision::Skip(SkipReason::Disabled);
        }

        let confidence = event.confidence();
        if !confidence.is_usable() {
            return Decision::Skip(SkipReason::ConfidenceUnusable { confidence });
        }
        let minimum = rule.minimum_confidence.get();
        if confidence.as_f32() < minimum.as_f32() {
            return Decision::Skip(SkipReason::Uncertain {
                confidence,
                minimum,
            });
        }

        // Both ends are widened by how well the source knows the moment. See
        // `Decision::Include`.
        let precision = event.timing().precision();
        let lead = rule.lead.get().saturating_add(precision);
        let trail = rule.trail.get().saturating_add(precision);
        if lead.is_zero() && trail.is_zero() {
            return Decision::Skip(SkipReason::EmptyWindow);
        }

        Decision::Include { lead, trail }
    }

    /// The window `event` would be clipped to, on the recording's timeline.
    ///
    /// [`None`] when the rules do not select it. The two ends are moments in
    /// the recording, not offsets into a file: turning them into a clip's in
    /// and out points is `clipped_library::window_around`, which is also what
    /// clamps them to the part of the timeline the file actually contains.
    #[must_use]
    pub fn window_for(&self, event: &GameEvent) -> Option<(EventTime, EventTime)> {
        match self.decision_for(event) {
            Decision::Include { lead, trail } => {
                let at = event.timing().at();
                Some((at.saturating_sub(lead), at.saturating_add(trail)))
            }
            Decision::Skip(_) => None,
        }
    }
}

/// The last layer that says anything, and which one that was.
fn pick<T>(default: T, layer: SettingSource, global: Option<T>, game: Option<T>) -> Resolved<T> {
    let mut value = default;
    let mut source = SettingSource::Default;
    if let Some(set) = global {
        value = set;
        source = SettingSource::Global;
    }
    if let Some(set) = game {
        value = set;
        source = SettingSource::Game;
    }
    Resolved::new(value, source, layer)
}
