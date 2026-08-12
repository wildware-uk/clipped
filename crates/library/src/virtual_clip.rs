//! A clip that has no file: what one *is*, why it exists, and what it costs.
//!
//! A virtual clip is a range of a recording that behaves like a clip without a
//! file existing, so that a highlight can be offered, listed and played before
//! anything is exported and costs nothing until the user asks for a file
//! (SPEC.md sections 19, 20 and 44). `docs/highlights.md` argues the model;
//! this is the map of it.
//!
//! # A virtual clip is an edit document plus a reason
//!
//! The obvious risk here was building a second edit model — a source
//! identifier, a start, an end and an arithmetic of its own — beside the one
//! [`clipped_edit`] already has (AGENTS.md section 55). It is not needed.
//! `EditDocument::from_recording` is already "one recording, one span, no
//! rendering", written for SPEC.md section 20, and every question a virtual
//! clip is asked about *time* — how long is it, what plays at this moment,
//! which part of which recording is that — is a question the document already
//! answers, in types that cannot be mixed up.
//!
//! So a virtual clip **composes** rather than duplicates:
//!
//! ```text
//!   VirtualClip
//!     ├── edit: EditDocument   what plays, and which parts of what
//!     ├── origin: ClipOrigin   why it exists
//!     └── tags                 what the user (or generation) filed it under
//! ```
//!
//! What the document deliberately cannot hold is the second field. Provenance
//! is game-event vocabulary — a kill, reported by a plugin, at a moment on the
//! recording's timeline — and [`clipped_edit`] is a layer 0 crate that must not
//! name [`clipped_events`]. It should not want to, either: an export must
//! produce the same file whether the range was dragged by hand or generated
//! from a kill, so "why" is not an instruction for rendering. It is library
//! metadata, and this crate is where the two meet.
//!
//! # "Virtual" is not a state of the model
//!
//! There is no `is_virtual` flag here, and no conversion from a virtual clip
//! into a real one. Virtual means *no exported file exists yet*, which is a
//! fact about the library's row (`clips.path`), not about the clip: exporting
//! adds a file, and the document stays what the clip is. That is what makes a
//! generated highlight free, and what makes re-exporting it at a different
//! quality a second file rather than a second clip.
//!
//! # What it costs
//!
//! Nothing that grows. A clip of two seconds and a clip of three hours are the
//! same handful of fields, and creating one neither reads nor writes a media
//! file — this module opens no file at all, which
//! `tests/a_virtual_clip_costs_nothing.rs` asserts against its source rather
//! than trusting this paragraph. For storage accounting ([issue
//! #93](https://github.com/wildware-uk/clipped/issues/93)) a virtual clip
//! contributes **zero bytes**; the bytes belong to the recording it points at,
//! counted once, there.
//!
//! # Threading
//!
//! Plain data, `Send` and `Sync`, owning everything it refers to and with no
//! interior mutability. Generation runs after a session on a background thread
//! ([issue #76](https://github.com/wildware-uk/clipped/issues/76)) and hands
//! the result to whoever stores it; nothing here needs a lock.

use core::time::Duration;

use clipped_edit::{EditDocument, RecordingId, SourceSpan, SourceTime};
use clipped_events::{EventKind, EventSource, EventTime, GameEvent, RecordedSpan};
use serde::{Deserialize, Serialize};

/// A clip that exists as metadata over a recording, with no file of its own.
///
/// Created by [`of_range`](Self::of_range) — the shape SPEC.md section 20
/// describes — or by [`from_edit`](Self::from_edit) once the editor has made it
/// something more than a range.
#[derive(Debug, Clone, PartialEq)]
pub struct VirtualClip {
    edit: EditDocument,
    origin: ClipOrigin,
    tags: Vec<String>,
}

impl VirtualClip {
    /// A clip of `span` of `recording`, titled `title`, that exists because of
    /// `origin`.
    ///
    /// Instant and free: one document with one source and one segment, and no
    /// media of any kind.
    #[must_use]
    pub fn of_range(
        title: impl Into<String>,
        recording: RecordingId,
        span: SourceSpan,
        origin: ClipOrigin,
    ) -> Self {
        Self::from_edit(EditDocument::from_recording(title, recording, span), origin)
    }

