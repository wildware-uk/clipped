//! Where the margin in `docs/exporting.md` comes from.
//!
//! [Issue #90](https://github.com/wildware-uk/clipped/issues/90) asks for an
//! estimated size "within a documented margin of the actual output". A margin
//! that is asserted is worth nothing, so this is the measurement it is taken
//! from: real recordings, planned and then exported by the real exporter, with
//! `ExportPlan::size` compared against the bytes the finished file occupies on
//! disk.
//!
//! # What is compared, and against what
//!
//! Two different things, to two very different accuracies:
//!
//! - **The coded media is exact.** A copy writes the recording's own packets, so
//!   the plan's `media_bytes` and the export's `byte_len` are compared for
//!   *equality*. Nothing here is a tolerance: if the estimate ever counted a
//!   packet the copy does not write, or missed one it does, this is where it
//!   shows.
//! - **The finished file is within a margin.** The container's header, its
//!   track declarations, its cluster framing and its cues are the writer's
//!   business and are modelled rather than counted, so the file is compared
//!   against `EstimatedSize::MARGIN`. Every byte of the disagreement lives
//!   there.
//!
//! The size on disk is read with `std::fs::metadata`, not from anything in
//! `clipped-export`: what a user wants to know is how much room the file will
//! take, and that is a question for the filesystem.
//!
//! # The range it is measured over
//!
//! One case is not a margin. The cases below cover clips from a fifth of a
//! second to the whole of a recording, one segment and two and three,
//! recordings with no sound track and with one and with two, and a keyframe
//! interval four times the writer's cluster window as well as one equal to it —
//! because the packet count, the cluster count, the keyframe count and the
//! track count are the four things the container model is a function of, and a
//! fixture where two of them coincide cannot tell a right model from a wrong
//! one. The observed error across all of them is printed by the test and
//! written down in `docs/exporting.md`.
//!
//! # What fails when the estimate drifts
//!
//! Three guards, in increasing tolerance. The media comparison is exact. The
//! *container* comparison — the modelled container against the file less its
//! media — is the one that notices a changed constant, because the documented
//! margin is a fraction of a file that is almost all media and would swallow a
//! container model that had doubled. The documented margin is asserted last,
//! and is what a caller is promised.

use std::path::Path;

use clipped_edit::{EditDocument, RecordingId, Segment, Source, SourceId, SourceSpan, SourceTime};
use clipped_export::{
    export, plan_export, EstimatedSize, ExportMethod, ExportOptions, SizeEstimate, SizeUnknown,
    SourceFiles,
};
use clipped_media_validation::{require_media_tools, TemporaryDirectory};

mod support;

use support::{keyframes, packets_of, recording_with, recording_with_sound, seconds_to_nanos};

/// The recording's video stream, in the source and in the export.
const VIDEO: usize = 0;

const RECORDING: &str = "rec-1";

/// How long each fixture is, in seconds.
///
/// Long enough to hold a dozen keyframes, so that a clip can be one keyframe
/// interval or the whole file and the two are meaningfully different sizes.
const FIXTURE_SECONDS: u32 = 12;

/// How far the modelled container may be from the container that was written.
///
/// A much tighter question than `EstimatedSize::MARGIN`, and the one that
/// notices a change: the file is mostly coded media, so a container model that
/// was wrong by half would still be inside the documented margin of a long clip.
/// The largest disagreement observed across the cases below is 1.7%.
const CONTAINER_TOLERANCE: f64 = 0.03;

/// How many pictures apart the sparse fixture's keyframes are asked to be.
///
/// Four seconds at the fixtures' ten pictures a second, which is four times the
/// window the container writer closes a cluster after. See the case that uses
/// it.
const SPARSE_KEYFRAME_INTERVAL: u32 = 40;

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

/// A document playing `ranges` of one recording, in the order they are given.
fn document_over(ranges: &[(f64, f64)]) -> EditDocument {
    let source = SourceId::new(0);
    let mut document = EditDocument::new("Ace").with_source(Source::new(source, recording_id()));
    for (start, end) in ranges {
        document = document.with_segment(Segment::new(
            source,
            span(seconds_to_nanos(*start), seconds_to_nanos(*end)),
        ));
    }
    document
}

/// What one export turned out to cost, against what its plan said it would.
#[derive(Debug)]
struct Measured {
    what: String,
    estimated: u64,
    on_disk: u64,
    /// The coded media, which the plan and the export agree on exactly.
    media: u64,
    /// What the plan modelled the container at.
    modelled_container: u64,
}

impl Measured {
    /// How far out the estimate was, as a fraction of the file that was written.
    fn error(&self) -> f64 {
        let on_disk = self.on_disk as f64;
        (self.estimated as f64 - on_disk) / on_disk
    }

    /// What the container really cost: everything in the file that is not the
    /// coded media, which is exactly the part the estimate models.
    fn container(&self) -> u64 {
        self.on_disk.saturating_sub(self.media)
    }
}

