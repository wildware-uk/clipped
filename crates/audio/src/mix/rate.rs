//! Converting one source's samples to the sample rate the compatibility mix is
//! written at.
//!
//! # Why this is not `crate::resample`
//!
//! Both modules change how many frames a buffer occupies, and they are not
//! interchangeable. [`LinearResampler`](crate::resample::LinearResampler)
//! corrects a source against the reference *clock*: the ratio it is ever asked
//! for is within a few hundred parts per million of `1.0`, so linear
//! interpolation cannot do much harm — the worst it does is round off a sample
//! that is already almost exactly where its neighbour is.
//!
//! This module converts between rates that genuinely differ — a 44.1 kHz
//! microphone into a 48 kHz mix — and there linear interpolation is a bad
//! answer, which is why `crate::resample` says in as many words that it is not
//! for this. Interpolating a straight line between neighbouring samples is a
//! triangular reconstruction kernel, whose stopband falls away far too slowly
//! to suppress the images the conversion creates: a 10 kHz tone taken from
//! 44.1 kHz to 48 kHz that way puts a spurious tone at 13.9 kHz about 23 dB
//! below it. That is plainly audible, and it would be in the one track a person
//! who double-clicks the file is going to hear.
//!
//! # What it does instead
//!
//! A windowed-sinc interpolator, which is the textbook answer and needs no
//! dependency (AGENTS.md section 10). Each output frame is
//!
//! ```text
//! y[n] = Σ  x[i + o] · h(o − f)
//! ```
//!
//! over [`TAPS`] taps `o` either side of the input position `i + f` that output
//! frame falls at, where `h` is a sinc — the ideal reconstruction filter —
//! multiplied by a Blackman window so that truncating it to [`TAPS`] taps does
//! not put the ripple back that the sinc was there to avoid.
//!
//! Three details make it correct rather than merely plausible:
//!
//! **The cutoff follows the lower of the two rates.** Going *down* in rate —
//! 48 kHz into a 44.1 kHz mix — anything above the output's Nyquist frequency
//! has nowhere to go but back down the spectrum as aliasing, so the sinc's
//! cutoff is the output Nyquist rather than the input's. Going up, the cutoff
//! is the input's own Nyquist and nothing is thrown away.
//!
//! **Every phase is normalised to unity gain.** The taps for a given fractional
//! position are scaled to sum to `1.0`, so a constant in is a constant out. That
//! matters most going *down* in rate, where it is not a refinement but the
//! difference between right and wrong: a sinc whose cutoff is a fraction of
//! Nyquist has a DC gain of the reciprocal of that fraction, so a conversion
//! from 48 kHz to 32 kHz would arrive 3.5 dB louder than it left — enough on its
//! own to push the compatibility mix into its limiter. Going *up* in rate the
//! same taps are already within two parts in a million of unity, so this costs
//! nothing there and is not what holds that case up. Normalising each row rather
//! than applying one scale to the whole table also removes the small ripple
//! between one fractional position and the next.
//!
//! **The fractional position is interpolated between the two rows of the table
//! either side of it, not taken from the nearer one.** [`PHASES`] rows quantise
//! the position to four thousandths of a sample, which is a timing jitter, and
//! a timing jitter spreads a tone into spurs across the whole spectrum rather
//! than into one place somebody could think to look. Measured on a 10 kHz tone
//! taken from 44.1 kHz to 48 kHz, everything that is not the tone comes to
//! **−56 dB** without the interpolation and **−103 dB** with it
//! (`a_converted_tone_arrives_with_nothing_beside_it`), for one extra multiply
//! and add per tap.
//!
//! # What it costs
//!
//! [`TAPS`] multiply-accumulates per output frame per channel, and one table of
//! `(PHASES + 1) × TAPS` `f32` — 33 KB — shared by nothing, built once when a
//! source is added. At 48 kHz stereo that is 3.1 million multiply-accumulates a
//! second for as long as the recording lasts, which is about a percent of one
//! core on the machine `docs/audio-routing.md` records its measurements on, and
//! it is paid **only** by a source whose rate differs from the mix's: a source
//! at the mix's own rate has no converter at all and its samples are added
//! exactly as they arrive.
//!
//! # What it does not touch
//!
//! The source's own track. [`Mixer::contribute`](super::Mixer::contribute)
//! takes `&[f32]`, this module writes to a buffer of its own, and what goes to
//! the isolated track is the capture's own samples at the capture's own rate —
//! unresampled, unmixed, and unlimited. That is the whole reason this is
//! allowed to happen at all: AGENTS.md section 22 is about what a *track*
//! contains, and no track's contents change here. Only the compatibility mix,
//! which is a combination by definition, gains a source it did not have.

