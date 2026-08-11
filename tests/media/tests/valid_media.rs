//! The harness against media that is genuinely valid.
//!
//! Half of what a validator has to be trusted for. The other half — that it
//! fails, and says something useful, when the media is broken — is
//! `invalid_media.rs`, and neither test file means much without the other.

mod support;

use std::time::Duration;

use clipped_media_validation::{require_media_tools, TemporaryDirectory};
use clipped_media_validation::{AudioStream, Media, Tone, VideoStream};

use support::{write_recording, AudioSource, FRAME_RATE, HEIGHT, WIDTH};

/// How long every fixture is.
const SECONDS: f64 = 2.0;

/// The tones AGENTS.md section 26 gives the controlled audio test
/// applications, one per source.
const GAME: f64 = 440.0;
const SYSTEM: f64 = 880.0;
const MICROPHONE: f64 = 1320.0;

#[test]
fn a_valid_recording_meets_every_expectation() {
    let Some(tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("media-valid");
    let path = directory.file("recording.mkv");
    write_recording(
        &tools,
        &path,
        SECONDS,
        &[
            AudioSource::tone("Game", GAME),
            AudioSource::tone("Other System Audio", SYSTEM),
        ],
    );

    let media = Media::open(&path).expect("a recording FFmpeg just wrote opens");

    media
        .validate()
        .stream_count(3)
        .video_stream_count(1)
        .audio_stream_count(2)
        .video(
            VideoStream::codec("h264")
                .resolution(WIDTH, HEIGHT)
                .pixel_format("yuv420p")
                .frame_rate(&format!("{FRAME_RATE}/1"))
                .title("Gameplay")
                .decoded_frames((SECONDS * f64::from(FRAME_RATE)) as u64),
        )
        .audio(
            0,
            AudioStream::codec("pcm_s16le")
                .sample_rate(48_000)
                .channels(2)
                .title("Game"),
        )
        .audio(
            1,
            AudioStream::codec("pcm_s16le")
                .sample_rate(48_000)
                .channels(2)
                .title("Other System Audio"),
        )
        .duration_seconds(SECONDS, 0.1)
        .monotonic_timestamps()
        .synchronised_within(Duration::from_millis(50))
        .assert_valid();
}

#[test]
fn every_track_is_checked_for_its_own_tone_and_the_absence_of_the_others() {
    // The assertion the multi-track model needs, and the one #28 will reuse: a
    // recorder that wrote the same mix to three tracks passes every structural
    // check in the file above.
    let Some(tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("media-isolation");
    let path = directory.file("recording.mkv");
    write_recording(
        &tools,
        &path,
        SECONDS,
        &[
            AudioSource::tone("Game", GAME),
            AudioSource::tone("Other System Audio", SYSTEM),
            AudioSource::tone("Microphone", MICROPHONE),
        ],
    );

    let media = Media::open(&path).expect("a recording FFmpeg just wrote opens");

    media
        .validate()
        .audio_stream_count(3)
        .audio_tone(
            0,
            Tone::at(GAME)
                .isolated_from(SYSTEM)
                .isolated_from(MICROPHONE),
        )
        .audio_tone(
            1,
            Tone::at(SYSTEM)
                .isolated_from(GAME)
                .isolated_from(MICROPHONE),
        )
        .audio_tone(
            2,
            Tone::at(MICROPHONE)
                .isolated_from(GAME)
                .isolated_from(SYSTEM),
        )
        .assert_valid();
}

#[test]
fn a_decoded_track_reports_the_frequency_that_was_generated() {
    // The measurement itself, against media rather than against samples this
    // crate synthesised for itself. The unit tests in `src/audio.rs` prove the
    // arithmetic; this proves the decode that feeds it.
    let Some(tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("media-tone");
    let path = directory.file("recording.mkv");
    write_recording(
        &tools,
        &path,
        SECONDS,
        &[AudioSource::tone("Microphone", MICROPHONE)],
    );

    let media = Media::open(&path).expect("a recording FFmpeg just wrote opens");
    let content = media.audio_content(0).expect("the track decodes");

    assert_eq!(content.sample_rate(), 48_000);
    assert!(
        (content.duration().as_secs_f64() - SECONDS).abs() < 0.05,
        "{SECONDS} seconds of audio decoded to {:?}",
        content.duration()
    );

    let (peak, magnitude) = content.dominant_frequency();
    eprintln!("decoded a:0: {peak:.1} Hz at magnitude {magnitude:.4}");
    assert!(
        (peak - MICROPHONE).abs() <= 5.0,
        "a {MICROPHONE} Hz track decoded to a dominant {peak:.1} Hz"
    );
}
