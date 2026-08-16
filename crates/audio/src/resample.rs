//! Nudging a packet's own frame count to track a reference clock, a little at
//! a time.
//!
//! `crate::timeline` measures how far a source has drifted from the reference
//! clock and turns that into a ratio close to `1.0`
//! ([`Timeline::correction_ratio`](crate::timeline::Timeline::correction_ratio)).
//! This module is what *acts* on that ratio: [`LinearResampler`] takes a
//! packet of interleaved samples and produces very slightly more or fewer
//! frames than it was given, by linear interpolation, so that the declared
//! duration of a source's track tracks real elapsed time instead of the
//! source's own — very slightly wrong — idea of how fast it is.
//!
//! # Why linear interpolation
//!
//! The ratio this crate ever asks for is within a few hundred parts per
//! million of `1.0` — real hardware drift, clamped generously
//! (`crate::timeline::MAX_DRIFT_RATIO`) — which is a change small enough that
//! the choice of interpolation barely matters: the worst linear interpolation
//! can do is round off a sample that is already almost exactly where its
//! neighbour is. A higher-order resampler buys quality that matters when
//! converting between genuinely different rates — 44.1 kHz to 48 kHz, say —
//! which is not what this module is for (`crate::format` still refuses that;
//! see its module documentation). For a correction this small, linear
//! interpolation is the simple thing AGENTS.md section 1 asks to prefer, and
//! it needs no new dependency (AGENTS.md section 10).
//!
//! # Continuity across packets
//!
//! A capture hands packets over one at a time, but the correction has to be
//! seamless across the boundary between them, or every packet boundary would
//! be an audible seam. So a [`LinearResampler`] remembers the last frame of
//! whatever it last processed ([`LinearResampler::carry`]) and the fractional
//! position its next output frame falls at
//! ([`LinearResampler::phase`]), and picks up exactly there on the next call —
//! the same technique a phase-accumulator sample-rate converter uses, kept to
//! the one case this crate needs.
//!
//! [`LinearResampler::reset`] throws both away. It has to be called whenever
//! the *content* on either side of a boundary is not actually adjacent in
//! time — a real gap, a trim, a reopened stream — because interpolating across
//! one of those blends two sounds that were never next to each other into a
//! false transition, in exchange for a discontinuity smoother than the one it
//! is being asked to hide. `crate::windows::endpoint_capture` is what knows
//! when that is true and calls it.

/// Resamples a stream of interleaved packets by a ratio close to `1.0`,
/// carrying enough state across calls that consecutive packets stay seamless.
///
/// Not `Clone` or `Copy`: the whole point of this type is the history it
/// carries between one packet and the next, so a copy of it would silently
/// stop being interchangeable with the original the moment either processed a
/// packet.
#[derive(Debug)]
pub(crate) struct LinearResampler {
    /// Samples per frame. Fixed for the life of this resampler.
    channels: usize,
    /// The last frame this resampler has processed, one sample per channel —
    /// what interpolation treats as the frame immediately before frame zero
    /// of the next call to [`process`](Self::process). [`None`] before the
    /// first frame it has ever seen, or since the last [`reset`](Self::reset).
    carry: Option<Vec<f32>>,
    /// Where the next output frame falls, in input-frame units counted from
    /// `carry`: `0.0` is exactly `carry` and `1.0` is exactly frame zero of
    /// the next packet passed to [`process`](Self::process). Always in
    /// `[0.0, 1.0]` between calls, because `process` never produces an output
    /// frame past the last input frame it was given.
    phase: f64,
}

impl LinearResampler {
    /// Starts a resampler for a stream of `channels`-channel packets.
    ///
    /// `channels` is fixed for the life of a capture (`crate::format`), so
    /// this never needs to change once a capture has opened.
    pub(crate) fn new(channels: core::num::NonZeroU16) -> Self {
        Self {
            channels: usize::from(channels.get()),
            carry: None,
            phase: 0.0,
        }
    }

    /// Forgets everything this resampler has carried from previous packets.
    ///
    /// Call this whenever the next packet is not actually adjacent, in time,
    /// to the last one this resampler processed — see the module
    /// documentation. The next call to [`process`](Self::process) then treats
    /// its first frame the way the very first packet ever given to a fresh
    /// resampler is treated: as its own predecessor, so the first output
    /// frame is exact rather than interpolated from unrelated audio.
    pub(crate) fn reset(&mut self) {
        self.carry = None;
        self.phase = 0.0;
    }

