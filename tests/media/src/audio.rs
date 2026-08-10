//! What is actually audible on a track.
//!
//! Structure is not content. A recording can declare three audio tracks, name
//! them, time them perfectly and still have written the same mix to all three —
//! and multi-track audio is the product (SPEC.md section 11). The only way to
//! tell the difference is to listen, so the harness listens: it decodes a track
//! and measures how much energy sits at a given frequency.
//!
//! That is what the controlled audio test applications are for. AGENTS.md
//! section 26 has them generating 440 Hz for the game, 880 Hz for other system
//! audio and 1320 Hz for the microphone, precisely so that a test can assert
//! each track carries its own tone *and none of the others*.
//!
//! # How the measurement works
//!
//! A Goertzel filter — a single-frequency discrete Fourier transform — over a
//! quarter-second window taken from a quarter of the way into the track, which
//! is the same technique `crates/audio/tests/system_audio.rs` uses to prove
//! WASAPI captured the tone that was played. It answers both questions this
//! harness needs with one primitive: sweep it to find the strongest frequency,
//! or point it at one frequency to ask whether a tone that should not be here
//! is.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::probe::MediaError;
use crate::tools::MediaTools;

/// How much of a track is analysed.
///
/// A quarter of a second at 48 kHz is 12,000 samples, which puts a Goertzel bin
/// about 4 Hz wide — fine enough to separate the tones the test applications
/// generate, and short enough that sweeping the spectrum stays quick in a debug
/// build.
const WINDOW: Duration = Duration::from_millis(250);

/// Where in the track the window is taken from.
///
/// Not the start: a track often opens with the silence between a recording
/// beginning and a source producing anything, and measuring that would report
/// every track as silent.
const WINDOW_POSITION: f64 = 0.25;

/// The lowest and highest frequencies a sweep considers.
///
/// Wide enough for anything a game or a voice produces, and bounded so a sweep
/// is a bounded amount of work.
const SEARCH_RANGE: (f64, f64) = (40.0, 8_000.0);

/// The sweep's coarse step, which must stay below the bin width above or the
/// sweep walks straight past peaks.
const COARSE_STEP: f64 = 3.0;

/// The step the sweep refines the coarse winner with.
const FINE_STEP: f64 = 0.25;

/// Anything quieter than this counts as silence.
///
/// Full scale is 1.0, so this is about -80 dBFS: below what a dithered
/// recording of a quiet room contains, and far below any tone.
const SILENCE: f32 = 1e-4;

/// One audio track, decoded to mono samples.
#[derive(Debug, Clone)]
pub struct AudioContent {
    samples: Vec<f32>,
    sample_rate: u32,
}

