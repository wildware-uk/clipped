import type { ReactNode } from 'react';

import { describeProblem, formatBytes, useGames, useSessions } from './library';
import { describeRecordingNow, WHERE_RECORDINGS_GO } from './recordingNow';
import { SessionList } from './SessionList';
import type { RecorderLinkState } from './useRecorderLink';
import { WaitingOn, type Waiting } from './WaitingOn';

/** How many sittings Home lists. It is a recent-activity list, not the library. */
const RECENT = 5;

/**
 * The Home screen (issues #60 and #301).
 *
 * # What it draws
 *
 * SPEC.md section 17 draws Home as four lists — recent sessions, recently
 * clipped, favourites, games. Two of them are on the screen: the most recent
 * sittings, and what the library holds per game. Both are read from the index
 * through the recorder, because this window can neither open `library.db` nor
 * link `clipped-library` (`docs/library.md`, ADR 0002, issue #301).
 *
 * The other two are still waiting on the things that produce them rather than on
 * a way to read them: nothing creates a clip yet (#91), and nothing can
 * favourite anything (#58).
 *
 * A library that could not be read says so and says why. It is never drawn as an
 * empty one: four tiles reading "0" over a database that could not be opened
 * would be indistinguishable from a machine that has recorded nothing
 * (AGENTS.md section 27).
 */

/**
 * What SPEC.md section 17 still asks of the Home screen, against what each one
 * is waiting for.
 *
 * Written out here rather than derived from anything, because it is a promise to
 * the reader. The rows for recent sessions and for the per-game figures are
 * gone: both are on the screen now.
 */
const WAITING: readonly Waiting[] = [
  {
    shows: 'Recently clipped, and the clips a session produced',
    needs: 'The virtual clip model (#74) and clip creation (#91). The read carries clips already',
  },
  {
    shows: 'Favourites, which are also what storage cleanup protects',
    needs:
      'Favouriting anything at all. Issue #58. The read already carries which things are favourited',
  },
];

/** What the Home screen is given. */
export interface HomeScreenProps {
  /**
   * Where the recorder link stands, or `null` outside the Clipped window.
   *
   * Passed in rather than taken from `useRecorderLink` here, so that the shell
   * holds one subscription rather than two and this screen is a pure function of
   * what it is told.
   */
  readonly link: RecorderLinkState | null;
}

/** The Home screen. */
export function HomeScreen({ link }: HomeScreenProps): ReactNode {
  const now = describeRecordingNow(link);
  const { read: sessions } = useSessions('', RECENT);
  const games = useGames();

  return (
    <>
      <h1 className="clipped-screen__title">Home</h1>

      <p className="clipped-screen__lead">
        Clipped records a game because it launched, and writes each sitting to ordinary files you
        keep whether or not you keep Clipped.
      </p>

      {/*
       * A live region, like the recorder status in the sidebar and the Games
       * screen's detection block, because a recording starting or ending changes
       * what this says while nobody is looking at it. It carries the state, the
       * reason for it and the file, and nothing else.
       */}
      <section className="clipped-panel" aria-label="Recording now" aria-live="polite">
        <h2 className="clipped-panel__heading">{now.state}</h2>
        <p className="clipped-panel__body">{now.detail}</p>
        {now.output !== undefined && (
          <p className="clipped-panel__body clipped-path">{now.output}</p>
        )}
        <p className="clipped-panel__body clipped-muted">{WHERE_RECORDINGS_GO}</p>
      </section>

      <section aria-label="Recent sessions">
        <h2 className="clipped-screen__heading">Recent sessions</h2>
        {sessions.state === 'reading' && (
          <p className="clipped-screen__lead clipped-muted" aria-busy="true">
            Reading your library…
          </p>
        )}
        {sessions.state === 'unread' && (
          <p className="clipped-screen__lead">{describeProblem(sessions.problem)}</p>
        )}
        {sessions.state === 'read' && sessions.value.length === 0 && (
          <p className="clipped-screen__lead">
            Your library was read and holds no sittings yet. One appears here after Clipped has
            recorded a game.
          </p>
        )}
        {sessions.state === 'read' && sessions.value.length > 0 && (
          <SessionList sessions={sessions.value} label="Recent sessions" />
        )}
      </section>

      <section aria-label="Games">
        <h2 className="clipped-screen__heading">Games</h2>
        {games.state === 'reading' && (
          <p className="clipped-screen__lead clipped-muted" aria-busy="true">
            Reading your library…
          </p>
        )}
        {games.state === 'unread' && (
          <p className="clipped-screen__lead">{describeProblem(games.problem)}</p>
        )}
        {games.state === 'read' && games.value.length === 0 && (
          <p className="clipped-screen__lead">Your library was read and holds no games yet.</p>
        )}
        {games.state === 'read' && games.value.length > 0 && (
          <table className="clipped-table" aria-label="Games">
            <thead>
              <tr>
                <th scope="col">Game</th>
                <th scope="col">Sessions</th>
                <th scope="col">Recordings</th>
                <th scope="col">Size</th>
                <th scope="col">Files</th>
              </tr>
            </thead>
            <tbody>
              {games.value.map((game) => (
                <tr key={game.game_id ?? 'unattributed'}>
                  <td>{game.name ?? 'Not recognised'}</td>
                  <td>{game.sessions}</td>
                  <td>{game.recordings}</td>
                  {/*
                   * A missing file contributes nothing to the size — the space
                   * it is not occupying is not being used — and is counted
                   * beside it instead, in words (docs/library.md).
                   */}
                  <td>{formatBytes(game.bytes)}</td>
                  <td>{game.missing === 0 ? 'All present' : `${String(game.missing)} missing`}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </section>

      <h2 className="clipped-screen__heading">What this screen will show</h2>
      <p className="clipped-screen__lead clipped-muted">
        None of it is drawn yet, and none of it is invented in the meantime. Each row names the work
        that supplies it.
      </p>

      <WaitingOn heading="What the Home screen will show" rows={WAITING} />
    </>
  );
}
