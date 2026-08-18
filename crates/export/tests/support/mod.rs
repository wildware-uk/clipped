//! Fixtures for the export tests, and the questions `clipped-media-validation`
//! deliberately does not answer.
//!
//! Everything structural about a produced file — it opens, the streams are
//! there, the duration is plausible, the timestamps increase — is asserted with
//! `clipped-media-validation`, which is the workspace's one harness for that
//! (AGENTS.md section 22, `docs/testing.md`). Two things it has no opinion
//! about are needed here and are read out of `ffprobe` in this module instead:
//!
//! - **Which packets are keyframes, and when.** A test that cuts on a keyframe
//!   has to find one, and asking the encoder to put them somewhere is not the
//!   same as it having done so.
//! - **The payload of each packet, as a hash.** That is what proves a copy did
//!   not re-encode: the coded bytes of the source are the coded bytes of the
//!   export. Counting frames or comparing durations would pass just as happily
//!   on a file that had been through an encoder.
//!
//! `crates/muxer/tests/support/mod.rs` reads the second of those the same way
//! and for the same reason. It is a test module of another crate and cannot be
//! shared; the shared thing is the harness both of them assert the *structure*
//! with.

// Each test binary compiles this module separately and uses the part of it that
// it needs, so anything used by only one of them is "unused" in the others.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use clipped_media_validation::{MediaTools, TemporaryDirectory};

/// How many pictures a second the fixtures are.
pub(crate) const FRAME_RATE: u32 = 10;

/// How many pictures apart the fixtures' keyframes are asked to be.
pub(crate) const KEYFRAME_INTERVAL: u32 = 10;

/// Writes a recording into `directory` and returns its path.
///
/// H.264 through `libopenh264`, which is what the pinned LGPL build has and
/// which does not reorder — the copy path refuses a reordered stream, and a
/// fixture that was one would be testing the refusal rather than the copy.
/// Uncompressed audio, which is what Clipped itself records
/// (`clipped_muxer::RECORDING_AUDIO_CODEC`).
///
/// # Panics
///
/// When `ffmpeg` fails, with its own diagnostics, because a fixture that was
/// not built means the test after it is asserting on nothing.
pub(crate) fn recording(
    tools: &MediaTools,
    directory: &TemporaryDirectory,
    name: &str,
    seconds: u32,
) -> PathBuf {
    recording_with_sound(tools, directory, name, seconds, 1)
}

/// Writes a recording with `audio_streams` sound tracks and returns its path.
///
/// The fixture with the keyframe interval the rest of these tests use.
///
/// # Panics
///
/// When `ffmpeg` fails, with its own diagnostics.
pub(crate) fn recording_with_sound(
    tools: &MediaTools,
    directory: &TemporaryDirectory,
    name: &str,
    seconds: u32,
    audio_streams: u32,
) -> PathBuf {
    recording_with(
        tools,
        directory,
        name,
        seconds,
        audio_streams,
        KEYFRAME_INTERVAL,
    )
}

/// Writes a recording with `audio_streams` sound tracks and `keyframe_interval`
/// pictures between keyframes, and returns its path.
///
/// The same fixture as [`recording`], which is this with one sound track and the
/// usual interval. A size estimate has to be measured against files with more
/// sound than that and against files with none, because the tracks are where the
/// estimate has the most to add up; and against a keyframe interval longer than
/// the writer's cluster window, because that is what separates "one cluster per
/// keyframe" from "one cluster a second" — a fixture where they coincide cannot
/// tell a right model from a wrong one.
///
/// # Panics
///
/// When `ffmpeg` fails, with its own diagnostics.
pub(crate) fn recording_with(
    tools: &MediaTools,
    directory: &TemporaryDirectory,
    name: &str,
    seconds: u32,
    audio_streams: u32,
    keyframe_interval: u32,
) -> PathBuf {
    let path = directory.file(name);
    let mut arguments: Vec<String> = vec![
        // `-nostdin` because `ffmpeg` otherwise reads the console, and a test
        // harness's console is not something it should be reading.
        "-nostdin".to_owned(),
        "-v".to_owned(),
        "error".to_owned(),
        "-f".to_owned(),
        "lavfi".to_owned(),
        "-i".to_owned(),
        format!("testsrc2=size=320x240:rate={FRAME_RATE}"),
    ];

    for stream in 0..audio_streams {
        // A different tone per track, so two tracks are two different sets of
        // packets rather than the same bytes twice.
        let frequency = 440 + stream * 110;
        arguments.extend([
            "-f".to_owned(),
            "lavfi".to_owned(),
            "-i".to_owned(),
            format!("sine=frequency={frequency}:sample_rate=48000"),
        ]);
    }

    arguments.extend([
        "-t".to_owned(),
        seconds.to_string(),
        "-map".to_owned(),
        "0:v:0".to_owned(),
        "-c:v".to_owned(),
        "libopenh264".to_owned(),
        "-g".to_owned(),
        keyframe_interval.to_string(),
    ]);

    if audio_streams == 0 {
        arguments.push("-an".to_owned());
    } else {
        for stream in 0..audio_streams {
            arguments.extend(["-map".to_owned(), format!("{}:a:0", stream + 1)]);
        }
        arguments.extend(["-c:a".to_owned(), "pcm_s16le".to_owned()]);
        for stream in 0..audio_streams {
            arguments.extend([
                format!("-metadata:s:a:{stream}"),
                format!("title={}", track_name(stream)),
            ]);
        }
    }

    arguments.push(path.to_string_lossy().into_owned());

    let output = Command::new(tools.ffmpeg())
        .args(&arguments)
        .output()
        .expect("the pinned ffmpeg can be run");

    assert!(
        output.status.success(),
        "the fixture could not be built: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    path
}

/// What a fixture's *n*th sound track is called.
///
/// The names a recording really carries, so that a test reading them back is
/// reading something a recording would have.
pub(crate) fn track_name(stream: u32) -> String {
    match stream {
        0 => "Game".to_owned(),
        1 => "Microphone".to_owned(),
        other => format!("Track {other}"),
    }
}

/// One packet of a file, as `ffprobe` reports it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProbedPacket {
    /// Which container stream it belongs to.
    pub(crate) stream: usize,
    /// When it is presented, in seconds.
    pub(crate) presentation_seconds: f64,
    /// Whether a decoder can start here.
    pub(crate) keyframe: bool,
    /// An MD5 of the coded payload, as `ffprobe` computes it.
    pub(crate) hash: String,
}

