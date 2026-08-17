import type { ReactNode } from 'react';
import { useRef } from 'react';
import { useLocation, useParams } from 'react-router';

import {
  describeClip,
  formatElapsed,
  type HandedRecording,
  MISSING,
  playbackSource,
  recordingOf,
  resolveClip,
} from './clipPlayback';
import { fileName } from './recordingActions';
import {
  describePlaybackProblem,
  headlinePlaybackProblem,
  trackLabel,
  usePlayback,
} from './playback';
import type { RecorderLinkView } from './useRecorderLink';

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
 * # What is drawn only when it is true
 *
 * A recording still being written gets no player: its container has no trailer,
 * so there is nothing whose length a transport could describe. A recording
 * whose file has gone gets the recorder's own sentence about it. Neither draws
 * a transport over nothing, which is AGENTS.md section 27.
 */

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

  const video = useRef<HTMLVideoElement>(null);
  /** Where the recording was when a different track was asked for. */
  const resumeAt = useRef<number | null>(null);

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
             * second answer to something the element already knows (SPEC.md
             * section 42's frame-accurate seeking is issue #52).
             */}
            {/* eslint-disable-next-line jsx-a11y/media-has-caption -- a recording of somebody's game has no caption track, and there is nothing to write one from. */}
            <video
              ref={video}
              className="clipped-player"
              src={playback.stream.url}
              controls
              aria-label={`Playing ${fileName(source.file)}`}
              onLoadedMetadata={() => {
                // Choosing a track is a different file, so the element starts
                // at zero; putting it back where it was is what makes the
                // choice a choice about sound rather than a restart.
                const resume = resumeAt.current;
                resumeAt.current = null;
                if (resume !== null && video.current !== null) {
                  video.current.currentTime = resume;
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
