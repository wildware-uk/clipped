import { describe, expect, it } from 'vitest';

import { storedDocument } from '../test/editDocumentFixture';
import { readEditDocument, type AudioTrack, type EditDocument, type Segment } from './document';
import {
  boundaries,
  fadeAmplitude,
  formatTickLabel,
  formatTimecode,
  locate,
  monitor,
  monitoredAmplitudeAt,
  nextBoundary,
  outputNanosOf,
  overlaysAt,
  previousBoundary,
  resolve,
  scrollToShow,
  SOLO_NONE,
  ticks,
  tickIntervalNanos,
  toggleSolo,
  totalOutputNanos,
  trackAmplitudeAt,
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
    schema_version: 2,
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

/** One audio track, spread over whatever a case changes about it. */
function track(changes: Record<string, unknown> = {}): AudioTrack {
  return {
    name: 'Game',
    inputs: [{ source: 0, stream: 0 }],
    gain_db: -3,
    muted: false,
    fade_in: 0,
    fade_out: 0,
    ...changes,
  };
}

/**
 * `resolve`, `monitor` and `Solo`, held to the figures `crates/edit/src/audio.rs`
 * asserts of `resolve`, `monitor` and `Solo::toggled` — the port for the same
 * reason every other block in this file exists: a preview a few frames or a
 * few decibels away from an export is the defect a second implementation would
 * produce, and a test against the port's own output could not catch it.
 */
describe('an audio track', () => {
  it('is heard at its own level in the export, and in a preview soloing nothing', () => {
    expect(resolve(track())).toEqual({ audible: true, gainDb: -3 });
    expect(monitor(track(), 0, SOLO_NONE)).toEqual({ audible: true, gainDb: -3 });
  });

  it('is silent when muted, whatever else is set', () => {
    expect(resolve(track({ muted: true }))).toEqual({ audible: false });
    // Mute wins over solo on the same track: soloing a muted track does not
    // unmute it, which is what every mixing desk does and what
    // `docs/editing.md` requires.
    expect(monitor(track({ muted: true }), 0, 0)).toEqual({ audible: false });
  });

  it('is silent in the preview when another track is soloed, and never in the export', () => {
    const other = track();

    expect(monitor(other, 1, 0)).toEqual({ audible: false });
    // "the export is never given a solo": crates/edit's own words for it.
    expect(resolve(other)).toEqual({ audible: true, gainDb: -3 });
  });

  it('solo changes nothing when nothing is soloed, so the preview matches the export', () => {
    expect(monitor(track(), 2, SOLO_NONE)).toEqual(resolve(track()));
  });
});

describe('toggling a solo', () => {
  it('moves the solo to whichever track is pressed', () => {
    expect(toggleSolo(SOLO_NONE, 0)).toBe(0);
    expect(toggleSolo(0, 2)).toBe(2);
  });

  it('clears the solo when the soloed track is pressed again', () => {
    expect(toggleSolo(2, 2)).toBe(SOLO_NONE);
  });

  it('names one track, so two of them cannot be soloed at once', () => {
    // The whole reason solo is not a field on a track: pressing solo on a
    // second track moves it rather than adding to it.
    const moved = toggleSolo(toggleSolo(SOLO_NONE, 0), 2);

    expect(moved).toBe(2);
    expect(monitor(only([track(), track(), track()], 0), 0, moved)).toEqual({ audible: false });
    expect(monitor(only([track(), track(), track()], 2), 2, moved)).toEqual({
      audible: true,
      gainDb: -3,
    });
  });
});

describe('the fade envelope of a track', () => {
  // The figures `crates/edit`'s own `a_fade_rises_from_silence_and_falls_back_to_it`
  // asserts: a two-second fade in and a four-second fade out on a twenty-second
  // clip.
  const faded = (): AudioTrack => track({ fade_in: 2_000_000_000, fade_out: 4_000_000_000 });
  const clipNanos = 20_000_000_000;
  const SECOND = 1_000_000_000;

  it('rises linearly in amplitude and falls back to it', () => {
    const at = (atNanos: number) => fadeAmplitude(faded(), atNanos, clipNanos);

    expect(at(0)).toBe(0);
    expect(at(SECOND)).toBeCloseTo(0.5, 12);
    expect(at(2 * SECOND)).toBe(1);
    expect(at(10 * SECOND)).toBe(1);
    expect(at(16 * SECOND)).toBe(1);
    expect(at(18 * SECOND)).toBeCloseTo(0.5, 12);
    expect(at(clipNanos - 1)).toBeLessThan(1e-8);
    expect(at(clipNanos)).toBe(0);
  });

  it('folds the level, the mute and the fade into one multiplier for the export', () => {
    expect(trackAmplitudeAt(faded(), 0, clipNanos)).toBe(0);
    expect(trackAmplitudeAt(track({ muted: true }), SECOND, clipNanos)).toBe(0);
    expect(trackAmplitudeAt(track({ gain_db: 0 }), 10 * SECOND, clipNanos)).toBe(1);
  });

  it('applies the editor’s solo on top, in the preview only', () => {
    // The preview plays the fade the export will write; the solo only decides
    // which tracks it plays it for — `crates/edit`'s
    // `a_soloed_preview_is_still_faded`.
    expect(monitoredAmplitudeAt(faded(), 2, 2, SECOND, clipNanos)).toBeCloseTo(0.5, 12);
    // A track the solo silences reads silent for the whole clip.
    expect(monitoredAmplitudeAt(faded(), 0, 2, SECOND, clipNanos)).toBe(0);
    expect(monitoredAmplitudeAt(faded(), 2, SOLO_NONE, SECOND, clipNanos)).toBe(
      trackAmplitudeAt(faded(), SECOND, clipNanos),
    );
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
