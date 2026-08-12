//! The event envelope: who said it, how sure they are, and what they attached.

use core::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

use crate::kind::{is_valid_segment, EventKind, MAX_IDENTIFIER_BYTES, RESERVED_NAMESPACE};
use crate::time::{EventTiming, RecordedSpan};

/// Something that happened in a game, placed on a recording's timeline.
///
/// The whole model in one sentence: **a kind, a moment, a source, a confidence
/// and an optional payload**, in terms that never name a game. A plugin
/// translates Counter-Strike's Game State Integration, League's Live Client
/// Data API or a log file into this, and nothing above it — the session, the
/// timeline, the highlight rules — knows which of them it is looking at
/// (AGENTS.md section 33).
///
/// # The envelope is frozen
///
/// These five fields, and the wire names they carry, are the compatibility
/// surface. A stored event outlives the build that wrote it, and the schema
/// policy in `docs/plugin-api.md` rests on the envelope staying readable
/// forever: an event whose *kind* a build has never met is still an event that
/// build can place on a timeline, attribute and draw. That only holds if the
/// envelope itself never changes shape, so adding a field here is a decision
/// with the same weight as changing a database schema (AGENTS.md section 43).
///
/// # Ordering
///
/// Events arrive in whatever order their transports allow, and late (see
/// [`EventTiming`]). Sort by `timing().at()`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameEvent {
    /// What happened.
    kind: EventKind,
    /// When it happened, how precisely that is known, and how late it was
    /// heard. Flattened, so the times sit at the top level of the document
    /// where a SQL query or a person reading the file can see them.
    #[serde(flatten)]
    timing: EventTiming,
    /// Who reported it.
    source: EventSource,
    /// How sure they are that it happened at all.
    confidence: Confidence,
    /// Whatever the source wanted to attach. Absent from the wire when empty,
    /// because most events have nothing to add and a `"data":{}` on every row
    /// is a cost paid forever.
    #[serde(default, skip_serializing_if = "EventPayload::is_empty")]
    data: EventPayload,
}

impl GameEvent {
    /// An event of `kind`, at `timing`, reported by `source` with `confidence`.
    ///
    /// All four are required because all four are claims a consumer acts on,
    /// and none of them has a safe default: see [`Confidence`] for why the
    /// certainty in particular is not one.
    #[must_use]
    pub fn new(
        kind: EventKind,
        timing: EventTiming,
        source: EventSource,
        confidence: Confidence,
    ) -> Self {
        Self {
            kind,
            timing,
            source,
            confidence,
            data: EventPayload::empty(),
        }
    }

    /// Attaches the source's own detail.
    #[must_use]
    pub fn with_data(mut self, data: EventPayload) -> Self {
        self.data = data;
        self
    }

    /// What happened.
    #[must_use]
    pub const fn kind(&self) -> &EventKind {
        &self.kind
    }

    /// When it happened.
    #[must_use]
    pub const fn timing(&self) -> &EventTiming {
        &self.timing
    }

    /// Who reported it.
    #[must_use]
    pub const fn source(&self) -> &EventSource {
        &self.source
    }

    /// How sure the source is that it happened.
    #[must_use]
    pub const fn confidence(&self) -> Confidence {
        self.confidence
    }

    /// Whatever the source attached.
    #[must_use]
    pub const fn data(&self) -> &EventPayload {
        &self.data
    }

    /// How far into `span` this event is, or [`None`] when the file does not
    /// cover the moment. See [`RecordedSpan::position_of`].
    #[must_use]
    pub fn position_in(&self, span: &RecordedSpan) -> Option<core::time::Duration> {
        self.timing.position_in(span)
    }
}

impl fmt::Display for GameEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at {} from {}", self.kind, self.timing, self.source)
    }
}

