//! Where an event sits on a session's timeline, how precisely that is known,
//! and how it becomes a position in a file.
//!
//! [`EventTime`] is a moment on the session's timeline -- one zero for the
//! whole sitting, however many files it wrote. [`EventTiming`] is
//! that moment together with the two honesty fields that make it usable: how
//! far out it might be, and how late it was heard. [`RecordedSpan`] is the part
//! of that timeline a file actually contains, and is what turns a moment into a
//! position a player can seek to — for a whole recording and for a replay clip
//! alike.
//!
//! `docs/plugin-api.md` argues the model; `docs/av-sync.md` owns the timeline
//! itself.

use core::fmt;
use core::time::Duration;

use serde::{Deserialize, Serialize};

/// A moment on **a session's** timeline, in nanoseconds from its start.
///
/// This is the same quantity as `clipped_capture::MediaTime` and in the same
/// units: signed nanoseconds from the timestamp of a first kept video frame.
/// [`from_media_nanos`](Self::from_media_nanos) is where a reading enters this
/// crate.
///
/// # One zero per session, not per file — and what that means for `MediaTime`
///
/// A session can write several files: a window destroyed and recreated, a game
/// restarted inside the grace period (`docs/sessions.md`). **They share this
/// timeline rather than each restarting at zero.** The second recording
/// occupies a span beginning at a positive offset, and an event heard during it
/// is stamped from the same origin as one heard during the first.
///
/// That is the property that makes [`RecordedSpan`]-based placement mean
/// anything: `clipped_library::events` sorts a session's segments on one axis
/// and asks which of them contains a moment, and both operations are nonsense
/// if every segment has its own zero. `docs/highlights.md` draws the model, and
/// [issue #338](https://github.com/wildware-uk/clipped/issues/338) requires
/// every event to be stamped through **one** `SessionTimeline`.
///
/// So the two zeros coincide only for a session's **first** recording. A
/// `CaptureClock` is started per recording (`docs/av-sync.md`), so the second
/// recording's `MediaTime` counts from *its own* first frame, and its readings
/// are not `EventTime`s. Converting one means adding where that recording
/// starts on this timeline — which is what its [`RecordedSpan`] holds.
/// [`from_media_nanos`](Self::from_media_nanos) takes a reading that is already
/// on the session's timeline; it does not rebase, and it cannot, because it is
/// handed a bare `i64`.
///
/// This section is explicit because the wording it replaced said "a recording's
/// timeline" and "the epoch a *recording's* `CaptureClock` was started at". An
/// implementer taking that literally would build a timeline per recording, and
/// every event of the second file would be placed — confidently, and without
/// any assertion failing anywhere — in the first.
///
/// # Why it is not `MediaTime`
///
/// Because `clipped-events` is layer 0 of the dependency table in `README.md`
/// and `clipped-capture` is layer 1, so this crate cannot name that type — the
/// same constraint that makes `clipped_audio::AudioTimestamp` a separate type
/// from `clipped_capture::CaptureTimestamp` (`docs/av-sync.md`, "Why audio
/// timestamps are a different type"). The duplication is deliberate and
/// bounded: one `i64` of nanoseconds, converted at one named pair of functions,
/// [`from_media_nanos`](Self::from_media_nanos) and
/// [`as_media_nanos`](Self::as_media_nanos).
///
/// # The shared `clipped-time` crate, and why this is not it
///
/// `docs/av-sync.md` names "a third crate needing the same vocabulary" as the
/// point at which a shared `clipped-time` crate earns its place, and
/// `clipped-events` is that third crate. The trigger has fired, and the answer
/// is **yes, extract one — as its own change**, not here.
///
/// It is a decision rather than a deferral, and the reasoning is on
/// [issue #253](https://github.com/wildware-uk/clipped/issues/253): the
/// extraction moves a type out of `clipped-capture` and rewrites every call
/// site in it and in `clipped-audio`, adds a crate to the layering table in
/// `README.md` and to `tests/integration/tests/workspace_layering.rs`, and
/// changes a public type name in two crates that six open issues are being
/// written against. None of that is verifiable by the tests of the event model,
/// and doing it inside this issue would mean a change nobody could review as
/// one thing (AGENTS.md section 39).
///
/// What is owed in the meantime is that the duplication stays exactly as
/// bounded as `docs/av-sync.md` says: one `i64` of nanoseconds, converted at
/// [`from_media_nanos`](Self::from_media_nanos) and
/// [`as_media_nanos`](Self::as_media_nanos) and nowhere else. #253 should land
/// before the next consumer copies it a fourth time — persistence
/// ([#71](https://github.com/wildware-uk/clipped/issues/71)) is the next one
/// due.
///
/// # Why it is signed
///
/// For the reason `MediaTime` is. The epoch is the session's first kept video
/// frame, and other sources were already running when it arrived, so a moment
/// before the epoch is normal rather than a fault — a plugin attached to a game
/// that was already running reports the match it joined, and a game event can
/// precede the first frame Clipped kept. An unsigned time would turn a second
/// of lead into eighteen billion seconds of lag, silently.
///
/// A negative time is not a position in any file, which is
/// [`RecordedSpan::position_of`]'s answer rather than something clamped here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventTime(i64);

