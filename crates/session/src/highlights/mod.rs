//! Which moments are worth a clip, how much of the recording each one keeps,
//! and how a burst of them becomes one highlight rather than twenty.
//!
//! [`clipped_events`] defines what happened; `clipped_library`'s virtual clip
//! defines what a clip is. This module is the rule between them — and, in
//! `generate.rs`, the step that turns the moments those rules chose into the
//! clips themselves. It is the only place the question "is that worth keeping"
//! is answered (`docs/highlights.md`).
//!
//! # The whole model in one example
//!
//! ```
//! use clipped_events::{Confidence, EventKind, EventSource, EventTime, EventTiming, GameEvent};
//! use clipped_session::config::Scope;
//! use clipped_session::highlights::HighlightRules;
//! # use core::time::Duration;
//!
//! # fn kill(second: i64) -> GameEvent {
//! #     GameEvent::new(
//! #         EventKind::Kill,
//! #         EventTiming::new(EventTime::from_media_nanos(second * 1_000_000_000), Duration::ZERO),
//! #         EventSource::plugin("acme-cs2").expect("a valid identifier"),
//! #         Confidence::CERTAIN,
//! #     )
//! # }
//! // Nothing configured: the rules Clipped ships with.
//! let rules = HighlightRules::resolve(Scope::Global, &HighlightRules::none(), None);
//!
//! // A kill, then two more while the fight is still going.
//! let events = [kill(60), kill(63), kill(66)];
//! let highlights = rules.highlights(&events);
//!
//! // One clip, not three: 15 s before the first kill to 10 s after the last.
//! assert_eq!(highlights.len(), 1);
//! assert_eq!(highlights[0].causes().len(), 3);
//! assert_eq!(highlights[0].duration(), Duration::from_secs(31));
//! ```
//!
//! # The three decisions
//!
//! **Merging is the substance.** A kill streak is one moment that produced five
//! events, and five clips of the same twenty seconds is not a feature — it is
//! the library becoming useless silently, with nothing to report and nothing to
//! fail. So [`ResolvedHighlightRules::highlights`] guarantees that no two
//! highlights it produces overlap, and `merge.rs` states which of the two rules
//! wins when they disagree: windows that touch always join, and a gap is
//! bridged only while the result stays inside the maximum length.
//!
//! **How much to keep differs by kind, and by game.** Fifteen seconds before a
//! kill and ten after (SPEC.md section 7); ten and five for a death, because
//! the interesting part of dying is what led to it. A game whose fights are
//! longer says so once, in its own layer, and inherits everything it does not
//! mention — the same three-layer fold, in the same order, with the same
//! meaning of "says nothing", that `crate::config` applies to the frame rate
//! (AGENTS.md sections 30 and 55).
//!
//! **A rule can ignore a guess.** An event carries how sure its source is that
//! it happened at all, separately from how well it knows when
//! (`clipped_events::Confidence`), because an integration reading an
//! authoritative feed and a detector watching the screen are not the same
//! claim. A rule filters on the first and pads its window with the second.
//!
//! # From a range to a clip
//!
//! [`HighlightGeneration`] is the other half, and it is where the detection
//! chain finally produces something a user sees: for each merged range, a
//! `clipped_library::VirtualClip` of the recording that holds it, titled after
//! what happened and tagged by kind ([issue
//! #76](https://github.com/wildware-uk/clipped/issues/76), `generate.rs`).
//!
//! **It writes no file, and no automatic generation ever will.** A virtual clip
//! costs zero bytes, so a session's twenty highlights cost twenty rows rather
//! than twenty re-encodes of footage the user already has — and a recorder that
//! wrote them out would be filling the storage quota
//! ([#93](https://github.com/wildware-uk/clipped/issues/93)) with copies, which
//! automatic cleanup ([#111](https://github.com/wildware-uk/clipped/issues/111))
//! would then make room for by deleting the originals. Rendering a file is what
//! an export is for, at the moment somebody asks for one.
//!
//! It also never reaches into the replay buffer. A clip cut from the buffer is a
//! file written at the moment of the save, because the packets are about to be
//! evicted (`docs/replay-buffer.md`), and that is a capture mode — Highlights
//! Only ([#77](https://github.com/wildware-uk/clipped/issues/77)) — rather than
//! generation. A moment no finished file covers produces no clip and says which
//! of the four cases it was, and a moment that runs past the end of the file it
//! started in is titled and tagged for the part inside the clip alone.
//!
//! # What this module does not do
//!
//! **It does not read the settings file.** [`HighlightRules::read`] and
//! [`HighlightRules::write`] are the section's format, and
//! `crate::config::Configuration` does not hold the section yet, so a user
//! cannot configure these rules by editing `settings.json` today. That wiring
//! is [issue #290](https://github.com/wildware-uk/clipped/issues/290) — one
//! field beside `hotkeys`, in a file this ticket deliberately did not touch —
//! and until it lands every caller gets the shipped defaults. Saying so is
//! better than a settings section that appears to be in force and is not
//! (AGENTS.md section 54); a build without it still preserves a `highlights`
//! section written by a build with it, because
//! `Configuration::unrecognised_keys` keeps top-level keys it does not
//! understand.
//!
//! **It does not decide anything live.** [`ResolvedHighlightRules::decision_for`]
//! is the per-event judgement the Highlights Only capture mode
//! ([issue #77](https://github.com/wildware-uk/clipped/issues/77)) needs as
//! each event arrives, and it is public for that reason, but the decision about
//! whether the replay buffer still holds the moment — the one
//! `clipped_events::EventTiming::latency` is for — belongs with the buffer.