/// Taps either side of the output position, so [`TAPS`] in total.
///
/// Sixteen either side is where the windowed sinc's stopband is deep enough
/// that the conversion is not what limits the mix: the images it leaves are
/// below −80 dB, two orders of magnitude under the −60 dB an ordinary consumer
/// endpoint's own converter manages. More taps buy nothing audible and cost
/// proportionally.
const HALF: usize = 32;

/// Taps in the filter, and the number of input frames each output frame reads.
const TAPS: usize = HALF * 2;

/// Fractional positions the table holds a row of taps for.
///
/// The position between them is interpolated, so this is not the resolution of
/// the conversion — it is how far apart the two rows being interpolated between
/// are, which decides how much the linear interpolation between them can be
/// wrong by.
const PHASES: usize = 256;

/// Converts a stream of interleaved packets from one sample rate to another,
/// carrying enough of the previous packet that the boundary between two of them
/// is not a seam.
///
/// Not `Clone`: like [`LinearResampler`](crate::resample::LinearResampler) the
/// point of this type is the history it carries, so a copy would stop being
/// interchangeable with the original the moment either processed a packet.
#[derive(Debug)]
pub(super) struct RateConverter {
    /// Samples per frame. Fixed for the life of a capture.
    channels: usize,
    /// Input frames advanced per output frame: the input rate over the output
    /// rate.
    step: f64,
    /// `(PHASES + 1) × TAPS` taps, row `p` holding the filter for fractional
    /// position `p / PHASES`. The extra row is what row `PHASES - 1`
    /// interpolates towards.
    table: Vec<f32>,
    /// The tail of everything processed so far: enough frames for the next
    /// output frame's taps to reach back into, interleaved.
    history: Vec<f32>,
    /// [`history`](Self::history) followed by the packet being processed.
    /// Reused so that steady-state conversion allocates nothing.
    work: Vec<f32>,
    /// Where the next output frame falls, as a fractional frame index into
    /// [`work`](Self::work).
    position: f64,
}

impl RateConverter {
    /// Starts a converter from `from` Hz to `to` Hz for `channels`-channel
    /// packets.
    pub(super) fn new(from: u32, to: u32, channels: u16) -> Self {
        let from = f64::from(from.max(1));
        let to = f64::from(to.max(1));
        // Down in rate: the output cannot hold anything above its own Nyquist
        // frequency, so the filter has to remove it before the conversion does
        // by folding it back down the spectrum. Up in rate: nothing is lost, so
        // the cutoff stays the input's own Nyquist and the filter is only there
        // to suppress the images.
        let cutoff = (to / from).min(1.0);

        let mut table = Vec::with_capacity((PHASES + 1) * TAPS);
        for phase in 0..=PHASES {
            let fraction = phase as f64 / PHASES as f64;
            let row = core::array::from_fn::<f64, TAPS, _>(|tap| {
                let at = (tap as f64 - (HALF - 1) as f64) - fraction;
                sinc(cutoff * at) * blackman(at)
            });
            // Unity gain at every fractional position, so that a constant in is
            // a constant out wherever the position happens to fall.
            let total: f64 = row.iter().sum();
            let scale = if total.abs() > f64::EPSILON {
                1.0 / total
            } else {
                1.0
            };
            table.extend(row.iter().map(|tap| (tap * scale) as f32));
        }

        let channels = usize::from(channels.max(1));
        let mut converter = Self {
            channels,
            step: from / to,
            table,
            history: Vec::new(),
            work: Vec::new(),
            position: 0.0,
        };
        converter.reset();
        converter
    }

    /// How far behind the samples it is given this converter's output runs, in
    /// input frames.
    ///
    /// An interpolator that reads [`HALF`] frames either side of an output
    /// frame cannot produce that output frame until those later input frames
    /// have arrived, so what comes out of a call is the content of [`HALF`]
    /// frames earlier. That is a *constant*, which makes it removable: the
    /// caller places the output this much earlier than the packet it came from
    /// and the conversion contributes no offset at all, rather than a third of
    /// a millisecond of one.
    pub(super) const fn delay_frames(&self) -> u64 {
        HALF as u64
    }

