import type { ExportProgress, LibraryRecording, TrashListing, TrashedItem } from '@clipped/shared';
import { exportFraction } from '@clipped/shared';
import { type ReactNode, useState } from 'react';
import { useNavigate } from 'react-router';

import { clipPath } from './clipPlayback';
import {
  asProblem,
  describeProblem,
  emptyTrash,
  headlineProblem,
  restoreFromTrash,
  useSessions,
  useTrash,
} from './library';
import {
  describeActionProblem,
  describeExportProgress,
  fileName,
  headlineActionProblem,
  useRecordingActions,
} from './recordingActions';
import { describeFavouriteProblem, useFavourites } from './favourites';
import { describeLockProblem, useLocks } from './locks';
import { SessionList } from './SessionList';
import type { RecorderLinkState } from './useRecorderLink';
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
 * those three are on the screen now, and so is the one for favouriting: every
 * sitting and every recording has a Keep control, which is issue #58.
 */
const WAITING: readonly Waiting[] = [
  {
    shows: 'Clips, and the highlights a session produced',
    needs:
      'The virtual clip model (#74), clip creation (#91) and automatic highlights (#76). The read carries them already',
  },
  {
    shows: 'Filtering the list down to favourites with one control',
    needs:
      'A control on this screen (#60). Marking is done and `favourite` is already in the query language, so this is a button that types it into the search box (`docs/search.md`)',
  },
  {
    shows: 'A thumbnail against each recording, and a waveform under each track',
    needs:
      'Thumbnails (#57) and waveforms (#66) are generated beside the files. This window can load neither, having no file-system permission, and how the bytes should reach it is issue #301',
  },
  {
    shows: 'Playing a clip, from the list',
    needs:
      'Clips themselves: the virtual clip model (#74) and clip creation (#91). A recording plays from here already (#304)',
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
  const { read, again } = useTrash();
  const [outcome, setOutcome] = useState<string | undefined>(undefined);
  const [confirming, setConfirming] = useState(false);

  const restore = (item: TrashedItem) => {
    restoreFromTrash(item.kind, item.id)
      .then((restored) => {
        setOutcome(
          restored.file_restored
            ? `Put ${restored.path} back${restored.renamed ? ', under a new name because something was in the way' : ''}.`
            : `${restored.path} is back in your library, and its file had already gone before it was deleted, so it will show as missing.`,
        );
        again();
      })
      .catch((thrown: unknown) => {
        setOutcome(describeProblem(asProblem(thrown)));
      });
  };

  const empty = (listing: TrashListing) => {
    setConfirming(false);
    // The counts the user was looking at. If the trash has gained anything
    // since, the recorder refuses and says so rather than deleting something
    // they never saw.
    emptyTrash(listing.total_items, listing.total_bytes)
      .then((emptied) => {
        const refused =
          emptied.refused.length > 0
            ? ` ${String(emptied.refused.length)} could not be removed: ${emptied.refused.join('; ')}`
            : '';
        setOutcome(
          `Removed ${String(emptied.removed)} thing(s), ${size(emptied.reclaimed_bytes)}.${refused}`,
        );
        again();
      })
      .catch((thrown: unknown) => {
        setOutcome(describeProblem(asProblem(thrown)));
        again();
      });
  };

  return (
    <section className="clipped-panel" aria-label="Trash">
      <h2 className="clipped-panel__heading">Trash</h2>

      {read.state === 'reading' && <p className="clipped-panel__body">Reading the trash…</p>}

      {read.state === 'unread' && (
        <>
          {/*
           * Its own headline rather than `headlineProblem`'s: the sessions panel
           * above says "Your library could not be read", and two panels saying
           * the same sentence about different reads would leave somebody unable
           * to tell which one failed.
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
                <th scope="col">Put back</th>
              </tr>
            </thead>
            <tbody>
              {read.value.items.map((item) => (
                <tr key={`${item.kind}-${String(item.id)}`}>
                  <th scope="row">{item.kind}</th>
                  <td>{item.deleted_at.slice(0, 10)}</td>
                  <td>{size(item.size_bytes)}</td>
                  {/*
                   * The path it had, not the one it has: a file inside the trash
                   * is named for the trash, and asking somebody to recognise
                   * their own recording by a name they have never seen is not
                   * showing them anything.
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
                  <td>
                    <button
                      type="button"
                      className="clipped-btn clipped-btn--secondary"
                      onClick={() => {
                        restore(item);
                      }}
                    >
                      Restore
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>

          {/*
           * Two presses rather than one, and the second says what it is about to
           * destroy. Emptying is the one thing on this screen that cannot be
           * undone, and a single button beside a list of somebody's recordings
           * is a mis-click away from losing all of them (AGENTS.md section 17).
           */}
          {confirming ? (
            <p className="clipped-panel__body">
              Destroy {read.value.total_items} thing(s), {size(read.value.total_bytes)}? This cannot
              be undone.{' '}
              <button
                type="button"
                className="clipped-btn clipped-btn--secondary"
                onClick={() => {
                  empty(read.value);
                }}
              >
                Empty the trash
              </button>{' '}
              <button
                type="button"
                className="clipped-btn clipped-btn--secondary"
                onClick={() => {
                  setConfirming(false);
                }}
              >
                Keep them
              </button>
            </p>
          ) : (
            <button
              type="button"
              className="clipped-btn clipped-btn--secondary"
              onClick={() => {
                setConfirming(true);
              }}
            >
              Empty the trash…
            </button>
          )}
        </>
      )}

      {/*
       * One region for what the last action did, announced rather than only
       * drawn: restoring and emptying both change files, and a screen that said
       * nothing would leave somebody unsure whether the button worked
       * (AGENTS.md sections 45 and 46).
       */}
      {outcome !== undefined && (
        <p role="status" className="clipped-panel__body">
          {outcome}
        </p>
      )}
    </section>
  );
}

/** The Library screen. */
/**
 * How far a running export has got, as a bar.
 *
 * A native `<meter>`, which is the pattern `SetupScreen`'s input level already
 * uses: it is the element this is, it is announced as one, and it needs no
 * stylesheet of its own.
 *
 * **With no `value` when the recording never said how long it was.** That is a
 * meter's own way of saying "something is happening and I cannot say how much",
 * and it is the honest drawing for an interrupted recording — which keeps every
 * packet it wrote and no total (ADR 0001). A `value` of nought would be a claim,
 * and one that never moved for the length of the copy. What advances in that
 * case is the bytes, which are in the sentence above this.
 */
function ExportBar({ of }: { readonly of: ExportProgress }): ReactNode {
  const fraction = exportFraction(of);

  return (
    <meter
      aria-label={`Export progress for ${fileName(of.destination)}`}
      min={0}
      max={1}
      value={fraction ?? undefined}
    />
  );
}

/** What the Library screen is given. */
export interface LibraryScreenProps {
  /**
   * Where the recorder link stands, or `null` outside the Clipped window.
   *
   * Passed in rather than taken from `useRecorderLink` here, so that the shell
   * holds one subscription rather than several — the same arrangement Home, the
   * Games screen and playback use.
   *
   * This screen wants it for one thing: the features the attached recorder
   * advertised. The Export control is a command an older recorder does not
   * have, and asking before drawing the control is the difference between a
   * refusal before it is pressed and one after a file name has been chosen
   * (issue #447).
   */
  readonly link: RecorderLinkState | null;
}

export function LibraryScreen({ link }: LibraryScreenProps): ReactNode {
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
  const actions = useRecordingActions(link);
  const favourites = useFavourites();
  const locks = useLocks();
  const navigate = useNavigate();

  /**
   * Opens a recording on the playback screen, handing over the row.
   *
   * The address carries the index's own identifier, and the row goes with it:
   * this screen has it in its hand, and the playback screen would otherwise
   * have to read the whole library back to find the one file it needs. A
   * reload has no row, and that screen says so rather than inventing one —
   * looking one up cold is issue #52.
   */
  const play = (recording: LibraryRecording): void => {
    void navigate(clipPath(String(recording.recording_id)), { state: { recording } });
  };

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
          <SessionList
            sessions={read.value}
            label="Sessions"
            actions={actions}
            favourites={favourites}
            locks={locks}
            onPlay={play}
          />

          {/*
           * One region for the outcome of the last thing somebody asked for,
           * announced rather than only drawn: opening a recording and showing
           * one in Explorer both happen in another application, and a window
           * that said nothing would leave somebody unsure whether the button
           * did anything at all (AGENTS.md sections 45 and 46).
           */}
          <p role="status" className="clipped-panel__body">
            {favourites.outcome.state === 'failed' &&
              `That could not be kept. ${describeFavouriteProblem(favourites.outcome.problem)}`}
            {locks.outcome.state === 'failed' &&
              `That could not be kept from cleanup. ${describeLockProblem(locks.outcome.problem)}`}
            {favourites.outcome.state === 'idle' &&
              actions.outcome.state === 'working' &&
              (actions.outcome.progress === null
                ? `${actions.outcome.what} ${fileName(actions.outcome.path)}…`
                : `${actions.outcome.what} ${fileName(actions.outcome.path)} — ${describeExportProgress(
                    actions.outcome.progress,
                  )}`)}
            {favourites.outcome.state === 'idle' &&
              actions.outcome.state === 'done' &&
              actions.outcome.message}
            {favourites.outcome.state === 'idle' &&
              actions.outcome.state === 'failed' &&
              `${headlineActionProblem(actions.outcome.problem)}. ${describeActionProblem(
                actions.outcome.problem,
              )}`}
          </p>
          {/*
           * The bar, drawn only once the recorder has actually said something.
           *
           * A copy of a four-second recording finishes before there is anything
           * to report and this never appears; a copy of a two-hour one is what
           * it exists for (issue #446). It is deliberately absent rather than
           * sitting at nought against a recorder without the `export_progress`
           * feature — that recorder copies the file exactly as it always did
           * and cannot say how far it has got, and a bar that never moves is a
           * control that does nothing (AGENTS.md section 27). The sentence
           * above still says an export is running, which is what that recorder
           * can honestly support.
           *
           * `max` is 1 with no value when the recording never said how long it
           * was, which is what a native meter draws as "something is happening
           * and I cannot say how much" — the bytes copied are in the sentence.
           */}
          {actions.outcome.state === 'working' && actions.outcome.progress !== null && (
            <ExportBar of={actions.outcome.progress} />
          )}
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
