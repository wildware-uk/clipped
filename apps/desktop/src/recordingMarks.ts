import type { LibraryEventLane } from '@clipped/shared';
import { useEffect, useState } from 'react';

import { describeMark, type EventMark, type MarkOrigin } from './events';
import { asProblem, readEvents, type LibraryProblem } from './library';
import { formatTimecode, NANOS_PER_SECOND } from './timecode';

/**
 * The marks of one recording, placed on the recording itself (issue #65).
 *
 * # Where the marks come from, and where they stop
 *
 * `library_events`, which is the recorder reading the `game_events` table and
 * subtracting the recording's own start from each event's moment on the
 * session's timeline (`apps/recorder/src/library.rs`). So `at` is already a
 * position **in the file the player is playing**, and this module never
 * subtracts anything: the one process that knows where a recording begins on a
 * session's clock has already done it.
 *
 * The other end is honest too. **Every mark that reaches this window today came
 * from a plugin.** `crates/plugins/src/report.rs`'s `ReportedEvent::into_event`
 * is the only production `GameEvent::new` in the workspace, so the recorder
 * itself writes none: a bookmark is a `BookmarkFile` beside the recording
 * (issue #64), a saved clip and a screenshot are `SessionEventKind` rows in
 * `session_events`, and microphone activity is not written down at all. None of
 * those is a `game_event` and none of them is on this timeline. The screen says
 * so rather than drawing a lane for each and leaving four of them permanently
 * empty (AGENTS.md section 27).
 *
 * # Why the marks are bucketed
 *
 * Issue #65's second criterion is a multi-hour recording with many markers that
 * stays responsive. The Editor's lane mounts one `<li>` and one `<button>` per
 * mark, which is right for a clip and wrong here: a three-hour sitting of a game
 * that reports a kill, a death and a round boundary is thousands of marks, and
 * at any width this panel is ever drawn at, most of them land on the same
 * pixel — so the cost is paid for marks that are invisible *and* unclickable,
 * because they are stacked under one another.
 *
 * {@link recordingTimeline} divides the recording into {@link TIMELINE_COLUMNS}
 * columns and draws **one marker per occupied column**, so the number of nodes
 * is bounded by the width of the panel rather than by the length of the
 * recording. Nothing is thrown away: a marker carries how many marks are under
 * it and says so, and the marks that fall outside the file are counted and
 * reported rather than dropped silently.
 *
 * What that bound is worth, measured against a documented fixture, is
 * `recordingMarks.test.ts`. It is the same position `virtualWindow.ts` takes
 * about the Library's table: no frame rate is claimed, because nothing in a
 * Vitest process can paint one — what is measured is the property a frame rate
 * depends on.
 */

/**
 * How many columns a recording is divided into, and so the most markers that
 * can be drawn at once.
 *
 * A number of columns rather than a number of markers, because what a marker
 * costs is a node and what it is worth is a pointer target. The panel this is
 * drawn in is a little under 1,200 CSS pixels at the window's minimum size on a
 * 200%-scaled display, which is the same figure `ClipPlaybackScreen` asks for
 * waveform peaks at (`docs/waveforms.md`); 240 columns is one marker every five
 * pixels there, which is about as close together as two marks can be and still
 * be told apart.
 *
 * Raising it draws more markers and makes each harder to hit. Lowering it puts
 * more marks under each marker, which costs nothing but detail. Neither can lose
 * a mark.
 */
export const TIMELINE_COLUMNS = 240;

/** One drawn marker: a place on the recording, and the marks that are there. */
export interface TimelineMarker {
  /** Stable across renders for the same recording and the same marks. */
  readonly key: string;
  /**
   * Where clicking it seeks to, in seconds.
   *
   * The exact position of the **earliest** mark under it, converted from the
   * recorder's nanoseconds and rounded nowhere. Not the column's centre: a
   * marker that seeked to where it is drawn rather than to where the event
   * happened would be accurate to a column, which on a three-hour recording is
   * forty-five seconds.
   */
  readonly atSeconds: number;
  /** Where it is drawn, as a fraction of the recording. */
  readonly fraction: number;
  /** The mark it seeks to, whole, so a caller need not look one up. */
  readonly mark: EventMark;
  /** How many marks are under it, that one included. Always at least one. */
  readonly count: number;
  /**
   * Who they came from, or `several` when the marks under one marker disagree.
   *
   * `several` is drawn and announced as its own thing rather than as whichever
   * of them happened to be first: a marker that said "reported by a plugin"
   * over a plugin's mark and your own note together would be attributing your
   * words to the plugin, which is the confusion `report.rs`'s guard exists to
   * prevent.
   */
  readonly origin: MarkOrigin | 'several';
}

