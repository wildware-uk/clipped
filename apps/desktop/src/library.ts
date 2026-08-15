import type {
  LibraryEventLane,
  LibraryGame,
  LibrarySession,
  LibrarySessionPage,
} from '@clipped/shared';
import { invoke } from '@tauri-apps/api/core';
import { useCallback, useEffect, useState } from 'react';

/**
 * Reading the recording library from the window.
 *
 * The window cannot open `library.db` — it has no file-system permission for it
 * and may not link `clipped-library` — so every question here is a Tauri command
 * in front of a protocol command the recorder answers from the index
 * (`docs/library.md`, `docs/ipc.md`, issue #301). What comes back is the
 * protocol's own types, taken from `@clipped/shared` rather than written a
 * second time (AGENTS.md section 55).
 *
 * # Three answers, never two
 *
 * Every read ends in one of three states, and the screens draw all three
 * differently:
 *
 * - **read**, holding what the index says, which may legitimately be nothing;
 * - **unread**, holding why — the recorder refused, or there was no recorder to
 *   ask;
 * - **reading**, while the round trip is in flight.
 *
 * Collapsing the first two would put "you have not recorded anything" on screen
 * over a library that could not be opened, which is the fabricated state
 * AGENTS.md section 27 forbids. That is why {@link LibraryRead} has no "empty"
 * case: empty is a successful read of an empty page.
 */

/** A library question that did not produce an answer. */
export interface LibraryProblem {
  /**
   * The recorder's own protocol code, or one of the window's own for a question
   * that never reached it.
   *
   * `library_unavailable` is an index that could not be read,
   * `invalid_parameters` a search that does not parse, `unknown_command` a
   * recorder built before this command existed, `recorder_unreachable` and
   * `no_recorder_configured` a question that never arrived.
   */
  readonly code: string;
  /** One sentence for a person. This is the part that is always shown. */
  readonly message: string;
}

/** What a library read produced, or why it produced nothing. */
export type LibraryRead<T> =
  | { readonly state: 'reading' }
  | { readonly state: 'read'; readonly value: T }
  | { readonly state: 'unread'; readonly problem: LibraryProblem };

/** Which page of the library to ask for. */
export interface SessionsRequest {
  /** How many sittings. The recorder clamps this to what one reply can carry. */
  readonly limit?: number;
  /** The cursor a previous page ended with. */
  readonly after?: string;
  /** A search query, in the language of `docs/search.md`. */
  readonly query?: string;
}

/** Reads one page of sittings. */
export async function readSessions(request: SessionsRequest): Promise<LibrarySessionPage> {
  return invoke<LibrarySessionPage>('library_sessions', {
    limit: request.limit ?? null,
    after: request.after ?? null,
    query: request.query ?? null,
  });
}

/** Reads what the library holds per game. */
export async function readGames(): Promise<readonly LibraryGame[]> {
  return invoke<LibraryGame[]>('library_games');
}

/**
 * Reads the marks on one recording's timeline.
 *
 * Already placed in that recording's file: `at` is nanoseconds into the file,
 * not a moment on the session's timeline. The recorder does that subtraction
 * because it needs the recording's span, which this process has no way to know.
 *
 * An empty `marks` means the recording has no events. It does **not** mean the
 * question was not asked — a caller that has not called this at all has no lane,
 * and the two are drawn differently.
 */
export async function readEvents(recording: string): Promise<LibraryEventLane> {
  return invoke<LibraryEventLane>('library_events', { recording });
}

/**
 * Whatever a rejected command threw, as something that can be rendered.
 *
 * The Tauri command rejects with the {@link LibraryProblem} its Rust side built,
 * but a window must not assume that: a runtime error thrown before the command
 * ran arrives here too, and dropping it would leave a screen stuck on "reading"
 * with no explanation (AGENTS.md section 15).
 */
