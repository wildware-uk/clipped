//! Remuxing a finished recording into MP4, and taking both files apart to see
//! whether the copy is really the recording.
//!
//! Issue #92's criteria are what this file is organised around:
//!
//! - the result plays and keeps every audio track, or the user is told in
//!   advance what will be lost;
//! - it is not re-encoded, so no quality is traded for the container;
//! - a failure leaves the recording untouched.
//!
//! Every assertion about the MP4 is made against the **source**, measured from
//! the same file in the same run, rather than against numbers written down here.
//! A remux that dropped every other frame would satisfy a hard-coded frame count
//! chosen to match it; it cannot satisfy the source's.
//!
//! `decoded_frames` throughout, never packet counts on their own: a container
//! can list ninety-two packets, have monotonic timestamps and one video stream,
//! and still decode to nothing at all (`tests/media`,
//! `decoded_frames_at_least`).

mod support;

use std::path::{Path, PathBuf};
use std::time::Duration;

use clipped_media_validation::{
    require_media_tools, AudioStream, Media, Stream, TemporaryDirectory, Tone, VideoStream,
};
use clipped_muxer::{
    remux_to_mp4, remux_to_mp4_carrying, AudioTracks, Carriage, Mp4Plan, RemuxError, TrackKind,
};
use support::{build_fixture_with_ffmpeg, run_synthetic_recording};

/// How long a recording these tests write.
///
/// Long enough for several keyframes, several clusters and hundreds of packets;
/// short enough that the software encoder produces it in well under a second.
const SECONDS: f64 = 4.0;
const FRAME_RATE: f64 = 30.0;

/// 20 ms audio packets for the whole recording.
const AUDIO_PACKETS_PER_SECOND: f64 = 50.0;

