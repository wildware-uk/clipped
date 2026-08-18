//! `clipped-recorder record` against a real window, and the file it leaves.
//!
//! This is the milestone-defining claim checked rather than assumed: the
//! recorder captures a window, encodes it, writes it, and stops cleanly leaving
//! a playable file ([issue #126](https://github.com/wildware-uk/clipped/issues/126),
//! and acceptance criterion 3 of
//! [issue #9](https://github.com/wildware-uk/clipped/issues/9)).
//!
//! Nothing here is mocked. The subject is `test-apps/video-pattern`, a real
//! window on a real display; the recorder is the built binary, run as a child
//! process the way a user runs it; the signal is a real `CTRL_C_EVENT`; and the
//! file is read back by the pinned FFmpeg build through
//! `clipped-media-validation`, which is the workspace's one media validator
//! (AGENTS.md section 22).
//!
//! # What is asserted, and what is not
//!
//! Asserted: the container opens, it holds exactly one video stream, that
//! stream carries the codec and the picture size the recorder said it wrote,
//! its timestamps increase, its duration matches how long the recording ran,
//! the frame rate it declares is the rate it sustained rather than the
//! `--framerate` ceiling it was recorded under, and **every frame the recorder
//! says it encoded is a frame that decodes out of the file**. That last one is
//! what makes this more than "a file exists": the recorder's own count and the
//! decoder's count are two independent accounts of the same recording, and a
//! pipeline that dropped packets, wrote them to the wrong track or duplicated
//! them would have to be wrong in both.
//!
//! The Ctrl+C test additionally asserts that the three finalisation lines
//! appear **in the order they happen**
//! ([`assert_finalisation_was_logged_in_order`]), which is what separates
//! "flushed, then the trailer written" from the other way round.
//!
//! **Not** asserted: that the decoded pictures are the *frames the source drew*,
//! in order. The video pattern draws a decodable counter into every frame for
//! exactly that check, and the decoder for it lives in `clipped-video-pattern`
//! — which the workspace's layering puts above this package precisely so that
//! nothing in the product can depend on a test application
//! (`tests/integration/tests/workspace_layering.rs`), dev-dependencies
//! included. A test that reads the counters back out of a *recording* therefore
//! has to live in that package, beside `tests/capture/wgc_video_pattern.rs`
//! which already reads them out of captured frames. It is
//! [issue #183](https://github.com/wildware-uk/clipped/issues/183), raised
//! rather than reimplemented here: a second copy of the pattern decoder is the
//! duplication AGENTS.md section 55 exists to prevent.
//!
//! # Why these are `#[ignore]`d
//!
//! They need a GPU, an encoder, a desktop session and about fifteen seconds of
//! wall-clock time, and they put a window on a display. CI has none of those,
//! so what they would test there is the runner (`tests/capture/README.md` says
//! the same about the capture tests). Being `#[ignore]`d rather than silently
//! skipped is deliberate: a test that decides for itself that it could not run
//! reads as a pass.
//!
//! ```text
//! cargo test -p clipped-recorder --test record_end_to_end -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `--test-threads=1` is worth typing: each test puts a window on screen and
//! records it, and two at once would be two windows and two encoder sessions.

#![cfg(windows)]

mod support;

use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use clipped_media_validation::{require_media_tools, Media, TemporaryDirectory, VideoStream};

use support::{
    client_area, collected_stderr, ensure_console, read_stderr, recorder_binary, resize,
    send_ctrl_c, wait_for_exit, PatternApp, Scratch, CREATE_NEW_PROCESS_GROUP,
};

/// The rate the pattern application presents at.
///
/// Half of a common refresh rate, deliberately: the compositor produces a
/// capture frame when the window's content changes, so a source slower than the
/// display is the case where every presented frame can be recorded.
const SOURCE_FPS: u32 = 30;

/// How long the Ctrl+C test records before interrupting.
const RECORD_FOR: Duration = Duration::from_secs(4);

/// How long the pattern renders for in the test that lets it close by itself.
const PATTERN_SECONDS: u64 = 8;

