//! Whether a recording's audio sources really stayed apart.
//!
//! Everything else in this crate's tests is about structure: the tracks are
//! declared, named, ordered, counted and timed. A writer that sent every source
//! into one stream and left the rest of the tracks holding a copy of it would
//! satisfy all of that (AGENTS.md section 21), so this file asserts on what is
//! *audible* on each track — a Goertzel filter over the decoded samples, through
//! `clipped-media-validation`'s [`Tone`], which measures how much energy sits at
//! a frequency and how much sits at one that belongs to somebody else.
//!
//! The tones are AGENTS.md section 26's own: 440 Hz for the game, 880 Hz for
//! other system audio, 1320 Hz for the microphone. They are produced by
//! `examples/synthetic_recording.rs`, which generates interleaved `f32` samples
//! the way `clipped-audio` does and writes them through
//! [`AudioTrackWriter`](clipped_muxer::AudioTrackWriter) — the same path a
//! recording session will take.

mod support;

use std::time::Duration;

use clipped_media_validation::{require_media_tools, AudioStream, Media, TemporaryDirectory, Tone};
use clipped_muxer::{
    AudioSource, AudioTrack, AudioTrackWriter, MkvWriter, MuxError, PacketTimestamp,
    RecordingLayout, TrackId, VideoCodec, VideoTrack,
};
use support::run_synthetic_recording;

/// Long enough for the analysis window a quarter of the way in to hold a
/// quarter-second of tone, short enough that the software encoder is quick.
const SECONDS: f64 = 3.0;

/// The frequency each source's track carries.
const GAME: f64 = 440.0;
const OTHER_SYSTEM_AUDIO: f64 = 880.0;
const MICROPHONE: f64 = 1320.0;

/// A real H.264 parameter set, for the layouts written in this file directly.
///
/// The same bytes `tests/mkv_writing.rs` uses, and for the same reason: Matroska
/// requires a video track's out-of-band header even in a recording that writes
/// no picture.
const H264_PARAMETER_SETS: &[u8] = &[
    0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0xc0, 0x1e, 0x8c, 0x68, 0x0a, 0x02, 0xff, 0x96, 0x01, 0xe1,
    0x10, 0x8d, 0x40, 0x00, 0x00, 0x00, 0x01, 0x68, 0xce, 0x3c, 0x80,
];

const SAMPLE_RATE: u32 = 48_000;

