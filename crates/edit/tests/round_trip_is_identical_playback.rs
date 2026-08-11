//! Saving and reloading an edit must not change what it plays.
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

use core::time::Duration;

use clipped_edit::{
    AudioTrack, CropRect, EditDocument, OutputSpan, OutputTime, OverlayPosition, RecordingId,
    Rotation, Segment, Source, SourceId, SourceSpan, SourceTime, Speed, TextOverlay, TrackInput,
    TrackOutput,
};

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
/// change, a crop, a rotation, a mix with a mute and a fade, and two overlays.
fn combined_edit() -> EditDocument {
    let first = SourceId::new(0);
    let second = SourceId::new(7);

    EditDocument::new("Two rounds")
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
            AudioTrack::new("Discord", vec![TrackInput::new(first, 2)]).at_gain_db(-0.1),
        )
        .with_overlay(
            TextOverlay::new("Round 12", when(0, 2_500_000_000))
                .at(OverlayPosition::new(0.5, 0.85).expect("a valid position"))
                .sized(7),
        )
        .with_overlay(TextOverlay::new(
            "Round 13",
            when(2_000_000_000, 9_000_000_000),
        ))
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
