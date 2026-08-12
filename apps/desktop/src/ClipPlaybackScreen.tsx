import type { ReactNode } from 'react';
import { useParams } from 'react-router';

import {
  describeClip,
  formatElapsed,
  MISSING,
  PLAYBACK_BLOCKERS,
  recordingOf,
  resolveClip,
} from './clipPlayback';
import type { RecorderLinkView } from './useRecorderLink';

/**
 * The clip playback screen (issue #52).
 *
 * # Why there is no player on it
 *
 * SPEC.md section 42 asks for playback with transport controls, keyboard
 * shortcuts, frame-accurate seeking and a track selector. **None of it is
 * drawn, because this window cannot play a Clipped recording at all** — four
 * separate facts stand in the way and each is enough on its own. They are the
 * table below, with the evidence for each, and `docs/desktop-ui.md` ("Playing a
 * recording") has the design that follows from them. Issue #304 builds it.
 *
 * A transport bar over a black rectangle was the tempting alternative and is
 * the one AGENTS.md section 27 rules out twice: controls that do nothing, above
 * a picture Clipped never made. The scrubber would be the worst of them — a
 * scrubber implies a duration, and nothing in this window has measured one.
 *
 * # What it does show
 *
 * The one thing that is real: what the recorder link says about *this*
 * recording. The window follows one recorder, so it learns of exactly two
 * recordings — the one being written now, and the one a recorder died in the
 * middle of, whose file ADR 0006 says naming is the whole of recovery. Any
 * other identifier is one this window has no index to look up, which it says
 * rather than reporting the recording as missing: it has not looked, and cannot
 * (issue #305).
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
  const resolution = resolveClip(recordingId, view);
  const description = describeClip(resolution);
  const recording = recordingOf(resolution);

  return (
    <>
      <h1 className="clipped-screen__title">Playback</h1>

      <p className="clipped-screen__lead">
        Recording <code>{recordingId}</code>.
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

      {recording === null ? null : (
        <>
          <h2 className="clipped-screen__heading">What the recorder said</h2>
          <p className="clipped-screen__lead clipped-muted">
            All of it, and no more. There is no duration and no picture: nothing here has measured
            the file or looked at a frame of it.
          </p>
          <table className="clipped-table">
            <tbody>
              <tr>
                <th scope="row">File</th>
                {/*
                 * Named in full and never abbreviated. It is the only thing on
                 * this screen anybody can act on — the recording can be opened in
                 * a player that does handle Matroska (AGENTS.md sections 28 and
                 * 45).
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

      <h2 className="clipped-screen__heading">Why nothing is playing</h2>
      <p className="clipped-screen__lead clipped-muted">
        Four facts, and each one on its own is enough, so fixing any single one changes nothing.
        Issue #304 carries the design that answers all four.
      </p>

      <table className="clipped-table">
        <thead>
          <tr>
            <th scope="col">What stops it</th>
            <th scope="col">Where that can be checked</th>
          </tr>
        </thead>
        <tbody>
          {PLAYBACK_BLOCKERS.map((blocker) => (
            <tr key={blocker.fact}>
              <td>{blocker.fact}</td>
              <td className="clipped-muted">{blocker.evidence}</td>
            </tr>
          ))}
        </tbody>
      </table>

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