export function asProblem(thrown: unknown): LibraryProblem {
  if (typeof thrown === 'object' && thrown !== null && 'message' in thrown) {
    const problem = thrown as { code?: unknown; message?: unknown };
    return {
      code: typeof problem.code === 'string' ? problem.code : 'unknown',
      message: typeof problem.message === 'string' ? problem.message : String(thrown),
    };
  }
  return { code: 'unknown', message: String(thrown) };
}

/**
 * What the window tells the user about a library it could not read.
 *
 * Each is a sentence and, where there is one, something to do about it
 * (AGENTS.md section 45). The code is the recorder's, so a code invented after
 * this build was compiled falls through to the message it came with rather than
 * to nothing.
 */
export function describeProblem(problem: LibraryProblem): string {
  switch (problem.code) {
    case 'no_recorder_configured':
    case 'recorder_unreachable':
      return `Clipped could not ask the recorder about your library. ${problem.message}`;
    case 'unknown_command':
      return 'The recorder that is running is older than this window and has no way to read the library. Restarting Clipped starts the recorder that came with it.';
    case 'library_unavailable':
      return `Your library could not be read. ${problem.message}`;
    default:
      return problem.message;
  }
}

/**
 * The heading that goes above {@link describeProblem}.
 *
 * A search the recorder would not parse is **not** a library that could not be
 * read: the library is fine and the query is not, the useful action is to fix
 * what was typed rather than to worry about the database, and a heading saying
 * otherwise would send somebody looking for a fault that is not there
 * (AGENTS.md sections 28 and 45).
 */
export function headlineProblem(problem: LibraryProblem): string {
  return problem.code === 'invalid_parameters'
    ? 'That search could not be run'
    : 'Your library could not be read';
}

/** One page of sittings, and the paging around it. */
export interface SessionsView {
  /** Where the first page stands. */
  readonly read: LibraryRead<readonly LibrarySession[]>;
  /** Whether there is another page to ask for. */
  readonly hasMore: boolean;
  /** Whether a further page is being fetched. */
  readonly loadingMore: boolean;
  /** Asks for the next page and appends it. */
  readonly loadMore: () => void;
}

/**
 * The sittings a query selects, a page at a time.
 *
 * The query is part of the effect's dependencies, so typing in a search box
 * re-reads from the beginning rather than appending matches to the results of
 * the last one.
 */
export function useSessions(query: string, pageSize: number): SessionsView {
  /**
   * What the last answer was, and which question it answered.
   *
   * The question is carried with the answer rather than reset in the effect,
   * because setting state in an effect body is a cascading render (and the lint
   * rule that says so is right): a stale answer is recognised by comparing the
   * two, not by clearing one.
   */
  const asked = `${String(pageSize)} ${query}`;
  const [answer, setAnswer] = useState<{
    readonly asked: string;
    readonly read: LibraryRead<readonly LibrarySession[]>;
    readonly cursor: string | undefined;
  }>({ asked, read: { state: 'reading' }, cursor: undefined });
  const [loadingMore, setLoadingMore] = useState(false);

  useEffect(() => {
    let current = true;
    readSessions({ limit: pageSize, ...(query === '' ? {} : { query }) })
      .then((page) => {
        // A page that arrived after the query changed is an answer to a
        // question nobody is asking any more.
        if (current) {
          setAnswer({
            asked,
            read: { state: 'read', value: page.sessions },
            cursor: page.next_cursor,
          });
        }
      })
      .catch((thrown: unknown) => {
        if (current) {
          setAnswer({
            asked,
            read: { state: 'unread', problem: asProblem(thrown) },
            cursor: undefined,
          });
        }
      });

    return (): void => {
      current = false;
    };
  }, [asked, query, pageSize]);

  const fresh = answer.asked === asked;
  const read: LibraryRead<readonly LibrarySession[]> = fresh ? answer.read : { state: 'reading' };
  const cursor = fresh ? answer.cursor : undefined;

  const loadMore = useCallback(() => {
    if (cursor === undefined || loadingMore) {
      return;
    }
    setLoadingMore(true);
    readSessions({ limit: pageSize, after: cursor, ...(query === '' ? {} : { query }) })
      .then((page) => {
        setAnswer((previous) =>
          previous.asked === asked && previous.read.state === 'read'
            ? {
                asked,
                read: { state: 'read', value: [...previous.read.value, ...page.sessions] },
                cursor: page.next_cursor,
              }
            : previous,
        );
      })
      .catch((thrown: unknown) => {
        setAnswer({
          asked,
          read: { state: 'unread', problem: asProblem(thrown) },
          cursor: undefined,
        });
      })
      .finally(() => {
        setLoadingMore(false);
      });
  }, [asked, cursor, loadingMore, pageSize, query]);

  return { read, hasMore: cursor !== undefined, loadingMore, loadMore };
}

