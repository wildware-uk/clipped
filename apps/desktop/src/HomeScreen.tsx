import type { LibraryClip, SessionSummary } from '@clipped/shared';
import type { ReactNode } from 'react';
import { useNavigate } from 'react-router';

import { clipPath } from './clipPlayback';
import { GamesTable } from './GamesTable';
import {
  describeProblem,
  formatDuration,
  formatMoment,
  recentClips,
  useGames,
  useSessions,
} from './library';
import { fileName } from './recordingActions';
import { describeRecordControl } from './recording';
import { describeRecordingNow, describeResizeEnding, WHERE_RECORDINGS_GO } from './recordingNow';
import { SessionList } from './SessionList';
import type { RecorderLinkState } from './useRecorderLink';
import { useRecording } from './useRecording';

/** How many sittings Home lists. It is a recent-activity list, not the library. */
const RECENT = 5;

/**
 * How many clips Home lists.
 *
 * More than the sittings, because one sitting can produce several clips and a
 * list of five would show one evening's worth. It is still a recent-activity
 * list rather than the library.
 */
const RECENT_CLIPS = 8;

/**
 * The Home screen (issues #60, #301 and #389).
 *
 * # The record control
 *
 * Pressing it starts a recording of the application the user was last in;
 * pressing it again stops that recording and finishes its file. That is the
 * whole of [issue #389](https://github.com/wildware-uk/clipped/issues/389), and
 * it is the smallest thing that makes Clipped an application rather than a set
 * of parts.
 *
 * Everything the control says about the recording comes from the recorder, asked
 * once a second (`useRecording`). Nothing here is set because a button was
 * pressed: a window that assumed its own command had worked would say
 * "recording" over a recorder that had died, and that state — confidently wrong
 * about somebody's footage — is worse than no state at all.
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
 * a way to read them: a saved replay is the only thing that makes a clip (#38),
 * and the timeline that would make the rest is #91. Favourites can now be set —
 * the Library screen's Keep control does it (#58) — so what is missing here is
 * the list of them rather than any way to make one.
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
/** What the Home screen is given. */
export interface HomeScreenProps {
  /**
   * Where the recorder link stands, or `null` outside the Clipped window.
   *
   * Passed in rather than taken from `useRecorderLink` here, so that the shell
   * holds one subscription rather than two. What the recorder is *doing* is not
   * passed in: it is asked for by this screen, and only while this screen is
   * open, so that leaving it stops the asking.
   */
  readonly link: RecorderLinkState | null;
  /**
   * The last sitting the recorder announced the end of, or `null` if none has
   * ended since this window opened.
   *
   * Passed in from the shell's one subscription, for the reason {@link link} is.
   * It is what a recording that ended *by itself* is made of: `useRecording`
   * only learns of an ending it asked for, so without this the panel has nothing
   * to say about a recording a size change stopped (issue #625).
   */
  readonly ended: SessionSummary | null;
}

