//! Turning the moments the rules chose into clips somebody can watch.
//!
//! This is the end of the detection chain. `clipped_events` says what happened,
//! [`super::merge`] decides which of it is worth keeping and merges a burst into
//! one moment, and this module produces the thing a user actually sees: a
//! [`VirtualClip`] of a recording, titled after what happened in it, tagged by
//! the kind of event that caused it (`docs/highlights.md`).
//!
//! # It writes nothing, and that is the design
//!
//! Generation produces virtual clips and never a file. A clip that has no file
//! is a range of a recording the user already has, so a session that produced
//! twenty interesting moments costs twenty rows of metadata rather than twenty
//! re-encodes of footage that is already on the disk (SPEC.md sections 19, 20
//! and 44).
//!
//! The alternative — writing each highlight out as it is found — was rejected,
//! and the reason is not only the encoder time:
//!
//! - **Automatic files spend a user's disk without being asked.** The storage
//!   quota ([issue #93](https://github.com/wildware-uk/clipped/issues/93)) is
//!   the budget the user set for the footage they chose to keep, and automatic
//!   cleanup ([issue #111](https://github.com/wildware-uk/clipped/issues/111))
//!   deletes the oldest unprotected recordings when that budget is reached. A
//!   recorder that generated a gigabyte of highlights after every session would
//!   be filling the budget with copies of footage the user already has, and
//!   then deleting their originals to make room for it. A virtual clip
//!   contributes **zero bytes** to that accounting, and its existence protects
//!   the recording it points at rather than competing with it.
//! - **Nineteen of the twenty will never be watched twice.** Rendering is what
//!   an export is for ([issue #89](https://github.com/wildware-uk/clipped/issues/89)),
//!   at the moment somebody asks for a file and at the quality they ask for.
//!
//! So nothing here has a means of writing anything:
//! `tests/generating_highlights_writes_nothing.rs` asserts it against this
//! module's own source, measures what generation costs, and compares a directory
//! byte for byte before and after a session's worth of clips is generated in it.
//!
//! # Which source a clip comes from
//!
//! **A file the session finished writing, and never the replay buffer.**
//!
//! A highlight is detected while the game is being played, so the material it
//! describes may be in the rolling buffer ([issue
//! #35](https://github.com/wildware-uk/clipped/issues/35)) rather than on disk.
//! Taking it from there is not a cheaper version of this: the packets a buffer
//! holds are in memory and about to be evicted, so keeping one means *writing a
//! file at that moment* (`docs/replay-buffer.md`), which is the thing this
//! module deliberately does not do. That save is a capture mode rather than
//! generation — Highlights Only ([issue
//! #77](https://github.com/wildware-uk/clipped/issues/77)) is the ticket whose
//! whole purpose is that the buffer is all there is — and the hotkey save it is
//! built on is `clipped_replay::save_clip`.
//!
//! Hence the answer to "what happens when the buffer has already evicted it":
//! **nothing is generated, and the reason says so.** A moment no file of the
//! session covers is reported as [`NotGenerated::NotRecorded`], carrying which
//! of the five cases it was — and for a session running the buffer alone, that
//! is `NothingRecorded`. Whether the buffer still holds the moment is a
//! question about memory that has already been overwritten by the time anything
//! could ask it, and offering a clip of a file that does not contain the kill it
//! claims would be a marker the user cannot check (AGENTS.md section 27).
//!
//! # When it runs
//!
//! After a recording has been finished, on whatever thread the caller likes —
//! and never on the one that is capturing. That is not a convention: the input
//! is a list of [`RecordedSegment`](clipped_library::events::RecordedSegment)s,
//! each of which is a *file and the span it covers*, and a file's span is only
//! known once it has been closed. A session that is still writing its second
//! recording therefore generates the highlights of its first, and the second
//! one's when it ends.
//!
//! Nothing here blocks, allocates on a capture path, takes a lock or waits on
//! anything (AGENTS.md section 20). It is arithmetic over data the caller
//! already holds: a busy three-hour session — 720 events, 180 clips — takes
//! 2.4 ms in a debug build, 13 µs a clip, and the same run again against those
//! 180 clips takes 0.3 ms. The test above is where those numbers come from.
//!
//! # Running it twice
//!
//! Generation is idempotent against the clips a session already has. Hand it
//! back what it produced last time — [`HighlightGeneration::with_existing_clips`]
//! — and it produces nothing new, however many times it is run. Two rules do
//! it, and both are needed:
//!
//! - **An event is clipped once.** A highlight one of whose causes is already
//!   the reason a generated clip exists is
//!   [`NotGenerated::AlreadyGenerated`].
//! - **Generated clips of a recording never overlap.** Even when the rules have
//!   changed between runs and the same events now merge differently, a range
//!   that covers seconds an existing generated clip already covers is
//!   [`NotGenerated::OverlapsAnExistingClip`]. This is the same invariant the
//!   merge guarantees within one run, extended across runs, and it is what keeps
//!   "re-run generation" from being the way a library fills with near-identical
//!   clips.
//!
//! Both are reported rather than silent, and neither deletes or rewrites
//! anything: a clip the user already has is theirs, and regenerating after
//! changing a rule leaves it alone (AGENTS.md section 56). A clip the user made
//! *by hand* takes no part in either rule — it is not a generated clip, and the
//! user asking for the same seconds twice is the user's business.