/** What the library holds per game. */
export function useGames(): LibraryRead<readonly LibraryGame[]> {
  const [read, setRead] = useState<LibraryRead<readonly LibraryGame[]>>({ state: 'reading' });

  useEffect(() => {
    let current = true;
    readGames()
      .then((games) => {
        if (current) {
          setRead({ state: 'read', value: games });
        }
      })
      .catch((thrown: unknown) => {
        if (current) {
          setRead({ state: 'unread', problem: asProblem(thrown) });
        }
      });
    return (): void => {
      current = false;
    };
  }, []);

  return read;
}

/**
 * How much footage a sitting produced, in seconds.
 *
 * The recordings' own durations rather than the span from its start to its end:
 * a session left open across a dinner break did not record the dinner.
 */
export function footageSeconds(session: LibrarySession): number {
  return session.recordings.reduce(
    (total, recording) => total + (recording.duration_seconds ?? 0),
    0,
  );
}

/** What a sitting occupies on disk, counting only the files that are there. */
export function presentBytes(session: LibrarySession): number {
  return session.recordings.reduce(
    (total, recording) =>
      recording.missing_since === undefined ? total + (recording.size_bytes ?? 0) : total,
    0,
  );
}

/** How many of a sitting's files could not be found. */
export function missingCount(session: LibrarySession): number {
  return session.recordings.filter((recording) => recording.missing_since !== undefined).length;
}

/**
 * A duration in seconds, as `1 h 49 m` or `24 s`.
 *
 * Whole units, because a library list is scanned rather than read: nobody needs
 * a session's length to the second, and a column of `6540.482 s` is harder to
 * compare at a glance than one of `1 h 49 m`.
 */
export function formatDuration(seconds: number): string {
  const whole = Math.max(0, Math.round(seconds));
  const hours = Math.floor(whole / 3600);
  const minutes = Math.floor((whole % 3600) / 60);
  if (hours > 0) {
    return `${String(hours)} h ${String(minutes)} m`;
  }
  if (minutes > 0) {
    return `${String(minutes)} m`;
  }
  return `${String(whole)} s`;
}

/**
 * A size in bytes, in the units a person reads.
 *
 * Powers of 1024 with the units Windows shows, so that a figure here and the
 * one in Explorer for the same file agree.
 */
export function formatBytes(bytes: number): string {
  const units = ['bytes', 'KB', 'MB', 'GB', 'TB'];
  let value = Math.max(0, bytes);
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  const rounded = unit === 0 ? String(Math.round(value)) : value.toFixed(1);
  return `${rounded} ${units[unit] ?? 'bytes'}`;
}

/**
 * When a sitting was recorded, in the reader's own locale.
 *
 * The timestamp carries the offset it was recorded in, and `Date` keeps the
 * instant; what is shown is that instant on this machine's clock, which is the
 * one the person reading it is living in.
 *
 * A timestamp that cannot be read is shown verbatim rather than as "Invalid
 * Date": the index copies what the recorder wrote, and a sidecar whose clock
 * could not be formatted writes `unknown-time` (`docs/sessions.md`).
 */
export function formatMoment(timestamp: string): string {
  const moment = new Date(timestamp);
  if (Number.isNaN(moment.getTime())) {
    return timestamp;
  }
  return moment.toLocaleString(undefined, {
    dateStyle: 'medium',
    timeStyle: 'short',
  });
}
