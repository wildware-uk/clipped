//! Turning a stream of events into the smallest set of clips that covers them.
//!
//! This is the substance of the rules. Selecting events one at a time is
//! arithmetic; deciding that a kill streak is **one** highlight rather than
//! five overlapping ones is the part a user would notice being wrong, because
//! the failure is silent — nothing errors, the library just fills with five
//! copies of the same twenty seconds.

use core::time::Duration;

use clipped_events::{EventTime, GameEvent};

use super::resolve::ResolvedHighlightRules;

/// A range of a recording the rules say is worth keeping, and why.
///
/// It borrows the events it was built from rather than copying them: an event's
/// payload is up to four kilobytes of a plugin's own detail
/// (`clipped_events::MAX_PAYLOAD_BYTES`), a session holds the events it is
/// deciding about anyway, and the consumer — generating the clips, [issue
/// #76](https://github.com/wildware-uk/clipped/issues/76) — turns each cause
/// into a `clipped_library::HighlightCause` immediately, which keeps three
/// fields of it.
///
/// Both ends are moments on the *recording's* timeline, which is what
/// `clipped_events::EventTime` counts, and neither has been clamped to any
/// file: a kill four seconds into a recording still has a window that starts
/// fifteen seconds before it. Fitting that to what a file contains is
/// `clipped_library::window_around`, which is also the place that knows a
/// saved replay does not start at zero.
#[derive(Debug, Clone, PartialEq)]
pub struct Highlight<'a> {
    start: EventTime,
    end: EventTime,
    causes: Vec<&'a GameEvent>,
}

impl<'a> Highlight<'a> {
    /// Where the clip would start on the recording's timeline.
    #[must_use]
    pub const fn start(&self) -> EventTime {
        self.start
    }

    /// Where it would end.
    #[must_use]
    pub const fn end(&self) -> EventTime {
        self.end
    }

    /// How long it would run for.
    #[must_use]
    pub fn duration(&self) -> Duration {
        self.end
            .duration_since(self.start)
            .unwrap_or(Duration::ZERO)
    }

    /// Every event that made this a highlight, earliest first.
    ///
    /// Never empty: a highlight exists because at least one event selected it.
    #[must_use]
    pub fn causes(&self) -> &[&'a GameEvent] {
        &self.causes
    }

    /// The earliest event in it.
    ///
    /// What the clip is named after, when something has to pick one — a clip of
    /// a firefight is a clip of what started the firefight. The rest are in
    /// [`causes`](Self::causes), and nothing here decides a title: that is
    /// [issue #76](https://github.com/wildware-uk/clipped/issues/76)'s, which
    /// has the recording and the game to name it with.
    ///
    /// # Panics
    ///
    /// Never: a highlight is only ever built around at least one event.
    #[must_use]
    pub fn primary(&self) -> &'a GameEvent {
        self.causes
            .first()
            .expect("a highlight is built around at least one event")
    }

    /// Whether this highlight and `other` cover any of the same recording.
    ///
    /// The property the merge exists to guarantee, and what the tests assert
    /// over every scenario rather than case by case.
    #[must_use]
    pub fn overlaps(&self, other: &Self) -> bool {
        self.start < other.end && other.start < self.end
    }
}

impl ResolvedHighlightRules {
    /// Every highlight these rules make of `events`.
    ///
    /// The events may arrive in any order and from any number of sources —
    /// nothing places an event by its arrival, and two integrations reporting
    /// the same second arrive in whatever order their transports allow
    /// (`clipped_events::EventTiming`). They are sorted here, by the moment
    /// each event *describes*.
    ///
    /// The result is in order, and **no two highlights overlap**. Two windows
    /// that touch are one clip; two that are closer than
    /// [`merge_gap`](Self::merge_gap) are one clip unless joining them would
    /// exceed [`maximum_length`](Self::maximum_length); anything further apart
    /// is two. Nothing is dropped by merging — every selected event is a cause
    /// of exactly one highlight — and nothing is truncated, so a burst that
    /// runs past the maximum becomes two clips rather than one clipped short.
    ///
    /// Events the rules do not select take no part: a disabled kind in the
    /// middle of a firefight neither extends a window nor splits one, because a
    /// rule that is off is off.
    #[must_use]
    pub fn highlights<'a, I>(&self, events: I) -> Vec<Highlight<'a>>
    where
        I: IntoIterator<Item = &'a GameEvent>,
    {
        let mut candidates: Vec<Candidate<'a>> = events
            .into_iter()
            .filter_map(|event| {
                self.window_for(event)
                    .map(|(start, end)| Candidate { start, end, event })
            })
            .collect();
        // Stable, so that two events with identical windows keep the order they
        // were given in and the result does not depend on the sort.
        candidates.sort_by_key(|candidate| (candidate.start, candidate.end));

        let merge_gap = self.merge_gap().get();
        let maximum_length = self.maximum_length().get();

        let mut highlights: Vec<Highlight<'a>> = Vec::new();
        for candidate in candidates {
            match highlights.last_mut() {
                Some(current) if joins(current, &candidate, merge_gap, maximum_length) => {
                    current.end = current.end.max(candidate.end);
                    current.causes.push(candidate.event);
                }
                _ => highlights.push(Highlight {
                    start: candidate.start,
                    end: candidate.end,
                    causes: vec![candidate.event],
                }),
            }
        }

        for highlight in &mut highlights {
            // The causes were added in window order, and a kind with a longer
            // lead opens its window earlier than a kind that happened before
            // it. What a reader of the clip wants is the order things happened.
            highlight.causes.sort_by_key(|event| event.timing().at());
        }
        highlights
    }
}

/// One event's window, before merging.
struct Candidate<'a> {
    start: EventTime,
    end: EventTime,
    event: &'a GameEvent,
}

/// Whether `candidate` belongs to `current` rather than starting a highlight of
/// its own.
///
/// The two halves are deliberately not symmetrical, and this is the decision
/// the module exists to make:
///
/// - **Overlapping windows always join.** Whatever the maximum length says. The
///   alternative is two clips of the same footage, which is the failure the
///   merge exists to prevent, and a user who set a short maximum asked for
///   shorter clips rather than for duplicates of the same seconds.
/// - **A gap is bridged only while the result stays within the maximum.** This
///   is where the ceiling bites: a burst that keeps going gets a second clip
///   rather than one that swallows the round.
fn joins(
    current: &Highlight<'_>,
    candidate: &Candidate<'_>,
    merge_gap: Duration,
    maximum_length: Duration,
) -> bool {
    if candidate.start <= current.end {
        return true;
    }
    if candidate.start > current.end.saturating_add(merge_gap) {
        return false;
    }
    let end = current.end.max(candidate.end);
    end.duration_since(current.start)
        .is_some_and(|length| length <= maximum_length)
}
