//! What [issue #84](https://github.com/wildware-uk/clipped/issues/84) is
//! actually about, checked against a model of the timeline that shares no code
//! with the one under test.
//!
//! An operation is easy to write in a way that passes every assertion about
//! segment spans and still exports the wrong frames, because the spans are what
//! the implementation manipulates: assert on them and the test agrees with the
//! code by construction. So this file keeps the clip a second way — as the list
//! of *moments of the original timeline that survived*, edited by removing
//! elements from a `Vec` — and asks the document where each of those moments
//! comes from. If the two disagree, the arithmetic that maps output time to
//! source time is wrong, and it is wrong here rather than in an export nobody
//! can debug.
//!
//! The export engine ([issue
//! #89](https://github.com/wildware-uk/clipped/issues/89)) does not exist, so
//! "exported output matches the edited timeline" cannot be checked end to end
//! yet. What can be checked is the half that will be blamed for it: every
//! position an exporter would ask about, answered exactly.

use clipped_edit::{
    AudioTrack, EditDocument, EditHistory, EditOperation, OutputSpan, OutputTime, OverlayPosition,
    RecordingId, Segment, Source, SourceId, SourceSpan, SourceTime, TextOverlay, TrackInput,
};

const SECOND: u64 = 1_000_000_000;

/// How finely the timeline is walked: ten milliseconds, about a frame at 100fps.
const STEP: u64 = 10_000_000;

fn span(start_nanos: u64, end_nanos: u64) -> SourceSpan {
    SourceSpan::new(
        SourceTime::from_nanos(start_nanos),
        SourceTime::from_nanos(end_nanos),
    )
    .expect("the test span ends after it starts")
}

fn when(start_nanos: u64, end_nanos: u64) -> OutputSpan {
    OutputSpan::new(
        OutputTime::from_nanos(start_nanos),
        OutputTime::from_nanos(end_nanos),
    )
    .expect("the test span ends after it starts")
}

/// A thirty-second clip of two recordings, with a mix and a title over it.
///
/// Every segment plays at its recorded speed, which is what trimming, splitting
/// and deleting produce on their own; the interaction with a speed change is
/// [issue #86](https://github.com/wildware-uk/clipped/issues/86)'s, and the
/// unit tests in `operations.rs` cover the arithmetic for it.
fn clip() -> EditDocument {
    let first = SourceId::new(0);
    let second = SourceId::new(1);

    EditDocument::new("Round 12 ace")
        .with_source(Source::new(first, RecordingId::new("rec-a")))
        .with_source(Source::new(second, RecordingId::new("rec-b")))
        .with_segment(Segment::new(first, span(30 * SECOND, 40 * SECOND)))
        .with_segment(Segment::new(first, span(90 * SECOND, 102 * SECOND)))
        .with_segment(Segment::new(second, span(5 * SECOND, 13 * SECOND)))
        .with_audio_track(
            AudioTrack::new(
                "Game",
                vec![TrackInput::new(first, 0), TrackInput::new(second, 0)],
            )
            .at_gain_db(-3.0)
            .with_fades(
                core::time::Duration::from_secs(2),
                core::time::Duration::from_secs(3),
            ),
        )
        .with_audio_track(AudioTrack::new("Microphone", vec![TrackInput::new(first, 1)]).muted())
        .with_overlay(
            TextOverlay::new("Round 12", when(SECOND, 25 * SECOND))
                .at(OverlayPosition::new(0.5, 0.85).expect("a valid position"))
                .sized(7),
        )
}

/// Which recording is on screen at `at`, and where in it, as an export reads it.
fn material_at(document: &EditDocument, at: u64) -> (String, u64) {
    let placement = document
        .locate(OutputTime::from_nanos(at))
        .expect("the position is inside the clip");
    let recording = document
        .source(placement.source)
        .expect("a validated document declares every source it plays")
        .recording
        .as_str()
        .to_owned();
    (recording, placement.source_time.as_nanos())
}

/// The moments of the *original* timeline a clip is expected to still play.
///
/// The second model: a plain list, edited by dropping elements from it. It
/// knows nothing about segments, spans or speeds — an element's position in the
/// list is its place on the edited timeline, and its value is where it came
/// from.
fn surviving_moments(clip_nanos: u64) -> Vec<u64> {
    (0..clip_nanos / STEP).map(|step| step * STEP).collect()
}

/// A position on the edited timeline as an index into that list.
fn sample(at_nanos: u64) -> usize {
    usize::try_from(at_nanos / STEP).expect("a sample index fits in a usize")
}