/// How far the file's duration may be from the duration the recorder reported.
///
/// They are measurements of the same thing by two programs: the muxer reports
/// the span between the first and last timestamps it wrote, and `ffprobe`
/// reports the duration the container declares, which is that span plus
/// whatever the last frame's own duration is taken to be. A frame at 30 fps is
/// 33 ms, so a fifth of a second is comfortably above the difference and far
/// below anything that would hide a truncated recording.
const DURATION_TOLERANCE: f64 = 0.2;

/// The slowest a recording of a [`SOURCE_FPS`] source may be and still be a
/// recording of it.
///
/// Not a performance target — it is the assertion that stops every other check
/// in this file from passing against a recording of four frames a second. Each
/// of them compares the file against what the recorder *said* it wrote, so a
/// pipeline that captured a seventh of the frames would satisfy all of them and
/// produce a slideshow. Half the source's rate leaves room for a busy machine
/// and a debug build without leaving room for that; the runs measured on this
/// project's machine sustain 27 to 30.
///
/// **A failure here is worth reading before blaming the recorder.** The
/// compositor produces a frame when a window's content changes, and a desktop
/// whose display has gone to sleep composes at a few frames a second whatever
/// the application presents — which is exactly what this fails on. Wake the
/// display and run it again; if it still fails, the pipeline is losing frames.
const MINIMUM_SUSTAINED_FRAMERATE: f64 = SOURCE_FPS as f64 / 2.0;

/// How long the resize test records before it changes the window's size.
///
/// Long enough that the file it leaves is unambiguously a recording — several
/// seconds of pictures, past the first keyframe and into a second cluster —
/// rather than a header and a trailer with nothing between them, which is the
/// thing "the file it did produce" has to distinguish itself from.
const RECORD_BEFORE_RESIZE: Duration = Duration::from_secs(5);

/// The size the subject's window is dragged to, mid-recording.
///
/// The same figure `tests/automatic_sessions.rs` uses, and for the same two
/// reasons: it differs from the 1280x720 the subject starts at, so the recording
/// is told apart by its own dimensions, and it is **even in both dimensions**
/// because an odd client area is a defect of its own
/// ([issue #561](https://github.com/wildware-uk/clipped/issues/561),
/// [ADR 0013](../../../docs/adr/0013-capture-rounds-an-odd-dimension-away.md))
/// and a resize into one would have this test measuring that instead of this.
const RESIZED_TO: (u32, u32) = (1024, 576);

