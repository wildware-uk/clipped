//! What a waveform *is*: buckets of minimum and maximum sample value, at
//! several resolutions.
//!
//! # Why buckets, and why this size
//!
//! A waveform is drawn at some pixel width. Storing one summary per sample
//! would be storing the audio again — 48,000 values a second where a 1920-pixel
//! editor can show at most 1920 of them — so the audio is reduced to the
//! smallest thing a drawing needs: for each slice of time, how far the signal
//! went in each direction. Minimum *and* maximum rather than a single
//! magnitude, because an asymmetric waveform is a real thing and drawing it as
//! a mirror image is a lie about the recording.
//!
//! [`BASE_BUCKET`] is 10 milliseconds. At 1920 pixels that is 19.2 seconds of
//! audio at one bucket per pixel, which covers the trim editor's working zoom
//! (SPEC.md section 19 asks for trim, split and fades, not sample editing), and
//! it is finer than one frame of 60 fps video, so a cut aligned to a video frame
//! can always be placed against a bucket boundary. Zooming closer than that
//! stretches each bucket over more than one pixel rather than showing more
//! detail. The cost of halving it is exactly double the bytes below, and
//! [`Waveform`](crate::Waveform) records its base bucket in the file
//! (`docs/waveforms.md`) so that changing this is a format version rather than a
//! cache that silently means something else.
//!
//! # Why several resolutions
//!
//! Storing one resolution makes zooming out wrong or slow: drawing a 200-pixel
//! overview of a three-hour recording from 10-millisecond buckets means reading
//! and reducing 1.08 million of them every time the view moves. So each track
//! carries a pyramid — level 0 is [`BASE_BUCKET`], and each level above it is
//! two buckets of the level below merged into one — down to a level that is at
//! most [`OVERVIEW_BUCKETS`] buckets long. Merging minima and maxima is exact
//! (the maximum of two maxima *is* the maximum of the union), so a coarse level
//! is not an approximation of the fine one, it is the same answer at a coarser
//! grid.
//!
//! A geometric series doubles the storage and no more:
//!
//! ```text
//! level 0    10 ms    6,000 buckets per minute
//! level 1    20 ms    3,000
//! level 2    40 ms    1,500
//!  ...
//!            total  < 12,000 buckets per minute per track
//! ```
//!
//! # What that costs
//!
//! Two bytes per bucket — [`Peak`] is a minimum and a maximum, each one signed
//! 8-bit — so **under 24 kB per minute per audio track**, of which 12 kB is
//! level 0. A one-hour recording with three audio tracks (SPEC.md section 11:
//! game, microphone, other system audio) is about 4.2 MB of cache.
//!
//! Eight bits of amplitude is chosen against the drawing, not against the
//! audio. The editor mock-up in SPEC.md section 19 gives an audio track on the
//! order of a hundred pixels of height; at 128 steps per direction the
//! quantisation error is under half a pixel at that size. Quantising rounds
//! outwards — minima down, maxima up — so a drawn waveform is never smaller
//! than the audio it came from, which matters when somebody is looking for the
//! quiet start of a sound to cut on.

use core::fmt;
use core::num::NonZeroUsize;
use core::ops::Range;
use core::time::Duration;

/// The finest resolution a waveform is stored at.
///
/// See the module documentation for why 10 milliseconds.
pub const BASE_BUCKET: Duration = Duration::from_millis(10);

/// How short the coarsest level of the pyramid is allowed to get.
///
/// The pyramid stops once a level would fit in this many buckets, because a
/// level coarser than an overview of the whole recording answers no question
/// anybody asks. 128 is roughly the narrowest a timeline is ever drawn.
pub const OVERVIEW_BUCKETS: usize = 128;

