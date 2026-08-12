import { describe, expect, it } from 'vitest';

import { storedDocument } from '../test/editDocumentFixture';
import { readEditDocument, type EditDocument, type Segment } from './document';
import {
  anySoloed,
  boundaries,
  formatTickLabel,
  formatTimecode,
  locate,
  nextBoundary,
  outputNanosOf,
  overlaysAt,
  previousBoundary,
  scrollToShow,
  ticks,
  tickIntervalNanos,
  totalOutputNanos,
  trackOutput,
} from './timeline';

/**
 * The timeline arithmetic, held to the figures `crates/edit` is held to.
 *
 * This file exists because the answer has to be the same on both sides. The
 * crate's own `timeline.rs` tests assert that eight seconds into this clip is
 * ninety-two seconds into the recording, that the material a cut removed is not
 * reachable, and that the end of the clip is past its end; the same assertions
 * are made here against the port. A test written from the port's own output
 * would prove only that it is consistent with itself, which is exactly the
 * defect that would put a preview a few frames away from an export.
 */

/** The fixture, read: the three-segment clip in `docs/editing.md`. */
function fixture(changes: Record<string, unknown> = {}): EditDocument {
  const read = readEditDocument(storedDocument(changes));
  if (!read.ok) {
    throw new Error(`the fixture should read: ${read.problem}`);
  }
  return read.document;
}

/** One segment of one source, at one speed. */
function segment(
  source: number,
  start: number,
  end: number,
  speed = { numerator: 1, denominator: 1 },
): Segment {
  return { source, span: { start, end }, speed, crop: null, rotation: 'none' };
}

/** The element at `index`, or a failure rather than an assertion on `undefined`. */
function only<T>(list: readonly T[], index: number): T {
  const found = list[index];
  if (found === undefined) {
    throw new Error(`the fixture has no element ${String(index)}`);
  }
  return found;
}

/** A document made of `segments` and nothing else. */
function clipOf(segments: readonly Segment[]): EditDocument {
  return {
    schema_version: 1,
    title: 'test',
    aspect_ratio: null,
    sources: [{ id: 0, recording: 'rec' }],
    segments,
    audio_tracks: [],
    overlays: [],
  };
}

describe('the length of a clip', () => {
  it('is its segments added up', () => {
    expect(totalOutputNanos(fixture())).toBe(24_000_000_000);
  });

  it('is nothing at all for an empty document, which is a valid one', () => {
    expect(totalOutputNanos(clipOf([]))).toBe(0);
  });

  it('is unreadable rather than wrong when a segment has a zero in its speed', () => {
    const broken = clipOf([segment(0, 0, 1_000_000_000, { numerator: 1, denominator: 0 })]);

    expect(outputNanosOf(only(broken.segments, 0))).toBeNull();
    expect(totalOutputNanos(broken)).toBeNull();
    expect(boundaries(broken)).toBeNull();
  });
});

