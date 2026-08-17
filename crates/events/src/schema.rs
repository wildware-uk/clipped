//! The stored form of an event, and the rules that keep it readable.
//!
//! An event outlives the build that wrote it. It is written to a database while
//! a game is running (issue #71), read back by a timeline months later, and
//! read again by highlight rules that did not exist when it was stored. This
//! module owns the one document those three agree on, and the policy that says
//! what a build may do to it.
//!
//! # The policy
//!
//! It is the compatibility policy of `docs/ipc.md`, applied to data at rest
//! rather than data on a wire, with one difference that follows from the change
//! of medium: **nothing here ever refuses to read a stored event**, because a
//! refusal at rest destroys something the user cannot get back (AGENTS.md
//! section 56), whereas a refusal on a wire only ends a connection that can be
//! made again.
//!
//! - **The envelope is frozen.** `schema`, `kind`, `at`, `precision`,
//!   `latency`, `source`, `confidence` and `data`: these names and their
//!   meanings do not change, ever. Everything below rests on it.
//! - **An unknown field is ignored, and kept.** Adding one to an event costs no
//!   version bump. Ignoring it is what `serde` does by default; *keeping* it is
//!   not, and had to be implemented, because a build that reads a library and
//!   writes it back — re-indexing it, moving it, exporting it — would otherwise
//!   delete every field it had not learned. [`ReadEvent::to_json`] is the write
//!   path that does not (AGENTS.md section 56).
//! - **An unknown `kind` is kept**, as [`EventKind::Unrecognised`],
//!   [`EventKind::Custom`] or [`EventKind::UserLabelled`] depending on whether
//!   it is namespaced, prefixed as a user label, or neither. Adding a kind
//!   costs no version bump either — and this is the part that has to be
//!   implemented rather than inherited, because a tagged union whose tag a
//!   build does not recognise fails the whole document it is part of. An event
//!   whose kind means nothing to this build is still a mark it can place,
//!   attribute and draw.
//! - **A document from a schema this build does not know is read, and
//!   flagged.** In practice that is a newer one. The envelope is
//!   frozen, so its times and its source are still exactly what they say they
//!   are; what a bump can change is what lies *inside* — the meaning of a
//!   payload, or of a kind. [`ReadEvent::schema`] says which build wrote it so
//!   that a consumer choosing to interpret [`GameEvent::data`] knows whether it
//!   may — and writing it back keeps that number, because relabelling a
//!   version-9 payload as version 1 would be this build asserting a meaning it
//!   never read.
//! - **A value the envelope's types would refuse is still read.** A `source`
//!   that breaks the identifier syntax, or a `confidence` outside 0 to 1, is
//!   kept as it was stored rather than failing the document: the producer
//!   boundary is where those rules are enforced, and enforcing them again on
//!   the way out of a database only destroys the event
//!   ([`EventSource::is_well_formed`](crate::EventSource::is_well_formed) and
//!   [`Confidence::is_usable`](crate::Confidence::is_usable) are how a consumer
//!   asks).
//! - **A document from an older schema is upgraded** by [`upgrade`], which is
//!   an exhaustive `match` over [`SchemaVersion`]: adding a version stops this
//!   module compiling until the step that migrates from the previous one is
//!   written.
//!
//! # When the version does change
//!
//! Adding a kind, a field, a source or a custom name does **not** bump
//! [`SchemaVersion`]. Removing one, renaming one, or changing what one means
//! does — and since the envelope is frozen, in practice a bump can only be
//! about the interpretation of a payload or of an existing kind.

use core::fmt;

use serde::Serialize;
use serde_json::{Map, Value};

use crate::event::GameEvent;

/// The version a stored event is written under.
///
/// A closed enumeration rather than a bare integer, so that
/// [`upgrade`]'s `match` is exhaustive and a new version cannot be added
/// without somebody being made to write, or explicitly decline to write, the
/// step that migrates the documents already on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SchemaVersion {
    /// The first schema: the model as issue #68 defined it.
    V1,
}

impl SchemaVersion {
    /// What this build writes.
    pub const CURRENT: Self = Self::V1;

    /// Every version this build knows, oldest first.
    ///
    /// One list, which [`from_number`](Self::from_number) walks rather than
    /// keeping a second copy of the mapping. The constant below is what keeps
    /// it complete.
    pub const ALL: &'static [Self] = &[Self::V1];

    /// The number that appears in the document.
    #[must_use]
    pub const fn number(self) -> u32 {
        match self {
            Self::V1 => 1,
        }
    }