    /// A clip playing `edit`, that exists because of `origin`.
    ///
    /// The constructor for a clip that is no longer a plain range — one the
    /// editor has trimmed, split or combined ([issues
    /// #84](https://github.com/wildware-uk/clipped/issues/84) and
    /// [#88](https://github.com/wildware-uk/clipped/issues/88)). Editing a
    /// generated highlight does not change why it was generated, so the origin
    /// travels with it.
    #[must_use]
    pub const fn from_edit(edit: EditDocument, origin: ClipOrigin) -> Self {
        Self {
            edit,
            origin,
            tags: Vec::new(),
        }
    }

    /// The same clip filed under `tag`.
    ///
    /// Blank tags and tags the clip already carries are ignored, so that
    /// generating highlights twice cannot produce `kill, kill` and so that a
    /// tag is never stored as whitespace nothing can search for.
    #[must_use]
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        let tag = tag.into();
        let tag = tag.trim();
        if !tag.is_empty() && !self.tags.iter().any(|existing| existing == tag) {
            self.tags.push(tag.to_owned());
        }
        self
    }

    /// What the clip is called.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.edit.title
    }

    /// What plays, and which parts of which recordings.
    #[must_use]
    pub const fn edit(&self) -> &EditDocument {
        &self.edit
    }

    /// The document, to edit.
    ///
    /// The editor's operations ([issues
    /// #84](https://github.com/wildware-uk/clipped/issues/84) to
    /// [#88](https://github.com/wildware-uk/clipped/issues/88)) act on an
    /// `EditDocument`, and a clip is not a different thing once one has been
    /// applied to it.
    pub const fn edit_mut(&mut self) -> &mut EditDocument {
        &mut self.edit
    }

    /// Why the clip exists.
    #[must_use]
    pub const fn origin(&self) -> &ClipOrigin {
        &self.origin
    }

    /// What the clip is filed under.
    #[must_use]
    pub fn tags(&self) -> &[String] {
        &self.tags
    }

    /// How long the clip runs for.
    ///
    /// [`None`] only for a document holding a segment that cannot be read; see
    /// [`EditDocument::output_duration_nanos`].
    #[must_use]
    pub fn duration(&self) -> Option<Duration> {
        self.edit.output_duration_nanos().map(Duration::from_nanos)
    }

    /// Every recording this clip needs in order to play.
    ///
    /// One in the ordinary case, and more once clips have been combined.
    pub fn source_recordings(&self) -> impl Iterator<Item = &RecordingId> {
        self.edit.sources.iter().map(|source| &source.recording)
    }

    /// Whether this clip needs `recording`.
    #[must_use]
    pub fn depends_on(&self, recording: &RecordingId) -> bool {
        self.source_recordings().any(|source| source == recording)
    }

    /// Whether the clip can be played, given what became of its sources.
    ///
    /// `availability` is the library's answer for one recording; this crate
    /// does not go to disk to ask. The worst answer wins, because a clip that
    /// needs two recordings and has one is not playable.
    pub fn state(&self, availability: impl Fn(&RecordingId) -> SourceAvailability) -> ClipState {
        self.source_recordings()
            .map(|recording| match availability(recording) {
                SourceAvailability::Present => ClipState::Playable,
                SourceAvailability::InTrash => ClipState::SourceInTrash,
                SourceAvailability::Missing => ClipState::SourceMissing,
            })
            .max()
            .unwrap_or(ClipState::Playable)
    }
}