    /// Forgets every sample this converter has carried between packets.
    ///
    /// For the same reason [`LinearResampler::reset`](crate::resample::LinearResampler::reset)
    /// exists: when the next packet is not adjacent in time to the last one —
    /// a gap the endpoint never described, a stream reopened — interpolating
    /// across the join blends two sounds that were never next to each other.
    /// The first [`HALF`] frames after a reset fade in from silence over about
    /// a third of a millisecond, which is the smaller of the two artefacts.
    pub(super) fn reset(&mut self) {
        self.history.clear();
        // Enough silence in front that the first output frame's taps have
        // something to read, and that it lands exactly `delay_frames` before
        // the first real input frame — so the caller's constant correction is
        // right from the first packet rather than only in the steady state.
        self.history.resize((TAPS - 1) * self.channels, 0.0);
        self.position = (HALF - 1) as f64;
    }

    /// Converts `input`, appending interleaved output frames to `out` (which is
    /// cleared first), and returns how many frames were produced.
    ///
    /// The count varies from call to call by a frame either way even for
    /// constant-sized input, because the fractional position is carried across
    /// calls rather than rounded at each one. That is what keeps an hour of
    /// conversion from drifting against its own rounding.
    pub(super) fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> usize {
        out.clear();
        let channels = self.channels;

        self.work.clear();
        self.work.reserve(self.history.len() + input.len());
        self.work.extend_from_slice(&self.history);
        self.work.extend_from_slice(input);

        let frames = self.work.len() / channels;
        // How far the next output frame may be placed and still have all of its
        // taps: it reads `HALF` frames forward, so it cannot be produced until
        // those have arrived. `frames` is always at least `TAPS - 1`, because
        // that is what `reset` puts in the history and what the retention below
        // leaves there, so this never actually saturates — it is written this
        // way rather than as an early return because a call that produces
        // nothing must still fall through to the retention and *keep* the
        // packet it was given.
        let last = frames.saturating_sub(HALF);

        let mut produced = 0;
        while self.position < last as f64 {
            let index = self.position as usize;
            let fraction = self.position - index as f64;
            let scaled = fraction * PHASES as f64;
            let row = scaled as usize;
            let between = (scaled - row as f64) as f32;

            let taps = row * TAPS;
            let next = taps + TAPS;
            let first = (index + 1 - HALF) * channels;
            for channel in 0..channels {
                let mut sum = 0.0_f32;
                for tap in 0..TAPS {
                    let weight = self.table[taps + tap]
                        + between * (self.table[next + tap] - self.table[taps + tap]);
                    sum += weight * self.work[first + tap * channels + channel];
                }
                out.push(sum);
            }
            produced += 1;
            self.position += self.step;
        }

        // Everything the next call's first output frame can still reach back
        // to, and nothing else, so the memory this holds is bounded by the
        // filter rather than by the packet.
        let keep = (self.position as usize + 1)
            .saturating_sub(HALF)
            .min(frames);
        self.history.clear();
        self.history
            .extend_from_slice(&self.work[keep * channels..]);
        self.position -= keep as f64;

        produced
    }
}

/// The ideal reconstruction filter, `sin(πx) / πx`, and `1.0` at zero.
fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        return 1.0;
    }
    let x = core::f64::consts::PI * x;
    x.sin() / x
}

/// The Blackman window over `[-HALF, HALF]`, and zero outside it.
///
/// Blackman rather than a plain truncation because truncating a sinc is
/// multiplying it by a rectangle, whose own transform has sidelobes 13 dB down
/// — which is the ripple the sinc was chosen to avoid. Blackman's are 58 dB
/// down and fall away as the cube of the distance, in exchange for a wider
/// transition band that [`HALF`] taps either side make narrow enough not to
/// matter.
fn blackman(x: f64) -> f64 {
    let half = HALF as f64;
    if x.abs() > half {
        return 0.0;
    }
    let phase = core::f64::consts::PI * (x + half) / half;
    0.42 - 0.5 * phase.cos() + 0.08 * (2.0 * phase).cos()
}

#[cfg(test)]
mod tests;
