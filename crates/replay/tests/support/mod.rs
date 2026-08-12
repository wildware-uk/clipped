//! Real coded video for the tests that save a clip and then decode it.
//!
//! A replay buffer holds whatever the encoder gave it and hands the same bytes
//! to the writer, so a test built on synthetic packets can prove the *selection*
//! and proves nothing at all about the file: a clip whose every packet fails to
//! decode still has one video stream, the right resolution and a plausible
//! duration. `decoded_frames_at_least` is the assertion that separates the two,
//! and it needs a real H.264 elementary stream to have anything to decode.
//!
//! # Why the FFmpeg programs rather than an encoder
//!
//! `clipped-encoder`'s software backend encodes on the CPU, but it takes a
//! Direct3D 11 texture and a graphics device, so a test using it opens the GPU —
//! which is exactly what the shared-machine test discipline forbids for a test
//! that runs by default. `clipped-muxer` owns the workspace's FFmpeg *linkage*
//! and this crate deliberately does not reach through it for a second one
//! (`docs/adr/0004-ffmpeg-dependency-strategy.md`).
//!
//! What is left is the pair of programs the media harness already locates and
//! that every machine building this workspace has: `ffmpeg` encodes a test
//! pattern to an Annex B elementary stream, and `ffprobe` says where each access
//! unit begins and ends in it. That is a *demuxer* deciding the packet
//! boundaries rather than this file guessing at NAL parsing, and it costs
//! nothing to trust: if it were wrong, the clip written from those packets would
//! not decode, and the test would fail.
//!
//! The fixture is produced once per test binary and shared, because encoding
//! seventy seconds of video for each of three tests would be the slowest thing
//! in the crate.

// Each test binary compiles this module separately and uses the part of it that
// it needs, so anything used by only one of them is "unused" in the others.
#![allow(dead_code)]

use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use clipped_encoder::{EncodedPacket, PictureKind};
use clipped_media_validation::{require_media_tools, TemporaryDirectory};
use clipped_muxer::{FrameRate, VideoCodec, VideoTrack};

/// The picture size the fixture is encoded at.
///
/// Small: the tests care about how many frames come out and whether they
/// decode, not about how big they are, and seventy seconds of 1080p would make
/// this the slowest test in the workspace for nothing.
pub(crate) const WIDTH: u32 = 640;
pub(crate) const HEIGHT: u32 = 360;

/// Frames a second, and therefore the spacing of the timestamps the buffer is
/// fed.
pub(crate) const FRAMES_PER_SECOND: u64 = 60;

/// How many frames apart the keyframes are: two seconds, which is
/// `clipped_encoder::KeyframeInterval::DEFAULT` and `DEFAULT_SEGMENT`.
pub(crate) const KEYFRAME_INTERVAL: u64 = 120;

/// How much video the fixture holds.
///
/// Longer than the sixty seconds the first acceptance criterion asks for, so
/// that a buffer of a sixty-second window is measured while it is evicting
/// rather than while it is still filling.
pub(crate) const FIXTURE_SECONDS: u64 = 70;

/// When frame `index` is presented, in media time.
///
/// Integer nanoseconds from the frame number rather than a clock, so the tests
/// behave identically however fast the machine runs them (AGENTS.md section 25).
pub(crate) fn presentation_time(index: u64) -> Duration {
    Duration::from_nanos(index * 1_000_000_000 / FRAMES_PER_SECOND)
}

/// How long one frame occupies.
pub(crate) fn frame_interval() -> Duration {
    Duration::from_nanos(1_000_000_000 / FRAMES_PER_SECOND)
}

/// One access unit: the bytes of one coded picture, and whether a decoder can
/// start at it.
#[derive(Debug)]
pub(crate) struct AccessUnit {
    offset: usize,
    length: usize,
    keyframe: bool,
}