impl EventTime {
    /// The start of the session: its first kept video frame.
    pub const ZERO: Self = Self(0);

    /// Takes a `clipped_capture::MediaTime::as_nanos` reading **on the
    /// session's timeline**.
    ///
    /// The name is the whole point: the caller is stating which timeline the
    /// number came from, and that claim is one line for a reviewer to check.
    /// Nanoseconds from any other zero — a performance counter's, a game's own
    /// match clock, a wall clock — produce an event in the wrong place.
    ///
    /// That includes a second recording's own `MediaTime`, which counts from
    /// *its* first frame rather than the session's. See the type documentation:
    /// such a reading must have the recording's start on the session timeline
    /// added to it first, and this function cannot do that for the caller
    /// because it is handed a bare `i64`.
    #[must_use]
    pub const fn from_media_nanos(nanos: i64) -> Self {
        Self(nanos)
    }

    /// The moment, in nanoseconds from the session's epoch.
    #[must_use]
    pub const fn as_media_nanos(self) -> i64 {
        self.0
    }

    /// This moment moved later by `offset`.
    ///
    /// Saturating: the inputs would have to be nearly three hundred years apart
    /// to reach the limit, and a value pinned at the end of the range is at
    /// least visibly wrong.
    #[must_use]
    pub fn saturating_add(self, offset: Duration) -> Self {
        Self(self.0.saturating_add(nanos_of(offset)))
    }

    /// This moment moved earlier by `offset`. Saturating, as above.
    #[must_use]
    pub fn saturating_sub(self, offset: Duration) -> Self {
        Self(self.0.saturating_sub(nanos_of(offset)))
    }

    /// How long after `earlier` this is, or [`None`] when it is not after it.
    ///
    /// A [`Duration`] cannot be negative, and an event ordering that has gone
    /// backwards is a fault to report rather than a length to take the modulus
    /// of.
    #[must_use]
    pub fn duration_since(self, earlier: Self) -> Option<Duration> {
        self.0
            .checked_sub(earlier.0)
            .and_then(|nanos| u64::try_from(nanos).ok())
            .map(Duration::from_nanos)
    }
}

impl fmt::Display for EventTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}ns", self.0)
    }
}

/// A [`Duration`] as a whole number of nanoseconds, saturating at [`u64::MAX`].
///
/// `Duration` counts nanoseconds in a `u128`, and 2^64 nanoseconds is 584
/// years, so this cannot narrow anything a recorder will ever see.
fn nanos_of(duration: Duration) -> i64 {
    i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX)
}