/// Every packet of a file, in the order they are stored.
///
/// # Panics
///
/// When `ffprobe` fails or the file holds no packets, because a comparison
/// against an empty list would prove nothing.
pub(crate) fn packets(ffprobe: &Path, file: &Path) -> Vec<ProbedPacket> {
    let output = Command::new(ffprobe)
        .args([
            "-hide_banner",
            "-v",
            "error",
            "-show_packets",
            "-show_data_hash",
            "MD5",
            "-show_entries",
            "packet=stream_index,pts_time,flags,data_hash",
            "-of",
            "compact=p=0:nk=0",
        ])
        .arg(file)
        .output()
        .expect("the pinned ffprobe can be run");

    assert!(
        output.status.success(),
        "ffprobe could not read {}: {}",
        file.display(),
        String::from_utf8_lossy(&output.stderr)
    );

    let text = String::from_utf8_lossy(&output.stdout);
    let mut packets = Vec::new();
    for line in text.lines() {
        let mut stream = None;
        let mut presentation = None;
        let mut flags = None;
        let mut hash = None;
        for field in line.trim().split('|') {
            match field.split_once('=') {
                Some(("stream_index", value)) => stream = value.parse().ok(),
                Some(("pts_time", value)) => presentation = value.parse().ok(),
                Some(("flags", value)) => flags = Some(value.to_owned()),
                Some(("data_hash", value)) => hash = Some(value.to_owned()),
                _ => {}
            }
        }
        if let (Some(stream), Some(presentation), Some(flags), Some(hash)) =
            (stream, presentation, flags, hash)
        {
            packets.push(ProbedPacket {
                stream,
                presentation_seconds: presentation,
                keyframe: flags.starts_with('K'),
                hash,
            });
        }
    }

    assert!(
        !packets.is_empty(),
        "no packets came back for {}, so a comparison against them would prove nothing",
        file.display()
    );
    packets
}

/// The packets of one stream, in the order they are stored.
pub(crate) fn packets_of(ffprobe: &Path, file: &Path, stream: usize) -> Vec<ProbedPacket> {
    packets(ffprobe, file)
        .into_iter()
        .filter(|packet| packet.stream == stream)
        .collect()
}

/// When the keyframes of a stream are, in seconds.
pub(crate) fn keyframes(ffprobe: &Path, file: &Path, stream: usize) -> Vec<f64> {
    packets_of(ffprobe, file, stream)
        .into_iter()
        .filter(|packet| packet.keyframe)
        .map(|packet| packet.presentation_seconds)
        .collect()
}

/// A file's contents, as a length and a hash.
///
/// What "the recording was not modified" is asserted with. A length alone would
/// miss a rewrite in place; a hash alone reads oddly in a failure message.
///
/// # Panics
///
/// When the file cannot be read, which for a fixture the test just wrote is a
/// failure of the test rather than of the code under it.
pub(crate) fn contents(path: &Path) -> (u64, u64) {
    let bytes = std::fs::read(path)
        .unwrap_or_else(|error| panic!("{} can be read: {error}", path.display()));

    // FNV-1a, written out rather than taken as a dependency: this is comparing
    // a file with itself a few seconds later, not defending against anything.
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in &bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (bytes.len() as u64, hash)
}

/// Nanoseconds as a source time, for building a document.
pub(crate) fn seconds_to_nanos(seconds: f64) -> u64 {
    (seconds * 1_000_000_000.0).round() as u64
}
