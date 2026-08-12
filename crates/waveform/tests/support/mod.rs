//! Audio whose waveform is known before it is measured.
//!
//! The point of these tests is to check a measurement, so the subject has to be
//! something whose answer is known independently of the code under test. A tone
//! at a stated amplitude for a stated number of seconds is exactly that: a
//! quarter-scale second must produce peaks at a quarter of full scale and a
//! silent second must produce none, and no amount of plausible-looking output
//! from the analyser can be mistaken for that.
//!
//! The files are written here rather than by `ffmpeg.exe` for the single-track
//! cases, so that those tests run on any checkout that can build the workspace —
//! a WAV header is 44 bytes and PCM is the samples. Multi-track containers do
//! need a muxer, and use the pinned build through `clipped-media-validation`.

// Each test binary compiles this module separately and uses the part of it that
// it needs, so anything used by only one of them is "unused" in the others.
#![allow(dead_code)]

use std::path::Path;
use std::process::Command;

use clipped_media_validation::MediaTools;

/// A stretch of one channel: a sine at a known amplitude, or silence.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Tone {
    /// How long it lasts.
    pub seconds: f64,
    /// Its peak amplitude, as a fraction of full scale. Zero is silence.
    pub amplitude: f32,
    /// Its frequency in hertz. Ignored when silent.
    pub hertz: f64,
}

impl Tone {
    pub(crate) fn silence(seconds: f64) -> Self {
        Self {
            seconds,
            amplitude: 0.0,
            hertz: 0.0,
        }
    }

    pub(crate) fn at(seconds: f64, amplitude: f32) -> Self {
        Self {
            seconds,
            amplitude,
            hertz: 440.0,
        }
    }
}

/// Writes a 16-bit PCM WAV file whose channels carry the given tones.
///
/// Every channel must describe the same total duration; a shorter one is padded
/// with silence, which keeps a hard-panned test case easy to write.
pub(crate) fn write_wav(path: &Path, sample_rate: u32, channels: &[Vec<Tone>]) {
    assert!(!channels.is_empty(), "a WAV file has at least one channel");
    let rendered: Vec<Vec<i16>> = channels
        .iter()
        .map(|tones| render(tones, sample_rate))
        .collect();
    let frames = rendered.iter().map(Vec::len).max().unwrap_or(0);

    let channel_count = u16::try_from(channels.len()).expect("a small channel count");
    let bytes_per_frame = u32::from(channel_count) * 2;
    let data_bytes = u32::try_from(frames).expect("a test-sized file") * bytes_per_frame;

    let mut file = Vec::with_capacity(44 + data_bytes as usize);
    file.extend_from_slice(b"RIFF");
    file.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    file.extend_from_slice(b"WAVE");
    file.extend_from_slice(b"fmt ");
    file.extend_from_slice(&16u32.to_le_bytes());
    file.extend_from_slice(&1u16.to_le_bytes()); // PCM
    file.extend_from_slice(&channel_count.to_le_bytes());
    file.extend_from_slice(&sample_rate.to_le_bytes());
    file.extend_from_slice(&(sample_rate * bytes_per_frame).to_le_bytes());
    file.extend_from_slice(&u16::try_from(bytes_per_frame).expect("small").to_le_bytes());
    file.extend_from_slice(&16u16.to_le_bytes());
    file.extend_from_slice(b"data");
    file.extend_from_slice(&data_bytes.to_le_bytes());

    for frame in 0..frames {
        for channel in &rendered {
            let sample = channel.get(frame).copied().unwrap_or(0);
            file.extend_from_slice(&sample.to_le_bytes());
        }
    }

    std::fs::write(path, file).expect("the test WAV can be written");
}

/// One channel's samples.
fn render(tones: &[Tone], sample_rate: u32) -> Vec<i16> {
    let mut samples = Vec::new();
    for tone in tones {
        let count = (tone.seconds * f64::from(sample_rate)).round() as usize;
        for index in 0..count {
            let time = index as f64 / f64::from(sample_rate);
            let value = if tone.amplitude == 0.0 {
                0.0
            } else {
                f64::from(tone.amplitude) * (core::f64::consts::TAU * tone.hertz * time).sin()
            };
            samples.push((value * 32_767.0).round() as i16);
        }
    }
    samples
}

/// Muxes WAV files into one Matroska container, one audio track each.
///
/// Returns `false` when this checkout has no `ffmpeg.exe`, having already
/// reported the skip; the caller returns without asserting. `CLIPPED_REQUIRE_MEDIA`
/// turns that into a failure, which is how CI is configured
/// (`.github/workflows/ci.yml`).
pub(crate) fn mux_tracks(sources: &[(&Path, &str)], destination: &Path) -> bool {
    let Some(tools) = clipped_media_validation::require_media_tools() else {
        return false;
    };
    run_ffmpeg(&tools, sources, destination);
    true
}

fn run_ffmpeg(tools: &MediaTools, sources: &[(&Path, &str)], destination: &Path) {
    let mut command = Command::new(tools.ffmpeg());
    command.arg("-nostdin").arg("-y");
    for (source, _) in sources {
        command.arg("-i").arg(source);
    }
    for index in 0..sources.len() {
        command.arg("-map").arg(format!("{index}:a"));
    }
    for (index, (_, title)) in sources.iter().enumerate() {
        command
            .arg(format!("-metadata:s:a:{index}"))
            .arg(format!("title={title}"));
    }
    // Copied rather than re-encoded: the samples the tests assert against have
    // to be the samples that were written.
    command.arg("-c:a").arg("copy").arg(destination);

    let output = command.output().expect("ffmpeg can be started");
    assert!(
        output.status.success(),
        "ffmpeg failed to mux the test tracks: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Writes a Matroska file with a video stream and no audio at all, which is
/// what every recording Clipped writes today looks like (issue #180).
///
/// Returns `false` when this checkout has no `ffmpeg.exe`.
pub(crate) fn write_silent_video(destination: &Path, seconds: u32) -> bool {
    let Some(tools) = clipped_media_validation::require_media_tools() else {
        return false;
    };
    let output = Command::new(tools.ffmpeg())
        .arg("-nostdin")
        .arg("-y")
        .args(["-f", "lavfi", "-i"])
        .arg(format!("testsrc=size=160x120:rate=10:duration={seconds}"))
        .args(["-c:v", "libopenh264", "-an"])
        .arg(destination)
        .output()
        .expect("ffmpeg can be started");
    assert!(
        output.status.success(),
        "ffmpeg failed to write a video-only file: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    true
}