/// Writes a synthetic recording into `directory` and returns its path.
fn record(directory: &TemporaryDirectory, name: &str, extra: &[&str]) -> PathBuf {
    let path = directory.file(name);
    let mut arguments = vec![
        "--output".to_owned(),
        path.to_string_lossy().into_owned(),
        "--seconds".to_owned(),
        SECONDS.to_string(),
    ];
    arguments.extend(extra.iter().map(|argument| (*argument).to_owned()));

    let borrowed: Vec<&str> = arguments.iter().map(String::as_str).collect();
    let output = run_synthetic_recording(&borrowed);
    assert!(
        output.status.success(),
        "the recorder failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    path
}

/// Everything about a track that must survive a remux.
///
/// Two things legitimately differ between the two containers, and both are
/// normalised here rather than excused in an assertion:
///
/// - **Where the name is stored.** Matroska keeps it in the track entry's `Name`
///   element, which `ffprobe` reports as `title`; MP4 keeps it in a `udta`/`name`
///   box, which `ffprobe` reports as `name`. The string is the same, which is
///   what this compares. `clipped_muxer::remux` promises that in its
///   documentation and this is what holds it to account.
/// - **How "no language" is spelled.** Matroska omits the element entirely for
///   an unknown language; MP4's media header has no way to omit it and writes
///   `und`, which is the same statement. Anything else — `eng` becoming `und`,
///   or `deu` becoming `eng` — is a real loss and is compared.
#[derive(Debug, PartialEq)]
struct TrackShape {
    kind: String,
    codec: String,
    name: Option<String>,
    language: Option<String>,
    default: bool,
    width: Option<f64>,
    height: Option<f64>,
    sample_rate: Option<f64>,
    channels: Option<f64>,
    packets: Option<f64>,
    decoded_frames: Option<f64>,
}

impl TrackShape {
    fn of(stream: &Stream<'_>) -> Self {
        Self {
            kind: stream.kind().to_owned(),
            codec: stream.field("codec_name").unwrap_or_default().to_owned(),
            name: stream
                .tag("title")
                .or_else(|| stream.tag("name"))
                .map(str::to_owned),
            language: stream
                .tag("language")
                .filter(|language| *language != "und")
                .map(str::to_owned),
            default: stream.is_default(),
            width: stream.number("width"),
            height: stream.number("height"),
            sample_rate: stream.number("sample_rate"),
            channels: stream.number("channels"),
            packets: stream.number("nb_read_packets"),
            decoded_frames: stream.number("nb_read_frames"),
        }
    }
}

/// Every track of a file, in the order the container declares them.
fn shape(media: &Media) -> Vec<TrackShape> {
    media.streams().iter().map(TrackShape::of).collect()
}

/// Each stream's declared start, in the order the container declares them.
fn starts(media: &Media) -> Vec<f64> {
    media
        .streams()
        .iter()
        .map(|stream| {
            stream
                .number("start_time")
                .expect("a finished file declares where each of its streams begins")
        })
        .collect()
}

#[test]
fn a_remuxed_recording_holds_every_track_the_source_did_and_decodes_the_same_pictures() {
    let Some(_tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("muxer-remux");

    // Three audio tracks, because a recording has several (SPEC.md section 11)
    // and "retains every audio track" is the first acceptance criterion. A
    // remuxer that carried only the first would pass every other check here.
    // With a language stated on the audio tracks, which the track model does not
    // invent (`clipped_muxer::AudioTrack::for_source`): the assertions below
    // include the language surviving the change of container, and Matroska omits
    // the element for an unknown language while MP4 writes `und`, so a recording
    // that stated nothing would satisfy them whether or not the tag was carried.
    let source = record(
        &directory,
        "recording.mkv",
        &["--audio-tracks", "3", "--audio-language", "eng"],
    );
    let destination = directory.file("recording.mp4");

    let summary = remux_to_mp4(&source, &destination).expect("the recording remuxes");
    assert!(
        summary.plan().is_lossless(),
        "the plan reported a loss for a recording MP4 can hold whole: {}",
        summary.plan()
    );

    let recorded = Media::open(&source).expect("the recording opens");
    let remuxed = Media::open(&destination).expect("the MP4 opens");
    assert!(
        remuxed.diagnostics().is_empty(),
        "ffprobe could not read the MP4 cleanly: {}",
        remuxed.diagnostics()
    );

    // The layout, compared against the source rather than against a list
    // written here: codecs, names, languages, default flags, channel counts,
    // packet counts and decoded frame counts, track by track.
    assert_eq!(
        shape(&remuxed),
        shape(&recorded),
        "the MP4's tracks are not the recording's tracks"
    );

    let audio_packets = (SECONDS * AUDIO_PACKETS_PER_SECOND) as u64;
    let source_duration = recorded
        .duration_seconds()
        .expect("a finished recording records its duration");

    remuxed
        .validate()
        .stream_count(4)
        .audio_stream_count(3)
        .video(
            VideoStream::codec("h264")
                .resolution(640, 360)
                .pixel_format("yuv420p")
                // Every picture comes out of a decoder. This is the assertion a
                // file that lists a video stream and plays nothing fails.
                .decoded_frames((SECONDS * FRAME_RATE) as u64)
                .packets((SECONDS * FRAME_RATE) as u64)
                // The nominal frame rate is what an editor labels a clip with,
                // and it is a property of the track rather than of the
                // container, so it has to survive the change of one.
                .frame_rate("30/1"),
        )
        .audio(
            0,
            AudioStream::codec("pcm_s16le")
                .sample_rate(48_000)
                .channels(2)
                .packets(audio_packets)
                .language("eng")
                // The compatibility mix is the track a player should choose on
                // its own (SPEC.md section 13). An MP4 that lost the flag plays
                // the wrong sound and looks perfect.
                .default_track(true),
        )
        .audio(
            1,
            AudioStream::codec("pcm_s16le")
                .sample_rate(48_000)
                .channels(2)
                .packets(audio_packets)
                .language("eng")
                .default_track(false),
        )
        .audio(
            2,
            AudioStream::codec("pcm_s16le")
                .sample_rate(48_000)
                .channels(2)
                .packets(audio_packets)
                .language("eng")
                .default_track(false),
        )
        .duration_seconds(source_duration, 0.01)
        .monotonic_timestamps()
        .synchronised_within(Duration::from_millis(40))
        .streams_start_at(0.0, 0.001)
        .assert_valid();

    // `title` in Matroska, `name` in MP4 — the same string in a different box.
    // Asserted directly as well as through `TrackShape`, because the module
    // documentation makes a promise about it that somebody will otherwise only
    // discover by looking for a tag that is not there.
    let names: Vec<Option<&str>> = remuxed
        .streams()
        .iter()
        .map(|stream| stream.tag("name"))
        .collect();
    assert_eq!(
        names,
        [
            Some("Gameplay"),
            Some("Compatibility Mix"),
            Some("Game"),
            Some("Other System Audio")
        ],
        "the track names did not survive into the MP4's `name` boxes"
    );

    assert_eq!(summary.packets(), (recorded.packets().len()) as u64);
    assert!(
        summary.duration().as_secs_f64() > SECONDS - 0.1,
        "the summary claims {:?} of media for a {SECONDS}s recording",
        summary.duration()
    );
}

#[test]
fn the_track_a_player_should_choose_survives_even_when_it_is_not_the_first() {
    let Some(_tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("muxer-remux-default");

    // The default track has to be the *second* one for this to prove anything.
    // FFmpeg's MP4 muxer enables the first track of each kind whether or not it
    // was told to, so a recording whose default audio track is the first one
    // produces an identical MP4 with the flag copied and with the copy deleted —
    // which is a test that cannot fail. This recording marks the second.
    let source = record(
        &directory,
        "recording.mkv",
        &["--audio-tracks", "3", "--default-audio-track", "1"],
    );
    let destination = directory.file("recording.mp4");
    remux_to_mp4(&source, &destination).expect("the recording remuxes");

    let recorded = Media::open(&source).expect("the recording opens");
    let remuxed = Media::open(&destination).expect("the MP4 opens");

    let flags: Vec<bool> = remuxed
        .audio_streams()
        .iter()
        .map(Stream::is_default)
        .collect();
    assert_eq!(
        flags,
        recorded
            .audio_streams()
            .iter()
            .map(Stream::is_default)
            .collect::<Vec<bool>>()
    );
    assert_eq!(
        flags,
        [false, true, false],
        "the MP4 marks the wrong audio track as the one to play"
    );
}

#[test]
fn the_recording_is_byte_for_byte_unchanged_by_a_remux() {
    let Some(_tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("muxer-remux-readonly");
    let source = record(&directory, "recording.mkv", &[]);

    let before = std::fs::read(&source).expect("the recording can be read");
    remux_to_mp4(&source, &directory.file("recording.mp4")).expect("the recording remuxes");
    let after = std::fs::read(&source).expect("the recording can be read again");

    assert_eq!(
        before.len(),
        after.len(),
        "the remux changed the length of the recording"
    );
    assert!(
        before == after,
        "the remux changed the bytes of the recording it was copying"
    );
}

#[test]
fn no_packet_is_re_encoded_on_the_way_into_the_mp4() {
    let Some(_tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("muxer-remux-lossless");
    let source = record(&directory, "recording.mkv", &["--audio-tracks", "2"]);
    let destination = directory.file("recording.mp4");

    remux_to_mp4(&source, &destination).expect("the recording remuxes");

    // The whole claim of a remux, and the only assertion that can carry it. A
    // duration, a frame count and a stream layout are all satisfied by a file
    // that went through an encoder and came out looking worse; identical coded
    // bytes are not.
    let recorded = payloads_by_stream(&source);
    let remuxed = payloads_by_stream(&destination);

    assert_eq!(
        remuxed.len(),
        recorded.len(),
        "the MP4 has a different number of streams to the recording"
    );
    for (stream, expected) in recorded.iter().enumerate() {
        assert_eq!(
            &remuxed[stream], expected,
            "stream {stream} of the MP4 does not hold the recording's coded bytes; the media \
             was re-encoded or reordered rather than copied"
        );
    }
}

/// Every packet payload of a file, grouped by stream and kept in order.
///
/// The comparison this crate's remux tests are built on, and it lives in
/// `clipped-media-validation` rather than here because
/// `apps/recorder/tests/ipc_protocol.rs` makes the same one about the export
/// the desktop application asks for (issue #399). One harness answering "were
/// these the same coded bytes" is the point of that crate (AGENTS.md section
/// 55).
fn payloads_by_stream(file: &Path) -> Vec<Vec<String>> {
    Media::open(file)
        .expect("a file being compared for coded bytes opens")
        .packet_payloads_by_stream()
}

#[test]
fn a_recording_whose_tracks_do_not_start_together_keeps_the_gap_between_them() {
    let Some(_tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("muxer-remux-offset");

    // Audio half a second into the video. This is where a naive remux drifts:
    // a copy that rebased each track onto its own first packet would pull the
    // sound forward by exactly this much, and every structural assertion in
    // this file would still pass.
    let source = record(&directory, "offset.mkv", &["--audio-offset-ms", "500"]);
    let destination = directory.file("offset.mp4");

    remux_to_mp4(&source, &destination).expect("the recording remuxes");

    let recorded = Media::open(&source).expect("the recording opens");
    let remuxed = Media::open(&destination).expect("the MP4 opens");

    let recorded_starts = starts(&recorded);
    let remuxed_starts = starts(&remuxed);

    assert!(
        recorded_starts[1] - recorded_starts[0] > 0.4,
        "the fixture does not have the offset this test is about: {recorded_starts:?}"
    );

    for (stream, (expected, found)) in recorded_starts
        .iter()
        .zip(remuxed_starts.iter())
        .enumerate()
    {
        assert!(
            (expected - found).abs() < 0.002,
            "stream {stream} starts at {found:.3}s in the MP4 and at {expected:.3}s in the \
             recording"
        );
    }

    remuxed
        .validate()
        .video(VideoStream::codec("h264").decoded_frames((SECONDS * FRAME_RATE) as u64))
        .monotonic_timestamps()
        .assert_valid();
}

#[test]
fn a_stream_that_reorders_keeps_its_composition_offsets() {
    let Some(tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("muxer-remux-reordered");
    let source = directory.file("reordered.mkv");
    let destination = directory.file("reordered.mp4");

    // `libopenh264`, which everything else here is written with, does not
    // reorder, so a source with B-frames has to come from somewhere else. MPEG-4
    // part 2 with two B-frames per group produces exactly the shape that matters:
    // decode timestamps that are not presentation timestamps, and a first decode
    // timestamp before zero. MP4 stores the gap as a composition offset and the
    // negative start as an edit list, and a remux that rebased or resorted
    // either would put the picture out of order.
    build_fixture_with_ffmpeg(
        tools.ffmpeg(),
        &[
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=30",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "2",
            "-c:v",
            "mpeg4",
            "-bf",
            "2",
            "-c:a",
            "aac",
            &source.to_string_lossy(),
        ],
    );

    let recorded = Media::open(&source).expect("the fixture opens");
    let reordered = recorded
        .packets()
        .iter()
        .filter(|packet| packet.stream_index == 0)
        .filter(|packet| packet.decode_seconds < packet.presentation_seconds - 0.001)
        .count();
    assert!(
        reordered > 4,
        "the fixture holds {reordered} reordered packets, so this test would prove nothing \
         about composition offsets"
    );

    remux_to_mp4(&source, &destination).expect("the fixture remuxes");
    let remuxed = Media::open(&destination).expect("the MP4 opens");

    // Same pictures out of a decoder, and the same number of packets.
    let decoded = recorded.video_streams()[0]
        .number("nb_read_frames")
        .expect("the harness counted the fixture's frames");
    remuxed
        .validate()
        .video(
            VideoStream::codec("mpeg4")
                .decoded_frames(decoded as u64)
                .packets(
                    recorded.video_streams()[0]
                        .number("nb_read_packets")
                        .expect("the harness counted the fixture's packets")
                        as u64,
                ),
        )
        .monotonic_timestamps()
        .assert_valid();

    // And the same timeline, packet for packet: both timestamps of every packet
    // of every stream, in the order the container stores them.
    let recorded_times: Vec<(i64, f64, f64)> = recorded
        .packets()
        .iter()
        .map(|packet| {
            (
                packet.stream_index,
                packet.presentation_seconds,
                packet.decode_seconds,
            )
        })
        .collect();
    let remuxed_times: Vec<(i64, f64, f64)> = remuxed
        .packets()
        .iter()
        .map(|packet| {
            (
                packet.stream_index,
                packet.presentation_seconds,
                packet.decode_seconds,
            )
        })
        .collect();

    assert_eq!(
        remuxed_times.len(),
        recorded_times.len(),
        "the MP4 holds a different number of packets to the source"
    );
    for (position, (expected, found)) in recorded_times.iter().zip(&remuxed_times).enumerate() {
        assert_eq!(
            found.0, expected.0,
            "packet {position} of the MP4 belongs to a different stream"
        );
        assert!(
            (found.1 - expected.1).abs() < 0.002 && (found.2 - expected.2).abs() < 0.002,
            "packet {position} is presented at {:.6}s and decoded at {:.6}s in the MP4, and at \
             {:.6}s / {:.6}s in the source",
            found.1,
            found.2,
            expected.1,
            expected.2
        );
    }
}

#[test]
fn a_sound_track_mp4_cannot_carry_is_named_in_advance_and_refused_without_writing_anything() {
    let Some(tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("muxer-remux-refused");
    let source = directory.file("wavpack.mkv");
    let destination = directory.file("wavpack.mp4");

    // WavPack is a real codec Matroska stores happily and MP4 has no mapping
    // for: FFmpeg's MP4 muxer refuses it with "Could not find tag for codec
    // wavpack" — while writing the header, by which point the file exists.
    build_fixture_with_ffmpeg(
        tools.ffmpeg(),
        &[
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x120:rate=10",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=44100",
            "-t",
            "1",
            "-c:v",
            "libopenh264",
            "-c:a",
            "wavpack",
            &source.to_string_lossy(),
        ],
    );

    // Answered before anything is attempted, which is what lets a caller warn
    // somebody rather than report a failure afterwards.
    let plan = Mp4Plan::inspect(&source).expect("the fixture can be inspected");
    let blocking = plan.blocking();
    assert_eq!(blocking.len(), 1, "{plan}");
    assert_eq!(blocking[0].kind(), TrackKind::Audio);
    assert_eq!(blocking[0].codec(), "wavpack");
    assert_eq!(blocking[0].carriage(), Carriage::CodecUnsupported);
    assert!(!plan.is_lossless());
    assert_eq!(
        plan.tracks()[0].carriage(),
        Carriage::Copied,
        "the video track is carriable and the plan should say so"
    );

    let before = std::fs::read(&source).expect("the fixture can be read");
    let error = remux_to_mp4(&source, &destination).expect_err("the remux is refused");

    assert!(
        matches!(error, RemuxError::MediaNotCarried { .. }),
        "the wrong failure: {error}"
    );
    assert!(
        error.to_string().contains("wavpack"),
        "the refusal does not name the codec that caused it: {error}"
    );
    assert!(
        !destination.exists(),
        "a refused remux left a file behind at {}",
        destination.display()
    );
    assert!(
        std::fs::read(&source).expect("the fixture can be read again") == before,
        "a refused remux changed the recording"
    );
}

#[test]
fn an_mp4_that_already_exists_is_never_overwritten() {
    let Some(_tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("muxer-remux-exists");
    let source = record(&directory, "recording.mkv", &[]);
    let destination = directory.file("recording.mp4");

    // Whatever is already at that name may be a previous export somebody still
    // wants (AGENTS.md section 56). Truncating it is what every avio mode that
    // could create the file would do.
    std::fs::write(&destination, b"an export somebody already made")
        .expect("the destination can be created");
    let recording_before = std::fs::read(&source).expect("the recording can be read");

    let error = remux_to_mp4(&source, &destination).expect_err("an existing file is refused");
    assert!(
        matches!(error, RemuxError::Output { .. }),
        "the wrong failure: {error}"
    );

    assert_eq!(
        std::fs::read(&destination).expect("the destination can be read"),
        b"an export somebody already made",
        "the remux overwrote a file that was already there"
    );
    assert!(
        std::fs::read(&source).expect("the recording can be read again") == recording_before,
        "a refused remux changed the recording"
    );
}

#[test]
fn a_recording_that_cannot_be_read_is_reported_rather_than_half_copied() {
    let Some(_tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("muxer-remux-unreadable");
    let source = directory.file("missing.mkv");
    let destination = directory.file("missing.mp4");

    let error = remux_to_mp4(&source, &destination).expect_err("a missing recording is refused");
    assert!(
        matches!(error, RemuxError::SourceUnreadable { .. }),
        "the wrong failure: {error}"
    );
    assert!(
        !destination.exists(),
        "a remux of a recording that does not exist created an MP4"
    );
}

// The copy that carries one sound track, which is what a player is served
// (issue #304). The tones are the ones `examples/synthetic_recording.rs`
// produces, and they are the whole point of these two tests: a selection that
// took the wrong stream would still produce a one-audio-track MP4 of the right
// length with the right codec, and only what is *audible* on it says which
// track was taken.

/// The frequency each source's track carries, as AGENTS.md section 26 sets them.
const GAME: f64 = 440.0;
const OTHER_SYSTEM_AUDIO: f64 = 880.0;
const MICROPHONE: f64 = 1320.0;

#[test]
fn every_sound_track_can_be_chosen_and_the_copy_carries_the_one_that_was() {
    let Some(_tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("muxer-remux-one-track");
    // Video, then the compatibility mix, the game, other system audio and the
    // microphone: the model's order, which is the order the container declares
    // (`clipped_muxer::AudioSource`).
    let source = record(&directory, "recording.mkv", &["--audio-tracks", "4"]);
    let recorded = Media::open(&source).expect("a finished recording opens");
    let recording_before = std::fs::read(&source).expect("the recording can be read");

    for (stream, title, tone, other_tones) in [
        (2, "Game", GAME, [OTHER_SYSTEM_AUDIO, MICROPHONE]),
        (
            3,
            "Other System Audio",
            OTHER_SYSTEM_AUDIO,
            [GAME, MICROPHONE],
        ),
        (4, "Microphone", MICROPHONE, [GAME, OTHER_SYSTEM_AUDIO]),
    ] {
        let destination = directory.file(&format!("track-{stream}.mp4"));
        let summary = remux_to_mp4_carrying(&source, &destination, AudioTracks::Only(stream))
            .unwrap_or_else(|error| panic!("stream {stream} could not be carried: {error}"));

        // What was made, said by the plan rather than remembered by the caller:
        // one track copied, the other three named as tracks nobody asked for,
        // and the picture carried whichever sound was chosen.
        let carried: Vec<usize> = summary
            .plan()
            .tracks()
            .iter()
            .filter(|track| track.carriage() == Carriage::Copied)
            .map(clipped_muxer::PlannedTrack::index)
            .collect();
        assert_eq!(
            carried,
            vec![0, stream],
            "the plan for stream {stream} carried the wrong tracks"
        );
        assert!(
            !summary.plan().is_lossless(),
            "a copy holding one of four sound tracks claimed to hold everything"
        );

        let copy = Media::open(&destination).expect("the copy opens");
        copy.validate()
            .stream_count(2)
            .audio_stream_count(1)
            .video(
                VideoStream::codec("h264")
                    .decoded_frames((SECONDS * FRAME_RATE) as u64)
                    .resolution(640, 360),
            )
            .audio(
                0,
                AudioStream::codec("pcm_s16le")
                    .sample_rate(48_000)
                    .channels(2)
                    .packets((SECONDS * AUDIO_PACKETS_PER_SECOND) as u64),
            )
            // The assertion the feature exists for. Anything else on this track
            // — the mix, or the neighbouring source — fails here.
            .audio_tone(
                0,
                Tone::at(tone)
                    .isolated_from(other_tones[0])
                    .isolated_from(other_tones[1]),
            )
            .duration_seconds(
                recorded
                    .duration_seconds()
                    .expect("the recording declares a duration"),
                0.05,
            )
            .monotonic_timestamps()
            .assert_valid();

        // The name follows the track, which is what tells a selector what it is
        // offering. It moves as `TrackShape` describes above — Matroska's
        // `Name` becomes MP4's `udta`/`name`, which `ffprobe` reports as `name`
        // rather than as `title` — so it is read the way that comparison reads
        // it rather than through `AudioStream::title`, which asks for `title`.
        let carried = copy
            .audio_streams()
            .first()
            .and_then(|stream| stream.tag("title").or_else(|| stream.tag("name")))
            .map(str::to_owned);
        assert_eq!(
            carried.as_deref(),
            Some(title),
            "the copy of stream {stream} carries the wrong track's name"
        );
    }

    assert!(
        std::fs::read(&source).expect("the recording can be read again") == recording_before,
        "carrying one track changed the recording it was taken from"
    );
}

#[test]
fn a_sound_track_the_recording_has_not_got_is_refused_rather_than_copied_silent() {
    let Some(_tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("muxer-remux-no-such-track");
    let source = record(&directory, "recording.mkv", &["--audio-tracks", "2"]);

    // Stream 0 is the picture and stream 9 is past the end. Both would produce
    // a file with no sound in it, which is indistinguishable from a recording
    // that never had any — so both are refused before anything is created.
    for stream in [0, 9] {
        let destination = directory.file(&format!("silent-{stream}.mp4"));
        let error = remux_to_mp4_carrying(&source, &destination, AudioTracks::Only(stream))
            .expect_err("a stream that is not a sound track is refused");

        assert!(
            matches!(error, RemuxError::NoSuchAudioTrack { index, .. } if index == stream),
            "the wrong failure for stream {stream}: {error}"
        );
        assert!(
            !destination.exists(),
            "a refused copy of stream {stream} left an MP4 behind"
        );
    }
}
