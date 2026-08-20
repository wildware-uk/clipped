import type {
  LibraryEventLane,
  CatalogueGame,
  LibraryClip,
  LibraryGame,
  LibrarySession,
  LibrarySessionPage,
  RestoredItem,
  TrashEmptied,
  TrashListing,
} from '@clipped/shared';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { useCallback, useEffect, useState } from 'react';

import { inTauriWindow } from './useRecorderLink';

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
 * Reads what is waiting in the trash.
 *
 * The whole of it rather than a page: the trash is what somebody deleted and
 * has not emptied, bounded by the retention period rather than by the size of
 * the library (issue #450).
 */
export async function readTrash(): Promise<TrashListing> {
  return invoke<TrashListing>('library_trash');
}

/**
 * What is in the trash, kept as a read so that "could not be asked" and "there
 * is nothing in it" are different states on screen.
 */
export function useTrash(): TrashView {
  const [read, setRead] = useState<LibraryRead<TrashListing>>({ state: 'reading' });
  const [asked, setAsked] = useState(0);

  useEffect(() => {
    let current = true;
    readTrash()
      .then((trash) => {
        if (current) {
          setRead({ state: 'read', value: trash });
        }
      })
      .catch((thrown: unknown) => {
        if (current) {
          setRead({ state: 'unread', problem: asProblem(thrown) });
        }
      });
    return () => {
      current = false;
    };
  }, [asked]);

  // Restoring and emptying change what is in the trash, so the screen has to
  // read it again afterwards — a listing that still showed a restored recording
  // would be a screen contradicting what the user just did.
  const again = useCallback(() => {
    setAsked((count) => count + 1);
  }, []);

  return { read, again };
}

/** What is in the trash, and how to ask again after changing it. */
export interface TrashView {
  /** The listing. */
  readonly read: LibraryRead<TrashListing>;
  /** Reads it again. */
  readonly again: () => void;
}

/** Puts one thing back where it was. */
export async function restoreFromTrash(kind: string, id: number): Promise<RestoredItem> {
  return invoke<RestoredItem>('restore_from_trash', { kind, id });
}

/**
 * Destroys everything in the trash.
 *
 * Both numbers are the listing the user was shown. The recorder refuses if the
 * trash has gained anything since, which is why this takes them rather than
 * nothing: emptying something somebody has not seen is the failure the
 * confirmation exists to prevent (issue #450).
 */
export async function emptyTrash(items: number, bytes: number): Promise<TrashEmptied> {
  return invoke<TrashEmptied>('empty_trash', { items, bytes });
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

/**
 * The name the Rust side emits recorder link events under.
 *
 * The same event `useRecorderLink` and `recordingActions` follow. A third
 * subscription rather than threading the link through two screens: they want
 * different halves of it, and a subscription is a callback in a list.
 */
const LINK_EVENT = 'recorder-link';

/** The one link event this file cares about. */
interface SessionEndedEnvelope {
  readonly event: 'session_ended';
  readonly [key: string]: unknown;
}

/**
 * How many sittings this window has been told have ended since it opened.
 *
 * The recorder announces a sitting the moment it closes one — a game exited, a
 * recording somebody stopped — and asks for the index to be brought up to date
 * as it does (`crates/ipc`'s `RecorderLinkEvent::SessionEnded`, issue #588).
 * That announcement is the only thing this window ever gets that says the
 * library has changed underneath it; without it a sitting somebody has just
 * finished playing is missing from every screen until Clipped is restarted.
 *
 * A count rather than the sitting itself, because what the reads below need is
 * a reason to ask again and nothing more. What the sitting *contains* comes
 * back from the recorder's own index a moment later, which is the same answer
 * every other row on the screen came from — rather than one row assembled from
 * an event and the rest from a read, which would be two sources for one screen.
 *
 * Zero outside the Clipped window, where there is no Rust side to hear from.
 */
function useSittingsEnded(): number {
  const [ended, setEnded] = useState(0);

  useEffect(() => {
    if (!inTauriWindow()) {
      return;
    }

    let current = true;
    const subscription = listen<SessionEndedEnvelope>(LINK_EVENT, ({ payload }) => {
      if (current && payload.event === 'session_ended') {
        setEnded((count) => count + 1);
      }
    });

    return () => {
      current = false;
      subscription
        // Wrapped rather than called bare: unsubscribing is a round trip to the
        // Rust side and returns a promise, and a bare call leaves its rejection
        // unhandled — which in a webview is a console error nobody reads.
        .then((unlisten) => Promise.resolve<void>(unlisten()))
        .catch(() => {
          // Nothing to do: the listener is going away with the screen.
        });
    };
  }, []);

  return ended;
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
 *
 * # Why a sitting ending re-reads, when a favourite does not
 *
 * {@link useFavourites} and {@link useLocks} deliberately do *not* re-read: a
 * star is clicked while somebody is reading the page, this hook accumulates
 * pages, and re-reading would throw a reader who had scrolled through five of
 * them back to the first because they marked a row.
 *
 * A sitting ending is the opposite situation on every count. It happens because
 * the person stopped playing a game, not because they touched this screen; the
 * row it adds is at the *top*, where no amount of paging will ever reveal it;
 * and there is nothing else that would ever bring it in — until issue #588 a
 * sitting somebody had just finished was absent from the Library until Clipped
 * was restarted. Nothing blanks while the re-read is in flight, because the
 * answer only changes when the new one arrives.
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
  const ended = useSittingsEnded();
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
  }, [asked, query, pageSize, ended]);

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

/**
 * Every game the recorder's catalogue knows.
 *
 * Deliberately not {@link readGames}, which answers a different question: that
 * is what has been *recorded*, and this is what the recorder *knows*. A game can
 * be in the catalogue and never played, which is most of them, and a sitting can
 * be recorded under no catalogue entry at all
 * ([issue #245](https://github.com/wildware-uk/clipped/issues/245)).
 */
export async function readCatalogue(): Promise<readonly CatalogueGame[]> {
  return invoke<CatalogueGame[]>('catalogue_games');
}

/**
 * What the recorder's catalogue knows.
 *
 * Read once when the screen mounts rather than on every sitting, unlike
 * {@link useGames}: a catalogue changes when the user edits their overlay or
 * installs a new build, and neither happens while somebody is looking at this
 * screen. There is no paging — a catalogue is tens to hundreds of entries and
 * the screen draws all of them.
 */
export function useCatalogue(): LibraryRead<readonly CatalogueGame[]> {
  const [read, setRead] = useState<LibraryRead<readonly CatalogueGame[]>>({ state: 'reading' });

  useEffect(() => {
    let current = true;
    readCatalogue()
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
 * What the library holds per game.
 *
 * Read again when a sitting ends, for the reason {@link useSessions} is: the
 * figures on the Games screen are how much has been recorded of each game, and
 * a sitting somebody has just finished playing is exactly what changes one.
 * There is no paging here to lose.
 */
export function useGames(): LibraryRead<readonly LibraryGame[]> {
  const ended = useSittingsEnded();
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
  }, [ended]);

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

/** A clip, and the sitting it was cut from. */
export interface ClipInSession {
  /** The clip itself. */
  readonly clip: LibraryClip;
  /** The sitting it belongs to, for naming the game it came from. */
  readonly session: LibrarySession;
}

/**
 * The newest clips across the sittings given, newest first.
 *
 * Home reads recent sittings already and every one of them carries its clips,
 * so "recently clipped" is a flatten and a sort rather than a query. That is
 * worth saying because the row Home used to draw claimed the opposite — that
 * this needed work that had not landed — while the answer was in a read the
 * screen was already making.
 *
 * # Why it sorts on the clip and not on the sitting
 *
 * A clip can be made long after the sitting it came from: somebody watches an
 * old recording and cuts a moment out of it. Ordering by the sitting would file
 * that clip under a date months old and bury it, which is the opposite of what
 * a "recently clipped" list is for.
 *
 * Clips with no `created_at` cannot happen — the protocol requires it — so
 * there is no third case here for one that sorts nowhere.
 */
export function recentClips(
  sessions: readonly LibrarySession[],
  limit: number,
): readonly ClipInSession[] {
  return sessions
    .flatMap((session) => session.clips.map((clip) => ({ clip, session })))
    .sort((left, right) => right.clip.created_at.localeCompare(left.clip.created_at))
    .slice(0, Math.max(0, limit));
}
