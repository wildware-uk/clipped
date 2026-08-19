// @vitest-environment node
import type { Preview, PreviewTrack } from '@clipped/shared';
import { describe, expect, it } from 'vitest';

import { readEditDocument, type EditDocument } from './document';
import { lanePieces, type LanePiece, type PeaksOf } from './lanePeaks';
import { totalOutputNanos } from './timeline';
import type { WaveformView } from '../preview';
import { storedDocument } from '../test/editDocumentFixture';

/**
 * Where a lane's peaks come from, and which part of which recording they are
 * (issue #66).
 *
 * # Why this file is arithmetic and not a render
 *
 * jsdom lays nothing out, so a rendered test can see that a picture is in a
 * lane and not what is *in* the picture. Every failure this file is shaped
 * around draws a perfectly plausible waveform:
 *
 * - the whole recording under a clip trimmed from its middle;
 * - the same recorded stream under every lane;
 * - the peaks of recording A under a segment that plays recording B.
 *
 * A case asserting "a path is drawn" passes on all three. So the peaks below
 * are a **ramp** — bucket `n` of a track carries a sample that says which
 * bucket it is — and the assertions read the drawn samples back off the path
 * and compare bucket numbers. A build that drew the wrong material fails with
 * `[0, 1, 2, …]` against `[30, 31, …]`, which names the range rather than
 * saying an element is missing.
 *
 * The recording that {@link storedDocument} describes is the three-segment clip
 * out of `crates/edit`'s own documentation: eight seconds from 30s of one
 * recording, twelve more from 92s of it, then four seconds from 5s of a second
 * recording. That is a clip trimmed from the middle *and* a clip cut from two
 * files, which is the pair of cases this whole file is about.
 */

/** How tall a lane is drawn, written out here rather than imported. */
const LANE = 40;

/** What full scale is, likewise: the production constant is what is on trial. */
const FULL_SCALE = 127;

/**
 * A track whose bucket `n` carries `offset + n` as its maximum.
 *
 * The peaks are a ruler. A drawn sample of 92 can only have come from bucket
 * `92 - offset` of this track and from nowhere else, which is what lets the
 * assertions below name a range instead of counting elements.
 *
 * The minimum is deliberately *not* the mirror of the maximum — real audio is
 * asymmetric, and a build that drew the maxima twice would otherwise pass.
 */
function ramp(
  index: number,
  buckets: number,
  seconds: number,
  offset: number,
  name?: string,
): PreviewTrack {
  const peaks: number[] = [];
  for (let bucket = 0; bucket < buckets; bucket += 1) {
    const maximum = offset + bucket;
    peaks.push(-Math.floor(maximum / 2), maximum);
  }
  return {
    index,
    ...(name === undefined ? {} : { name }),
    sample_rate: 48_000,
    channels: 2,
    duration_seconds: seconds,
    peaks,
  };
}

/** The first recording of the fixture, as the recorder would answer for it. */
const RECORDING_A = 'rec-2026-08-11-cs2';

/** The second, which only the Game track draws on. */
const RECORDING_B = 'rec-2026-08-11-cs2-b';

/**
 * Two minutes long, one bucket a second, and its two streams carry different
 * numbers so that a lane drawn from the wrong one is a different list.
 */
const A_TRACKS: readonly PreviewTrack[] = [
  ramp(0, 120, 120, 0, 'Game'),
  // Counting down rather than up, so that no bucket of this stream carries the
  // same number as the same bucket of the one above.
  {
    ...ramp(1, 120, 120, 0, 'Microphone'),
    peaks: countDown(120),
  },
];

/** One minute long, and every bucket of it above anything recording A holds. */
const B_TRACKS: readonly PreviewTrack[] = [ramp(0, 60, 60, 60, 'Game')];

/** Bucket `n` carrying `127 - n`, which is `ramp` in the other direction. */
function countDown(buckets: number): number[] {
  const peaks: number[] = [];
  for (let bucket = 0; bucket < buckets; bucket += 1) {
    const maximum = FULL_SCALE - bucket;
    peaks.push(-Math.floor(maximum / 2), maximum);
  }
  return peaks;
}

/** A `ready` answer carrying `tracks`. */
function ready(tracks: readonly PreviewTrack[]): WaveformView {
  return { state: 'answered', preview: { kind: 'waveform', state: 'ready', tracks } };
}

/** An answer in one of the states that carries no peaks. */
function answered(preview: Preview): WaveformView {
  return { state: 'answered', preview };
}

/** What the recorder says about the fixture's two recordings. */
const PEAKS: PeaksOf = (recording) => {
  if (recording === RECORDING_A) {
    return ready(A_TRACKS);
  }
  if (recording === RECORDING_B) {
    return ready(B_TRACKS);
  }
  return { state: 'unasked' };
};

/** The fixture clip, read the way the screen reads one. */
function fixture(changes: Record<string, unknown> = {}): EditDocument {
  const read = readEditDocument(storedDocument(changes));
  if (!read.ok) {
    throw new Error(read.problem);
  }
  return read.document;
}

