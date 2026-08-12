//! The encoding, its version, and what happens either side of that version.
//!
//! # Where a document lives
//!
//! Nowhere this crate can see. An edit is metadata about a recording, and
//! AGENTS.md section 32 puts application metadata in SQLite; the schema and the
//! migrations for it are [issue
//! #55](https://github.com/wildware-uk/clipped/issues/55). So an edit document
//! is **text in a column**, and this module is the encoder and decoder for that
//! text, not a second place that stores things (AGENTS.md section 55).
//!
//! JSON rather than a binary format because a user must keep access to their
//! own data (AGENTS.md section 32), and because the desktop editor reads these
//! documents over an IPC protocol that is already versioned JSON
//! ([docs/ipc.md](../../../docs/ipc.md)) — one encoding across the boundary
//! rather than a conversion nobody can debug.
//!
//! # The version, and what happens either side of it
//!
//! [`SCHEMA_VERSION`] is the version of the *format*, not of Clipped. A
//! document carrying it is read directly. An older one is converted through
//! [`MIGRATIONS`] first. A newer one is **refused, and nothing is written
//! back**: a build that cannot understand a document has no business rewriting
//! it, and a user who edited a clip on the machine that was up to date must
//! still have that edit tomorrow (AGENTS.md sections 43 and 56).
//!
//! Every shape change bumps the version — including one that only *adds* a
//! field. That is why every structure in this crate carries
//! `deny_unknown_fields`: these documents are written by Clipped and never by
//! hand, so there is no cost to bumping, and the alternative is an older build
//! silently dropping the field a newer one added the moment the user saves.
//! Refusing to open beats opening and quietly discarding.
//!
//! # The other copy of this walk
//!
//! `crates/game-detection/src/catalogue/schema.rs` holds the same `Migration`,
//! the same `migrate` walk and the same "read the version out of the raw
//! document" rule, over `toml::Table` instead of `serde_json::Map`. That is a
//! deliberate second copy rather than an oversight, and [issue
//! #268](https://github.com/wildware-uk/clipped/issues/268) records both the
//! reasoning and what would have to change to remove it: the shared crate would
//! have to be visible from layer 0 and layer 1 at once, and layer 0 depends on
//! nothing in this workspace. **Change one of the two and read the other**,
//! because the rules they encode — follow each step's `from`, never overshoot
//! the target, refuse rather than half-convert, and write nothing back — are
//! meant to be the same rules.
//!
//! `crates/storage/src/migrations.rs` is not a third copy. It applies SQL
//! inside a transaction against a database file it backs up first; it shares
//! only the word.
//!
//! # What the caller owes a migrated document
//!
//! [`Loaded::migrated`] says the text on disk is now out of date. Replacing it
//! is the caller's decision because the caller owns the row, but the rule it
//! must follow is the catalogue's
//! (`crates/game-detection/src/catalogue/overlay.rs`): **keep the original**.
//! The conversion happens entirely in memory here and nothing is written by
//! this crate at all, so a caller that stores the new text without keeping the
//! old one has thrown away the only copy of something it could not itself
//! produce.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::document::EditDocument;
use crate::error::EditDocumentError;

/// The version of the edit document format.
///
/// | Version | What changed |
/// | --- | --- |
/// | 1 | The first there has ever been ([issue #82](https://github.com/wildware-uk/clipped/issues/82)) |
/// | 2 | An audio track no longer carries `soloed` ([issue #85](https://github.com/wildware-uk/clipped/issues/85)) |
pub const SCHEMA_VERSION: u32 = 2;

/// The key the version is stored under, read before anything else is trusted.
const VERSION_KEY: &str = "schema_version";

/// One step from one schema version to the next.
///
/// A step is handed the whole document as JSON and may do anything to it; the
/// framework sets [`VERSION_KEY`] to [`Self::to`] afterwards, so a step never
/// has to remember to. Returning `Err` refuses the document rather than
/// producing a half-converted one.
#[derive(Clone, Copy)]
pub(crate) struct Migration {
    /// The version this step reads.
    pub(crate) from: u32,
    /// The version it produces, which must be greater than `from`.
    pub(crate) to: u32,
    /// The conversion.
    pub(crate) apply: fn(&mut Map<String, Value>) -> Result<(), String>,
}