/// Who reported an event.
///
/// A plugin identifier, or [`APPLICATION`](Self::APPLICATION) for the parts of
/// Clipped that report events themselves — the process watcher knows when a
/// game started without any plugin's help.
///
/// It is one string on the wire rather than a tagged union, because the set of
/// things that can report an event grows and a reader must never fail over a
/// source it has not met. The syntax is [`CustomName`](crate::CustomName)'s
/// without the namespace requirement: lowercase ASCII letters, digits, `-`,
/// `_` and `.`, each dot-separated segment starting with a letter, at most
/// [`MAX_IDENTIFIER_BYTES`] bytes, and not [`APPLICATION`](Self::APPLICATION).
///
/// # Checked when produced, not when read
///
/// [`plugin`](Self::plugin) is the boundary a producer comes through, and it
/// enforces all of the above. Reading a stored document does **not**: a `source`
/// already in a user's library is kept exactly as it was written, because
/// refusing the document would delete an event to enforce a rule the event has
/// already broken (AGENTS.md section 56, `crates/events/src/schema.rs`).
/// [`is_well_formed`](Self::is_well_formed) is how a consumer asks whether what
/// it read obeys the syntax.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct EventSource(String);

impl EventSource {
    /// Clipped itself: the process watcher, the session, the user interface.
    ///
    /// The identifier is [`RESERVED_NAMESPACE`], which
    /// [`plugin`](Self::plugin) refuses and which no plugin may use as the
    /// namespace of a custom event, so an event this build produced claiming to
    /// come from the application did come from the application.
    ///
    /// It is a claim about what this build will *produce*, not about what it
    /// will read: a document is not evidence of who wrote it, and anything that
    /// can write to a user's library can write this string into it.
    pub const APPLICATION: &'static str = RESERVED_NAMESPACE;

    /// The source Clipped itself reports under.
    #[must_use]
    pub fn application() -> Self {
        Self(Self::APPLICATION.to_owned())
    }

    /// A plugin's identifier.
    ///
    /// # Errors
    ///
    /// [`InvalidSource`] when the identifier breaks the syntax above, naming
    /// the rule it broke.
    pub fn plugin(identifier: &str) -> Result<Self, InvalidSource> {
        if identifier == Self::APPLICATION {
            return Err(InvalidSource::Reserved {
                identifier: identifier.to_owned(),
            });
        }
        if identifier.is_empty()
            || identifier.len() > MAX_IDENTIFIER_BYTES
            || !identifier.split('.').all(is_valid_segment)
        {
            return Err(InvalidSource::Malformed {
                identifier: identifier.to_owned(),
            });
        }
        Ok(Self(identifier.to_owned()))
    }

    /// The identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this is Clipped itself rather than a plugin.
    #[must_use]
    pub fn is_application(&self) -> bool {
        self.0 == Self::APPLICATION
    }

    /// Whether the identifier obeys the syntax.
    ///
    /// True for everything [`plugin`](Self::plugin) and
    /// [`application`](Self::application) produce, so the only way to hold a
    /// source this is false for is to have read one from a stored document.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.is_application() || Self::plugin(&self.0).is_ok()
    }
}

/// Kept verbatim, without the checks [`EventSource::plugin`] applies.
///
/// See the type's documentation: the producer boundary is where the syntax is
/// enforced, and a stored event is past it.
impl<'de> Deserialize<'de> for EventSource {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self)
    }
}

impl fmt::Display for EventSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for EventSource {
    type Error = InvalidSource;

    fn try_from(identifier: String) -> Result<Self, Self::Error> {
        Self::plugin(&identifier)
    }
}

impl From<EventSource> for String {
    fn from(source: EventSource) -> Self {
        source.0
    }
}

/// Why an event source identifier was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidSource {
    /// It is empty, too long, or contains something the syntax does not allow.
    Malformed {
        /// The identifier offered.
        identifier: String,
    },
    /// It is [`EventSource::APPLICATION`], which only Clipped itself reports
    /// under.
    Reserved {
        /// The identifier offered.
        identifier: String,
    },
}