/// Why a clip exists.
///
/// Three producers, and a clip can only have come from one of them. It is a
/// closed vocabulary rather than free text because the library filters on it —
/// "show me what Clipped generated" is a different question from "show me what
/// I saved" — and because a generated clip has to be traceable back to the
/// event that caused it, which a label cannot do.
///
/// # Its stored form
///
/// Serialises as an internally tagged object, `{"origin":"highlight", …}`,
/// which is the shape the `session_events` table already stores a kind and its
/// detail in (`docs/storage.md`). The tag is `origin` rather than `kind`
/// because the detail of a generated clip has a `kind` of its own — the
/// event's — and two different kinds under one name is how a reader ends up
/// with the wrong one. Nothing persists a virtual clip yet: the `clips` table
/// requires a `path`, so storing one needs a migration ([issue
/// #269](https://github.com/wildware-uk/clipped/issues/269)).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "origin", rename_all = "kebab-case")]
pub enum ClipOrigin {
    /// A range the user chose on the timeline ([issue
    /// #91](https://github.com/wildware-uk/clipped/issues/91)).
    Manual,
    /// The window the replay buffer held when a hotkey was pressed
    /// (`docs/replay-buffer.md`).
    ///
    /// A saved replay is written to a file at the moment it is saved, because
    /// the packets it is made of are in memory and about to be evicted — so
    /// this origin describes a clip that has a file from the start. It is here
    /// because the range and the reason are the same shape as the other two,
    /// and a library that modelled saved replays separately would ask every
    /// screen to handle two kinds of clip.
    ReplayBuffer,
    /// Generated from something that happened in the game ([issue
    /// #76](https://github.com/wildware-uk/clipped/issues/76)).
    Highlight(HighlightCause),
}

impl ClipOrigin {
    /// Whether Clipped made this clip rather than the user.
    ///
    /// The distinction the library needs before deleting anything in bulk: a
    /// clip the user made by hand is not something to tidy away.
    #[must_use]
    pub const fn is_generated(&self) -> bool {
        matches!(self, Self::Highlight(_))
    }

    /// What caused the clip, when a game event did.
    #[must_use]
    pub const fn cause(&self) -> Option<&HighlightCause> {
        match self {
            Self::Highlight(cause) => Some(cause),
            Self::Manual | Self::ReplayBuffer => None,
        }
    }
}

/// The game event a generated clip was made for.
///
/// Enough to answer "why is this here?" — what happened, when it happened on
/// the recording's timeline, and who said so — and deliberately not the whole
/// [`GameEvent`]. The payload can be four kilobytes of a plugin's own detail
/// and would be a second copy of a row that persistence
/// ([#71](https://github.com/wildware-uk/clipped/issues/71)) already owns; the
/// confidence is what the rules ([#75](https://github.com/wildware-uk/clipped/issues/75))
/// filtered on before deciding to generate at all, and repeating it on the
/// result would invite a second, later judgement of the same number.
///
/// [`at`](Self::at) is the moment the event describes, not the moment it was
/// reported: `clipped_events::EventTiming` keeps those apart, and a marker
/// drawn at the arrival time is a marker in the wrong place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HighlightCause {
    /// What happened.
    kind: EventKind,
    /// When it happened, on the recording's timeline.
    at: EventTime,
    /// Who reported it.
    source: EventSource,
}

impl HighlightCause {
    /// The cause taken from the event that produced the clip.
    #[must_use]
    pub fn of(event: &GameEvent) -> Self {
        Self {
            kind: event.kind().clone(),
            at: event.timing().at(),
            source: event.source().clone(),
        }
    }

    /// What happened.
    #[must_use]
    pub const fn kind(&self) -> &EventKind {
        &self.kind
    }

    /// When it happened, on the recording's timeline.
    #[must_use]
    pub const fn at(&self) -> EventTime {
        self.at
    }

    /// Who reported it.
    #[must_use]
    pub const fn source(&self) -> &EventSource {
        &self.source
    }
}

