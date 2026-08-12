//! AGENTS.md sections 56 and 57, held to on every path an export can take.
//!
//! > Do not re-encode or modify source recordings merely because the user
//! > created a clip.
//!
//! An export writes a *new* file. The recording it drew on is the one thing in
//! this product nobody can recreate, so "it was not modified" is asserted
//! against its bytes rather than against the intention — on the succeeding
//! path, on the refusing path, on the failing path and on the cancelled path.
//!
//! The last test is a different kind of assertion and is the cheaper guarantee:
//! the reader's own source is checked for any way of opening a file that could
//! write to one. A recording cannot be damaged by code that never opens it for
//! writing.

use core::time::Duration;

use clipped_edit::{EditDocument, RecordingId, SourceSpan, SourceTime};
use clipped_export::{export, ExportError, ExportOptions, SourceFiles};
use clipped_media_validation::{require_media_tools, TemporaryDirectory};

mod support;

use support::{contents, keyframes, packets_of, recording, seconds_to_nanos};

const VIDEO: usize = 0;

fn recording_id() -> RecordingId {
    RecordingId::new("rec-1")
}

fn span(start_nanos: u64, end_nanos: u64) -> SourceSpan {
    SourceSpan::new(
        SourceTime::from_nanos(start_nanos),
        SourceTime::from_nanos(end_nanos),
    )
    .expect("the test span ends after it starts")
}

#[test]
fn the_recording_is_unchanged_whether_the_export_succeeds_is_refused_or_fails() {
    let Some(tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("export-untouched");
    let source = recording(&tools, &directory, "match.mkv", 4);
    let before = contents(&source);
    let sources = SourceFiles::new().with(recording_id(), &source);

    let keyframes = keyframes(tools.ffprobe(), &source, VIDEO);
    let copyable = EditDocument::from_recording(
        "Ace",
        recording_id(),
        span(
            seconds_to_nanos(keyframes[1]),
            seconds_to_nanos(keyframes[2]),
        ),
    );

    // Succeeding.
    let destination = directory.file("ace.mkv");
    export(&copyable, &sources, &destination, &ExportOptions::new())
        .expect("the clip can be exported");
    assert_eq!(
        contents(&source),
        before,
        "a successful export modified the recording"
    );

    // Refused, because the cut is not on a keyframe.
    let between = packets_of(tools.ffprobe(), &source, VIDEO)
        .into_iter()
        .find(|packet| !packet.keyframe && packet.presentation_seconds > 0.0)
        .expect("the fixture has a picture that is not a keyframe");
    let refused = EditDocument::from_recording(
        "Ace",
        recording_id(),
        span(
            seconds_to_nanos(between.presentation_seconds),
            seconds_to_nanos(between.presentation_seconds + 0.5),
        ),
    );
    let error = export(
        &refused,
        &sources,
        &directory.file("refused.mkv"),
        &ExportOptions::new(),
    )
    .expect_err("a cut between keyframes cannot be copied");
    assert!(
        matches!(error, ExportError::ReencodeRequired { .. }),
        "{error}"
    );
    assert_eq!(
        contents(&source),
        before,
        "a refused export modified the recording"
    );

    // Failing, because the destination is taken. The file that was already
    // there must survive too: nothing here truncates anything.
    let occupied = directory.file("occupied.mkv");
    std::fs::write(&occupied, b"this is not a clip").expect("the test can write a file");
    let taken = contents(&occupied);
    let error = export(&copyable, &sources, &occupied, &ExportOptions::new())
        .expect_err("a destination that is taken is refused");
    assert!(matches!(error, ExportError::Output { .. }), "{error}");
    assert_eq!(
        contents(&occupied),
        taken,
        "the export truncated a file that was already at the destination"
    );
    assert_eq!(
        contents(&source),
        before,
        "a failed export modified the recording"
    );

    // And a recording that cannot be found at all.
    let missing = SourceFiles::new().with(recording_id(), directory.file("nothing.mkv"));
    let error = export(
        &copyable,
        &missing,
        &directory.file("nowhere.mkv"),
        &ExportOptions::new(),
    )
    .expect_err("a recording that is not there cannot be exported");
    assert!(
        matches!(error, ExportError::SourceUnreadable { .. }),
        "{error}"
    );
    assert_eq!(contents(&source), before);
}

#[test]
fn an_export_reads_the_recording_and_has_no_way_of_writing_to_one() {
    // The cheaper guarantee, and the one that keeps holding when this crate
    // grows: code that never opens a recording for writing cannot damage one.
    // `avformat_open_input` opens for reading, and the writing calls below are
    // the ones that would not.
    //
    // Asserted against the source of the module that owns the FFmpeg reading
    // rather than against a paragraph in it, which is what
    // `crates/edit/tests/sources_are_never_touched.rs` does for the document
    // model.
    let reader = include_str!("../src/media.rs");

    for forbidden in [
        "avio_open",
        "AVIO_FLAG_WRITE",
        "avformat_write_header",
        "avformat_alloc_output_context2",
        "av_interleaved_write_frame",
        "OpenOptions",
        "File::create",
        "fs::write",
        "remove_file",
    ] {
        assert!(
            !reader.contains(forbidden),
            "the recording reader names `{forbidden}`, which is a way of writing to a file"
        );
    }

    assert!(
        reader.contains("avformat_open_input"),
        "the reader does not open a container at all, so this assertion is checking nothing"
    );
}

#[test]
fn exporting_the_same_clip_twice_produces_the_same_file() {
    // A copy is deterministic: the same document over the same recording gives
    // the same coded packets at the same times. Worth asserting because it is
    // what makes an export re-runnable after a cancelled attempt, and because a
    // difference here would mean something in the path had a clock or an
    // allocation address in it (AGENTS.md section 25).
    let Some(tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("export-deterministic");
    let source = recording(&tools, &directory, "match.mkv", 4);
    let sources = SourceFiles::new().with(recording_id(), &source);

    let keyframes = keyframes(tools.ffprobe(), &source, VIDEO);
    let document = EditDocument::from_recording(
        "Ace",
        recording_id(),
        span(
            seconds_to_nanos(keyframes[0]),
            seconds_to_nanos(keyframes[2]),
        ),
    );

    let first = directory.file("first.mkv");
    let second = directory.file("second.mkv");
    export(&document, &sources, &first, &ExportOptions::new()).expect("the first export");
    // Far enough apart that a timestamp in the file would differ.
    std::thread::sleep(Duration::from_millis(20));
    export(&document, &sources, &second, &ExportOptions::new()).expect("the second export");

    let one: Vec<_> = packets_of(tools.ffprobe(), &first, VIDEO)
        .into_iter()
        .map(|packet| (packet.presentation_seconds.to_bits(), packet.hash))
        .collect();
    let two: Vec<_> = packets_of(tools.ffprobe(), &second, VIDEO)
        .into_iter()
        .map(|packet| (packet.presentation_seconds.to_bits(), packet.hash))
        .collect();

    assert_eq!(one, two, "two exports of the same clip differ");
}
