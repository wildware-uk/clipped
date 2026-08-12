//! What keeps the mix under full scale when several sources are loud at once.
//!
//! # The problem
//!
//! Summing is addition, and addition overflows. A game at −6 dBFS and a
//! microphone at −6 dBFS sum to full scale; add a chat application and the
//! result is above it. Whatever writes the mix has to do *something* with a
//! sample of 1.4, and the two obvious somethings are both bad:
//!
//! - **Clip it.** Truncating every excursion to ±1.0 flattens the tops of the
//!   waveform, which is a squared-off wave: broadband harmonic distortion,
//!   loudest exactly when the recording is at its most exciting. This is the
//!   failure the issue names — a mix that distorts when the game is loud and
//!   somebody speaks is worse than no mix at all.
//! - **Divide everything by the number of sources.** A mix that is 12 dB
//!   quieter than the game's own track whenever four sources are declared, most
//!   of them silent, is a recording somebody turns up and then discovers is
//!   noisy.
//!
//! # What this does instead
//!
//! A peak limiter: one gain, applied to every channel of a frame together, that
//! drops instantly to whatever keeps that frame under [`CEILING`] and recovers
//! towards unity over [`RELEASE`]. Quiet passages pass through at exactly unity
//! — the common case costs one comparison per frame and no multiplication —
//! and a loud passage is turned down as a whole rather than having its peaks
//! sliced off.
//!
//! One gain for the whole frame is the part that is easy to get wrong.
//! Computing a gain per channel would turn a loud left channel into a stereo
//! image that swings to the right whenever the mix is hot, which is a stranger
//! artefact than the distortion it avoids.
//!
//! # What it is not
//!
//! There is no look-ahead. A true brickwall limiter delays the signal by a few
//! milliseconds so it can begin turning down *before* a transient arrives, and
//! that delay would have to be reconciled with the timestamps the mix carries —
//! the mix is placed against the recording's clock, and a track that is five
//! milliseconds late against the picture to protect against an artefact nobody
//! can hear is a bad trade. Instantaneous attack means the first frame of a
//! sudden transient is turned down abruptly; the audible result is a moment of
//! dullness rather than the sustained buzz that clipping produces, and the test
//! beside this module measures the difference rather than asserting it.

use core::num::NonZeroU32;

/// The highest amplitude the mix is allowed to reach.
///
/// Just under full scale rather than at it. The mix is written as 16-bit PCM by
/// `clipped-muxer`, where +1.0 is one count past the largest representable
/// positive sample; leaving a little room means the conversion cannot wrap a
/// peak round to a full-scale negative, which is an audible click rather than a
/// rounding error.
pub(crate) const CEILING: f32 = 0.99;

/// How long the limiter takes to recover towards unity after a loud passage.
///
/// 200 ms is the usual compromise for programme material: fast enough that a
/// gunshot does not leave the following dialogue quiet for a noticeable time,
/// slow enough that the gain does not move within one cycle of a bass note,
/// which is what makes a limiter audible as distortion in its own right.
const RELEASE_SECONDS: f32 = 0.2;

/// One mix's gain, and the rule that moves it.
///
/// Stateful across blocks on purpose: the gain at the start of a block is the
/// gain at the end of the last one, so a loud passage that spans a block
/// boundary is not re-attacked and the recovery is not restarted.
#[derive(Debug)]
pub(crate) struct Limiter {
    /// How much of the way to the target the gain moves per frame while
    /// recovering. Derived from [`RELEASE_SECONDS`] and the sample rate once,
    /// because `exp` per frame would be the most expensive thing in the mixer.
    release: f32,
    /// The multiplier the next frame will be scaled by, before this frame's own
    /// peak is taken into account.
    gain: f32,
}

impl Limiter {
    /// Starts a limiter for a mix at `sample_rate`, at unity gain.
    pub(crate) fn new(sample_rate: NonZeroU32) -> Self {
        let frames = RELEASE_SECONDS * sample_rate.get() as f32;
        Self {
            // The one-pole coefficient that reaches 1 − 1/e of the way to the
            // target in `RELEASE_SECONDS`.
            release: 1.0 - (-1.0 / frames).exp(),
            gain: 1.0,
        }
    }