/// The range of a recording to keep around a moment in it.
///
/// The one place an event's time becomes an edit's time. They are the same
/// quantity against the same zero — nanoseconds from the recording's epoch,
/// which is the timestamp of its first kept video frame — but they are types in
/// two layer 0 crates that cannot name each other, so somebody has to convert,
/// and it should be somewhere with a test rather than at each call site (the
/// duplication [issue #253](https://github.com/wildware-uk/clipped/issues/253)
/// is about). This is that somewhere.
///
/// Two things it does that a subtraction at the call site would get wrong:
///
/// - **It measures from the file, not from the session.** `recorded` is the
///   part of the timeline the file actually contains, and for a saved replay
///   that does not start at zero (`clipped_events::RecordedSpan`). An event
///   twenty minutes into a session is ten seconds into that clip.
/// - **It clamps rather than failing.** A kill four seconds into a recording
///   with a fifteen-second lead still deserves a clip; it just starts at the
///   beginning. The window is intersected with what exists, and [`None`] means
///   the intersection was empty — the file does not cover the moment at all,
///   so there is nothing to offer.
///
/// It is arithmetic, not policy: which events are worth a clip, how much lead
/// and trail each kind gets, and how a burst of them is merged into one clip
/// are [issue #75](https://github.com/wildware-uk/clipped/issues/75)'s rules,
/// which produce the `lead` and `trail` passed in here.
#[must_use]
pub fn window_around(
    at: EventTime,
    lead: Duration,
    trail: Duration,
    recorded: &RecordedSpan,
) -> Option<SourceSpan> {
    let start = at.saturating_sub(lead).max(recorded.start());
    let end = at.saturating_add(trail).min(recorded.end());
    if end <= start {
        return None;
    }

    // Both ends are inside the span by construction, so `position_of` answers
    // for both; it is what turns a moment on the recording's timeline into an
    // offset into the file, which is what `SourceTime` counts.
    let start = recorded.position_of(start)?;
    let end = recorded.position_of(end)?;
    SourceSpan::new(
        SourceTime::from_nanos(u64::try_from(start.as_nanos()).ok()?),
        SourceTime::from_nanos(u64::try_from(end.as_nanos()).ok()?),
    )
}

/// What the library knows about a recording a clip needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceAvailability {
    /// The file is where the library expects it.
    Present,
    /// The user deleted it and it is recoverable (SPEC.md section 28).
    InTrash,
    /// It is not on disk and it is not in the trash.
    Missing,
}

/// Whether a clip can be played, and why not when it cannot.
///
/// Ordered by how bad the news is, so that the worst source of a clip decides:
/// [`Playable`](Self::Playable) < [`SourceInTrash`](Self::SourceInTrash) <
/// [`SourceMissing`](Self::SourceMissing).
///
/// A clip is never removed for being in either of the last two states. It is
/// listed, marked and left alone: a recording restored from the trash makes its
/// clips play again, and deleting a user's clip because their drive was
/// unplugged is exactly what AGENTS.md section 56 forbids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ClipState {
    /// Every recording it needs is there.
    Playable,
    /// A recording it needs is in the trash, and restoring it would fix this.
    SourceInTrash,
    /// A recording it needs is gone.
    SourceMissing,
}

/// What deleting a recording would cost the clips that depend on it.
///
/// A virtual clip has no file of its own, so it cannot outlive its source the
/// way a saved replay can: deleting the recording is deleting the only copy of
/// the material. The rule that follows, and the answer to "what happens when
/// the source is deleted":
///
/// - **Automatic cleanup never deletes a referenced recording.** The storage
///   manager ([issue #111](https://github.com/wildware-uk/clipped/issues/111))
///   skips it exactly as it skips a favourite, which is what SPEC.md section 27
///   means by protecting what the user has marked. This is how a virtual clip
///   counts towards the storage protection of its source.
/// - **A person may still delete it, having been told.** Blocking outright
///   would leave a user unable to reclaim their own disk; so the deletion is
///   confirmed rather than refused, with the number of clips that will stop
///   playing stated first.
/// - **The clips are kept either way.** The recording goes to the trash, the
///   clips become [`ClipState::SourceInTrash`], and restoring the recording
///   restores them. Nothing is deleted on the user's behalf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceDeletion {
    /// No clip depends on the recording.
    Unreferenced,
    /// Clips depend on it, and this many will stop playing.
    Referenced {
        /// How many clips need the recording.
        clips: usize,
    },
}

impl SourceDeletion {
    /// What deleting `recording` would cost, given `clips`.
    #[must_use]
    pub fn examine<'a>(
        recording: &RecordingId,
        clips: impl IntoIterator<Item = &'a VirtualClip>,
    ) -> Self {
        let clips = clips
            .into_iter()
            .filter(|clip| clip.depends_on(recording))
            .count();
        if clips == 0 {
            Self::Unreferenced
        } else {
            Self::Referenced { clips }
        }
    }

    /// How many clips would stop playing.
    #[must_use]
    pub const fn dependent_clips(self) -> usize {
        match self {
            Self::Unreferenced => 0,
            Self::Referenced { clips } => clips,
        }
    }

    /// Whether automatic cleanup may delete the recording.
    #[must_use]
    pub const fn automatic_cleanup_may_delete(self) -> bool {
        matches!(self, Self::Unreferenced)
    }

    /// Whether a person has to be told what they are about to lose first.
    #[must_use]
    pub const fn needs_confirmation(self) -> bool {
        matches!(self, Self::Referenced { .. })
    }
}

