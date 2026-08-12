//! Saving and reloading an edit must not change what it plays, and must not
//! quietly drop any part of it.
//!
//! Comparing the two documents with `==` would be the easy version of this and
//! would prove less than it looks: two documents can be equal and still be read
//! differently if the reading depends on anything outside them. So this test
//! *reads* both, the way an export does — walking the whole output timeline and
//! asking, at each step, which recording is on screen, which frame of it, which
//! text is over it and how loud each audio track is — and compares the answers.
//!
//! That sampled transcript is the closest thing to "identical playback" a model
//! with no decoder can assert, and it is the same question [issue
//! #89](https://github.com/wildware-uk/clipped/issues/89) will ask several
//! thousand times per export.
//!
//! # Why the fixture is checked as well as the round trip
//!
//! A round trip only covers the fields the fixture happens to set to something
//! other than their default. An earlier version of this file left
//! `aspect_ratio` and `soloed` at theirs, and marking both
//! `#[serde(skip_serializing)]` — so that every save silently discarded them —
//! left the whole suite green. In a *non-destructive* editing model, a save
//! that loses an edit is the failure this crate exists to prevent.
//!
//! So [`every_field_of_a_document_is_set_to_something_other_than_its_default`]
//! compares the fixture against a baseline document built from this crate's
//! plain constructors, field by field, over the **serialised** form — which is
//! what a save and a reload actually carry, and which reaches the private
//! fields of `SourceSpan`, `Speed` and the rest. Every value the baseline holds
//! must appear somewhere in the fixture with a different value, so a field
//! added to any structure in this crate later arrives at that constructor's
//! value on both sides, compares equal, and fails the test by name. The two
//! together are the acceptance criterion: the fixture exercises every field,
//! and the round trip proves every field survives.

use core::time::Duration;
use std::collections::BTreeMap;

use clipped_edit::{
    AspectRatio, AudioTrack, CropRect, EditDocument, OutputSpan, OutputTime, OverlayPosition,
    RecordingId, Rotation, Segment, Source, SourceId, SourceSpan, SourceTime, Speed, TextOverlay,
    TrackInput, TrackOutput, SCHEMA_VERSION,
};
use serde_json::Value;

/// What the clip is doing at one moment.
#[derive(Debug, Clone, PartialEq)]
struct Frame {
    at_nanos: u64,
    /// The segment, source and position in that recording, or `None` past the
    /// end of the clip.
    material: Option<(usize, u32, u64)>,
    overlays: Vec<String>,
    audio: Vec<TrackOutput>,
}

/// Reads the clip at one-tenth of a second, from before it starts to past its
/// end.
fn transcript(document: &EditDocument) -> Vec<Frame> {
    let duration = document
        .output_duration_nanos()
        .expect("a validated document has a duration");
    let step = 100_000_000;

    (0..=(duration / step + 2))
        .map(|index| {
            let at_nanos = index * step;
            let at = OutputTime::from_nanos(at_nanos);
            Frame {
                at_nanos,
                material: document.locate(at).map(|placement| {
                    (
                        placement.segment,
                        placement.source.get(),
                        placement.source_time.as_nanos(),
                    )
                }),
                overlays: document
                    .overlays_at(at)
                    .map(|overlay| overlay.text.clone())
                    .collect(),
                audio: (0..document.audio_tracks.len())
                    .map(|track| {
                        document
                            .track_output(track)
                            .expect("the track is in the document")
                    })
                    .collect(),
            }
        })
        .collect()
}

fn span(start_nanos: u64, end_nanos: u64) -> SourceSpan {
    SourceSpan::new(
        SourceTime::from_nanos(start_nanos),
        SourceTime::from_nanos(end_nanos),
    )
    .expect("the span ends after it starts")
}

fn when(start_nanos: u64, end_nanos: u64) -> OutputSpan {
    OutputSpan::new(
        OutputTime::from_nanos(start_nanos),
        OutputTime::from_nanos(end_nanos),
    )
    .expect("the span ends after it starts")
}