/// The most base-resolution buckets one track may hold, which is eight hours of
/// audio.
///
/// A bound rather than a guess: the accumulator holds two `f32` per bucket while
/// it works, so an unbounded one turns a corrupt or hostile duration into
/// however much memory the file claims. Eight hours is far beyond a recording
/// Clipped produces — a session ends when the game does — and analysing
/// something longer fails with a reason rather than by exhausting the machine.
pub const MAX_BASE_BUCKETS: usize = 8 * 60 * 60 * 1_000 / 10;

/// The scale a sample value is quantised onto.
///
/// 127 rather than 128 so that full scale in both directions is representable:
/// a signal that reaches +1.0 and -1.0 becomes +127 and -127, and -128 is
/// reserved for the one-sided full scale a signed 16-bit sample can actually
/// hold.
const FULL_SCALE: f32 = 127.0;

/// Nanoseconds in a second, for turning a bucket duration into samples.
const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// How far the signal went in each direction over one slice of time.
///
/// Both values are the sample value scaled to ±127 (see [`FULL_SCALE`]).
/// [`Peak::SILENT`] is the value of a slice with no audio in it, and of a slice
/// that lies outside the recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Peak {
    minimum: i8,
    maximum: i8,
}

impl Peak {
    /// Silence: the signal went nowhere in either direction.
    pub const SILENT: Self = Self {
        minimum: 0,
        maximum: 0,
    };

    /// A peak from an already-quantised pair.
    ///
    /// The arguments are ordered if they arrive the wrong way round, so a
    /// corrupt cache file cannot produce a peak that draws inside out.
    #[must_use]
    pub fn new(minimum: i8, maximum: i8) -> Self {
        if minimum <= maximum {
            Self { minimum, maximum }
        } else {
            Self {
                minimum: maximum,
                maximum: minimum,
            }
        }
    }

    /// The lowest sample value in the slice, scaled to ±127.
    #[must_use]
    pub fn minimum(self) -> i8 {
        self.minimum
    }

    /// The highest sample value in the slice, scaled to ±127.
    #[must_use]
    pub fn maximum(self) -> i8 {
        self.maximum
    }

    /// The larger of the two excursions, as a fraction of full scale.
    ///
    /// What a drawing that shows one bar per bucket wants, rather than the two
    /// values a drawing that shows a filled envelope wants.
    #[must_use]
    pub fn amplitude(self) -> f32 {
        let low = f32::from(self.minimum).abs();
        let high = f32::from(self.maximum).abs();
        low.max(high) / FULL_SCALE
    }

    /// This peak and `other`, as one slice.
    ///
    /// Exact rather than approximate, which is what makes a coarse pyramid level
    /// the same answer as the fine one rather than a summary of it.
    #[must_use]
    pub fn merged(self, other: Self) -> Self {
        Self {
            minimum: self.minimum.min(other.minimum),
            maximum: self.maximum.max(other.maximum),
        }
    }

    /// Quantises a minimum, rounding away from zero so that the drawn waveform
    /// is never smaller than the audio.
    fn quantise_minimum(value: f32) -> i8 {
        clamp_to_i8((value.clamp(-1.0, 1.0) * FULL_SCALE).floor())
    }

    /// Quantises a maximum, rounding away from zero for the same reason.
    fn quantise_maximum(value: f32) -> i8 {
        clamp_to_i8((value.clamp(-1.0, 1.0) * FULL_SCALE).ceil())
    }
}

/// Narrows a value already known to be within ±127 to an `i8`.
#[allow(clippy::cast_possible_truncation)]
fn clamp_to_i8(value: f32) -> i8 {
    value.clamp(-127.0, 127.0) as i8
}

/// One resolution of one track's pyramid.
#[derive(Clone)]
pub(crate) struct Level {
    bucket_nanos: u64,
    peaks: Vec<Peak>,
}

impl Level {
    pub(crate) fn new(bucket_nanos: u64, peaks: Vec<Peak>) -> Self {
        Self {
            bucket_nanos,
            peaks,
        }
    }

    pub(crate) fn bucket_nanos(&self) -> u64 {
        self.bucket_nanos
    }

