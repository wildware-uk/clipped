import type { PreviewTrack } from '@clipped/shared';

/**
 * The arithmetic behind every waveform this window draws: peaks in, an SVG path
 * out.
 *
 * A module of its own rather than an export beside the component, because it is
 * the only part of drawing a waveform that can be asserted on. jsdom lays
 * nothing out, so a component test can see that a path is on the screen and not
 * that it is the right shape — and the shape is the whole claim: a scale that
 * divided by the wrong number, or that lost the sign, draws a plausible
 * waveform of a recording that does not sound like that.
 *
 * It is shared by the **two** screens that draw peaks, and deliberately so
 * (issue #66). `Waveform.tsx` draws a whole recording under the player;
 * `editor/lanePeaks.ts` draws the part of a recording one segment of a clip
 * uses. Their geometry differs — one lane per *sound track of a file* against
 * one lane per *audio track of an edit*, laid out in output time, at a zoom, in
 * a scroller — but the numbers behind the picture are the same numbers, and a
 * second copy of them is how the two screens would start disagreeing about what
 * silence looks like.
 */

/** How tall one track's lane is drawn, in the SVG's own units. */
export const LANE = 40;

/** The middle of a lane, which is where a sample of zero sits. */
const MIDDLE = LANE / 2;

/**
 * The largest a peak can be, which is what the peaks are scaled against.
 *
 * `docs/waveforms.md`: minima and maxima are quantised to ±127, and 127 rather
 * than 128 is what full scale means in that quantisation.
 */
const FULL_SCALE = 127;

/** Nanoseconds in a second, which is the unit an edit document counts in. */
const NANOS_PER_SECOND = 1_000_000_000;

/**
 * A half-open run of a track's buckets, `[from, to)`.
 *
 * Bucket numbers rather than a time, because the peaks are the grid: a range
 * expressed in nanoseconds would have to be turned into buckets somewhere, and
 * doing it once — in {@link bucketsOver} — is what keeps the rounding rule in
 * one place.
 */
export interface BucketRange {
  /** The first bucket drawn. */
  readonly from: number;
  /** One past the last bucket drawn. */
  readonly to: number;
}

/** How many buckets of peaks `track` carries. */
export function bucketCount(track: PreviewTrack): number {
  return Math.floor((track.peaks?.length ?? 0) / 2);
}

/**
 * The buckets of `track` covering `[fromNanos, toNanos)` of the **recording**,
 * or `null` when that range holds no bucket at all.
 *
 * The peaks span the whole track — `PreviewTrack.duration_seconds` is what they
 * cover, the first bucket starting at zero — so a clip trimmed from the middle
 * of a recording draws a *slice* of them. Handing the whole array to a lane
 * that covers eight seconds of a two-hour file is the mistake this function
 * exists to make impossible, and it is invisible in a rendered test: a path
 * would be drawn either way.
 *
 * Rounding is **outwards** — the first bucket down, the last one up — which is
 * the rule `docs/waveforms.md` already applies to the peaks themselves: a drawn
 * waveform is never smaller than the audio it came from, which matters when
 * somebody is hunting for the quiet start of a sound to cut on. The cost is
 * that a piece may draw up to one bucket either side of its material, stretched
 * to the piece's own width.
 *
 * `null` for a track with no peaks, one whose duration the recorder reported as
 * nothing, a backwards range, and a range that falls outside the track
 * entirely — a segment pointing past the end of the recording it names. Every
 * one of those is a lane with nothing to draw rather than an error, and none of
 * them is a flat line.
 */
export function bucketsOver(
  track: PreviewTrack,
  fromNanos: number,
  toNanos: number,
): BucketRange | null {
  const buckets = bucketCount(track);
  const durationNanos = track.duration_seconds * NANOS_PER_SECOND;
  if (buckets === 0 || durationNanos <= 0 || toNanos <= fromNanos) {
    return null;
  }

  const perBucket = durationNanos / buckets;
  const from = clamp(Math.floor(fromNanos / perBucket), buckets);
  const to = clamp(Math.ceil(toNanos / perBucket), buckets);
  return from >= to ? null : { from, to };
}

/** `value` held inside `[0, buckets]`, which is where a bucket boundary lives. */
function clamp(value: number, buckets: number): number {
  return Math.min(Math.max(value, 0), buckets);
}

/**
 * The outline of one track, as an SVG path, or `null` when there is nothing to
 * draw.
 *
 * Exported because it is the whole of the arithmetic and the only part of this
 * file worth asserting on: a component test in jsdom can see that a path exists
 * and not that it is the right shape.
 *
 * The path runs left to right along the maxima and back right to left along the
 * minima, which closes into one filled shape per track rather than two strokes
 * that have to be kept in step. Peaks are interleaved — minimum then maximum —
 * so bucket `n` is at `peaks[2n]` and `peaks[2n + 1]`
 * (`clipped_ipc::PreviewTrack`).
 *
 * A peak of ±127 reaches the edge of the lane and a peak of zero sits on its
 * middle. **Nothing is clamped upward**: a bucket whose sound was silent is
 * drawn on the middle line as part of the outline, which is not the same as a
 * whole track drawn as a line — the shape either side of it is what says so.
 *
 * `range` draws a **part** of the track, for a clip that uses part of a
 * recording. The path is re-based on the range, so its x runs from zero to
 * `to - from` and the caller's `viewBox` is the number of buckets it asked for
 * rather than the number the track has. Left out, the whole track is drawn,
 * which is what the playback screen wants.
 */
export function envelope(track: PreviewTrack, range?: BucketRange): string | null {
  const peaks = track.peaks ?? [];
  const whole = { from: 0, to: Math.floor(peaks.length / 2) };
  const { from, to } = range ?? whole;
  if (to <= from) {
    return null;
  }

  const y = (sample: number): number =>
    // Negative samples go *down* the lane, so the sign is inverted: SVG's y
    // grows downward and a waveform's minimum is drawn below its middle.
    MIDDLE - (Math.max(-FULL_SCALE, Math.min(FULL_SCALE, sample)) / FULL_SCALE) * MIDDLE;

  const top: string[] = [];
  const bottom: string[] = [];
  for (let bucket = from; bucket < to; bucket += 1) {
    const minimum = peaks[bucket * 2] ?? 0;
    const maximum = peaks[bucket * 2 + 1] ?? 0;
    const x = bucket - from;
    top.push(`${x},${round(y(maximum))}`);
    bottom.push(`${x},${round(y(minimum))}`);
  }
  bottom.reverse();

  return `M${top.join(' L')} L${bottom.join(' L')} Z`;
}

/** Two decimal places, which is finer than any display draws. */
function round(value: number): number {
  return Math.round(value * 100) / 100;
}