/// An edit using every part of the model: two recordings, a cut, a speed
/// change, a crop, a rotation, an exported shape, a mix with a mute, a solo and
/// a fade, and two overlays.
///
/// Every field of every structure this crate writes is set here to something
/// other than what a plain constructor would give it, which is what makes the
/// round trip below cover the whole document rather than the parts somebody
/// remembered. `every_field_of_a_document_is_set_to_something_other_than_its_default`
/// holds that claim to account.
fn combined_edit() -> EditDocument {
    let first = SourceId::new(3);
    let second = SourceId::new(7);

    EditDocument::new("Two rounds")
        .with_aspect_ratio(AspectRatio::VERTICAL)
        .with_source(Source::new(first, RecordingId::new("rec-1")))
        .with_source(Source::new(second, RecordingId::new("rec-2")))
        .with_segment(Segment::new(first, span(30_000_000_000, 38_000_000_000)))
        .with_segment(
            Segment::new(first, span(92_000_000_000, 104_000_000_000))
                .at_speed(Speed::new(3, 2).expect("a valid speed"))
                .cropped_to(CropRect::new(0.05, 0.1, 0.9, 0.8).expect("a valid crop"))
                .rotated(Rotation::Clockwise90),
        )
        .with_segment(
            Segment::new(second, span(5_000_000_000, 9_000_000_000))
                .at_speed(Speed::new(1, 4).expect("a valid speed")),
        )
        .with_audio_track(
            AudioTrack::new(
                "Game",
                vec![TrackInput::new(first, 0), TrackInput::new(second, 0)],
            )
            .at_gain_db(-6.5)
            .with_fades(Duration::from_millis(750), Duration::from_secs(2)),
        )
        .with_audio_track(AudioTrack::new("Microphone", vec![TrackInput::new(first, 1)]).muted())
        .with_audio_track(
            AudioTrack::new("Discord", vec![TrackInput::new(first, 2)])
                .at_gain_db(-0.1)
                .soloed(),
        )
        .with_overlay(
            TextOverlay::new("Round 12", when(0, 2_500_000_000))
                .at(OverlayPosition::new(0.25, 0.85).expect("a valid position"))
                .sized(7),
        )
        .with_overlay(TextOverlay::new(
            "Round 13",
            when(2_000_000_000, 9_000_000_000),
        ))
}

/// The same document with nothing chosen: one of everything, as this crate's
/// own constructors build it.
///
/// The baseline the fixture is measured against. It deliberately holds a value
/// for the *optional* fields too — a crop and an aspect ratio — because a field
/// left `null` here is a field nothing underneath it compares, and the point of
/// the comparison is that it cannot be quietly narrowed.
/// [`the_baseline_holds_a_value_for_every_field`] asserts that directly, so an
/// `Option` field added to this crate later fails there until it is given a
/// value in both documents.
fn plain_edit() -> EditDocument {
    let only = SourceId::new(0);

    EditDocument::new("Untitled")
        .with_aspect_ratio(AspectRatio::WIDESCREEN)
        .with_source(Source::new(only, RecordingId::new("recording")))
        .with_segment(Segment::new(only, span(0, 1)).cropped_to(CropRect::FULL))
        .with_audio_track(AudioTrack::new("Track", vec![TrackInput::new(only, 0)]))
        .with_overlay(TextOverlay::new("Text", when(0, 1)))
}

/// Every value a document's text holds, keyed by where in the document it sits.
///
/// Array indices collapse to `[]`, so `segments[].speed.numerator` names the
/// speed of every segment at once and a value that is only interesting on the
/// second of them still counts. Reading the serialised form rather than the
/// Rust structures is deliberate: it is exactly what a save and a reload carry,
/// it reaches the private fields the model does not expose, and a field added
/// later appears here without anybody having to remember it.
fn values_by_path(document: &EditDocument) -> BTreeMap<String, Vec<Value>> {
    let text = document.write().expect("the document saves");
    let value: Value = serde_json::from_str(&text).expect("what this crate writes is JSON");
    let mut paths = BTreeMap::new();
    collect(&value, String::new(), &mut paths);
    paths
}

