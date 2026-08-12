//! The recordings an edit refers to, and how it refers to them.
//!
//! An edit names a recording by the library's identifier for it, never by a
//! path. Paths rot: users move a folder, rename a drive letter or restore from
//! a backup, and a document holding `D:\Clips\2026-08\rec.mkv` is then a clip
//! that cannot be opened even though the file is right there.
//! `clipped-library` already reconciles identifiers against what is on disk
//! because "users move and delete files behind the application's back", and an
//! edit document is one more thing that should benefit from it rather than
//! keeping a second, staler copy of the answer.

use serde::{Deserialize, Serialize};

/// The library's identifier for a recording.
///
/// Opaque here on purpose: this crate is at the bottom of the stack and must
/// not know what the database's identifiers look like ([issue
/// #55](https://github.com/wildware-uk/clipped/issues/55) owns that). It is a
/// string because that is the one shape every candidate — an integer row id, a
/// UUID, a filename stem — can be written as without this crate having an
/// opinion.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RecordingId(String);

impl RecordingId {
    /// Wraps a library identifier.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The identifier as the library wrote it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether the identifier is anything at all.
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl core::fmt::Display for RecordingId {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// How a segment names one of the document's sources.
///
/// Document-local and stable: segments and audio tracks refer to a source by
/// this rather than by its position in the list, so that removing the second of
/// three sources does not silently repoint everything that referred to the
/// third.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceId(u32);

impl SourceId {
    /// A source identifier with the given number.
    #[must_use]
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    /// The number.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl core::fmt::Display for SourceId {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// One recording an edit draws material from.
///
/// A document holds a list of these rather than a single `source` field, which
/// is the decision that keeps [issue
/// #88](https://github.com/wildware-uk/clipped/issues/88) — combining clips
/// from more than one recording — from being a rewrite of everything built on
/// top of this. A single-recording edit is simply a document with one entry.
///
/// Deliberately two fields. The recording's duration, resolution and frame rate
/// are all things the library and the file itself already know, and a copy kept
/// here would be a second answer that goes stale the moment either changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// How this document refers to the recording.
    pub id: SourceId,
    /// Which recording it is, in the library's terms.
    pub recording: RecordingId,
}

impl Source {
    /// Declares `recording` as source `id` of a document.
    #[must_use]
    pub fn new(id: SourceId, recording: RecordingId) -> Self {
        Self { id, recording }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recording_id_is_carried_through_untouched() {
        let id = RecordingId::new("2026-08-11T20-14-03-cs2");
        assert_eq!(id.as_str(), "2026-08-11T20-14-03-cs2");
        assert_eq!(id.to_string(), "2026-08-11T20-14-03-cs2");
        assert!(!id.is_empty());
    }

    #[test]
    fn a_blank_recording_id_counts_as_missing() {
        assert!(RecordingId::new("   ").is_empty());
        assert!(RecordingId::new("").is_empty());
    }

    #[test]
    fn a_source_serialises_as_the_two_fields_it_has() {
        let source = Source::new(SourceId::new(3), RecordingId::new("rec-9"));
        let json = serde_json::to_string(&source).expect("a source serialises");

        assert_eq!(json, r#"{"id":3,"recording":"rec-9"}"#);
        assert_eq!(
            serde_json::from_str::<Source>(&json).expect("it reads back"),
            source
        );
    }

    #[test]
    fn a_source_with_a_field_this_build_does_not_know_is_refused() {
        // Every shape change bumps the schema version (see `crate::schema`), so
        // an unexpected key at the current version is damage rather than a
        // newer build being friendly.
        let error =
            serde_json::from_str::<Source>(r#"{"id":1,"recording":"r","path":"D:\\x.mkv"}"#)
                .expect_err("an unknown field is refused");
        assert!(
            error.to_string().contains("path"),
            "the message should name the field: {error}"
        );
    }
}