/** Every piece of one lane of a document. */
function lane(
  document: EditDocument,
  track: number,
  peaksOf: PeaksOf = PEAKS,
): readonly LanePiece[] {
  const durationNanos = totalOutputNanos(document);
  if (durationNanos === null) {
    throw new Error('the fixture has no length');
  }
  return lanePieces(document, track, durationNanos, peaksOf);
}

/** The samples a piece draws, read back off its path. */
function drawn(piece: LanePiece): {
  readonly x: number[];
  readonly maxima: number[];
  readonly minima: number[];
} {
  if (!piece.content.drawn) {
    throw new Error(`piece ${String(piece.segment)} drew nothing: ${piece.content.why}`);
  }
  const points = piece.content.outline
    .replace(/^M/, '')
    .replace(/ Z$/, '')
    .split(' L')
    .map((point) => point.split(',').map(Number));
  const half = points.length / 2;
  // The inverse of the production scale, written out here rather than imported:
  // a test that used the same function to expect and to produce would agree
  // with itself whatever either did.
  const sample = (y: number): number => Math.round(((LANE / 2 - y) / (LANE / 2)) * FULL_SCALE);
  return {
    x: points.slice(0, half).map((point) => point[0] ?? Number.NaN),
    maxima: points.slice(0, half).map((point) => sample(point[1] ?? Number.NaN)),
    minima: points
      .slice(half)
      .reverse()
      .map((point) => sample(point[1] ?? Number.NaN)),
  };
}

/** Why a piece drew nothing. */
function absence(piece: LanePiece): string {
  return piece.content.drawn ? 'drawn' : piece.content.why;
}

/** The numbers `first` up to but not including `last`. */
function run(first: number, last: number): number[] {
  return Array.from({ length: last - first }, (_, step) => first + step);
}

describe('the peaks under one lane', () => {
  it('draws the part of the recording a segment uses, not the whole of it', () => {
    // The failure this whole file exists for. The first segment plays 30s to
    // 38s of a two-minute recording; a build handing the lane the entire array
    // would draw buckets 0 to 119, which is a waveform of the right recording
    // and the wrong thirty seconds — and it would look completely convincing.
    const first = lane(fixture(), 0)[0];

    expect(first).toBeDefined();
    expect(drawn(first!).maxima).toEqual(run(30, 38));
    expect(first!.content).toMatchObject({ buckets: 8 });
  });

  it('re-bases the picture on the slice, so a zoom stretches it rather than moving it', () => {
    // The x of the path runs from zero however far into the recording the
    // material is, because the `viewBox` is the slice. A path that kept the
    // track's own bucket numbers would draw the first segment somewhere off the
    // right-hand end of its own picture.
    const first = lane(fixture(), 0)[0];

    expect(drawn(first!).x).toEqual(run(0, 8));
  });

  it('draws the minima as well as the maxima, rather than mirroring one of them', () => {
    // The fixture's minima are half its maxima, rounded down. A build that
    // reflected the top of the outline would draw -30 where -15 belongs, and
    // every waveform in Clipped would be symmetrical.
    const first = lane(fixture(), 0)[0];

    expect(drawn(first!).minima).toEqual(run(30, 38).map((bucket) => -Math.floor(bucket / 2)));
  });

  it('puts each piece where its segment is on the clip', () => {
    // 8s, 12s and 4s of a 24-second clip. A piece in the right place drawn from
    // the wrong material is what the case above catches; this is the other half.
    const pieces = lane(fixture(), 0);

    expect(pieces.map((piece) => [piece.startFraction, piece.widthFraction] as const)).toEqual([
      [0, 1 / 3],
      [1 / 3, 1 / 2],
      [5 / 6, 1 / 6],
    ]);
  });

  it('draws every segment of the clip, including the ones it cannot draw peaks for', () => {
    // Three segments, three pieces. A lane that dropped what it could not draw
    // would put everything after the gap in the wrong place.
    expect(lane(fixture(), 1).map((piece) => piece.segment)).toEqual([0, 1, 2]);
  });
});

describe('a lane per audio track', () => {
  it('draws each lane from the recorded stream that feeds it', () => {
    // "Game" takes stream 0 and "Microphone" stream 1 of the same recording. A
    // build that looked the recording up and took its first track would draw
    // the same peaks under both, and every lane of a four-track clip would be
    // identical — which is a picture nobody would question.
    const game = drawn(lane(fixture(), 0)[0]!).maxima;
    const microphone = drawn(lane(fixture(), 1)[0]!).maxima;

    expect(game).toEqual(run(30, 38));
    expect(microphone).toEqual(run(30, 38).map((bucket) => 127 - bucket));
  });

  it('says a lane takes nothing from a recording rather than leaving it blank', () => {
    // The fixture's Microphone track lists an input for the first recording and
    // none for the second, so the exported track is silent for the last four
    // seconds. That is not a missing waveform, and it does not read like one.
    expect(lane(fixture(), 1).map(absence)).toEqual(['drawn', 'drawn', 'silent']);
  });

  it('has nothing at all to draw for an audio track the document does not have', () => {
    expect(lane(fixture(), 7)).toEqual([]);
  });
});

