/**
 * Reading the timeline: where the playhead is, and what is under it.
 *
 * # Two kinds of time, and never one
 *
 * A timeline draws **output** time — measured from the start of the clip — while
 * the material it points at is in **source** time, measured from the first frame
 * of one recording. The moment an edit contains a cut or a speed change the two
 * diverge, and `docs/editing.md` is where that model is argued. Everything
 * below counts nanoseconds, as the document does.
 *
 * # Why this arithmetic is here rather than asked for
 *
 * `EditDocument::locate` is "what both the preview and the exporter must use,
 * so that they cannot disagree" (`docs/editing.md`), and `crates/edit`'s own
 * documentation names this screen as one of its two callers. The window cannot
 * call it: it links exactly one crate of the workspace, and that one is the
 * control protocol (`tests/integration/tests/workspace_layering.rs`). So the
 * same arithmetic is written here, from the formulae that document states, and
 * held to the same figures the crate's own tests use — which is what
 * `timeline.test.ts` does.
 *
 * ```text
 *   segment_start[k] = Σ output_nanos(segment[i]) for i < k
 *   output_nanos(segment)  = span_nanos × denominator ÷ numerator
 *   source_time(at)        = span.start + (at − segment_start) × numerator ÷ denominator
 * ```
 *
 * Both are integer divisions, truncating, and both are done in `BigInt` here
 * for the reason the crate does them in 128 bits: a span multiplied by a speed's
 * numerator can leave the range JavaScript holds integers exactly in, and a
 * preview that rounded differently from the exporter would drift over a long
 * clip. Nanoseconds themselves stay ordinary numbers — 2^53 of them is a
 * hundred and four days, against recordings that last hours.
 *
 * Every range is half-open, `[start, end)`: the segment ending at twelve seconds
 * and the one starting there do not both claim the frame between them.
 *
 * # Unreadable rather than wrong
 *
 * A segment with a zero in its speed or a backwards span has no length, so
 * every function here answers `null` rather than guessing — the same choice
 * `total_output_nanos` makes. The editor shows that as a clip it cannot draw.
 */

import {
  recordingOf,
  type AudioTrack,
  type EditDocument,
  type Segment,
  type TextOverlay,
} from './document';

/** A thousand million: one second of the document's nanoseconds. */
export const NANOS_PER_SECOND = 1_000_000_000;

/** Where a position on the edited timeline comes from. */
export interface Placement {
  /** Which segment of the document covers it. */
  readonly segment: number;
  /** Which source that segment draws from. */
  readonly source: number;
  /** Where in that recording the material is, in nanoseconds. */
  readonly sourceNanos: number;
  /** Where the segment starts on the edited timeline, in nanoseconds. */
  readonly segmentStartNanos: number;
}

/** `numerator ÷ denominator` of `value`, truncating, exactly. */
function scale(value: number, numerator: number, denominator: number): number | null {
  if (numerator <= 0 || denominator <= 0) {
    return null;
  }
  const result = (BigInt(value) * BigInt(numerator)) / BigInt(denominator);
  return Number(result);
}

/** How long `segment` lasts in the output, or `null` if it cannot be read. */
export function outputNanosOf(segment: Segment): number | null {
  if (segment.span.end < segment.span.start) {
    return null;
  }
  return scale(
    segment.span.end - segment.span.start,
    segment.speed.denominator,
    segment.speed.numerator,
  );
}

/**
 * Where each segment starts on the edited timeline, with the clip's own end
 * last — so the list is every boundary a cut can be at, in order.
 *
 * `null` if any segment cannot be read, because a length that cannot be
 * computed makes every position after it unknown rather than approximate.
 */
export function boundaries(document: EditDocument): readonly number[] | null {
  const found = [0];
  let total = 0;
  for (const segment of document.segments) {
    const length = outputNanosOf(segment);
    if (length === null) {
      return null;
    }
    total += length;
    found.push(total);
  }
  return found;
}

/** How long the clip lasts, or `null` if one of its segments cannot be read. */
export function totalOutputNanos(document: EditDocument): number | null {
  const found = boundaries(document);
  return found === null ? null : (found[found.length - 1] ?? 0);
}