use core::fmt;
use core::time::Duration;

use clipped_edit::{RecordingId, SourceSpan};
use clipped_events::{EventKind, GameEvent};
use clipped_library::events::{NotRecorded, Placement, SessionRecordings};
use clipped_library::virtual_clip::{window_around, ClipOrigin, HighlightCause, VirtualClip};

use super::merge::Highlight;
use super::resolve::ResolvedHighlightRules;

/// The longest a generated title may be, in characters.
///
/// A title is built from event kinds, and a kind is an open vocabulary: a
/// plugin names its own, and a kind read back from a newer build's storage is
/// whatever text was stored. Ninety-six characters is comfortably more than
/// "Kill ×3, assist at 20:05" and still a title a list can show.
const MAXIMUM_TITLE_CHARACTERS: usize = 96;

/// Generating a session's highlight clips.
///
/// Three inputs, and the ownership of each is the point: the rules are resolved
/// once for the game being recorded ([`ResolvedHighlightRules`]), the recordings
/// are the files the session has *finished*, and the existing clips are what the
/// library already holds for it. Nothing here reads a setting, opens a file or
/// asks the library anything; a caller that has all three can generate on any
/// thread it likes.
#[derive(Debug, Clone, Copy)]
pub struct HighlightGeneration<'a> {
    rules: &'a ResolvedHighlightRules,
    recordings: &'a SessionRecordings,
    existing: &'a [VirtualClip],
}

impl<'a> HighlightGeneration<'a> {
    /// Generation for a session that has no clips yet.
    #[must_use]
    pub const fn new(rules: &'a ResolvedHighlightRules, recordings: &'a SessionRecordings) -> Self {
        Self {
            rules,
            recordings,
            existing: &[],
        }
    }

    /// The same generation, against the clips the session already has.
    ///
    /// This is what makes running generation twice produce nothing the second
    /// time. Clips that are not generated ones take no part — see the module
    /// documentation.
    #[must_use]
    pub const fn with_existing_clips(mut self, clips: &'a [VirtualClip]) -> Self {
        self.existing = clips;
        self
    }