/** The Home screen. */
export function HomeScreen({ link, ended }: HomeScreenProps): ReactNode {
  const recording = useRecording(link);
  const now = describeRecordingNow(link, recording.status, recording.problem);
  const resized = describeResizeEnding(ended);
  const control = describeRecordControl(link, recording.status, recording.target);
  const navigate = useNavigate();
  const { read: sessions } = useSessions('', RECENT);
  // A second read with a query rather than a command of its own: `favourite` is
  // already in the query language, and the library answers it (issue #695).
  const { read: favourites } = useSessions('favourite', RECENT);
  /*
   * The clips of the sittings already read, newest first. No second question
   * asked of the recorder: every sitting carries the clips cut from it.
   */
  const clipped = sessions.state === 'read' ? recentClips(sessions.value, RECENT_CLIPS) : [];

  /*
   * Under its own key in the route state, as the Library does it: the playback
   * screen reads the identifier in the address as a recording's when handed a
   * recording, and a clip's identifier means nothing to the index that holds
   * marks (`clipPlayback.ts`).
   */
  const playClip = (clip: LibraryClip): void => {
    if (clip.path === undefined) {
      return;
    }
    void navigate(clipPath(String(clip.clip_id)), {
      state: {
        clip: {
          path: clip.path,
          ...(clip.missing_since === undefined ? {} : { missing_since: clip.missing_since }),
        },
      },
    });
  };
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
       * reason for it, the control and the file, and nothing else.
       */}
      <section className="clipped-panel" aria-label="Recording now" aria-live="polite">
        <h2 className="clipped-panel__heading">{now.state}</h2>
        <p className="clipped-panel__body">{now.detail}</p>

        {now.elapsed !== undefined && (
          /*
           * `aria-live="off"` inside a polite region, deliberately. The region
           * is live because a recording starting is worth announcing; this
           * figure changes every second, and a screen reader reading a new
           * duration aloud once a second would make the screen unusable and
           * drown the announcement that actually matters (AGENTS.md section 46).
           * It stays in the accessibility tree and is read on demand.
           */
          <p className="clipped-panel__body" aria-live="off">
            Recording for {now.elapsed}, as the recorder last reported it.
          </p>
        )}

        {now.output !== undefined && (
          <p className="clipped-panel__body clipped-path">{now.output}</p>
        )}

        <p className="clipped-panel__body">
          <button
            type="button"
            className={
              control.action === 'stop'
                ? 'clipped-btn clipped-btn--secondary'
                : 'clipped-btn clipped-btn--primary'
            }
            disabled={control.action === null || recording.working}
            onClick={control.action === 'stop' ? recording.stop : recording.start}
          >
            {control.label}
          </button>
        </p>

        {control.reason !== undefined && (
          <p className="clipped-panel__body clipped-muted">{control.reason}</p>
        )}

        {/*
         * A refusal the recorder made, in the recorder's own words (AGENTS.md
         * section 45). `role="alert"` because it is the answer to something the
         * user just did, and the polite region around it would otherwise hold it
         * behind whatever else changed.
         */}
        {recording.refusal !== null && (
          <p className="clipped-panel__body" role="alert">
            {recording.refusal.message}
          </p>
        )}

        {/*
         * Where the recording that was just stopped ended up. The panel would
         * otherwise go from "Recording cs2.exe" straight to "not recording" and
         * take the path with it, leaving somebody with a file they had just made
         * and no idea where.
         */}
        {recording.finished !== null && (
          <>
            <p className="clipped-panel__body">Recording finished, and its file is closed.</p>
            <p className="clipped-panel__body clipped-path">{recording.finished.output}</p>
          </>
        )}

        {/*
         * The same courtesy for a recording that ended without being asked to.
         * `recording.finished` is set from the reply to a stop this window sent,
         * so a recording a size change stopped never reaches it — the state went
         * from "recording" to "idle" and the panel took the path with it,
         * leaving somebody with a file, no path to it, and nothing said about
         * why their recording had stopped (issue #625, ADR 0012).
         */}
        {resized !== undefined && (
          <>
            <p className="clipped-panel__body">{resized.detail}</p>
            <p className="clipped-panel__body clipped-path">{resized.output}</p>
          </>
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

      <section aria-label="Favourites">
        <h2 className="clipped-screen__heading">Favourites</h2>
        {/*
         * Here rather than only on the Library screen because these are what
         * automatic cleanup protects. Somebody who marks a sitting to keep it
         * and can never see which are marked cannot check that the thing
         * protecting their footage is working -- they find out when something
         * they meant to keep has gone (issue #695).
         */}
        {favourites.state === 'reading' && (
          <p className="clipped-screen__lead clipped-muted" aria-busy="true">
            Reading your library…
          </p>
        )}
        {favourites.state === 'unread' && (
          <p className="clipped-screen__lead">{describeProblem(favourites.problem)}</p>
        )}
        {favourites.state === 'read' && favourites.value.length === 0 && (
          <p className="clipped-screen__lead">
            Nothing is marked a favourite yet. Marking a sitting keeps it out of automatic cleanup,
            and it appears here.
          </p>
        )}
        {favourites.state === 'read' && favourites.value.length > 0 && (
          <SessionList sessions={favourites.value} label="Favourites" />
        )}
      </section>

      <section aria-label="Recently clipped">
        <h2 className="clipped-screen__heading">Recently clipped</h2>
        {/*
         * The clips of the sittings above, newest first. No second read: every
         * sitting Home already asks for carries the clips cut from it, so this
         * is a flatten and a sort. Until it was drawn, the one thing a player
         * presses a hotkey for was invisible on the screen the application
         * opens on.
         */}
        {sessions.state === 'reading' && (
          <p className="clipped-screen__lead clipped-muted" aria-busy="true">
            Reading your library…
          </p>
        )}
        {/*
         * No failure message of its own. This section reads nothing the
         * sittings list above has not already asked for, so a library that
         * could not be read is reported once rather than in every section that
         * happens to depend on the same answer.
         */}
        {sessions.state === 'read' && clipped.length === 0 && (
          <p className="clipped-screen__lead">
            Nothing has been clipped yet. Press the save-replay hotkey while a game is recording and
            the clip appears here.
          </p>
        )}
        {sessions.state === 'read' && clipped.length > 0 && (
          <ul aria-label="Recently clipped">
            {clipped.map(({ clip, session }) => (
              <li key={clip.clip_id}>
                {clip.path === undefined ? (
                  <span>{clip.title ?? 'A clip with no file'}</span>
                ) : (
                  <button
                    type="button"
                    aria-label={`Play ${clip.title ?? fileName(clip.path)}, ${
                      session.game_name ?? 'not recognised'
                    }`}
                    onClick={() => {
                      playClip(clip);
                    }}
                  >
                    {clip.title ?? fileName(clip.path)}
                  </button>
                )}{' '}
                <span className="clipped-muted">
                  {session.game_name ?? 'Not recognised'} · {formatMoment(clip.created_at)}
                  {clip.duration_seconds !== undefined &&
                    ` · ${formatDuration(clip.duration_seconds)}`}
                </span>
              </li>
            ))}
          </ul>
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
          /*
           * The summary width. The Games screen draws the same component with
           * the columns SPEC.md section 17 asks for; Home is what has been
           * recorded lately, above a list of sittings, and seven columns here
           * would bury it.
           */
          <GamesTable games={games.value} showing="summary" label="Games" />
        )}
      </section>
    </>
  );
}
