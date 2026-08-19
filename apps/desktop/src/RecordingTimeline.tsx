import type { ReactNode } from 'react';
import { useMemo, useState } from 'react';

import { describeProblem, headlineProblem } from './library';
import type { MarkOrigin } from './events';
import {
  markerName,
  originsPresent,
  ORIGIN_SHAPES,
  ORIGIN_WORDS,
  recordingTimeline,
  TIMELINE_COLUMNS,
  type MarksRead,
} from './recordingMarks';
import { formatTimecode, NANOS_PER_SECOND } from './timecode';

/**
 * A recording's timeline, and the marks on it (issue #65).
 *
 * # Why it is on the playback screen and not in the Editor
 *
 * Because this is where a recording is. A timeline whose whole purpose is to
 * seek needs something that plays, and the playback screen has had one since
 * issue #304; the Editor has a document, and its own event lane over
 * edit-document time. The two share the vocabulary rather than the placement
 * (`events.ts`): they count different clocks, which is the distinction
 * `docs/desktop-ui.md` calls "the whole reason that screen is hard" — and it is
 * the same division that makes the Editor's waveforms a slice of a recording per
 * segment rather than one picture per lane (issue #66).
 *
 * # What tells one mark's source from another's
 *
 * Three things, and only the third is a colour, so removing colour altogether
 * loses nothing (AGENTS.md section 46):
 *
 * 1. **A word in the accessible name.** "reported by the counter-strike-2
 *    plugin", "marked by Clipped", "labelled by you" — `markerName`.
 * 2. **A shape.** A plugin's mark is a triangle, Clipped's own is a square,
 *    yours is a diamond, and a marker covering marks that disagree is a cross.
 *    {@link ORIGIN_SHAPES} is the whole of that decision, and the shapes differ
 *    in outline rather than in fill so they survive a monochrome display.
 * 3. **A legend**, above the lane, naming each origin that is on this recording
 *    in words beside the shape it is drawn as, with what it accounts for.
 *
 * That is the pattern PR #652 established for a per-game override: a word, and
 * a second signal that is not a colour.
 *
 * # What is not drawn
 *
 * A lane per source. SPEC.md section 18 asks for marks from bookmarks,
 * clipping, microphone activity and screenshots as well as from integrations,
 * and **none of those four is written as a game event by anything in this
 * build** — the note under the lane says which and why. Four empty lanes would
 * look like four features that had stopped working.
 */

/** What a recording's timeline is drawn from. */
export interface RecordingTimelineProps {
  /** Where the screen got to with the recorder's answer. */
  readonly read: MarksRead;
  /**
   * The recording's length in seconds, as the media element measured it, or
   * `null` before it has reported one.
   *
   * The element's own measurement rather than the library's, because it is the
   * timeline the seek actually happens on: a marker drawn at a fraction of one
   * length and seeking to a position on another would be drawn in the wrong
   * place by exactly their difference. It is also the rule the rest of this
   * screen keeps — the length and the position are the player's.
   */
  readonly durationSeconds: number | null;
  /** What to call the recording, for a screen reader. */
  readonly of: string;
  /** One sentence for why nothing was asked, drawn when nothing was. */
  readonly unasked: string;
  /** Puts the player at `seconds`. */
  readonly onSeek: (seconds: number) => void;
}

/**
 * What has no producer, said once, under the lane.
 *
 * Named individually rather than as "some sources are missing", because each is
 * a thing somebody may be looking for and each is missing for its own reason
 * (AGENTS.md sections 27 and 28).
 */
const NO_PRODUCER =
  'Only a game integration writes a mark today. A bookmark is written beside the recording rather than as an event (#64); saving a clip and taking a screenshot are recorded against the sitting, not the recording; and microphone activity is not written down at all. None of those four reaches this timeline yet — issue #71.';