/// Nanoseconds on the wire, rather than `serde`'s `{"secs":…,"nanos":…}`.
///
/// A stored event is read by SQLite queries, by the TypeScript timeline and by
/// whoever opens the file to see what went wrong, and one integer is legible to
/// all three. It is also the same unit as every other time in this crate, so a
/// document cannot be misread by a factor of a thousand.
mod wire_nanos {
    use core::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(
        duration: &Duration,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Duration, D::Error> {
        u64::deserialize(deserializer).map(Duration::from_nanos)
    }
}

/// When an event happened, how well that is known, and how late it was heard.
///
/// # Why three numbers rather than one
///
/// A plugin does not observe a game; it observes a game *telling it something*,
/// and the two are not simultaneous. Counter-Strike's Game State Integration
/// posts when the state it watches changes, League's Live Client Data API is
/// polled, and a log-reading integration sees a line when the game got round to
/// flushing it. Each of those puts a gap between the moment being described and
/// the moment it can be timed, and a model with one timestamp per event has to
/// pick which of the two it means and be wrong about the other.
///
/// So an event carries the moment it *describes*, [`at`](Self::at) — that is
/// where it goes on the timeline, and it is the only field a timeline draws
/// with — plus:
///
/// - [`precision`](Self::precision), how far either side of `at` the true
///   moment may lie. A source that is polled every two seconds knows the
///   moment to within a second: it places `at` in the middle of the window it
///   is sure about and says so. Zero means the source timed the event itself.
/// - [`latency`](Self::latency), how much later than `at` the report arrived.
///   This is not a correction — the event does not move — it is what tells a
///   consumer whether reacting was possible at all. A replay buffer holds a
///   fixed window, and an event whose latency exceeds it describes a moment the
///   buffer has already evicted.
///
/// # Events arrive out of order, and late
///
/// Nothing places an event by its arrival. Two integrations reporting the same
/// second arrive in whatever order their transports allow, and a consumer that
/// appended them would draw them in that order. **Sort by [`at`](Self::at)**;
/// the timeline is a set of marks on a recording, not a log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventTiming {
    /// The moment the event describes.
    at: EventTime,
    /// How far either side of `at` the true moment may lie.
    ///
    /// Deliberately **not** `#[serde(default)]`: a missing precision would read
    /// as zero, which is the claim "this was timed exactly", and a document
    /// that never made that claim would start making it (AGENTS.md section 27).
    #[serde(with = "wire_nanos")]
    precision: Duration,
    /// How much later than `at` the report arrived. Zero — the default — means
    /// the report was not late, which is a weaker and much safer claim than
    /// zero precision.
    #[serde(
        with = "wire_nanos",
        default,
        skip_serializing_if = "Duration::is_zero"
    )]
    latency: Duration,
}

impl EventTiming {
    /// An event describing `at`, known to within `precision` either side.
    ///
    /// Both arguments are required because both are claims. Pass
    /// [`Duration::ZERO`] as `precision` only when the source timed the event
    /// itself on the recording's clock; a source that polls, or that reads a
    /// clock with coarser resolution than it pretends, knows its own window and
    /// should say so.
    #[must_use]
    pub const fn new(at: EventTime, precision: Duration) -> Self {
        Self {
            at,
            precision,
            latency: Duration::ZERO,
        }
    }

    /// Records that the report arrived `latency` after the moment it describes.
    #[must_use]
    pub const fn reported_late_by(mut self, latency: Duration) -> Self {
        self.latency = latency;
        self
    }

    /// The moment the event describes: where it is drawn, and what it sorts by.
    #[must_use]
    pub const fn at(&self) -> EventTime {
        self.at
    }

    /// How far either side of [`at`](Self::at) the true moment may lie.
    #[must_use]
    pub const fn precision(&self) -> Duration {
        self.precision
    }

    /// How much later than [`at`](Self::at) the report arrived.
    #[must_use]
    pub const fn latency(&self) -> Duration {
        self.latency
    }

    /// The earliest moment the event may describe.
    #[must_use]
    pub fn earliest(&self) -> EventTime {
        self.at.saturating_sub(self.precision)
    }

    /// The latest moment the event may describe.
    #[must_use]
    pub fn latest(&self) -> EventTime {
        self.at.saturating_add(self.precision)
    }

    /// When the report arrived, on the same timeline.
    #[must_use]
    pub fn observed(&self) -> EventTime {
        self.at.saturating_add(self.latency)
    }

    /// Where this event sits in `span`, or [`None`] when the span does not
    /// contain it. See [`RecordedSpan::position_of`].
    #[must_use]
    pub fn position_in(&self, span: &RecordedSpan) -> Option<Duration> {
        span.position_of(self.at)
    }
}

impl fmt::Display for EventTiming {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ±{:?}", self.at, self.precision)?;
        if !self.latency.is_zero() {
            write!(f, ", heard {:?} later", self.latency)?;
        }
        Ok(())
    }
}