/** A recording's marks, as the timeline draws them. */
export interface RecordingTimelineView {
  /** The markers, earliest first, at most {@link TIMELINE_COLUMNS} of them. */
  readonly markers: readonly TimelineMarker[];
  /** How many marks the recorder sent. */
  readonly total: number;
  /**
   * How many of them are not on the file the player is playing.
   *
   * The recorder subtracts a recording's start from an event's session time and
   * does not clamp the result (`apps/recorder/src/library.rs`), so a session
   * whose spans were recorded oddly can produce a negative position or one past
   * the end of the file. Counting them and saying so is the only honest answer:
   * drawing them at zero would put marks on the timeline that are not there,
   * and dropping them silently would show fewer marks than the recorder found.
   */
  readonly offRecording: number;
}

/**
 * The outline each origin is drawn as, in a twelve-by-twelve box.
 *
 * Four shapes that differ in the number of corners rather than in size or
 * weight, so that they are still four things at 200% scale, in monochrome, and
 * at the five pixels apart two markers can be. It is the signal that survives
 * when colour is taken away, which is why it lives beside the words rather than
 * inside the component that draws it.
 */
export const ORIGIN_SHAPES: Readonly<Record<MarkOrigin | 'several', string>> = {
  /** A triangle: a game integration said this happened. */
  plugin: 'M6 0 L12 11 L0 11 Z',
  /** A square: Clipped itself put this here. */
  clipped: 'M1 1 H11 V11 H1 Z',
  /** A diamond: somebody typed this. */
  you: 'M6 0 L12 6 L6 12 L0 6 Z',
  /** A cross: more than one of the above is under this marker. */
  several: 'M4 0 H8 V4 H12 V8 H8 V12 H4 V8 H0 V4 H4 Z',
};

/** What markers of each origin are called, in one word, for a legend. */
export const ORIGIN_WORDS: Readonly<Record<MarkOrigin | 'several', string>> = {
  plugin: 'Plugin',
  clipped: 'Clipped',
  you: 'Yours',
  several: 'Several sources',
};

/**
 * Places `marks` on a recording of `durationSeconds`.
 *
 * One pass over the marks and one over the occupied columns, so a recording
 * with ten thousand marks costs ten thousand comparisons once — memoised on the
 * marks and the duration by whoever draws it — rather than ten thousand nodes on
 * every render.
 *
 * The order the marks arrive in is not relied on. `library_events` answers
 * `ORDER BY at_nanos`, but the earliest mark in a column is found by comparison
 * rather than by taking the first one seen, so a reordering upstream changes
 * nothing here.
 */
export function recordingTimeline(
  marks: readonly EventMark[],
  durationSeconds: number,
  columns: number = TIMELINE_COLUMNS,
): RecordingTimelineView {
  if (!Number.isFinite(durationSeconds) || durationSeconds <= 0 || columns < 1) {
    // Nothing to place them against. The screen does not draw a timeline until
    // the media element has reported a length, so this is the state before the
    // first `loadedmetadata` rather than a failure.
    return { markers: [], total: marks.length, offRecording: 0 };
  }

  /** The earliest mark in each occupied column, and what else is in it. */
  const occupied = new Map<number, Column>();
  let offRecording = 0;

  for (const mark of marks) {
    const atSeconds = mark.at / NANOS_PER_SECOND;
    if (atSeconds < 0 || atSeconds > durationSeconds) {
      offRecording += 1;
      continue;
    }
    // The last column holds the final instant, so a mark exactly at the end of
    // the recording is in it rather than one column past it.
    const column = Math.min(columns - 1, Math.floor((atSeconds / durationSeconds) * columns));
    const found = occupied.get(column);
    if (found === undefined) {
      occupied.set(column, {
        atSeconds,
        mark,
        count: 1,
        origins: new Set([describeMark(mark).origin]),
      });
      continue;
    }
    found.count += 1;
    found.origins.add(describeMark(mark).origin);
    if (atSeconds < found.atSeconds) {
      found.atSeconds = atSeconds;
      found.mark = mark;
    }
  }

  const markers = [...occupied.entries()]
    .sort(([left], [right]) => left - right)
    .map(([column, found]) => markerOf(column, found, durationSeconds));

  return { markers, total: marks.length, offRecording };
}

/** What one column of the recording accumulated. */
interface Column {
  /** The earliest position in it, in seconds. */
  atSeconds: number;
  /** The mark at that position. */
  mark: EventMark;
  /** How many marks fell in this column. */
  count: number;
  /** Every origin among them. */
  readonly origins: Set<MarkOrigin>;
}

/** One occupied column, as the marker drawn for it. */
function markerOf(column: number, found: Column, durationSeconds: number): TimelineMarker {
  const [only] = [...found.origins];
  return {
    key: `${String(column)}-${found.mark.kind}-${String(found.mark.at)}`,
    atSeconds: found.atSeconds,
    fraction: found.atSeconds / durationSeconds,
    mark: found.mark,
    count: found.count,
    origin: found.origins.size === 1 && only !== undefined ? only : 'several',
  };
}