mod document;
mod error;
mod generate;
#[cfg(test)]
mod generation_tests;
mod merge;
mod resolve;
mod rule;
mod rules;
#[cfg(test)]
mod tests;

use clipped_edit::RecordingId;
use clipped_events::{EventTime, RecordedSpan};
use clipped_library::events::{RecordedSegment, SessionRecordings};
use clipped_library::virtual_clip::VirtualClip;

use crate::automatic::RecordingOutcomeSummary;
use crate::config::Scope;

pub use error::{HighlightRuleError, RuleSetting};
pub use generate::{GeneratedHighlights, HighlightGeneration, NotGenerated, WithheldHighlight};
pub use merge::Highlight;
pub use resolve::{Decision, ResolvedHighlightRules, ResolvedRule, SkipReason};
pub use rule::{
    default_minimum_confidence, shipped_rule, HighlightRule, ShippedRule, DEFAULT_MAXIMUM_LENGTH,
    DEFAULT_MERGE_GAP, LONGEST_MAXIMUM_LENGTH, MAXIMUM_LEAD, MAXIMUM_MERGE_GAP, MAXIMUM_TRAIL,
    SHORTEST_MAXIMUM_LENGTH,
};
pub use rules::HighlightRules;

/// Turns a finished session's events into the clips they were worth.
///
/// The caller [issue #76](https://github.com/wildware-uk/clipped/issues/76)
/// asks for, and the reason every piece beneath it exists: the rules decide
/// which moments deserve a clip, `generate` cuts the ranges, and the session
/// carries them to its sidecar and from there into the library.
///
/// # What it does not do
///
/// **It renders nothing.** A generated clip is a range of a recording and costs
/// no disk until somebody asks for a file (SPEC.md sections 19, 20 and 44), so
/// this is arithmetic over events and touches no encoder, no capture thread and
/// no filesystem.
///
/// **It does not run twice to a different answer.** The clips already on the
/// session are handed to the generator, which refuses to cut a range it has
/// already taken, and `Session::record_generated` replaces rather than appends.
/// Re-generating a session it has seen answers the same clips.
///
/// # Which rules
///
/// The shipped defaults. Nothing records per-game highlight rules yet -- the
/// configuration model has no notion of them -- so resolving against
/// [`HighlightRules::none`] is the whole of the answer rather than a
/// placeholder for one: it *is* what an unconfigured Clipped should generate.
/// When rules become configurable, this is the one call site that has to read
/// them.
///
/// # Which recordings
///
/// Only the ones with a span: a recording that produced no frame covers no
/// moment, and one whose row predates spans being recorded has no timeline to
/// cut against. A session with no such recording generates nothing, which is
/// the truthful answer -- there is no footage for a clip to be a range *of*.
#[must_use]
pub fn generate_for(session: &crate::automatic::Session) -> Vec<VirtualClip> {
    let segments: Vec<RecordedSegment> = session
        .recordings()
        .iter()
        .filter_map(|recording| {
            let start = EventTime::from_media_nanos(recording.starts_at_nanos()?);
            let RecordingOutcomeSummary::Recorded { duration, .. } = recording.outcome()? else {
                return None;
            };
            let span = RecordedSpan::new(start, start.saturating_add(*duration))?;
            // The recording's ordinal, which is how a sidecar names one and how
            // the library resolves it back to a row (`source_recording`).
            Some(RecordedSegment::new(
                RecordingId::new(recording.index().to_string()),
                span,
            ))
        })
        .collect();

    let recordings = SessionRecordings::of(segments);
    let rules = HighlightRules::resolve(Scope::Global, &HighlightRules::none(), None);

    HighlightGeneration::new(&rules, &recordings)
        .with_existing_clips(session.generated())
        .generate(session.game_events())
        .into_clips()
}
