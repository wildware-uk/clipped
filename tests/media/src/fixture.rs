//! Real coded video to build media out of.
//!
//! The rest of this crate answers "is what you produced valid media". This
//! answers the question that comes before it: **what do you produce it from**.
//!
//! A test that pushes synthetic bytes through a buffer, a muxer or a remuxer can
//! prove the *selection* and proves nothing about the file, because a clip whose
//! every packet fails to decode still has one video stream, the right
//! resolution, a plausible duration and monotonic timestamps.
//! [`Validation::video`](crate::Validation::video)'s `decoded_frames` is the
//! assertion that separates the two, and it needs a real H.264 elementary stream
//! to have anything to decode.
//!
//! # Why the FFmpeg programs rather than an encoder
//!
//! `clipped-encoder`'s software backend encodes on the CPU, but it takes a
//! Direct3D 11 texture and a graphics device, so a test using it opens the GPU —
//! which is what the shared-machine test discipline forbids for a test that runs
//! by default. This crate deliberately sits *below* `clipped-muxer`, which owns
//! the workspace's FFmpeg linkage, so it cannot reach FFmpeg as a library
//! either (see the crate documentation).
//!
//! What is left is the pair of programs the harness already locates and that
//! every machine building this workspace has: `ffmpeg` encodes a test pattern to
//! an Annex B elementary stream, and `ffprobe` says where each access unit
//! begins and ends in it. That is a *demuxer* deciding the packet boundaries
//! rather than this file guessing at NAL parsing, and it costs nothing to trust:
//! if it were wrong, a file written from those packets would not decode, and the
//! test would fail.
//!
//! # What it deliberately does not know
//!
//! Any type from `clipped-encoder` or `clipped-muxer`. It hands back bytes,
//! offsets and a picture kind, and each caller maps those onto its own crate's
//! packet type — which is what lets this crate stay below both of them in the
//! layering (`tests/integration/tests/workspace_layering.rs`).
//!
//! # Cost
//!
//! One `ffmpeg` invocation per fixture, of `seconds` of small video. Callers
//! that need it more than once should build it once per test binary and share
//! it: `crates/replay/tests/support/mod.rs` holds one in a `OnceLock` for
//! exactly that reason.

use std::path::Path;
use std::process::Command;

use crate::temporary::TemporaryDirectory;
use crate::tools::require_media_tools;

/// One access unit: the bytes of one coded picture, and whether a decoder can
/// start at it.
#[derive(Debug, Clone, Copy)]
pub struct AccessUnit {
    offset: usize,
    length: usize,
    keyframe: bool,
}

impl AccessUnit {
    /// Whether a stream can be cut immediately before this picture.
    #[must_use]
    pub const fn is_keyframe(&self) -> bool {
        self.keyframe
    }
}

/// A test pattern encoded to H.264, taken apart into access units.
///
/// Held entirely in memory: the encoded file is deleted once it has been read,
/// so a fixture kept in a `static` — which is never dropped — does not leave
/// megabytes on the disk after every run.
#[derive(Debug)]
pub struct CodedVideo {
    stream: Vec<u8>,
    units: Vec<AccessUnit>,
    parameter_sets: Vec<u8>,
    width: u32,
    height: u32,
    frames_per_second: u32,
    keyframe_interval: u32,
}

impl CodedVideo {
    /// Encodes `seconds` of a moving test pattern, or answers [`None`] when the
    /// FFmpeg programs are not on this machine.
    ///
    /// A missing FFmpeg is a clean skip, exactly as it is everywhere else that
    /// validates media — unless `CLIPPED_REQUIRE_MEDIA` is set, which
    /// [`require_media_tools`](crate::require_media_tools) turns into a failure
    /// so that a machine which is supposed to validate media cannot quietly stop
    /// doing it.
    ///
    /// # Panics
    ///
    /// If `ffmpeg` or `ffprobe` could be found and then failed, or produced
    /// something other than the frames it was asked for. That is a broken
    /// toolchain rather than a failed expectation, and a fixture that quietly
    /// returned fewer frames would weaken every assertion made against it.
    #[must_use]
    pub fn encode(
        width: u32,
        height: u32,
        frames_per_second: u32,
        keyframe_interval: u32,
        seconds: u32,
    ) -> Option<Self> {
        let tools = require_media_tools()?;
        let directory = TemporaryDirectory::new("coded-video");
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
                    "testsrc2=size={width}x{height}:rate={frames_per_second}:duration={seconds}"
                ),
            ])
            .args(["-c:v", "libopenh264", "-b:v", "2000000"])
            .args(["-g", &keyframe_interval.to_string()])
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
            u64::from(seconds) * u64::from(frames_per_second),
            "the fixture does not hold the frames it was asked for"
        );

        let opening = units[0];
        let parameter_sets =
            parameter_sets(&stream[opening.offset..opening.offset + opening.length]);
        assert!(
            !parameter_sets.is_empty(),
            "no sequence or picture parameter set was found in the first access unit"
        );

        // Everything a caller needs is in memory now, so the encoded file goes
        // here rather than being carried around by the fixture.
        drop(directory);

        Some(Self {
            stream,
            units,
            parameter_sets,
            width,
            height,
            frames_per_second,
            keyframe_interval,
        })
    }

    /// How many coded pictures there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.units.len()
    }

    /// Whether it holds no pictures. Never true of an encoded fixture.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.units.is_empty()
    }

    /// The bytes of picture `index`, wrapping round the fixture.
    ///
    /// The fixture is finite and a caller that pushes for a while goes round it
    /// more than once: what matters is that the pictures are real and that a
    /// keyframe arrives every [`keyframe_interval`](Self::keyframe_interval).
    #[must_use]
    pub fn picture(&self, index: u64) -> &[u8] {
        let unit = self.unit(index);
        &self.stream[unit.offset..unit.offset + unit.length]
    }

    /// Whether picture `index` is one a decoder can start at.
    #[must_use]
    pub fn is_keyframe(&self, index: u64) -> bool {
        self.unit(index).keyframe
    }

    /// The sequence and picture parameter sets, in the Annex B form a
    /// container's out-of-band header is written in.
    ///
    /// The same form a Windows hardware encoder hands `clipped-session`, so a
    /// caller writing them into a track is exercising the path a recording uses.
    #[must_use]
    pub fn parameter_sets(&self) -> &[u8] {
        &self.parameter_sets
    }

    /// The picture's width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// The picture's height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Frames a second, and therefore the spacing of the timestamps a caller
    /// should stamp them with.
    #[must_use]
    pub const fn frames_per_second(&self) -> u32 {
        self.frames_per_second
    }

    /// How many frames apart the keyframes are.
    #[must_use]
    pub const fn keyframe_interval(&self) -> u32 {
        self.keyframe_interval
    }

    fn unit(&self, index: u64) -> AccessUnit {
        self.units[usize::try_from(index).unwrap_or(0) % self.units.len()]
    }
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

/// The sequence and picture parameter sets of an access unit.
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
