import type { ReactNode } from 'react';

import { describeGameDetection, whatWorksToday } from './gameDetection';
import { GamesTable } from './GamesTable';
import { describeProblem, useGames } from './library';
import type { RecorderLinkState } from './useRecorderLink';
import { WaitingOn, type Waiting } from './WaitingOn';

/**
 * The Games screen (issue #107).
 *
 * # What this screen shows, and why it is not a list of games
 *
 * SPEC.md sections 6 and 17 draw a table of detected games with sessions, clips
 * and storage against each, and the application deck draws the same table with
 * an Add Game control above it. **None of that data can reach this window in
 * this build, and none of those controls would do anything**, so none of it is
 * drawn (AGENTS.md section 27):
 *
 * - the catalogue that says which processes are games is the recorder's, in
 *   `clipped-game-detection` — half of it compiled in, half of it the user's
 *   overlay at `%LOCALAPPDATA%\Clipped\games.toml`. The control protocol has no
 *   command that lists it, and this window has no file-system permission to
 *   read it itself: `src-tauri/capabilities/default.json` grants three `core:`
 *   permissions and nothing else. Issue #245;
 * - registering an unknown executable, renaming, excluding and disabling
 *   capture per game all write that same overlay, so they have no path out of
 *   this window either. Issue #45 owns the behaviour, #245 the way to reach it;
 * - sessions, clips and storage per game come from the library index, which is
 *   issue #55. The session sidecars `clipped-recorder watch` writes beside its
 *   recordings (`docs/sessions.md`) are the only record that exists today, and
 *   nothing can read one from here;
 * - which game is being recorded *now* was issue #241, and is not waiting on
 *   anything: the status carries a `SessionSummary` with `game_id` and
 *   `game_name`, and Home draws it through `describeRecordingNow`. It is not
 *   drawn twice. SPEC.md section 6 asks this screen for detection sources and
 *   the game database rather than for a live recording panel, so this is a
 *   choice about where one thing is shown and not a promise outstanding.
 *
 * What is left is real, and it is the one thing somebody opening this screen
 * most needs to know: **whether anything is detecting games at all**. That is
 * drawn from the recorder link, it changes when the link does, and it is the
 * whole of what this screen claims.
 */

/**
 * Everything SPEC.md sections 6 and 17 ask of this screen, against what each
 * one is waiting for.
 *
 * One row left here when issue #55 closed and nobody came back: "sessions,
 * clips, favourites and storage against each game", waiting on "the library
 * index that counts them". That index has existed for some time, `library_games`
 * carries exactly those figures to this window, and `useGames` — whose own
 * documentation says "the figures on the Games screen" — was being used by Home
 * and by the per-game settings and not by this screen. The table below is that
 * row, drawn.
 *
 * This table is the alternative to drawing four convincing empty columns. An
 * empty table headed Game / Recording / Last played is indistinguishable from a
 * machine that has played nothing, and this screen would be claiming to have
 * looked. It has not, because it cannot.
 *
 * The table itself is `WaitingOn`, shared with Home and Library (issue #60),
 * which draw one for the same reason. The rows below stay here: they are this
 * screen's promise, not the component's.
 */
const MISSING: readonly Waiting[] = [
  {
    shows: 'Every game Clipped knows, with the executable and launcher it is recognised by',
    needs:
      'A protocol command that reads the catalogue, including the user’s own entries. Issue #245',
  },
  {
    shows:
      'Adding an unknown executable, renaming a game, excluding an application, and disabling capture per game',
    needs:
      'The same command, able to write the user overlay rather than the shipped seed data. Issues #45 and #245',
  },
];

/** What the Games screen is given. */
export interface GamesScreenProps {
  /**
   * Where the recorder link stands, or `null` outside the Clipped window.
   *
   * Passed in rather than taken from `useRecorderLink` here, so that the shell
   * holds one subscription rather than two and this screen is a pure function
   * of what it is told.
   */
  readonly link: RecorderLinkState | null;
}

/** The Games screen. */
export function GamesScreen({ link }: GamesScreenProps): ReactNode {
  const detection = describeGameDetection(link);
  const games = useGames();

  return (
    <>
      <h1 className="clipped-screen__title">Games</h1>

      <p className="clipped-screen__lead">
        Clipped records a game because it launched. The recorder holds the catalogue that says which
        processes are games, and writes a session record beside the files each sitting produces.
      </p>

      {/*
       * A live region, like the recorder status in the sidebar, so that a
       * recorder appearing or going is announced rather than only drawn. It
       * carries the state and the reason for it and nothing else; there is no
       * mark beside it, because unlike the sidebar's block this one is a
       * sentence rather than a word and so has no colour to be the only signal.
       */}
      <section className="clipped-panel" aria-label="Game detection" aria-live="polite">
        <h2 className="clipped-panel__heading">{detection.state}</h2>
        <p className="clipped-panel__body">{detection.detail}</p>
        <p className="clipped-panel__body clipped-muted">{whatWorksToday(link)}</p>
      </section>

      <h2 className="clipped-screen__heading">What has been recorded</h2>

      <section className="clipped-panel" aria-label="Games recorded">
        {games.state === 'reading' && (
          <p className="clipped-panel__body clipped-muted" aria-busy="true">
            Reading your library…
          </p>
        )}
        {games.state === 'unread' && (
          <p className="clipped-panel__body">{describeProblem(games.problem)}</p>
        )}
        {/*
         * "No games recorded yet" and not an empty table. SPEC.md section 6 asks
         * for an empty state that reflects the truth rather than sample data,
         * and an empty table under those headings is indistinguishable from a
         * library that could not be read.
         */}
        {games.state === 'read' && games.value.length === 0 && (
          <p className="clipped-panel__body">
            Your library was read and holds no games yet. A game appears here once Clipped has
            recorded a sitting of it.
          </p>
        )}
        {games.state === 'read' && games.value.length > 0 && (
          <GamesTable games={games.value} showing="everything" label="Games recorded" />
        )}
      </section>

      <h2 className="clipped-screen__heading">What this screen will show</h2>
      <p className="clipped-screen__lead clipped-muted">
        None of it is drawn yet, and none of it is invented in the meantime. Each row names the work
        that supplies it.
      </p>

      <WaitingOn heading="What the Games screen will show" rows={MISSING} />
    </>
  );
}
