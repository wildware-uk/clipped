//! Where a session's game events sit in the files that session produced.
//!
//! A plugin reports that something happened at a moment on the recording's
//! timeline ([`clipped_events`], `docs/plugin-api.md`). A timeline draws a
//! position in a *file*. For one recording written from its start those are the
//! same number, and for everything else they are not — which is the whole of
//! [issue #71](https://github.com/wildware-uk/clipped/issues/71)'s "correct
//! positioning when a session spans multiple recording segments or the
//! recording started after the game". This module is the one place that
//! conversion is written.
//!
//! # Why here
//!
//! `clipped-events` is layer 0 and knows nothing about a library;
//! `clipped-edit` is layer 0 and may not name it. `clipped-library` is the
//! lowest crate that can see both, which is the same argument
//! `docs/highlights.md` makes for [`crate::virtual_clip`] living here, and
//! [`window_around`](crate::virtual_clip::window_around) is the companion of
//! this module: that one turns a moment into a *range* for a clip to be cut
//! from, this one turns a moment into a *point* for a timeline to draw.
//!
//! # The model
//!
//! One session is a list of [`RecordedSegment`]s — a recording, and the part of
//! the session's timeline that its file contains ([`RecordedSpan`]). A moment
//! is either inside exactly one of them, or inside none:
//!
//! ```text
//!   session timeline   ├──────────────────────────────────────────────┤
//!   recordings              ├── #1 ───┤        ├───── #2 ─────┤
//!   events               ✕      ✓          ✕          ✓             ✕
//!                        │      │          │          │             │
//!            before the first   │   between recordings│    after the last
//!                          in #1, 8s in         in #2, 21s in
//! ```
//!
//! Every one of those five is answered, and none of them is an error. A game
//! that was already running when Clipped attached, a recording that started
//! after the match did, a window that was destroyed and recreated mid-session,
//! and a replay-buffer-only session that has written no file at all are all
//! ordinary (`docs/sessions.md`, `docs/replay-buffer.md`).
//!
//! # Nothing is dropped, and nothing is invented
//!
//! [`SessionRecordings::marks`] answers with **every** event it was given, in
//! the order things happened, each carrying where it belongs or why it belongs
//! nowhere. Two rules, both AGENTS.md section 27:
//!
//! - An event whose [kind](clipped_events::EventKind) this build has never met
//!   places exactly as a `kill` does. Placement is arithmetic on a time; it
//!   does not read the kind, so a newer build's vocabulary cannot make marks
//!   disappear from a user's timeline. A consumer that filtered on a known list
//!   would show fewer marks than the recorder found and say nothing about it.
//! - An event no file covers is reported as such rather than pinned to the
//!   nearest frame. A marker at the start of a clip that does not contain the
//!   kill it claims is a lie the user cannot check, which is why
//!   [`RecordedSpan::position_of`] answers [`None`] rather than clamping.
//!
//! # Replay-buffer-only sessions
//!
//! A session with the replay buffer running and nothing being written to disk
//! produces no [`RecordedSegment`] at all, so every event it hears places as
//! [`NotRecorded::NothingRecorded`]. The events are still the session's — they
//! are what a saved replay is offered *for* — and the moment the hotkey writes
//! a clip, that clip is a segment whose span starts at the keyframe the buffer
//! began with, and the events inside it place in it. Nothing about this module
//! changes between the two cases; the list of segments does.
//!
//! # Threading and cost
//!
//! Plain data with no interior mutability, and no I/O of any kind: placing an
//! event is a comparison and a subtraction. Placing every event of a session is
//! one sort and one scan, and it may run wherever the caller likes.

use core::time::Duration;

use clipped_edit::RecordingId;
use clipped_events::{EventTime, GameEvent, RecordedSpan};

