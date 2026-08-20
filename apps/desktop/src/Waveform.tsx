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
 * # The playhead, and seeking (issue #694)
 *
 * Drawn only when a caller gives it a length and a position, and clickable only
 * when a caller gives it somewhere to seek to. That is not defensiveness: this
 * component is also drawn for a recording nothing is playing — the Library
 * screen's rows, a poster with no element behind it — and a playhead over
 * peaks nothing is moving through would be a control that lies about what it
 * is. A picture without those props is exactly what it was before.
 *
 * The lane whose track is being played is marked, because a media element plays
 * one track at a time and a screen showing four identical lanes says nothing
 * about which one you are hearing.
 *
 * What is still not here: the recording's marks are a strip of their own below
 * this one — `RecordingTimeline.tsx`, issue #65 — because they are placed by
 * the recorder in the file's own time and have nothing to do with what a bucket
 * of peaks says.
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
  /**
   * How long the recording is, in seconds, or `null` when nothing knows.
   *
   * The playhead needs it to place itself and seeking needs it to say where to
   * go, so without it neither is drawn. A container still being written reports
   * no usable length, which is one of the ways this is legitimately absent.
   */
  readonly durationSeconds?: number | null;
  /** Where the recording is being played, in seconds, or `null`. */
  readonly positionSeconds?: number | null;
  /**
   * The index of the track being played, or `null`.
   *
   * The track's own index as the file numbers them, not its position in the
   * list, because that is what the selector above chooses by and what the
   * recorder answers with.
   */
  readonly playingTrack?: number | null;
  /**
   * Called when somebody clicks or drags a lane, with where they pointed.
   *
   * Absent for a waveform with nothing to seek — then no lane takes a pointer
   * at all, rather than taking one and doing nothing.
   */
  readonly onSeek?: (seconds: number) => void;
}

/** One recording's sound, in whichever state its peaks are in. */
export function Waveform({
  preview,
  of,
  durationSeconds = null,
  positionSeconds = null,
  playingTrack = null,
  onSeek,
}: WaveformProps): ReactNode {
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
        <TrackLane
          key={track.index}
          track={track}
          of={of}
          position={position}
          durationSeconds={durationSeconds}
          positionSeconds={positionSeconds}
          playing={playingTrack !== null && track.index === playingTrack}
          onSeek={onSeek}
        />
      ))}
    </figure>
  );
}

/** One track's row. */
function TrackLane({
  track,
  of,
  position,
  durationSeconds,
  positionSeconds,
  playing,
  onSeek,
}: {
  readonly track: PreviewTrack;
  readonly of: string;
  readonly position: number;
  readonly durationSeconds: number | null;
  readonly positionSeconds: number | null;
  readonly playing: boolean;
  readonly onSeek?: ((seconds: number) => void) | undefined;
}): ReactNode {
  // A track the recording did not name is shown by its position, the same way
  // the track selector above it does, rather than being given a name here:
  // what is on it is a fact about the file and this window has not heard it.
  const label = track.name ?? `Audio ${position + 1}`;
  const outline = envelope(track);

  // A length of zero would put every position at the start and make seeking a
  // division by nothing, so it is treated as no length at all.
  const seekable = durationSeconds !== null && durationSeconds > 0;
  const buckets = bucketCount(track);
  const at =
    seekable && positionSeconds !== null && durationSeconds !== null
      ? Math.min(Math.max(positionSeconds / durationSeconds, 0), 1) * buckets
      : null;

  function seekFrom(event: { clientX: number; currentTarget: Element }): void {
    if (onSeek === undefined || !seekable || durationSeconds === null) {
      return;
    }
    const box = event.currentTarget.getBoundingClientRect();
    if (box.width === 0) {
      return;
    }
    const across = Math.min(Math.max((event.clientX - box.left) / box.width, 0), 1);
    onSeek(across * durationSeconds);
  }

  return (
    <div
      className={
        playing
          ? 'clipped-waveform__lane clipped-waveform__lane--playing'
          : 'clipped-waveform__lane'
      }
    >
      <span className="clipped-waveform__name">{label}</span>
      {outline === null ? (
        // No peaks at all in a track the recorder called ready. Nothing is
        // drawn, because the one thing that must not be drawn is a flat line.
        <span className="clipped-muted">No waveform for this track</span>
      ) : (
        <svg
          className={
            onSeek !== undefined && seekable
              ? 'clipped-waveform__lane-picture clipped-waveform__lane-picture--seekable'
              : 'clipped-waveform__lane-picture'
          }
          viewBox={`0 0 ${buckets} ${LANE}`}
          preserveAspectRatio="none"
          role="img"
          aria-label={
            playing ? `Sound of ${label} in ${of}, playing` : `Sound of ${label} in ${of}`
          }
          onPointerDown={
            onSeek === undefined || !seekable
              ? undefined
              : (event): void => {
                  // Captured so a drag that leaves the lane keeps seeking, the
                  // way a scrubber behaves everywhere else.
                  event.currentTarget.setPointerCapture(event.pointerId);
                  seekFrom(event);
                }
          }
          onPointerMove={
            onSeek === undefined || !seekable
              ? undefined
              : (event): void => {
                  if (event.buttons !== 0) {
                    seekFrom(event);
                  }
                }
          }
        >
          <path d={outline} />
          {at === null ? null : (
            // A line in the picture's own units, so it stretches with it. Drawn
            // last so it is over the outline rather than under it.
            <rect
              className="clipped-waveform__playhead"
              x={at}
              y={0}
              width={Math.max(buckets / 400, 1)}
              height={LANE}
            />
          )}
        </svg>
      )}
    </div>
  );
}