#[cfg(test)]
mod tests {
    use clipped_edit::{OutputTime, Segment, Source, SourceId};
    use clipped_events::{Confidence, EventTiming};

    use super::*;

    const SECOND: u64 = 1_000_000_000;

    fn span(start_seconds: u64, end_seconds: u64) -> SourceSpan {
        SourceSpan::new(
            SourceTime::from_nanos(start_seconds * SECOND),
            SourceTime::from_nanos(end_seconds * SECOND),
        )
        .expect("the test span ends after it starts")
    }

    fn at_second(seconds: i64) -> EventTime {
        EventTime::from_media_nanos(seconds * 1_000_000_000)
    }

    /// A kill that was heard about two seconds after it happened.
    ///
    /// The latency is not decoration. `clipped_events::EventTiming` keeps the
    /// moment an event *describes* apart from the moment it was *reported*, and
    /// a cause that recorded the second would put every generated clip two
    /// seconds late — so the fixture makes the two numbers different and the
    /// tests below name which one they expect.
    fn kill_at(seconds: i64) -> GameEvent {
        GameEvent::new(
            EventKind::Kill,
            EventTiming::new(at_second(seconds), Duration::ZERO)
                .reported_late_by(Duration::from_secs(2)),
            EventSource::plugin("acme-cs2").expect("a well-formed plugin identifier"),
            Confidence::new(1.0).expect("a valid confidence"),
        )
    }

    fn manual_clip(recording: &str, from: u64, to: u64) -> VirtualClip {
        VirtualClip::of_range(
            "Ace",
            RecordingId::new(recording),
            span(from, to),
            ClipOrigin::Manual,
        )
    }

    #[test]
    fn a_virtual_clip_is_an_edit_document_and_a_reason() {
        let clip = manual_clip("rec-1", 30, 42);

        // The range half is the edit model's, unchanged: one source, one
        // segment, and the document's own arithmetic answering for it.
        assert_eq!(clip.edit().sources.len(), 1);
        assert_eq!(clip.edit().segments.len(), 1);
        assert_eq!(clip.duration(), Some(Duration::from_secs(12)));
        assert_eq!(clip.title(), "Ace");
        clip.edit().validate().expect("an instant clip is valid");

        // Two seconds into the clip is thirty-two seconds into the recording,
        // and this model did not have to work that out.
        let placement = clip
            .edit()
            .locate(OutputTime::from_nanos(2 * SECOND))
            .expect("two seconds in is inside the clip");
        assert_eq!(placement.source_time, SourceTime::from_nanos(32 * SECOND));

        // And the half a document cannot hold.
        assert_eq!(clip.origin(), &ClipOrigin::Manual);
        assert!(!clip.origin().is_generated());
    }

    #[test]
    fn a_generated_clip_carries_the_event_that_caused_it() {
        let event = kill_at(600);
        let cause = HighlightCause::of(&event);
        let clip = VirtualClip::of_range(
            "Kill",
            RecordingId::new("rec-1"),
            span(585, 610),
            ClipOrigin::Highlight(cause),
        )
        .with_tag("kill");

        let cause = clip
            .origin()
            .cause()
            .expect("a generated clip says what caused it");
        assert_eq!(cause.kind(), &EventKind::Kill);
        assert_eq!(
            cause.at(),
            at_second(600),
            "the cause is the moment the kill happened, not the moment the plugin said so"
        );
        assert_eq!(cause.source().as_str(), "acme-cs2");
        assert!(clip.origin().is_generated());
        assert_eq!(clip.tags(), ["kill"]);
    }

    #[test]
    fn a_tag_is_neither_repeated_nor_stored_as_whitespace() {
        let clip = manual_clip("rec-1", 0, 10)
            .with_tag("kill")
            .with_tag(" kill ")
            .with_tag("   ")
            .with_tag("ace");

        assert_eq!(clip.tags(), ["kill", "ace"]);
    }