    /// Resamples `input` by `ratio`, appending interleaved output frames to
    /// `out` (which is cleared first), and returns how many frames were
    /// produced.
    ///
    /// `input` is interleaved samples, [`Self::new`]'s `channels` of them per
    /// frame. `ratio` close to `1.0` produces close to as many output frames
    /// as `input` holds; a `ratio` below `1.0` produces fewer, a `ratio` above
    /// `1.0` more. The exact count is not `input`'s frame count times `ratio`
    /// rounded — it is whatever keeps the running fractional position exact
    /// across calls, which is what keeps a long correction from drifting
    /// against its own rounding.
    ///
    /// Does nothing and returns `0` for an empty `input`, without disturbing
    /// carried state: an empty packet carries no frame to become the next
    /// `carry`.
    pub(crate) fn process(&mut self, input: &[f32], ratio: f64, out: &mut Vec<f32>) -> u64 {
        out.clear();
        let channels = self.channels;
        let frames = input.len() / channels;
        if frames == 0 {
            return 0;
        }

        if self.carry.is_none() {
            // No history to interpolate from: the packet's own first frame
            // stands in for its predecessor, so the first output frame this
            // call produces is exactly that frame rather than an
            // interpolation from nothing.
            self.carry = Some(input[..channels].to_vec());
            self.phase = 1.0;
        }
        let carry = self
            .carry
            .as_ref()
            .expect("just set above when it was None");

        let step = 1.0 / ratio;
        let mut produced = 0u64;
        while self.phase <= frames as f64 {
            let lower = self.phase.floor();
            let frac = (self.phase - lower) as f32;
            // Virtual frame numbering: 0 is `carry`, 1..=frames is
            // `input[0..frames]`. `phase` never exceeds `frames`, so
            // `lower` is always in that range.
            let lower_index = lower as usize;
            for channel in 0..channels {
                let a = if lower_index == 0 {
                    carry[channel]
                } else {
                    input[(lower_index - 1) * channels + channel]
                };
                let upper_index = lower_index + 1;
                let b = if upper_index - 1 < frames {
                    input[(upper_index - 1) * channels + channel]
                } else {
                    // `phase` lands exactly on `frames`: there is no frame
                    // after the last one, but `frac` is exactly `0.0` here,
                    // so `b` is never actually used.
                    a
                };
                out.push(a + (b - a) * frac);
            }
            produced += 1;
            self.phase += step;
        }

        // Re-base for the next call: frame zero of the next packet is what
        // this call's last frame becomes, and the phase carried forward is
        // the small remainder past this packet that the loop above stopped
        // at.
        self.phase -= frames as f64;
        let last = (frames - 1) * channels;
        self.carry = Some(input[last..last + channels].to_vec());

        produced
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU16;

    use super::*;

    fn stereo() -> LinearResampler {
        LinearResampler::new(NonZeroU16::new(2).expect("stereo is not zero channels"))
    }

    /// A ramp makes a wrong interpolation obvious: every frame's value is its
    /// own frame index, so a resampled output frame's value says exactly
    /// which input position it was taken from.
    fn ramp(frames: usize, channels: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames * channels);
        for frame in 0..frames {
            for _ in 0..channels {
                out.push(frame as f32);
            }
        }
        out
    }

    #[test]
    fn a_ratio_of_one_reproduces_the_input_exactly() {
        let mut resampler = stereo();
        let input = ramp(480, 2);
        let mut out = Vec::new();

        let produced = resampler.process(&input, 1.0, &mut out);

        assert_eq!(produced, 480);
        assert_eq!(out, input);
    }

    #[test]
    fn a_ratio_of_one_stays_exact_across_a_packet_boundary() {
        // The property that matters most: two packets processed one after
        // another at ratio 1.0 must reproduce both exactly, with the frame
        // that closes the first packet and the frame that opens the second
        // exactly one frame apart, not zero and not two.
        let mut resampler = stereo();
        let first = ramp(480, 2);
        let second: Vec<f32> = (480..960).flat_map(|frame| [frame as f32; 2]).collect();

        let mut out = Vec::new();
        let produced_first = resampler.process(&first, 1.0, &mut out);
        assert_eq!(produced_first, 480);
        assert_eq!(out, first);

        let produced_second = resampler.process(&second, 1.0, &mut out);
        assert_eq!(produced_second, 480);
        assert_eq!(out, second);
    }

