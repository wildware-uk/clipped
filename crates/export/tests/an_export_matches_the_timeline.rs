//! The claim [issue #84](https://github.com/wildware-uk/clipped/issues/84) is
//! waiting on: the exported file shows what the edited timeline says it should,
//! within a stated tolerance.
//!
//! # The tolerance, and how it is measured
//!
//! The expected picture times are not computed from anything in
//! `clipped-export`. They are read out of the **source recording** with
//! `ffprobe` — an outside program, parsing the bytes on disk — and moved onto
//! the output timeline by the one rule `docs/editing.md` fixes:
//!
//! ```text
//!   output = segment.output_start + (source − segment.span.start)
//! ```
//!
//! for every picture presented at or after `span.start` and strictly before
//! `span.end`. The exported file's picture times are then read out of the
//! export the same way and compared against that list, one for one.
//!
//! The agreement is exact to **one millisecond**, which is not a slack the
//! exporter needs: it is the resolution Matroska stores timestamps at, so a
//! picture at 1.2345 s is stored as 1.234 s or 1.235 s and no writer can do
//! better. **No picture is lost, none is duplicated, and none moves by a
//! frame.** `crates/export/tests/support/mod.rs` is where the probing lives.

use clipped_edit::{EditDocument, RecordingId, Segment, Source, SourceId, SourceSpan, SourceTime};
use clipped_export::{export, plan_export, ExportMethod, ExportOptions, SourceFiles};
use clipped_media_validation::{
    require_media_tools, AudioStream, Media, TemporaryDirectory, VideoStream,
};

mod support;

use support::{contents, keyframes, packets_of, recording, seconds_to_nanos};

/// How far an exported timestamp may be from the one the timeline asks for.
///
/// One Matroska tick. See the module documentation.
const TOLERANCE_SECONDS: f64 = 0.001;

/// The recording's video stream, in both the source and the export: Clipped
/// writes the picture first and so does `ffmpeg`.
const VIDEO: usize = 0;

/// The recording's audio stream, in both files, for the same reason.
const AUDIO: usize = 1;

const RECORDING: &str = "rec-1";

fn recording_id() -> RecordingId {
    RecordingId::new(RECORDING)
}

fn span(start_nanos: u64, end_nanos: u64) -> SourceSpan {
    SourceSpan::new(
        SourceTime::from_nanos(start_nanos),
        SourceTime::from_nanos(end_nanos),
    )
    .expect("the test span ends after it starts")
}

/// What the timeline says the exported pictures should be, in output seconds.
///
/// Read out of the source with `ffprobe` and moved by the segment offsets, so
/// that the expectation comes from the recording rather than from the code
/// being tested.
fn expected_output_seconds(
    source_packets: &[support::ProbedPacket],
    ranges: &[(f64, f64)],
) -> Vec<f64> {
    let mut expected = Vec::new();
    let mut output_start = 0.0;
    for (start, end) in ranges {
        for packet in source_packets {
            let at = packet.presentation_seconds;
            if at >= *start && at < *end {
                expected.push(output_start + (at - start));
            }
        }
        output_start += end - start;
    }
    expected
}

/// Compares two lists of timestamps, saying which one differs first.
fn assert_timeline(actual: &[support::ProbedPacket], expected: &[f64]) {
    let actual_seconds: Vec<f64> = actual
        .iter()
        .map(|packet| packet.presentation_seconds)
        .collect();

    assert_eq!(
        actual_seconds.len(),
        expected.len(),
        "the export holds {} pictures and the timeline says {}: {actual_seconds:?} against \
         {expected:?}",
        actual_seconds.len(),
        expected.len()
    );

    for (index, (found, wanted)) in actual_seconds.iter().zip(expected).enumerate() {
        assert!(
            (found - wanted).abs() <= TOLERANCE_SECONDS,
            "picture {index} is at {found}s and the timeline puts it at {wanted}s, which is \
             more than {TOLERANCE_SECONDS}s out"
        );
    }
}