/// Plans, exports, and compares the two answers.
///
/// # Panics
///
/// When the clip is not a copy — every case here is one, and a case that
/// silently became a re-encode would be measuring the refusal rather than the
/// estimate.
fn measure(what: &str, source: &Path, destination: &Path, ranges: &[(f64, f64)]) -> Measured {
    let document = document_over(ranges);
    let sources = SourceFiles::new().with(recording_id(), source);

    let plan = plan_export(&document, &sources).expect("the clip can be planned");
    assert_eq!(
        plan.method(),
        ExportMethod::StreamCopy,
        "{what} is not a copy, so it measures nothing: {:?}",
        plan.blockers()
    );
    let SizeEstimate::Estimated(estimate) = plan.size() else {
        panic!("{what} is a copy and has no estimate: {}", plan.size());
    };

    let exported = export(&document, &sources, destination, &ExportOptions::new())
        .expect("the clip can be exported");

    // The exact half. The estimate counted the packets the copy would write, and
    // this is the copy saying how many bytes of packet it wrote.
    assert_eq!(
        estimate.media_bytes(),
        exported.byte_len(),
        "{what}: the plan counted {} bytes of coded media and the export wrote {}",
        estimate.media_bytes(),
        exported.byte_len()
    );

    let on_disk = std::fs::metadata(destination)
        .expect("the export can be measured")
        .len();
    assert!(
        on_disk > estimate.media_bytes(),
        "{what}: the file is {on_disk} bytes and holds {} of media, so the container costs \
         nothing, which is not possible",
        estimate.media_bytes()
    );

    Measured {
        what: what.to_owned(),
        estimated: estimate.bytes(),
        on_disk,
        media: estimate.media_bytes(),
        modelled_container: estimate.container_bytes(),
    }
}

