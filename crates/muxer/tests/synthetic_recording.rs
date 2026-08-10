//! A whole recording, written and then taken apart again.
//!
//! `tests/mkv_writing.rs` covers what the writer puts in the header and what it
//! refuses. This one is about the finished article: real H.264 produced by the
//! pinned build's software encoder, real PCM on two named tracks, written
//! through the muxer by `examples/synthetic_recording.rs` and then checked
//! against every part of issue #21's first acceptance criterion — the container
//! opens, there is one video stream and the expected audio streams, the
//! duration is plausible, the timestamps are monotonic and the codec metadata
//! is right.
//!
//! The checking is `clipped-media-validation` (`tests/media`), the workspace's
//! media harness, rather than assertions written here: it makes exactly these
//! seven checks, it reports every failed expectation at once alongside an
//! inventory of what the file really holds, and it is itself tested against
//! files that are broken (AGENTS.md sections 22 and 55).
//!
//! Every frame is decoded rather than counted: `decoded_frames` is what came
//! out of the decoder, which is the difference between a file that lists a
//! video stream and a file that plays.

mod support;

use std::time::Duration;

use clipped_media_validation::{
    require_media_tools, AudioStream, Media, TemporaryDirectory, VideoStream,
};
use support::run_synthetic_recording;

/// How long a recording these tests write.
///
/// Long enough to contain several keyframes, several clusters and hundreds of
/// packets; short enough that the software encoder produces it in well under a
/// second.
const SECONDS: f64 = 4.0;
const FRAME_RATE: f64 = 30.0;

/// 20 ms audio packets for the whole recording.
const AUDIO_PACKETS_PER_SECOND: f64 = 50.0;

#[test]
fn a_finished_recording_contains_what_it_was_given() {
    let Some(_tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("muxer-synthetic");
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

    let media = Media::open(&path).expect("a finished recording opens");
    assert!(
        media.diagnostics().is_empty(),
        "ffprobe could not read the recording cleanly: {}",
        media.diagnostics()
    );

    let audio_packets = (SECONDS * AUDIO_PACKETS_PER_SECOND) as u64;

    media
        .validate()
        // The container opens, and holds exactly the tracks that were declared.
        .stream_count(3)
        .audio_stream_count(2)
        // One video stream, with the codec metadata the caller described, whose
        // every frame decodes — and as many of them as were written.
        .video(
            VideoStream::codec("h264")
                .resolution(640, 360)
                .pixel_format("yuv420p")
                // The nominal frame rate is what an editor labels the clip with.
                .frame_rate("30/1")
                .title("Gameplay")
                .decoded_frames((SECONDS * FRAME_RATE) as u64)
                .packets((SECONDS * FRAME_RATE) as u64),
        )
        // Two audio streams, named, in the order they were declared, each with
        // its own packets: a writer that sent everything to the first stream
        // would satisfy every other assertion here.
        .audio(
            0,
            AudioStream::codec("pcm_s16le")
                .sample_rate(48_000)
                .channels(2)
                .title("Compatibility Mix")
                .packets(audio_packets),
        )
        .audio(
            1,
            AudioStream::codec("pcm_s16le")
                .sample_rate(48_000)
                .channels(2)
                .title("Game")
                .packets(audio_packets),
        )
        // A plausible duration: what was asked for, to within the last packet.
        .duration_seconds(SECONDS + 0.05, 0.05)
        // Monotonic timestamps, per stream, which is the container's own
        // requirement and this ticket's third criterion.
        .monotonic_timestamps()
        // And the tracks start and end together: a recording whose audio begins
        // a second into the video is out of sync however monotonic its
        // timestamps are.
        .synchronised_within(Duration::from_millis(40))
        .assert_valid();
}