#[test]
#[ignore = "needs a GPU, an encoder and a desktop session; see the module docs"]
fn ctrl_c_during_a_recording_leaves_a_playable_file() {
    // Issue #126 itself. A real recording, a real `CTRL_C_EVENT`, and the file
    // it left behind decoded afterwards.
    let Some(_tools) = require_media_tools() else {
        return;
    };
    ensure_console();

    let directory = TemporaryDirectory::new("recorder-ctrl-c");
    let output = directory.file("interrupted.mkv");
    let pattern = PatternApp::start(SOURCE_FPS, 120);

    let mut recorder = Command::new(recorder_binary())
        .args(record_arguments(pattern.process_id(), &output))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // A group of its own, so the event reaches the recorder and not
        // `cargo test`. The recorder asks Ctrl+C back on for itself
        // (`clipped_recorder::shutdown::allow_ctrl_c`), which is the only
        // reason this can be a real Ctrl+C rather than a kill.
        .creation_flags(CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .expect("the recorder binary can be started");
    let diagnostics = read_stderr(&mut recorder);

    thread::sleep(RECORD_FOR);
    send_ctrl_c(&recorder);

    let status = wait_for_exit(&mut recorder, "the recorder");
    let diagnostics = collected_stderr(&diagnostics);
    assert!(
        status.success(),
        "Ctrl+C should stop a recording cleanly, not kill it; exit status was {status}.\n\
         {diagnostics}"
    );

    assert_finalisation_was_logged_in_order(&diagnostics);
    assert!(
        diagnostics.contains("reason=interrupted"),
        "the finalisation hook should have been told the run was interrupted:\n{diagnostics}"
    );

    let summary = Summary::parse(&diagnostics);
    assert_playable(&output, &summary, pattern.client_size());

    // The recording is roughly as long as the recorder was left running. The
    // bound is loose at both ends deliberately: the lower end allows for a
    // window that took a moment to start drawing, and the upper end for a
    // machine slow to deliver the signal. What it does catch is a file that
    // holds a fraction of a second, which is what a recording finalised
    // without its last cluster would leave.
    let seconds = summary.seconds;
    assert!(
        (RECORD_FOR.as_secs_f64() - 2.0..=RECORD_FOR.as_secs_f64() + 2.0).contains(&seconds),
        "the recorder ran for about {:.1}s and the recording is {seconds:.2}s long:\n{diagnostics}",
        RECORD_FOR.as_secs_f64()
    );
}

#[test]
#[ignore = "needs a GPU, an encoder and a desktop session; see the module docs"]
fn a_recording_ends_when_the_window_closes_and_leaves_valid_media() {
    // The other way a recording ends, and the one AGENTS.md section 16 asks
    // for by name: the game closes. The recorder should finish the file and
    // exit successfully rather than reporting a failure — everything up to the
    // moment the window went is a real recording of it.
    let Some(_tools) = require_media_tools() else {
        return;
    };

    let directory = TemporaryDirectory::new("recorder-window-closed");
    let output = directory.file("until-closed.mkv");
    let mut pattern = PatternApp::start(SOURCE_FPS, PATTERN_SECONDS);

    let mut recorder = Command::new(recorder_binary())
        .args(record_arguments(pattern.process_id(), &output))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the recorder binary can be started");
    let diagnostics = read_stderr(&mut recorder);

    pattern.wait_for_exit();
    let status = wait_for_exit(&mut recorder, "the recorder");
    let diagnostics = collected_stderr(&diagnostics);

    assert!(
        status.success(),
        "a window closing ends a recording; it is not a failure. Exit status was {status}.\n\
         {diagnostics}"
    );
    assert!(
        diagnostics.contains("Stopped because the recorded window closed."),
        "the summary should say why the recording ended:\n{diagnostics}"
    );

    let summary = Summary::parse(&diagnostics);
    assert_playable(&output, &summary, pattern.client_size());

    assert!(
        summary.seconds >= 3.0,
        "the pattern rendered for {PATTERN_SECONDS}s and only {:.2}s was recorded:\n{diagnostics}",
        summary.seconds
    );
}

#[test]
#[ignore = "needs a GPU, an encoder and a desktop session; see the module docs"]
fn a_resize_ends_a_command_line_recording_and_says_which_file_it_left() {
    // [Issue #625](https://github.com/wildware-uk/clipped/issues/625), against a
    // resize this test really makes rather than one it tells the recorder about.
    //
    // ADR 0012 decided that a size change finishes the file and the *session*
    // starts the next one, and `record` is the path with no session to do that:
    // it was given one path to write and it writes that file. The decision for
    // this command is therefore that it keeps stopping — and that the person
    // watching the terminal is told, in words, that a size change is why and
    // which file they have. `tests/automatic_sessions.rs` proves the other half
    // of the same ADR, that `watch` follows the resize with a second file.
    //
    // Nothing here touches `EndReason` or `report_resize` by name: the window
    // changes size, the recorder exits on its own, and every assertion below is
    // read out of what it printed and what it left on disk.
    let Some(_tools) = require_media_tools() else {
        return;
    };

    let directory = Scratch::new("recorder-resized");
    let output = directory.file("until-resized.mkv");
    let mut pattern = PatternApp::start(SOURCE_FPS, 300);
    let before = pattern.client_size();
    assert_ne!(
        before, RESIZED_TO,
        "the subject already has the size this test resizes it to, so the resize would change \
         nothing and the recording would not end"
    );

    let mut recorder = Command::new(recorder_binary())
        .args(record_arguments(pattern.process_id(), &output))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the recorder binary can be started");
    let diagnostics = read_stderr(&mut recorder);

    thread::sleep(RECORD_BEFORE_RESIZE);

    // A real `SetWindowPos` on the subject's real window, from outside the
    // process that owns it — which is what a user dragging an edge is. The
    // recorder is not told; it finds out through capture.
    resize(pattern.window(), RESIZED_TO);
    assert_eq!(
        client_area(pattern.window()),
        RESIZED_TO,
        "the window did not actually change size, so nothing below would be about a resize"
    );

    let status = wait_for_exit(&mut recorder, "the recorder");
    let diagnostics = collected_stderr(&diagnostics);

    assert!(
        pattern.is_running(),
        "the subject must still have been running when the recorder gave up, or the recording \
         ended because its window went rather than because it changed size:
{diagnostics}"
    );
    assert!(
        status.success(),
        "a window changing size ends a recording; it is not a failure. Exit status was \
         {status}.
{diagnostics}"
    );

    // **The three things the person watching is told**, and the reason this
    // test exists. Each is asserted separately so that a run which loses one of
    // them says which, rather than failing on a single string that could have
    // gone missing at either end.
    //
    // The first two are the issue's own words — "a recording that stops because
    // the window changed size should tell the user that, naming the file it did
    // produce".
    assert!(
        diagnostics.contains(
            "The recorded window changed size, so this recording was finished where it was \
             and nothing after the change was recorded."
        ),
        "the run never said that a size change is what ended it, so somebody who dragged a \
         window's edge is left to work out why their recording stopped:
{diagnostics}"
    );
    assert!(
        diagnostics.contains(&format!(
            "Everything up to the change is in {}.",
            output.display()
        )),
        "the run never named the file it did produce, which is the only thing on the screen \
         anybody can act on:
{diagnostics}"
    );
    // And the third, which is what the issue is actually about: the same action
    // has two outcomes depending on how the recording started, and this is the
    // one place a `record` user finds out which of the two they are in.
    assert!(
        diagnostics.contains("clipped-recorder watch"),
        "the run never said that the other mode follows a size change instead of stopping, so \
         nothing tells a `record` user that the two behave differently at all:
{diagnostics}"
    );

    // And the file itself, at the size the window was: finished on purpose
    // rather than abandoned, which is the whole reason ending here is honest.
    let summary = Summary::parse(&diagnostics);
    assert_playable(&output, &summary, before);
    assert!(
        summary.seconds >= 2.0,
        "the recorder ran for about {:.1}s before the resize and the file holds {:.2}s; a \
         header and a trailer with nothing between them is not the file this claims to \
         leave:
{diagnostics}",
        RECORD_BEFORE_RESIZE.as_secs_f64(),
        summary.seconds
    );
}

/// The three lines that say the recording was finalised deliberately, in the
/// order the finalisation happens in.
///
/// This is acceptance criterion 2 of #126 — the hook is *shown* to have run,
/// rather than the file merely happening to be readable. The order is part of
/// the claim and is checked as such: the encoder is told the stream has ended
/// and drained of the pictures it was holding back, *then* the container's
/// trailer is written, *then* the recorder's own shutdown seam runs its hook
/// knowing why the run ended. A pipeline that wrote the trailer before flushing
/// the encoder would lose the last fraction of a second — which is the part
/// somebody who pressed Ctrl+C was watching — and would satisfy three
/// unordered `contains` checks perfectly well.
const FINALISATION_LINES: [&str; 3] = [
    "the encoder was flushed and the container is being closed",
    "recording file finished",
    "the recording was finalised and the finalisation hook ran",
];

/// Asserts that [`FINALISATION_LINES`] all appear, in that order.
///
/// Each line is searched for in what is left of the diagnostics after the
/// previous one, so a line that appears only *before* its predecessor fails
/// exactly as a missing line does.
fn assert_finalisation_was_logged_in_order(diagnostics: &str) {
    let mut rest = diagnostics;
    for expected in FINALISATION_LINES {
        let found = rest.find(expected).unwrap_or_else(|| {
            panic!(
                "the recorder never logged `{expected}` after the finalisation lines before \
                 it; the three are expected in the order {FINALISATION_LINES:?}:\n{diagnostics}"
            )
        });
        rest = &rest[found + expected.len()..];
    }
}

/// The arguments a recording of `process_id` is made with.
///
/// `--pid` rather than `--window`, so that two pattern windows open at once
/// cannot make the selector ambiguous. Audio is turned off explicitly: the
/// session cannot record it yet and would otherwise warn on every run, and a
/// test should ask for what it expects to get.
fn record_arguments(process_id: u32, output: &Path) -> Vec<String> {
    vec![
        "record".to_owned(),
        "--pid".to_owned(),
        process_id.to_string(),
        "--output".to_owned(),
        output.to_string_lossy().into_owned(),
        "--microphone".to_owned(),
        "none".to_owned(),
        "--system-audio".to_owned(),
        "none".to_owned(),
    ]
}

/// What the recorder said it wrote, read out of the line it prints when a
/// recording ends.
///
/// Parsed rather than assumed so that every assertion below compares the
/// recorder's own account against the file's. The line's shape is pinned by a
/// unit test in `crates/session/src/report.rs`, so a change to it fails there
/// first with a diff rather than here with a parse error.
#[derive(Debug)]
struct Summary {
    frames: u64,
    width: u32,
    height: u32,
    codec: String,
    seconds: f64,
}

impl Summary {
    fn parse(diagnostics: &str) -> Self {
        let line = diagnostics
            .lines()
            .find(|line| line.starts_with("Recorded "))
            .unwrap_or_else(|| {
                panic!("the recorder printed no summary of what it recorded:\n{diagnostics}")
            });

        let frames = field(line, "Recorded ", " frames")
            .parse()
            .unwrap_or_else(|_| panic!("the frame count is not a number: {line}"));

        // `1280x720 H.264`
        let description = field(line, " of ", " in ");
        let (size, codec) = description
            .split_once(' ')
            .unwrap_or_else(|| panic!("the summary does not describe the picture: {line}"));
        let (width, height) = size
            .split_once('x')
            .unwrap_or_else(|| panic!("the picture size is not WIDTHxHEIGHT: {line}"));

        let seconds = field(line, " in ", "s to ")
            .rsplit(' ')
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or_else(|| panic!("the duration is not a number: {line}"));

        Self {
            frames,
            width: width.parse().expect("the width is a number"),
            height: height.parse().expect("the height is a number"),
            codec: ffprobe_codec_name(codec),
            seconds,
        }
    }
}

/// Frames a second the recording actually sustained.
///
/// Intervals rather than frames: two frames a second apart are 2 fps over a
/// one-second span, not 2.
fn sustained_framerate(summary: &Summary) -> f64 {
    if summary.seconds <= 0.0 || summary.frames < 2 {
        return 0.0;
    }
    (summary.frames - 1) as f64 / summary.seconds
}

/// The text between `start` and `end` in `line`.
fn field<'line>(line: &'line str, start: &str, end: &str) -> &'line str {
    line.split_once(start)
        .and_then(|(_, rest)| rest.split_once(end))
        .map(|(value, _)| value)
        .unwrap_or_else(|| panic!("the summary has no `{start}…{end}` field: {line}"))
}

