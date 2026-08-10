//! The harness against media that is broken, which is the half that matters.
//!
//! A validator only ever run against good input is not a validator: every one
//! of these files would sail through a check that asked "did the call return
//! `Ok`?". Each test here damages a real recording in a way a real recorder can
//! damage one, and asserts on the *text* the harness produces — because the
//! text is the deliverable. A contributor woken by one of these at two in the
//! morning has to be able to act on it without running `ffprobe` by hand.
//!
//! Each test prints the report it got. Run with `--nocapture` to read them.

mod support;

use std::time::Duration;

use clipped_media_validation::{require_media_tools, TemporaryDirectory};
use clipped_media_validation::{AudioStream, Media, MediaError, Tone, VideoStream};

use support::{second_cluster_rewound, truncated, write_recording, AudioSource, HEIGHT, WIDTH};

const SECONDS: f64 = 2.0;
const GAME: f64 = 440.0;
const SYSTEM: f64 = 880.0;
const MICROPHONE: f64 = 1320.0;

#[test]
fn a_file_truncated_before_its_header_reached_the_disk_will_not_open() {
    let Some(tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("media-unopenable");
    let good = write_recording(
        &tools,
        &directory.file("recording.mkv"),
        SECONDS,
        &[AudioSource::tone("Game", GAME)],
    );
    // A few hundred bytes: enough for the EBML header, nothing like enough for
    // the track entries.
    let broken = truncated(&good, &directory.file("truncated.mkv"), 0.0004);

    let error = Media::open(&broken).expect_err("a file with no track entries is not media");
    eprintln!("{error}");

    assert!(
        matches!(error, MediaError::NotMedia { .. }),
        "unexpected error: {error}"
    );
    let message = error.to_string();
    assert!(
        message.contains("is not media that can be opened")
            && message.contains("ffprobe found no streams")
            && message.contains("bytes"),
        "the message does not say what happened: {message}"
    );
}

#[test]
fn a_recording_that_stops_early_fails_its_duration_and_frame_count() {
    // The file still opens — the header reached the disk — so nothing about
    // "can FFmpeg read this?" catches it. What catches it is that there is less
    // media in it than there was supposed to be.
    //
    // Note which check does the catching. This file was finished and *then*
    // cut, so its segment header still claims the full two seconds and the
    // duration expectation passes; only decoding the video finds the missing
    // half. That is why `decoded_frames` counts what came out of a decoder
    // rather than what the container lists.
    let Some(tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("media-truncated");
    let good = write_recording(
        &tools,
        &directory.file("recording.mkv"),
        SECONDS,
        &[AudioSource::tone("Game", GAME)],
    );
    let broken = truncated(&good, &directory.file("truncated.mkv"), 0.5);

    let media = Media::open(&broken).expect("the header survived, so the file still opens");
    let report = media
        .validate()
        .video(
            VideoStream::codec("h264")
                .resolution(WIDTH, HEIGHT)
                .decoded_frames(60),
        )
        .duration_seconds(SECONDS, 0.1)
        .check()
        .expect_err("half a recording is not a recording");
    eprintln!("{report}");

    let text = report.to_string();
    assert!(
        text.contains("v:0 decoded frames: expected 60, found"),
        "the report does not say how much video was lost: {text}"
    );
    assert!(
        text.contains("File ended prematurely"),
        "the report does not pass on what ffprobe said about the damage: {text}"
    );
    assert!(
        text.contains("what the file actually holds"),
        "the report does not say what was there instead: {text}"
    );
}

#[test]
fn a_missing_audio_track_is_reported_with_the_tracks_that_are_there() {
    // The failure multi-track audio dies of: two of the three sources reached
    // the file, and every other check passes.
    let Some(tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("media-missing-track");
    let path = write_recording(
        &tools,
        &directory.file("recording.mkv"),
        SECONDS,
        &[
            AudioSource::tone("Game", GAME),
            AudioSource::tone("Other System Audio", SYSTEM),
        ],
    );

    let media = Media::open(&path).expect("the recording opens");
    let report = media
        .validate()
        .audio_stream_count(3)
        .audio(2, AudioStream::codec("pcm_s16le").title("Microphone"))
        .check()
        .expect_err("the microphone track was never written");
    eprintln!("{report}");

    let text = report.to_string();
    assert!(
        text.contains("audio stream count: expected 3, found 2"),
        "the report does not name the expectation: {text}"
    );
    assert!(
        text.contains("a:0 pcm_s16le 48000 Hz stereo") && text.contains("'Game'"),
        "the report does not say which tracks are there: {text}"
    );
    assert!(
        text.contains("a:2: the file has no such stream"),
        "the report does not say the track being asserted on is absent: {text}"
    );
}

#[test]
fn a_cluster_timestamped_backwards_fails_the_monotonic_check() {
    // What a recorder whose clock stepped writes. The container is still
    // structurally perfect: it opens, every track is there, every packet
    // decodes. Only the timeline is wrong.
    let Some(tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("media-backwards");
    let good = write_recording(
        &tools,
        &directory.file("recording.mkv"),
        SECONDS,
        &[AudioSource::tone("Game", GAME)],
    );
    let broken = second_cluster_rewound(&good, &directory.file("backwards.mkv"))
        .expect("a two-second recording has more than one cluster");

    // The good one first, so that a change which made this check pass on
    // anything at all would be caught here rather than read as a pass.
    Media::open(&good)
        .expect("the recording opens")
        .validate()
        .monotonic_timestamps()
        .assert_valid();

    let media = Media::open(&broken).expect("only the timeline was damaged");
    let report = media
        .validate()
        .monotonic_timestamps()
        .check()
        .expect_err("a cluster timestamped before the one in front of it is not monotonic");
    eprintln!("{report}");

    let text = report.to_string();
    assert!(
        text.contains("timestamps:") && text.contains("goes backwards"),
        "the report does not name the expectation: {text}"
    );
    assert!(
        text.contains("packet ") && text.contains("after packet "),
        "the report does not say which packets disagree: {text}"
    );
}

#[test]
fn a_track_another_source_bled_into_fails_its_isolation_check() {
    // Structurally flawless and audibly wrong: the microphone track carries the
    // game's tone as well as its own. This is the check #28 needs, and the one
    // no amount of `ffprobe` output can make.
    let Some(tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("media-bleed");
    let path = write_recording(
        &tools,
        &directory.file("recording.mkv"),
        SECONDS,
        &[
            AudioSource::tone("Game", GAME),
            AudioSource::mixed("Microphone", MICROPHONE, GAME),
        ],
    );

    let media = Media::open(&path).expect("the recording opens");

    // Nothing structural is wrong with it.
    media
        .validate()
        .audio_stream_count(2)
        .audio(1, AudioStream::codec("pcm_s16le").title("Microphone"))
        .monotonic_timestamps()
        .synchronised_within(Duration::from_millis(50))
        .assert_valid();

    let report = media
        .validate()
        .audio_tone(1, Tone::at(MICROPHONE).isolated_from(GAME))
        .check()
        .expect_err("the game is audible on the microphone track");
    eprintln!("{report}");

    let text = report.to_string();
    assert!(
        text.contains("a:1 isolation") && text.contains("440 Hz belongs to another source"),
        "the report does not name the source that leaked: {text}"
    );
}

#[test]
fn a_silent_track_is_reported_as_silent_rather_than_as_a_frequency() {
    // A track that was declared, named and written, and that nothing was ever
    // routed into. Reporting the strongest frequency in silence would name
    // whatever the arithmetic landed on, which is worse than useless.
    let Some(tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("media-silent");
    let path = write_recording(
        &tools,
        &directory.file("recording.mkv"),
        SECONDS,
        &[AudioSource::silence("Microphone")],
    );

    let media = Media::open(&path).expect("the recording opens");
    let report = media
        .validate()
        .audio_tone(0, Tone::at(MICROPHONE))
        .check()
        .expect_err("a silent track carries no tone");
    eprintln!("{report}");

    assert!(
        report.to_string().contains("the track is silent"),
        "the report does not say the track is empty: {report}"
    );
}

#[test]
fn audio_that_starts_late_fails_the_synchronisation_bound() {
    // Half a second of lip-sync error. Every other check passes: the tracks are
    // both there, both decode, and both have timestamps that only ever
    // increase — they just do not describe the same moment.
    let Some(tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("media-out-of-sync");
    let path = write_recording(
        &tools,
        &directory.file("recording.mkv"),
        SECONDS,
        &[AudioSource::tone("Game", GAME).starting_at(0.5)],
    );

    let media = Media::open(&path).expect("the recording opens");
    media.validate().monotonic_timestamps().assert_valid();

    let report = media
        .validate()
        .synchronised_within(Duration::from_millis(50))
        .check()
        .expect_err("audio half a second behind the video is not in sync");
    eprintln!("{report}");

    let text = report.to_string();
    assert!(
        text.contains("A/V synchronisation: the tracks start 0.500s apart")
            && text.contains("stated 0.050s bound"),
        "the report does not say how far apart the tracks are: {text}"
    );
    assert!(
        text.contains("a:0 starts at 0.500s") && text.contains("v:0 starts at 0.000s"),
        "the report does not say which track is late: {text}"
    );
}
