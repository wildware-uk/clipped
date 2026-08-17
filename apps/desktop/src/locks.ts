import type { LockMark } from '@clipped/shared';
import { invoke } from '@tauri-apps/api/core';
import { useCallback, useMemo, useState } from 'react';

import { asProblem, type LibraryProblem } from './library';

/**
 * Keeping a recording out of automatic cleanup's reach (issue #472).
 *
 * # What a lock is, and what it is not
 *
 * It protects against **automatic cleanup only**. When a storage limit is
 * reached the sweep takes the oldest recordings it is allowed to take, and a
 * locked one is not one of those. Deleting it yourself works exactly as it did:
 * the lock does not make the delete button ask twice, and this window must not
 * imply that it does.
 *
 * That is why the control is worded as it is. "Locked" alone would read as "you
 * cannot delete this", which is false.
 *
 * # A lock and a favourite are different things
 *
 * Both protect against cleanup, which invites the question of why there are
 * two. A favourite is a statement about the recording — this one was good. A
 * lock is a statement about the disk — do not reclaim this space. Somebody with
 * four hundred favourites has said something about their library; somebody with
 * four locks has said something about a full drive.
 *
 * They are also shaped differently: favouriting a sitting does not favourite
 * its recordings, and locking one **does** protect them, because the unit
 * somebody decides what to keep in is the night.
 *
 * # Locked, and protected, are two questions
 *
 * A recording inside a locked sitting has no lock of its own and cleanup will
 * still not take it. `protected` is the one a padlock is drawn from;
 * {@link LockMark.locked} is the one a *control* is drawn from, because a
 * recording protected by its sitting has nothing of its own to release.
 */

/** What can be locked. Clips cannot: cleanup does not delete them. */
export type LockKind = 'session' | 'recording';

/** One thing that can be locked, addressed the way its table is. */
export interface LockTarget {
  /** Which sort of thing. */
  readonly kind: LockKind;
  /** The sitting's own identifier, for `session`. */
  readonly sessionId?: string;
  /** The library's integer identifier, for `recording`. */
  readonly id?: number;
}

/** Locks one thing against automatic cleanup, or unlocks it. */
export async function setLock(target: LockTarget, locked: boolean): Promise<LockMark> {
  return invoke<LockMark>('set_lock', {
    kind: target.kind,
    sessionId: target.sessionId ?? '',
    id: target.id ?? 0,
    locked,
  });
}

/** How one target is remembered. */
export function lockKey(target: LockTarget): string {
  return `${target.kind}:${target.sessionId ?? ''}:${String(target.id ?? 0)}`;
}

/** What the last change turned out to be. */
export type LockOutcome =
  { readonly state: 'idle' } | { readonly state: 'failed'; readonly problem: LibraryProblem };

/** Locking things, and what is known about what is locked. */
export interface Locks {
  /** Whether this has a lock of its own, given what the page said. */
  readonly isLocked: (target: LockTarget, asRead: boolean) => boolean;
  /** Whether a change to this target is in flight. */
  readonly isChanging: (target: LockTarget) => boolean;
  /** Sets the lock to `locked`. */
  readonly set: (target: LockTarget, locked: boolean) => void;
  /** Why the last change did not happen, when it did not. */
  readonly outcome: LockOutcome;
}

/**
 * The locking a screen does, and what it has been told since it read its page.
 *
 * The same shape as `useFavourites`, and for the same reasons: the page is not
 * re-read, because `useSessions` accumulates pages and re-reading would throw
 * somebody back to the first one; and what is kept is the recorder's own reply
 * rather than the click, so a request against a row that has gone leaves the
 * padlock open.
 */
export function useLocks(): Locks {
  const [known, setKnown] = useState<ReadonlyMap<string, boolean>>(new Map());
  const [changing, setChanging] = useState<ReadonlySet<string>>(new Set());
  const [outcome, setOutcome] = useState<LockOutcome>({ state: 'idle' });

  const set = useCallback((target: LockTarget, locked: boolean): void => {
    const key = lockKey(target);
    setChanging((previous) => new Set(previous).add(key));
    setOutcome({ state: 'idle' });

    setLock(target, locked)
      .then((mark) => {
        setKnown((previous) => new Map(previous).set(key, mark.locked));
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
      isLocked: (target, asRead) => known.get(lockKey(target)) ?? asRead,
      isChanging: (target) => changing.has(lockKey(target)),
      set,
      outcome,
    }),
    [known, changing, set, outcome],
  );
}

/** What a window tells somebody about a lock that would not go on. */
export function describeLockProblem(problem: LibraryProblem): string {
  switch (problem.code) {
    case 'unknown_command':
      return 'The recorder that is running is older than this window and cannot lock a recording. Restarting Clipped starts the recorder that came with it.';
    case 'no_recorder_configured':
    case 'recorder_unreachable':
      return `Clipped could not ask the recorder to change that. ${problem.message}`;
    case 'library_unavailable':
      return `${problem.message} Nothing was changed.`;
    default:
      return problem.message;
  }
}