impl InvalidSource {
    /// The identifier that was refused.
    #[must_use]
    pub fn identifier(&self) -> &str {
        match self {
            Self::Malformed { identifier } | Self::Reserved { identifier } => identifier,
        }
    }
}

impl fmt::Display for InvalidSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed { identifier } => write!(
                f,
                "`{identifier}` is not a usable event source: an identifier is up to \
                 {MAX_IDENTIFIER_BYTES} bytes of lowercase ASCII letters, digits, `-`, `_` and \
                 `.`, and each dot-separated segment starts with a letter"
            ),
            Self::Reserved { identifier } => write!(
                f,
                "`{identifier}` is the identifier Clipped itself reports events under, and is \
                 reserved: a plugin's events must be traceable to the plugin"
            ),
        }
    }
}

impl core::error::Error for InvalidSource {}

/// How sure a source is that the event happened at all.
///
/// # Not the same question as precision
///
/// [`EventTiming::precision`] is how sure the source is about *when*.
/// Confidence is how sure it is *that*, and the two come apart in both
/// directions: Game State Integration says exactly what happened but is polled,
/// so it is certain and imprecise; a detector watching the screen for a kill
/// feed knows the frame it looked at but is guessing about the kill, so it is
/// precise and unsure. A highlight rule filters on this one and pads a clip
/// with the other, which is why they are separate fields rather than one
/// number.
///
/// # Why it has no default
///
/// Because the honest value is not the same for every source, and the
/// convenient value — 1.0 — is a claim. An integration reading an authoritative
/// feed reports [`CERTAIN`](Self::CERTAIN); a detector that computes a score
/// reports the score it computed. Nothing in between should be invented
/// (AGENTS.md section 27).
///
/// # Checked when produced, not when read
///
/// As with [`EventSource`], and for the same reason: [`new`](Self::new) is the
/// producer boundary and refuses anything outside 0 to 1, while a value already
/// in a user's library is read back as it was written. Refusing it would fail
/// the whole document — losing an event's kind, time and payload over one
/// number — to enforce a range the stored value has already left.
/// [`is_usable`](Self::is_usable) is how a consumer asks.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct Confidence(f32);

impl Confidence {
    /// The source knows: the game said so.
    pub const CERTAIN: Self = Self(1.0);

    /// A confidence between 0 and 1 inclusive.
    ///
    /// # Errors
    ///
    /// [`InvalidConfidence`] for anything outside that range, and for `NaN` —
    /// which would otherwise make every comparison against a highlight rule's
    /// threshold false, and the event silently invisible.
    pub fn new(confidence: f32) -> Result<Self, InvalidConfidence> {
        if (0.0..=1.0).contains(&confidence) {
            Ok(Self(confidence))
        } else {
            Err(InvalidConfidence { confidence })
        }
    }

    /// The value. Between 0 and 1 unless it was read from a stored document,
    /// which is what [`is_usable`](Self::is_usable) reports.
    #[must_use]
    pub const fn as_f32(self) -> f32 {
        self.0
    }

    /// Whether this is a confidence a rule can act on: a number between 0 and
    /// 1.
    ///
    /// True for everything [`new`](Self::new) produces. False only for a value
    /// read from a document that was not written by this model — where the
    /// honest answer is that the source's certainty is not known, and inventing
    /// one by clamping would put a number in a user's library that nothing ever
    /// claimed.
    #[must_use]
    pub fn is_usable(self) -> bool {
        (0.0..=1.0).contains(&self.0)
    }
}