describe('locating a position', () => {
  it('maps a position in the first segment straight into the recording', () => {
    // The crate's `a_position_inside_the_first_segment_maps_straight_into_the_recording`.
    expect(locate(fixture(), 2_000_000_000)).toEqual({
      segment: 0,
      source: 0,
      sourceNanos: 32_000_000_000,
      segmentStartNanos: 0,
    });
  });

  it('does not reach the material a cut removed', () => {
    // The crate's `the_material_a_cut_removed_is_not_reachable_from_the_output`,
    // which is the assertion that a cut is a cut: everything from 38s to 92s of
    // the recording is gone, and the frame after the cut is the one at 92s.
    const clip = fixture();

    expect(locate(clip, 7_999_999_999)).toMatchObject({
      segment: 0,
      sourceNanos: 37_999_999_999,
    });
    expect(locate(clip, 8_000_000_000)).toMatchObject({
      segment: 1,
      sourceNanos: 92_000_000_000,
    });
  });

  it('finds the other recording in a later segment', () => {
    expect(locate(fixture(), 21_000_000_000)).toEqual({
      segment: 2,
      source: 1,
      sourceNanos: 6_000_000_000,
      segmentStartNanos: 20_000_000_000,
    });
  });

  it('ends where the clip does, because every range is half-open', () => {
    const clip = fixture();

    expect(locate(clip, 23_999_999_999)).not.toBeNull();
    expect(locate(clip, 24_000_000_000)).toBeNull();
    expect(locate(clipOf([]), 0)).toBeNull();
  });

  it('stretches output time for a slowed segment without moving the material', () => {
    // Four seconds of material at half speed is eight seconds of output, and
    // half way through the output is a quarter of the way through the material.
    const clip = clipOf([
      segment(0, 10_000_000_000, 14_000_000_000, { numerator: 1, denominator: 2 }),
    ]);

    expect(totalOutputNanos(clip)).toBe(8_000_000_000);
    expect(locate(clip, 4_000_000_000)).toMatchObject({ sourceNanos: 12_000_000_000 });
    expect(locate(clip, 8_000_000_000)).toBeNull();
  });

  it('runs through the material faster for a sped-up segment', () => {
    const clip = clipOf([segment(0, 0, 12_000_000_000, { numerator: 3, denominator: 1 })]);

    expect(totalOutputNanos(clip)).toBe(4_000_000_000);
    expect(locate(clip, 1_000_000_000)).toMatchObject({ sourceNanos: 3_000_000_000 });
  });

  it('stays exact at the top of the range a document may hold, where a double does not', () => {
    /*
     * `docs/editing.md` chose nanoseconds partly because the editor is
     * JavaScript, which holds integers exactly only below 2^53 — a hundred and
     * four days of them. A *position* that size is exact; the multiplication by
     * a speed is not, and the second line below is the proof: computed as a
     * double, this lands one nanosecond past the answer `crates/edit` computes
     * in 128 bits.
     *
     * Ordinary clips are nowhere near this, and that is the reason the
     * arithmetic is in `BigInt` rather than left as doubles with a comment: a
     * call site cannot check whether it is in the range where the two happen to
     * agree, and a preview that rounds differently from the exporter is the
     * defect the shared arithmetic exists to prevent.
     */
    const clip = clipOf([segment(0, 0, 9_000_000_000_000_000, { numerator: 3, denominator: 2 })]);
    const at = 3_412_826_364_271_777;

    expect(locate(clip, at)).toMatchObject({ sourceNanos: 5_119_239_546_407_665 });
    expect(Math.trunc((at * 3) / 2)).toBe(5_119_239_546_407_666);
  });
});

describe('the boundaries a cut can be at', () => {
  it('are the start of every segment and the end of the clip', () => {
    expect(boundaries(fixture())).toEqual([0, 8_000_000_000, 20_000_000_000, 24_000_000_000]);
  });

  it('step to the one before and the one after, and stop at the ends', () => {
    const cuts = boundaries(fixture()) ?? [];

    expect(nextBoundary(cuts, 0)).toBe(8_000_000_000);
    expect(nextBoundary(cuts, 8_000_000_000)).toBe(20_000_000_000);
    expect(nextBoundary(cuts, 24_000_000_000)).toBe(24_000_000_000);
    expect(previousBoundary(cuts, 24_000_000_000)).toBe(20_000_000_000);
    expect(previousBoundary(cuts, 8_000_000_000)).toBe(0);
    expect(previousBoundary(cuts, 0)).toBe(0);
  });
});