/// What `ffprobe` calls the codec the recorder named.
fn ffprobe_codec_name(codec: &str) -> String {
    match codec {
        "H.264" => "h264",
        "HEVC" => "hevc",
        "AV1" => "av1",
        other => panic!("the recorder reported a codec this test does not know: {other}"),
    }
    .to_owned()
}

/// Asserts that the file is what the recorder said it wrote.
///
/// Prints the recording's own account of itself first, because that is the
/// evidence a run produces and AGENTS.md section 53 asks for it to be recorded
/// rather than inferred from a green test.
fn assert_playable(output: &Path, summary: &Summary, client: (u32, u32)) {
    eprintln!(
        "\n=== recording ===\n\
         frames encoded : {}\n\
         picture        : {}x{} {}\n\
         duration       : {:.2}s\n\
         sustained      : {:.1} fps\n\
         file           : {}\n",
        summary.frames,
        summary.width,
        summary.height,
        summary.codec,
        summary.seconds,
        sustained_framerate(summary),
        output.display(),
    );

    assert!(
        sustained_framerate(summary) >= MINIMUM_SUSTAINED_FRAMERATE,
        "the source presented {SOURCE_FPS} frames a second and only {:.1} a second were \
         recorded; a recording that keeps a fraction of the frames is a recording nobody \
         would watch, and it would pass every other assertion here",
        sustained_framerate(summary)
    );

    assert_eq!(
        (summary.width, summary.height),
        client,
        "a borderless window's capture is exactly its client area, so the recording should \
         be the size the pattern announced"
    );

    let media = Media::open(output)
        .unwrap_or_else(|error| panic!("the recording is not usable at all: {error}"));

    media
        .validate()
        // One video stream and nothing else. The audio track is not written
        // yet, and a test that accepted "at least one video stream" would not
        // notice one appearing half-wired.
        .stream_count(1)
        .video_stream_count(1)
        .video(
            VideoStream::codec(&summary.codec)
                .resolution(summary.width, summary.height)
                // Every frame the recorder says it encoded is a frame that
                // decodes out of the file. Two independent accounts of the same
                // recording: a pipeline that lost a packet, wrote one to the
                // wrong track or duplicated one would disagree here.
                .decoded_frames(summary.frames),
        )
        .duration_seconds(summary.seconds, DURATION_TOLERANCE)
        .monotonic_timestamps()
        .assert_valid();

    assert_declared_frame_rate_is_the_one_recorded(&media, summary);

    assert!(
        summary.frames > 0,
        "a recording of no frames is not a recording"
    );
}