impl AudioContent {
    /// Decodes audio stream `index` of `path` to mono.
    ///
    /// Downmixed to mono deliberately: what is being asked is what the track
    /// *contains*, and a tone panned to one side is still the track's tone.
    /// Channel counts and layouts are asserted structurally instead, from what
    /// `ffprobe` reports.
    pub(crate) fn decode(
        tools: &MediaTools,
        path: &Path,
        index: usize,
        sample_rate: u32,
    ) -> Result<Self, MediaError> {
        let output = Command::new(tools.ffmpeg())
            .args(["-hide_banner", "-v", "error", "-i"])
            .arg(path)
            .args([
                "-map",
                &format!("0:a:{index}"),
                "-ac",
                "1",
                "-f",
                "f32le",
                "-",
            ])
            .output()
            .map_err(|error| MediaError::ToolFailed {
                tool: tools.ffmpeg().to_path_buf(),
                error,
            })?;

        if !output.status.success() {
            return Err(MediaError::Undecodable {
                stream: format!("a:{index}"),
                diagnostics: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }

        let samples = output
            .stdout
            .chunks_exact(4)
            .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            .collect();

        Ok(Self {
            samples,
            sample_rate,
        })
    }

    /// Builds content from samples that are already in hand, which is how the
    /// measurement itself is tested.
    pub fn from_samples(samples: Vec<f32>, sample_rate: u32) -> Self {
        Self {
            samples,
            sample_rate,
        }
    }

    /// The rate the track was decoded at.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Every decoded sample.
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    /// How long the track is.
    pub fn duration(&self) -> Duration {
        if self.sample_rate == 0 {
            return Duration::ZERO;
        }
        Duration::from_secs_f64(self.samples.len() as f64 / f64::from(self.sample_rate))
    }

    /// The loudest sample in the analysis window. Zero for a silent track.
    pub fn peak_amplitude(&self) -> f32 {
        self.window()
            .iter()
            .fold(0.0f32, |peak, sample| peak.max(sample.abs()))
    }

    /// Whether the track carries anything at all.
    pub fn is_silent(&self) -> bool {
        self.peak_amplitude() < SILENCE
    }

    /// How much energy sits at `frequency`, normalised so that windows of
    /// different lengths are comparable.
    ///
    /// A full-scale sine at exactly this frequency measures about 1.0.
    pub fn magnitude_at(&self, frequency: f64) -> f64 {
        let window = self.window();
        if window.is_empty() || self.sample_rate == 0 {
            return 0.0;
        }

        let coefficient =
            2.0 * f64::cos(2.0 * std::f64::consts::PI * frequency / f64::from(self.sample_rate));
        let mut previous = 0.0f64;
        let mut before_that = 0.0f64;
        for sample in window {
            let current = f64::from(*sample) + coefficient * previous - before_that;
            before_that = previous;
            previous = current;
        }

        let power =
            previous * previous + before_that * before_that - coefficient * previous * before_that;
        power.max(0.0).sqrt() / (window.len() as f64 / 2.0)
    }

    /// The strongest frequency in the track, and how strong it is.
    ///
    /// Swept coarsely and then refined, because a step wider than a Goertzel
    /// bin walks past peaks: a 5 Hz sweep over a recording of a 997 Hz tone
    /// looked at 995 and 1000 and reported background noise as the strongest
    /// frequency in the file.
    pub fn dominant_frequency(&self) -> (f64, f64) {
        let (from, to) = SEARCH_RANGE;
        let coarse = self.strongest_between(from, to, COARSE_STEP);
        self.strongest_between(
            (coarse.0 - COARSE_STEP).max(from),
            (coarse.0 + COARSE_STEP).min(to),
            FINE_STEP,
        )
    }

    fn strongest_between(&self, from: f64, to: f64, step: f64) -> (f64, f64) {
        let mut best = (from, -1.0);
        let mut frequency = from;
        while frequency <= to {
            let magnitude = self.magnitude_at(frequency);
            if magnitude > best.1 {
                best = (frequency, magnitude);
            }
            frequency += step;
        }
        best
    }

    /// The samples every measurement is made over.
    fn window(&self) -> &[f32] {
        if self.sample_rate == 0 || self.samples.is_empty() {
            return &self.samples;
        }

        let length = (WINDOW.as_secs_f64() * f64::from(self.sample_rate)) as usize;
        if self.samples.len() <= length {
            return &self.samples;
        }

        let start = ((self.samples.len() - length) as f64 * WINDOW_POSITION) as usize;
        &self.samples[start..start + length]
    }
}

/// The tone a track is expected to carry, and the ones it must not.
#[derive(Debug, Clone)]
pub struct Tone {
    frequency: f64,
    tolerance: f64,
    absent: Vec<f64>,
    ratio: f64,
}

impl Tone {
    /// How far the measured frequency may sit from the expected one before it
    /// is a different tone. A whole tone at 440 Hz is about 50 Hz away, so this
    /// is comfortably inside "the wrong source".
    const DEFAULT_TOLERANCE: f64 = 5.0;

    /// How much stronger the expected tone must be than one that should not be
    /// on the track at all.
    ///
    /// Eight times, in amplitude, is about 18 dB. Two sources mixed together at
    /// anything like equal level are nowhere near that far apart, and a track
    /// that merely picked up a little bleed through a shared device is.
    const DEFAULT_RATIO: f64 = 8.0;

    /// The tone this track should carry.
    pub fn at(frequency: f64) -> Self {
        Self {
            frequency,
            tolerance: Self::DEFAULT_TOLERANCE,
            absent: Vec::new(),
            ratio: Self::DEFAULT_RATIO,
        }
    }

    /// How far from the expected frequency the measurement may land.
    #[must_use]
    pub fn tolerance(mut self, hertz: f64) -> Self {
        self.tolerance = hertz;
        self
    }

    /// A tone that must *not* be audible here, because it belongs to another
    /// source. This is the isolation assertion.
    #[must_use]
    pub fn isolated_from(mut self, frequency: f64) -> Self {
        self.absent.push(frequency);
        self
    }

    /// How much stronger this track's own tone must be than one it is isolated
    /// from.
    #[must_use]
    pub fn minimum_ratio(mut self, ratio: f64) -> Self {
        self.ratio = ratio;
        self
    }

    pub(crate) fn check(&self, label: &str, content: &AudioContent, failures: &mut Vec<String>) {
        if content.is_silent() {
            failures.push(format!(
                "{label} tone: expected {} Hz, but the track is silent (peak amplitude {:.2e} over \
                 {:.2}s of audio)",
                self.frequency,
                content.peak_amplitude(),
                content.duration().as_secs_f64()
            ));
            return;
        }

        let (peak, magnitude) = content.dominant_frequency();
        if (peak - self.frequency).abs() > self.tolerance {
            failures.push(format!(
                "{label} tone: expected {} Hz +/- {} Hz, found {peak:.1} Hz (magnitude \
                 {magnitude:.4}; {} Hz measures {:.4} here)",
                self.frequency,
                self.tolerance,
                self.frequency,
                content.magnitude_at(self.frequency)
            ));
        }

        let wanted = content.magnitude_at(self.frequency);
        for absent in &self.absent {
            let intruder = content.magnitude_at(*absent);
            if intruder * self.ratio > wanted {
                failures.push(format!(
                    "{label} isolation: {absent} Hz belongs to another source and must not be \
                     audible here, but it measures {intruder:.4} against this track's own {} Hz at \
                     {wanted:.4} — {:.1}x apart, and anything below {}x counts as bleed",
                    self.frequency,
                    wanted / intruder.max(f64::MIN_POSITIVE),
                    self.ratio
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    /// A second of a sine wave at `frequency`, at `amplitude` of full scale.
    fn sine(frequency: f64, amplitude: f32) -> Vec<f32> {
        (0..RATE)
            .map(|sample| {
                let phase =
                    2.0 * std::f64::consts::PI * frequency * f64::from(sample) / f64::from(RATE);
                amplitude * phase.sin() as f32
            })
            .collect()
    }

    fn mixed(first: &[f32], second: &[f32]) -> Vec<f32> {
        first
            .iter()
            .zip(second)
            .map(|(left, right)| left + right)
            .collect()
    }

    #[test]
    fn the_dominant_frequency_of_a_tone_is_the_tone() {
        for frequency in [440.0, 880.0, 1320.0] {
            let content = AudioContent::from_samples(sine(frequency, 0.5), RATE);
            let (peak, magnitude) = content.dominant_frequency();
            assert!(
                (peak - frequency).abs() <= 1.0,
                "a {frequency} Hz tone measured as {peak} Hz"
            );
            assert!(
                magnitude > 0.4,
                "magnitude {magnitude} for a half-scale tone"
            );
        }
    }

    #[test]
    fn a_tone_that_is_not_there_measures_far_below_one_that_is() {
        let content = AudioContent::from_samples(sine(440.0, 0.5), RATE);
        assert!(
            content.magnitude_at(880.0) * 8.0 < content.magnitude_at(440.0),
            "880 Hz measured {} in a recording of 440 Hz at {}",
            content.magnitude_at(880.0),
            content.magnitude_at(440.0)
        );
    }

    #[test]
    fn silence_is_reported_as_silence_rather_than_as_a_frequency() {
        let content = AudioContent::from_samples(vec![0.0; RATE as usize], RATE);
        assert!(content.is_silent());

        let mut failures = Vec::new();
        Tone::at(440.0).check("a:0", &content, &mut failures);
        assert_eq!(failures.len(), 1, "{failures:#?}");
        assert!(
            failures[0].contains("the track is silent"),
            "unhelpful failure: {}",
            failures[0]
        );
    }

    #[test]
    fn a_track_carrying_its_own_tone_passes() {
        let content = AudioContent::from_samples(sine(440.0, 0.5), RATE);
        let mut failures = Vec::new();
        Tone::at(440.0)
            .isolated_from(880.0)
            .isolated_from(1320.0)
            .check("a:0", &content, &mut failures);
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn a_track_that_another_source_bled_into_fails_isolation() {
        // The failure the multi-track model exists to prevent: the game's tone
        // is still the loudest thing on the microphone track, so every
        // structural check passes and the track is still wrong.
        let content =
            AudioContent::from_samples(mixed(&sine(1320.0, 0.5), &sine(440.0, 0.4)), RATE);

        let mut failures = Vec::new();
        Tone::at(1320.0)
            .isolated_from(440.0)
            .check("a:2", &content, &mut failures);

        assert_eq!(failures.len(), 1, "{failures:#?}");
        assert!(
            failures[0].contains("a:2 isolation")
                && failures[0].contains("440 Hz belongs to another source"),
            "unhelpful failure: {}",
            failures[0]
        );
    }

    #[test]
    fn the_wrong_tone_entirely_names_both_frequencies() {
        let content = AudioContent::from_samples(sine(880.0, 0.5), RATE);
        let mut failures = Vec::new();
        Tone::at(440.0).check("a:1", &content, &mut failures);

        assert_eq!(failures.len(), 1, "{failures:#?}");
        assert!(
            failures[0].contains("expected 440 Hz") && failures[0].contains("found 880"),
            "unhelpful failure: {}",
            failures[0]
        );
    }
}