describe('a clip cut from two recordings', () => {
  it('draws each segment from the peaks of the recording it plays', () => {
    // The last segment is 5s to 9s of a *second* file. Every bucket of that
    // file carries a number recording A does not, so a build that drew the
    // first recording's peaks under it fails with recording A's numbers rather
    // than with a missing element.
    const pieces = lane(fixture(), 0);

    expect(pieces.map((piece) => piece.recording)).toEqual([RECORDING_A, RECORDING_A, RECORDING_B]);
    expect(drawn(pieces[2]!).maxima).toEqual(run(65, 69));
  });

  it('draws the second span of the first recording from where that span is', () => {
    // 92s to 104s of the same file the first segment came from, which is the
    // case a build that cached "the peaks of this lane" would get wrong.
    expect(drawn(lane(fixture(), 0)[1]!).maxima).toEqual(run(92, 104));
  });
});

describe('a segment played at a speed', () => {
  /** One segment at 2x and one at the recording's own speed, same material. */
  const SPED = {
    sources: [{ id: 0, recording: RECORDING_A }],
    segments: [
      {
        source: 0,
        span: { start: 30_000_000_000, end: 38_000_000_000 },
        speed: { numerator: 2, denominator: 1 },
        crop: null,
        rotation: 'none',
      },
      {
        source: 0,
        span: { start: 38_000_000_000, end: 46_000_000_000 },
        speed: { numerator: 1, denominator: 1 },
        crop: null,
        rotation: 'none',
      },
    ],
    audio_tracks: [{ name: 'Game', inputs: [{ source: 0, stream: 0 }], gain_db: 0, muted: false }],
    overlays: [],
  };

  it('draws the same material into a narrower piece rather than fewer buckets', () => {
    // Eight seconds of recording at 2x is four seconds of clip. The peaks are
    // the same eight buckets; what changes is the width they are stretched
    // into, which is why nothing in `lanePeaks.ts` multiplies a peak by
    // anything.
    const pieces = lane(fixture(SPED), 0);

    expect(drawn(pieces[0]!).maxima).toEqual(run(30, 38));
    expect(drawn(pieces[1]!).maxima).toEqual(run(38, 46));
    expect(pieces.map((piece) => piece.widthFraction)).toEqual([1 / 3, 2 / 3]);
  });
});

describe('when there are no peaks to draw', () => {
  it('keeps "not generated yet" and "there will not be one" apart', () => {
    // `docs/waveforms.md`'s third state, in the editor. "Not yet" is the
    // ordinary state of a recording written a minute ago and it resolves
    // itself; "unavailable" will not, and a lane that said the same for both
    // would promise a waveform that is not coming.
    const pending = lane(fixture(), 0, () =>
      answered({ kind: 'waveform', state: 'pending', tracks: [] }),
    );
    const never = lane(fixture(), 0, () =>
      answered({
        kind: 'waveform',
        state: 'unavailable',
        tracks: [],
        reason: 'that file is in a codec this build has no decoder for',
      }),
    );

    expect(pending.map(absence)).toEqual(['pending', 'pending', 'pending']);
    expect(never.map(absence)).toEqual(['none', 'none', 'none']);
  });

  it('draws no outline at all for a state that has none, rather than a flat line', () => {
    // The trap `docs/waveforms.md` forbids by name: a line through the middle
    // of the lane is indistinguishable from silence. Nothing here may carry an
    // outline, so there is nothing for a stylesheet to draw.
    for (const view of [
      answered({ kind: 'waveform', state: 'pending', tracks: [] }),
      answered({ kind: 'waveform', state: 'unavailable', tracks: [], reason: 'no decoder' }),
      answered({ kind: 'waveform', state: 'ready', tracks: [] }),
      { state: 'unasked' } as WaveformView,
      { state: 'asking' } as WaveformView,
      {
        state: 'refused',
        problem: { code: 'recorder_unreachable', message: 'no recorder' },
      } as WaveformView,
    ]) {
      expect(lane(fixture(), 0, () => view).map((piece) => piece.content.drawn)).toEqual([
        false,
        false,
        false,
      ]);
    }
  });

  it('draws nothing for a segment past the end of the recording it names', () => {
    // The fixture's second recording is a minute long; a segment reaching past
    // it has no bucket, and the honest answer is an empty piece rather than the
    // last bucket stretched across it.
    const past = fixture({
      segments: [
        {
          source: 1,
          span: { start: 90_000_000_000, end: 95_000_000_000 },
          speed: { numerator: 1, denominator: 1 },
          crop: null,
          rotation: 'none',
        },
      ],
    });

    expect(lane(past, 0).map(absence)).toEqual(['none']);
  });

  it('draws nothing for a stream the recording does not carry', () => {
    // The document says Microphone is stream 1 of the first recording; this
    // answer has only stream 0. Drawing stream 0 under it would put the game's
    // audio under the microphone's slider.
    expect(lane(fixture(), 1, () => ready([A_TRACKS[0]!])).map(absence)).toEqual([
      'none',
      'none',
      'silent',
    ]);
  });
});