/// One file a session produced, and the part of the session's timeline it
/// contains.
///
/// `recorded` is on the same timeline as an event's
/// [`EventTime`](clipped_events::EventTime): nanoseconds from the recording
/// epoch the events were placed against. Whoever wrote the file knows it —
/// [`RecordedSpan::from_epoch`] for a recording written from the start of that
/// timeline, and the keyframe a `SegmentLease` began with for a replay clip
/// (`docs/replay-buffer.md`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedSegment {
    recording: RecordingId,
    recorded: RecordedSpan,
}

impl RecordedSegment {
    /// The segment `recording`'s file covers.
    #[must_use]
    pub const fn new(recording: RecordingId, recorded: RecordedSpan) -> Self {
        Self {
            recording,
            recorded,
        }
    }

    /// Which recording it is.
    #[must_use]
    pub const fn recording(&self) -> &RecordingId {
        &self.recording
    }

    /// The part of the session's timeline its file contains.
    #[must_use]
    pub const fn recorded(&self) -> &RecordedSpan {
        &self.recorded
    }
}

/// The files one session produced, in the order they were recorded.
///
/// Built once and asked about many events, because the sort is the only part
/// that is not constant time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionRecordings {
    segments: Vec<RecordedSegment>,
}

impl SessionRecordings {
    /// A session's recordings, however the caller happened to hold them.
    ///
    /// Sorted here by where each file starts, so that placing an event is a
    /// walk in time order and the answer does not depend on the order rows came
    /// back from a query.
    #[must_use]
    pub fn of(segments: impl IntoIterator<Item = RecordedSegment>) -> Self {
        let mut segments: Vec<RecordedSegment> = segments.into_iter().collect();
        // Stable, so that two segments claiming the same start — which should
        // not happen, and which the caller can see in `segments()` — keep the
        // order they were given rather than swapping between runs.
        segments.sort_by_key(|segment| segment.recorded.start());
        Self { segments }
    }

    /// A session that wrote nothing: the replay buffer running on its own.
    #[must_use]
    pub fn none_recorded() -> Self {
        Self {
            segments: Vec::new(),
        }
    }

    /// The segments, earliest first.
    #[must_use]
    pub fn segments(&self) -> &[RecordedSegment] {
        &self.segments
    }

    /// Whether the session wrote any file at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Where `at` falls among this session's recordings.
    ///
    /// The first segment that contains the moment wins. Spans are not expected
    /// to overlap — a session records one thing at a time — but two that touch
    /// at an instant both contain it ([`RecordedSpan::contains`] includes both
    /// ends), and taking the earlier one makes the answer depend on the
    /// recordings rather than on the order they were listed in.
    #[must_use]
    pub fn place(&self, at: EventTime) -> Placement {
        if self.segments.is_empty() {
            return Placement::NotRecorded(NotRecorded::NothingRecorded);
        }

        let mut passed = false;
        for segment in &self.segments {
            if let Some(position) = segment.recorded.position_of(at) {
                return Placement::In {
                    recording: segment.recording.clone(),
                    at: position,
                };
            }
            if at < segment.recorded.start() {
                // Earlier than a segment that has not been passed yet: the
                // moment is before everything from here on.
                return Placement::NotRecorded(if passed {
                    NotRecorded::BetweenRecordings
                } else {
                    NotRecorded::BeforeTheFirstRecording
                });
            }
            passed = true;
        }

        Placement::NotRecorded(NotRecorded::AfterTheLastRecording)
    }

    /// Every event, in the order things happened, with where each one belongs.
    ///
    /// Sorted by [`EventTiming::at`](clipped_events::EventTiming::at) and never
    /// by arrival: events reach a session late and out of order, and a timeline
    /// is a set of marks on a recording rather than a log
    /// (`crates/events/src/time.rs`). The sort is stable, so two events
    /// describing the same instant keep the order they were reported in.
    ///
    /// Nothing is filtered. An event no recording covers is in the answer with
    /// a [`NotRecorded`] reason, because a caller that wants to say "four of
    /// these five are on this file, and the fifth happened before it started"
    /// cannot say it from a list the fifth was quietly removed from.
    #[must_use]
    pub fn marks<'a>(&self, events: impl IntoIterator<Item = &'a GameEvent>) -> Vec<Mark<'a>> {
        let mut marks: Vec<Mark<'a>> = events
            .into_iter()
            .map(|event| Mark {
                placement: self.place(event.timing().at()),
                event,
            })
            .collect();
        marks.sort_by_key(|mark| mark.event.timing().at());
        marks
    }
}