impl core::fmt::Debug for Migration {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Migration")
            .field("from", &self.from)
            .field("to", &self.to)
            .finish_non_exhaustive()
    }
}

/// Every step this build knows, in no required order.
pub(crate) const MIGRATIONS: &[Migration] = &[SOLO_IS_NOT_AN_EDIT];

/// Version 1 to 2: a solo is the editor listening, not part of the clip.
///
/// Version 1 stored `soloed` on each audio track, which made "listening to the
/// microphone on its own" a property of the saved document — so a clip could be
/// exported with every other track silent because the user had left a solo on,
/// and two soloed tracks were a state the format could express and nobody had
/// defined. [Issue #85](https://github.com/wildware-uk/clipped/issues/85) moved
/// it out to [`Solo`](crate::Solo), which the editor holds and no document
/// carries.
///
/// The conversion therefore **drops the flag**. It is the only honest one: a
/// solo describes a moment of somebody's editing session and not the clip, so
/// there is nothing in a version 2 document for it to become. Turning it into
/// mutes on the other tracks would silence material the user never asked to
/// lose, which is the opposite of AGENTS.md section 56 — and the level, the
/// mute and the fades of every track, which *are* the edit, come through
/// untouched.
const SOLO_IS_NOT_AN_EDIT: Migration = Migration {
    from: 1,
    to: 2,
    apply: drop_solo,
};

/// Removes `soloed` from every audio track of a version 1 document.
///
/// Silent about anything that is not an audio track carrying the key: a
/// document shaped differently is refused by the deserialiser afterwards, with
/// a message about the shape rather than one from in here.
fn drop_solo(document: &mut Map<String, Value>) -> Result<(), String> {
    let Some(tracks) = document
        .get_mut("audio_tracks")
        .and_then(Value::as_array_mut)
    else {
        return Ok(());
    };
    for track in tracks {
        if let Some(track) = track.as_object_mut() {
            track.remove("soloed");
        }
    }
    Ok(())
}

/// A document, and whether reading it converted anything.
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded {
    /// The document, validated.
    pub document: EditDocument,
    /// What was converted, if the stored text was older than this build.
    ///
    /// `Some` means the stored text is out of date and the caller may replace
    /// it — **keeping the original**, which is the only copy of a document this
    /// build could not have produced.
    pub migrated: Option<Migrated>,
}

/// A conversion that happened while reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Migrated {
    /// The version the stored text was in.
    pub from: u32,
    /// The version the document is in now.
    pub to: u32,
}

/// Reads `text` at `target`, converting an older document in memory only.
///
/// `target` and `migrations` are parameters rather than constants so that the
/// migration machinery can be exercised by tests that own both ends of a
/// conversion. [`EditDocument::read`] passes [`SCHEMA_VERSION`] and
/// [`MIGRATIONS`].
pub(crate) fn read(
    text: &str,
    target: u32,
    migrations: &[Migration],
) -> Result<Loaded, EditDocumentError> {
    let value: Value = serde_json::from_str(text).map_err(|error| EditDocumentError::Syntax {
        message: error.to_string(),
    })?;
    let Value::Object(mut document) = value else {
        return Err(EditDocumentError::Shape {
            message: "an edit is a JSON object, and this is not one".to_owned(),
        });
    };

    let found = schema_version(&document).ok_or(EditDocumentError::SchemaVersionMissing)?;
    if found > target {
        return Err(EditDocumentError::SchemaTooNew {
            found,
            supported: target,
        });
    }

    let migrated = if found == target {
        None
    } else {
        migrate(&mut document, found, target, migrations)?;
        Some(Migrated {
            from: found,
            to: target,
        })
    };

    let document: EditDocument =
        serde_json::from_value(Value::Object(document)).map_err(|error| {
            EditDocumentError::Shape {
                message: error.to_string(),
            }
        })?;
    document.validate()?;

    Ok(Loaded { document, migrated })
}

