//! What the writer puts in the file, and what it refuses to do.
//!
//! These tests write real recordings through the public API and then take them
//! apart again, because a muxer that returns `Ok` has not proved anything
//! (AGENTS.md section 22). The audio is uncompressed PCM, which needs no
//! encoder and is therefore genuinely decodable rather than a plausible shape
//! of bytes; the video track is declared and left empty, since what these tests
//! are about is the container's description of the recording. Real encoded
//! video goes through `tests/synthetic_recording.rs`.
//!
//! Taking them apart is `clipped-media-validation` (`tests/media`), the
//! workspace's media harness, which reports every failed expectation at once
//! and is itself tested against files that are broken.

mod support;

use std::time::Duration;

use clipped_media_validation::{
    require_media_tools, AudioStream, Media, TemporaryDirectory, VideoStream,
};
use clipped_muxer::{
    AudioCodec, AudioTrack, EncodedPacket, Language, MkvWriter, MuxError, PacketTimestamp,
    RecordingLayout, TrackId, VideoCodec, VideoTrack,
};

/// 48 kHz stereo, which is what Windows shared-mode audio runs at.
const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: u16 = 2;

/// A real H.264 sequence and picture parameter set, in the Annex B form every
/// Windows hardware encoder emits.
///
/// Matroska requires a video track's out-of-band header, so these tests need
/// one even though they write no video packets. It is real rather than
/// invented: it is what the pinned build's `libopenh264` produced for a 640x360
/// picture, read back out of the file `tests/synthetic_recording.rs` writes
/// (`ffprobe -show_data`) and split at its start codes. Handing the muxer the
/// Annex B form is deliberate — converting it into the `avcC` record Matroska
/// stores is one of the things the muxer does, and
/// [`the_container_stores_the_header_in_the_form_matroska_wants`] checks that
/// it did.
const H264_PARAMETER_SETS: &[u8] = &[
    // Sequence parameter set: 640x360, Constrained Baseline, level 3.0.
    0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0xc0, 0x1e, 0x8c, 0x68, 0x0a, 0x02, 0xff, 0x96, 0x01, 0xe1,
    0x10, 0x8d, 0x40, // Picture parameter set.
    0x00, 0x00, 0x00, 0x01, 0x68, 0xce, 0x3c, 0x80,
];

/// The picture the parameter sets above describe.
const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;

/// A video track carrying the header above.
fn video_track() -> VideoTrack {
    VideoTrack::new(VideoCodec::H264, WIDTH, HEIGHT).with_codec_private(H264_PARAMETER_SETS)
}

/// The length of one audio packet, as Windows loopback capture delivers them.
const PACKET: Duration = Duration::from_millis(20);

/// A recording with one video track and three named audio tracks.
///
/// Three rather than one because multi-track audio is the product (SPEC.md
/// section 11) and a writer that quietly wrote every packet to the first stream
/// would pass every single-track test there is.
fn three_track_layout() -> RecordingLayout {
    RecordingLayout::new(video_track().with_name("Gameplay"))
        .with_audio_track(
            AudioTrack::new(AudioCodec::PcmS16Le, SAMPLE_RATE, CHANNELS)
                .with_name("Compatibility Mix")
                .as_default(),
        )
        .with_audio_track(
            AudioTrack::new(AudioCodec::PcmS16Le, SAMPLE_RATE, CHANNELS).with_name("Game"),
        )
        .with_audio_track(
            AudioTrack::new(AudioCodec::PcmS16Le, SAMPLE_RATE, 1)
                .with_name("Microphone")
                .with_language(Language::ENGLISH),
        )
}

/// One packet of silence for a track with `channels` channels.
fn silence(channels: u16) -> Vec<u8> {
    let frames = SAMPLE_RATE as usize * PACKET.as_millis() as usize / 1000;
    vec![0; frames * usize::from(channels) * 2]
}

/// Writes `packets` packets of silence to every audio track of the layout
/// above, one packet length apart.
fn write_silence(writer: &mut MkvWriter, packet_count: u32) {
    for index in 0..packet_count {
        let timestamp = PacketTimestamp::from_nanos(i64::from(index) * PACKET.as_nanos() as i64);
        for (track, channels) in [(0, CHANNELS), (1, CHANNELS), (2, 1)] {
            let audio = silence(channels);
            writer
                .write_packet(
                    &EncodedPacket::new(TrackId::Audio(track), timestamp, &audio)
                        .with_duration(PACKET)
                        .with_keyframe(true),
                )
                .expect("silence can be written");
        }
    }
}

