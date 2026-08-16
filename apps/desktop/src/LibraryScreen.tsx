import { type ReactNode, useState } from 'react';

import { describeProblem, headlineProblem, useSessions, useTrash } from './library';
import {
  describeActionProblem,
  fileName,
  headlineActionProblem,
  useRecordingActions,
} from './recordingActions';
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
    shows: 'Playing a recording inside this window, rather than in your own player',
    needs:
      'WebView2 cannot decode the uncompressed sound the archival file carries (#392), and three other blockers. Issue #304. Open plays it in whatever you already use',
  },
  {
    shows: 'Playing a clip, from the list',
    needs: 'The playback screen. Issue #52',
  },
  {
    shows: 'Restoring something deleted by mistake, and emptying the trash',
    needs:
      'The two commands that change it. The trash itself is built (#94) and listing it is on this screen now (#450); `restore_from_trash` and `empty_trash` are the other half, and emptying has to take back the listing it was shown, so a trash that changed in between is refused rather than emptied',
  },
];

/** How a size is shown, in the unit a person reads. */
function size(bytes: number | undefined): string {
  if (bytes === undefined) {
    // Never measured, which is not the same as empty. A screen that showed
    // `0 B` for a file nobody weighed would be inventing a measurement.
    return 'size unknown';
  }
  if (bytes >= 1_000_000_000) {
    return `${(bytes / 1_000_000_000).toFixed(1)} GB`;
  }
  if (bytes >= 1_000_000) {
    return `${(bytes / 1_000_000).toFixed(1)} MB`;
  }
  return `${bytes} B`;
}

/**
 * What is in the trash, and what it would cost to keep or free.
 *
 * The half of issue #450 that is a read. Restoring and emptying change
 * something and are separate commands; until they land this says what is there
 * rather than pretending the feature is absent — a user who deleted something
 * can at least see that it is recoverable, and where the file went.
 */
function Trash(): ReactNode {
  const read = useTrash();

  return (
    <section className="clipped-panel" aria-label="Trash">
      <h2 className="clipped-panel__heading">Trash</h2>

      {read.state === 'reading' && <p className="clipped-panel__body">Reading the trash…</p>}

      {read.state === 'unread' && (
        <>
          {/*
           * Its own headline rather than `headlineProblem`'s: the sessions
           * panel above says "Your library could not be read", and two panels
           * saying the same sentence about different reads would leave somebody
           * unable to tell which one failed.
           */}
          <h3 className="clipped-panel__heading">The trash could not be read</h3>
          <p className="clipped-panel__body">{describeProblem(read.problem)}</p>
        </>
      )}

      {read.state === 'read' && read.value.items.length === 0 && (
        <p className="clipped-panel__body">
          Nothing has been deleted. Anything you delete waits here before it goes for good.
        </p>
      )}

      {read.state === 'read' && read.value.items.length > 0 && (
        <>
          <p className="clipped-panel__body">
            {read.value.total_items} thing(s), {size(read.value.total_bytes)}, in{' '}
            <code className="clipped-code">{read.value.directory}</code>.
          </p>
          <table className="clipped-table">
            <thead>
              <tr>
                <th scope="col">What</th>
                <th scope="col">Deleted</th>
                <th scope="col">Size</th>
                <th scope="col">Where it was</th>
              </tr>
            </thead>
            <tbody>
              {read.value.items.map((item) => (
                <tr key={`${item.kind}-${String(item.id)}`}>
                  <th scope="row">{item.kind}</th>
                  <td>{item.deleted_at.slice(0, 10)}</td>
                  <td>{size(item.size_bytes)}</td>
                  {/*
                   * The path it had, not the one it has: a file inside the
                   * trash is named for the trash, and asking somebody to
                   * recognise their own recording by a name they have never
                   * seen is not showing them anything.
                   */}
                  <td>
                    <code className="clipped-code">{item.original_path}</code>
                    {item.dependent_clips > 0 && (
                      <span className="clipped-muted">
                        {' '}
                        — {item.dependent_clips} clip(s) were cut from this
                      </span>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          <p className="clipped-panel__body clipped-muted">
            Restoring puts a file back where it was, byte for byte, and is the other half of this
            screen (issue #450). Deleting the file from the folder above does the same job and
            breaks nothing.
          </p>
        </>
      )}
    </section>
  );
}

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
  const actions = useRecordingActions();

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
          <h2 className="clipped-panel__heading">{headlineProblem(read.problem)}</h2>
          <p className="clipped-panel__body">{describeProblem(read.problem)}</p>
          {/*
           * Only for a library that could not be read. A query that would not
           * parse is the user's own typing and needs no reassurance about their
           * recordings being safe — offering it would imply something is wrong
           * with the library when nothing is.
           */}
          {read.problem.code !== 'invalid_parameters' && (
            <p className="clipped-panel__body clipped-muted">
              This is not the same as an empty library, and nothing here has been guessed at. Your
              recordings are ordinary video files and play in anything; the session record beside
              them is JSON, and is what the index is rebuilt from.
            </p>
          )}
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
          <SessionList sessions={read.value} label="Sessions" actions={actions} />
          {/*
           * One region for the outcome of the last thing somebody asked for,
           * announced rather than only drawn: opening a recording and showing
           * one in Explorer both happen in another application, and a window
           * that said nothing would leave somebody unsure whether the button
           * did anything at all (AGENTS.md sections 45 and 46).
           */}
          <p role="status" className="clipped-panel__body">
            {actions.outcome.state === 'working' &&
              `${actions.outcome.what} ${fileName(actions.outcome.path)}…`}
            {actions.outcome.state === 'done' && actions.outcome.message}
            {actions.outcome.state === 'failed' &&
              `${headlineActionProblem(actions.outcome.problem)}. ${describeActionProblem(
                actions.outcome.problem,
              )}`}
          </p>
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

      <Trash />

      <h2 className="clipped-screen__heading">What this screen will show</h2>
      <p className="clipped-screen__lead clipped-muted">
        None of it is drawn yet, and none of it is invented in the meantime. Each row names the work
        that supplies it.
      </p>

      <WaitingOn heading="What the Library screen will show" rows={WAITING} />
    </>
  );
}