/// Writes a document as the text a caller stores.
///
/// Serialisation of this type cannot fail — there are no maps with non-string
/// keys and validation has already refused the non-finite floats that JSON
/// cannot represent — but the result is reported rather than unwrapped, because
/// "cannot fail" is a claim about today's fields.
pub(crate) fn write(document: &EditDocument) -> Result<String, EditDocumentError> {
    serde_json::to_string_pretty(document).map_err(|error| EditDocumentError::Shape {
        message: error.to_string(),
    })
}

/// Reads the version out of a raw document.
///
/// Read from the JSON rather than from a deserialised structure because that is
/// the whole point of it: a version-2 document may have a shape this build
/// cannot deserialise at all, and it still has to be able to say so as a
/// version rather than as a parse failure.
fn schema_version(document: &Map<String, Value>) -> Option<u32> {
    document
        .get(VERSION_KEY)
        .and_then(Value::as_u64)
        .and_then(|version| u32::try_from(version).ok())
}

/// Walks a document from `found` up to `target`, one step at a time.
///
/// Each step's `to` becomes the next step's `from`, so the chain is followed
/// rather than assumed to be a sequence of single increments. A version with no
/// step out of it stops the walk with
/// [`EditDocumentError::MigrationMissing`] and the caller writes nothing.
fn migrate(
    document: &mut Map<String, Value>,
    found: u32,
    target: u32,
    migrations: &[Migration],
) -> Result<(), EditDocumentError> {
    let mut current = found;
    while current < target {
        let step = migrations
            .iter()
            .find(|migration| migration.from == current && migration.to <= target)
            .ok_or(EditDocumentError::MigrationMissing {
                from: current,
                to: target,
            })?;
        // A step that did not advance would spin here for ever, and a step that
        // overshot would leave the document claiming a version it is not.
        debug_assert!(step.to > step.from, "a migration must advance the version");
        (step.apply)(document).map_err(|reason| EditDocumentError::MigrationFailed {
            from: step.from,
            to: step.to,
            reason,
        })?;
        document.insert(VERSION_KEY.to_owned(), Value::from(step.to));
        current = step.to;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::DocumentProblem;
    use crate::source::RecordingId;
    use crate::time::{SourceSpan, SourceTime};

    fn document() -> EditDocument {
        EditDocument::from_recording(
            "Ace",
            RecordingId::new("rec-1"),
            SourceSpan::new(SourceTime::ZERO, SourceTime::from_nanos(10_000_000_000))
                .expect("the test span ends after it starts"),
        )
    }

    /// A current document, as this build writes it.
    fn current_text() -> String {
        document().write().expect("a valid document writes")
    }

    /// The same document in a hypothetical format that called `title` `name`.
    ///
    /// No shipped version has ever called it that, so this is a fixture rather
    /// than history — exactly as the game catalogue's migration tests use one.
    /// It is what lets the *machinery* be tested against conversions the test
    /// owns both ends of: the chain, the refusal to convert what it cannot, the
    /// step that would overshoot, and the validation of the result.
    /// [`a_version_one_document_is_converted_by_the_migration_this_build_ships`]
    /// covers the real conversion.
    fn older_text() -> String {
        current_text().replace(r#""title""#, r#""name""#).replace(
            &format!(r#""schema_version": {SCHEMA_VERSION}"#),
            r#""schema_version": 1"#,
        )
    }

    /// A document as version 1 wrote one, with a solo saved into its mix.
    ///
    /// Written out rather than derived from what this build produces, because
    /// the point of it is the shape this build no longer writes.
    const VERSION_ONE: &str = r#"{
      "schema_version": 1,
      "title": "Ace",
      "aspect_ratio": null,
      "sources": [{ "id": 0, "recording": "rec-1" }],
      "segments": [
        {
          "source": 0,
          "span": { "start": 0, "end": 10000000000 },
          "speed": { "numerator": 1, "denominator": 1 },
          "crop": null,
          "rotation": "none"
        }
      ],
      "audio_tracks": [
        {
          "name": "Game",
          "inputs": [{ "source": 0, "stream": 0 }],
          "gain_db": -3.5,
          "muted": false,
          "soloed": false,
          "fade_in": 0,
          "fade_out": 1000000000
        },
        {
          "name": "Microphone",
          "inputs": [{ "source": 0, "stream": 1 }],
          "gain_db": 0.0,
          "muted": true,
          "soloed": true,
          "fade_in": 0,
          "fade_out": 0
        }
      ],
      "overlays": []
    }"#;

    /// Renames `name` back to `title`.
    fn rename_name_to_title(document: &mut Map<String, Value>) -> Result<(), String> {
        let Some(name) = document.remove("name") else {
            return Ok(());
        };
        document.insert("title".to_owned(), name);
        Ok(())
    }

    /// Marks the title, so that a second step is visible in the result.
    fn mark_migrated(document: &mut Map<String, Value>) -> Result<(), String> {
        let title = document
            .get("title")
            .and_then(Value::as_str)
            .ok_or_else(|| "the document has no `title`".to_owned())?
            .to_owned();
        document.insert("title".to_owned(), Value::from(format!("{title} (v3)")));
        Ok(())
    }

    const RENAME: Migration = Migration {
        from: 1,
        to: 2,
        apply: rename_name_to_title,
    };
    const MARK: Migration = Migration {
        from: 2,
        to: 3,
        apply: mark_migrated,
    };
    const REFUSES: Migration = Migration {
        from: 1,
        to: 2,
        apply: |_| Err("this edit is not something I can convert".to_owned()),
    };
    /// Converts the document into one that no longer validates.
    const BREAKS: Migration = Migration {
        from: 1,
        to: 2,
        apply: |document| {
            rename_name_to_title(document)?;
            document.insert("sources".to_owned(), Value::Array(Vec::new()));
            Ok(())
        },
    };

    #[test]
    fn a_document_round_trips_through_text_unchanged() {
        let original = document();
        let text = original.write().expect("it writes");
        let loaded = EditDocument::read(&text).expect("it reads back");

        assert_eq!(loaded.document, original);
        assert_eq!(loaded.migrated, None);
        assert_eq!(
            loaded.document.write().expect("it writes again"),
            text,
            "writing what was read must produce the same bytes, or a save with no \
             edits in it would still change the stored document"
        );
    }

    #[test]
    fn the_version_is_written_into_the_document() {
        let text = current_text();
        assert!(
            text.contains(&format!(r#""schema_version": {SCHEMA_VERSION}"#)),
            "{text}"
        );
    }

    #[test]
    fn a_document_with_no_version_is_refused() {
        let text = current_text().replace(r#""schema_version""#, r#""version""#);

        assert!(matches!(
            EditDocument::read(&text),
            Err(EditDocumentError::SchemaVersionMissing)
        ));
    }

    #[test]
    fn a_document_from_a_newer_build_is_refused_rather_than_read_badly() {
        let text = current_text().replace(
            &format!(r#""schema_version": {SCHEMA_VERSION}"#),
            r#""schema_version": 99"#,
        );

        let error = EditDocument::read(&text).expect_err("a newer document is refused");
        assert!(matches!(
            error,
            EditDocumentError::SchemaTooNew {
                found: 99,
                supported: SCHEMA_VERSION,
            }
        ));
    }

    #[test]
    fn a_field_this_build_does_not_know_is_refused_at_the_same_version() {
        // Every shape change bumps the version, so an unexpected key at the
        // current version is damage — and reading it would mean writing the
        // document back without whatever it said.
        let text = current_text().replace(
            r#""title""#,
            r#""transitions": [{"kind":"crossfade"}], "title""#,
        );

        let error = EditDocument::read(&text).expect_err("an unknown field is refused");
        assert!(
            matches!(&error, EditDocumentError::Shape { message } if message.contains("transitions")),
            "{error}"
        );
    }

    #[test]
    fn text_that_is_not_json_says_so() {
        let error = EditDocument::read("this is not an edit").expect_err("it is not JSON");
        assert!(matches!(error, EditDocumentError::Syntax { .. }), "{error}");

        let error = EditDocument::read("[1, 2, 3]").expect_err("a list is not an edit");
        assert!(matches!(error, EditDocumentError::Shape { .. }), "{error}");
    }

    #[test]
    fn a_document_that_says_something_impossible_is_refused_on_the_way_in() {
        let text = current_text().replace(r#""recording": "rec-1""#, r#""recording": """#);

        let error = EditDocument::read(&text).expect_err("a source with no recording is refused");
        assert!(
            matches!(
                error,
                EditDocumentError::Invalid(DocumentProblem::SourceWithoutRecording { .. })
            ),
            "{error}"
        );
    }

    #[test]
    fn an_older_document_is_converted_and_reported_as_converted() {
        let loaded = read(&older_text(), 2, &[RENAME]).expect("the migration runs");

        assert_eq!(loaded.document.title, "Ace");
        assert_eq!(loaded.migrated, Some(Migrated { from: 1, to: 2 }));
        assert_eq!(
            loaded.document.schema_version(),
            2,
            "the converted document carries the version it is now in"
        );
    }

    #[test]
    fn a_migration_chain_runs_every_step_in_order() {
        // Deliberately out of order in the list: the chain is followed by
        // matching each step's `from`, not by the order somebody wrote them.
        let loaded = read(&older_text(), 3, &[MARK, RENAME]).expect("both steps run");

        assert_eq!(loaded.document.title, "Ace (v3)");
        assert_eq!(loaded.migrated, Some(Migrated { from: 1, to: 3 }));
    }

    #[test]
    fn a_step_beyond_the_target_is_not_taken() {
        // Target 2 with both steps available: the 2-to-3 step must not run, or
        // the document would end up claiming a version this build cannot read.
        let loaded = read(&older_text(), 2, &[RENAME, MARK]).expect("only the first step runs");

        assert_eq!(loaded.document.title, "Ace");
        assert_eq!(loaded.document.schema_version(), 2);
    }

    #[test]
    fn a_step_that_overshoots_the_target_is_not_taken() {
        // The only route out of version 1 lands on version 3, and this build
        // reads 2. Taking it would leave the user with a document their own
        // Clipped then refuses.
        let overshoots = Migration {
            from: 1,
            to: 3,
            apply: rename_name_to_title,
        };

        let error =
            read(&older_text(), 2, &[overshoots]).expect_err("there is no route to version 2");
        assert!(
            matches!(
                error,
                EditDocumentError::MigrationMissing { from: 1, to: 2 }
            ),
            "{error}"
        );
    }

    #[test]
    fn an_older_document_with_no_migration_is_refused_and_says_it_was_left_alone() {
        let error = read(&older_text(), 2, &[]).expect_err("there is no route to version 2");

        assert!(
            matches!(
                error,
                EditDocumentError::MigrationMissing { from: 1, to: 2 }
            ),
            "{error}"
        );
        assert!(
            error.to_string().contains("left exactly as it was"),
            "the caller has to know not to write anything: {error}"
        );
    }

    #[test]
    fn a_migration_that_refuses_carries_its_own_reason_out() {
        let error = read(&older_text(), 2, &[REFUSES]).expect_err("the step refuses");

        assert!(
            matches!(
                error,
                EditDocumentError::MigrationFailed { from: 1, to: 2, .. }
            ),
            "{error}"
        );
        assert!(
            error
                .to_string()
                .contains("this edit is not something I can convert"),
            "the step's own reason should reach the user: {error}"
        );
    }

    #[test]
    fn a_migration_whose_result_does_not_validate_is_refused() {
        // The converted document plays a source it no longer declares. Reading
        // it would hand an exporter a clip it cannot render, so validation runs
        // after the conversion and not only before it.
        let error = read(&older_text(), 2, &[BREAKS]).expect_err("the result is not valid");

        assert!(
            matches!(
                error,
                EditDocumentError::Invalid(DocumentProblem::UnknownSource { segment: 0, .. })
            ),
            "{error}"
        );
    }

    #[test]
    fn a_version_one_document_is_converted_by_the_migration_this_build_ships() {
        // The real conversion, through the real list: a version 1 document
        // carrying `soloed` on its tracks opens, says it was converted, and
        // arrives with the mix it described — minus the solo, which was never
        // part of the clip (`crate::audio`).
        let loaded = EditDocument::read(VERSION_ONE).expect("a version 1 clip still opens");

        assert_eq!(loaded.migrated, Some(Migrated { from: 1, to: 2 }));
        assert_eq!(loaded.document.schema_version(), SCHEMA_VERSION);
        assert_eq!(loaded.document.audio_tracks.len(), 2);
        assert_eq!(loaded.document.audio_tracks[0].gain_db, -3.5);
        assert_eq!(
            loaded.document.audio_tracks[0].fade_out,
            core::time::Duration::from_secs(1)
        );
        assert!(
            loaded.document.audio_tracks[1].muted,
            "a mute is an edit and survives the conversion"
        );

        let text = loaded
            .document
            .write()
            .expect("the converted document is one this build can store");
        assert!(
            !text.contains("solo"),
            "the converted document must not carry a solo anywhere: {text}"
        );
    }

    #[test]
    fn a_version_one_document_that_left_the_solo_out_converts_just_the_same() {
        // `soloed` had a default, so a version 1 document need not carry it.
        // The step has to be silent about that rather than refusing.
        let without = VERSION_ONE
            .replace(r#""soloed": false,"#, "")
            .replace(r#""soloed": true,"#, "");

        let loaded = EditDocument::read(&without).expect("it opens");
        assert_eq!(loaded.migrated, Some(Migrated { from: 1, to: 2 }));
        assert_eq!(loaded.document.audio_tracks.len(), 2);
    }

    #[test]
    fn a_version_one_document_is_refused_without_the_step_that_converts_it() {
        // The evidence that the assertions above are the migration's work and
        // not `serde` shrugging at a key it does not know: with no step from 1,
        // the same text is refused rather than read.
        let error =
            read(VERSION_ONE, SCHEMA_VERSION, &[]).expect_err("there is no route to version 2");

        assert!(
            matches!(
                error,
                EditDocumentError::MigrationMissing { from: 1, to: 2 }
            ),
            "{error}"
        );
    }

    #[test]
    fn every_shipped_migration_advances_the_version_and_stops_at_this_one() {
        for step in MIGRATIONS {
            assert!(
                step.to > step.from,
                "{step:?} does not advance the version, so the walk would not end"
            );
            assert!(
                step.to <= SCHEMA_VERSION,
                "{step:?} lands past the version this build reads"
            );
        }
        // Every version there has ever been needs a route to this one, or a
        // document somebody saved is a document they cannot open. Walked
        // rather than counted, because the chain is not assumed to be single
        // increments anywhere else either.
        for version in 1..SCHEMA_VERSION {
            let mut current = version;
            while let Some(step) = MIGRATIONS
                .iter()
                .find(|step| step.from == current && step.to <= SCHEMA_VERSION)
            {
                current = step.to;
            }
            assert_eq!(
                current, SCHEMA_VERSION,
                "a document in format {version} gets as far as {current} and no further"
            );
        }
    }

    #[test]
    fn writing_is_deterministic() {
        let document = document();
        assert_eq!(
            document.write().expect("it writes"),
            document.write().expect("it writes again"),
            "two saves of an unchanged document must not differ"
        );
    }
}
