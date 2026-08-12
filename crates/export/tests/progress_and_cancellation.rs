//! An export is the longest thing a user waits for, so it has to say how far it
//! has got and it has to stop when asked — leaving nothing behind when it does.
//!
//! Both are acceptance criteria of
//! [issue #89](https://github.com/wildware-uk/clipped/issues/89): "progress
//! reporting, cancellation and cleanup of partial output", and "cancelling
//! leaves no partial file behind".

use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;
use std::sync::Mutex;

use clipped_edit::{EditDocument, RecordingId, SourceSpan, SourceTime};
use clipped_export::{
    export, Cancellation, ExportError, ExportOptions, ExportProgress, SourceFiles,
};
use clipped_media_validation::{require_media_tools, Media, TemporaryDirectory};

mod support;

use support::{contents, keyframes, recording, seconds_to_nanos};

const VIDEO: usize = 0;
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

#[test]
fn progress_climbs_to_the_end_of_the_clip_and_never_past_it() {
    let Some(tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("export-progress");
    let source = recording(&tools, &directory, "match.mkv", 6);
    let destination = directory.file("ace.mkv");

    let keyframes = keyframes(tools.ffprobe(), &source, VIDEO);
    let (start, end) = (keyframes[0], keyframes[4]);
    let document = EditDocument::from_recording(
        "Ace",
        recording_id(),
        span(seconds_to_nanos(start), seconds_to_nanos(end)),
    );
    let sources = SourceFiles::new().with(recording_id(), &source);

    let reports: Mutex<Vec<ExportProgress>> = Mutex::new(Vec::new());
    let record = |progress: ExportProgress| {
        reports
            .lock()
            .expect("no test thread panics while holding this")
            .push(progress);
    };
    let options = ExportOptions::new()
        .reporting_to(&record)
        // Every picture, so that a four-second clip produces a list worth
        // asserting on rather than a handful of points.
        .every(Duration::ZERO);

    let exported =
        export(&document, &sources, &destination, &options).expect("the clip can be exported");

    let reports = reports.into_inner().expect("the lock is not poisoned");
    assert!(
        reports.len() > 10,
        "a {}s clip produced {} progress reports",
        end - start,
        reports.len()
    );

    let mut previous = 0.0;
    for report in &reports {
        let fraction = report.fraction();
        assert!(
            (0.0..=1.0).contains(&fraction),
            "a progress report was {fraction}"
        );
        assert!(
            fraction >= previous,
            "progress went backwards, from {previous} to {fraction}"
        );
        previous = fraction;
        assert_eq!(report.total_nanos, exported.duration().as_nanos() as u64);
    }

    let last = reports.last().expect("there is at least one report");
    assert!(
        (last.fraction() - 1.0).abs() < f64::EPSILON,
        "the last report was {} rather than the end of the clip",
        last.fraction()
    );
    assert_eq!(
        last.packets,
        exported.packets(),
        "the last report disagrees with what was written"
    );
}

#[test]
fn cancelling_stops_the_export_and_leaves_no_partial_file_behind() {
    let Some(tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("export-cancel");
    let source = recording(&tools, &directory, "match.mkv", 6);
    let destination = directory.file("ace.mkv");

    let keyframes = keyframes(tools.ffprobe(), &source, VIDEO);
    let (start, end) = (keyframes[0], keyframes[4]);
    let document = EditDocument::from_recording(
        "Ace",
        recording_id(),
        span(seconds_to_nanos(start), seconds_to_nanos(end)),
    );
    let sources = SourceFiles::new().with(recording_id(), &source);

    let before = contents(&source);
    let cancellation = Cancellation::new();
    let seen = AtomicU64::new(0);
    // Cancels as soon as the export is under way, which is what a user pressing
    // the button in the middle of one does. Waiting for a few reports first is
    // what makes this a cancellation of a *running* export rather than of one
    // that had not started.
    let stop = |progress: ExportProgress| {
        if seen.fetch_add(1, Ordering::Relaxed) >= 2 {
            cancellation.cancel();
        }
        let _ = progress;
    };
    let options = ExportOptions::new()
        .reporting_to(&stop)
        .every(Duration::ZERO)
        .cancelled_by(cancellation.clone());

    let error = export(&document, &sources, &destination, &options)
        .expect_err("a cancelled export produces no clip");

    assert!(
        matches!(error, ExportError::Cancelled { .. }),
        "a cancelled export reported {error}"
    );
    assert!(
        seen.load(Ordering::Relaxed) > 2,
        "the export never got far enough to be cancelled mid-flight"
    );
    assert!(
        !destination.exists(),
        "a cancelled export left {} behind",
        destination.display()
    );
    assert_eq!(
        contents(&source),
        before,
        "a cancelled export modified the recording"
    );

    // And the name is usable again, which it would not be if a partial file
    // were still there: nothing here overwrites a file.
    let finished = export(&document, &sources, &destination, &ExportOptions::new())
        .expect("the same clip exports after a cancelled attempt");
    assert!(finished.frames() > 0);
    Media::open(&destination)
        .expect("the second attempt opens")
        .validate()
        .monotonic_timestamps()
        .assert_valid();
}

#[test]
fn an_export_cancelled_before_it_starts_writes_nothing_at_all() {
    let Some(tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("export-cancel-early");
    let source = recording(&tools, &directory, "match.mkv", 4);
    let destination = directory.file("ace.mkv");

    let keyframes = keyframes(tools.ffprobe(), &source, VIDEO);
    let document = EditDocument::from_recording(
        "Ace",
        recording_id(),
        span(
            seconds_to_nanos(keyframes[0]),
            seconds_to_nanos(keyframes[2]),
        ),
    );
    let sources = SourceFiles::new().with(recording_id(), &source);

    let cancellation = Cancellation::new();
    cancellation.cancel();
    let options = ExportOptions::new().cancelled_by(cancellation);

    let error = export(&document, &sources, &destination, &options)
        .expect_err("an export cancelled before it began produces no clip");

    assert!(matches!(error, ExportError::Cancelled { .. }), "{error}");
    assert!(
        !destination.exists(),
        "{} was left behind",
        destination.display()
    );
}