/// Writes a four-track recording with the example and returns where it is.
fn record_four_tracks(directory: &TemporaryDirectory, name: &str) -> std::path::PathBuf {
    let path = directory.file(name);
    let output = run_synthetic_recording(&[
        "--output",
        &path.to_string_lossy(),
        "--seconds",
        &SECONDS.to_string(),
        "--audio-tracks",
        "4",
    ]);
    assert!(
        output.status.success(),
        "the recorder failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    path
}

#[test]
fn every_source_is_audible_on_its_own_track_and_on_no_other() {
    let Some(_tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("muxer-track-isolation");
    let path = record_four_tracks(&directory, "recording.mkv");

    let media = Media::open(&path).expect("a finished recording opens");
    media
        .validate()
        // The tracks the model declares, in the model's order, with the names
        // an editor shows and the compatibility mix marked as the one a player
        // takes on its own (SPEC.md sections 11 and 13).
        .audio_stream_count(4)
        .audio(
            0,
            AudioStream::codec("pcm_s16le")
                .sample_rate(SAMPLE_RATE)
                .channels(2)
                .title("Compatibility Mix")
                .default_track(true),
        )
        .audio(
            1,
            AudioStream::codec("pcm_s16le")
                .title("Game")
                .default_track(false),
        )
        .audio(
            2,
            AudioStream::codec("pcm_s16le")
                .title("Other System Audio")
                .default_track(false),
        )
        .audio(
            3,
            AudioStream::codec("pcm_s16le")
                .title("Microphone")
                .default_track(false),
        )
        // And what is actually on them. This is the assertion the feature exists
        // for: each source's own tone, and none of anybody else's.
        .audio_tone(
            1,
            Tone::at(GAME)
                .isolated_from(OTHER_SYSTEM_AUDIO)
                .isolated_from(MICROPHONE),
        )
        .audio_tone(
            2,
            Tone::at(OTHER_SYSTEM_AUDIO)
                .isolated_from(GAME)
                .isolated_from(MICROPHONE),
        )
        .audio_tone(
            3,
            Tone::at(MICROPHONE)
                .isolated_from(GAME)
                .isolated_from(OTHER_SYSTEM_AUDIO),
        )
        // The tracks still line up with each other and with the picture: an
        // isolation test passes just as happily on a recording whose microphone
        // track starts a second late.
        .synchronised_within(Duration::from_millis(40))
        .streams_start_at(0.0, 0.001)
        .assert_valid();

    // The compatibility mix is the other half of the same claim. Every tone in
    // the recording is in it — that is what makes it the track a naive player
    // should take (SPEC.md section 13) — which also means the isolation above is
    // a statement about routing rather than about a file that only ever
    // contained one tone.
    let mix = media
        .audio_content(0)
        .expect("the compatibility mix decodes");
    let own = mix.magnitude_at(GAME);
    for tone in [GAME, OTHER_SYSTEM_AUDIO, MICROPHONE] {
        let magnitude = mix.magnitude_at(tone);
        assert!(
            magnitude > own / 2.0,
            "the compatibility mix should carry every source: {tone} Hz measures {magnitude:.4} \
             against {GAME} Hz at {own:.4}"
        );
    }
}

#[test]
fn a_source_that_produced_nothing_leaves_a_declared_but_empty_track() {
    // The microphone Windows had muted, the device that never opened, the
    // application that was routed to a track and never started. Matroska fixes
    // its track list in the header, so the track is in the file whatever
    // happens; what must not happen is the recorder saying nothing about it, or
    // inventing audio to fill it (AGENTS.md sections 21 and 54).
    let Some(_tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("muxer-silent-source");
    let path = directory.file("recording.mkv");

    let layout = RecordingLayout::new(
        VideoTrack::new(VideoCodec::H264, 640, 360).with_codec_private(H264_PARAMETER_SETS),
    )
    .with_audio_track(AudioTrack::for_source(
        AudioSource::CompatibilityMix,
        SAMPLE_RATE,
        2,
    ))
    .with_audio_track(AudioTrack::for_source(AudioSource::Game, SAMPLE_RATE, 2))
    .with_audio_track(AudioTrack::for_source(
        AudioSource::Microphone,
        SAMPLE_RATE,
        1,
    ));

    let microphone = layout
        .audio_track_for(&AudioSource::Microphone)
        .expect("the microphone track was declared");
    let mut writer = MkvWriter::create(&path, &layout).expect("the recording can be created");

    // Half a second on the two tracks whose sources produced something, and
    // nothing at all on the microphone.
    let mut tracks: Vec<AudioTrackWriter> = layout
        .audio_tracks()
        .iter()
        .enumerate()
        .filter(|(_, track)| track.source() != Some(&AudioSource::Microphone))
        .map(|(index, track)| {
            let index = u16::try_from(index).expect("three tracks fit in a u16");
            AudioTrackWriter::new(TrackId::Audio(index), track)
                .expect("a PCM track can be written to")
        })
        .collect();

    let samples = vec![0.25_f32; SAMPLE_RATE as usize / 100]; // 5 ms, stereo
    for packet in 0..100_i64 {
        let at = PacketTimestamp::from_nanos(packet * 5_000_000);
        for track in &mut tracks {
            track
                .write_samples(&mut writer, at, &samples)
                .expect("samples can be written");
        }
    }

    assert_eq!(
        writer.packets_written(microphone),
        Some(0),
        "nothing was written to the microphone track, and the writer has to know it"
    );
    let summary = writer.finish().expect("the recording can be finished");
    assert_eq!(
        summary.audio_tracks_without_packets, 1,
        "the summary is what tells a session to tell the user their microphone track is empty"
    );
    assert!(
        summary.to_string().contains("1 audio tracks with no audio"),
        "the summary should say so in words as well: {summary}"
    );

    let media = Media::open(&path).expect("a finished recording opens");
    media
        .validate()
        // The track is in the file, named, and holds nothing: no manufactured
        // silence, and no track quietly left out because its source was quiet.
        .audio_stream_count(3)
        .audio(2, AudioStream::codec("pcm_s16le").title("Microphone"))
        .assert_valid();

    let microphone_stream = media.audio_streams()[2]
        .number("index")
        .expect("the microphone stream has an index") as i64;
    let packets = media
        .packets()
        .iter()
        .filter(|packet| packet.stream_index == microphone_stream)
        .count();
    assert_eq!(packets, 0, "the empty track was written to after all");
}

#[test]
fn samples_that_are_not_a_whole_number_of_frames_are_refused() {
    // The caller's channel count and the track's have diverged. Writing the
    // samples anyway swaps the channels of every frame after the short one, and
    // nothing about the file looks wrong.
    let directory = TemporaryDirectory::new("muxer-partial-frame");
    let path = directory.file("recording.mkv");

    let stereo = AudioTrack::for_source(AudioSource::Game, SAMPLE_RATE, 2);
    let layout = RecordingLayout::new(
        VideoTrack::new(VideoCodec::H264, 640, 360).with_codec_private(H264_PARAMETER_SETS),
    )
    .with_audio_track(stereo.clone());

    let mut writer = MkvWriter::create(&path, &layout).expect("the recording can be created");
    let mut track =
        AudioTrackWriter::new(TrackId::Audio(0), &stereo).expect("a PCM track can be written to");

    let error = track
        .write_samples(&mut writer, PacketTimestamp::from_nanos(0), &[0.1; 7])
        .expect_err("seven samples is three and a half stereo frames");
    match error {
        MuxError::PartialAudioFrame {
            track,
            samples,
            channels,
        } => {
            assert_eq!(track, TrackId::Audio(0));
            assert_eq!((samples, channels), (7, 2));
        }
        other => panic!("unexpected error: {other}"),
    }
    assert_eq!(
        writer.packets_written(TrackId::Audio(0)),
        Some(0),
        "the refused buffer must not have been partly written"
    );
}

#[test]
fn an_audio_track_with_a_blank_name_is_refused_before_anything_is_recorded() {
    // Reachable through an application track configured with an empty name.
    // Matroska would write the empty element, an editor would show an unnamed
    // row, and the recording is the one thing nobody can correct afterwards.
    let directory = TemporaryDirectory::new("muxer-blank-track-name");
    let path = directory.file("recording.mkv");

    let layout = RecordingLayout::new(
        VideoTrack::new(VideoCodec::H264, 640, 360).with_codec_private(H264_PARAMETER_SETS),
    )
    .with_audio_track(AudioTrack::for_source(
        AudioSource::application("   "),
        SAMPLE_RATE,
        2,
    ));

    let error = MkvWriter::create(&path, &layout).expect_err("a blank track name is not a name");
    match error {
        MuxError::InvalidTrack { track, reason } => {
            assert_eq!(track, TrackId::Audio(0));
            assert!(reason.contains("blank"), "unhelpful reason: {reason}");
        }
        other => panic!("unexpected error: {other}"),
    }
    assert!(
        !path.exists(),
        "the recording was refused, so nothing should have been created"
    );
}

#[test]
fn adding_audio_tracks_does_not_move_the_video() {
    // The third acceptance criterion of issue #28, and the one that would
    // otherwise be taken on trust: audio and video share one interleaving queue
    // inside libavformat, so "the picture is unaffected" is a claim about a
    // component that does see the extra tracks. Two recordings of the same
    // synthetic pattern, one with a single audio track and one with five, must
    // hold the same video packets at the same times.
    let Some(_tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("muxer-track-count-sync");

    let mut video_timings = Vec::new();
    for tracks in ["1", "5"] {
        let path = directory.file(&format!("recording-{tracks}.mkv"));
        let output = run_synthetic_recording(&[
            "--output",
            &path.to_string_lossy(),
            "--seconds",
            &SECONDS.to_string(),
            "--audio-tracks",
            tracks,
        ]);
        assert!(
            output.status.success(),
            "the recorder failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let media = Media::open(&path).expect("a finished recording opens");
        media
            .validate()
            .audio_stream_count(tracks.parse().expect("a track count"))
            .synchronised_within(Duration::from_millis(40))
            .streams_start_at(0.0, 0.001)
            .monotonic_timestamps()
            .assert_valid();

        let video = media.video_streams()[0]
            .number("index")
            .expect("the video stream has an index") as i64;
        video_timings.push(
            media
                .packets()
                .iter()
                .filter(|packet| packet.stream_index == video)
                .map(|packet| (packet.decode_seconds, packet.presentation_seconds))
                .collect::<Vec<_>>(),
        );
    }

    assert!(
        !video_timings[0].is_empty(),
        "no video packets came back, so this would compare two empty lists"
    );
    assert_eq!(
        video_timings[0], video_timings[1],
        "the video's timestamps changed when the recording gained four audio tracks"
    );
}