/// One event, and where it belongs.
#[derive(Debug, Clone, PartialEq)]
pub struct Mark<'a> {
    event: &'a GameEvent,
    placement: Placement,
}

impl<'a> Mark<'a> {
    /// The event.
    #[must_use]
    pub const fn event(&self) -> &'a GameEvent {
        self.event
    }

    /// Where it belongs.
    #[must_use]
    pub const fn placement(&self) -> &Placement {
        &self.placement
    }

    /// How far into `recording`'s file this mark is, or [`None`] when it is not
    /// in that file.
    ///
    /// What one timeline draws: the recording asks, and everything that belongs
    /// to a different file or to no file at all answers [`None`].
    #[must_use]
    pub fn on(&self, recording: &RecordingId) -> Option<Duration> {
        match &self.placement {
            Placement::In {
                recording: found,
                at,
            } if found == recording => Some(*at),
            Placement::In { .. } | Placement::NotRecorded(_) => None,
        }
    }
}

/// Where a moment on a session's timeline is, in the files that session wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// Inside one recording, this far into its file.
    In {
        /// Which file.
        recording: RecordingId,
        /// How far into it, which is what a player seeks to and a timeline
        /// draws at.
        at: Duration,
    },
    /// No file covers the moment. The event is still the session's.
    NotRecorded(NotRecorded),
}

impl Placement {
    /// Whether a file covers the moment.
    #[must_use]
    pub const fn is_recorded(&self) -> bool {
        matches!(self, Self::In { .. })
    }
}

/// Why no file covers a moment.
///
/// Four cases rather than one boolean, because they are four different things
/// to tell somebody and only one of them is the ordinary "it happened outside
/// the recording" (AGENTS.md section 45). A session that recorded nothing has
/// not lost the event; a moment between two recordings is a window that was
/// destroyed and recreated; a moment before the first is the recorder having
/// started after the game did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotRecorded {
    /// The session wrote no file at all — the replay buffer on its own, with
    /// nothing saved yet.
    NothingRecorded,
    /// Before the first recording began. The recorder attached to a game that
    /// was already running, or the event describes something before the first
    /// frame it kept.
    BeforeTheFirstRecording,
    /// In a gap between two of the session's recordings.
    BetweenRecordings,
    /// After the last recording ended.
    AfterTheLastRecording,
}

impl core::fmt::Display for NotRecorded {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::NothingRecorded => "nothing was being recorded",
            Self::BeforeTheFirstRecording => "before the recording started",
            Self::BetweenRecordings => "between two recordings",
            Self::AfterTheLastRecording => "after the recording ended",
        })
    }
}

#[cfg(test)]
mod tests {
    use clipped_events::{Confidence, CustomName, EventKind, EventSource, EventTiming};

    use super::*;

    const SECOND: Duration = Duration::from_secs(1);

    fn seconds(seconds: i64) -> EventTime {
        EventTime::from_media_nanos(seconds * 1_000_000_000)
    }

    fn recording(name: &str) -> RecordingId {
        RecordingId::new(name)
    }

    fn segment(name: &str, from: i64, to: i64) -> RecordedSegment {
        RecordedSegment::new(
            recording(name),
            RecordedSpan::new(seconds(from), seconds(to)).expect("a valid span"),
        )
    }

    fn event(kind: EventKind, at: i64) -> GameEvent {
        GameEvent::new(
            kind,
            EventTiming::new(seconds(at), Duration::ZERO),
            EventSource::plugin("counter-strike-2").expect("a valid identifier"),
            Confidence::CERTAIN,
        )
    }

    fn kill(at: i64) -> GameEvent {
        event(EventKind::Kill, at)
    }

