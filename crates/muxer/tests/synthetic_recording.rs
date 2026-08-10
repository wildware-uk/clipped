//! A whole recording, written and then taken apart again.
//!
//! `tests/mkv_writing.rs` covers what the writer puts in the header and what it
//! refuses. This one is about the finished article: real H.264 produced by the
//! pinned build's software encoder, real PCM on two named tracks, written
//! through the muxer by `examples/synthetic_recording.rs` and then checked with
//! `ffprobe` against every part of issue #21's first acceptance criterion —
//! the container opens, there is one video stream and the expected audio
//! streams, the duration is plausible, the timestamps are monotonic and the
//! codec metadata is right.
//!
//! Every frame is decoded rather than counted: `nb_read_frames` is what came
//! out of the decoder, which is the difference between a file that lists a
//! video stream and a file that plays.

mod support;

use support::{
    assert_decode_timestamps_increase, field, number, packets, run_synthetic_recording, tag, Probe,
    TemporaryDirectory,
};

/// How long a recording these tests write.
///
/// Long enough to contain several keyframes, several clusters and hundreds of
/// packets; short enough that the software encoder produces it in well under a
/// second.
const SECONDS: f64 = 4.0;
const FRAME_RATE: f64 = 30.0;

#[test]
fn a_finished_recording_contains_what_it_was_given() {
    let directory = TemporaryDirectory::new("synthetic");
    let path = directory.file("recording.mkv");

    let output = run_synthetic_recording(&[
        "--output",
        &path.to_string_lossy(),
        "--seconds",
        &SECONDS.to_string(),
    ]);
    assert!(
        output.status.success(),
        "the recorder failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let probe = Probe::of(&path);
    assert!(
        probe.diagnostics.is_empty(),
        "ffprobe could not read the recording cleanly: {}",
        probe.diagnostics
    );

    // The container opens, and holds exactly the tracks that were declared.
    assert_eq!(probe.streams.len(), 3, "streams: {:#?}", probe.streams);

    // One video stream, with the codec metadata the caller described.
    let video = probe.streams_of("video");
    assert_eq!(video.len(), 1);
    assert_eq!(field(video[0], "codec_name"), Some("h264"));
    assert_eq!(field(video[0], "pix_fmt"), Some("yuv420p"));
    assert_eq!(number(video[0], "width"), Some(640.0));
    assert_eq!(number(video[0], "height"), Some(360.0));
    assert_eq!(
        field(video[0], "avg_frame_rate"),
        Some("30/1"),
        "the nominal frame rate is what an editor labels the clip with"
    );
    assert_eq!(tag(video[0], "title"), Some("Gameplay"));

    // Every frame decodes, and there are as many of them as there were frames.
    let expected_frames = SECONDS * FRAME_RATE;
    let decoded = number(video[0], "nb_read_frames").expect("frames were counted");
    assert_eq!(
        decoded, expected_frames,
        "the video does not decode to the {expected_frames} frames that were written"
    );
    assert_eq!(
        number(video[0], "nb_read_packets"),
        Some(decoded),
        "some packets did not produce a frame"
    );

    // Two audio streams, named, in the order they were declared.
    let audio = probe.streams_of("audio");
    assert_eq!(audio.len(), 2);
    assert_eq!(tag(audio[0], "title"), Some("Compatibility Mix"));
    assert_eq!(tag(audio[1], "title"), Some("Game"));
    for stream in &audio {
        assert_eq!(field(stream, "codec_name"), Some("pcm_s16le"));
        assert_eq!(number(stream, "sample_rate"), Some(48_000.0));
        assert_eq!(number(stream, "channels"), Some(2.0));
        // 20 ms packets for the whole recording.
        assert_eq!(
            number(stream, "nb_read_packets"),
            Some(SECONDS * 50.0),
            "an audio track is missing packets"
        );
    }

    // A plausible duration: what was asked for, to within the last packet.
    let duration = probe.duration_seconds().expect("a finished file has one");
    assert!(
        (SECONDS..SECONDS + 0.1).contains(&duration),
        "a {SECONDS} second recording came out {duration} seconds long"
    );

    // Monotonic timestamps, per stream, which is the container's own
    // requirement and this ticket's third criterion.
    let written = packets(&path);
    assert_decode_timestamps_increase(&written);

    // And the tracks start together: a recording whose audio begins a second
    // into the video is out of sync however monotonic its timestamps are.
    for stream in &probe.streams {
        assert_eq!(
            number(stream, "start_time"),
            Some(0.0),
            "a track does not start at the beginning of the recording"
        );
    }
}