    /// The version a number names, or [`None`] when this build has never heard
    /// of it — in practice, a document written by a newer build.
    #[must_use]
    pub const fn from_number(number: u32) -> Option<Self> {
        let mut index = 0;
        while index < Self::ALL.len() {
            let version = Self::ALL[index];
            if version.number() == number {
                return Some(version);
            }
            index += 1;
        }
        None
    }

    /// Where this version sits in [`ALL`](Self::ALL).
    ///
    /// A `match` on `Self` rather than a search, and that is the whole point:
    /// it is exhaustive, so it is where a new variant first fails to compile.
    const fn position(self) -> usize {
        match self {
            Self::V1 => 0,
        }
    }
}

// Compile-time proof that `SchemaVersion::ALL` lists every version, in order,
// ending at `CURRENT`.
//
// Adding `V2` stops `position` compiling; giving it position 1 then stops this
// compiling until `ALL` holds it there, and until `CURRENT` names the last
// entry. Both halves are needed. Without the first, a version could be added
// with no position; without the second, it could be given a position and left
// out of the list — and either way `from_number` would answer `None` for a
// version this build itself writes, so every document written under it would
// read back as one from an unknown schema: flagged not understood, never
// upgraded, its payload uninterpretable on the machine that wrote it.
//
// `upgrade` is the other half of the guard, and catches a different mistake:
// this one is about a version the build cannot *recognise*, that one about a
// version it cannot *migrate*.
const _: () = {
    let mut index = 0;
    while index < SchemaVersion::ALL.len() {
        assert!(
            SchemaVersion::ALL[index].position() == index,
            "SchemaVersion::ALL must list every version at its own position, oldest first"
        );
        index += 1;
    }
    assert!(
        SchemaVersion::CURRENT.position() + 1 == SchemaVersion::ALL.len(),
        "SchemaVersion::CURRENT must be the last entry in SchemaVersion::ALL"
    );
};

impl fmt::Display for SchemaVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.number())
    }
}

/// The field that carries the version. Frozen, like the rest of the envelope.
pub const SCHEMA_FIELD: &str = "schema";

/// The names [`GameEvent`] occupies in a document. Frozen, and exhaustive.
///
/// Reading uses it to tell an envelope field from one this build has never met,
/// so that the second kind can be kept and written back rather than deleted.
/// `the_envelope_is_exactly_the_fields_the_event_writes` is what keeps the list
/// honest: a field added to [`GameEvent`] without being added here would be
/// written twice, once by the event and once as something unknown.
const ENVELOPE_FIELDS: [&str; 7] = [
    "kind",
    "at",
    "precision",
    "latency",
    "source",
    "confidence",
    "data",
];

/// An event as it is stored: the envelope, plus the version it was written
/// under.
///
/// The version travels **with each event** rather than with the file or the
/// table that holds it, so that an event copied out of one and into another —
/// exported, attached to a bug report, moved between a session's sidecar and
/// the library database — is still self-describing.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StoredEvent {
    /// The schema this document was written under.
    schema: u32,
    /// The event.
    #[serde(flatten)]
    event: GameEvent,
    /// Fields the build that read this document had no name for, carried back
    /// out exactly as they came in. Empty for an event this build produced.
    #[serde(flatten)]
    unknown: Map<String, Value>,
}

impl StoredEvent {
    /// Prepares an event **this build produced** for storage at the current
    /// schema version.
    ///
    /// For an event that came off a disk, use [`ReadEvent::to_json`] or
    /// [`from_read`](Self::from_read) instead: this constructor writes only
    /// what [`GameEvent`] models, which is the right answer for a new event and
    /// the wrong one for a document that arrived carrying more.
    #[must_use]
    pub fn new(event: GameEvent) -> Self {
        Self {
            schema: SchemaVersion::CURRENT.number(),
            event,
            unknown: Map::new(),
        }
    }

    /// Prepares an event that was read back for storage again, keeping the
    /// parts this build did not understand.
    ///
    /// The schema number is the one the document goes back out under, and it is
    /// not always [`SchemaVersion::CURRENT`]: see
    /// [`WrittenUnder::written_back_as`].
    #[must_use]
    pub fn from_read(read: ReadEvent) -> Self {
        Self {
            schema: read.schema.written_back_as(),
            event: read.event,
            unknown: read.unknown,
        }
    }

    /// The schema number this document carries.
    #[must_use]
    pub const fn schema(&self) -> u32 {
        self.schema
    }

    /// The document, as JSON.
    ///
    /// # Errors
    ///
    /// [`serde_json::Error`] only if the payload contains something JSON cannot
    /// represent, which [`EventPayload`](crate::EventPayload) construction
    /// already excludes.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

/// A stored event that has been read back, and what wrote it.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadEvent {
    /// The event, upgraded to the current schema if it needed it.
    pub event: GameEvent,
    /// Which schema wrote the document.
    pub schema: WrittenUnder,
    /// Everything else the document carried. See
    /// [`unknown_fields`](Self::unknown_fields).
    unknown: Map<String, Value>,
}