/**
 * Finds the material at `atNanos`, or `null` if the clip has already ended.
 *
 * Linear over the segments, as the crate's is: a clip is a handful of them, and
 * an index keyed on cumulative offsets would be a second representation of the
 * timeline that could disagree with the first.
 */
export function locate(document: EditDocument, atNanos: number): Placement | null {
  let segmentStart = 0;

  for (const [index, segment] of document.segments.entries()) {
    const length = outputNanosOf(segment);
    if (length === null) {
      return null;
    }
    const end = segmentStart + length;
    if (atNanos < end) {
      const intoSource = scale(
        atNanos - segmentStart,
        segment.speed.numerator,
        segment.speed.denominator,
      );
      if (intoSource === null) {
        return null;
      }
      return {
        segment: index,
        source: segment.source,
        sourceNanos: segment.span.start + intoSource,
        segmentStartNanos: segmentStart,
      };
    }
    segmentStart = end;
  }

  return null;
}

/**
 * Where a moment in one recording lands on the edited timeline, in nanoseconds.
 *
 * The inverse of {@link locate}, and it answers a *list* because an edit is not
 * a one-to-one map from a recording onto the clip:
 *
 * - **none**, when every segment that plays this recording was trimmed past the
 *   moment. A kill that was cut out of the clip is not on the clip's timeline,
 *   and drawing it at the nearest frame would be a marker for something the
 *   footage does not contain (AGENTS.md section 27).
 * - **more than one**, when the same seconds of a recording are used twice.
 *   Both are the moment, and showing one of them would be picking a favourite.
 *
 * Half-open, `[start, end)`, like every other range here: the segment ending at
 * twelve seconds does not claim a moment the next segment starts with. Empty
 * when a segment cannot be read, for the reason {@link boundaries} is `null`
 * there — a length that cannot be computed makes every position after it
 * unknown rather than approximate.
 */
export function outputPositionsOf(
  document: EditDocument,
  recording: string,
  sourceNanos: number,
): readonly number[] {
  const found: number[] = [];
  let segmentStart = 0;

  for (const segment of document.segments) {
    const length = outputNanosOf(segment);
    if (length === null) {
      return [];
    }
    const plays =
      recordingOf(document, segment.source) === recording &&
      segment.span.start <= sourceNanos &&
      sourceNanos < segment.span.end;
    if (plays) {
      const into = scale(
        sourceNanos - segment.span.start,
        segment.speed.denominator,
        segment.speed.numerator,
      );
      if (into !== null) {
        found.push(segmentStart + into);
      }
    }
    segmentStart += length;
  }

  return found;
}

/**
 * Which track, if any, the editor is listening to on its own; `null` is the
 * whole mix. The TypeScript mirror of `Solo` in `crates/edit/src/audio.rs`.
 *
 * **Playback state, not part of a document.** [Issue
 * #85](https://github.com/wildware-uk/clipped/issues/85) moved soloing out of
 * the edit document — `docs/editing.md#solo-is-not-a-property-of-a-track` —
 * because it describes a moment of somebody's editing session rather than the
 * clip. So this holds a track index rather than a document field, the editor
 * keeps it in its own component state (`ClipEditor`'s `useState`), it is never
 * written to storage, and {@link resolve} — what an export reads — does not
 * take one at all. A `number` rather than a set, so that "two tracks are
 * soloed" is not a state this type can hold: {@link toggleSolo} moves the solo
 * rather than adding to it.
 */
export type Solo = number | null;

/** Listening to the whole mix, which is where the editor starts. */
export const SOLO_NONE: Solo = null;

/**
 * The solo after the control on `track` is pressed: `Solo::toggled` in
 * `crates/edit`.
 *
 * Pressing it on the soloed track clears the solo; pressing it on any other
 * track moves the solo there. There is deliberately no way to reach a state
 * with two tracks soloed.
 */
export function toggleSolo(current: Solo, track: number): Solo {
  return current === track ? SOLO_NONE : track;
}

/** Whether `solo` silences `track`, which is true of every other track. */
function soloSilences(solo: Solo, track: number): boolean {
  return solo !== null && solo !== track;
}

/** What a track contributes once mute and solo are resolved. */
export type TrackOutput =
  { readonly audible: false } | { readonly audible: true; readonly gainDb: number };