#[test]
fn a_trim_on_keyframes_exports_exactly_the_pictures_the_timeline_says() {
    let Some(tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("export-trim");
    let source = recording(&tools, &directory, "match.mkv", 6);
    let destination = directory.file("ace.mkv");

    // Cut on real keyframes, found in the file rather than assumed from the
    // arguments the encoder was given.
    let keyframes = keyframes(tools.ffprobe(), &source, VIDEO);
    assert!(
        keyframes.len() >= 4,
        "the fixture has {} keyframes, which is not enough to cut between: {keyframes:?}",
        keyframes.len()
    );
    let (start, end) = (keyframes[1], keyframes[3]);

    let document = EditDocument::from_recording(
        "Ace",
        recording_id(),
        span(seconds_to_nanos(start), seconds_to_nanos(end)),
    );
    let sources = SourceFiles::new().with(recording_id(), &source);

    let before = contents(&source);
    let plan = plan_export(&document, &sources).expect("the clip can be planned");
    assert_eq!(
        plan.method(),
        ExportMethod::StreamCopy,
        "a cut on two keyframes with nothing else changed has to be a copy: {:?}",
        plan.blockers()
    );

    let exported = export(&document, &sources, &destination, &ExportOptions::new())
        .expect("the clip can be exported");

    assert_eq!(
        contents(&source),
        before,
        "the recording was modified by exporting a clip of it"
    );

    // What the file is, asserted with the workspace's own harness.
    let media = Media::open(&destination).expect("the export opens");
    let source_media = Media::open(&source).expect("the recording opens");
    let expected_frames = expected_output_seconds(
        &packets_of(tools.ffprobe(), &source, VIDEO),
        &[(start, end)],
    );
    media
        .validate()
        .stream_count(2)
        .video(
            VideoStream::codec("h264")
                .resolution(320, 240)
                .decoded_frames(expected_frames.len() as u64),
        )
        .audio_stream_count(1)
        .audio(0, AudioStream::codec("pcm_s16le").sample_rate(48_000))
        .duration_seconds(end - start, 0.2)
        .monotonic_timestamps()
        .assert_valid();

    // And where every picture is, against what the timeline says.
    assert_timeline(
        &packets_of(tools.ffprobe(), &destination, VIDEO),
        &expected_frames,
    );

    assert_eq!(
        exported.frames(),
        expected_frames.len() as u64,
        "the export reported a different number of pictures from the one it wrote"
    );
    assert!(
        source_media.duration_seconds().unwrap_or_default() > end - start,
        "the recording is not longer than the clip, so this proves nothing about trimming"
    );
}

#[test]
fn a_delete_in_the_middle_joins_the_two_halves_with_no_frame_lost_or_repeated() {
    // What #84's split-then-delete produces: two segments of one recording laid
    // end to end, with the material between them gone. The join is the place a
    // frame is most easily duplicated or dropped, and the comparison below is
    // one-for-one over every picture in the file.
    let Some(tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("export-delete");
    let source = recording(&tools, &directory, "match.mkv", 6);
    let destination = directory.file("ace.mkv");

    let keyframes = keyframes(tools.ffprobe(), &source, VIDEO);
    assert!(keyframes.len() >= 5, "{keyframes:?}");
    let ranges = [(keyframes[0], keyframes[1]), (keyframes[3], keyframes[4])];

    let source_id = SourceId::new(0);
    let document = EditDocument::new("Ace")
        .with_source(Source::new(source_id, recording_id()))
        .with_segment(Segment::new(
            source_id,
            span(seconds_to_nanos(ranges[0].0), seconds_to_nanos(ranges[0].1)),
        ))
        .with_segment(Segment::new(
            source_id,
            span(seconds_to_nanos(ranges[1].0), seconds_to_nanos(ranges[1].1)),
        ));
    let sources = SourceFiles::new().with(recording_id(), &source);

    let before = contents(&source);
    let exported = export(&document, &sources, &destination, &ExportOptions::new())
        .expect("the clip can be exported");
    assert_eq!(contents(&source), before, "the recording was modified");

    let source_packets = packets_of(tools.ffprobe(), &source, VIDEO);
    let expected = expected_output_seconds(&source_packets, &ranges);
    let exported_packets = packets_of(tools.ffprobe(), &destination, VIDEO);

    assert_timeline(&exported_packets, &expected);

    // The material between the two segments is not in the export at all: the
    // deleted pictures are the ones whose payloads are missing from it.
    let deleted: Vec<&support::ProbedPacket> = source_packets
        .iter()
        .filter(|packet| {
            packet.presentation_seconds >= ranges[0].1 && packet.presentation_seconds < ranges[1].0
        })
        .collect();
    assert!(
        !deleted.is_empty(),
        "the test deleted nothing, so it proves nothing"
    );
    let written: Vec<&str> = exported_packets
        .iter()
        .map(|packet| packet.hash.as_str())
        .collect();
    for packet in deleted {
        assert!(
            !written.contains(&packet.hash.as_str()),
            "a picture the edit deleted, at {}s, is in the export",
            packet.presentation_seconds
        );
    }

    assert_eq!(
        exported.plan().method(),
        ExportMethod::StreamCopy,
        "{:?}",
        exported.plan().blockers()
    );
    Media::open(&destination)
        .expect("the export opens")
        .validate()
        .video(VideoStream::codec("h264").decoded_frames(expected.len() as u64))
        .monotonic_timestamps()
        .assert_valid();
}

#[test]
fn a_copy_writes_the_recordings_own_coded_pictures_and_not_new_ones() {
    // The whole argument for copying: the export is the recording's own coded
    // bytes in a new container, so nothing was traded for the cut. Counting
    // frames would pass just as happily on a file that had been re-encoded.
    let Some(tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("export-lossless");
    let source = recording(&tools, &directory, "match.mkv", 4);
    let destination = directory.file("ace.mkv");

    let keyframes = keyframes(tools.ffprobe(), &source, VIDEO);
    let (start, end) = (keyframes[1], keyframes[2]);

    let document = EditDocument::from_recording(
        "Ace",
        recording_id(),
        span(seconds_to_nanos(start), seconds_to_nanos(end)),
    );
    let sources = SourceFiles::new().with(recording_id(), &source);

    export(&document, &sources, &destination, &ExportOptions::new())
        .expect("the clip can be exported");

    let expected: Vec<String> = packets_of(tools.ffprobe(), &source, VIDEO)
        .into_iter()
        .filter(|packet| packet.presentation_seconds >= start && packet.presentation_seconds < end)
        .map(|packet| packet.hash)
        .collect();
    let written: Vec<String> = packets_of(tools.ffprobe(), &destination, VIDEO)
        .into_iter()
        .map(|packet| packet.hash)
        .collect();

    assert!(!expected.is_empty(), "the range holds no pictures");
    assert_eq!(
        written, expected,
        "the exported pictures are not the recording's own coded bytes, so something \
         re-encoded them"
    );
}

#[test]
fn an_edit_that_says_nothing_about_audio_exports_the_recordings_audio() {
    // An instant clip declares no mix. Exporting it silent, or without the
    // track, would be a clip of a match with the sound turned off — and the
    // person who found out would be the one who uploaded it.
    let Some(tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("export-audio");
    let source = recording(&tools, &directory, "match.mkv", 4);
    let destination = directory.file("ace.mkv");

    let keyframes = keyframes(tools.ffprobe(), &source, VIDEO);
    let document = EditDocument::from_recording(
        "Ace",
        recording_id(),
        span(
            seconds_to_nanos(keyframes[1]),
            seconds_to_nanos(keyframes[2]),
        ),
    );
    let sources = SourceFiles::new().with(recording_id(), &source);

    export(&document, &sources, &destination, &ExportOptions::new())
        .expect("the clip can be exported");

    Media::open(&destination)
        .expect("the export opens")
        .validate()
        .audio_stream_count(1)
        .audio(
            0,
            AudioStream::codec("pcm_s16le")
                .sample_rate(48_000)
                // The name the recording gave the track, carried across: a file
                // with an unnamed audio track is one whose tracks have to be
                // identified by listening to them.
                .title("Game"),
        )
        .assert_valid();

    // And the sound is the recording's own sound, moved onto the clip's
    // timeline by the same rule the picture was: every audio packet the range
    // covers, at the time the timeline puts it, and nothing else.
    //
    // Audio is cut on packet boundaries, so the *ends* of a segment are
    // accurate to one audio packet rather than exactly — the fixture's audio
    // packets are 21 ms apart and no keyframe falls on one. That is the
    // tolerance `docs/exporting.md` states for sound.
    let expected_audio = expected_output_seconds(
        &packets_of(tools.ffprobe(), &source, AUDIO),
        &[(keyframes[1], keyframes[2])],
    );
    let audio = packets_of(tools.ffprobe(), &destination, AUDIO);
    assert_timeline(&audio, &expected_audio);

    let last = audio
        .last()
        .expect("the export has audio packets")
        .presentation_seconds;
    let length = keyframes[2] - keyframes[1];
    assert!(
        last < length,
        "the audio runs to {last}s in a clip that is {length}s long"
    );
    assert!(
        last >= length - 0.1,
        "the audio stops at {last}s in a clip that is {length}s long"
    );
    assert!(
        audio
            .first()
            .expect("the export has audio packets")
            .presentation_seconds
            <= 0.05,
        "the audio does not start at the start of the clip"
    );
}

#[test]
fn a_cut_that_is_not_on_a_keyframe_is_refused_rather_than_moved() {
    // `docs/editing.md`: "the boundary in the document is what the output must
    // show, and a segment whose start is not a keyframe is re-encoded from the
    // cut". Re-encoding is not built, so the export says so and writes nothing
    // — which is the honest form of that gap (AGENTS.md section 54).
    let Some(tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("export-refusal");
    let source = recording(&tools, &directory, "match.mkv", 4);
    let destination = directory.file("ace.mkv");

    let packets = packets_of(tools.ffprobe(), &source, VIDEO);
    let between = packets
        .iter()
        .find(|packet| !packet.keyframe && packet.presentation_seconds > 0.0)
        .expect("the fixture has a picture that is not a keyframe");

    let document = EditDocument::from_recording(
        "Ace",
        recording_id(),
        span(
            seconds_to_nanos(between.presentation_seconds),
            seconds_to_nanos(between.presentation_seconds + 1.0),
        ),
    );
    let sources = SourceFiles::new().with(recording_id(), &source);

    let before = contents(&source);
    let error = export(&document, &sources, &destination, &ExportOptions::new())
        .expect_err("a cut between keyframes cannot be copied");

    let message = error.to_string();
    assert!(
        message.contains("re-encoded"),
        "the refusal has to say what would be needed: {message}"
    );
    assert!(
        message.contains("not a picture a decoder can start at"),
        "the refusal has to name the reason: {message}"
    );
    assert!(
        !destination.exists(),
        "a refused export left a file behind at {}",
        destination.display()
    );
    assert_eq!(contents(&source), before, "the recording was modified");
}