/// Kept verbatim, without the range check [`Confidence::new`] applies.
///
/// `NaN` is the one exception, and it is unreachable through
/// [`schema::read`](crate::schema::read): JSON has no literal for it and
/// `serde_json::Number` cannot hold one. The arm is here so that "a
/// `Confidence` is never `NaN`" is a property of the type rather than of the
/// format — a `NaN` compares false against every threshold, which would make an
/// event silently invisible instead of merely uncertain, and that is the one
/// failure mode worse than refusing the value.
impl<'de> Deserialize<'de> for Confidence {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let confidence = f32::deserialize(deserializer)?;
        if confidence.is_nan() {
            return Err(serde::de::Error::custom(InvalidConfidence { confidence }));
        }
        Ok(Self(confidence))
    }
}

impl fmt::Display for Confidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.2}", self.0)
    }
}

impl TryFrom<f32> for Confidence {
    type Error = InvalidConfidence;

    fn try_from(confidence: f32) -> Result<Self, Self::Error> {
        Self::new(confidence)
    }
}

impl From<Confidence> for f32 {
    fn from(confidence: Confidence) -> Self {
        confidence.0
    }
}

/// A confidence outside 0..=1, or `NaN`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InvalidConfidence {
    /// The value offered.
    pub confidence: f32,
}

impl fmt::Display for InvalidConfidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "a confidence is between 0 and 1 inclusive, and {} is not",
            self.confidence
        )
    }
}

impl core::error::Error for InvalidConfidence {}

/// Whatever the source wanted to attach to an event.
///
/// A JSON object, not arbitrary JSON: an event's detail is a set of named
/// fields, and allowing a bare array or number at the top level would mean
/// every consumer handling a shape it can do nothing with.
///
/// # Nothing above the plugin interprets it
///
/// That is the point. `weapon`, `championKilled` and `hero_id` are the
/// vocabularies this model exists to keep out of the core, so they travel here
/// where the plugin that produced them and a rule written for that plugin can
/// read them, and everything else can ignore them. A consumer that finds itself
/// switching on a payload key to decide what an event *means* has moved a
/// game's protocol back into the core, and the answer is a new
/// [`EventKind`](crate::EventKind) variant.
///
/// # The size limit, and where it is enforced
///
/// [`MAX_PAYLOAD_BYTES`] bounds what a plugin can attach, because a plugin is
/// another program's output and every one of these is stored (issue #71) and
/// held in memory in between. The check is on
/// [`new`](Self::new) — the boundary where a producer's payload enters — and
/// deliberately **not** on deserialisation: a payload already on disk has
/// already been paid for, and refusing to read it back would destroy a user's
/// data to enforce a limit that has already been exceeded (AGENTS.md section
/// 56).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventPayload(Map<String, Value>);

/// The most a payload may carry, measured as its serialised JSON.
///
/// Generous for the detail a game event actually has — a weapon, a victim, a
/// score — and small enough that a plugin cannot use the event stream as a
/// transport for something else.
pub const MAX_PAYLOAD_BYTES: usize = 4096;

impl EventPayload {
    /// No detail.
    #[must_use]
    pub fn empty() -> Self {
        Self(Map::new())
    }

    /// A payload of named fields.
    ///
    /// # Errors
    ///
    /// [`PayloadTooLarge`] when the object serialises to more than
    /// [`MAX_PAYLOAD_BYTES`], naming the size so the plugin author knows by how
    /// much.
    pub fn new(fields: Map<String, Value>) -> Result<Self, PayloadTooLarge> {
        let bytes = serde_json::to_vec(&fields).map_or(usize::MAX, |json| json.len());
        if bytes > MAX_PAYLOAD_BYTES {
            return Err(PayloadTooLarge { bytes });
        }
        Ok(Self(fields))
    }

    /// The fields.
    #[must_use]
    pub fn fields(&self) -> &Map<String, Value> {
        &self.0
    }

    /// Whether there is any detail at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// A payload over [`MAX_PAYLOAD_BYTES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PayloadTooLarge {
    /// How large it was, serialised.
    pub bytes: usize,
}

