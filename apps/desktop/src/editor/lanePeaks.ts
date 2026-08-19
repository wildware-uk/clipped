import type { PreviewTrack } from '@clipped/shared';

import { recordingOf, type EditDocument } from './document';
import { outputNanosOf } from './timeline';
import type { WaveformView } from '../preview';
import { bucketsOver, envelope } from '../waveformOutline';

/**
 * What one audio lane of the editor's timeline draws, and where (issue #66).
 *
 * # The whole difficulty, in one sentence
 *
 * The lane is in **edit-document time** and the peaks are in **recording
 * time**, and a clip trimmed from the middle of a recording must draw the peaks
 * of *that part* rather than the whole recording squeezed to fit. `timeline.ts`
 * argues the two kinds of time and `docs/editing.md` is where the model is;
 * what this file does is walk the segments and place a *slice* of one
 * recording's peaks under each one.
 *
 * ```text
 *   clip           |<-- seg 0 -->|<------ seg 1 ------>|<- seg 2 ->|
 *   recording A     30s .. 38s     92s .. 104s
 *   recording B                                          5s .. 9s
 * ```
 *
 * Three pieces, three slices, and none of them is the whole of anything. A
 * build that handed the lane the entire array of peaks would draw a perfectly
 * plausible waveform of the wrong audio, and a test that asserted "a path is
 * drawn" would pass on it — which is why {@link LanePiece} carries the range it
 * came from and the screen puts that range in the picture's label.
 *
 * # A lane is pieces, not a waveform
 *
 * One `<path>` per lane is the shape only for a clip made of one span of one
 * recording. `docs/editing.md`'s audio model has an output track fed by *which
 * stream of which recording* — `"Game" ← source 0 stream 0, source 1 stream 0`
 * — so a clip cut from two recordings draws one piece per segment, each from
 * its own file's peaks, laid end to end in output time.
 *
 * It also follows from that model that a lane can be legitimately **blank**
 * over a segment: an output track that lists no input for the recording a
 * segment plays contributes nothing there, and the exported track is silent for
 * those seconds. That is {@link LaneAbsence} `silent`, and it is a different
 * statement from "the peaks have not been computed yet" — which is why the
 * absences are an enumeration rather than one string.
 *
 * # Speed is handled by where a piece goes, not by what it holds
 *
 * A segment played at 2× draws the same span of the recording into half as much
 * output, so the slice is the same slice and the piece it is stretched into is
 * narrower. Nothing here multiplies a peak by anything.
 */

/** Why a lane has nothing to draw over one segment. */
export type LaneAbsence =
  /** A round trip for this recording's peaks is in flight. */
  | 'asking'
  /** They have not been computed yet, and asking has queued them. */
  | 'pending'
  /**
   * There will not be any, or the asking failed, or the recording carries no
   * such stream. Three routes to one lane, because the lane has room for three
   * words; which of them it was is said under the timeline.
   */
  | 'none'
  /**
   * This output track takes nothing from this recording, so the exported track
   * is silent here. Not a missing waveform at all.
   */
  | 'silent';

/** What a lane draws over one segment. */
export type LanePieceContent =
  | {
      readonly drawn: true;
      /** The outline, re-based so its x starts at zero. */
      readonly outline: string;
      /** How many buckets it holds, which is the picture's `viewBox` width. */
      readonly buckets: number;
      /** What the recording calls the stream this came from, where it named it. */
      readonly trackName?: string;
    }
  | { readonly drawn: false; readonly why: LaneAbsence };

/** One segment's worth of one lane. */
export interface LanePiece {
  /** Which segment of the document it covers. */
  readonly segment: number;
  /** The recording that segment plays, or `null` for an undeclared source. */
  readonly recording: string | null;
  /** Where the piece starts on the clip, as a fraction of the clip. */
  readonly startFraction: number;
  /** How much of the clip it covers, as a fraction. */
  readonly widthFraction: number;
  /** Where the material starts in the **recording**, in nanoseconds. */
  readonly fromNanos: number;
  /** Where it ends in the recording, in nanoseconds. */
  readonly toNanos: number;
  /** The peaks, or why there are none. */
  readonly content: LanePieceContent;
}

/**
 * What the peaks of a recording are, as far as the screen has been told.
 *
 * A lookup rather than a map so that a caller can answer for a recording it has
 * never heard of without building an entry for it, and so that "nobody asked"
 * is one of the answers rather than a missing key.
 */
export type PeaksOf = (recording: string) => WaveformView;

/**
 * Every piece of the lane for `document`'s audio track `trackIndex`.
 *
 * One per segment, in output order, whatever state the peaks are in — a lane
 * that dropped the pieces it could not draw would be a lane whose remaining
 * pieces were in the wrong places.
 */
export function lanePieces(
  document: EditDocument,
  trackIndex: number,
  durationNanos: number,
  peaksOf: PeaksOf,
): readonly LanePiece[] {
  const audio = document.audio_tracks[trackIndex];
  if (audio === undefined) {
    return [];
  }

  const pieces: LanePiece[] = [];
  let segmentStart = 0;

  for (const [segment, cut] of document.segments.entries()) {
    const length = outputNanosOf(cut);
    if (length === null) {
      // A segment with no length makes every position after it unknown rather
      // than approximate, which is what `boundaries` answers `null` for. The
      // screen refuses such a document before it reaches here.
      return [];
    }
    const recording = recordingOf(document, cut.source) ?? null;
    const stream = audio.inputs.find((input) => input.source === cut.source)?.stream;
    pieces.push({
      segment,
      recording,
      startFraction: fractionOf(segmentStart, durationNanos),
      widthFraction: fractionOf(length, durationNanos),
      fromNanos: cut.span.start,
      toNanos: cut.span.end,
      content:
        recording === null || stream === undefined
          ? { drawn: false, why: 'silent' }
          : contentOf(peaksOf(recording), stream, cut.span.start, cut.span.end),
    });
    segmentStart += length;
  }

  return pieces;
}

/** What one piece holds, given where that recording's peaks stand. */
function contentOf(
  view: WaveformView,
  stream: number,
  fromNanos: number,
  toNanos: number,
): LanePieceContent {
  if (view.state === 'asking') {
    return { drawn: false, why: 'asking' };
  }
  if (view.state !== 'answered') {
    // `unasked` — this recorder cannot make waveforms — and `refused`, where the
    // asking itself failed. Neither is "not yet", and neither says so.
    return { drawn: false, why: 'none' };
  }
  if (view.preview.state === 'pending') {
    return { drawn: false, why: 'pending' };
  }
  if (view.preview.state !== 'ready') {
    return { drawn: false, why: 'none' };
  }

  const track = (view.preview.tracks ?? []).find((candidate) => candidate.index === stream);
  if (track === undefined) {
    return { drawn: false, why: 'none' };
  }
  return outlineOf(track, fromNanos, toNanos);
}

/** The slice of `track` this segment uses, drawn. */
function outlineOf(track: PreviewTrack, fromNanos: number, toNanos: number): LanePieceContent {
  const range = bucketsOver(track, fromNanos, toNanos);
  if (range === null) {
    return { drawn: false, why: 'none' };
  }
  const outline = envelope(track, range);
  if (outline === null) {
    return { drawn: false, why: 'none' };
  }
  return {
    drawn: true,
    outline,
    buckets: range.to - range.from,
    ...(track.name === undefined ? {} : { trackName: track.name }),
  };
}

/** A position on the timeline as a fraction of the clip. */
function fractionOf(nanos: number, durationNanos: number): number {
  return durationNanos <= 0 ? 0 : nanos / durationNanos;
}