    #[test]
    fn an_event_in_a_recording_written_from_the_start_is_at_its_own_time() {
        let session = SessionRecordings::of([RecordedSegment::new(
            recording("1"),
            RecordedSpan::from_epoch(SECOND * 300),
        )]);

        assert_eq!(
            session.place(seconds(61)),
            Placement::In {
                recording: recording("1"),
                at: SECOND * 61,
            }
        );
    }

    #[test]
    fn an_event_in_the_second_of_two_recordings_is_measured_from_that_file() {
        // The case the issue names. A window destroyed and recreated gives one
        // session two files, and the second one's zero is not the session's:
        // a kill at 250s of the session is 50s into a file that opened at 200s.
        let session = SessionRecordings::of([segment("1", 0, 120), segment("2", 200, 400)]);

        assert_eq!(
            session.place(seconds(250)),
            Placement::In {
                recording: recording("2"),
                at: SECOND * 50,
            }
        );
        assert_eq!(
            session.place(seconds(30)),
            Placement::In {
                recording: recording("1"),
                at: SECOND * 30,
            }
        );
    }

    #[test]
    fn the_order_recordings_were_listed_in_does_not_change_the_answer() {
        let forwards = SessionRecordings::of([segment("1", 0, 120), segment("2", 200, 400)]);
        let backwards = SessionRecordings::of([segment("2", 200, 400), segment("1", 0, 120)]);

        assert_eq!(forwards, backwards);
        assert_eq!(forwards.place(seconds(250)), backwards.place(seconds(250)));
        assert_eq!(
            backwards.segments()[0].recording(),
            &recording("1"),
            "the segments are held earliest first whatever order they arrived in"
        );
    }

    #[test]
    fn an_event_before_the_first_recording_is_told_apart_from_one_after_the_last() {
        // "The recording started after the game" is the first of these, and it
        // is the ordinary case for a plugin attached to a game that was already
        // running: `EventTime` is signed for exactly this reason.
        let session = SessionRecordings::of([segment("1", 0, 120), segment("2", 200, 400)]);

        assert_eq!(
            session.place(EventTime::from_media_nanos(-1)),
            Placement::NotRecorded(NotRecorded::BeforeTheFirstRecording)
        );
        assert_eq!(
            session.place(seconds(160)),
            Placement::NotRecorded(NotRecorded::BetweenRecordings)
        );
        assert_eq!(
            session.place(seconds(401)),
            Placement::NotRecorded(NotRecorded::AfterTheLastRecording)
        );
        for reason in [
            NotRecorded::NothingRecorded,
            NotRecorded::BeforeTheFirstRecording,
            NotRecorded::BetweenRecordings,
            NotRecorded::AfterTheLastRecording,
        ] {
            assert!(
                !reason.to_string().is_empty(),
                "every reason has to be sayable to a person"
            );
        }
    }

    #[test]
    fn a_gap_that_starts_where_the_recording_before_it_ended_has_no_moments_in_it() {
        // Both ends of a span are inside it, so a moment on the boundary is in
        // the file rather than in the gap after it.
        let session = SessionRecordings::of([segment("1", 0, 120), segment("2", 120, 200)]);

        assert_eq!(
            session.place(seconds(120)),
            Placement::In {
                recording: recording("1"),
                at: SECOND * 120,
            },
            "the earlier file wins the instant they share, so the answer does not depend on \
             which was listed first"
        );
    }

    #[test]
    fn a_session_that_recorded_nothing_says_so_rather_than_reporting_a_gap() {
        // A replay-buffer-only session. The events are still the session's;
        // there is simply no file to draw them on until one is saved.
        let session = SessionRecordings::none_recorded();

        assert!(session.is_empty());
        assert_eq!(
            session.place(seconds(61)),
            Placement::NotRecorded(NotRecorded::NothingRecorded)
        );
    }

