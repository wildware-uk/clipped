import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type MouseEvent,
  type ReactNode,
} from 'react';

import { recordingOf, type AudioTrack, type EditDocument } from './document';
import {
  anySoloed,
  boundaries,
  formatTickLabel,
  formatTimecode,
  locate,
  nextBoundary,
  NANOS_PER_SECOND,
  overlaysAt,
  outputNanosOf,
  previousBoundary,
  scrollToShow,
  ticks,
  tickIntervalNanos,
  trackOutput,
  ZOOM_STEPS,
} from './timeline';

/**
 * The editor's shell: a timeline, the audio tracks under it, and what is at the
 * playhead (issue #83).
 *
 * # What this draws, and what it deliberately does not
 *
 * SPEC.md section 19 asks for a deliberately lightweight editor — "this is not
 * Premiere" — with the audio appearing as individual editable lanes. What is
 * here is the shell of that: the layout, the timeline with its ruler and
 * playhead, one lane per audio track, zoom, and the wiring that shows a
 * document.
 *
 * The **operations** are not here. Trim, split and delete are built, in
 * `crates/edit` (issue #84); the mix is #85, framing and speed #86, overlays
 * #87, combining recordings #88 and export #89. Each of those tickets owns its
 * own control, and drawing a Split button here that could not reach the crate
 * behind it is the control with nothing behind it AGENTS.md section 27 forbids.
 * What this screen does is show a document and let somebody move about inside
 * it accurately, which is what every one of those controls will be aimed with.
 *
 * # Two things are absent rather than drawn
 *
 * Neither is a gap in this component; both are things this window cannot reach
 * at all, and section 27 says an unavailable feature is represented as
 * unavailable rather than mocked up:
 *
 * - **the picture.** A frame lives in a recording, and this window has no
 *   file-system permission and no command that serves one.
 * - **the waveforms.** `crates/waveform` computes the peaks (#66) and writes
 *   them beside the recordings; the window cannot read a file, so a lane says
 *   it has no waveform rather than drawing a flat line, which is exactly what
 *   `docs/waveforms.md` says a missing waveform must never look like.
 *
 * # Nothing here changes anything
 *
 * The playhead and the zoom are this component's own state. No path from this
 * file writes a document, let alone a recording (AGENTS.md sections 56 and 57).
 */

/** What a keyboard step moves the playhead by. */
const STEP_NANOS = NANOS_PER_SECOND / 10;

/** What a step moves it by with Shift held. */
const COARSE_STEP_NANOS = NANOS_PER_SECOND;

/** What the editor is given. */
export interface ClipEditorProps {
  /** The document to show. */
  readonly clip: EditDocument;
  /** How long it lasts, in nanoseconds; the screen has already read it. */
  readonly durationNanos: number;
}

/** A position on the timeline as a fraction of the clip, for drawing. */
function fractionOf(nanos: number, durationNanos: number): number {
  return durationNanos <= 0 ? 0 : nanos / durationNanos;
}

/**
 * A fraction as a CSS percentage.
 *
 * Every distance the timeline draws is a proportion of the clip rather than a
 * length, which is why none of them is a token: a percentage scales with the
 * window, and `stylesheet.test.ts` deliberately leaves percentages outside the
 * gate for exactly this reason.
 */
function percent(fraction: number): string {
  return `${String(Math.min(Math.max(fraction, 0), 1) * 100)}%`;
}

/** How a track's level reads once mute and solo are resolved. */
function describeLevel(track: AudioTrack, soloed: boolean): string {
  const output = trackOutput(track, soloed);
  if (!output.audible) {
    return track.muted ? 'Muted' : 'Silent while another track is soloed';
  }
  if (output.gainDb === 0) {
    return 'As recorded';
  }
  return `${output.gainDb > 0 ? '+' : ''}${output.gainDb.toFixed(1)} dB`;
}