impl ReadEvent {
    /// Whether this build knows the schema the document was written under, and
    /// may therefore interpret [`GameEvent::data`].
    ///
    /// The envelope is readable either way; this is about what is inside it.
    #[must_use]
    pub const fn is_understood(&self) -> bool {
        matches!(self.schema, WrittenUnder::Known(_))
    }

    /// The document's fields that are not part of the envelope: what a newer
    /// build added, and this one has no name for.
    ///
    /// Held rather than discarded so that [`to_json`](Self::to_json) can put
    /// them back. Nothing in this crate interprets them, and nothing should:
    /// they are exactly the part whose meaning this build does not know.
    #[must_use]
    pub const fn unknown_fields(&self) -> &Map<String, Value> {
        &self.unknown
    }

    /// This event, ready to be stored again.
    #[must_use]
    pub fn to_stored(&self) -> StoredEvent {
        StoredEvent::from_read(self.clone())
    }

    /// The document, as JSON: the envelope as this build understands it, plus
    /// every field it did not, under [the version it goes back out
    /// under](WrittenUnder::written_back_as).
    ///
    /// This is the write path for an event that came off a disk, and the reason
    /// "an event survives a build that does not understand it" is a property
    /// this crate has rather than one it hopes for. A library re-indexed by an
    /// older Clipped is written back through here.
    ///
    /// # What is not byte-identical
    ///
    /// Fields this build *does* understand are written in this build's normal
    /// form, which is the point of understanding them: a document that spelled
    /// out `"latency":0` or `"data":{}` gets them back omitted, exactly as this
    /// build omits them when it writes an event of its own. Nothing a consumer
    /// can observe changes. Everything else — unknown fields, an unknown kind,
    /// the schema number — is returned as it arrived.
    ///
    /// # Errors
    ///
    /// As [`StoredEvent::to_json`].
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        self.to_stored().to_json()
    }
}

/// The schema a stored event was written under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WrittenUnder {
    /// A version this build knows. The event has been upgraded to
    /// [`SchemaVersion::CURRENT`] if it was not already there.
    Known(SchemaVersion),
    /// A version this build does not know, carried verbatim.
    ///
    /// In practice this is a build from the future: the file was written by a
    /// newer Clipped than the one reading it. It is deliberately *not* named
    /// `Newer`, because a number below [`SchemaVersion::CURRENT`] would land
    /// here too — a corrupted field, or a version this build is too new to
    /// remember — and calling that "newer" would be a claim the reader cannot
    /// support (AGENTS.md section 27). The envelope was still read, because it
    /// is frozen.
    Unknown(u32),
}

impl WrittenUnder {
    /// The number the document carried.
    #[must_use]
    pub const fn number(self) -> u32 {
        match self {
            Self::Known(version) => version.number(),
            Self::Unknown(number) => number,
        }
    }

    /// The number the event is written back under.
    ///
    /// A version this build knows has been through [`upgrade`], so it goes back
    /// out at [`SchemaVersion::CURRENT`] — that is what upgrading it means.
    ///
    /// A version this build does **not** know goes back out under the number it
    /// arrived with, unchanged. The alternative, stamping it with the current
    /// version, was what this crate did first and it is wrong twice over: the
    /// payload and the kind inside were never re-encoded by this build, so
    /// calling the document "version 1" is a claim about their meaning that
    /// nothing here read (AGENTS.md section 27); and the next build to open the
    /// library — possibly the newer one that wrote it — would interpret a
    /// version-9 payload under version-1 rules, which is worse than not being
    /// able to interpret it at all.
    #[must_use]
    pub const fn written_back_as(self) -> u32 {
        match self {
            Self::Known(_) => SchemaVersion::CURRENT.number(),
            Self::Unknown(number) => number,
        }
    }
}

/// Reads a stored event, upgrading it from whatever version wrote it.
///
/// # Errors
///
/// [`ReadError`] when the document is not JSON, is not an object, has no usable
/// `schema` field, or does not carry a readable envelope. Those are the only
/// ways to fail: an unknown kind, an unknown field, an unknown version, a
/// malformed source and an out-of-range confidence are all read rather than
/// refused.
pub fn read(json: &str) -> Result<ReadEvent, ReadError> {
    let document: Value = serde_json::from_str(json).map_err(ReadError::NotJson)?;
    read_value(document)
}