    #[test]
    fn a_saved_replay_places_the_events_it_covers_and_only_those() {
        // The moment the hotkey is pressed, the buffer writes a file whose
        // first packet is a keyframe twenty minutes into the session. The same
        // events that placed nowhere a second ago now place in it, rebased onto
        // the clip — which is `RecordedSpan`'s whole reason for existing.
        let session = SessionRecordings::of([segment("replay", 1200, 1230)]);

        assert_eq!(
            session.place(seconds(1215)),
            Placement::In {
                recording: recording("replay"),
                at: SECOND * 15,
            }
        );
        assert_eq!(
            session.place(seconds(600)),
            Placement::NotRecorded(NotRecorded::BeforeTheFirstRecording)
        );
    }

    #[test]
    fn a_kind_this_build_has_never_met_places_exactly_as_a_kill_does() {
        // The rule the issue is explicit about: a mark this build cannot name
        // must still be a mark, or a user sees fewer of them than the recorder
        // found and nothing says so (AGENTS.md section 27).
        //
        // Asked of `marks` rather than of `place`, deliberately: `place` takes
        // a time and cannot see a kind, so a version of this test written
        // against it could not fail however hard the kinds were filtered.
        let session = SessionRecordings::of([segment("1", 0, 300)]);
        let events = [
            event(EventKind::Kill, 61),
            event(EventKind::Unrecognised("objective_taken".to_owned()), 61),
            event(
                EventKind::Custom(CustomName::new("acme-cs2.flag_captured").expect("valid")),
                61,
            ),
        ];
        let expected = Placement::In {
            recording: recording("1"),
            at: SECOND * 61,
        };

        let marks = session.marks(&events);

        assert_eq!(
            marks.len(),
            events.len(),
            "every kind must reach the timeline, including the two this build has no name for"
        );
        for mark in &marks {
            assert_eq!(
                mark.placement(),
                &expected,
                "{} placed differently from a kill",
                mark.event().kind().as_str()
            );
        }
    }

    #[test]
    fn the_marks_of_a_session_are_every_event_it_was_given_in_time_order() {
        let session = SessionRecordings::of([segment("1", 0, 120), segment("2", 200, 400)]);
        // Deliberately out of order and deliberately including two the files do
        // not cover: events arrive late and out of order, and a timeline draws
        // what happened rather than what was heard.
        let events = [kill(250), kill(160), kill(30), kill(-5)];

        let marks = session.marks(&events);

        assert_eq!(
            marks
                .iter()
                .map(|mark| mark.event().timing().at())
                .collect::<Vec<_>>(),
            vec![seconds(-5), seconds(30), seconds(160), seconds(250)],
        );
        assert_eq!(
            marks.len(),
            events.len(),
            "an event no file covers must still be in the answer"
        );
        assert_eq!(marks[1].on(&recording("1")), Some(SECOND * 30));
        assert_eq!(
            marks[1].on(&recording("2")),
            None,
            "a mark belongs to one file, and the other must not draw it"
        );
        assert_eq!(marks[3].on(&recording("2")), Some(SECOND * 50));
        assert_eq!(marks[0].on(&recording("1")), None);
        assert!(!marks[2].placement().is_recorded());
        assert!(marks[3].placement().is_recorded());
    }

    #[test]
    fn two_events_at_the_same_moment_keep_the_order_they_were_reported_in() {
        // Assists and kills land on the same instant constantly, and a sort
        // that shuffled them would make a timeline redraw itself differently on
        // every load for no reason anybody could see.
        let session = SessionRecordings::of([segment("1", 0, 300)]);
        let events = [event(EventKind::Kill, 61), event(EventKind::Assist, 61)];

        let marks = session.marks(&events);

        assert_eq!(
            marks
                .iter()
                .map(|mark| mark.event().kind().as_str())
                .collect::<Vec<_>>(),
            vec!["kill", "assist"]
        );
    }

    #[test]
    fn a_recording_with_no_events_has_no_marks() {
        // The other half of "no invented data": nothing is produced for a
        // recording nothing happened in.
        let session = SessionRecordings::of([segment("1", 0, 300)]);

        assert!(session.marks(&[]).is_empty());
    }
}
