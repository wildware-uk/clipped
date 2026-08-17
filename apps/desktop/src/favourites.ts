import type { FavouriteMark } from '@clipped/shared';
import { invoke } from '@tauri-apps/api/core';
import { useCallback, useMemo, useState } from 'react';

import { asProblem, type LibraryProblem } from './library';

/**
 * Marking things to keep (issue #58, SPEC.md section 29).
 *
 * Every library read has carried `favourite` since the Library screen was built
 * and the database has carried `favourited_at` since its first migration —
 * automatic cleanup already protects what is marked — and until now nothing in
 * the product could *set* one. This is the half that was missing.
 *
 * # A mark is a state, not a toggle
 *
 * {@link setFavourite} says what the mark should be afterwards. Two windows open
 * on one library would disagree about what a toggle means, and a screen that has
 * just drawn a star already knows which way it points.
 *
 * # Why the page is not read again
 *
 * The obvious answer to "the row on screen is now out of date" is to re-read it,
 * which is what the trash does. It is wrong here: `useSessions` accumulates
 * pages, so re-reading would throw somebody who had scrolled through five pages
 * back to the first one because they starred something.
 *
 * So {@link useFavourites} keeps what it has been told. That is **not** a guess
 * about what the click did: the recorder answers with the mark the library
 * actually holds afterwards, read back after the write, so a request against a
 * row that has gone comes back `false` and the star does not fill in. It is a
 * more recent read of one row, not an optimistic update, and the distinction is
 * the whole reason `FavouriteMark` carries the state rather than an
 * acknowledgement.
 */

/** What can be favourited. */
export type FavouriteKind = 'session' | 'recording' | 'clip';

/** One thing that can be favourited, addressed the way its table is. */
export interface FavouriteTarget {
  /** Which sort of thing. */
  readonly kind: FavouriteKind;
  /** The sitting's own identifier, for `session`. */
  readonly sessionId?: string;
  /** The library's integer identifier, for `recording` and `clip`. */
  readonly id?: number;
}

/** Marks one thing a favourite, or clears the mark. */
export async function setFavourite(
  target: FavouriteTarget,
  favourite: boolean,
): Promise<FavouriteMark> {
  return invoke<FavouriteMark>('set_favourite', {
    kind: target.kind,
    sessionId: target.sessionId ?? '',
    id: target.id ?? 0,
    favourite,
  });
}

/**
 * How one target is remembered.
 *
 * The kind is part of it: a recording and a clip can both be number 3, and a
 * key of the number alone would have starring one fill in the other.
 */
export function favouriteKey(target: FavouriteTarget): string {
  return `${target.kind}:${target.sessionId ?? ''}:${String(target.id ?? 0)}`;
}

/** What the last change turned out to be. */
export type FavouriteOutcome =
  { readonly state: 'idle' } | { readonly state: 'failed'; readonly problem: LibraryProblem };

/** Marking things, and what is known about what is marked. */
export interface Favourites {
  /**
   * Whether this is a favourite, given what the page said.
   *
   * `asRead` is the row's own `favourite` field. What this adds is anything
   * more recent: a target changed since that page arrived is answered from the
   * recorder's reply instead.
   */
  readonly isFavourite: (target: FavouriteTarget, asRead: boolean) => boolean;
  /** Whether a change to this target is in flight. */
  readonly isChanging: (target: FavouriteTarget) => boolean;
  /** Sets the mark to `favourite`. */
  readonly set: (target: FavouriteTarget, favourite: boolean) => void;
  /** Why the last change did not happen, when it did not. */
  readonly outcome: FavouriteOutcome;
}

/**
 * The marking a screen does, and what it has been told since it read its page.
 *
 * One outcome rather than one per row, for the reason `useRecordingActions`
 * gives: a screen keeping a message against every row accumulates stale ones
 * nobody dismissed.
 */
export function useFavourites(): Favourites {
  const [known, setKnown] = useState<ReadonlyMap<string, boolean>>(new Map());
  const [changing, setChanging] = useState<ReadonlySet<string>>(new Set());
  const [outcome, setOutcome] = useState<FavouriteOutcome>({ state: 'idle' });

  const set = useCallback((target: FavouriteTarget, favourite: boolean): void => {
    const key = favouriteKey(target);
    setChanging((previous) => new Set(previous).add(key));
    setOutcome({ state: 'idle' });

    setFavourite(target, favourite)
      .then((mark) => {
        // The state the library holds, not the state that was asked for. They
        // differ for a row that has gone, and a star that filled in anyway
        // would be a window disagreeing with its own next read.
        setKnown((previous) => new Map(previous).set(key, mark.favourite));
      })
      .catch((thrown: unknown) => {
        setOutcome({ state: 'failed', problem: asProblem(thrown) });
      })
      .finally(() => {
        setChanging((previous) => {
          const next = new Set(previous);
          next.delete(key);
          return next;
        });
      });
  }, []);

  return useMemo(
    () => ({
      isFavourite: (target, asRead) => known.get(favouriteKey(target)) ?? asRead,
      isChanging: (target) => changing.has(favouriteKey(target)),
      set,
      outcome,
    }),
    [known, changing, set, outcome],
  );
}

/**
 * What a window tells somebody about a mark that would not go on.
 *
 * Every branch is a different useful next step (AGENTS.md section 45). The
 * default is the recorder's own sentence: a target it refused says which field
 * was missing, and this window cannot say it better.
 */
export function describeFavouriteProblem(problem: LibraryProblem): string {
  switch (problem.code) {
    case 'unknown_command':
      return 'The recorder that is running is older than this window and cannot favourite anything. Restarting Clipped starts the recorder that came with it.';
    case 'no_recorder_configured':
    case 'recorder_unreachable':
      return `Clipped could not ask the recorder to change that. ${problem.message}`;
    case 'library_unavailable':
      return `${problem.message} Nothing was changed.`;
    default:
      return problem.message;
  }
}