/**
 * What `track` contributes to the **export**: `resolve` in
 * `crates/edit/src/audio.rs`.
 *
 * Mute and the level, and nothing about how anybody was listening — an export
 * is never given a {@link Solo}, so nothing about a solo left on can reach the
 * file. A type rather than a level, so that "silent" cannot be misread as "no
 * gain applied", which is the mistake that exports a muted microphone at full
 * volume.
 */
export function resolve(track: AudioTrack): TrackOutput {
  if (track.muted) {
    return { audible: false };
  }
  return { audible: true, gainDb: track.gain_db };
}

/**
 * What `track` contributes to the **preview**, given the editor's `solo`:
 * `monitor` in `crates/edit/src/audio.rs`.
 *
 * The rules are `docs/editing.md`'s, which are every mixing desk's: **mute
 * wins**, including on the track that is itself soloed, so soloing a muted
 * track does not unmute it; **solo is exclusive**, silencing every other track
 * while it is on; and **solo does nothing when nothing is soloed**, so the
 * preview matches {@link resolve} — the export — exactly in the ordinary case.
 */
export function monitor(track: AudioTrack, index: number, solo: Solo): TrackOutput {
  if (soloSilences(solo, index)) {
    return { audible: false };
  }
  return resolve(track);
}

/** An amplitude multiplier from a level in decibels: `amplitude_from_db` in `crates/edit`. */
function amplitudeFromDb(gainDb: number): number {
  return 10 ** (gainDb / 20);
}

/**
 * The fade envelope of `track` at `atNanos` of a clip lasting `clipNanos`:
 * `fade_amplitude` in `crates/edit/src/audio.rs`.
 *
 * A multiplier between `0` and `1` on top of the track's level, defined once so
 * that a preview and an export cannot draw two different curves. It rises
 * linearly in amplitude across `fade_in` and falls linearly to zero across
 * `fade_out`; past the end of the clip there is nothing to hear, which is why
 * `atNanos >= clipNanos` answers `0` rather than being asked for.
 */
export function fadeAmplitude(track: AudioTrack, atNanos: number, clipNanos: number): number {
  if (atNanos >= clipNanos) {
    return 0;
  }
  let amplitude = 1;
  if (atNanos < track.fade_in) {
    amplitude *= atNanos / track.fade_in;
  }
  const remaining = clipNanos - atNanos;
  if (remaining <= track.fade_out) {
    amplitude *= remaining / track.fade_out;
  }
  return amplitude;
}

/** `output`'s level and `track`'s fade envelope, folded into one multiplier. */
function amplitudeOf(
  output: TrackOutput,
  track: AudioTrack,
  atNanos: number,
  clipNanos: number,
): number {
  if (!output.audible) {
    return 0;
  }
  return amplitudeFromDb(output.gainDb) * fadeAmplitude(track, atNanos, clipNanos);
}

/**
 * How loud `track` is at `atNanos` of a clip lasting `clipNanos`, in the
 * **export**: `track_amplitude_at` in `crates/edit`.
 *
 * The whole mix for one track at one moment, as a multiplier on the recorded
 * samples: its level, its mute and its fades, resolved into the number an
 * exporter would multiply by. `0` is silence and `1` is the track exactly as
 * recorded.
 */
export function trackAmplitudeAt(track: AudioTrack, atNanos: number, clipNanos: number): number {
  return amplitudeOf(resolve(track), track, atNanos, clipNanos);
}

/**
 * The same figure in the **preview**, with the editor's `solo` applied:
 * `monitored_amplitude_at` in `crates/edit`. Agrees with
 * {@link trackAmplitudeAt} exactly whenever `solo` is {@link SOLO_NONE}.
 */
export function monitoredAmplitudeAt(
  track: AudioTrack,
  index: number,
  solo: Solo,
  atNanos: number,
  clipNanos: number,
): number {
  return amplitudeOf(monitor(track, index, solo), track, atNanos, clipNanos);
}

/** The overlays on screen at `atNanos`, in the order the document holds them. */
export function overlaysAt(document: EditDocument, atNanos: number): readonly TextOverlay[] {
  return document.overlays.filter(
    (overlay) => overlay.when.start <= atNanos && atNanos < overlay.when.end,
  );
}