    pub(crate) fn peaks(&self) -> &[Peak] {
        &self.peaks
    }
}

impl fmt::Debug for Level {
    /// Reports the shape rather than the contents: a level is up to a million
    /// peaks, and a log line or a test failure holding all of them is unusable.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Level")
            .field("bucket_nanos", &self.bucket_nanos)
            .field("buckets", &self.peaks.len())
            .finish()
    }
}

/// Builds the pyramid above a base level.
///
/// The base level is included as level 0, so the result is never empty.
pub(crate) fn build_levels(base: Vec<Peak>, base_bucket: Duration) -> Vec<Level> {
    let mut bucket_nanos = u64::try_from(base_bucket.as_nanos()).unwrap_or(u64::MAX);
    let mut levels = vec![Level::new(bucket_nanos, base)];

    while levels
        .last()
        .is_some_and(|level| level.peaks.len() > OVERVIEW_BUCKETS)
    {
        let previous = levels.last().expect("just checked there is one");
        let reduced = previous
            .peaks
            .chunks(2)
            .map(|pair| {
                pair.iter()
                    .copied()
                    .reduce(Peak::merged)
                    .unwrap_or_default()
            })
            .collect();
        bucket_nanos = bucket_nanos.saturating_mul(2);
        levels.push(Level::new(bucket_nanos, reduced));
    }

    levels
}

/// Reads peaks for a time range out of a pyramid, at whatever resolution the
/// caller is drawing at.
///
/// `buckets` is how many the caller wants back — in practice the pixel width of
/// the track it is drawing. The coarsest level whose buckets are no wider than
/// one output bucket is used, so an overview of a long recording reads a few
/// hundred values rather than a million, and a close zoom still reads level 0.
///
/// Time outside the recording answers [`Peak::SILENT`] rather than being an
/// error or a shorter result: a timeline drawing a fixed width should not have
/// to special-case the end of the file.
pub(crate) fn read_levels(
    levels: &[Level],
    range: Range<Duration>,
    buckets: NonZeroUsize,
) -> Vec<Peak> {
    let buckets = buckets.get();
    let start = range.start.as_nanos();
    let span = range.end.as_nanos().saturating_sub(start);
    if span == 0 || levels.is_empty() {
        return vec![Peak::SILENT; buckets];
    }

    let target = span / buckets as u128;
    let level = levels
        .iter()
        .rev()
        .find(|level| u128::from(level.bucket_nanos) <= target)
        .unwrap_or(&levels[0]);
    let bucket_nanos = u128::from(level.bucket_nanos.max(1));

    (0..buckets)
        .map(|index| {
            let from = start + span * index as u128 / buckets as u128;
            let to = start + span * (index as u128 + 1) / buckets as u128;
            let first = from / bucket_nanos;
            // At least one source bucket per output bucket, so that zooming in
            // past level 0 repeats a bucket rather than returning silence.
            let last = (to.div_ceil(bucket_nanos)).max(first + 1);
            slice(level.peaks(), first, last)
                .iter()
                .copied()
                .reduce(Peak::merged)
                .unwrap_or(Peak::SILENT)
        })
        .collect()
}

/// The peaks between two bucket indices, clamped to what the level holds.
fn slice(peaks: &[Peak], first: u128, last: u128) -> &[Peak] {
    let length = peaks.len() as u128;
    let first = usize::try_from(first.min(length)).unwrap_or(peaks.len());
    let last = usize::try_from(last.clamp(first as u128, length)).unwrap_or(peaks.len());
    &peaks[first..last]
}

