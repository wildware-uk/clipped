import type { ReactNode } from 'react';

import { describeRecordingNow, WHERE_RECORDINGS_GO } from './recordingNow';
import type { RecorderLinkState } from './useRecorderLink';
import { WaitingOn, type Waiting } from './WaitingOn';

/**
 * The Home screen (issue #60).
 *
 * # Why there are no recent sessions on it
 *
 * SPEC.md section 17 draws Home as four lists — recent sessions, recently
 * clipped, favourites, games — and the application deck draws them as tiles.
 * **Every one of them is a read of the library index, and this window cannot
 * perform one.** `clipped-library` reconciles the session sidecars into
 * `library.db` inside the recorder's process (#56, `docs/library.md`); the
 * control protocol has no command that reads a row of it; and this window has no
 * file-system permission to open the database itself, because
 * `src-tauri/capabilities/default.json` grants three `core:` permissions and
 * nothing else. Linking the crate is not an option either — the workspace
 * layering test permits `apps/desktop/src-tauri` exactly one member of the
 * workspace, `clipped-ipc`. Issue #301 is the gap.
 *
 * Four tiles reading "0" would be indistinguishable from a machine that has
 * recorded nothing, and this build has not looked (AGENTS.md section 27). So the
 * screen shows the one thing it can establish — what is being recorded right
 * now, and into which file — and then names each list it owes against the work
 * that lands it.
 */

/**
 * Everything SPEC.md section 17 asks of the Home screen, against what each one
 * is waiting for.
 *
 * Written out here rather than derived from anything, because it is a promise to
 * the reader: these four are what this screen will become, and each is pinned to
 * the work that lands it.
 */
const WAITING: readonly Waiting[] = [
  {
    shows: 'Recent sessions: each sitting, the game it was, when it ran and what it produced',
    needs:
      'The library index that records them (#56, built and uncalled), and a way for this window to read it. Issue #301',
  },
  {
    shows: 'Recently clipped, and the clips a session produced',
    needs:
      'The virtual clip model (#74) and clip creation (#91), then the same read. Issues #74 and #301',
  },
  {
    shows: 'Favourites, which are also what storage cleanup protects',
    needs: 'Favouriting anything at all. Issue #58, then issue #301',
  },
  {
    shows:
      'Games, with the sessions, clips, favourites and bytes against each (SPEC.md section 17)',
    needs:
      'The index already computes these as game_summaries; nothing carries them to this window. Issue #301. The Games screen itself is #107',
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

      <h2 className="clipped-screen__heading">What this screen will show</h2>
      <p className="clipped-screen__lead clipped-muted">
        None of it is drawn yet, and none of it is invented in the meantime. Each row names the work
        that supplies it.
      </p>

      <WaitingOn heading="What the Home screen will show" rows={WAITING} />
    </>
  );
}