/// The part of a session's timeline that one file contains.
///
/// # What it is for
///
/// An event's [`EventTime`] is a moment in the *session*, and a player seeks
/// by position in a *file*. For a recording written from its start those two
/// are the same number, and it is tempting to let them stay conflated — until
/// the replay buffer, where they are not. A saved replay clip is cut from a
/// buffer that has been running for however long the game has: its first packet
/// is a keyframe some way down the session's timeline, the file starts there,
/// and an event twenty minutes into the session is ten seconds into the clip.
/// The subtraction is one line, and this type exists so that it is written once
/// rather than at every call site that draws a marker.
///
/// # What the caller has to supply
///
/// `start` and `end` are the media times of the first and last packets the file
/// contains, which is what whoever wrote the file knows: a whole recording
/// starts at [`EventTime::ZERO`] by construction, and a replay clip starts at
/// the keyframe the `SegmentLease` began with, including the leading slack the
/// buffer could not trim (`docs/replay-buffer.md`).
///
/// # The assumption, stated
///
/// That the file's own zero is `start`. The muxer sets a file's origin from the
/// first packet it is given and clamps anything earlier to it
/// (`docs/muxing.md`), so this holds when the packet that opens the file is the
/// one whose media time is `start`. Audio captured before the first video frame
/// is the known exception, and trimming it at the epoch is
/// [issue #174](https://github.com/wildware-uk/clipped/issues/174); until that
/// lands, a recording's head carries an error of however early the audio thread
/// opened its endpoint, and positions computed here inherit it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RecordedSpan {
    start: EventTime,
    end: EventTime,
}

impl RecordedSpan {
    /// The span a file covers, from the media time of its first packet to that
    /// of its last.
    ///
    /// [`None`] when `end` is before `start`, which is not a file.
    #[must_use]
    pub fn new(start: EventTime, end: EventTime) -> Option<Self> {
        (end >= start).then_some(Self { start, end })
    }

    /// The span of a recording written from its start, lasting `duration`.
    #[must_use]
    pub fn from_epoch(duration: Duration) -> Self {
        Self {
            start: EventTime::ZERO,
            end: EventTime::ZERO.saturating_add(duration),
        }
    }

    /// The media time of the file's first packet.
    #[must_use]
    pub const fn start(&self) -> EventTime {
        self.start
    }

    /// The media time of the file's last packet.
    #[must_use]
    pub const fn end(&self) -> EventTime {
        self.end
    }

    /// How long the file runs for.
    #[must_use]
    pub fn duration(&self) -> Duration {
        self.end
            .duration_since(self.start)
            .unwrap_or(Duration::ZERO)
    }

    /// Whether `at` is inside the file, ends included.
    #[must_use]
    pub fn contains(&self, at: EventTime) -> bool {
        at >= self.start && at <= self.end
    }

    /// How far into the file `at` is, or [`None`] when the file does not
    /// contain that moment.
    ///
    /// [`None`] rather than a clamp, deliberately. A clip that does not cover
    /// an event has no place to draw it, and a marker pinned to the first frame
    /// of a clip that does not contain the kill it claims is worse than no
    /// marker: it is a lie the user cannot check (AGENTS.md section 27). The
    /// caller decides — a timeline omits it, a highlight rule looks for a
    /// different clip.
    #[must_use]
    pub fn position_of(&self, at: EventTime) -> Option<Duration> {
        self.contains(at).then(|| {
            at.duration_since(self.start)
                .expect("a contained moment is at or after the start")
        })
    }
}