/** The editor. */
export function ClipEditor({ clip, durationNanos }: ClipEditorProps): ReactNode {
  const [playheadNanos, setPlayheadNanos] = useState(0);
  const [zoom, setZoom] = useState(ZOOM_STEPS[0] ?? 1);
  const scroller = useRef<HTMLDivElement>(null);

  /*
   * The boundaries a cut can be at, which Page Up and Page Down step between.
   * Held rather than recomputed so that the key handler below is not rebuilt on
   * every render — and `?? [0]` because a document whose segments cannot be
   * read has no boundaries; the screen refuses one before it reaches here, and
   * this is what keeps that a screen decision rather than a crash.
   */
  const cuts = useMemo(() => boundaries(clip) ?? [0], [clip]);
  const placement = locate(clip, playheadNanos);
  const soloed = anySoloed(clip);
  const interval = tickIntervalNanos(durationNanos, zoom);
  const marks = ticks(durationNanos, zoom);
  const overlays = overlaysAt(clip, playheadNanos);

  const seek = useCallback(
    (to: number) => {
      setPlayheadNanos(Math.min(Math.max(Math.round(to), 0), durationNanos));
    },
    [durationNanos],
  );

  const changeZoom = useCallback((by: number) => {
    setZoom((current) => {
      const at = ZOOM_STEPS.indexOf(current);
      return ZOOM_STEPS[Math.min(Math.max(at + by, 0), ZOOM_STEPS.length - 1)] ?? current;
    });
  }, []);

  /*
   * The playhead is kept on screen rather than left behind by a zoom or an
   * arrow key. `scrollToShow` is the rule; this is the only place it is
   * applied, and it does nothing until the element has been laid out, which in
   * a test environment with no layout means it does nothing at all.
   */
  useEffect(() => {
    const element = scroller.current;
    if (!element || element.clientWidth <= 0) {
      return;
    }
    element.scrollLeft = scrollToShow(
      fractionOf(playheadNanos, durationNanos),
      element.scrollWidth,
      element.clientWidth,
      element.scrollLeft,
    );
  }, [playheadNanos, durationNanos, zoom]);

  /*
   * Every core action of this screen is a key, because a timeline that needs
   * precise dragging is unusable for anybody who cannot do it (AGENTS.md
   * section 46). `docs/desktop-ui.md` carries the same table.
   */
  const onKeyDown = useCallback(
    (event: KeyboardEvent<HTMLDivElement>) => {
      const step = event.shiftKey ? COARSE_STEP_NANOS : STEP_NANOS;
      switch (event.key) {
        case 'ArrowLeft':
          seek(playheadNanos - step);
          break;
        case 'ArrowRight':
          seek(playheadNanos + step);
          break;
        case 'Home':
          seek(0);
          break;
        case 'End':
          seek(durationNanos);
          break;
        case 'PageUp':
          seek(previousBoundary(cuts, playheadNanos));
          break;
        case 'PageDown':
          seek(nextBoundary(cuts, playheadNanos));
          break;
        case '+':
        case '=':
          changeZoom(1);
          break;
        case '-':
          changeZoom(-1);
          break;
        case '0':
          setZoom(ZOOM_STEPS[0] ?? 1);
          break;
        default:
          return;
      }
      event.preventDefault();
    },
    [changeZoom, cuts, durationNanos, playheadNanos, seek],
  );

  /*
   * Clicking the timeline seeks to where it was clicked. It is an alternative
   * to the keys above and never the only way to do anything, and it is guarded
   * against a zero-width box so that a click before layout cannot send the
   * playhead to the start.
   */
  const onClick = useCallback(
    (event: MouseEvent<HTMLDivElement>) => {
      const box = event.currentTarget.getBoundingClientRect();
      if (box.width <= 0) {
        return;
      }
      seek(((event.clientX - box.left) / box.width) * durationNanos);
    },
    [durationNanos, seek],
  );

  return (
    <section className="clipped-editor" aria-label={`Clip: ${clip.title}`}>
      <h2 className="clipped-screen__heading">{clip.title}</h2>

      <div className="clipped-editor__top">
        <div className="clipped-editor__frame">
          <p className="clipped-editor__frame-note">
            No picture. A frame is in the recording, and this window cannot open one: it has no
            file-system permission and no command that serves frames. Issue #306.
          </p>
        </div>

        <dl className="clipped-editor__facts" aria-label="At the playhead">
          <dt>Playhead</dt>
          <dd>
            {formatTimecode(playheadNanos)} of {formatTimecode(durationNanos)}
          </dd>
          {placement === null ? (
            <>
              <dt>Material</dt>
              <dd>
                {clip.segments.length === 0
                  ? 'This clip is empty. Nothing plays at any position.'
                  : 'The end of the clip. Nothing plays here.'}
              </dd>
            </>
          ) : (
            <>
              <dt>Recording</dt>
              <dd>{recordingOf(clip, placement.source) ?? `source ${String(placement.source)}`}</dd>
              <dt>Source time</dt>
              <dd>{formatTimecode(placement.sourceNanos)}</dd>
              <dt>Segment</dt>
              <dd>
                {placement.segment + 1} of {clip.segments.length}, starting at{' '}
                {formatTimecode(placement.segmentStartNanos)}
              </dd>
              <dt>Speed</dt>
              <dd>{describeSpeed(clip, placement.segment)}</dd>
            </>
          )}
          <dt>Text on screen</dt>
          <dd>
            {overlays.length === 0 ? 'None' : overlays.map((overlay) => overlay.text).join(', ')}
          </dd>
        </dl>
      </div>

      <div className="clipped-editor__transport">
        <p className="clipped-editor__position">
          <strong>{formatTimecode(playheadNanos)}</strong> of {formatTimecode(durationNanos)}
        </p>
        <div className="clipped-editor__zoom">
          <span className="clipped-muted">Zoom {zoom}&times;</span>
          <button
            type="button"
            className="clipped-btn clipped-btn--secondary"
            onClick={() => {
              changeZoom(-1);
            }}
            disabled={zoom === ZOOM_STEPS[0]}
          >
            Zoom out
          </button>
          <button
            type="button"
            className="clipped-btn clipped-btn--secondary"
            onClick={() => {
              changeZoom(1);
            }}
            disabled={zoom === ZOOM_STEPS[ZOOM_STEPS.length - 1]}
          >
            Zoom in
          </button>
          <button
            type="button"
            className="clipped-btn clipped-btn--secondary"
            onClick={() => {
              setZoom(ZOOM_STEPS[0] ?? 1);
            }}
            disabled={zoom === ZOOM_STEPS[0]}
          >
            Fit
          </button>
        </div>
      </div>

      <div className="clipped-editor__timeline">
        {/*
         * The lanes' names sit outside the scroller, so that they are still
         * there when the timeline is scrolled — and they are a real list rather
         * than decoration, because a track's name and what it is contributing
         * are the two things about it worth reading. Mute and solo are said in
         * words, never by colour alone (AGENTS.md section 46).
         */}
        <ul className="clipped-timeline__labels" aria-label="Tracks">
          <li
            className="clipped-timeline__label clipped-timeline__label--ruler"
            aria-hidden="true"
          />
          <li className="clipped-timeline__label">
            <span className="clipped-timeline__track-name">Video</span>
            <span className="clipped-muted">
              {clip.segments.length} {clip.segments.length === 1 ? 'segment' : 'segments'}
            </span>
          </li>
          {clip.audio_tracks.map((track) => (
            <li className="clipped-timeline__label" key={track.name}>
              <span className="clipped-timeline__track-name">{track.name}</span>
              <span className="clipped-muted">{describeLevel(track, soloed)}</span>
            </li>
          ))}
        </ul>

        <div className="clipped-timeline" ref={scroller}>
          <div className="clipped-timeline__content" style={{ width: `${String(zoom * 100)}%` }}>
            <div className="clipped-timeline__ruler" aria-hidden="true">
              {marks.map((mark) => (
                <span
                  className="clipped-timeline__tick"
                  key={mark}
                  style={{ left: percent(fractionOf(mark, durationNanos)) }}
                >
                  {formatTickLabel(mark, interval)}
                </span>
              ))}
            </div>

            {/*
             * The playhead is a slider, which is what it is: a value between two
             * ends that the arrow keys, Home and End move. Taking the platform's
             * own role rather than inventing one is what makes it reach a screen
             * reader at all, and `aria-valuetext` is what makes it say a
             * timecode rather than a number of milliseconds.
             */}
            <div
              role="slider"
              tabIndex={0}
              aria-label="Playhead"
              aria-valuemin={0}
              aria-valuemax={Math.round(durationNanos / 1_000_000)}
              aria-valuenow={Math.round(playheadNanos / 1_000_000)}
              aria-valuetext={`${formatTimecode(playheadNanos)} of ${formatTimecode(durationNanos)}`}
              className="clipped-timeline__lanes"
              onKeyDown={onKeyDown}
              onClick={onClick}
            >
              <div className="clipped-timeline__lane clipped-timeline__lane--video">
                {clip.segments.map((segment, index) => {
                  const length = outputNanosOf(segment) ?? 0;
                  const start = cuts[index] ?? 0;
                  return (
                    <span
                      className="clipped-timeline__segment"
                      key={`${String(segment.source)}-${String(segment.span.start)}-${String(index)}`}
                      style={{
                        left: percent(fractionOf(start, durationNanos)),
                        width: percent(fractionOf(length, durationNanos)),
                      }}
                    >
                      {recordingOf(clip, segment.source) ?? `source ${String(segment.source)}`}
                    </span>
                  );
                })}
              </div>

              {clip.audio_tracks.map((track) => (
                <div className="clipped-timeline__lane" key={track.name}>
                  <span className="clipped-timeline__absent">No waveform</span>
                </div>
              ))}

              <div
                className="clipped-timeline__playhead"
                style={{ left: percent(fractionOf(playheadNanos, durationNanos)) }}
              />
            </div>
          </div>
        </div>
      </div>

      <p className="clipped-editor__note clipped-muted">
        No lane has a waveform. The peaks are computed from the recording and written beside it
        (issue #66); this window cannot read a file, so a lane is drawn empty rather than as a flat
        line, which would be indistinguishable from silence. Issue #306.
      </p>
    </section>
  );
}

/** How a segment's speed reads, so that "unchanged" is said rather than 1/1. */
function describeSpeed(clip: EditDocument, index: number): string {
  const segment = clip.segments[index];
  if (!segment) {
    return 'Unchanged';
  }
  const { numerator, denominator } = segment.speed;
  return numerator === denominator
    ? 'Unchanged'
    : `${String(numerator)}/${String(denominator)} of the recording's own`;
}