/// Reads a stored event from an already-parsed document. See [`read`].
///
/// # Errors
///
/// As [`read`], less the parse.
pub fn read_value(document: Value) -> Result<ReadEvent, ReadError> {
    let Value::Object(mut fields) = document else {
        return Err(ReadError::NotAnObject);
    };

    let written = match fields.get(SCHEMA_FIELD) {
        None => return Err(ReadError::NoSchemaVersion),
        Some(value) => match schema_number(value) {
            Some(number) => number,
            None => {
                return Err(ReadError::UnreadableSchemaVersion {
                    found: value.to_string(),
                })
            }
        },
    };

    let schema = match SchemaVersion::from_number(written) {
        Some(version) => {
            fields = upgrade(fields, version);
            WrittenUnder::Known(version)
        }
        // Not upgraded, because there is no step from a version this build has
        // never seen. Read anyway: the envelope is frozen, so a newer build's
        // event still has a kind, a time and a source that mean what they say.
        None => WrittenUnder::Unknown(written),
    };

    // Taken after the upgrade, so that a step which consumes or renames a field
    // is not undone by that field reappearing as something unknown.
    let unknown = unknown_fields(&fields);
    let event =
        serde_json::from_value::<GameEvent>(Value::Object(fields)).map_err(ReadError::Malformed)?;

    Ok(ReadEvent {
        event,
        schema,
        unknown,
    })
}

/// The version number a `schema` field carries, or [`None`] when the value is
/// not a version at all.
///
/// Deliberately narrow, and deliberately separate from "absent". A version is a
/// whole number that fits in a `u32`; `1.0`, `"1"`, `-1` and `1e12` are not
/// versions, and a document carrying one of those was written by something that
/// is not this crate. Reporting that as a *missing* field would send whoever
/// opens the file looking for a field that is right in front of them.
fn schema_number(value: &Value) -> Option<u32> {
    value.as_u64().and_then(|number| u32::try_from(number).ok())
}

/// The fields of a document that are not part of the envelope.
///
/// Whatever a newer build added. Kept so that reading an event and writing it
/// back does not delete it; see [`ReadEvent::to_json`].
fn unknown_fields(document: &Map<String, Value>) -> Map<String, Value> {
    document
        .iter()
        .filter(|(name, _)| {
            name.as_str() != SCHEMA_FIELD && !ENVELOPE_FIELDS.contains(&name.as_str())
        })
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

/// Brings a document written under `from` up to [`SchemaVersion::CURRENT`].
///
/// The `match` is the mechanism. Adding a variant to [`SchemaVersion`] stops
/// this function compiling until the arm that upgrades documents written under
/// the previous version is written here — so a schema can be bumped, but it
/// cannot be bumped *quietly*, leaving events already on somebody's disk to be
/// discovered unreadable later.
fn upgrade(document: Map<String, Value>, from: SchemaVersion) -> Map<String, Value> {
    match from {
        // The current version. When `V2` exists, the arm above this one takes a
        // `V1` document and returns a `V2` one, and this arm stays as it is.
        SchemaVersion::V1 => document,
    }
}

/// Why a stored event could not be read.
#[derive(Debug)]
pub enum ReadError {
    /// It is not JSON.
    NotJson(serde_json::Error),
    /// It is JSON, but not an object.
    NotAnObject,
    /// It has no `schema` field, so there is no way to know what it means. A
    /// document written by this crate always has one.
    NoSchemaVersion,
    /// It has a `schema` field, but the value is not a version number: a
    /// string, a fraction, a negative, or one too large for a `u32`.
    ///
    /// Separate from [`NoSchemaVersion`](Self::NoSchemaVersion) because
    /// "absent" and "unreadable" are different faults with different fixes, and
    /// an error that says a field is missing when it is present in front of the
    /// reader is worse than no error at all (AGENTS.md section 45).
    UnreadableSchemaVersion {
        /// The value found, as JSON.
        found: String,
    },
    /// The envelope itself could not be read: a field the model has always
    /// required is missing or has the wrong type. An unknown kind, an unknown
    /// field and an unknown version are *not* this.
    Malformed(serde_json::Error),
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotJson(error) => write!(f, "a stored event must be JSON: {error}"),
            Self::NotAnObject => f.write_str("a stored event is a JSON object, and this is not"),
            Self::NoSchemaVersion => write!(
                f,
                "a stored event records the schema version it was written under in `{SCHEMA_FIELD}`, \
                 and this document has no such field"
            ),
            Self::UnreadableSchemaVersion { found } => write!(
                f,
                "a stored event records the schema version it was written under in `{SCHEMA_FIELD}` \
                 as a whole number, and this document has `{found}` there"
            ),
            Self::Malformed(error) => write!(
                f,
                "this document carries a schema version but not a readable event envelope: {error}"
            ),
        }
    }
}