/// Accumulates decoded samples into base-resolution buckets.
///
/// Samples arrive per channel and per decoded frame, in whatever order the
/// decoder produces them, and are merged by position: a track's channels are
/// summarised together, so a sound that is hard-panned to the left is as visible
/// in the waveform as one in the middle. Merging is order-independent, which is
/// what lets each channel be pushed separately.
///
/// Gaps are silence. A track that starts a second into the recording, or one
/// with a hole in it, produces buckets of [`Peak::SILENT`] there rather than
/// having its audio slide to the left of where it belongs.
#[derive(Debug)]
pub(crate) struct PeakAccumulator {
    samples_per_bucket: u64,
    /// Minimum and maximum per bucket, kept unquantised until the end so that
    /// merging never accumulates rounding error.
    ///
    /// A bucket nothing has been written to holds `(+∞, -∞)` — an impossible
    /// pair, which is how [`finish`](Self::finish) tells a gap from audio that
    /// happens to sit at zero. Initialising to `(0.0, 0.0)` instead would put a
    /// floor of zero under every minimum, so a bucket holding only positive
    /// samples would be drawn from the axis rather than where the signal
    /// actually is.
    buckets: Vec<(f32, f32)>,
    /// One past the highest sample position written, which is the track's
    /// length in samples.
    length: u64,
}

/// What an unwritten bucket holds. See [`PeakAccumulator::buckets`].
const EMPTY_BUCKET: (f32, f32) = (f32::INFINITY, f32::NEG_INFINITY);

/// The analysed audio is longer than [`MAX_BASE_BUCKETS`] allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TooLong;

impl PeakAccumulator {
    /// An accumulator for a track at `sample_rate`.
    ///
    /// The bucket is a whole number of samples, rounded to nearest and never
    /// zero, so a sample rate that is not a multiple of 100 — 44,100 is, 22,050
    /// is not — gives a bucket a fraction of a per cent away from
    /// [`BASE_BUCKET`] rather than a drifting one.
    pub(crate) fn new(sample_rate: u32, bucket: Duration) -> Self {
        let nanos = u64::try_from(bucket.as_nanos()).unwrap_or(u64::MAX);
        let per_bucket = u64::from(sample_rate)
            .saturating_mul(nanos)
            .saturating_add(NANOS_PER_SECOND / 2)
            / NANOS_PER_SECOND;
        Self {
            samples_per_bucket: per_bucket.max(1),
            buckets: Vec::new(),
            length: 0,
        }
    }

    /// How many samples one bucket covers.
    ///
    /// Only the tests ask: the production path works in sample positions and
    /// lets [`add_run`](Self::add_run) do the arithmetic.
    #[cfg(test)]
    pub(crate) fn samples_per_bucket(&self) -> u64 {
        self.samples_per_bucket
    }

    /// Merges one channel's samples, starting at sample position `start`.
    ///
    /// Called once per channel per decoded frame with the same `start`.
    pub(crate) fn add_run(&mut self, start: u64, samples: &[f32]) -> Result<(), TooLong> {
        let mut position = start;
        let mut rest = samples;

        while !rest.is_empty() {
            let bucket = position / self.samples_per_bucket;
            let remaining_in_bucket = (bucket + 1) * self.samples_per_bucket - position;
            let take = usize::try_from(remaining_in_bucket)
                .unwrap_or(usize::MAX)
                .min(rest.len());
            let (head, tail) = rest.split_at(take);
            self.merge(bucket, head)?;
            position += take as u64;
            rest = tail;
        }

        self.length = self.length.max(position);
        Ok(())
    }

    /// Merges `values`, all of which fall inside `bucket`.
    fn merge(&mut self, bucket: u64, values: &[f32]) -> Result<(), TooLong> {
        let index = usize::try_from(bucket).map_err(|_| TooLong)?;
        if index >= MAX_BASE_BUCKETS {
            return Err(TooLong);
        }
        if index >= self.buckets.len() {
            self.buckets.resize(index + 1, EMPTY_BUCKET);
        }

        let entry = &mut self.buckets[index];
        for &value in values {
            // NaN cannot come out of an integer sample format and should not
            // come out of a float one, but a corrupt file is a thing: `min` and
            // `max` on `f32` return the other operand for NaN, so a NaN sample
            // leaves the bucket alone instead of poisoning it.
            entry.0 = entry.0.min(value);
            entry.1 = entry.1.max(value);
        }
        Ok(())
    }

