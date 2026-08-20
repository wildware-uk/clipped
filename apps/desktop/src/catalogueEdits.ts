/**
 * Changing the recorder's game catalogue from the window.
 *
 * The four edits `docs/game-detection.md` describes — register, rename, exclude
 * and forget — as the window reaches them. Every one is forwarded to the
 * recorder, which writes the user's own overlay at
 * `%LOCALAPPDATA%\Clipped\games.toml` and never the data Clipped ships
 * ([issue #245](https://github.com/wildware-uk/clipped/issues/245)).
 *
 * # Nothing here decides what the catalogue now holds
 *
 * Each edit answers with the whole catalogue, and that answer replaces what the
 * screen was drawing. The alternative — patching the row that changed — would
 * be this side reimplementing the precedence between the shipped entries and
 * the user's overlay, which is exactly the rule that has one home already.
 *
 * It also matters for the cases where an edit changes a row it was not about:
 * clearing a rename restores a name this side never held, and a registration
 * receives an identifier the recorder derived.
 */

import type { CatalogueGame } from '@clipped/shared';
import { invoke } from '@tauri-apps/api/core';
import { useCallback, useState } from 'react';

import { asProblem, type LibraryProblem } from './library';

/** What an edit produced: the entry it was about, and the whole catalogue. */
export interface CatalogueEdit {
  /**
   * The entry the edit was about.
   *
   * For a registration this is the identifier the recorder derived from the
   * name, which the caller could not have predicted.
   */
  readonly game_id: string;
  /** Every entry, as the catalogue listing gives them. */
  readonly games: readonly CatalogueGame[];
}

/** Registers a game the catalogue does not know. */
export async function registerGame(
  name: string,
  executable: string,
  pathContains: string | null,
): Promise<CatalogueEdit> {
  return invoke<CatalogueEdit>('register_game', {
    name,
    executable,
    pathContains,
  });
}

/**
 * Calls a game something else, or puts the shipped name back.
 *
 * `null` is the clearing form rather than a second function, because renaming
 * and un-renaming are the same decision seen from either end.
 */
export async function renameGame(gameId: string, name: string | null): Promise<CatalogueEdit> {
  return invoke<CatalogueEdit>('rename_game', { gameId, name });
}

/**
 * Excludes a game from recording, or stops excluding it.
 *
 * The state to be in rather than a toggle, for the reason `setFavourite` takes
 * one: two windows open on one catalogue cannot disagree about which way it
 * points.
 */
export async function setGameExcluded(gameId: string, excluded: boolean): Promise<CatalogueEdit> {
  return invoke<CatalogueEdit>('set_game_excluded', { gameId, excluded });
}

/**
 * Drops an entry the user added.
 *
 * Never the operation for a game Clipped ships: the next update would bring it
 * back, and excluding it is the one that lasts.
 */
export async function forgetGame(gameId: string): Promise<CatalogueEdit> {
  return invoke<CatalogueEdit>('forget_game', { gameId });
}

/** What the screen needs to draw the catalogue and change it. */
export interface CatalogueEditor {
  /**
   * The catalogue as it now stands, or `null` while it has never been read.
   *
   * An edit's answer replaces this whole, which is why an edit does not need
   * the screen to re-read.
   */
  readonly games: readonly CatalogueGame[] | null;
  /**
   * The entry an edit is in flight for, or `null`.
   *
   * One at a time, and named rather than a boolean: the row that is changing
   * is the row whose controls must be disabled, and disabling every row
   * because one is busy would be a worse answer than disabling none.
   */
  readonly pending: string | null;
  /**
   * Why the last edit did not happen, or `null`.
   *
   * Kept until the next edit rather than cleared on a timer. An edit that
   * failed silently is a person believing they excluded a game that is still
   * being recorded, which is the whole reason this screen exists.
   */
  readonly problem: LibraryProblem | null;
  /** Runs one edit, holding its result. */
  readonly apply: (gameId: string, edit: () => Promise<CatalogueEdit>) => Promise<void>;
}

/**
 * Holds the catalogue across edits.
 *
 * `read` is what the screen last read, and is used until the first edit answers
 * — after that this hook's own copy is the newer truth and wins.
 */
export function useCatalogueEditor(read: readonly CatalogueGame[] | null): CatalogueEditor {
  const [edited, setEdited] = useState<readonly CatalogueGame[] | null>(null);
  const [pending, setPending] = useState<string | null>(null);
  const [problem, setProblem] = useState<LibraryProblem | null>(null);

  const apply = useCallback(
    async (gameId: string, edit: () => Promise<CatalogueEdit>): Promise<void> => {
      setPending(gameId);
      setProblem(null);
      try {
        const result = await edit();
        setEdited(result.games);
      } catch (thrown: unknown) {
        setProblem(asProblem(thrown));
      } finally {
        setPending(null);
      }
    },
    [],
  );

  return { games: edited ?? read, pending, problem, apply };
}