    /// Every clip these events are worth, and every one they are not.
    ///
    /// The events may arrive in any order and from any number of sources;
    /// [`ResolvedHighlightRules::highlights`] sorts and merges them, so what is
    /// generated is one clip per *moment* rather than one per event. Events the
    /// rules do not select never reach here at all, and
    /// [`ResolvedHighlightRules::decision_for`] is what says why for one of
    /// them.
    #[must_use]
    pub fn generate<'e>(
        &self,
        events: impl IntoIterator<Item = &'e GameEvent>,
    ) -> GeneratedHighlights {
        let highlights = self.rules.highlights(events);
        let mut taken = Taken::of(self.existing);
        let mut generated = GeneratedHighlights::default();

        for highlight in &highlights {
            let cause = HighlightCause::of(highlight.primary());
            let (recording, window) = match self.cut(highlight, &taken) {
                Ok(cut) => cut,
                Err(reason) => {
                    generated.withheld.push(WithheldHighlight { cause, reason });
                    continue;
                }
            };

            let mut clip = VirtualClip::of_range(
                title_of(highlight.causes(), window.start().as_nanos()),
                recording.clone(),
                window,
                ClipOrigin::Highlight(cause),
            );
            for event in highlight.causes() {
                // Blank tags and repeats are `VirtualClip::with_tag`'s business,
                // so a highlight of three kills is tagged `kill` once.
                clip = clip.with_tag(event.kind().as_str());
            }

            // Claimed before the next highlight is considered, so that one run
            // is held to the same two rules as two runs are.
            taken.claim(highlight, &recording, window);
            generated.clips.push(clip);
        }

        generated
    }

    /// Which file this highlight is cut from and which part of it, or why it is
    /// not cut at all.
    ///
    /// Every refusal is decided here and the clip is built from the answer, so
    /// that construction has no branches in it and no reason can be reached by
    /// one path and not the other.
    fn cut(
        &self,
        highlight: &Highlight<'_>,
        taken: &Taken,
    ) -> Result<(RecordingId, SourceSpan), NotGenerated> {
        if taken.has_clipped_any_of(highlight) {
            return Err(NotGenerated::AlreadyGenerated);
        }

        // The file is the one holding the moment the clip is *named* after —
        // the earliest event in it. A merged window can reach past the end of
        // that file, into the gap before the session's next recording; it is
        // clamped to the file rather than split, because a clip drawing on two
        // recordings is [issue #88](https://github.com/wildware-uk/clipped/issues/88)
        // and the events outside it are still recorded as its causes.
        let at = highlight.primary().timing().at();
        let recording = match self.recordings.place(at) {
            Placement::In { recording, .. } => recording,
            Placement::NotRecorded(reason) => return Err(NotGenerated::NotRecorded(reason)),
        };
        // The same segment `place` answered from: one list, in one order, and
        // the first that contains the moment wins for both.
        let recorded = *self
            .recordings
            .segments()
            .iter()
            .find(|segment| segment.recording() == &recording && segment.recorded().contains(at))
            .expect("`place` answered with a segment that contains the moment")
            .recorded();

        // The one conversion from a moment on the recording's timeline into a
        // range of a file, clamped to what the file holds
        // (`clipped_library::window_around`). Written as a lead of nothing and a
        // trail of the whole highlight, because the rules have decided both ends
        // already: this is the merged window, not one event's.
        let window = window_around(
            highlight.start(),
            Duration::ZERO,
            highlight.duration(),
            &recorded,
        )
        .ok_or(NotGenerated::NothingToCut)?;

        if taken.overlaps(&recording, window) {
            return Err(NotGenerated::OverlapsAnExistingClip);
        }
        Ok((recording, window))
    }
}

/// What generation produced, and what it did not.
///
/// Both halves, always. A caller that only wanted the clips would have no way
/// to say "four of these five kills are in the recording and the fifth happened
/// before it started", which is the difference between a recorder that explains
/// itself and one that quietly produces fewer clips than the user expected
/// (AGENTS.md sections 15 and 27).
#[derive(Debug, Clone, Default)]
pub struct GeneratedHighlights {
    clips: Vec<VirtualClip>,
    withheld: Vec<WithheldHighlight>,
}

impl GeneratedHighlights {
    /// The clips, in the order the moments happened.
    #[must_use]
    pub fn clips(&self) -> &[VirtualClip] {
        &self.clips
    }

    /// The clips, to store.
    #[must_use]
    pub fn into_clips(self) -> Vec<VirtualClip> {
        self.clips
    }