    /// Holds `samples` — interleaved, `channels` per frame — under the ceiling,
    /// and reports how many frames had to be turned down to do it.
    ///
    /// The count is the diagnostic: a mix whose limiter never engages has
    /// headroom to spare, and one that is engaged for most of a session is a
    /// mix whose levels are set too hot. It is a count of frames and not a
    /// measurement of them, which is what keeps it printable in a log beside a
    /// microphone (AGENTS.md section 13).
    pub(crate) fn apply(&mut self, samples: &mut [f32], channels: usize) -> u64 {
        debug_assert!(channels > 0, "a frame has at least one channel");
        let mut limited = 0;

        for frame in samples.chunks_exact_mut(channels) {
            let peak = frame
                .iter()
                .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
            let target = if peak > CEILING { CEILING / peak } else { 1.0 };

            if target < self.gain {
                // Attack: whatever it takes, this frame. `gain` is now exactly
                // `CEILING / peak`, so the loudest sample in the frame lands on
                // the ceiling and nothing in it can exceed it.
                self.gain = target;
            } else {
                // Release: towards the target, never past it, so the guarantee
                // above survives a frame whose own peak needs less reduction
                // than the last one did.
                self.gain += (target - self.gain) * self.release;
            }

            if self.gain < 1.0 {
                for sample in frame.iter_mut() {
                    *sample *= self.gain;
                }
                limited += 1;
            }
        }

        limited
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn rate() -> NonZeroU32 {
        NonZeroU32::new(RATE).expect("48 kHz is not zero")
    }

    /// `seconds` of a sine at `frequency`, at `amplitude`, as mono frames.
    fn sine(frequency: f32, amplitude: f32, seconds: f32) -> Vec<f32> {
        let frames = (seconds * RATE as f32) as usize;
        (0..frames)
            .map(|frame| {
                let phase = core::f32::consts::TAU * frequency * frame as f32 / RATE as f32;
                amplitude * phase.sin()
            })
            .collect()
    }

    fn peak(samples: &[f32]) -> f32 {
        samples
            .iter()
            .fold(0.0f32, |peak, sample| peak.max(sample.abs()))
    }

    #[test]
    fn audio_that_already_fits_is_passed_through_untouched() {
        // The common case, and the one that must cost nothing: a mix with
        // headroom has to come out bit-identical, or every recording is
        // quietly altered by a stage that exists for the loud ones.
        let mut limiter = Limiter::new(rate());
        let original = sine(440.0, 0.4, 0.2);
        let mut samples = original.clone();

        let limited = limiter.apply(&mut samples, 1);

        assert_eq!(limited, 0, "nothing here needed turning down");
        assert_eq!(samples, original);
    }

    #[test]
    fn a_sum_that_exceeds_full_scale_comes_out_below_the_ceiling() {
        // Three sources at −6 dBFS: exactly the case in the issue, and 1.5 in
        // arithmetic.
        let mut limiter = Limiter::new(rate());
        let mut samples: Vec<f32> = sine(440.0, 0.5, 0.5)
            .iter()
            .zip(sine(880.0, 0.5, 0.5))
            .zip(sine(1320.0, 0.5, 0.5))
            .map(|((game, system), microphone)| game + system + microphone)
            .collect();
        assert!(
            peak(&samples) > 1.0,
            "this test needs an input that actually overflows"
        );

        limiter.apply(&mut samples, 1);

        assert!(
            peak(&samples) <= CEILING + f32::EPSILON,
            "the mix reached {} against a ceiling of {CEILING}",
            peak(&samples)
        );
    }

    #[test]
    fn the_gain_is_the_same_for_every_channel_of_a_frame() {
        // A per-channel gain would swing the stereo image to the right whenever
        // the left channel was loud, which is a stranger artefact than the one
        // this module exists to avoid.
        let mut limiter = Limiter::new(rate());
        // One frame, hard left: the left channel needs turning down and the
        // right one does not.
        let mut frame = [1.6f32, 0.4];

        limiter.apply(&mut frame, 2);

        let ratio = frame[0] / frame[1];
        assert!(
            (ratio - 4.0).abs() < 1e-3,
            "the two channels were 4:1 before limiting and are {ratio}:1 after"
        );
        assert!(frame[0] <= CEILING + f32::EPSILON, "{}", frame[0]);
    }

    #[test]
    fn the_gain_recovers_towards_unity_rather_than_snapping_back() {
        // Without a release, the gain would return to 1.0 the moment the loud
        // passage ended, and the boundary between the two would be a step in
        // level on every transient in the recording.
        let mut limiter = Limiter::new(rate());

        // A tenth of a second of overload, then quiet.
        let mut loud = vec![1.5f32; RATE as usize / 10];
        limiter.apply(&mut loud, 1);
        let after_overload = limiter.gain;
        assert!(
            after_overload < 0.7,
            "gain after overload: {after_overload}"
        );

        // 20 ms of quiet: recovering, but nowhere near back to unity.
        let mut quiet = vec![0.1f32; RATE as usize / 50];
        limiter.apply(&mut quiet, 1);
        assert!(
            limiter.gain > after_overload && limiter.gain < 0.95,
            "20 ms into the release the gain is {}",
            limiter.gain
        );

        // A second later it is back, so a single loud moment does not leave the
        // rest of the recording quiet.
        let mut later = vec![0.1f32; RATE as usize];
        limiter.apply(&mut later, 1);
        assert!(limiter.gain > 0.99, "a second later: {}", limiter.gain);
    }

    #[test]
    fn the_gain_carries_across_blocks_rather_than_restarting_at_unity() {
        // The reason the gain is state rather than a per-block calculation. The
        // mixer emits in instalments, so the recovery from a loud passage almost
        // always spans a block boundary; a limiter that started each block at
        // unity would jump the level back up at the boundary and then pull it
        // down again, which is an artefact at the block rate — audible, and
        // present in every recording rather than only in the loud ones.
        let mut limiter = Limiter::new(rate());
        let mut loud = vec![1.5f32; 480];
        limiter.apply(&mut loud, 1);
        let reduced = limiter.gain;
        assert!(reduced < 0.7, "gain after the loud block: {reduced}");

        // Ten milliseconds of quiet, in a block of its own. It fits under the
        // ceiling on its own account, so a limiter with no memory would pass it
        // through untouched.
        let mut quiet = vec![0.5f32; 480];
        limiter.apply(&mut quiet, 1);

        assert!(
            (quiet[0] - 0.5 * reduced).abs() < 1e-3,
            "the first frame of the next block came out at {}, where the gain the last block \
             ended on would make it {}",
            quiet[0],
            0.5 * reduced
        );
    }
}
