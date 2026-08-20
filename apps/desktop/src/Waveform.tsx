import type { Preview, PreviewTrack } from '@clipped/shared';
import type { ReactNode } from 'react';

import { bucketCount, envelope, LANE } from './waveformOutline';

/**
 * A recording's sound, drawn from the peaks the recorder answered with
 * (issue #448, and `docs/waveforms.md`).
 *
 * # Why an inline `<svg>` and not a canvas
 *
 * A canvas is a bitmap the accessibility tree cannot see and the window has to
 * redraw itself on every resize and every change of scale — and this window
 * runs at 200% on most of the machines it is for. An `<svg>` with a
 * `viewBox` scales without being told, and it can carry a name, which is the
 * whole of what a screen reader gets from a waveform.
 *
 * It is one `<path>` per track rather than one element per bucket. A minute of
 * audio at the width of this screen is on the order of a thousand buckets, and
 * a thousand `<rect>`s is a thousand nodes for a picture that never changes
 * once it is drawn.
 *
 * # Why never a flat line
 *
 * `docs/waveforms.md` is explicit about it: a flat line is indistinguishable
 * from silence, so a track with no peaks is drawn as **nothing plus a
 * sentence**, never as a line through the middle. The same rule is why a
 * `pending` or an `unavailable` waveform says so in words instead of drawing an
 * empty lane, which is `docs/desktop-ui.md`'s "No waveform" contract for the
 * Editor's lanes, kept here.
 *
 * # What this does not do
 *
 * It is not a timeline. There is no playhead over it and nothing to scrub; a
 * control drawn here that did nothing would be AGENTS.md section 27, and the
 * screen that puts peaks under a playhead is the Editor (issue #694). What is
 * drawn is what the peaks say and nothing else. The recording's marks are a
 * strip of their own below this one — `RecordingTimeline.tsx`, issue #65 —
 * because they are placed by the recorder in the file's own time and have
 * nothing to do with what a bucket of peaks says.
 *
 * # The other screen that draws these
 *
 * The Editor's lanes, since issue #66. They share `waveformOutline.ts` and one
 * CSS declaration and nothing else: this draws a row per sound track of one
 * *file*, and that draws a row per audio track of an *edit* — in output time,
 * at a zoom, one picture per segment holding the slice of a recording that
 * segment uses (`editor/lanePeaks.ts`). Sharing the arithmetic is the point;
 * a second copy of it is how the two would start disagreeing about what silence
 * looks like.
 */

/** What a waveform is drawn for. */
export interface WaveformProps {
  /**
   * What the recorder answered, or `null` while nothing has been asked or the
   * recorder cannot be asked.
   */
  readonly preview: Preview | null;
  /** What to call the recording, for a screen reader. */
  readonly of: string;
}

/** One recording's sound, in whichever state its peaks are in. */
export function Waveform({ preview, of }: WaveformProps): ReactNode {
  if (preview === null) {
    return null;
  }

  if (preview.state === 'pending') {
    // The ordinary state of a recording that has just been written. It says
    // what is happening rather than drawing a lane that would fill in later
    // without warning, and it never draws a line (`docs/waveforms.md`).
    return <p className="clipped-panel__body">No waveform yet. Clipped is reading the sound.</p>;
  }

  if (preview.state === 'unavailable') {
    return (
      <p className="clipped-panel__body">
        No waveform.{preview.reason === undefined ? '' : ` ${preview.reason}`}
      </p>
    );
  }

  const tracks = preview.tracks ?? [];
  if (tracks.length === 0) {
    // A supported answer and not a failure. This said "every recording Clipped
    // writes today has no sound at all, until multi-track audio (issue #180)" —
    // true when it was written and false since #180 closed. A recording now
    // carries a track per source, and `tests/audio/track_isolation.rs` measures
    // three of them.
    //
    // The state is still reachable, which is why the branch stays: a recording
    // made with `--microphone none` and system audio off has no sound in it, and
    // so does one whose audio the analyser could not decode
    // (`crates/waveform/src/analyse.rs`).
    return <p className="clipped-panel__body">This recording has no sound in it.</p>;
  }

  return (
    <figure className="clipped-waveform">
      <figcaption className="clipped-muted">
        The whole recording, one row per sound track. Drawn from the peaks the recorder computed;
        nothing here has decoded any audio.
      </figcaption>
      {tracks.map((track, position) => (
        <TrackLane key={track.index} track={track} of={of} position={position} />
      ))}
    </figure>
  );
}

/** One track's row. */
function TrackLane({
  track,
  of,
  position,
}: {
  readonly track: PreviewTrack;
  readonly of: string;
  readonly position: number;
}): ReactNode {
  // A track the recording did not name is shown by its position, the same way
  // the track selector above it does, rather than being given a name here:
  // what is on it is a fact about the file and this window has not heard it.
  const label = track.name ?? `Audio ${position + 1}`;
  const outline = envelope(track);

  return (
    <div className="clipped-waveform__lane">
      <span className="clipped-waveform__name">{label}</span>
      {outline === null ? (
        // No peaks at all in a track the recorder called ready. Nothing is
        // drawn, because the one thing that must not be drawn is a flat line.
        <span className="clipped-muted">No waveform for this track</span>
      ) : (
        <svg
          className="clipped-waveform__lane-picture"
          viewBox={`0 0 ${bucketCount(track)} ${LANE}`}
          preserveAspectRatio="none"
          role="img"
          aria-label={`Sound of ${label} in ${of}`}
        >
          <path d={outline} />
        </svg>
      )}
    </div>
  );
}