impl core::error::Error for ReadError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::NotJson(error) | Self::Malformed(error) => Some(error),
            Self::NotAnObject | Self::NoSchemaVersion | Self::UnreadableSchemaVersion { .. } => {
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::event::{Confidence, EventPayload, EventSource};
    use crate::kind::{CustomName, EventKind, UserLabel};
    use crate::time::{EventTime, EventTiming};

    fn kill() -> GameEvent {
        GameEvent::new(
            EventKind::Kill,
            EventTiming::new(EventTime::from_media_nanos(61_500_000_000), Duration::ZERO)
                .reported_late_by(Duration::from_millis(480)),
            EventSource::plugin("counter-strike-2").expect("a valid identifier"),
            Confidence::CERTAIN,
        )
    }

    /// A version-1 document, written out by hand.
    ///
    /// Literal on purpose, and the most load-bearing test in this crate. This
    /// is the shape that goes into users' databases from the first release
    /// onward: if a field is renamed, retyped or dropped, this fails and
    /// somebody has to explain themselves in review (AGENTS.md section 43).
    const VERSION_1_KILL: &str = r#"{"schema":1,"kind":"kill","at":61500000000,"precision":0,"latency":480000000,"source":"counter-strike-2","confidence":1.0}"#;

    /// One document per schema version, oldest first.
    ///
    /// The list `a_document_from_every_version_this_build_knows_still_reads`
    /// walks, and it must cover [`SchemaVersion::ALL`] — which is the mechanism
    /// that turns the first schema bump into a real migration test rather than
    /// a remembered intention. At version 2, `VERSION_1_KILL` becomes a
    /// version-1 document read by a version-2 build, which is the acceptance
    /// criterion issue #68 asks for and the thing that cannot be written today.
    const GOLDEN_DOCUMENTS: &[(SchemaVersion, &str)] = &[(SchemaVersion::V1, VERSION_1_KILL)];

    /// The document from the review that showed the old round-trip test could
    /// not fail: a later schema, a kind this build has never met, a payload it
    /// must not interpret, and two fields it has no name for at all.
    const FROM_THE_FUTURE: &str = r#"{"schema":9,"kind":"objective_taken","at":61500000000,"precision":250000000,"latency":1000000000,"source":"acme","confidence":0.5,"data":{"objective":"mid"},"team":"ct","replay_id":"abc"}"#;

    /// The document, parsed, so that two of them can be compared whole.
    fn document(json: &str) -> Value {
        serde_json::from_str(json).expect("a test document is JSON")
    }

    #[test]
    fn the_stored_shape_is_the_one_documented_and_frozen() {
        assert_eq!(
            StoredEvent::new(kill()).to_json().expect("it serialises"),
            VERSION_1_KILL
        );
    }

    #[test]
    fn the_envelope_is_exactly_the_fields_the_event_writes() {
        // `ENVELOPE_FIELDS` is what tells reading which fields belong to this
        // build and which are somebody else's, so a field added to `GameEvent`
        // and not added there would be written twice: once by the event, once
        // as something unknown carried back out.
        let event = kill().with_data(
            EventPayload::new(
                json!({"weapon": "ak47"})
                    .as_object()
                    .expect("an object")
                    .clone(),
            )
            .expect("within the limit"),
        );
        let Value::Object(fields) = serde_json::to_value(&event).expect("it serialises") else {
            panic!("an event is a JSON object");
        };

        let mut expected = ENVELOPE_FIELDS.to_vec();
        expected.sort_unstable();
        assert_eq!(
            fields.keys().map(String::as_str).collect::<Vec<_>>(),
            expected,
            "every field an event writes must be listed in ENVELOPE_FIELDS, and nothing else"
        );
    }

    #[test]
    fn a_stored_event_reads_back_as_itself() {
        let read = read(VERSION_1_KILL).expect("a version 1 document is readable");
        assert_eq!(read.event, kill());
        assert_eq!(read.schema, WrittenUnder::Known(SchemaVersion::V1));
        assert!(read.is_understood());
    }

    #[test]
    fn every_field_of_the_envelope_survives_the_round_trip() {
        let event = kill().with_data(
            EventPayload::new(
                json!({"weapon": "ak47", "headshot": true})
                    .as_object()
                    .expect("an object")
                    .clone(),
            )
            .expect("within the limit"),
        );
        let json = StoredEvent::new(event.clone())
            .to_json()
            .expect("it serialises");
        let read = read(&json).expect("it reads back");
        assert_eq!(read.event, event);
        assert_eq!(read.event.timing().latency(), Duration::from_millis(480));
        assert_eq!(read.event.data().fields()["headshot"], json!(true));
    }

    #[test]
    fn a_document_written_under_a_later_schema_is_still_read_and_flagged() {
        // The criterion this whole module exists for: an event stored by a
        // build newer than this one must not disappear. The envelope is frozen,
        // so everything a timeline needs is still there; only the reading of
        // `data` is in doubt, and `schema` is how a consumer knows.
        let future = r#"{"schema":7,"kind":"kill","at":61500000000,"precision":0,"source":"counter-strike-2","confidence":1.0,"data":{"weapon":"ak47"},"team":"ct"}"#;

        let read = read(future).expect("a newer document must still be readable");

        assert_eq!(read.schema, WrittenUnder::Unknown(7));
        assert!(!read.is_understood());
        assert_eq!(read.event.kind(), &EventKind::Kill);
        assert_eq!(read.event.timing().at().as_media_nanos(), 61_500_000_000);
        assert_eq!(read.event.source().as_str(), "counter-strike-2");
        assert_eq!(read.event.data().fields()["weapon"], json!("ak47"));
    }

    #[test]
    fn a_schema_number_this_build_does_not_know_is_not_called_newer() {
        // `0` is not a version anything wrote. It could only be corruption, or
        // a build so old this one has forgotten it — and reporting either as
        // "written by a newer Clipped" would be a claim the reader cannot
        // support. It is still read, because the envelope is frozen.
        let odd =
            r#"{"schema":0,"kind":"kill","at":1,"precision":0,"source":"acme","confidence":1.0}"#;
        let read = read(odd).expect("an unknown version is still read");
        assert_eq!(read.schema, WrittenUnder::Unknown(0));
        assert!(!read.is_understood());
        assert_eq!(read.event.kind(), &EventKind::Kill);
    }

    #[test]
    fn a_kind_added_after_this_build_shipped_is_kept_rather_than_dropped() {
        let future = r#"{"schema":1,"kind":"objective_taken","at":61500000000,"precision":0,"source":"acme","confidence":0.5}"#;

        let read = read(future).expect("an unknown kind must not break the document");

        assert_eq!(
            read.event.kind(),
            &EventKind::Unrecognised("objective_taken".to_owned())
        );
        assert_eq!(
            read.schema,
            WrittenUnder::Known(SchemaVersion::V1),
            "adding a kind does not bump the schema, so this is still a version 1 document"
        );
        // And it is handed back exactly as it arrived, so a build that does
        // understand it still can.
        let rewritten = StoredEvent::new(read.event).to_json().expect("it writes");
        assert!(
            rewritten.contains(r#""kind":"objective_taken""#),
            "an unknown kind must survive being written back: {rewritten}"
        );
    }

    #[test]
    fn a_custom_event_survives_a_build_that_has_never_met_the_plugin() {
        let stored = r#"{"schema":1,"kind":"acme-cs2.flag_captured","at":1,"precision":0,"source":"acme-cs2","confidence":1.0}"#;

        let read = read(stored).expect("a custom event is readable anywhere");

        assert_eq!(
            read.event.kind(),
            &EventKind::Custom(CustomName::new("acme-cs2.flag_captured").expect("valid"))
        );
    }

    #[test]
    fn a_user_labelled_event_round_trips_through_storage_with_its_label_intact() {
        // Issue #345's acceptance criterion, exercised at the layer that
        // actually matters: the shape these end up in, in a library's
        // database and a recording's sidecar.
        let event = GameEvent::new(
            EventKind::UserLabelled(
                UserLabel::new("My Ultimate! (é)").expect("a well-formed label"),
            ),
            EventTiming::new(EventTime::from_media_nanos(1), Duration::ZERO),
            EventSource::application_component("input").expect("a valid component"),
            Confidence::CERTAIN,
        );

        let json = StoredEvent::new(event.clone())
            .to_json()
            .expect("it serialises");
        assert!(
            json.contains(r#""kind":"user:My Ultimate! (é)""#),
            "the label is the wire form of the kind, unprefixed nowhere: {json}"
        );
        assert!(
            json.contains(r#""source":"clipped.input""#),
            "the host component is its own source, distinct from `clipped`: {json}"
        );

        let read = read(&json).expect("a user-labelled event is readable");
        assert_eq!(read.event, event);
        let EventKind::UserLabelled(label) = read.event.kind() else {
            panic!("expected a user-labelled kind, got {:?}", read.event.kind());
        };
        assert_eq!(label.label(), "My Ultimate! (é)");
        assert!(read.event.source().is_application());
    }

    #[test]
    fn an_unknown_field_is_ignored_rather_than_fatal() {
        let stored = r#"{"schema":1,"kind":"kill","at":1,"precision":0,"source":"acme","confidence":1.0,"invented_later":42}"#;
        let read = read(stored).expect("an unknown field costs no version bump");
        assert_eq!(read.event.kind(), &EventKind::Kill);
    }

    #[test]
    fn a_document_with_no_schema_version_is_refused_by_name() {
        let error = read(r#"{"kind":"kill","at":1,"precision":0,"source":"a","confidence":1.0}"#)
            .expect_err("a stored event must say what wrote it");
        assert!(matches!(error, ReadError::NoSchemaVersion));
        assert!(
            error.to_string().contains(SCHEMA_FIELD),
            "the message should name the missing field: {error}"
        );
    }

    #[test]
    fn a_schema_version_that_is_not_a_number_is_not_reported_as_a_missing_one() {
        // Each of these has a `schema` field. Telling the person looking at the
        // document that it is absent sends them looking for something that is
        // in front of them (AGENTS.md section 45).
        for value in ["\"1\"", "1.0", "-1", "null", "99999999999999"] {
            let json = format!(
                r#"{{"schema":{value},"kind":"kill","at":1,"precision":0,"source":"a","confidence":1.0}}"#
            );
            let error = read(&json).expect_err("`{value}` is not a schema version");
            let ReadError::UnreadableSchemaVersion { ref found } = error else {
                panic!("`{value}` should be unreadable rather than {error:?}");
            };
            assert_eq!(found, value, "the message should quote what it found");
            assert!(
                error.to_string().contains(value),
                "the message should say what is there: {error}"
            );
        }
    }

    #[test]
    fn a_broken_envelope_is_refused_rather_than_guessed_at() {
        // The envelope is the one thing that cannot be absorbed: an event with
        // no time is not a mark on any timeline.
        let error = read(r#"{"schema":1,"kind":"kill","source":"a","confidence":1.0}"#)
            .expect_err("an event without a time is not an event");
        assert!(matches!(error, ReadError::Malformed(_)));

        assert!(matches!(
            read("not json").unwrap_err(),
            ReadError::NotJson(_)
        ));
        assert!(matches!(read("[]").unwrap_err(), ReadError::NotAnObject));
    }

    #[test]
    fn the_version_this_build_writes_is_the_one_it_reads_without_upgrading() {
        assert_eq!(SchemaVersion::CURRENT.number(), 1);
        assert_eq!(SchemaVersion::from_number(1), Some(SchemaVersion::V1));
        assert_eq!(SchemaVersion::from_number(2), None);

        let document = serde_json::from_str::<Value>(VERSION_1_KILL)
            .expect("it parses")
            .as_object()
            .expect("an object")
            .clone();
        assert_eq!(
            upgrade(document.clone(), SchemaVersion::V1),
            document,
            "the current version needs no upgrading, and the upgrade must not invent fields"
        );
    }

    #[test]
    fn every_version_this_build_knows_is_one_it_can_recognise_by_number() {
        // The other half of the compile-time guard, checked at run time: a
        // version left out of `ALL`, or given a number `from_number` does not
        // answer to, would make every document written under it read back as
        // one from an unknown schema — on the machine that wrote it.
        for version in SchemaVersion::ALL {
            assert_eq!(
                SchemaVersion::from_number(version.number()),
                Some(*version),
                "{version} is a version this build writes, so it must recognise its number"
            );
        }
        assert!(
            SchemaVersion::ALL.contains(&SchemaVersion::CURRENT),
            "the version this build writes must be one it knows"
        );
    }

    #[test]
    fn a_document_from_every_version_this_build_knows_still_reads() {
        // The acceptance criterion's harness. Today it exercises one version
        // and no migration, because there is one version; at the first bump the
        // version-1 golden below becomes a version-1 document read by a
        // version-2 build, and the assertions here are what "survives" means.
        assert_eq!(
            GOLDEN_DOCUMENTS.len(),
            SchemaVersion::ALL.len(),
            "every schema version needs a golden document, or the migration from it is untested"
        );

        for (version, golden) in GOLDEN_DOCUMENTS {
            assert_eq!(
                SchemaVersion::ALL[version.position()],
                *version,
                "the goldens are in version order"
            );

            let first = read(golden).unwrap_or_else(|error| {
                panic!("a version {version} document must still read: {error}")
            });
            assert_eq!(first.schema, WrittenUnder::Known(*version));
            assert!(first.is_understood(), "an upgraded document is understood");
            assert_eq!(
                first.to_stored().schema(),
                SchemaVersion::CURRENT.number(),
                "a document this build understands is written back at the current version"
            );
            assert_ne!(
                first.event.timing().at(),
                EventTime::ZERO,
                "a golden that describes the start of the recording proves nothing about times"
            );

            let rewritten = first.to_json().expect("a document that read must write");
            let again = read(&rewritten).expect("and read again");
            assert_eq!(
                again, first,
                "reading, writing and reading again must reach the same event"
            );
        }
    }

    #[test]
    fn an_event_read_from_the_future_is_written_back_as_the_same_document() {
        // The property the crate exists to have, and the one the previous
        // version of this test could not see: an older build that reads a
        // library, re-indexes it and writes it out again must not quietly
        // destroy what it did not understand. Comparing the fields this build
        // happens to model would pass while `team` and `replay_id` were
        // deleted, so the whole document is compared.
        let first = read(FROM_THE_FUTURE).expect("readable");
        let rewritten = first.to_json().expect("writable");

        assert_eq!(
            document(&rewritten),
            document(FROM_THE_FUTURE),
            "the whole document, not just the parts this build has names for: {rewritten}"
        );

        // And the parts, so a failure says which one moved.
        assert_eq!(
            first.unknown_fields()["team"],
            json!("ct"),
            "a field this build has no name for is kept, not ignored"
        );
        assert_eq!(first.unknown_fields()["replay_id"], json!("abc"));
        assert_eq!(
            first.schema,
            WrittenUnder::Unknown(9),
            "the document was written under 9, and saying so is the point of the field"
        );
        assert_eq!(
            first.to_stored().schema(),
            9,
            "it goes back out as 9: this build never re-encoded the payload, so calling it \
             version 1 would be a claim about a meaning it did not read"
        );

        let again = read(&rewritten).expect("readable again");
        assert_eq!(again, first, "including the fields it did not understand");
        assert_eq!(
            again.event.kind(),
            &EventKind::Unrecognised("objective_taken".to_owned())
        );
        assert_eq!(again.event.data().fields()["objective"], json!("mid"));
        assert_eq!(again.event.timing().precision(), Duration::from_millis(250));
    }

    #[test]
    fn a_field_added_after_this_build_shipped_survives_a_version_it_does_know() {
        // The same guarantee at the version this build writes, which is the
        // common case: adding a field costs no schema bump, so an older build
        // reading a current-version document meets fields it has no name for
        // and must hand them back.
        let extended = r#"{"schema":1,"kind":"kill","at":1,"precision":0,"source":"acme","confidence":1.0,"invented_later":42}"#;

        let first = read(extended).expect("an unknown field costs no version bump");
        let rewritten = first.to_json().expect("writable");

        assert_eq!(first.schema, WrittenUnder::Known(SchemaVersion::V1));
        assert_eq!(document(&rewritten), document(extended));
        assert_eq!(first.unknown_fields()["invented_later"], json!(42));
    }

    #[test]
    fn a_stored_value_the_producer_boundary_would_refuse_is_still_read() {
        // `source` and `confidence` are checked where a producer creates one.
        // Checking them again on the way out of a database would fail the whole
        // document — losing the kind, the time and the payload — to enforce
        // rules the stored values have already broken (AGENTS.md section 56).
        let awkward = r#"{"schema":1,"kind":"kill","at":61500000000,"precision":0,"source":"Counter Strike","confidence":1.5}"#;

        let first = read(awkward).expect("a stored event is not refused over its own values");

        assert_eq!(first.event.source().as_str(), "Counter Strike");
        assert!(!first.event.source().is_well_formed());
        assert!(!first.event.confidence().is_usable());
        assert_eq!(
            document(&first.to_json().expect("writable")),
            document(awkward),
            "and neither value is repaired on the way back out"
        );
    }

    #[test]
    fn an_event_this_build_produced_is_written_without_anything_borrowed() {
        // `StoredEvent::new` is the producer's path, and must not acquire
        // fields from anywhere: the unknown fields belong to a document that
        // was read, not to an event that was made.
        let stored = StoredEvent::new(kill());
        assert_eq!(stored.schema(), SchemaVersion::CURRENT.number());
        assert_eq!(stored.to_json().expect("it serialises"), VERSION_1_KILL);
    }

    #[test]
    fn a_field_this_build_understands_is_written_in_this_builds_own_form() {
        // The stated limit of the round trip, tested rather than assumed. A
        // document that spells out a zero latency gets it back omitted, exactly
        // as this build omits it when writing an event of its own — because
        // this is a field it does understand, and nothing a consumer can see
        // changes.
        let spelled_out = r#"{"schema":1,"kind":"kill","at":1,"precision":0,"latency":0,"data":{},"source":"acme","confidence":1.0}"#;
        let first = read(spelled_out).expect("readable");
        let rewritten = first.to_json().expect("writable");

        assert!(!rewritten.contains("latency"), "{rewritten}");
        assert!(!rewritten.contains("data"), "{rewritten}");
        assert_eq!(
            read(&rewritten).expect("stable").event,
            first.event,
            "and the event it describes is unchanged"
        );
    }
}
