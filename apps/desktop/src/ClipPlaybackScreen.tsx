import type { ReactNode } from 'react';
import { useEffect, useRef, useState } from 'react';
import { useLocation, useParams } from 'react-router';

import {
  describeClip,
  formatElapsed,
  type HandedRecording,
  MISSING,
  markedRecording,
  playbackSource,
  recordingOf,
  resolveClip,
} from './clipPlayback';
import { fileName } from './recordingActions';
import {
  describePlaybackProblem,
  headlinePlaybackProblem,
  playbackKeyAction,
  trackLabel,
  usePlayback,
} from './playback';
import { PREVIEWS, pictureUri, type ThumbnailView, useThumbnail, useWaveform } from './preview';
import { RecordingTimeline } from './RecordingTimeline';
import { useRecordingMarks } from './recordingMarks';
import { recorderCanDo, type RecorderLinkView } from './useRecorderLink';
import { Waveform } from './Waveform';

/**
 * The clip playback screen (issue #52), and the player on it (issue #304).
 *
 * # Where the picture comes from
 *
 * The recorder. This window has no file-system permission and no asset
 * protocol, so it asks the Tauri host to open a recording for playback; the
 * host asks the recorder, which answers with a file, and the host serves that
 * one file over its own `clip` scheme (`playback.ts`,
 * `src-tauri/src/playback.rs`). The element is pointed at an address, never at
 * a path.
 *
 * # Why the track selector is a set of buttons and not a control on the video
 *
 * `HTMLMediaElement.audioTracks` is not implemented in Chromium, which WebView2
 * is, so a `<video>` given a recording with four sound tracks plays the first
 * and offers no way off it. Each button asks the recorder for that track, which
 * answers with a file carrying it — and the position is carried across, because
 * changing track is not a request to start again.
 *
 * # The poster frame and the waveform
 *
 * Both come from the recorder, over `open_preview`, and both are the first time
 * anything has drawn either (issue #448). The poster is the thumbnail
 * `crates/library` already generates for the Library's tiles — the same picture,
 * the same command — handed to the element's `poster` attribute so that
 * something of the recording is on screen before a frame has been decoded. The
 * waveform is the peaks `crates/waveform` computes, drawn under the transport.
 *
 * Neither arrives as a file. The picture is base64 in the reply and is drawn
 * from a `data:` URI, which the content security policy already permits; the
 * `clip` scheme this screen plays through still serves recordings and only
 * recordings (`src-tauri/src/playback.rs`,
 * `docs/adr/0016-derived-pictures-cross-the-control-protocol.md`).
 *
 * # The timeline under it
 *
 * `RecordingTimeline` draws the marks the library holds for this recording, and
 * pressing one moves the player to it (issue #65). It is here rather than in the
 * Editor because this is the screen that has a recording open — the Editor
 * cannot open a clip yet, which is issue #306 — and because the marks are
 * already placed in *this file* by the recorder, in the file's own time.
 *
 * The length they are placed against is the element's own `duration`, taken from
 * `loadedmetadata`, for the same reason everything else here is: it is the
 * timeline the seek actually happens on.
 *
 * # What is drawn only when it is true
 *
 * A recording still being written gets no player: its container has no trailer,
 * so there is nothing whose length a transport could describe. A recording
 * whose file has gone gets the recorder's own sentence about it. Neither draws
 * a transport over nothing, which is AGENTS.md section 27.
 */

/**
 * The element's `poster`, when there is a picture to be one.
 *
 * Spread rather than passed as `poster={undefined}`, because an empty `poster`
 * attribute is not the same as none: a `<video>` given one shows nothing at all
 * where it would otherwise show its first frame.
 *
 * Every state but a ready thumbnail is no poster. A recording whose picture has
 * not been made yet, or will never be made, still plays — and the element's own
 * first frame is a better stand-in than a tile saying why there is no tile,
 * which is a sentence a player has no room for and no need of.
 */
function posterOf(view: ThumbnailView): { poster?: string } {
  if (view.state !== 'answered') {
    return {};
  }
  const picture = view.preview.picture;
  return view.preview.state === 'ready' && picture !== undefined
    ? { poster: pictureUri(picture) }
    : {};
}