    #[test]
    fn the_cost_of_a_clip_does_not_grow_with_the_material_it_covers() {
        // The claim "no video data" in one assertion: a clip of two seconds and
        // a clip of three hours of the same recording are the same size, so
        // nothing proportional to the footage was copied.
        let short = manual_clip("rec-1", 0, 2);
        let long = manual_clip("rec-1", 0, 3 * 60 * 60);

        let short_text = short.edit().write().expect("the document saves");
        let long_text = long.edit().write().expect("the document saves");

        assert_eq!(
            short_text.len() + 4,
            long_text.len(),
            "the only difference should be the four extra digits of the end time:\n{short_text}\n{long_text}"
        );
        assert!(
            long_text.len() < 512,
            "a three-hour clip is {} bytes",
            long_text.len()
        );
    }

    #[test]
    fn every_recording_a_clip_needs_is_reported() {
        let combined = VirtualClip::from_edit(
            EditDocument::new("Both")
                .with_source(Source::new(SourceId::new(0), RecordingId::new("rec-1")))
                .with_source(Source::new(SourceId::new(1), RecordingId::new("rec-2")))
                .with_segment(Segment::new(SourceId::new(0), span(0, 4)))
                .with_segment(Segment::new(SourceId::new(1), span(9, 12))),
            ClipOrigin::Manual,
        );

        assert_eq!(
            combined.source_recordings().collect::<Vec<_>>(),
            vec![&RecordingId::new("rec-1"), &RecordingId::new("rec-2")]
        );
        assert!(combined.depends_on(&RecordingId::new("rec-2")));
        assert!(!combined.depends_on(&RecordingId::new("rec-3")));
    }

    #[test]
    fn a_clip_is_unplayable_by_its_worst_source_rather_than_its_first() {
        let combined = VirtualClip::from_edit(
            EditDocument::new("Both")
                .with_source(Source::new(SourceId::new(0), RecordingId::new("rec-1")))
                .with_source(Source::new(SourceId::new(1), RecordingId::new("rec-2")))
                .with_segment(Segment::new(SourceId::new(0), span(0, 4))),
            ClipOrigin::Manual,
        );

        let availability = |recording: &RecordingId| match recording.as_str() {
            "rec-2" => SourceAvailability::Missing,
            _ => SourceAvailability::Present,
        };
        assert_eq!(combined.state(availability), ClipState::SourceMissing);

        let trashed = |recording: &RecordingId| match recording.as_str() {
            "rec-2" => SourceAvailability::InTrash,
            _ => SourceAvailability::Present,
        };
        assert_eq!(combined.state(trashed), ClipState::SourceInTrash);

        assert_eq!(
            combined.state(|_| SourceAvailability::Present),
            ClipState::Playable
        );
    }

    #[test]
    fn deleting_a_recording_nothing_needs_is_unremarkable() {
        let clips = [manual_clip("rec-1", 0, 10)];

        let deletion = SourceDeletion::examine(&RecordingId::new("rec-2"), &clips);
        assert_eq!(deletion, SourceDeletion::Unreferenced);
        assert_eq!(deletion.dependent_clips(), 0);
        assert!(deletion.automatic_cleanup_may_delete());
        assert!(!deletion.needs_confirmation());
    }

    #[test]
    fn a_recording_clips_depend_on_is_protected_from_automatic_deletion() {
        let clips = [
            manual_clip("rec-1", 0, 10),
            manual_clip("rec-1", 40, 50),
            manual_clip("rec-2", 0, 10),
        ];

        let deletion = SourceDeletion::examine(&RecordingId::new("rec-1"), &clips);
        assert_eq!(deletion, SourceDeletion::Referenced { clips: 2 });
        assert_eq!(deletion.dependent_clips(), 2);
        assert!(
            !deletion.automatic_cleanup_may_delete(),
            "automatic cleanup must not delete the only copy of a clip's material"
        );
        assert!(
            deletion.needs_confirmation(),
            "a person deleting it has to be told what stops playing"
        );
    }