#[test]
fn audio_tracks_keep_their_names_languages_and_default_flag() {
    let Some(_tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("muxer-named-tracks");
    let path = directory.file("recording.mkv");

    let mut writer =
        MkvWriter::create(&path, &three_track_layout()).expect("the recording can be created");
    write_silence(&mut writer, 50);
    let summary = writer.finish().expect("the recording can be finished");

    assert_eq!(summary.packets, 150);
    assert_eq!(summary.timestamps_corrected(), 0);

    let media = Media::open(&path).expect("a finished recording opens");
    assert!(
        media.diagnostics().is_empty(),
        "ffprobe reported: {}",
        media.diagnostics()
    );

    media
        .validate()
        // One video track, declared even though nothing was written to it, and
        // three audio tracks in the order they were declared. The names are
        // what an editor shows instead of `Audio 2` (ADR 0001), and the
        // compatibility mix is the track a player should choose on its own
        // (SPEC.md section 13).
        .video(VideoStream::codec("h264").resolution(WIDTH, HEIGHT))
        .audio_stream_count(3)
        .audio(
            0,
            AudioStream::codec("pcm_s16le")
                .sample_rate(SAMPLE_RATE)
                .channels(CHANNELS)
                .title("Compatibility Mix")
                .default_track(true),
        )
        .audio(
            1,
            AudioStream::codec("pcm_s16le")
                .sample_rate(SAMPLE_RATE)
                .channels(CHANNELS)
                .title("Game")
                .default_track(false),
        )
        .audio(
            2,
            AudioStream::codec("pcm_s16le")
                .sample_rate(SAMPLE_RATE)
                // The microphone track was declared as mono and has to stay
                // mono.
                .channels(1)
                .title("Microphone")
                .language("eng")
                .default_track(false),
        )
        // A second of media, give or take the last packet's length: fifty 20 ms
        // packets.
        .duration_seconds(1.015, 0.035)
        .assert_valid();

    // Matroska omits the language element when it is `und`, which is why only
    // the microphone track carries one.
    assert_eq!(media.audio_streams()[0].tag("language"), None);
}

#[test]
fn every_track_gets_its_own_packets() {
    // A writer that sent every packet to the first stream would satisfy every
    // assertion about names and codecs above, and produce a recording with one
    // usable track. The packet counts are what catch it.
    let Some(_tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("muxer-packet-routing");
    let path = directory.file("recording.mkv");

    let mut writer =
        MkvWriter::create(&path, &three_track_layout()).expect("the recording can be created");
    write_silence(&mut writer, 25);
    writer.finish().expect("the recording can be finished");

    Media::open(&path)
        .expect("a finished recording opens")
        .validate()
        .audio(0, AudioStream::codec("pcm_s16le").packets(25))
        .audio(1, AudioStream::codec("pcm_s16le").packets(25))
        .audio(2, AudioStream::codec("pcm_s16le").packets(25))
        .assert_valid();
}

#[test]
fn packets_that_arrive_out_of_order_are_still_written_in_order() {
    // A clock that steps, or a queue that reorders. The recording must survive
    // it: dropping the packet loses audio and refusing the write loses the
    // session (AGENTS.md section 17).
    let Some(_tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("muxer-out-of-order");
    let path = directory.file("recording.mkv");

    let layout = RecordingLayout::new(video_track()).with_audio_track(AudioTrack::new(
        AudioCodec::PcmS16Le,
        SAMPLE_RATE,
        CHANNELS,
    ));

    let mut writer = MkvWriter::create(&path, &layout).expect("the recording can be created");
    let audio = silence(CHANNELS);
    // The third packet is timed before the second.
    for millis in [0_i64, 20, 10, 40, 60] {
        writer
            .write_packet(
                &EncodedPacket::new(
                    TrackId::Audio(0),
                    PacketTimestamp::from_nanos(millis * 1_000_000),
                    &audio,
                )
                .with_duration(PACKET)
                .with_keyframe(true),
            )
            .expect("a packet with a backwards timestamp is still written");
    }
    let summary = writer.finish().expect("the recording can be finished");

    assert_eq!(summary.packets, 5, "nothing was dropped");
    assert_eq!(
        summary.timestamps_forced_monotonic, 1,
        "exactly one packet needed correcting, and the summary has to say so"
    );

    let media = Media::open(&path).expect("a finished recording opens");
    media.validate().monotonic_timestamps().assert_valid();

    let written = media.packets();
    assert_eq!(written.len(), 5);
    // The corrected packet is one millisecond past its predecessor, so the
    // packet after it is unaffected.
    let times: Vec<_> = written
        .iter()
        .map(|packet| (packet.decode_seconds * 1000.0).round() as i64)
        .collect();
    assert_eq!(times, [0, 20, 21, 40, 60]);
}

#[test]
fn the_container_stores_the_header_in_the_form_matroska_wants() {
    // The caller hands over the Annex B parameter sets a hardware encoder
    // produces; Matroska stores an `avcC` record and length-prefixed NAL units.
    // If that conversion did not happen the file would still open, and no
    // player would decode it.
    let Some(_tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("muxer-codec-private");
    let path = directory.file("recording.mkv");

    let mut writer =
        MkvWriter::create(&path, &three_track_layout()).expect("the recording can be created");
    write_silence(&mut writer, 5);
    writer.finish().expect("the recording can be finished");

    // Read from the stream directly rather than through an expectation: how a
    // codec header is stored is a question about H.264 in Matroska, and the
    // harness has no opinion about it.
    let media = Media::open(&path).expect("a finished recording opens");
    let video = media.video_streams();
    assert_eq!(
        video[0].field("is_avc"),
        Some("true"),
        "the header was stored as-is instead of being converted to an avcC record"
    );
    assert_eq!(video[0].field("nal_length_size"), Some("4"));
    // 27 bytes of Annex B become a 30-byte avcC record: two start codes are
    // replaced by the record's own header and two length fields.
    assert_eq!(video[0].number("extradata_size"), Some(30.0));
}

#[test]
fn a_video_track_without_its_out_of_band_header_is_refused() {
    // FFmpeg refuses this too, but only from `avformat_write_header`, with
    // `Invalid data found when processing input` and a file already created.
    // The caller is told what is missing instead (AGENTS.md section 15).
    let directory = TemporaryDirectory::new("muxer-no-video-header");
    let path = directory.file("recording.mkv");

    let layout = RecordingLayout::new(VideoTrack::new(VideoCodec::H264, WIDTH, HEIGHT));
    let error = MkvWriter::create(&path, &layout).expect_err("H.264 needs its parameter sets");

    match error {
        MuxError::InvalidTrack { track, reason } => {
            assert_eq!(track, TrackId::Video);
            assert!(reason.contains("SPS and PPS"), "unhelpful reason: {reason}");
        }
        other => panic!("unexpected error: {other}"),
    }
    assert!(
        !path.exists(),
        "the recording was refused, so nothing should have been created"
    );
}

#[test]
fn an_audio_track_without_the_header_its_codec_needs_is_refused() {
    // Opus stores its identification header in the track entry. PCM has none to
    // store, which is why the tracks above need nothing.
    let directory = TemporaryDirectory::new("muxer-no-audio-header");
    let path = directory.file("recording.mkv");

    let layout = RecordingLayout::new(video_track())
        .with_audio_track(AudioTrack::new(AudioCodec::Opus, 48_000, 2).with_name("Game"));
    let error = MkvWriter::create(&path, &layout).expect_err("Opus needs its header");

    match error {
        MuxError::InvalidTrack { track, reason } => {
            assert_eq!(track, TrackId::Audio(0));
            assert!(
                reason.contains("identification header"),
                "unhelpful reason: {reason}"
            );
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn a_recording_is_never_written_over_an_existing_file() {
    // The failure this prevents is a session pointed at the name of a recording
    // that already exists. Opening for writing would truncate it, and the
    // footage is not recoverable (AGENTS.md section 56).
    let directory = TemporaryDirectory::new("muxer-no-overwrite");
    let path = directory.file("recording.mkv");
    std::fs::write(&path, b"an irreplaceable recording").expect("the file can be created");

    let error = MkvWriter::create(&path, &three_track_layout())
        .expect_err("an existing file must not be opened for writing");

    assert!(
        matches!(error, MuxError::OutputExists { .. }),
        "unexpected error: {error}"
    );
    assert_eq!(
        std::fs::read(&path).expect("the file is still there"),
        b"an irreplaceable recording",
        "the existing file was modified"
    );
}

#[test]
fn a_recording_that_fails_to_start_leaves_no_file_behind() {
    // The file is created before the container header is written, so a failure
    // in between can leave an empty one. That matters here more than it usually
    // would: `create` refuses a path that exists rather than truncating it, so
    // a leftover would make that name permanently unrecordable, and a session
    // retrying after a transient failure would be told it was about to
    // overwrite a recording.
    //
    // The failure used is a codec header the muxer accepts as present and
    // FFmpeg then rejects while writing the header: four bytes that are neither
    // Annex B parameter sets nor an `avcC` record.
    let directory = TemporaryDirectory::new("muxer-failed-start");
    let path = directory.file("recording.mkv");

    let layout = RecordingLayout::new(
        VideoTrack::new(VideoCodec::H264, WIDTH, HEIGHT).with_codec_private([2, 3, 4, 5]),
    );
    let error = MkvWriter::create(&path, &layout)
        .expect_err("FFmpeg cannot write a header for a track whose codec header is nonsense");

    assert!(
        matches!(error, MuxError::Ffmpeg { .. }),
        "unexpected error: {error}"
    );
    assert!(
        !path.exists(),
        "a failed start left {} behind, and that name can now never be recorded to",
        path.display()
    );
}

#[test]
fn a_packet_for_a_track_the_recording_does_not_have_is_refused() {
    let directory = TemporaryDirectory::new("muxer-unknown-track");
    let path = directory.file("recording.mkv");

    let layout = RecordingLayout::new(video_track()).with_audio_track(AudioTrack::new(
        AudioCodec::PcmS16Le,
        SAMPLE_RATE,
        CHANNELS,
    ));
    let mut writer = MkvWriter::create(&path, &layout).expect("the recording can be created");

    let audio = silence(CHANNELS);
    let error = writer
        .write_packet(&EncodedPacket::new(
            TrackId::Audio(4),
            PacketTimestamp::from_nanos(0),
            &audio,
        ))
        .expect_err("a track that was never declared cannot be written to");

    match error {
        MuxError::UnknownTrack {
            track,
            audio_tracks,
        } => {
            assert_eq!(track, TrackId::Audio(4));
            assert_eq!(audio_tracks, 1);
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn a_packet_with_no_data_is_refused() {
    let directory = TemporaryDirectory::new("muxer-empty-packet");
    let path = directory.file("recording.mkv");

    let layout = RecordingLayout::new(video_track()).with_audio_track(AudioTrack::new(
        AudioCodec::PcmS16Le,
        SAMPLE_RATE,
        CHANNELS,
    ));
    let mut writer = MkvWriter::create(&path, &layout).expect("the recording can be created");

    let error = writer
        .write_packet(&EncodedPacket::new(
            TrackId::Audio(0),
            PacketTimestamp::from_nanos(0),
            &[],
        ))
        .expect_err("a packet with no bytes is not media");

    assert!(
        matches!(error, MuxError::EmptyPacket { .. }),
        "unexpected error: {error}"
    );
}

#[test]
fn a_writer_dropped_without_being_finished_still_finishes_the_file() {
    // A session that fails or panics drops its writer without calling `finish`.
    // The recording that was being made is still the user's, so the trailer is
    // written on the way out and the file arrives complete.
    let Some(_tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("muxer-dropped");
    let path = directory.file("recording.mkv");

    {
        let mut writer =
            MkvWriter::create(&path, &three_track_layout()).expect("the recording can be created");
        write_silence(&mut writer, 25);
    }

    Media::open(&path)
        .expect("a file whose trailer was written opens")
        .validate()
        .audio_stream_count(3)
        // Twenty-five 20 ms packets: about half a second.
        .duration_seconds(0.515, 0.035)
        .assert_valid();
}

#[test]
fn timestamps_are_relative_to_the_first_packet_of_the_recording() {
    // Callers pass performance counter readings, which count from when the
    // machine booted. A recording that started them at their face value would
    // be a file whose first frame is days in.
    let Some(_tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("muxer-rebased");
    let path = directory.file("recording.mkv");

    let layout = RecordingLayout::new(video_track()).with_audio_track(AudioTrack::new(
        AudioCodec::PcmS16Le,
        SAMPLE_RATE,
        CHANNELS,
    ));
    let mut writer = MkvWriter::create(&path, &layout).expect("the recording can be created");

    // Ten days of uptime, in nanoseconds.
    let boot_offset = 10 * 24 * 60 * 60 * 1_000_000_000_i64;
    let audio = silence(CHANNELS);
    for index in 0..10_i64 {
        writer
            .write_packet(
                &EncodedPacket::new(
                    TrackId::Audio(0),
                    PacketTimestamp::from_nanos(boot_offset + index * PACKET.as_nanos() as i64),
                    &audio,
                )
                .with_duration(PACKET)
                .with_keyframe(true),
            )
            .expect("a packet timed against the machine's uptime can be written");
    }
    writer.finish().expect("the recording can be finished");

    let media = Media::open(&path).expect("a finished recording opens");
    let written = media.packets();
    assert_eq!(written.len(), 10);
    assert_eq!(written[0].decode_seconds, 0.0);
    assert!(
        (written[9].decode_seconds - 0.18).abs() < 0.002,
        "the tenth packet should be 180 ms in, not {}s",
        written[9].decode_seconds
    );
}