impl fmt::Display for RecordedSpan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: Duration = Duration::from_secs(1);

    fn seconds(seconds: i64) -> EventTime {
        EventTime::from_media_nanos(seconds * 1_000_000_000)
    }

    #[test]
    fn a_moment_before_the_epoch_stays_negative() {
        // The epoch is the first video frame kept, and a plugin attached to a
        // game that was already running can describe something earlier. An
        // unsigned time would read that as eighteen billion seconds of lag.
        let before = EventTime::from_media_nanos(-250_000_000);
        assert!(before < EventTime::ZERO);
        assert_eq!(before.as_media_nanos(), -250_000_000);
    }

    #[test]
    fn a_time_is_the_media_nanoseconds_it_was_built_from() {
        assert_eq!(seconds(61).as_media_nanos(), 61_000_000_000);
        assert_eq!(EventTime::ZERO.as_media_nanos(), 0);
    }

    #[test]
    fn a_duration_backwards_is_none_rather_than_a_wrapped_length() {
        assert_eq!(seconds(9).duration_since(seconds(4)), Some(SECOND * 5));
        assert_eq!(seconds(4).duration_since(seconds(9)), None);
    }

    #[test]
    fn precision_brackets_the_moment_on_both_sides() {
        let timing = EventTiming::new(seconds(61), SECOND);
        assert_eq!(timing.earliest(), seconds(60));
        assert_eq!(timing.at(), seconds(61));
        assert_eq!(timing.latest(), seconds(62));
    }

    #[test]
    fn a_late_report_does_not_move_the_event() {
        // The whole point of the latency field: what a plugin heard late still
        // happened when it happened.
        let timing = EventTiming::new(seconds(61), Duration::ZERO).reported_late_by(SECOND * 3);
        assert_eq!(timing.at(), seconds(61));
        assert_eq!(timing.observed(), seconds(64));
        assert_eq!(timing.latency(), SECOND * 3);
    }

    #[test]
    fn an_event_in_a_whole_recording_is_at_its_own_media_time() {
        let recording = RecordedSpan::from_epoch(SECOND * 300);
        assert_eq!(recording.position_of(seconds(61)), Some(SECOND * 61));
        assert_eq!(recording.position_of(EventTime::ZERO), Some(Duration::ZERO));
    }

    #[test]
    fn an_event_in_a_replay_clip_is_rebased_onto_the_clip() {
        // The case the type exists for. A clip cut from a buffer twenty minutes
        // into a session starts at 1200s of recording time; a kill at 1215s is
        // fifteen seconds into the clip, not twelve hundred and fifteen.
        let clip = RecordedSpan::new(seconds(1200), seconds(1230)).expect("a valid span");
        assert_eq!(clip.position_of(seconds(1215)), Some(SECOND * 15));
        assert_eq!(clip.position_of(seconds(1200)), Some(Duration::ZERO));
        assert_eq!(clip.position_of(seconds(1230)), Some(SECOND * 30));
        assert_eq!(clip.duration(), SECOND * 30);
    }

    #[test]
    fn an_event_outside_a_clip_has_no_position_in_it() {
        let clip = RecordedSpan::new(seconds(1200), seconds(1230)).expect("a valid span");
        assert_eq!(clip.position_of(seconds(1199)), None);
        assert_eq!(clip.position_of(seconds(1231)), None);
        assert!(!clip.contains(seconds(1199)));
    }

    #[test]
    fn an_event_before_the_epoch_has_no_position_in_the_recording() {
        let recording = RecordedSpan::from_epoch(SECOND * 300);
        assert_eq!(
            recording.position_of(EventTime::from_media_nanos(-1)),
            None,
            "a moment before the first kept frame is not in the file"
        );
    }

    #[test]
    fn a_span_that_ends_before_it_starts_is_not_a_file() {
        assert!(RecordedSpan::new(seconds(30), seconds(29)).is_none());
        assert!(RecordedSpan::new(seconds(30), seconds(30)).is_some());
    }

    #[test]
    fn timing_can_place_itself_in_a_span() {
        let clip = RecordedSpan::new(seconds(1200), seconds(1230)).expect("a valid span");
        let timing = EventTiming::new(seconds(1215), SECOND);
        assert_eq!(timing.position_in(&clip), Some(SECOND * 15));
    }

    #[test]
    fn timing_survives_a_round_trip_through_json_in_nanoseconds() {
        let timing = EventTiming::new(seconds(61), SECOND).reported_late_by(SECOND * 2);
        let json = serde_json::to_string(&timing).expect("it serialises");
        assert_eq!(
            json,
            r#"{"at":61000000000,"precision":1000000000,"latency":2000000000}"#
        );
        assert_eq!(
            serde_json::from_str::<EventTiming>(&json).expect("and deserialises"),
            timing
        );
    }

    #[test]
    fn an_unstated_latency_is_absent_from_the_wire_and_reads_back_as_none() {
        let timing = EventTiming::new(seconds(61), Duration::ZERO);
        let json = serde_json::to_string(&timing).expect("it serialises");
        assert_eq!(json, r#"{"at":61000000000,"precision":0}"#);
        assert_eq!(
            serde_json::from_str::<EventTiming>(&json).expect("and deserialises"),
            timing
        );
    }

    #[test]
    fn a_document_without_a_precision_is_refused_rather_than_read_as_exact() {
        // Zero precision is the claim "timed exactly", and a document that
        // never made it must not start making it.
        let error = serde_json::from_str::<EventTiming>(r#"{"at":61000000000}"#)
            .expect_err("precision is required");
        assert!(
            error.to_string().contains("precision"),
            "the error should name the missing field: {error}"
        );
    }
}