    #[test]
    fn a_window_around_an_event_keeps_the_lead_and_the_trail() {
        let recorded = RecordedSpan::from_epoch(Duration::from_secs(1800));

        let window = window_around(
            at_second(600),
            Duration::from_secs(15),
            Duration::from_secs(10),
            &recorded,
        )
        .expect("the recording covers the kill");

        assert_eq!(window.start(), SourceTime::from_nanos(585 * SECOND));
        assert_eq!(window.end(), SourceTime::from_nanos(610 * SECOND));
    }

    #[test]
    fn a_window_is_clamped_to_what_the_file_holds_rather_than_lost() {
        let recorded = RecordedSpan::from_epoch(Duration::from_secs(20));

        let early = window_around(
            at_second(4),
            Duration::from_secs(15),
            Duration::from_secs(10),
            &recorded,
        )
        .expect("a kill four seconds in still deserves a clip");
        assert_eq!(early.start(), SourceTime::from_nanos(0));
        assert_eq!(early.end(), SourceTime::from_nanos(14 * SECOND));

        let late = window_around(
            at_second(18),
            Duration::from_secs(15),
            Duration::from_secs(10),
            &recorded,
        )
        .expect("a kill near the end still deserves a clip");
        assert_eq!(late.start(), SourceTime::from_nanos(3 * SECOND));
        assert_eq!(
            late.end(),
            SourceTime::from_nanos(20 * SECOND),
            "the clip ends where the recording does"
        );
    }

    #[test]
    fn a_window_the_file_does_not_reach_is_refused_rather_than_invented() {
        let recorded = RecordedSpan::from_epoch(Duration::from_secs(20));

        assert_eq!(
            window_around(
                at_second(-30),
                Duration::from_secs(5),
                Duration::from_secs(5),
                &recorded,
            ),
            None,
            "an event before the recording started has no clip in it"
        );
        assert_eq!(
            window_around(
                at_second(60),
                Duration::from_secs(5),
                Duration::from_secs(5),
                &recorded,
            ),
            None,
            "nor does one after it ended"
        );
        assert_eq!(
            window_around(at_second(10), Duration::ZERO, Duration::ZERO, &recorded,),
            None,
            "and a window of no length is not a clip"
        );
    }

    #[test]
    fn a_window_in_a_replay_clip_is_measured_from_the_file_not_the_session() {
        // A saved replay's file starts partway down the recording's timeline
        // (`clipped_events::RecordedSpan`): its first packet is a keyframe at
        // 19:50 of a session, and a kill at 20:00 is ten seconds into the file.
        // A subtraction that used the event's media time directly would place
        // this window twenty minutes into a thirty-second file.
        let recorded = RecordedSpan::new(at_second(1190), at_second(1220))
            .expect("the clip ends after it starts");

        let window = window_around(
            at_second(1200),
            Duration::from_secs(15),
            Duration::from_secs(10),
            &recorded,
        )
        .expect("the clip covers the kill");

        assert_eq!(
            window.start(),
            SourceTime::from_nanos(0),
            "fifteen seconds before the kill is before the file starts, so the clip opens there"
        );
        assert_eq!(
            window.end(),
            SourceTime::from_nanos(20 * SECOND),
            "and ends twenty seconds into the file, not 1210 seconds into it"
        );
    }

    #[test]
    fn an_origin_round_trips_through_its_stored_form() {
        let cases = [
            ClipOrigin::Manual,
            ClipOrigin::ReplayBuffer,
            ClipOrigin::Highlight(HighlightCause::of(&kill_at(600))),
        ];

        for origin in cases {
            let text = serde_json::to_string(&origin).expect("an origin serialises");
            assert_eq!(
                serde_json::from_str::<ClipOrigin>(&text).expect("it reads back"),
                origin,
                "{text}"
            );
        }

        assert_eq!(
            serde_json::to_string(&ClipOrigin::ReplayBuffer).expect("an origin serialises"),
            r#"{"origin":"replay-buffer"}"#
        );
        assert_eq!(
            serde_json::to_string(&ClipOrigin::Highlight(HighlightCause::of(&kill_at(600))))
                .expect("an origin serialises"),
            r#"{"origin":"highlight","kind":"kill","at":600000000000,"source":"acme-cs2"}"#
        );
    }
}
