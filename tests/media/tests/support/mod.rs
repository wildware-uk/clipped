//! Fixtures for the harness's own tests: media that is known to be good, and
//! media that is known to be broken.
//!
//! # Why FFmpeg builds the good ones
//!
//! The harness is what every media-producing crate in this workspace checks its
//! output with, so its fixtures must not come from any of them. A validator
//! whose only test subject is the muxer it validates cannot tell "the file is
//! right" from "the file is wrong in the same way the checker is". The pinned
//! FFmpeg build writes these instead — a completely independent implementation
//! of Matroska — and `crates/muxer/tests/synthetic_recording.rs` is where the
//! harness meets Clipped's own writer.
//!
//! # Why the broken ones are made by damaging a good one
//!
//! Because that is how recordings break. A recorder killed by a power cut
//! leaves a file that stops mid-element; a clock that steps backwards leaves a
//! cluster timestamped before the one in front of it. Both are produced here by
//! taking a real recording apart at the byte level, so what the harness is
//! tested against is a real file with a real defect rather than a mock.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use clipped_media_validation::MediaTools;

/// The size and rate every fixture's video is written at. Small and short: what
/// is being tested is the checking, not the encoding.
pub(crate) const WIDTH: u32 = 320;
pub(crate) const HEIGHT: u32 = 180;
pub(crate) const FRAME_RATE: u32 = 30;

/// One audio track of a fixture: the name it carries, and the lavfi graph that
/// generates what is on it.
pub(crate) struct AudioSource {
    /// The Matroska track name.
    pub(crate) title: &'static str,
    /// A lavfi filter graph producing exactly one audio output.
    pub(crate) graph: String,
    /// How far into the recording this track starts, in seconds.
    pub(crate) offset: f64,
}

impl AudioSource {
    /// A pure tone, which is what the controlled audio test applications
    /// generate (AGENTS.md section 26).
    pub(crate) fn tone(title: &'static str, frequency: f64) -> Self {
        Self {
            title,
            graph: format!("sine=frequency={frequency}:sample_rate=48000"),
            offset: 0.0,
        }
    }

    /// A track whose first packet is `seconds` after the video's, which is a
    /// recording that is out of sync from its first frame.
    pub(crate) fn starting_at(mut self, seconds: f64) -> Self {
        self.offset = seconds;
        self
    }

    /// A track that stops `seconds` into a longer recording, which is a
    /// recording that started in sync and came apart while it ran.
    ///
    /// Trimming the generated audio rather than shortening the whole output:
    /// what is wanted is a file whose tracks *begin* together and end far
    /// apart, so that only the end-of-recording half of the synchronisation
    /// check can catch it.
    pub(crate) fn ending_at(mut self, seconds: f64) -> Self {
        self.graph = format!("{},atrim=end={seconds}", self.graph);
        self
    }

    /// Two tones on one track: a source that leaked into another's track, which
    /// is the failure multi-track audio exists to prevent.
    pub(crate) fn mixed(title: &'static str, first: f64, second: f64) -> Self {
        Self {
            title,
            graph: format!(
                "sine=frequency={first}:sample_rate=48000[first];\
                 sine=frequency={second}:sample_rate=48000[second];\
                 [first][second]amix=inputs=2:normalize=0"
            ),
            offset: 0.0,
        }
    }

    /// A track with nothing on it.
    pub(crate) fn silence(title: &'static str) -> Self {
        Self {
            title,
            graph: "anullsrc=sample_rate=48000:channel_layout=stereo".to_owned(),
            offset: 0.0,
        }
    }
}