/// A test pattern encoded to H.264, taken apart into access units.
///
/// Held entirely in memory, and deliberately: the fixture lives in a `static`
/// for the life of the test binary, statics are never dropped, and a
/// `TemporaryDirectory` kept in one would therefore leave its fifteen megabytes
/// on the disk after every run.
#[derive(Debug)]
pub(crate) struct CodedVideo {
    stream: Vec<u8>,
    units: Vec<AccessUnit>,
    codec_private: Vec<u8>,
}

impl CodedVideo {
    /// How many coded pictures there are.
    pub(crate) fn len(&self) -> usize {
        self.units.len()
    }

    /// The packet for frame `index` of the stream, timed at `at`.
    ///
    /// The fixture is finite and the tests that push for a while go round it
    /// more than once, so the index wraps while the timestamp does not: what
    /// matters to the buffer is that the pictures are real and that a keyframe
    /// arrives every two seconds.
    pub(crate) fn packet(&self, index: u64, at: Duration) -> EncodedPacket<'_> {
        let unit = &self.units[usize::try_from(index).unwrap_or(0) % self.units.len()];

        EncodedPacket::new(
            &self.stream[unit.offset..unit.offset + unit.length],
            at,
            at,
            if unit.keyframe {
                PictureKind::Keyframe
            } else {
                PictureKind::Predicted
            },
        )
    }

    /// The track description a clip of this video needs.
    ///
    /// The sequence and picture parameter sets are the container's mandatory
    /// out-of-band header; without them the file lists a video stream that
    /// nothing can decode.
    pub(crate) fn track(&self) -> VideoTrack {
        VideoTrack::new(VideoCodec::H264, WIDTH, HEIGHT)
            .with_frame_rate(
                FrameRate::per_second(u32::try_from(FRAMES_PER_SECOND).expect("60 fits"))
                    .expect("a real rate"),
            )
            .with_codec_private(self.codec_private.clone())
            .with_name("Gameplay")
    }
}

/// The shared fixture, or [`None`] when the FFmpeg programs are not on this
/// machine.
///
/// A missing FFmpeg is a clean skip, exactly as it is everywhere else that
/// validates media — unless `CLIPPED_REQUIRE_MEDIA` is set, which
/// `require_media_tools` turns into a failure so that a machine which is
/// supposed to validate media cannot quietly stop doing it.
pub(crate) fn coded_video() -> Option<&'static CodedVideo> {
    static FIXTURE: OnceLock<Option<CodedVideo>> = OnceLock::new();

    FIXTURE.get_or_init(encode_fixture).as_ref()
}

/// Encodes the pattern and takes it apart, once.
fn encode_fixture() -> Option<CodedVideo> {
    let tools = require_media_tools()?;
    let directory = TemporaryDirectory::new("replay-fixture");
    let path = directory.file("pattern.h264");

    // `testsrc2` moves, so consecutive frames really differ and the encoder
    // produces predicted pictures rather than a stream of near-empty ones.
    // `aud=insert` puts an access unit delimiter in front of every picture,
    // which is what lets ffprobe's raw H.264 demuxer report exact packet
    // boundaries below.
    let encoded = Command::new(tools.ffmpeg())
        .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y"])
        .args([
            "-f",
            "lavfi",
            "-i",
            &format!(
                "testsrc2=size={WIDTH}x{HEIGHT}:rate={FRAMES_PER_SECOND}:duration={FIXTURE_SECONDS}"
            ),
        ])
        .args(["-c:v", "libopenh264", "-b:v", "2000000"])
        .args(["-g", &KEYFRAME_INTERVAL.to_string()])
        .args(["-bsf:v", "h264_metadata=aud=insert", "-f", "h264"])
        .arg(&path)
        .output()
        .expect("ffmpeg can be started");
    assert!(
        encoded.status.success(),
        "the fixture could not be encoded: {}",
        String::from_utf8_lossy(&encoded.stderr)
    );

    let stream = std::fs::read(&path).expect("the encoded fixture can be read");
    let units = access_units(tools.ffprobe(), &path);
    assert_eq!(
        units.len() as u64,
        FIXTURE_SECONDS * FRAMES_PER_SECOND,
        "the fixture does not hold the frames it was asked for"
    );

    let opening = &units[0];
    let codec_private = parameter_sets(&stream[opening.offset..opening.offset + opening.length]);
    assert!(
        !codec_private.is_empty(),
        "no sequence or picture parameter set was found in the first access unit"
    );

    // Everything the tests need is in memory now, so the encoded file goes here
    // rather than being carried around by the fixture: see `CodedVideo`.
    drop(directory);

    Some(CodedVideo {
        stream,
        units,
        codec_private,
    })
}