impl fmt::Display for PayloadTooLarge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "an event payload is at most {MAX_PAYLOAD_BYTES} bytes of JSON, and this one is {}",
            self.bytes
        )
    }
}

impl core::error::Error for PayloadTooLarge {}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use serde::de::IntoDeserializer;
    use serde_json::json;

    use super::*;
    use crate::time::EventTime;

    fn timing() -> EventTiming {
        EventTiming::new(EventTime::from_media_nanos(61_500_000_000), Duration::ZERO)
    }

    fn source() -> EventSource {
        EventSource::plugin("counter-strike-2").expect("a valid identifier")
    }

    #[test]
    fn an_event_carries_its_kind_time_source_and_confidence() {
        let event = GameEvent::new(EventKind::Kill, timing(), source(), Confidence::CERTAIN);
        assert_eq!(event.kind(), &EventKind::Kill);
        assert_eq!(event.timing().at().as_media_nanos(), 61_500_000_000);
        assert_eq!(event.source().as_str(), "counter-strike-2");
        assert!((event.confidence().as_f32() - 1.0).abs() < f32::EPSILON);
        assert!(event.data().is_empty());
    }

    #[test]
    fn an_event_without_detail_carries_no_data_field() {
        let event = GameEvent::new(EventKind::Kill, timing(), source(), Confidence::CERTAIN);
        let json = serde_json::to_string(&event).expect("it serialises");
        assert!(
            !json.contains("data"),
            "an empty payload should not be written at all: {json}"
        );
    }

    #[test]
    fn a_games_own_vocabulary_travels_in_the_payload() {
        let fields = json!({"weapon": "ak47", "headshot": true, "victim": "someone"});
        let payload = EventPayload::new(fields.as_object().expect("an object literal").clone())
            .expect("well under the limit");
        let event = GameEvent::new(EventKind::Kill, timing(), source(), Confidence::CERTAIN)
            .with_data(payload);

        let json = serde_json::to_string(&event).expect("it serialises");
        let back: GameEvent = serde_json::from_str(&json).expect("it reads back");
        assert_eq!(back, event);
        assert_eq!(back.data().fields()["weapon"], json!("ak47"));
    }

    #[test]
    fn a_payload_over_the_limit_is_refused_at_the_producer_boundary() {
        let mut fields = Map::new();
        fields.insert("blob".to_owned(), json!("x".repeat(MAX_PAYLOAD_BYTES)));
        let error = EventPayload::new(fields).expect_err("over the limit");
        assert!(error.bytes > MAX_PAYLOAD_BYTES);
        assert!(
            error.to_string().contains(&error.bytes.to_string()),
            "the message should say how large it was"
        );
    }

    #[test]
    fn a_payload_already_stored_is_still_read_back_over_the_limit() {
        // The asymmetry is deliberate: refusing to read something already on
        // disk destroys a user's data to enforce a limit that was already
        // exceeded (AGENTS.md section 56).
        let oversize = json!({"blob": "x".repeat(MAX_PAYLOAD_BYTES)});
        let payload: EventPayload =
            serde_json::from_value(oversize).expect("a stored payload is readable");
        assert!(!payload.is_empty());
    }

    #[test]
    fn the_application_is_a_source_no_plugin_can_claim() {
        assert!(EventSource::application().is_application());
        assert!(!source().is_application());
        assert_eq!(EventSource::application().as_str(), "clipped");

        // The identifier is reserved at the producer boundary, so an event this
        // build produces that appears to come from the application did. It is
        // the same rule `CustomName` applies to the namespace of a custom event
        // name, applied to the other place a plugin could speak for the project.
        let refused = EventSource::plugin("clipped").expect_err("`clipped` is reserved");
        assert_eq!(
            refused,
            InvalidSource::Reserved {
                identifier: "clipped".to_owned()
            }
        );
        assert!(
            refused.to_string().contains("reserved"),
            "the message should say why: {refused}"
        );
        // A namespaced identifier under the reserved name is a different
        // string, and is nobody's impersonation of the application.
        assert!(EventSource::plugin("clipped-cs2").is_ok());
        assert!(!EventSource::plugin("clipped.cs2")
            .expect("namespaced, and not the reserved identifier")
            .is_application());
    }

    #[test]
    fn a_source_read_from_a_document_is_kept_rather_than_refused() {
        // The read path does not re-run the producer's checks: an event already
        // in a library is past that boundary, and failing the document would
        // delete a mark from a timeline to enforce a rule it has already broken
        // (AGENTS.md section 56).
        let stored: EventSource =
            serde_json::from_str(r#""Counter Strike""#).expect("a stored source is readable");
        assert_eq!(stored.as_str(), "Counter Strike");
        assert!(!stored.is_well_formed());
        assert!(source().is_well_formed());
        assert!(EventSource::application().is_well_formed());
        // And it goes back out as it came in.
        assert_eq!(
            serde_json::to_string(&stored).expect("it serialises"),
            r#""Counter Strike""#
        );
    }

    #[test]
    fn a_source_identifier_is_checked() {
        assert!(EventSource::plugin("counter-strike-2").is_ok());
        assert!(EventSource::plugin("acme.cs2").is_ok());
        for refused in ["", "Counter Strike", "9acme", "acme..cs2"] {
            assert_eq!(
                EventSource::plugin(refused),
                Err(InvalidSource::Malformed {
                    identifier: refused.to_owned()
                }),
                "`{refused}` should be refused as malformed"
            );
        }
        assert!(EventSource::plugin(&"a".repeat(MAX_IDENTIFIER_BYTES + 1)).is_err());
    }

    #[test]
    fn a_confidence_outside_the_range_is_refused_including_nan() {
        assert!(Confidence::new(0.0).is_ok());
        assert!(Confidence::new(1.0).is_ok());
        assert!(Confidence::new(1.1).is_err());
        assert!(Confidence::new(-0.1).is_err());
        assert!(
            Confidence::new(f32::NAN).is_err(),
            "a NaN confidence compares false against every threshold, which makes an event \
             invisible rather than uncertain"
        );
    }

    #[test]
    fn a_confidence_from_a_document_is_read_rather_than_refused() {
        assert!(serde_json::from_str::<Confidence>("0.75")
            .expect("a normal confidence")
            .is_usable());

        // The producer boundary refuses this; the read path does not, because
        // failing here fails the whole document and loses the event's kind,
        // time and payload over one number (AGENTS.md section 56).
        assert!(Confidence::new(1.5).is_err());
        let stored: Confidence =
            serde_json::from_str("1.5").expect("a stored confidence is readable");
        assert!(
            !stored.is_usable(),
            "it is readable, and a rule is told not to act on it"
        );
        assert!(
            (stored.as_f32() - 1.5).abs() < f32::EPSILON,
            "kept verbatim"
        );
        assert_eq!(
            serde_json::to_string(&stored).expect("it serialises"),
            "1.5",
            "and written back as it was stored"
        );
    }

    #[test]
    fn a_nan_confidence_cannot_arrive_from_a_document_at_all() {
        // JSON has no literal for it, and `serde_json::Number` cannot hold one,
        // so the read path never has to decide what a `NaN` certainty means.
        assert!(serde_json::from_str::<Confidence>("NaN").is_err());
        assert!(serde_json::Number::from_f64(f64::NAN).is_none());

        // The type refuses it regardless, so that the property does not rest on
        // the format: a `NaN` compares false against every threshold, which is
        // an invisible event rather than an uncertain one. Deserialised from a
        // `f32` directly, because no JSON document can pose the question.
        let nan = IntoDeserializer::<serde::de::value::Error>::into_deserializer(f32::NAN);
        assert!(Confidence::deserialize(nan).is_err());
        assert!(Confidence::new(f32::NAN).is_err());
    }
}