/** What the playback screen is given. */
export interface ClipPlaybackScreenProps {
  /**
   * Everything the window knows about the recorder.
   *
   * Passed in rather than taken from `useRecorderLink` here, so that the shell
   * holds one subscription rather than two and this screen is a pure function
   * of what it is told — the same arrangement the Games screen uses.
   */
  readonly view: RecorderLinkView;
}

/** The clip playback screen. */
export function ClipPlaybackScreen({ view }: ClipPlaybackScreenProps): ReactNode {
  // The route is `/clip/:recordingId`, so the parameter is always present; an
  // empty string is what a route without one would give, and it resolves to
  // "not known to this window" like any other identifier the link never named.
  const { recordingId = '' } = useParams();
  // What the screen somebody came from handed over. The Library has the row in
  // its hand when the Play button is pressed, and passing it is what saves a
  // second read of the same thing; a reload has none, and the screen says so
  // rather than inventing one (issue #52).
  const handed = (useLocation().state as { recording?: HandedRecording } | null)?.recording ?? null;

  const resolution = resolveClip(recordingId, view);
  const description = describeClip(resolution);
  const recording = recordingOf(resolution);
  const source = playbackSource(resolution, handed);
  const playback = usePlayback(source.file);

  /*
   * Asked of the recorder that is attached rather than assumed of the one this
   * window shipped with: a recorder from before issue #448 has no
   * `open_preview` and would refuse both of these by name, and a poster that
   * never arrives is better drawn as no poster than as a failure.
   */
  const previews = recorderCanDo(view.link, PREVIEWS) ? source.file : null;
  const poster = useThumbnail(previews);
  /*
   * A round number rather than a measured width, because nothing in jsdom or in
   * a window that has not laid out yet can measure one, and because merging
   * buckets is exact: peaks answered at 1,200 and drawn at 1,340 are the same
   * answer stretched, not a wrong one. 1,200 is a little under the widest this
   * panel is at the window's minimum size on a 200%-scaled display, which is
   * the machine this runs on (`docs/waveforms.md`).
   */
  const peaks = useWaveform(previews, 1_200);

  const video = useRef<HTMLVideoElement>(null);
  /** Where the recording was when a different track was asked for. */
  const resumeAt = useRef<number | null>(null);
  /*
   * The recording's length, as the element measured it. Held in state rather
   * than read off the ref, because a ref changing is not a render and the
   * timeline below has to be drawn again once there is something to place marks
   * against. `NaN` until a container has been read, and a track change starts
   * that again from a fresh element.
   */
  const [durationSeconds, setDurationSeconds] = useState<number | null>(null);
  /*
   * Where the element is, for the playhead over the waveform (issue #694). In
   * state rather than read off the ref for the reason the length is: a ref
   * changing is not a render, and a playhead that only moved when something
   * else happened to redraw would be worse than none.
   *
   * `timeupdate` fires a few times a second rather than per frame, which is the
   * rate this wants: a playhead over a whole recording moves less than a pixel
   * between frames, and a `requestAnimationFrame` loop would be a render loop
   * for the life of the screen.
   */
  const [positionSeconds, setPositionSeconds] = useState<number | null>(null);

  /*
   * The marks the library holds for this recording (issue #65). Asked for by the
   * index's own key, which is only in the address when the Library put it there
   * — `markedRecording` is where that is argued.
   */
  const marked = markedRecording(recordingId, handed, resolution);
  const marks = useRecordingMarks(marked.recording);

  /*
   * The screen's keyboard shortcuts (SPEC.md section 42, issue #52).
   *
   * On the document rather than on a container, because the point is the keys
   * working when focus is *anywhere on this screen* — after pressing a track
   * button, a mark, or nothing at all. A handler on a wrapper answers only
   * while focus is inside it, which is the case that already worked.
   *
   * `playbackKeyAction` is given what has focus and declines every key that
   * control is going to use, so this adds shortcuts without taking the track
   * buttons away from anybody navigating by keyboard.
   */
  useEffect(() => {
    function onKeyDown(event: KeyboardEvent): void {
      const element = video.current;
      if (element === null) {
        return;
      }
      const focused = document.activeElement?.tagName ?? null;
      const action = playbackKeyAction(event, focused);
      if (action === null) {
        return;
      }
      // Only once it is certain this screen is answering: a `preventDefault`
      // before that would stop the page scrolling on a space this screen was
      // about to decline.
      event.preventDefault();

      switch (action.kind) {
        case 'toggle':
          if (element.paused) {
            void element.play();
          } else {
            element.pause();
          }
          break;
        case 'seek': {
          // Clamped, because assigning past the end is a seek to the end
          // followed by an `ended` event, and assigning below zero throws in
          // some engines. The length is the element's own, which is `NaN`
          // until it has read a container.
          const length = element.duration;
          const wanted = element.currentTime + action.seconds;
          element.currentTime = Number.isFinite(length)
            ? Math.min(Math.max(wanted, 0), length)
            : Math.max(wanted, 0);
          break;
        }
        case 'start':
          element.currentTime = 0;
          break;
        case 'end':
          if (Number.isFinite(element.duration)) {
            element.currentTime = element.duration;
          }
          break;
      }
    }

    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('keydown', onKeyDown);
    };
  }, []);

  return (
    <>
      <h1 className="clipped-screen__title">Playback</h1>

      {/*
       * The file in full, and exactly once. A recording the link knows about has
       * it in the table below — that table is what the *recorder* said — so this
       * names it only for a recording handed over by another screen, which has
       * no table (AGENTS.md section 28: a path with an ellipsis in it cannot be
       * typed into Explorer).
       */}
      <p className="clipped-screen__lead">
        {source.file !== null && recording === null ? (
          <code className="clipped-path">{source.file}</code>
        ) : (
          <>
            Recording <code>{recordingId}</code>.
          </>
        )}
      </p>

      {/*
       * A live region, like the recorder status in the sidebar and the Games
       * screen's detection block: a recording that ends, or a recorder that
       * dies while this screen is open, changes what it says without anybody
       * touching the window.
       */}
      <section className="clipped-panel" aria-label="Recording" aria-live="polite">
        <h2 className="clipped-panel__heading">{description.state}</h2>
        <p className="clipped-panel__body">{description.detail}</p>
      </section>

      <section className="clipped-panel" aria-label="Player">
        {source.file === null ? (
          <p className="clipped-panel__body">{source.why}</p>
        ) : playback.problem !== null ? (
          <>
            {/*
             * The recorder's own sentence, which for the commonest failure
             * names the file and says it has gone. A player drawn over it would
             * be a control that does nothing (AGENTS.md sections 27 and 45).
             */}
            <h2 className="clipped-panel__heading">{headlinePlaybackProblem(playback.problem)}</h2>
            <p className="clipped-panel__body">{describePlaybackProblem(playback.problem)}</p>
          </>
        ) : playback.stream === null ? (
          <p className="clipped-panel__body">Opening {fileName(source.file)}…</p>
        ) : (
          <>
            {/*
             * `controls` is the element's own transport: play, pause, a
             * scrubber over a duration it measured, and the volume. Clipped
             * draws none of its own, because every one of them would be a
             * second answer to something the element already knows (a transport
             * of Clipped's own is SPEC.md section 42 and issue #52; its
             * frame-accurate seeking is already here, which is the correction
             * in ADR 0011).
             */}
            {/* eslint-disable-next-line jsx-a11y/media-has-caption -- a recording of somebody's game has no caption track, and there is nothing to write one from. */}
            <video
              ref={video}
              className="clipped-player"
              src={playback.stream.url}
              controls
              aria-label={`Playing ${fileName(source.file)}`}
              {...posterOf(poster)}
              onTimeUpdate={() => {
                setPositionSeconds(video.current?.currentTime ?? null);
              }}
              onLoadedMetadata={() => {
                // Choosing a track is a different file, so the element starts
                // at zero; putting it back where it was is what makes the
                // choice a choice about sound rather than a restart.
                const resume = resumeAt.current;
                resumeAt.current = null;
                if (resume !== null && video.current !== null) {
                  video.current.currentTime = resume;
                }
                // The one measurement of this recording's length anything in
                // this window has. A container still being written, or one the
                // element could not read, reports `NaN` or `Infinity`, and the
                // timeline below says it has no length rather than drawing
                // marks against one.
                const length = video.current?.duration ?? Number.NaN;
                setDurationSeconds(Number.isFinite(length) && length > 0 ? length : null);
              }}
            />

            {/*
             * Under the transport, because that is where a waveform belongs and
             * because it describes the whole recording: `tracks` is every sound
             * track of the file, one lane each, which is what the selector
             * below chooses between.
             *
             * The lane for the track being played is marked and the playhead
             * runs across all of them, so the picture says both what is in the
             * file and which part of it you are hearing (issue #694).
             */}
            <Waveform
              preview={peaks.state === 'answered' ? peaks.preview : null}
              of={fileName(source.file)}
              durationSeconds={durationSeconds}
              positionSeconds={positionSeconds}
              playingTrack={playback.stream.audio_track ?? null}
              onSeek={(seconds) => {
                if (video.current !== null) {
                  video.current.currentTime = seconds;
                  // So the playhead lands where it was pointed rather than
                  // waiting for the element's next `timeupdate`.
                  setPositionSeconds(seconds);
                }
              }}
            />

            {/*
             * The recording's own timeline (issue #65). Under the waveform,
             * because both describe the whole file and this one is the thing
             * that moves the player: pressing a marker seeks the element to the
             * position the recorder placed that event at.
             */}
            <RecordingTimeline
              read={marks}
              durationSeconds={durationSeconds}
              of={fileName(source.file)}
              unasked={marked.recording === null ? marked.why : ''}
              onSeek={(seconds: number) => {
                if (video.current !== null) {
                  video.current.currentTime = seconds;
                }
              }}
            />

            {playback.stream.audio_tracks.length > 1 && (
              <fieldset className="clipped-tracks">
                <legend>Sound</legend>
                <p className="clipped-muted">
                  One track at a time: a media element cannot switch between them, so choosing one
                  asks the recorder for it.
                </p>
                {playback.stream.audio_tracks.map((track, position) => {
                  const chosen = track.index === playback.stream?.audio_track;
                  return (
                    <button
                      key={track.index}
                      type="button"
                      className="clipped-btn clipped-btn--secondary"
                      aria-pressed={chosen}
                      disabled={playback.busy}
                      onClick={() => {
                        resumeAt.current = video.current?.currentTime ?? null;
                        playback.choose(track.index);
                      }}
                    >
                      {trackLabel(track, position)}
                    </button>
                  );
                })}
              </fieldset>
            )}
          </>
        )}
      </section>

      {recording === null ? null : (
        <>
          <h2 className="clipped-screen__heading">What the recorder said</h2>
          <p className="clipped-screen__lead clipped-muted">
            All of it, and no more. The length and the position under the picture are the player’s
            own measurements of the file; nothing here has counted a clock.
          </p>
          <table className="clipped-table">
            <tbody>
              <tr>
                <th scope="row">File</th>
                {/*
                 * Named in full and never abbreviated. It is the thing anybody
                 * can act on — the recording can be opened in another player,
                 * or found in Explorer (AGENTS.md sections 28 and 45).
                 */}
                <td>
                  <code>{recording.output}</code>
                </td>
              </tr>
              <tr>
                <th scope="row">Capture target</th>
                <td>{recording.target}</td>
              </tr>
              <tr>
                <th scope="row">Recorded for</th>
                <td>
                  {formatElapsed(recording.elapsedMs)}{' '}
                  <span className="clipped-muted">
                    when the recorder last said so, which is a lower bound rather than the length of
                    the file
                  </span>
                </td>
              </tr>
            </tbody>
          </table>
        </>
      )}

      <h2 className="clipped-screen__heading">What this screen will do</h2>
      <p className="clipped-screen__lead clipped-muted">
        None of it is drawn yet, and none of it is invented in the meantime. Each row names the work
        that supplies it.
      </p>

      <table className="clipped-table">
        <thead>
          <tr>
            <th scope="col">What the playback screen will do</th>
            <th scope="col">What has to exist first</th>
          </tr>
        </thead>
        <tbody>
          {MISSING.map((entry) => (
            <tr key={entry.shows}>
              <td>{entry.shows}</td>
              <td className="clipped-muted">{entry.needs}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </>
  );
}
