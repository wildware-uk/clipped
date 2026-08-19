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
import { countByKind, describeKind, kindsPresent, type EventMark } from '../events';
import { ExportDialog } from './ExportDialog';
import { lanePieces, type LaneAbsence, type LanePiece, type PeaksOf } from './lanePeaks';
import { LANE } from '../waveformOutline';
import {
  boundaries,
  formatTickLabel,
  formatTimecode,
  locate,
  monitor,
  nextBoundary,
  NANOS_PER_SECOND,
  outputPositionsOf,
  overlaysAt,
  outputNanosOf,
  previousBoundary,
  scrollToShow,
  SOLO_NONE,
  ticks,
  tickIntervalNanos,
  toggleSolo,
  type Solo,
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
 * #87 and combining recordings #88. Each of those tickets owns its own control,
 * and drawing a Split button here that could not reach the crate behind it is
 * the control with nothing behind it AGENTS.md section 27 forbids. What this
 * screen does is show a document and let somebody move about inside it
 * accurately, which is what every one of those controls will be aimed with.
 *
 * The one control over the whole clip is **Export**, which opens the export
 * dialog (issue #90). Opening it is this component's own state, as the zoom is;
 * what the dialog says an export of this clip would be — and that no export can
 * be started from this window yet — is `ExportDialog`'s.
 *
 * # The waveforms under the lanes
 *
 * Real peaks, since issue #66: `crates/waveform` computes them beside each
 * recording and `open_preview` carries them, so a lane draws the sound of the
 * part of the recording its segments use. `lanePeaks.ts` is the arithmetic and
 * argues the hard half of it — the lane counts **edit-document** time and the
 * peaks are in **recording** time, so a clip trimmed from the middle of a
 * session draws a *slice*, and a clip cut from two recordings draws a piece per
 * segment from its own file's peaks.
 *
 * What has not changed is the rule a missing waveform is drawn by: **never a
 * flat line**, which `docs/waveforms.md` forbids by name because it is
 * indistinguishable from silence. A lane with nothing to draw says so in
 * words — and says which of "not yet", "not at all" and "this track takes
 * nothing from this recording" it is, because those send somebody to three
 * different places (AGENTS.md sections 27 and 45).
 *
 * # One thing is still absent rather than drawn
 *
 * **The picture.** A frame lives in a recording, and this window has no
 * file-system permission and no command that serves one. Section 27 says an
 * unavailable feature is represented as unavailable rather than mocked up.
 *
 * # Nothing here changes anything, with one exception
 *
 * The playhead, the zoom and which track is soloed are this component's own
 * state. No path from this file writes a document, let alone a recording
 * (AGENTS.md sections 56 and 57) — and solo is not an exception to that: it is
 * `Solo` (`crates/edit/src/audio.rs`), a value the editor listens with rather
 * than a field of the document, so soloing a track changes what is heard in
 * this window's preview and nothing that could be saved ([issue
 * #85](https://github.com/wildware-uk/clipped/issues/85)).
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
  /**
   * The game events of the recordings this clip draws on, each already placed
   * in one of them by `clipped_library::events` (issue #71).
   *
   * `null` and an empty list are deliberately different answers, and the lane
   * says which one it got. `[]` is "this recording had no events"; `null` is
   * "nobody has been asked", which is the only answer available in this build —
   * the window cannot read the library and there is no command that serves
   * events. Drawing an empty lane for the second would report a quiet session
   * where there had merely been no question (AGENTS.md section 27).
   */
  readonly events?: readonly EventMark[] | null;
  /**
   * Where each recording this clip draws on stands for peaks, from
   * `clipWaveforms.ts` (issue #66).
   *
   * `null` — the default — is "nobody has asked", which is not the same as any
   * of the four answers a lookup can give and is drawn differently: one "No
   * waveform" for the lane rather than a piece-by-piece account of a question
   * nothing put. A caller that has a recorder to ask always passes a lookup,
   * and the lookup answers `unasked` for a recorder that cannot make waveforms
   * (AGENTS.md section 27).
   */
  readonly waveforms?: PeaksOf | null;
}

/** One event mark, at the place on the edited timeline where it is drawn. */
interface PlacedMark {
  readonly mark: EventMark;
  readonly atNanos: number;
}

/** Where a clip's events are on it, and how many of them are not on it at all. */
interface ClipMarks {
  /** Every event that lands somewhere on this clip, earliest first. */
  readonly placed: readonly PlacedMark[];
  /**
   * Events the clip does not contain, because every segment that plays their
   * recording was trimmed past the moment.
   *
   * Counted rather than ignored: "four kills, two of them outside this clip" is
   * a different thing to be told from "two kills", and drawing the missing two
   * at the nearest frame would be a marker for footage the clip does not have
   * (AGENTS.md section 27).
   */
  readonly trimmedAway: number;
}

/**
 * Where a clip's events sit on its edited timeline.
 *
 * An event whose seconds the clip uses twice appears twice, which is what
 * `outputPositionsOf` answers a list for. Nothing here reads a kind: a mark
 * this build has no name for is placed exactly as a kill is.
 */
function placeMarks(clip: EditDocument, events: readonly EventMark[]): ClipMarks {
  const placed: PlacedMark[] = [];
  let trimmedAway = 0;

  for (const mark of events) {
    const positions = outputPositionsOf(clip, mark.recording, mark.at);
    if (positions.length === 0) {
      trimmedAway += 1;
      continue;
    }
    for (const atNanos of positions) {
      placed.push({ mark, atNanos });
    }
  }

  placed.sort((one, other) => one.atNanos - other.atNanos);
  return { placed, trimmedAway };
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
function describeLevel(track: AudioTrack, index: number, solo: Solo): string {
  const output = monitor(track, index, solo);
  if (!output.audible) {
    return track.muted ? 'Muted' : 'Silent while another track is soloed';
  }
  if (output.gainDb === 0) {
    return 'As recorded';
  }
  return `${output.gainDb > 0 ? '+' : ''}${output.gainDb.toFixed(1)} dB`;
}

/** The editor. */
export function ClipEditor({
  clip,
  durationNanos,
  events = null,
  waveforms = null,
}: ClipEditorProps): ReactNode {
  const [playheadNanos, setPlayheadNanos] = useState(0);
  const [zoom, setZoom] = useState(ZOOM_STEPS[0] ?? 1);
  const [hiddenKinds, setHiddenKinds] = useState<ReadonlySet<string>>(new Set());
  const [showExport, setShowExport] = useState(false);
  /*
   * Which track, if any, the editor is listening to alone. `Solo` in
   * `crates/edit`, held here rather than on the document for the same reason
   * the playhead and the zoom are this component's own state (issue #85): it
   * describes this window's listening, not the clip.
   */
  const [solo, setSolo] = useState<Solo>(SOLO_NONE);
  const scroller = useRef<HTMLDivElement>(null);

  /*
   * The boundaries a cut can be at, which Page Up and Page Down step between.
   * Held rather than recomputed so that the key handler below is not rebuilt on
   * every render — and `?? [0]` because a document whose segments cannot be
   * read has no boundaries; the screen refuses one before it reaches here, and
   * this is what keeps that a screen decision rather than a crash.
   */
  const cuts = useMemo(() => boundaries(clip) ?? [0], [clip]);

  /*
   * The marks this clip can draw, and the kinds among them. Both are held
   * because both are walks over every event, and the timeline re-renders on
   * every arrow key.
   */
  const eventMarks = useMemo(
    () => (events === null ? null : placeMarks(clip, events)),
    [clip, events],
  );
  const onClip = useMemo(() => (eventMarks?.placed ?? []).map((one) => one.mark), [eventMarks]);
  const kinds = useMemo(() => kindsPresent(onClip), [onClip]);
  const counts = useMemo(() => countByKind(onClip), [onClip]);
  const shown = useMemo(
    () =>
      eventMarks === null
        ? null
        : eventMarks.placed.filter((one) => !hiddenKinds.has(one.mark.kind)),
    [eventMarks, hiddenKinds],
  );
  /*
   * What each audio lane draws, and where. Held because it is a walk over every
   * segment of every track and the timeline re-renders on every arrow key —
   * and `null` when nobody has asked, which is a different lane entirely.
   */
  const lanes = useMemo(
    () =>
      waveforms === null
        ? null
        : clip.audio_tracks.map((_, index) => lanePieces(clip, index, durationNanos, waveforms)),
    [clip, durationNanos, waveforms],
  );
  const placement = locate(clip, playheadNanos);
  const interval = tickIntervalNanos(durationNanos, zoom);
  const marks = ticks(durationNanos, zoom);
  const overlays = overlaysAt(clip, playheadNanos);

  const seek = useCallback(
    (to: number) => {
      setPlayheadNanos(Math.min(Math.max(Math.round(to), 0), durationNanos));
    },
    [durationNanos],
  );

  /*
   * Pressing a track's Solo button moves the solo there, or clears it if that
   * track was already the one soloed — `toggleSolo`, which is `Solo::toggled`
   * in `crates/edit`. There is deliberately no way to reach two tracks soloed
   * at once.
   */
  const onToggleSolo = useCallback((track: number) => {
    setSolo((current) => toggleSolo(current, track));
  }, []);

  /* Hiding a kind is the one thing that keeps a mark off the timeline, and it
     is the user's own decision every time. */
  const toggleKind = useCallback((kind: string) => {
    setHiddenKinds((current) => {
      const next = new Set(current);
      if (!next.delete(kind)) {
        next.add(kind);
      }
      return next;
    });
  }, []);

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
      <div className="clipped-editor__header">
        <h2 className="clipped-screen__heading">{clip.title}</h2>
        {/*
         * Opening the dialog is this component's own state, which is why it is
         * a control this screen may draw at all — the same reason the zoom
         * controls are here and a Split button is not. What the dialog then
         * says about exporting, including that no export can be started from
         * this window, is `ExportDialog`'s (issue #90).
         *
         * Export is the **first** tab stop of the editor, ahead of the zoom
         * controls, the kind filters, each track's Solo button, the event
         * marks and the playhead. That is not an accident of where two
         * branches happened to meet: it is where the button is drawn — in
         * the header, beside the clip's title — and
         * focus order follows visual order. Putting the whole-document action
         * last, after the timeline "content", would read well in the abstract
         * and would mean a keyboard user's focus jumping from the bottom of the
         * screen back to the top, which is the mismatch WCAG 2.4.3 is about and
         * which only a positive `tabIndex` could produce. `docs/desktop-ui.md`
         * carries the reasoning and the whole order; `EditorEvents.test.tsx`
         * asserts every stop of it, so a control added here fails a test rather
         * than silently reordering the editor.
         */}
        <button
          type="button"
          className="clipped-btn clipped-btn--secondary"
          onClick={() => {
            setShowExport(true);
          }}
        >
          Export&hellip;
        </button>
      </div>

      <div className="clipped-editor__top">
        <div className="clipped-editor__frame">
          <p className="clipped-editor__frame-note">
            No picture. A frame is in the recording, and this window cannot open one: it has no
            file-system permission and no command that serves frames. Issue #658.
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

      {eventMarks !== null && kinds.length > 0 && (
        <div className="clipped-editor__event-filter" role="group" aria-label="Show events">
          <span className="clipped-muted">Events</span>
          {kinds.map((kind) => {
            const description = describeKind(kind);
            const isShown = !hiddenKinds.has(kind);
            return (
              <button
                key={kind}
                type="button"
                className={`clipped-btn ${isShown ? 'clipped-btn--secondary' : 'clipped-btn--ghost'}`}
                aria-pressed={isShown}
                onClick={() => {
                  toggleKind(kind);
                }}
              >
                {description.label} ({counts.get(kind) ?? 0})
                {/* Said in words as well as drawn, because a pressed state told
                    only by a fill is a state told only by colour (AGENTS.md
                    section 46). */}
                {isShown ? '' : <span className="clipped-muted"> hidden</span>}
              </button>
            );
          })}
        </div>
      )}

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
          {eventMarks !== null && (
            <li className="clipped-timeline__label">
              <span className="clipped-timeline__track-name">Events</span>
              <span className="clipped-muted">
                {shown === null || shown.length === 0
                  ? 'None on this clip'
                  : `${shown.length} shown`}
              </span>
            </li>
          )}
          <li className="clipped-timeline__label">
            <span className="clipped-timeline__track-name">Video</span>
            <span className="clipped-muted">
              {clip.segments.length} {clip.segments.length === 1 ? 'segment' : 'segments'}
            </span>
          </li>
          {clip.audio_tracks.map((track, index) => (
            <li className="clipped-timeline__label" key={track.name}>
              <span className="clipped-timeline__track-name">{track.name}</span>
              {/*
               * Soloing is this component's own state (issue #85): it changes
               * what the preview plays, here and nowhere the document could
               * carry it, so it needs no path to `crates/edit` the way a
               * volume slider or a mute button would. `aria-pressed` says the
               * state in words as well as by style, matching every other
               * toggle on this screen (AGENTS.md section 46).
               */}
              <button
                type="button"
                className={`clipped-btn ${solo === index ? 'clipped-btn--secondary' : 'clipped-btn--ghost'}`}
                aria-pressed={solo === index}
                onClick={() => {
                  onToggleSolo(index);
                }}
              >
                Solo
              </button>
              <span className="clipped-muted">{describeLevel(track, index, solo)}</span>
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
             * The events lane. Outside the slider below, because each mark is a
             * button and a button inside a `role="slider"` is not something a
             * keyboard or a screen reader can make sense of; a list, because
             * that is what it is, and it is what gives the marks a name a
             * screen reader announces before reading them.
             */}
            {shown !== null && (
              <ul className="clipped-timeline__events" aria-label="Events on this clip">
                {shown.map((one, index) => {
                  const description = describeKind(one.mark.kind);
                  return (
                    <li
                      className="clipped-timeline__event"
                      key={`${one.mark.recording}-${String(one.mark.at)}-${one.mark.kind}-${String(index)}`}
                      style={{ left: percent(fractionOf(one.atNanos, durationNanos)) }}
                    >
                      <button
                        type="button"
                        className="clipped-timeline__event-mark"
                        aria-label={describeMark(description.label, one.atNanos, one.mark.source)}
                        onClick={() => {
                          seek(one.atNanos);
                        }}
                      />
                    </li>
                  );
                })}
              </ul>
            )}

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
              <div className="clipped-timeline__lane">
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

              {clip.audio_tracks.map((track, index) => (
                <div className="clipped-timeline__lane" key={track.name}>
                  {lanes === null ? (
                    // Nobody has asked. One statement for the lane, because
                    // "nothing was asked" is a fact about the whole of it and
                    // not about any one segment.
                    <span className="clipped-timeline__absent">No waveform</span>
                  ) : (
                    (lanes[index] ?? []).map((piece) => (
                      <LanePeaks key={piece.segment} piece={piece} lane={track.name} />
                    ))
                  )}
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

      <EventNote events={events} marks={eventMarks} />

      <WaveformNote clip={clip} waveforms={waveforms} />

      {showExport && (
        <ExportDialog
          clip={clip}
          durationNanos={durationNanos}
          onClose={() => {
            setShowExport(false);
          }}
        />
      )}
    </section>
  );
}

/**
 * What a mark is called to somebody who cannot see it.
 *
 * The kind, where it is, and who said so. The source is in the name because a
 * mark this build has no vocabulary for — a plugin's own word, or a kind added
 * after this build shipped — is otherwise a label nobody can trace to anything.
 */
function describeMark(label: string, atNanos: number, source: string): string {
  return `${label} at ${formatTimecode(atNanos)}, reported by ${source}`;
}

/**
 * What the timeline says about events beyond the marks themselves.
 *
 * Three different statements, and the difference between the first two is the
 * whole of AGENTS.md section 27 here: "nobody asked" is not "there were none".
 */
function EventNote({
  events,
  marks,
}: {
  readonly events: readonly EventMark[] | null;
  readonly marks: ClipMarks | null;
}): ReactNode {
  if (events === null || marks === null) {
    return (
      <p className="clipped-editor__note clipped-muted">
        The timeline has no events, and that is not the same as this recording having none: nothing
        in this window has asked. A session’s game events are rows of the library’s database, which
        this window can neither read nor ask the recorder for. Issues #329 and #301.
      </p>
    );
  }

  if (events.length === 0) {
    return (
      <p className="clipped-editor__note clipped-muted">
        No events were reported during these recordings.
      </p>
    );
  }

  if (marks.trimmedAway === 0) {
    return null;
  }

  return (
    <p className="clipped-editor__note clipped-muted">
      {marks.trimmedAway === 1
        ? 'One event of these recordings is not on this clip'
        : `${String(marks.trimmedAway)} events of these recordings are not on this clip`}
      , because the seconds they happened in were trimmed out. They are not drawn at the nearest
      cut, which would be a mark for footage this clip does not contain.
    </p>
  );
}

/**
 * What a lane says when it has no peaks for one segment, in the lane itself.
 *
 * Four states and three sentences, because one of them is not a sentence: a
 * round trip in flight draws nothing at all rather than a label that appears
 * for an instant and is replaced by a picture.
 *
 * The other three are deliberately different words. "Not yet" is the ordinary
 * state of a recording written a minute ago and it resolves itself; "No
 * waveform" will not; and a track that takes nothing from the recording under
 * this part of the clip is not missing anything at all — the exported track is
 * silent there. Collapsing them into one string is the fabricated state
 * AGENTS.md section 27 is about, and `docs/waveforms.md` names the first two as
 * the pair that must never read alike.
 */
const ABSENT: Record<LaneAbsence, string | null> = {
  asking: null,
  pending: 'No waveform yet',
  none: 'No waveform',
  silent: 'Not in this recording',
};

/**
 * One segment's worth of one audio lane.
 *
 * The picture is an inline `<svg>` for the reasons `Waveform.tsx` gives — it
 * scales without being told, at the 200% this window runs at, and it can carry
 * a name, which is the whole of what a screen reader gets from a waveform. Its
 * `viewBox` is one unit per bucket and `preserveAspectRatio="none"` stretches
 * that to whatever width the segment has at this zoom, so zooming in draws the
 * same peaks wider rather than asking for more of them.
 *
 * **The label names the range**, and that is not decoration. "A waveform is
 * drawn" is true of a build that put the whole recording's peaks under a clip
 * trimmed from its middle; "Game from 01:32.000 to 01:44.000 of rec-…" is not,
 * so the failure that catches it names what went wrong instead of saying an
 * image is missing.
 */
function LanePeaks({
  piece,
  lane,
}: {
  readonly piece: LanePiece;
  readonly lane: string;
}): ReactNode {
  const style = {
    left: percent(piece.startFraction),
    width: percent(piece.widthFraction),
  };

  if (!piece.content.drawn) {
    const said = ABSENT[piece.content.why];
    return said === null ? null : (
      <span className="clipped-timeline__absent clipped-timeline__absent--part" style={style}>
        {said}
      </span>
    );
  }

  return (
    <svg
      className="clipped-timeline__peaks"
      style={style}
      viewBox={`0 0 ${String(piece.content.buckets)} ${String(LANE)}`}
      preserveAspectRatio="none"
      role="img"
      aria-label={describePeaks(piece, lane)}
    >
      <path d={piece.content.outline} />
    </svg>
  );
}

/**
 * What one piece of a lane is called to somebody who cannot see it.
 *
 * The lane, the part of the recording drawn, and which recording — in the
 * recording's own time, because that is the question a waveform answers and it
 * is the one the timeline's own timecodes cannot: the ruler above is counting
 * the clip.
 */
function describePeaks(piece: LanePiece, lane: string): string {
  const range = `${formatTimecode(piece.fromNanos)} to ${formatTimecode(piece.toNanos)}`;
  return `${lane} from ${range} of ${piece.recording ?? 'an undeclared source'}`;
}

/**
 * What the timeline says about the waveforms beyond the lanes themselves.
 *
 * Nothing at all in the ordinary case — every recording answered and every lane
 * drew — because a note repeating what is on the screen is a note nobody reads
 * the day it says something. What it does carry is the two things a lane has no
 * room for: which recording an absence is about, and what to do about it
 * (AGENTS.md sections 28 and 45).
 */
function WaveformNote({
  clip,
  waveforms,
}: {
  readonly clip: EditDocument;
  readonly waveforms: PeaksOf | null;
}): ReactNode {
  if (clip.audio_tracks.length === 0) {
    return null;
  }

  if (waveforms === null) {
    return (
      <p className="clipped-editor__note clipped-muted">
        No lane has a waveform: the peaks of these recordings have not been asked for. That is not
        the same as their having none.
      </p>
    );
  }

  const said = describeWaveforms(clip, waveforms);
  return said.length === 0 ? null : (
    <p className="clipped-editor__note clipped-muted">{said.join(' ')}</p>
  );
}

/**
 * A sentence for each recording whose peaks are not here, and why.
 *
 * One per *recording* rather than one per lane: the answer is about a file, so
 * a clip with four audio tracks over one recording that is still being read has
 * one thing to say and not four.
 */
function describeWaveforms(clip: EditDocument, waveforms: PeaksOf): readonly string[] {
  const said: string[] = [];
  let older = false;

  for (const recording of new Set(clip.sources.map((source) => source.recording))) {
    const view = waveforms(recording);
    if (view.state === 'unasked') {
      older = true;
    } else if (view.state === 'refused') {
      said.push(`Clipped could not ask for the sound of ${recording}. ${view.problem.message}`);
    } else if (view.state === 'answered' && view.preview.state === 'pending') {
      said.push(
        `Clipped is still reading the sound of ${recording}. Its peaks appear the next time this clip is opened.`,
      );
    } else if (view.state === 'answered' && view.preview.state === 'unavailable') {
      said.push(
        `There will be no waveform for ${recording}.${view.preview.reason === undefined ? '' : ` ${view.preview.reason}`}`,
      );
    }
  }

  if (older) {
    // Reached whenever the attached recorder does not advertise `previews`,
    // which is every recorder built before issue #448. The remedy is an
    // up-to-date Clipped rather than a different clip, so that is what it says.
    said.unshift(
      'The recorder that is running is older than this window and cannot make waveforms. Restarting Clipped starts the recorder that came with it.',
    );
  }
  return said;
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
