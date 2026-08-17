import type { PreviewTrack } from '@clipped/shared';

/**
 * The arithmetic behind `Waveform.tsx`: peaks in, an SVG path out.
 *
 * A module of its own rather than an export beside the component, because it is
 * the only part of drawing a waveform that can be asserted on. jsdom lays
 * nothing out, so a component test can see that a path is on the screen and not
 * that it is the right shape — and the shape is the whole claim: a scale that
 * divided by the wrong number, or that lost the sign, draws a plausible
 * waveform of a recording that does not sound like that.
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
 */
export function envelope(track: PreviewTrack): string | null {
  const peaks = track.peaks ?? [];
  const buckets = Math.floor(peaks.length / 2);
  if (buckets === 0) {
    return null;
  }

  const y = (sample: number): number =>
    // Negative samples go *down* the lane, so the sign is inverted: SVG's y
    // grows downward and a waveform's minimum is drawn below its middle.
    MIDDLE - (Math.max(-FULL_SCALE, Math.min(FULL_SCALE, sample)) / FULL_SCALE) * MIDDLE;

  const top: string[] = [];
  const bottom: string[] = [];
  for (let bucket = 0; bucket < buckets; bucket += 1) {
    const minimum = peaks[bucket * 2] ?? 0;
    const maximum = peaks[bucket * 2 + 1] ?? 0;
    top.push(`${bucket},${round(y(maximum))}`);
    bottom.push(`${bucket},${round(y(minimum))}`);
  }
  bottom.reverse();

  return `M${top.join(' L')} L${bottom.join(' L')} Z`;
}

/** Two decimal places, which is finer than any display draws. */
function round(value: number): number {
  return Math.round(value * 100) / 100;
}