describe('an audio track', () => {
  const track = (changes: Record<string, unknown>) => ({
    name: 'Game',
    inputs: [{ source: 0, stream: 0 }],
    gain_db: -3,
    muted: false,
    soloed: false,
    fade_in: 0,
    fade_out: 0,
    ...changes,
  });

  it('is heard at its own level when nothing is soloed', () => {
    expect(trackOutput(track({}), false)).toEqual({ audible: true, gainDb: -3 });
  });

  it('is silent when muted, whatever else is set', () => {
    expect(trackOutput(track({ muted: true }), false)).toEqual({ audible: false });
    // Mute wins over solo on the same track: soloing a muted track does not
    // unmute it, which is what every mixing desk does and what
    // `docs/editing.md` requires.
    expect(trackOutput(track({ muted: true, soloed: true }), true)).toEqual({ audible: false });
  });

  it('is silent when something else is soloed', () => {
    expect(trackOutput(track({}), true)).toEqual({ audible: false });
    expect(trackOutput(track({ soloed: true }), true)).toEqual({ audible: true, gainDb: -3 });
  });

  it('reports whether anything in the document is soloed at all', () => {
    expect(anySoloed(fixture())).toBe(false);

    const soloed = fixture({
      audio_tracks: [
        { name: 'Game', inputs: [{ source: 0, stream: 0 }], soloed: true },
        { name: 'Microphone', inputs: [{ source: 0, stream: 1 }] },
      ],
    });
    expect(anySoloed(soloed)).toBe(true);
    expect(trackOutput(only(soloed.audio_tracks, 1), anySoloed(soloed))).toEqual({
      audible: false,
    });
  });
});

describe('overlays', () => {
  it('are on screen for a half-open range of output time', () => {
    const clip = fixture();

    expect(overlaysAt(clip, 0).map((overlay) => overlay.text)).toEqual(['Round 12']);
    expect(overlaysAt(clip, 2_999_999_999)).toHaveLength(1);
    expect(overlaysAt(clip, 3_000_000_000)).toHaveLength(0);
  });
});

describe('a timecode', () => {
  it('reads as minutes, seconds and milliseconds', () => {
    expect(formatTimecode(0)).toBe('00:00.000');
    expect(formatTimecode(8_000_000_000)).toBe('00:08.000');
    expect(formatTimecode(92_500_000_000)).toBe('01:32.500');
  });

  it('grows an hours field only once a clip is that long', () => {
    expect(formatTimecode(3_599_000_000_000)).toBe('59:59.000');
    expect(formatTimecode(3_600_000_000_000)).toBe('1:00:00.000');
  });

  it('drops the milliseconds from a ruler mark that is a second or more apart', () => {
    expect(formatTickLabel(5_000_000_000, 1_000_000_000)).toBe('00:05');
    expect(formatTickLabel(5_500_000_000, 500_000_000)).toBe('00:05.500');
  });
});

describe('the ruler', () => {
  it('marks a short clip finely and a long one coarsely', () => {
    expect(tickIntervalNanos(2_000_000_000, 1)).toBe(250_000_000);
    expect(tickIntervalNanos(24_000_000_000, 1)).toBe(5_000_000_000);
    expect(tickIntervalNanos(3_600_000_000_000, 1)).toBe(600_000_000_000);
  });

  it('marks more finely as the timeline is zoomed in', () => {
    expect(tickIntervalNanos(24_000_000_000, 4)).toBe(1_000_000_000);
    expect(tickIntervalNanos(24_000_000_000, 16)).toBe(250_000_000);
  });

  it('runs from the start of the clip to its end', () => {
    const marks = ticks(24_000_000_000, 1);

    expect(marks[0]).toBe(0);
    expect(marks[marks.length - 1]).toBe(20_000_000_000);
    expect(marks).toHaveLength(5);
  });
});

describe('keeping the playhead on screen', () => {
  it('does not move a view the playhead is comfortably inside', () => {
    expect(scrollToShow(0.5, 2000, 1000, 400)).toBe(400);
  });

  it('centres a playhead that has left the view', () => {
    expect(scrollToShow(0.9, 2000, 1000, 0)).toBe(1000);
    expect(scrollToShow(0.1, 2000, 1000, 900)).toBe(0);
  });

  it('does not scroll at all when the whole clip fits', () => {
    expect(scrollToShow(0.9, 1000, 1000, 0)).toBe(0);
  });
});