/// Writes a Matroska recording with one video track and the audio tracks given.
///
/// # Panics
///
/// When FFmpeg refuses to write the file, because a fixture that did not build
/// is a test that proves nothing.
pub(crate) fn write_recording(
    tools: &MediaTools,
    path: &Path,
    seconds: f64,
    audio: &[AudioSource],
) -> PathBuf {
    let mut command = Command::new(tools.ffmpeg());
    command.args(["-hide_banner", "-v", "error", "-nostdin"]);
    command.args([
        "-f",
        "lavfi",
        "-i",
        &format!("testsrc=size={WIDTH}x{HEIGHT}:rate={FRAME_RATE}"),
    ]);
    for source in audio {
        if source.offset != 0.0 {
            // Applied to the input, so the track's own timestamps move rather
            // than its content being padded with silence: what is wanted is a
            // track that *starts late*, not one that begins with nothing on it.
            command.args(["-itsoffset", &source.offset.to_string()]);
        }
        command.args(["-f", "lavfi", "-i", &source.graph]);
    }

    command.args(["-map", "0:v"]);
    for index in 0..audio.len() {
        command.args(["-map", &format!("{}:a", index + 1)]);
    }
    command.args([
        "-c:v",
        "libopenh264",
        "-c:a",
        "pcm_s16le",
        "-ac",
        "2",
        "-t",
        &seconds.to_string(),
        // Half a second of media per cluster, which is roughly what
        // `crates/muxer` writes and nothing like FFmpeg's own default of five
        // seconds. Two reasons: a fixture that fits in one cluster cannot be
        // given a second cluster to damage, and a recording whose clusters are
        // seconds long is not the shape of recording Clipped produces
        // (docs/muxing.md).
        "-cluster_time_limit",
        "500",
        "-metadata:s:v:0",
        "title=Gameplay",
    ]);
    for (index, source) in audio.iter().enumerate() {
        command.args([
            &format!("-metadata:s:a:{index}"),
            &format!("title={}", source.title),
        ]);
    }
    command.arg(path);

    let output = command.output().expect("the pinned ffmpeg can be run");
    assert!(
        output.status.success(),
        "the fixture could not be written: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    path.to_path_buf()
}

/// Writes a bare H.264 elementary stream: video with no container around it.
///
/// Not a recording, and that is the point. It is what an encoder emits before
/// anything muxes it, it is what `crates/encoder`'s hardware tests are pointed
/// at today (issue #154), and because there is no container there is no
/// timeline: `ffprobe` reports the stream with no `start_time` and no duration.
/// A harness that treats a field the file does not report as 0.000s would call
/// this file synchronised.
///
/// # Panics
///
/// When FFmpeg refuses to write it.
pub(crate) fn write_elementary_stream(tools: &MediaTools, path: &Path, seconds: f64) -> PathBuf {
    let output = Command::new(tools.ffmpeg())
        .args(["-hide_banner", "-v", "error", "-nostdin"])
        .args([
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc=size={WIDTH}x{HEIGHT}:rate={FRAME_RATE}"),
        ])
        .args([
            "-c:v",
            "libopenh264",
            "-t",
            &seconds.to_string(),
            "-f",
            "h264",
        ])
        .arg(path)
        .output()
        .expect("the pinned ffmpeg can be run");
    assert!(
        output.status.success(),
        "the fixture could not be written: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    path.to_path_buf()
}

/// Copies the first `fraction` of a recording, and nothing else.
///
/// What a recorder killed by a power cut leaves behind: the bytes that reached
/// the disk, ending wherever they ended.
pub(crate) fn truncated(source: &Path, destination: &Path, fraction: f64) -> PathBuf {
    let bytes = std::fs::read(source).expect("the fixture can be read");
    let keep = (bytes.len() as f64 * fraction) as usize;
    std::fs::write(destination, &bytes[..keep]).expect("the truncated copy can be written");
    destination.to_path_buf()
}

/// Copies a recording with the second cluster's timestamp moved back to the
/// beginning of the file.
///
/// A Matroska cluster carries one absolute timestamp and every block in it is
/// relative to that, so rewinding it puts a whole cluster of packets *before*
/// the cluster in front of them — which is exactly what a recorder whose clock
/// stepped backwards writes, and what the muxer's own monotonic correction
/// exists to prevent (`crates/muxer/src/timeline.rs`).
///
/// Only the value bytes are overwritten, so every element keeps its length and
/// the file stays structurally a valid Matroska stream. The defect is in the
/// timeline and nowhere else, which is the point: a corruption that also broke
/// the container would be caught by the container opening at all, and would
/// prove nothing about the timestamp check.
///
/// Returns [`None`] when the recording has fewer than two clusters, so a caller
/// can say so rather than assert on a file it never managed to damage.
pub(crate) fn second_cluster_rewound(source: &Path, destination: &Path) -> Option<PathBuf> {
    let mut bytes = std::fs::read(source).expect("the fixture can be read");
    let second = clusters(&bytes).into_iter().nth(1)?;

    // The Timestamp element (ID 0xE7) is the first child of every cluster.
    let timestamp = element(&bytes, second, 0xE7)?;
    for byte in &mut bytes[timestamp.clone()] {
        *byte = 0;
    }

    std::fs::write(destination, &bytes).expect("the damaged copy can be written");
    Some(destination.to_path_buf())
}

/// Where each top-level cluster's *content* starts and ends.
///
/// Found by scanning for the Cluster ID and keeping only the matches whose
/// declared size lands exactly on another cluster or on the end of the file, so
/// that four bytes of video data that happen to read as a cluster header are
/// not mistaken for one.
fn clusters(bytes: &[u8]) -> Vec<std::ops::Range<usize>> {
    const CLUSTER_ID: [u8; 4] = [0x1F, 0x43, 0xB6, 0x75];

    let mut found = Vec::new();
    let mut index = 0;
    while index + 4 < bytes.len() {
        if bytes[index..index + 4] != CLUSTER_ID {
            index += 1;
            continue;
        }
        let Some((size, header)) = variable_length_integer(&bytes[index + 4..]) else {
            index += 1;
            continue;
        };
        let start = index + 4 + header;
        let Some(end) = start.checked_add(size).filter(|end| *end <= bytes.len()) else {
            index += 1;
            continue;
        };
        found.push(start..end);
        index = end;
    }
    found
}

/// Where the value of the first `id` element inside `within` lives.
fn element(bytes: &[u8], within: std::ops::Range<usize>, id: u8) -> Option<std::ops::Range<usize>> {
    let mut index = within.start;
    while index < within.end {
        if bytes[index] != id {
            index += 1;
            continue;
        }
        let (size, header) = variable_length_integer(&bytes[index + 1..])?;
        let start = index + 1 + header;
        return Some(start..start + size);
    }
    None
}

/// Reads an EBML variable-length integer, returning its value and how many
/// bytes it occupied.
fn variable_length_integer(bytes: &[u8]) -> Option<(usize, usize)> {
    let first = *bytes.first()?;
    let length = first.leading_zeros() as usize + 1;
    if first == 0 || length > 8 || bytes.len() < length {
        return None;
    }

    // The marker bit is part of the length prefix rather than of the value.
    let mut value = usize::from(first & (0xFF >> length));
    for byte in &bytes[1..length] {
        value = (value << 8) | usize::from(*byte);
    }
    Some((value, length))
}