    /// The highlights that produced no clip, and why each did not.
    #[must_use]
    pub fn withheld(&self) -> &[WithheldHighlight] {
        &self.withheld
    }

    /// How many highlights were withheld for `reason`.
    ///
    /// What a session logs at the end of a generation run: a count per reason
    /// says which of them happened without keeping the events themselves
    /// (AGENTS.md section 15).
    #[must_use]
    pub fn withheld_for(&self, reason: &NotGenerated) -> usize {
        self.withheld
            .iter()
            .filter(|withheld| &withheld.reason == reason)
            .count()
    }

    /// Whether nothing at all came of this run.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.clips.is_empty() && self.withheld.is_empty()
    }
}

/// A moment the rules chose that did not become a clip.
#[derive(Debug, Clone, PartialEq)]
pub struct WithheldHighlight {
    cause: HighlightCause,
    reason: NotGenerated,
}

impl WithheldHighlight {
    /// The event the clip would have been named after.
    #[must_use]
    pub const fn cause(&self) -> &HighlightCause {
        &self.cause
    }

    /// Why there is no clip.
    #[must_use]
    pub const fn reason(&self) -> &NotGenerated {
        &self.reason
    }
}

/// Why a moment the rules chose produced no clip.
///
/// None of these is an error, and none of them is a fault in a plugin: a game
/// that was running before the recorder attached, a session running only the
/// replay buffer, and a second generation run over a session that already has
/// its clips are all ordinary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotGenerated {
    /// No file this session finished covers the moment.
    ///
    /// Including [`NotRecorded::NothingRecorded`], which is the
    /// replay-buffer-only session: the material is in memory, keeping it means
    /// writing a file, and generation does not write files. See the module
    /// documentation.
    NotRecorded(NotRecorded),
    /// One of the events this moment is made of already has a clip.
    AlreadyGenerated,
    /// The range covers seconds a generated clip of the same recording already
    /// covers.
    OverlapsAnExistingClip,
    /// The part of the moment inside the file has no length.
    ///
    /// Reachable when the moment is the file's very last instant: the window
    /// clamped to the recording collapses to a point, and a clip of no length
    /// plays nothing.
    NothingToCut,
}

impl fmt::Display for NotGenerated {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRecorded(reason) => write!(formatter, "{reason}"),
            Self::AlreadyGenerated => {
                formatter.write_str("this moment already has a clip of its own")
            }
            Self::OverlapsAnExistingClip => {
                formatter.write_str("a generated clip already covers these seconds")
            }
            Self::NothingToCut => {
                formatter.write_str("the recording ends at the moment, so there is nothing to cut")
            }
        }
    }
}

/// What generated clips already cover: the events they exist for, and the
/// ranges of the files they play.
///
/// Built from the clips the library holds and added to as a run goes, so that
/// one run and two runs are held to the same rules.
#[derive(Debug, Default)]
struct Taken {
    causes: Vec<HighlightCause>,
    ranges: Vec<(RecordingId, SourceSpan)>,
}

impl Taken {
    /// What `clips` cover.
    ///
    /// Only the generated ones. A clip the user made by hand neither suppresses
    /// a highlight nor is suppressed by one; the library filters on
    /// `ClipOrigin` for exactly this kind of question.
    fn of(clips: &[VirtualClip]) -> Self {
        let mut taken = Self::default();
        for clip in clips.iter().filter(|clip| clip.origin().is_generated()) {
            if let Some(cause) = clip.origin().cause() {
                taken.causes.push(cause.clone());
            }
            let document = clip.edit();
            for segment in &document.segments {
                let Some(source) = document.source(segment.source) else {
                    // A document naming a source it does not declare is invalid
                    // and cannot be played; it is not this module's to repair,
                    // and treating it as covering nothing is the conservative
                    // reading (`EditDocument::validate`).
                    continue;
                };
                taken.ranges.push((source.recording.clone(), segment.span));
            }
        }
        taken
    }