fn collect(value: &Value, path: String, into: &mut BTreeMap<String, Vec<Value>>) {
    match value {
        Value::Object(fields) => {
            for (key, field) in fields {
                let child = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                collect(field, child, into);
            }
        }
        // An empty list is recorded as a value of its own rather than skipped,
        // so that a document holding none of something is not silently
        // indistinguishable from one holding several.
        Value::Array(items) if items.is_empty() => {
            into.entry(format!("{path}[]"))
                .or_default()
                .push(Value::Array(Vec::new()));
        }
        Value::Array(items) => {
            for item in items {
                collect(item, format!("{path}[]"), into);
            }
        }
        leaf => into.entry(path).or_default().push(leaf.clone()),
    }
}

#[test]
fn the_baseline_holds_a_value_for_every_field() {
    // The baseline is what the fixture is measured against, so a hole in it is
    // a hole in the measurement. A `null` is an optional field nobody filled
    // in, and an empty list is a whole structure never reached — either way the
    // fields underneath are compared against nothing at all.
    let plain = values_by_path(&plain_edit());

    let empty: Vec<&String> = plain
        .iter()
        .filter(|(_, values)| {
            values
                .iter()
                .any(|value| value.is_null() || value == &Value::Array(Vec::new()))
        })
        .map(|(path, _)| path)
        .collect();
    assert!(
        empty.is_empty(),
        "the baseline document must hold a real value for every field, including the \
         optional ones, or nothing compares what is inside them. Empty at: {empty:?}"
    );

    let several: Vec<&String> = plain
        .iter()
        .filter(|(_, values)| values.len() > 1)
        .map(|(path, _)| path)
        .collect();
    assert!(
        several.is_empty(),
        "the baseline holds exactly one of everything, so that each of its values is the \
         single thing the fixture has to differ from. Several at: {several:?}"
    );
}

#[test]
fn every_field_of_a_document_is_set_to_something_other_than_its_default() {
    // The test that keeps the round trip below honest, and the one a newly
    // added field fails. A field the fixture leaves at its default cannot be
    // seen to be lost by a save: it comes back as that default either way, so
    // `skip_serializing` on it goes unnoticed and the round trip proves nothing
    // about it.
    let plain = values_by_path(&plain_edit());
    let full = values_by_path(&combined_edit());

    assert_eq!(
        full.get("schema_version"),
        Some(&vec![Value::from(SCHEMA_VERSION)]),
        "every document this build writes carries the current format version, which is \
         the one field that must not differ"
    );

    let mut untested = Vec::new();
    for (path, baseline) in &plain {
        if path == "schema_version" {
            continue;
        }
        let default = baseline
            .first()
            .expect("a path is recorded because it has a value");
        match full.get(path) {
            None => untested.push(format!("{path} (the fixture holds nothing there at all)")),
            Some(values) if values.iter().all(|value| value == default) => {
                untested.push(format!("{path} (still {default})"));
            }
            Some(_) => {}
        }
    }

    assert!(
        untested.is_empty(),
        "these fields are still at the value a plain constructor gives them, so saving \
         and reloading the fixture cannot show whether they survive. Give each of them a \
         different value in `combined_edit`: {untested:#?}"
    );
}

#[test]
fn a_saved_and_reloaded_edit_plays_exactly_the_same() {
    let original = combined_edit();
    original.validate().expect("the edit is valid");

    let text = original.write().expect("it saves");
    let reloaded = EditDocument::read(&text).expect("it loads").document;

    assert_eq!(reloaded, original, "the documents should be equal");
    assert_eq!(
        transcript(&reloaded),
        transcript(&original),
        "and reading them should give the same answers, all the way through"
    );
}