/** The boundary before `atNanos`, or the start of the clip. */
export function previousBoundary(found: readonly number[], atNanos: number): number {
  const earlier = found.filter((boundary) => boundary < atNanos);
  return earlier.length === 0 ? 0 : (earlier[earlier.length - 1] ?? 0);
}

/** The boundary after `atNanos`, or the end of the clip. */
export function nextBoundary(found: readonly number[], atNanos: number): number {
  return found.find((boundary) => boundary > atNanos) ?? found[found.length - 1] ?? 0;
}

/** `nanos` as `mm:ss.mmm`, or `h:mm:ss.mmm` once a clip runs past the hour. */
export function formatTimecode(nanos: number): string {
  const totalMs = Math.floor(nanos / 1_000_000);
  const ms = totalMs % 1000;
  const totalSeconds = Math.floor(totalMs / 1000);
  const seconds = totalSeconds % 60;
  const minutes = Math.floor(totalSeconds / 60) % 60;
  const hours = Math.floor(totalSeconds / 3600);
  const pad = (value: number, width = 2): string => String(value).padStart(width, '0');
  const withoutHours = `${pad(minutes)}:${pad(seconds)}.${pad(ms, 3)}`;
  return hours === 0 ? withoutHours : `${String(hours)}:${withoutHours}`;
}

/** `nanos` as a tick label: whole seconds once the ticks are that far apart. */
export function formatTickLabel(nanos: number, intervalNanos: number): string {
  const timecode = formatTimecode(nanos);
  return intervalNanos >= NANOS_PER_SECOND ? timecode.slice(0, -4) : timecode;
}

/**
 * How far apart the ruler's marks are, at this length and this zoom.
 *
 * Zoom is how many screenfuls wide the timeline is drawn, so the number of
 * marks grows with it and the interval steps down through the list rather than
 * being a division of the clip's own length: a mark every 3.7 seconds is a mark
 * nobody can read a position off.
 */
const TICK_INTERVALS: readonly number[] = [
  0.1, 0.25, 0.5, 1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 1800, 3600,
].map((seconds) => seconds * NANOS_PER_SECOND);

/** About this many marks fit across the window at any zoom. */
const MARKS_PER_SCREEN = 8;

/** The interval between the ruler's marks, in nanoseconds. */
export function tickIntervalNanos(durationNanos: number, zoom: number): number {
  const wanted = MARKS_PER_SCREEN * zoom;
  const first = TICK_INTERVALS[0] ?? NANOS_PER_SECOND;
  const last = TICK_INTERVALS[TICK_INTERVALS.length - 1] ?? NANOS_PER_SECOND;
  return (
    TICK_INTERVALS.find((interval) => durationNanos / interval <= wanted) ??
    (durationNanos > 0 ? last : first)
  );
}

/** Every mark of the ruler, in nanoseconds, from zero up to the clip's end. */
export function ticks(durationNanos: number, zoom: number): readonly number[] {
  const interval = tickIntervalNanos(durationNanos, zoom);
  const marks: number[] = [];
  for (let at = 0; at <= durationNanos; at += interval) {
    marks.push(at);
  }
  return marks;
}

/** How wide the timeline is drawn, as a multiple of the window. */
export const ZOOM_STEPS: readonly number[] = [1, 2, 4, 8, 16];

/**
 * Where the scroller has to be for the playhead to be on screen.
 *
 * Returned rather than performed so that the rule is a function with a test
 * rather than three lines inside an effect: a playhead that has left the window
 * is centred, and one already comfortably inside it does not move the view at
 * all, because a timeline that scrolled on every arrow key could not be read.
 */
export function scrollToShow(
  playheadFraction: number,
  contentWidth: number,
  viewportWidth: number,
  scrollLeft: number,
): number {
  if (contentWidth <= viewportWidth) {
    return 0;
  }
  const at = playheadFraction * contentWidth;
  const margin = viewportWidth / 10;
  const centred = Math.min(Math.max(at - viewportWidth / 2, 0), contentWidth - viewportWidth);
  const tooFarLeft = at < scrollLeft + margin;
  const tooFarRight = at > scrollLeft + viewportWidth - margin;
  return tooFarLeft || tooFarRight ? centred : scrollLeft;
}