    /// Whether any event of `highlight` already has a clip.
    fn has_clipped_any_of(&self, highlight: &Highlight<'_>) -> bool {
        highlight.causes().iter().any(|event| {
            let cause = HighlightCause::of(event);
            self.causes.iter().any(|taken| taken == &cause)
        })
    }

    /// Whether a generated clip of `recording` already covers any of `window`.
    fn overlaps(&self, recording: &RecordingId, window: SourceSpan) -> bool {
        self.ranges.iter().any(|(taken, span)| {
            taken == recording && window.start() < span.end() && span.start() < window.end()
        })
    }

    /// Records that `highlight` has become a clip of `window` of `recording`.
    fn claim(&mut self, highlight: &Highlight<'_>, recording: &RecordingId, window: SourceSpan) {
        for event in highlight.causes() {
            self.causes.push(HighlightCause::of(event));
        }
        self.ranges.push((recording.clone(), window));
    }
}

/// What a clip of these events is called.
///
/// Made of what the events actually say and nothing else: which kinds happened,
/// how many of each, and where in the file the clip starts. "Kill ×3, assist at
/// 20:05" is a title somebody scanning a list can tell from the other nineteen,
/// which is the whole job — a clip named "Highlight" thirty times over is a
/// library nobody searches (AGENTS.md section 28).
///
/// The kinds are in the order the events happened, which the merge has already
/// sorted them into, so the first one named is the moment the clip opens on.
/// Nothing is inferred beyond counting: three kills close together is "Kill ×3"
/// and not "Triple kill", because this module does not know that game's word
/// for it and inventing one would be putting a claim in a user's library that
/// nothing checked (AGENTS.md section 27).
fn title_of(causes: &[&GameEvent], start_nanos: u64) -> String {
    let mut counted: Vec<(String, usize)> = Vec::new();
    for event in causes {
        let Some(label) = label_of(event.kind()) else {
            continue;
        };
        match counted.iter_mut().find(|(named, _)| named == &label) {
            Some((_, count)) => *count += 1,
            None => counted.push((label, 1)),
        }
    }

    let named: Vec<String> = counted
        .iter()
        .enumerate()
        .map(|(position, (label, count))| {
            // Sentence case: the first kind opens the title, the rest read as
            // part of the same sentence.
            let label = if position == 0 {
                sentence_case(label)
            } else {
                label.clone()
            };
            if *count > 1 {
                format!("{label} ×{count}")
            } else {
                label
            }
        })
        .collect();

    // An event whose kind is blank is possible only for one read back from
    // storage, and a clip still deserves a name.
    let named = if named.is_empty() {
        "Highlight".to_owned()
    } else {
        named.join(", ")
    };
    shortened(format!("{named} at {}", timecode(start_nanos)))
}

/// How an event kind reads in a title, or [`None`] when it says nothing.
///
/// A plugin's own name is namespaced — `acme-cs2.flag_captured`
/// (`docs/plugin-api.md`) — and the namespace is how the vocabulary stays
/// collision-free rather than something to show somebody, so a title uses the
/// part after it. Underscores are how the wire spells a space.
fn label_of(kind: &EventKind) -> Option<String> {
    let wire = kind.as_str();
    let name = wire.rsplit('.').next().unwrap_or(wire).replace('_', " ");
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// `text` with its first character in upper case.
fn sentence_case(text: &str) -> String {
    let mut characters = text.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    }
}

/// Where in the file a clip starts, as a person reads a position.
fn timecode(nanos: u64) -> String {
    let seconds = nanos / 1_000_000_000;
    let (hours, minutes, seconds) = (seconds / 3_600, (seconds % 3_600) / 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// `title` cut to [`MAXIMUM_TITLE_CHARACTERS`], with an ellipsis when it was.
fn shortened(title: String) -> String {
    if title.chars().count() <= MAXIMUM_TITLE_CHARACTERS {
        return title;
    }
    let cut = title
        .char_indices()
        .nth(MAXIMUM_TITLE_CHARACTERS - 1)
        .map_or(title.len(), |(index, _)| index);
    format!("{}…", &title[..cut])
}