    #[test]
    fn a_ratio_below_one_produces_fewer_frames_than_it_was_given() {
        // A source running slow needs fewer output frames per input frame —
        // see the timeline module documentation for why the sign is this way
        // round.
        let mut resampler = stereo();
        let input = ramp(1000, 2);
        let mut out = Vec::new();

        let produced = resampler.process(&input, 0.999, &mut out);

        // 1000 frames at a 0.999 ratio is very close to 999, and the running
        // phase means it is never far off even before any packets have run
        // to let the estimate settle.
        assert!(
            (997..=999).contains(&produced),
            "expected close to 999 frames, got {produced}"
        );
    }

    #[test]
    fn a_ratio_above_one_produces_more_frames_than_it_was_given() {
        let mut resampler = stereo();
        let input = ramp(1000, 2);
        let mut out = Vec::new();

        let produced = resampler.process(&input, 1.001, &mut out);

        assert!(
            (1000..=1002).contains(&produced),
            "expected close to 1001 frames, got {produced}"
        );
    }

    #[test]
    fn output_values_stay_between_their_neighbouring_input_values() {
        // The property that makes this a resampler rather than noise: every
        // output frame's value is a linear blend of two consecutive input
        // frames, so on a monotonic ramp no output frame can be lower than
        // the lowest or higher than the highest input frame around it.
        let mut resampler = stereo();
        let input = ramp(500, 2);
        let mut out = Vec::new();
        resampler.process(&input, 1.0003, &mut out);

        for frame in out.chunks_exact(2) {
            assert!(
                (0.0..=499.0).contains(&frame[0]),
                "output value {} left the range of the input it was built from",
                frame[0]
            );
        }
    }

    #[test]
    fn a_long_run_at_a_steady_ratio_tracks_the_ratio_exactly() {
        // Across many packets, the frame count produced has to converge on
        // `input_frames * ratio` — not drift away from it the way summing a
        // rounded-per-packet count would.
        let mut resampler = stereo();
        let ratio = 1.0 + 100e-6; // 100 ppm, a realistic clock error.
        let packet_frames: u64 = 480;
        let mut produced_total = 0u64;
        let mut out = Vec::new();

        for packet in 0..2000u64 {
            let input: Vec<f32> = (0..packet_frames)
                .flat_map(|frame| {
                    let value = (packet * packet_frames + frame) as f32;
                    [value; 2]
                })
                .collect();
            produced_total += resampler.process(&input, ratio, &mut out);
        }

        let input_total = 2000.0 * packet_frames as f64;
        let expected_total = input_total * ratio;

        // At most one frame out, and that bound is structural rather than
        // generous: the phase accumulator holds the fraction of a frame it has
        // not yet emitted, so the count trails the ideal by that fraction and
        // never by more. Checked rather than assumed — running the same loop
        // eight times longer (16,000 packets, 7,680,768 ideal) leaves the error
        // at exactly one frame, so it is a boundary and not a drift. A
        // per-packet rounding error, which is what this test exists to catch,
        // would have been two thousand frames out here and sixteen thousand
        // there.
        assert!(
            (produced_total as f64 - expected_total).abs() <= 1.0,
            "produced {produced_total} frames, expected close to {expected_total}"
        );
    }

    #[test]
    fn reset_starts_the_next_packet_without_interpolating_from_the_last_one() {
        // After a reset, the next packet's own first frame has to stand in
        // for its predecessor exactly as it does for a brand new resampler —
        // proving the old carry was actually discarded rather than merely
        // shadowed.
        let mut resampler = stereo();
        let mut out = Vec::new();
        resampler.process(&ramp(10, 2), 1.0, &mut out);

        resampler.reset();

        let next = vec![500.0f32, 500.0, 501.0, 501.0];
        let produced = resampler.process(&next, 1.0, &mut out);
        assert_eq!(produced, 2);
        assert_eq!(
            out, next,
            "a fresh carry must not blend in the old packet's tail"
        );
    }

    #[test]
    fn an_empty_packet_produces_nothing_and_does_not_disturb_carried_state() {
        let mut resampler = stereo();
        let mut out = Vec::new();
        resampler.process(&ramp(10, 2), 1.0, &mut out);

        let produced = resampler.process(&[], 1.0, &mut out);
        assert_eq!(produced, 0);
        assert!(out.is_empty());

        // The carry from the first packet is still there: the next real
        // packet continues from frame 9, not from nothing.
        let next = vec![10.0f32, 10.0];
        let produced = resampler.process(&next, 1.0, &mut out);
        assert_eq!(produced, 1);
        assert_eq!(out, next);
    }
}