/// Asserts that `edited` plays `expected`, moment for moment.
fn assert_plays(original: &EditDocument, edited: &EditDocument, expected: &[u64], after: &str) {
    let duration = edited
        .output_duration_nanos()
        .expect("an edited document has a duration");
    assert_eq!(
        duration / STEP,
        u64::try_from(expected.len()).expect("the sample count fits in a u64"),
        "after {after}, the clip is not as long as what survived"
    );

    for (index, moment) in expected.iter().enumerate() {
        let at = u64::try_from(index).expect("the index fits in a u64") * STEP;
        assert_eq!(
            material_at(edited, at),
            material_at(original, *moment),
            "after {after}, {at} ns of the clip should play what {moment} ns used to"
        );
    }
}

#[test]
fn trimming_splitting_and_deleting_leave_a_clip_that_plays_exactly_what_survived() {
    let original = clip();
    let clip_nanos = original
        .output_duration_nanos()
        .expect("the fixture is readable");
    assert_eq!(clip_nanos, 30 * SECOND);

    let mut moments = surviving_moments(clip_nanos);
    let mut document = original.clone();

    // A split changes the shape of the document and nothing about what it
    // plays, which is the easiest thing in this file to get wrong unnoticed.
    document = document
        .apply(EditOperation::Split {
            at: OutputTime::from_nanos(15 * SECOND),
        })
        .expect("fifteen seconds in is inside the clip");
    assert_eq!(document.segments.len(), 4);
    assert_plays(&original, &document, &moments, "a split at 15s");

    // Delete four seconds out of the middle of the second recording's segment.
    document = document
        .apply(EditOperation::DeleteSection {
            range: when(23 * SECOND, 27 * SECOND),
        })
        .expect("the range is inside the clip");
    moments.drain(sample(23 * SECOND)..sample(27 * SECOND));
    assert_plays(&original, &document, &moments, "deleting 23s to 27s");

    // Delete a range that spans a boundary between two segments.
    document = document
        .apply(EditOperation::DeleteSection {
            range: when(8 * SECOND, 12 * SECOND),
        })
        .expect("the range is inside the clip");
    moments.drain(sample(8 * SECOND)..sample(12 * SECOND));
    assert_plays(&original, &document, &moments, "deleting 8s to 12s");

    // Then trim both ends of what is left.
    document = document
        .apply(EditOperation::TrimStart {
            at: OutputTime::from_nanos(2 * SECOND),
        })
        .expect("two seconds in is inside the clip");
    moments.drain(..sample(2 * SECOND));
    assert_plays(&original, &document, &moments, "trimming the first 2s");

    document = document
        .apply(EditOperation::TrimEnd {
            at: OutputTime::from_nanos(15 * SECOND),
        })
        .expect("fifteen seconds in is inside the clip");
    moments.truncate(sample(15 * SECOND));
    assert_plays(&original, &document, &moments, "trimming back to 15s");

    // And the result is a clip that can be stored and opened again.
    let text = document.write().expect("an edited clip saves");
    let reloaded = EditDocument::read(&text).expect("and opens again");
    assert_eq!(reloaded.document, document);
    assert_eq!(reloaded.migrated, None);
    assert_plays(&original, &reloaded.document, &moments, "a save and a load");
}

#[test]
fn undo_restores_the_exact_text_that_would_have_been_stored() {
    let mut history = EditHistory::new(clip());
    let mut expected = vec![history.document().write().expect("the clip saves")];

    for operation in [
        EditOperation::TrimStart {
            at: OutputTime::from_nanos(3 * SECOND),
        },
        EditOperation::DeleteSection {
            range: when(5 * SECOND, 9 * SECOND),
        },
        EditOperation::Split {
            at: OutputTime::from_nanos(4 * SECOND),
        },
        EditOperation::TrimEnd {
            at: OutputTime::from_nanos(12 * SECOND),
        },
        // A change to the mix is an edit like any other, so it has to undo like
        // any other: the level the user dragged away from must come back
        // exactly, and not as the level a fresh document would have had.
        EditOperation::SetTrackGain {
            track: 0,
            gain_db: -11.5,
        },
        EditOperation::SetTrackFades {
            track: 1,
            fade_in: core::time::Duration::from_secs(1),
            fade_out: core::time::Duration::from_secs(2),
        },
    ] {
        assert!(
            history.apply(operation).expect("the operation applies"),
            "{operation:?} should have changed the document"
        );
        expected.push(history.document().write().expect("the edited clip saves"));
    }

    for text in expected.iter().rev().skip(1) {
        assert!(history.undo());
        assert_eq!(
            &history.document().write().expect("the clip saves"),
            text,
            "undo has to restore the exact prior state, down to the stored text"
        );
    }
    assert!(!history.can_undo());

    for text in expected.iter().skip(1) {
        assert!(history.redo());
        assert_eq!(&history.document().write().expect("the clip saves"), text);
    }
    assert!(!history.can_redo());
}