/** One recording's marks, in whichever state the read is in. */
export function RecordingTimeline({
  read,
  durationSeconds,
  of,
  unasked,
  onSeek,
}: RecordingTimelineProps): ReactNode {
  /** The marker last clicked, so that a seek says so as well as doing it. */
  const [sought, setSought] = useState<string | null>(null);

  const marks = read.state === 'read' ? read.marks : null;
  /*
   * Held, because it is a walk over every mark of the recording and this
   * component re-renders whenever the player reports a new length or somebody
   * presses a marker. Ten thousand marks placed once per recording is nothing;
   * ten thousand placed on every click is the cost the bucketing exists to
   * avoid.
   */
  const placed = useMemo(
    () => (marks === null ? null : recordingTimeline(marks, durationSeconds ?? 0)),
    [marks, durationSeconds],
  );
  const origins = useMemo(() => (placed === null ? [] : originsPresent(placed.markers)), [placed]);

  if (read.state === 'unasked') {
    return <p className="clipped-panel__body">{unasked}</p>;
  }

  if (read.state === 'reading') {
    return <p className="clipped-panel__body">Reading the marks on this recording…</p>;
  }

  if (read.state === 'unread') {
    // The recorder's own sentence, headed the way every other library read on
    // every other screen heads one (AGENTS.md section 45).
    return (
      <>
        <p className="clipped-panel__body">{headlineProblem(read.problem)}</p>
        <p className="clipped-muted">{describeProblem(read.problem)}</p>
      </>
    );
  }

  if (read.marks.length === 0) {
    // A successful read of a recording with nothing on it, which is the
    // ordinary state of a recording of a game Clipped has no integration for.
    return (
      <p className="clipped-panel__body">
        No marks on this recording. <span className="clipped-muted">{NO_PRODUCER}</span>
      </p>
    );
  }

  if (durationSeconds === null || placed === null) {
    // Marks with nothing to place them against. Saying so beats a lane drawn at
    // some assumed length, which would put every mark in the wrong place.
    return (
      <p className="clipped-panel__body">
        {read.marks.length} mark{read.marks.length === 1 ? '' : 's'} on this recording. The player
        has not reported its length yet, so there is nothing to place them along.
      </p>
    );
  }

  return (
    <figure className="clipped-marks">
      <figcaption className="clipped-muted">
        {placed.total} mark{placed.total === 1 ? '' : 's'} on this recording, at the positions the
        recorder placed them in this file. Choosing one moves the player to it.
      </figcaption>

      <ul className="clipped-marks__legend" aria-label="What the marks are">
        {origins.map((entry) => (
          <li className="clipped-marks__legend-entry" key={entry.origin}>
            <Glyph origin={entry.origin} />
            <span>
              {ORIGIN_WORDS[entry.origin]}{' '}
              <span className="clipped-muted">
                — {entry.marks} mark{entry.marks === 1 ? '' : 's'}
              </span>
            </span>
          </li>
        ))}
      </ul>

      <ul className="clipped-marks__lane" aria-label={`Marks on ${of}`}>
        {placed.markers.map((marker) => (
          <li
            className="clipped-marks__at"
            key={marker.key}
            style={{ left: `${String(marker.fraction * 100)}%` }}
          >
            <button
              type="button"
              className="clipped-marks__mark"
              aria-label={markerName(marker)}
              onClick={() => {
                setSought(markerName(marker));
                onSeek(marker.atSeconds);
              }}
            >
              <Glyph origin={marker.origin} />
            </button>
          </li>
        ))}
      </ul>

      <p className="clipped-marks__span">
        <span>{formatTimecode(0)}</span>
        <span>{formatTimecode(durationSeconds * NANOS_PER_SECOND)}</span>
      </p>

      {/*
       * A marker is a few pixels wide and the player it moves is above it, so
       * nothing else on screen confirms that a click landed where it was aimed.
       * Polite rather than assertive: it follows a deliberate action.
       */}
      <p className="clipped-marks__sought" aria-live="polite">
        {sought === null ? '' : `Moved to ${sought}`}
      </p>

      {placed.offRecording > 0 && (
        <p className="clipped-panel__body">
          {placed.offRecording} mark{placed.offRecording === 1 ? ' is' : 's are'} outside this
          recording and {placed.offRecording === 1 ? 'is' : 'are'} not drawn. The library placed{' '}
          {placed.offRecording === 1 ? 'it' : 'them'} before the file starts or after it ends.
        </p>
      )}

      <p className="clipped-muted">
        {NO_PRODUCER} The recording is divided into {TIMELINE_COLUMNS} columns and one marker is
        drawn per column that has anything in it, so a long recording stays usable; where marks fall
        inside one column, a single marker carries them all and says how many.
      </p>
    </figure>
  );
}

/**
 * The outline one origin is drawn as.
 *
 * `aria-hidden`, because the marker's own name already says who reported it in
 * words: a shape announced as well would be the same fact twice, and there is no
 * wording for "triangle" that means anything to somebody who cannot see it.
 */
function Glyph({ origin }: { readonly origin: MarkOrigin | 'several' }): ReactNode {
  return (
    <svg
      className={`clipped-marks__glyph clipped-marks__glyph--${origin}`}
      viewBox="0 0 12 12"
      aria-hidden="true"
      focusable="false"
    >
      <path d={ORIGIN_SHAPES[origin]} />
    </svg>
  );
}
