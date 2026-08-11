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
//! - **An unknown field is ignored.** Adding one to an event costs no version
//!   bump, which is what `serde` does by default.
//! - **An unknown `kind` is kept**, as [`EventKind::Unrecognised`] or
//!   [`EventKind::Custom`] depending on whether it is namespaced. Adding a kind
//!   costs no version bump either — and this is the part that has to be
//!   implemented rather than inherited, because a tagged union whose tag a
//!   build does not recognise fails the whole document it is part of. An event
//!   whose kind means nothing to this build is still a mark it can place,
//!   attribute and draw.
//! - **A document from a newer schema is read, and flagged.** The envelope is
//!   frozen, so its times and its source are still exactly what they say they
//!   are; what a bump can change is what lies *inside* — the meaning of a
//!   payload, or of a kind. [`ReadEvent::schema`] says which build wrote it so
//!   that a consumer choosing to interpret [`GameEvent::data`] knows whether it
//!   may.
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

    /// The number that appears in the document.
    #[must_use]
    pub const fn number(self) -> u32 {
        match self {
            Self::V1 => 1,
        }
    }

    /// The version a number names, or [`None`] when this build has never heard
    /// of it — which today means a document written by a newer build.
    #[must_use]
    pub const fn from_number(number: u32) -> Option<Self> {
        match number {
            1 => Some(Self::V1),
            _ => None,
        }
    }
}

impl fmt::Display for SchemaVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.number())
    }
}

/// The field that carries the version. Frozen, like the rest of the envelope.
pub const SCHEMA_FIELD: &str = "schema";

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
}

impl StoredEvent {
    /// Prepares an event for storage at the current schema version.
    #[must_use]
    pub fn new(event: GameEvent) -> Self {
        Self {
            schema: SchemaVersion::CURRENT.number(),
            event,
        }
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
}

/// The schema a stored event was written under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WrittenUnder {
    /// A version this build knows. The event has been upgraded to
    /// [`SchemaVersion::CURRENT`] if it was not already there.
    Known(SchemaVersion),
    /// A version from the future: this build has been overtaken by one that
    /// wrote the file. The envelope was still read, because it is frozen.
    Newer(u32),
}

/// Reads a stored event, upgrading it from whatever version wrote it.
///
/// # Errors
///
/// [`ReadError`] when the document is not JSON, is not an object, has no
/// `schema` field, or does not carry a readable envelope. Those are the only
/// four ways to fail: an unknown kind, an unknown field and an unknown version
/// are all read rather than refused.
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

    let written = match fields.get(SCHEMA_FIELD).and_then(Value::as_u64) {
        Some(number) => u32::try_from(number).unwrap_or(u32::MAX),
        None => return Err(ReadError::NoSchemaVersion),
    };

    let schema = match SchemaVersion::from_number(written) {
        Some(version) => {
            fields = upgrade(fields, version);
            WrittenUnder::Known(version)
        }
        // Not upgraded, because there is no step from a version this build has
        // never seen. Read anyway: the envelope is frozen, so a newer build's
        // event still has a kind, a time and a source that mean what they say.
        None => WrittenUnder::Newer(written),
    };

    let event =
        serde_json::from_value::<GameEvent>(Value::Object(fields)).map_err(ReadError::Malformed)?;

    Ok(ReadEvent { event, schema })
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
            Self::NotAnObject | Self::NoSchemaVersion => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::event::{Confidence, EventPayload, EventSource};
    use crate::kind::{CustomName, EventKind};
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

    #[test]
    fn the_stored_shape_is_the_one_documented_and_frozen() {
        assert_eq!(
            StoredEvent::new(kill()).to_json().expect("it serialises"),
            VERSION_1_KILL
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

        assert_eq!(read.schema, WrittenUnder::Newer(7));
        assert!(!read.is_understood());
        assert_eq!(read.event.kind(), &EventKind::Kill);
        assert_eq!(read.event.timing().at().as_media_nanos(), 61_500_000_000);
        assert_eq!(read.event.source().as_str(), "counter-strike-2");
        assert_eq!(read.event.data().fields()["weapon"], json!("ak47"));
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
    fn an_event_read_from_the_future_can_be_written_back_without_loss() {
        // What "survive" has to mean in practice: an old build that reads a
        // library, re-indexes it and writes it out again must not quietly
        // destroy what it did not understand.
        let future = r#"{"schema":9,"kind":"objective_taken","at":61500000000,"precision":250000000,"latency":1000000000,"source":"acme","confidence":0.5,"data":{"objective":"mid"}}"#;

        let first = read(future).expect("readable");
        let rewritten = StoredEvent::new(first.event.clone())
            .to_json()
            .expect("writable");
        let again = read(&rewritten).expect("readable again");

        assert_eq!(again.event, first.event);
        assert_eq!(
            again.event.kind(),
            &EventKind::Unrecognised("objective_taken".to_owned())
        );
        assert_eq!(again.event.data().fields()["objective"], json!("mid"));
        assert_eq!(again.event.timing().precision(), Duration::from_millis(250));
        assert_eq!(
            again.schema,
            WrittenUnder::Known(SchemaVersion::V1),
            "this build can only write what it understands, and says so rather than claiming 9"
        );
    }
}