#[test]
fn the_transcript_is_not_vacuous() {
    // A test comparing two empty transcripts would pass against anything, so
    // this is the assertion that the one above is looking at something.
    let document = combined_edit();
    let transcript = transcript(&document);

    assert!(transcript.len() > 200, "{} samples", transcript.len());
    assert!(
        transcript.iter().any(|frame| frame.material.is_none()),
        "the walk should run off the end of the clip"
    );
    assert_eq!(
        transcript
            .iter()
            .filter_map(|frame| frame.material.map(|(segment, ..)| segment))
            .max(),
        Some(2),
        "and should visit every segment"
    );
    assert!(
        transcript
            .iter()
            .any(|frame| frame.overlays.len() == 2 && frame.audio.len() == 3),
        "and should catch both overlays on screen at once, with the whole mix"
    );
    assert!(
        transcript.iter().all(|frame| frame.audio
            == [
                TrackOutput::Silent,
                TrackOutput::Silent,
                TrackOutput::Audible { gain_db: -0.1 },
            ]),
        "and should read the mix the solo rule gives — the soloed track alone, at its own \
         level, with the merely loud one silenced beside the muted one — so that a solo \
         lost in a save changes the transcript and not only the structure"
    );
}

#[test]
fn saving_a_reloaded_edit_produces_the_same_text() {
    // The other half of round-tripping, and the one that matters for a
    // database: opening a clip and saving it with no changes must not rewrite
    // the stored document into something different.
    let text = combined_edit().write().expect("it saves");
    let reloaded = EditDocument::read(&text).expect("it loads").document;

    assert_eq!(reloaded.write().expect("it saves again"), text);
}

#[test]
fn a_fractional_level_survives_being_written_as_text() {
    // Decibels are the one part of the document that is not an integer, and a
    // level that came back as -6.4999999 would be a mix that drifts every time
    // the user opens the clip.
    let original = combined_edit();
    let reloaded = EditDocument::read(&original.write().expect("it saves"))
        .expect("it loads")
        .document;

    for (before, after) in original
        .audio_tracks
        .iter()
        .zip(reloaded.audio_tracks.iter())
    {
        assert_eq!(
            before.gain_db.to_bits(),
            after.gain_db.to_bits(),
            "`{}` came back at a different level",
            before.name
        );
    }
    assert_eq!(
        reloaded.audio_tracks[2].gain_db.to_bits(),
        (-0.1_f64).to_bits()
    );
}

/// Every place in a document's text where a key could be added, as a pointer.
fn object_pointers(value: &Value, pointer: String, into: &mut Vec<String>) {
    match value {
        Value::Object(fields) => {
            into.push(pointer.clone());
            for (key, field) in fields {
                object_pointers(field, format!("{pointer}/{key}"), into);
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                object_pointers(item, format!("{pointer}/{index}"), into);
            }
        }
        _ => {}
    }
}

#[test]
fn a_key_this_build_does_not_understand_is_refused_wherever_it_appears() {
    // The other way a save loses an edit. Every shape change bumps the schema
    // version (`clipped_edit::schema`), so an unexpected key at the current
    // version is damage — and a structure that shrugged at one would be read,
    // and then written back without whatever it said. `deny_unknown_fields`
    // therefore belongs on every structure in this crate rather than on the
    // outermost two, which is what this sweeps: a key is pushed into each
    // object of a fully populated document in turn, including the ones nested
    // inside a segment, a track and an overlay, and each must be refused by
    // name. A structure added later is swept without anybody listing it here.
    const UNKNOWN: &str = "a_key_this_build_does_not_understand";

    let text = combined_edit().write().expect("it saves");
    let document: Value = serde_json::from_str(&text).expect("what this crate writes is JSON");

    let mut pointers = Vec::new();
    object_pointers(&document, String::new(), &mut pointers);
    assert!(
        pointers.len() > 10,
        "a fully populated document is more than a handful of objects: {pointers:?}"
    );

    for pointer in pointers {
        let mut damaged = document.clone();
        damaged
            .pointer_mut(&pointer)
            .and_then(Value::as_object_mut)
            .expect("the pointer names an object of the document")
            .insert(UNKNOWN.to_owned(), Value::from("crossfade"));

        let text = serde_json::to_string(&damaged).expect("the damaged document serialises");
        match EditDocument::read(&text) {
            Ok(_) => panic!(
                "a key this build does not understand was accepted at `{pointer}`, so the \
                 next save would write the document back without whatever it said"
            ),
            Err(error) => assert!(
                error.to_string().contains(UNKNOWN),
                "the refusal at `{pointer}` should name the key: {error}"
            ),
        }
    }
}