#[test]
fn an_export_is_the_size_its_plan_said_it_would_be() {
    let Some(tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("export-size");

    // Three recordings, differing in the one thing the container model is most
    // sensitive to: how many tracks it has to declare and interleave.
    let silent = recording_with_sound(&tools, &directory, "silent.mkv", FIXTURE_SECONDS, 0);
    let one_track = recording_with_sound(&tools, &directory, "match.mkv", FIXTURE_SECONDS, 1);
    let two_tracks = recording_with_sound(&tools, &directory, "party.mkv", FIXTURE_SECONDS, 2);

    // Cuts on real keyframes, found in each file rather than assumed from the
    // arguments the encoder was given.
    let cuts = keyframes(tools.ffprobe(), &one_track, VIDEO);
    assert!(
        cuts.len() >= 8,
        "the fixture has {} keyframes, which is not enough to cut a range of clips out of: \
         {cuts:?}",
        cuts.len()
    );
    let silent_cuts = keyframes(tools.ffprobe(), &silent, VIDEO);
    let two_track_cuts = keyframes(tools.ffprobe(), &two_tracks, VIDEO);

    let end_of = |file: &Path| {
        packets_of(tools.ffprobe(), file, VIDEO)
            .last()
            .expect("the fixture holds pictures")
            .presentation_seconds
            + 1.0
    };

    let mut measurements = Vec::new();
    let mut export_number = 0;
    let mut next = |name: &str| {
        export_number += 1;
        directory.file(&format!("{name}-{export_number}.mkv"))
    };

    // One sound track, across the lengths a clip is cut to.
    for (label, ranges) in [
        (
            "a fifth of a second, one track",
            vec![(cuts[1], cuts[1] + 0.2)],
        ),
        ("one keyframe interval, one track", vec![(cuts[1], cuts[2])]),
        ("three intervals, one track", vec![(cuts[1], cuts[4])]),
        (
            "the whole recording, one track",
            vec![(0.0, end_of(&one_track))],
        ),
        (
            "two segments, one track",
            vec![(cuts[0], cuts[2]), (cuts[4], cuts[6])],
        ),
        (
            "three segments, one track",
            vec![(cuts[0], cuts[1]), (cuts[2], cuts[3]), (cuts[5], cuts[7])],
        ),
    ] {
        measurements.push(measure(label, &one_track, &next("one"), &ranges));
    }

    // No sound at all: the picture and the container, and nothing else.
    for (label, ranges) in [
        (
            "a fifth of a second, no sound",
            vec![(silent_cuts[1], silent_cuts[1] + 0.2)],
        ),
        (
            "one keyframe interval, no sound",
            vec![(silent_cuts[1], silent_cuts[2])],
        ),
        (
            "the whole recording, no sound",
            vec![(0.0, end_of(&silent))],
        ),
    ] {
        measurements.push(measure(label, &silent, &next("silent"), &ranges));
    }

    // Two sound tracks: twice the audio packets, and a second declaration.
    for (label, ranges) in [
        (
            "three intervals, two tracks",
            vec![(two_track_cuts[1], two_track_cuts[4])],
        ),
        (
            "two segments, two tracks",
            vec![
                (two_track_cuts[0], two_track_cuts[2]),
                (two_track_cuts[4], two_track_cuts[6]),
            ],
        ),
    ] {
        measurements.push(measure(label, &two_tracks, &next("two"), &ranges));
    }

    // Keyframes four seconds apart, which is four times the window the writer
    // closes a cluster after. This is the case that separates a cluster from a
    // keyframe: a model that counted one cluster per keyframe is short by three
    // cluster headers a keyframe here, and right on every fixture above.
    let sparse = recording_with(
        &tools,
        &directory,
        "sparse.mkv",
        FIXTURE_SECONDS,
        1,
        SPARSE_KEYFRAME_INTERVAL,
    );
    let sparse_cuts = keyframes(tools.ffprobe(), &sparse, VIDEO);
    assert!(
        sparse_cuts.len() >= 3,
        "the sparse fixture has {} keyframes: {sparse_cuts:?}",
        sparse_cuts.len()
    );
    assert!(
        sparse_cuts[1] - sparse_cuts[0] > 1.0,
        "the sparse fixture's keyframes are {}s apart, which is inside the cluster window, so it \
         measures the same thing as the fixtures above",
        sparse_cuts[1] - sparse_cuts[0]
    );
    for (label, ranges) in [
        (
            "one sparse interval, one track",
            vec![(sparse_cuts[0], sparse_cuts[1])],
        ),
        ("the whole sparse recording", vec![(0.0, end_of(&sparse))]),
        (
            "two sparse segments",
            vec![
                (sparse_cuts[0], sparse_cuts[1]),
                (sparse_cuts[2], end_of(&sparse)),
            ],
        ),
    ] {
        measurements.push(measure(label, &sparse, &next("sparse"), &ranges));
    }

    // The table `docs/exporting.md` quotes. Printed rather than only asserted,
    // because the documented margin has to be re-read off a run of this test
    // whenever the estimate or the writer changes.
    println!(
        "{:<34} {:>10} {:>10} {:>8} {:>10} {:>10}",
        "case", "estimate", "on disk", "error", "modelled", "container"
    );
    for measured in &measurements {
        println!(
            "{:<34} {:>10} {:>10} {:>7.3}% {:>10} {:>10}",
            measured.what,
            measured.estimated,
            measured.on_disk,
            measured.error() * 100.0,
            measured.modelled_container,
            measured.container()
        );
    }

    let worst = measurements
        .iter()
        .max_by(|left, right| {
            left.error()
                .abs()
                .partial_cmp(&right.error().abs())
                .expect("an error is a real number")
        })
        .expect("there are measurements");
    println!(
        "worst: {} at {:.2}%, against a documented margin of {:.2}%",
        worst.what,
        worst.error() * 100.0,
        EstimatedSize::MARGIN * 100.0
    );

    // The sharper of the two guards, and the one that catches drift. The
    // documented margin is a fraction of the *file*, which is mostly media, so a
    // container model that doubled would still pass it on a long clip. This
    // compares the modelled container against the container the writer really
    // wrote — the file less the coded media — where a wrong constant has
    // nowhere to hide.
    for measured in &measurements {
        let container = measured.container() as f64;
        let modelled = measured.modelled_container as f64;
        let out = (modelled - container) / container;
        assert!(
            out.abs() <= CONTAINER_TOLERANCE,
            "{}: the container was modelled at {} bytes and cost {}, which is {:.1}% out — more \
             than the {:.1}% this has been measured at. One of the constants in \
             crates/export/src/plan.rs no longer describes what clipped-muxer writes.",
            measured.what,
            measured.modelled_container,
            measured.container(),
            out * 100.0,
            CONTAINER_TOLERANCE * 100.0
        );
    }

    for measured in &measurements {
        assert!(
            measured.error().abs() <= EstimatedSize::MARGIN,
            "{}: the plan estimated {} bytes and the file is {}, which is {:.2}% out — more than \
             the {:.2}% docs/exporting.md documents. Either the estimate has drifted or the \
             margin is no longer the truth, and the documented figure has to be re-measured \
             rather than widened.",
            measured.what,
            measured.estimated,
            measured.on_disk,
            measured.error() * 100.0,
            EstimatedSize::MARGIN * 100.0
        );
    }
}

#[test]
fn a_clip_that_would_be_re_encoded_carries_no_size_at_all() {
    // The other half of the acceptance criterion. Re-encoding is not built and
    // has no quality setting, so its size is not a number anything here knows —
    // and a figure on an export dialog is the figure somebody decides whether
    // they have room for (AGENTS.md section 27). The plan says it does not know,
    // and a caller drawing it has something to draw instead of a zero.
    let Some(tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("export-size-unknown");
    let source = recording_with_sound(&tools, &directory, "match.mkv", 4, 1);

    let between = packets_of(tools.ffprobe(), &source, VIDEO)
        .into_iter()
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

    let plan = plan_export(&document, &sources).expect("the clip can be planned");

    assert_eq!(plan.method(), ExportMethod::Reencode);
    assert_eq!(plan.size(), SizeEstimate::Unknown(SizeUnknown::Reencode));
    assert_eq!(
        plan.size().bytes(),
        None,
        "a re-encode has no estimated size, and a zero would be a figure rather than an absence"
    );

    let said = plan.size().to_string();
    assert!(
        said.contains("nothing has chosen"),
        "the plan has to say why it cannot say: {said}"
    );
}