/// Where each access unit begins and ends, according to `ffprobe`.
///
/// Asked for as `key=value` pairs rather than positionally, so that this does
/// not depend on the order `ffprobe` happens to print its fields in.
fn access_units(ffprobe: &Path, path: &Path) -> Vec<AccessUnit> {
    let probed = Command::new(ffprobe)
        .args(["-hide_banner", "-loglevel", "error"])
        .args(["-select_streams", "v:0", "-show_packets"])
        .args(["-show_entries", "packet=size,pos,flags"])
        .args(["-of", "compact=p=0"])
        .arg(path)
        .output()
        .expect("ffprobe can be started");
    assert!(
        probed.status.success(),
        "the fixture could not be read back: {}",
        String::from_utf8_lossy(&probed.stderr)
    );

    String::from_utf8_lossy(&probed.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let field = |name: &str| {
                line.split('|')
                    .filter_map(|pair| pair.split_once('='))
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| value.to_owned())
                    .unwrap_or_else(|| panic!("ffprobe reported no {name} for a packet: {line}"))
            };
            let parse = |name: &str| {
                field(name)
                    .parse::<usize>()
                    .unwrap_or_else(|_| panic!("ffprobe reported a {name} that is not a number"))
            };

            AccessUnit {
                offset: parse("pos"),
                length: parse("size"),
                // `K` in the first position is `AV_PKT_FLAG_KEY`.
                keyframe: field("flags").starts_with('K'),
            }
        })
        .collect()
}

/// The sequence and picture parameter sets of an access unit, in the Annex B
/// form the container's out-of-band header is written in.
///
/// The same form a Windows hardware encoder hands `clipped-session`, so the
/// path under test is the one a recording uses (`crates/muxer/src/packet.rs`).
fn parameter_sets(unit: &[u8]) -> Vec<u8> {
    const SEQUENCE_PARAMETER_SET: u8 = 7;
    const PICTURE_PARAMETER_SET: u8 = 8;

    let mut header = Vec::new();
    for (kind, payload) in nal_units(unit) {
        if kind == SEQUENCE_PARAMETER_SET || kind == PICTURE_PARAMETER_SET {
            header.extend_from_slice(&[0, 0, 0, 1]);
            header.extend_from_slice(payload);
        }
    }
    header
}

/// Every NAL unit in `bytes`, as its type and its payload without the start
/// code.
fn nal_units(bytes: &[u8]) -> Vec<(u8, &[u8])> {
    let mut starts = Vec::new();
    let mut index = 0;
    while index + 2 < bytes.len() {
        if bytes[index] == 0 && bytes[index + 1] == 0 && bytes[index + 2] == 1 {
            starts.push(index + 3);
            index += 3;
        } else {
            index += 1;
        }
    }

    starts
        .iter()
        .enumerate()
        .filter_map(|(position, start)| {
            let mut end = starts
                .get(position + 1)
                .map_or(bytes.len(), |next| next - 3);
            // A four-byte start code is the three-byte one with a zero in front
            // of it, and that zero belongs to neither unit. A NAL never ends in
            // a zero byte, so trimming them is unambiguous.
            while end > *start && bytes[end - 1] == 0 {
                end -= 1;
            }
            (end > *start).then(|| (bytes[*start] & 0x1f, &bytes[*start..end]))
        })
        .collect()
}