/// How far the rate the container declares may be from the rate the recording
/// sustained, as a fraction of it.
///
/// A tenth is far tighter than the failure it exists to catch — declaring the
/// `--framerate` ceiling of 60 over a 30 fps source is out by a factor of two —
/// and loose enough for the difference between a rate derived from `n - 1`
/// intervals and one derived from a container duration that includes the last
/// frame.
const FRAME_RATE_TOLERANCE: f64 = 0.1;

/// Asserts the file never claims a frame rate the recording did not achieve.
///
/// `--framerate` is a ceiling: a 30 fps source recorded with the default
/// `--framerate 60` is a real 30 fps recording. `avg_frame_rate` is by
/// definition the *average* the file carries, so declaring the ceiling there is
/// incorrect codec metadata (AGENTS.md section 22) — and it is what every
/// editor, player and transcoder reads to label the clip.
///
/// A recording made here declares no nominal rate of its own, so what
/// `avg_frame_rate` holds is whatever FFmpeg derived from the timestamps: the
/// rate the recording really ran at, or `0/0` when it declined to derive one at
/// all. Both are correct and both are accepted, and the failure this catches is
/// the third case — a number that came from the settings rather than from the
/// recording. `0/0` is not a way for a broken file to slip through: a track
/// carrying the ceiling reports `60/1`, which is a rate, and is checked.
fn assert_declared_frame_rate_is_the_one_recorded(media: &Media, summary: &Summary) {
    let sustained = sustained_framerate(summary);

    match declared_frame_rate(media) {
        None => eprintln!("declared rate  : none; read from the timestamps\n"),
        Some(declared) => {
            eprintln!("declared rate  : {declared:.1} fps\n");
            assert!(
                (declared - sustained).abs() <= sustained * FRAME_RATE_TOLERANCE,
                "the file declares {declared:.2} fps and the recording sustained \
                 {sustained:.2} fps. A container that names the `--framerate` ceiling rather \
                 than the rate it holds is wrong in the way nothing downstream can see: a \
                 player labels the clip with it, and an editor lays it on a timeline at it.\n{}",
                media.inventory()
            );
        }
    }
}

/// The average frame rate the container declares, as a number.
///
/// `ffprobe` reports it as a fraction — `30/1`, or `0/0` for a stream that
/// declares none, which is [`None`] here rather than a rate of zero.
fn declared_frame_rate(media: &Media) -> Option<f64> {
    let stream = media.video_streams().into_iter().next()?;
    let (numerator, denominator) = stream.field("avg_frame_rate")?.split_once('/')?;
    let numerator: f64 = numerator.parse().ok()?;
    let denominator: f64 = denominator.parse().ok()?;
    (denominator != 0.0).then_some(numerator / denominator)
}