    /// How many samples were accumulated, which is the track's length.
    pub(crate) fn length_in_samples(&self) -> u64 {
        self.length
    }

    /// Quantises what was accumulated into the base level.
    pub(crate) fn finish(self) -> Vec<Peak> {
        self.buckets
            .into_iter()
            .map(|(minimum, maximum)| {
                if minimum > maximum {
                    // Nothing was ever written here: a gap in the track, or
                    // audio that has not started yet.
                    Peak::SILENT
                } else {
                    Peak {
                        minimum: Peak::quantise_minimum(minimum),
                        maximum: Peak::quantise_maximum(maximum),
                    }
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peak(minimum: i8, maximum: i8) -> Peak {
        Peak::new(minimum, maximum)
    }

    #[test]
    fn quantising_rounds_outwards_so_a_drawing_never_understates_the_audio() {
        // 0.5 of full scale is 63.5 steps. A minimum becomes -64 and a maximum
        // 64: both further from zero than the true value, never nearer.
        assert_eq!(Peak::quantise_minimum(-0.5), -64);
        assert_eq!(Peak::quantise_maximum(0.5), 64);
        // A sample too quiet to reach one step still reaches one step.
        assert_eq!(Peak::quantise_maximum(0.001), 1);
        assert_eq!(Peak::quantise_minimum(-0.001), -1);
        // Digital silence is silence, not a half step.
        assert_eq!(Peak::quantise_maximum(0.0), 0);
        assert_eq!(Peak::quantise_minimum(0.0), 0);
        // Full scale, and beyond it, stay on the scale.
        assert_eq!(Peak::quantise_maximum(1.0), 127);
        assert_eq!(Peak::quantise_minimum(-1.0), -127);
        assert_eq!(Peak::quantise_maximum(9.0), 127);
        assert_eq!(Peak::quantise_minimum(-9.0), -127);
    }

    #[test]
    fn a_peak_that_arrives_inside_out_is_ordered_rather_than_drawn_inside_out() {
        assert_eq!(Peak::new(40, -40), Peak::new(-40, 40));
    }

    #[test]
    fn amplitude_is_the_larger_excursion() {
        assert!((peak(-127, 10).amplitude() - 1.0).abs() < f32::EPSILON);
        assert!((peak(-10, 127).amplitude() - 1.0).abs() < f32::EPSILON);
        assert!(peak(0, 0).amplitude().abs() < f32::EPSILON);
    }

    #[test]
    fn merging_takes_the_union_rather_than_an_average() {
        assert_eq!(peak(-10, 20).merged(peak(-30, 5)), peak(-30, 20));
    }

    #[test]
    fn runs_are_bucketed_by_sample_position() {
        // 100 samples a second, 10 ms buckets: one sample per bucket.
        let mut accumulator = PeakAccumulator::new(100, BASE_BUCKET);
        assert_eq!(accumulator.samples_per_bucket(), 1);
        accumulator.add_run(0, &[1.0, -1.0, 0.0]).expect("short");
        assert_eq!(accumulator.length_in_samples(), 3);
        // Each bucket holds one sample, so its minimum and maximum are that
        // sample: the accumulator reports where the signal was, not a band
        // drawn from the axis.
        assert_eq!(
            accumulator.finish(),
            vec![peak(127, 127), peak(-127, -127), peak(0, 0)]
        );
    }

    #[test]
    fn a_bucket_of_only_positive_samples_keeps_its_true_minimum() {
        // A bucket sitting entirely above the axis — a DC offset, or a very
        // short bucket in the rising part of a wave — is drawn where it is,
        // rather than as a band from zero.
        let mut accumulator = PeakAccumulator::new(200, BASE_BUCKET);
        accumulator.add_run(0, &[0.5, 0.75]).expect("short");
        let finished = accumulator.finish();
        assert_eq!(finished, vec![peak(63, 96)]);
    }

    #[test]
    fn channels_are_merged_rather_than_averaged() {
        // Two channels of the same frame, pushed separately with the same start
        // position. A sound only in the second channel has to survive.
        let mut accumulator = PeakAccumulator::new(400, BASE_BUCKET);
        assert_eq!(accumulator.samples_per_bucket(), 4);
        accumulator
            .add_run(0, &[0.0, 0.0, 0.0, 0.0])
            .expect("short");
        accumulator
            .add_run(0, &[0.0, 0.9, 0.0, 0.0])
            .expect("short");
        let finished = accumulator.finish();
        assert_eq!(finished.len(), 1);
        assert_eq!(finished[0].maximum(), 115);
    }

    #[test]
    fn a_gap_is_silence_rather_than_a_shift() {
        let mut accumulator = PeakAccumulator::new(100, BASE_BUCKET);
        accumulator.add_run(0, &[0.5]).expect("short");
        // Nothing at positions 1 and 2; audio resumes at 3.
        accumulator.add_run(3, &[-0.5]).expect("short");
        let finished = accumulator.finish();
        assert_eq!(finished.len(), 4);
        assert_eq!(finished[1], Peak::SILENT);
        assert_eq!(finished[2], Peak::SILENT);
        assert_eq!(finished[3].minimum(), -64);
    }

    #[test]
    fn a_run_that_spans_buckets_is_split_at_the_boundary() {
        let mut accumulator = PeakAccumulator::new(200, BASE_BUCKET);
        assert_eq!(accumulator.samples_per_bucket(), 2);
        accumulator
            .add_run(1, &[0.25, 0.5, 0.75, 1.0])
            .expect("short");
        let finished = accumulator.finish();
        // Positions 1 | 2,3 | 4.
        assert_eq!(finished.len(), 3);
        assert_eq!(finished[0].maximum(), 32);
        assert_eq!(finished[1].maximum(), 96);
        assert_eq!(finished[2].maximum(), 127);
    }

    #[test]
    fn audio_beyond_the_bound_is_refused_rather_than_allocated_for() {
        let mut accumulator = PeakAccumulator::new(100, BASE_BUCKET);
        let start = MAX_BASE_BUCKETS as u64;
        assert_eq!(accumulator.add_run(start, &[0.5]), Err(TooLong));
        // And the bucket immediately below it is still accepted, so the bound is
        // where it says it is.
        assert_eq!(accumulator.add_run(start - 1, &[0.5]), Ok(()));
    }

    #[test]
    fn a_bucket_is_a_whole_number_of_samples_at_an_awkward_sample_rate() {
        // 22,050 Hz: 10 ms is 220.5 samples, so 220 or 221 and not something
        // that drifts.
        let accumulator = PeakAccumulator::new(22_050, BASE_BUCKET);
        assert_eq!(accumulator.samples_per_bucket(), 221);
        // And a rate so low that a bucket would round to nothing still has one.
        assert_eq!(PeakAccumulator::new(1, BASE_BUCKET).samples_per_bucket(), 1);
    }

    #[test]
    fn the_pyramid_halves_until_it_is_an_overview() {
        let base = vec![peak(-1, 1); OVERVIEW_BUCKETS * 4];
        let levels = build_levels(base, BASE_BUCKET);
        let shape: Vec<_> = levels
            .iter()
            .map(|level| (level.bucket_nanos(), level.peaks().len()))
            .collect();
        assert_eq!(
            shape,
            vec![(10_000_000, 512), (20_000_000, 256), (40_000_000, 128),]
        );
    }

    #[test]
    fn a_level_the_pyramid_built_holds_the_loudest_of_what_it_merged() {
        // A single loud bucket in a long quiet track. Every level above the base
        // has to carry it, at the index it belongs to: a reduction that took one
        // of each pair rather than merging them would lose it here, and an
        // overview drawn from a coarse level would show a quiet recording.
        let mut base = vec![peak(-1, 1); 400];
        base[7] = peak(-120, 120);
        let levels = build_levels(base, BASE_BUCKET);

        assert_eq!(levels.len(), 3, "{levels:?}");
        assert_eq!(levels[1].peaks()[3], peak(-120, 120));
        assert_eq!(levels[2].peaks()[1], peak(-120, 120));
        // And nowhere else.
        assert_eq!(levels[1].peaks()[4], peak(-1, 1));
        assert_eq!(levels[2].peaks()[2], peak(-1, 1));
    }

    #[test]
    fn a_coarse_level_is_the_union_of_the_fine_one_rather_than_an_average() {
        let base = vec![peak(-4, 4), peak(-40, 40), peak(0, 0), peak(-2, 2)];
        let levels = build_levels(base, BASE_BUCKET);
        // Short enough that no level is built above it, so reduce by hand.
        assert_eq!(levels.len(), 1);
        let reduced: Vec<_> = levels[0]
            .peaks()
            .chunks(2)
            .map(|pair| pair.iter().copied().reduce(Peak::merged).expect("a pair"))
            .collect();
        assert_eq!(reduced, vec![peak(-40, 40), peak(-2, 2)]);
    }

    #[test]
    fn a_narrow_read_uses_the_fine_level_and_a_wide_one_uses_a_coarse_level() {
        // One second of audio: 100 base buckets, with a spike in the last one.
        let mut base = vec![Peak::SILENT; 100];
        base[99] = peak(-100, 100);
        let levels = build_levels(base, BASE_BUCKET);

        // A ten-bucket overview of the whole second still shows the spike, in
        // the last bucket.
        let overview = read_levels(
            &levels,
            Duration::ZERO..Duration::from_secs(1),
            NonZeroUsize::new(10).expect("ten"),
        );
        assert_eq!(overview.len(), 10);
        assert_eq!(overview[9], peak(-100, 100));
        assert!(overview[..9].iter().all(|peak| *peak == Peak::SILENT));

        // And a read of the last 50 ms places it in the last of five.
        let zoomed = read_levels(
            &levels,
            Duration::from_millis(950)..Duration::from_secs(1),
            NonZeroUsize::new(5).expect("five"),
        );
        assert_eq!(zoomed.len(), 5);
        assert_eq!(zoomed[4], peak(-100, 100));
        assert!(zoomed[..4].iter().all(|peak| *peak == Peak::SILENT));
    }

    #[test]
    fn reading_past_the_end_is_silence_rather_than_a_short_answer() {
        let levels = build_levels(vec![peak(-10, 10); 10], BASE_BUCKET);
        let read = read_levels(
            &levels,
            Duration::ZERO..Duration::from_secs(1),
            NonZeroUsize::new(20).expect("twenty"),
        );
        assert_eq!(read.len(), 20);
        // 10 buckets is 100 ms of a 1 second window: the first two output
        // buckets hold it, the rest are past the end.
        assert_eq!(read[0], peak(-10, 10));
        assert!(read[3..].iter().all(|peak| *peak == Peak::SILENT));
    }

    #[test]
    fn an_empty_range_is_answered_with_silence_rather_than_a_panic() {
        let levels = build_levels(vec![peak(-10, 10); 10], BASE_BUCKET);
        let read = read_levels(
            &levels,
            Duration::from_secs(1)..Duration::from_secs(1),
            NonZeroUsize::new(4).expect("four"),
        );
        assert_eq!(read, vec![Peak::SILENT; 4]);
    }

    #[test]
    fn zooming_closer_than_the_base_bucket_repeats_it_rather_than_showing_silence() {
        let levels = build_levels(vec![peak(-20, 20); 4], BASE_BUCKET);
        // 4 output buckets across a single 10 ms bucket.
        let read = read_levels(
            &levels,
            Duration::ZERO..Duration::from_millis(10),
            NonZeroUsize::new(4).expect("four"),
        );
        assert_eq!(read, vec![peak(-20, 20); 4]);
    }
}
