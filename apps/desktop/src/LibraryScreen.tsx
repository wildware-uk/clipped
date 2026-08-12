import { type ReactNode, useState } from 'react';

import { describeProblem, useSessions } from './library';
import { SessionList } from './SessionList';
import { WaitingOn, type Waiting } from './WaitingOn';

/**
 * The Library screen (issues #60 and #301).
 *
 * # What it draws now
 *
 * The sittings the index holds, newest first, a page at a time, and a search box
 * over them. Both are real reads: the window asks the recorder, the recorder
 * reads `library.db` and answers, and nothing on this screen is invented in the
 * meantime (`docs/library.md`, `docs/ipc.md`).
 *
 * Three states, never two. A library that could not be read says so and says
 * why; an empty library says it is empty; and the two are different sentences,
 * because "you have not recorded anything" over a database that could not be
 * opened is the fabricated state AGENTS.md section 27 forbids.
 *
 * # What it still does not draw, and why
 *
 * Clips and highlights (nothing creates one yet), favourites (nothing can
 * favourite anything yet), thumbnails and waveforms (they are files beside the
 * recordings and this window has no permission to load one), playback and the
 * trash. Each is a row in the table below, against the work that lands it —
 * the same contract the Games screen keeps.
 *
 * # And why there is still no tab strip
 *
 * Issue #215 asks for the deck's tab strip at the point a screen needs one.
 * There is one populated list here and two empty ones, so a strip over them
 * would be the component with no consumer that issue exists to avoid. It
 * belongs with the first screen that has three lists worth switching between,
 * which is this one once clips (#91) and highlights (#76) land.
 */

/** How many sittings a page of the Library screen holds. */
const PAGE = 25;

/**
 * What SPEC.md sections 17, 29 and 30 still ask of this screen, against what
 * each one is waiting for.
 *
 * A promise to the reader rather than a derived list. The rows that named issue
 * #301 for the session list, the search and a missing file are gone, because
 * those three are on the screen now.
 */
const WAITING: readonly Waiting[] = [
  {
    shows: 'Clips, and the highlights a session produced',
    needs:
      'The virtual clip model (#74), clip creation (#91) and automatic highlights (#76). The read carries them already',
  },
  {
    shows: 'Favourites, and filtering the list down to them (SPEC.md section 29)',
    needs:
      'Favouriting anything at all. Issue #58. The read already carries which things are favourited',
  },
  {
    shows: 'A thumbnail against each recording, and a waveform under each track',
    needs:
      'Thumbnails (#57) and waveforms (#66) are generated beside the files. This window can load neither, having no file-system permission, and how the bytes should reach it is issue #301',
  },
  {
    shows: 'Playing a clip, from the list',
    needs: 'The playback screen. Issue #52',
  },
  {
    shows: 'Restoring something deleted by mistake',
    needs: 'The trash, with its retention and restore. Issue #94',
  },
];

/** The Library screen. */
export function LibraryScreen(): ReactNode {
  /**
   * What has been typed, and what has been searched for.
   *
   * They are separate because the search runs when the form is submitted rather
   * than on every keystroke: a query is parsed by the recorder, and `game:` on
   * the way to `game:cs2` is a parse error nobody asked about.
   */
  const [typed, setTyped] = useState('');
  const [query, setQuery] = useState('');
  const { read, hasMore, loadingMore, loadMore } = useSessions(query, PAGE);

  return (
    <>
      <h1 className="clipped-screen__title">Library</h1>

      <p className="clipped-screen__lead">
        Every sitting Clipped records leaves video files and one session record beside them, in the
        output folder the recorder was given.
      </p>

      <form
        className="clipped-panel"
        role="search"
        onSubmit={(event) => {
          event.preventDefault();
          setQuery(typed.trim());
        }}
      >
        <label className="clipped-field" htmlFor="library-search">
          Search your library
        </label>
        <input
          id="library-search"
          type="search"
          value={typed}
          placeholder="game:cs2 tag:clutch"
          onChange={(event) => {
            setTyped(event.target.value);
          }}
        />
        <button type="submit">Search</button>
        <p className="clipped-panel__body clipped-muted">
          Words match anywhere; <code>game:</code>, <code>tag:</code>, <code>date:</code>,{' '}
          <code>duration:</code> and <code>favourite</code> narrow it, and <code>-</code> in front
          of a term excludes it.
        </p>
      </form>

      {read.state === 'reading' && (
        <section className="clipped-panel" aria-label="Sessions" aria-busy="true">
          <p className="clipped-panel__body">Reading your library…</p>
        </section>
      )}

      {read.state === 'unread' && (
        <section className="clipped-panel" aria-label="Sessions">
          <h2 className="clipped-panel__heading">Your library could not be read</h2>
          <p className="clipped-panel__body">{describeProblem(read.problem)}</p>
          <p className="clipped-panel__body clipped-muted">
            This is not the same as an empty library, and nothing here has been guessed at. Your
            recordings are ordinary video files and play in anything; the session record beside them
            is JSON, and is what the index is rebuilt from.
          </p>
        </section>
      )}

      {read.state === 'read' && read.value.length === 0 && (
        <section className="clipped-panel" aria-label="Sessions">
          <h2 className="clipped-panel__heading">
            {query === '' ? 'Nothing recorded yet' : 'Nothing matches that search'}
          </h2>
          <p className="clipped-panel__body">
            {query === ''
              ? 'Your library was read and holds no sittings. One appears here after Clipped has recorded a game.'
              : 'Your library was read and no sitting matches. Clearing the search shows everything.'}
          </p>
        </section>
      )}

      {read.state === 'read' && read.value.length > 0 && (
        <section aria-label="Sessions">
          <SessionList sessions={read.value} label="Sessions" />
          {hasMore && (
            <button
              type="button"
              disabled={loadingMore}
              onClick={() => {
                loadMore();
              }}
            >
              {loadingMore ? 'Loading…' : 'Show more'}
            </button>
          )}
        </section>
      )}

      <h2 className="clipped-screen__heading">What this screen will show</h2>
      <p className="clipped-screen__lead clipped-muted">
        None of it is drawn yet, and none of it is invented in the meantime. Each row names the work
        that supplies it.
      </p>

      <WaitingOn heading="What the Library screen will show" rows={WAITING} />
    </>
  );
}