/**
 * What a screen reader is given for one marker.
 *
 * Every marker names three things: what happened, **when**, and **who said so**.
 * The when is a timecode to the millisecond because it is also the claim about
 * where clicking seeks to — two marks two hundred milliseconds apart announced
 * as the same time would make a wrong seek unreadable — and the who is a phrase
 * rather than an identifier, because a screen reader saying
 * "counter-strike-2" does not say that a plugin claimed it.
 *
 * This is the first of the two signals that are not colour that issue #65's
 * third criterion asks for; the shape the marker is drawn as is the second
 * (`RecordingTimeline.tsx`), and the legend above the lane says all of it in
 * words again.
 */
export function markerName(marker: TimelineMarker): string {
  const { label, by } = describeMark(marker.mark);
  const when = formatTimecode(marker.mark.at);
  const others =
    marker.count === 1
      ? ''
      : ` and ${String(marker.count - 1)} more mark${marker.count === 2 ? '' : 's'} here`;
  const who = marker.origin === 'several' ? 'from several sources' : by;
  return `${label} at ${when}${others}, ${who}`;
}

/** How many markers of each origin the lane is drawing. */
export interface OriginCount {
  /** Which of the four. */
  readonly origin: MarkOrigin | 'several';
  /** How many markers carry it. */
  readonly markers: number;
  /** How many of the recording's marks are under those markers. */
  readonly marks: number;
}

/**
 * The legend above the lane: every origin drawn, and how much of it there is.
 *
 * A fixed order rather than the order they were met in, so that the legend does
 * not rearrange itself between two recordings of the same game — and only
 * origins that are actually on this recording, because a legend listing a row
 * with nothing under it is the empty lane this screen refuses to draw.
 */
export function originsPresent(markers: readonly TimelineMarker[]): readonly OriginCount[] {
  const counted = new Map<MarkOrigin | 'several', { markers: number; marks: number }>();
  for (const marker of markers) {
    const found = counted.get(marker.origin) ?? { markers: 0, marks: 0 };
    counted.set(marker.origin, { markers: found.markers + 1, marks: found.marks + marker.count });
  }
  const order: readonly (MarkOrigin | 'several')[] = ['plugin', 'clipped', 'you', 'several'];
  return order.flatMap((origin) => {
    const found = counted.get(origin);
    return found === undefined ? [] : [{ origin, markers: found.markers, marks: found.marks }];
  });
}

/** Where a screen stands with one recording's marks. */
export type MarksRead =
  /** Nothing was asked, because this screen has no library identifier to ask with. */
  | { readonly state: 'unasked' }
  /** A round trip is in flight. */
  | { readonly state: 'reading' }
  /** The recorder answered. `marks` may legitimately be empty. */
  | { readonly state: 'read'; readonly marks: readonly EventMark[] }
  /** The recorder refused, or there was no recorder to ask. */
  | { readonly state: 'unread'; readonly problem: LibraryProblem };

/**
 * Reads one recording's marks, and follows the recording as the screen changes.
 *
 * `null` asks for nothing. That is what the playback screen passes when the
 * identifier in the address is **not** the library's: the Library hands over its
 * own integer key when the Play button is pressed, and `library_events` parses
 * exactly that — `apps/recorder/src/library.rs` turns it into an `i64` before it
 * opens the database — while the recorder's own `recording_id` for a live or an
 * interrupted recording is a different identifier entirely. Asking with the
 * wrong one would spend a round trip to be told the parameters were invalid, and
 * put "your library could not be read" on a screen whose library is fine.
 *
 * The answer carries which recording it was about, and the view is derived by
 * comparing that against the recording being drawn now — the arrangement
 * `preview.ts` uses, and for the same reason: a reply that arrives after
 * somebody has moved to another recording is a reply about a file this screen is
 * no longer showing, and drawing another recording's marks would be worse than
 * drawing none.
 */
export function useRecordingMarks(recording: string | null): MarksRead {
  const [answer, setAnswer] = useState<Answer | null>(null);

  useEffect(() => {
    if (recording === null) {
      return;
    }
    let current = true;
    readEvents(recording)
      .then((lane: LibraryEventLane) => {
        if (current) {
          setAnswer({ recording, result: { state: 'read', marks: lane.marks } });
        }
      })
      .catch((thrown: unknown) => {
        if (current) {
          // The recorder's own sentence rather than one invented here, which is
          // the rule every read in `library.ts` keeps (AGENTS.md section 15).
          setAnswer({ recording, result: { state: 'unread', problem: asProblem(thrown) } });
        }
      });
    return (): void => {
      current = false;
    };
  }, [recording]);

  if (recording === null) {
    return { state: 'unasked' };
  }
  return answer !== null && answer.recording === recording ? answer.result : { state: 'reading' };
}

/** One round trip's outcome, and which recording it was about. */
interface Answer {
  /** The recording asked about. */
  readonly recording: string;
  /** How it came out: either of the two ways a round trip can end. */
  readonly result:
    | { readonly state: 'read'; readonly marks: readonly EventMark[] }
    | { readonly state: 'unread'; readonly problem: LibraryProblem };
}
