import type { LibrarySession, LibrarySessionPage } from '@clipped/shared';
import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { StrictMode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { App } from './App';
import { LibraryScreen } from './LibraryScreen';
import { stubRecorderLinkRuntime } from './test/recorderLinkRuntime';

/**
 * The Library screen's contract, as tests (issues #60 and #301).
 *
 * The screen now reads the real index through the recorder, so the cases here
 * are about what it does with each of the three answers a read can produce. Two
 * of them look alike on a careless screen and must not: a library that holds
 * nothing, and a library that could not be read. Drawing the second as the first
 * is the fabricated state AGENTS.md section 27 forbids, and it is the reason
 * `library_unavailable` exists on the wire at all.
 */

/** A sitting, with the fields SPEC.md section 17 draws. */
function session(overrides: Partial<LibrarySession> = {}): LibrarySession {
  return {
    session_id: 'cs2-20260811-201400',
    game_id: 'cs2',
    game_name: 'Counter-Strike 2',
    started_at: '2026-08-11T20:14:00+01:00',
    ended_at: '2026-08-11T22:03:00+01:00',
    favourite: false,
    recordings: [
      {
        recording_id: 12,
        session_index: 1,
        path: 'D:\\clips\\cs2-20260811-201400-1.mkv',
        started_at: '2026-08-11T20:14:00+01:00',
        duration_seconds: 6540,
        size_bytes: 9_812_009_112,
        favourite: false,
        tags: [],
      },
    ],
    clips: [],
    ...overrides,
  };
}

/** A page holding these sittings and nothing after them. */
function page(sessions: readonly LibrarySession[], next?: string): LibrarySessionPage {
  return next === undefined ? { sessions } : { sessions, next_cursor: next };
}

/** Mounts the application the way `main.tsx` does, StrictMode and all. */
function renderApp(): void {
  render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}

/** Opens the Library screen from the sidebar. */
async function openLibrary(user: ReturnType<typeof userEvent.setup>): Promise<void> {
  await user.click(screen.getByRole('link', { name: 'Library' }));
}

const ATTACHED = {
  link: 'attached',
  recorder_process_id: 7,
  status: { state: 'idle' },
} as const;

describe('the Library screen', () => {
  beforeEach(() => {
    window.location.hash = '';
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    window.location.hash = '';
  });

  it('lists the sittings the index holds, with what each produced', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve(page([session()])),
    });
    renderApp();
    await openLibrary(user);

    const table = await screen.findByRole('table', { name: 'Sessions' });
    const row = within(table).getAllByRole('row')[1] as HTMLElement;
    const cells = within(row)
      .getAllByRole('cell')
      .map((cell) => cell.textContent);

    expect(cells[0]).toBe('Counter-Strike 2');
    expect(cells[2]).toBe('1 h 49 m');
    expect(cells[3]).toBe('9.1 GB');
    expect(cells[4]).toBe('1 recording');
  });

  /*
   * Issue #60's third acceptance criterion and issue #305's second, on the
   * screen rather than on the wire. The index keeps the row and records
   * `missing_since`; a screen that dropped it would leave somebody unable to
   * tell a file they moved from a session that never recorded one.
   */
  it('says a recording whose file has gone is gone, rather than drawing it as present', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () =>
        Promise.resolve(
          page([
            session({
              recordings: [
                {
                  recording_id: 12,
                  session_index: 1,
                  path: 'D:\\clips\\gone.mkv',
                  started_at: '2026-08-11T20:14:00+01:00',
                  duration_seconds: 6540,
                  size_bytes: 9_812_009_112,
                  missing_since: '2026-08-12T09:00:00+01:00',
                  favourite: false,
                  tags: [],
                },
              ],
            }),
          ]),
        ),
    });
    renderApp();
    await openLibrary(user);

    const table = await screen.findByRole('table', { name: 'Sessions' });
    const row = within(table).getAllByRole('row')[1] as HTMLElement;
    const cells = within(row)
      .getAllByRole('cell')
      .map((cell) => cell.textContent);

    expect(cells[4]).toBe('1 recording, 1 file missing');
    expect(cells[3]).toBe(
      '0 bytes',
      // The space a file nobody can find is not occupying is not being used.
    );
  });

  /*
   * The distinction the whole read is shaped around. Both of these are a screen
   * with no sessions on it, and they must not say the same thing.
   */
  it('tells an empty library from a library it could not read', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime(ATTACHED, null, { sessions: () => Promise.resolve(page([])) });
    renderApp();
    await openLibrary(user);

    expect(
      await screen.findByRole('heading', { name: 'Nothing recorded yet' }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole('heading', { name: 'Your library could not be read' }),
    ).not.toBeInTheDocument();
  });

  it('says why a library it could not read could not be read', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () =>
        Promise.reject({
          code: 'library_unavailable',
          message: 'the recording library could not be opened: the database is from a newer build',
        }),
    });
    renderApp();
    await openLibrary(user);

    const panel = await screen.findByRole('heading', { name: 'Your library could not be read' });
    expect(panel).toBeInTheDocument();
    expect(screen.getByRole('main')).toHaveTextContent(/from a newer build/);
    expect(screen.queryByRole('heading', { name: 'Nothing recorded yet' })).not.toBeInTheDocument();
    expect(screen.queryByRole('table', { name: 'Sessions' })).not.toBeInTheDocument();
  });

  it('sends what was typed as a query, and says when nothing matches it', async () => {
    const user = userEvent.setup();
    const asked: (string | null)[] = [];
    stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: (args) => {
        asked.push(args['query'] as string | null);
        return Promise.resolve(page(args['query'] === null ? [session()] : []));
      },
    });
    renderApp();
    await openLibrary(user);
    await screen.findByRole('table', { name: 'Sessions' });

    await user.type(
      screen.getByRole('searchbox', { name: 'Search your library' }),
      'game:minecraft',
    );
    await user.click(screen.getByRole('button', { name: 'Search' }));

    expect(
      await screen.findByRole('heading', { name: 'Nothing matches that search' }),
    ).toBeInTheDocument();
    expect(asked).toContain('game:minecraft');
  });

  /*
   * A search that does not parse is refused by the recorder with the position
   * and what was expected there, and that sentence is what the user needs — not
   * an empty list, which would say the library holds nothing matching a query
   * nobody ever ran (AGENTS.md section 45).
   */
  it('shows what is wrong with a query the recorder would not parse', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: (args) =>
        args['query'] === null
          ? Promise.resolve(page([session()]))
          : Promise.reject({
              code: 'invalid_parameters',
              message:
                '`library_sessions` was not given a usable query: expected a term after `OR` at position 8',
            }),
    });
    renderApp();
    await openLibrary(user);
    await screen.findByRole('table', { name: 'Sessions' });

    await user.type(screen.getByRole('searchbox', { name: 'Search your library' }), 'kill OR');
    await user.click(screen.getByRole('button', { name: 'Search' }));

    expect(await screen.findByText(/expected a term after/)).toBeInTheDocument();
    // And it is said as a search that could not be run, not as a library that
    // could not be read. The library is fine; the query is not, and sending
    // somebody to look for a fault in their recordings would be wrong twice
    // over — it is untrue, and it hides the thing they can actually fix.
    expect(
      await screen.findByRole('heading', { name: 'That search could not be run' }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole('heading', { name: 'Your library could not be read' }),
    ).not.toBeInTheDocument();
    expect(screen.getByRole('main')).not.toHaveTextContent(/ordinary video files/);
  });

  /*
   * Issue #60's second acceptance criterion needs something to scroll. The
   * cursor is opaque to this window, so the only thing worth asserting is that
   * what came back is what goes out again — a screen that dropped it would page
   * from the beginning for ever, and a screen that offered the control with no
   * cursor would have a button that does nothing (AGENTS.md section 27).
   */
  it('asks for the next page with the cursor the last one ended with', async () => {
    const user = userEvent.setup();
    const asked: (string | null)[] = [];
    stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: (args) => {
        asked.push(args['after'] as string | null);
        return Promise.resolve(
          args['after'] === null
            ? page([session()], 'cursor-1')
            : page([session({ session_id: 'cs2-20260810-190000', game_name: 'Minecraft' })]),
        );
      },
    });
    renderApp();
    await openLibrary(user);

    await user.click(await screen.findByRole('button', { name: 'Show more' }));

    await waitFor(() => {
      expect(screen.getByRole('table', { name: 'Sessions' })).toHaveTextContent('Minecraft');
    });
    // Only the cursors, because the first page is asked for more than once:
    // StrictMode runs effects twice, and Home has already read its own five.
    expect(asked.filter((cursor) => cursor !== null)).toEqual(['cursor-1']);
    expect(screen.queryByRole('button', { name: 'Show more' })).not.toBeInTheDocument();
  });

  /*
   * A recorder from before issue #301 has no such command and refuses it by
   * name. The window has to say that rather than showing an empty library,
   * because the useful action — restart Clipped — is not one a user would guess.
   */
  it('says a recorder too old to read the library is too old', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () =>
        Promise.reject({
          code: 'unknown_command',
          message: 'this recorder has no `library_sessions` command',
        }),
    });
    renderApp();
    await openLibrary(user);

    expect(await screen.findByText(/older than this window/)).toBeInTheDocument();
  });

  /*
   * What SPEC.md sections 17, 29 and 30 still ask of this screen, and the issue
   * that supplies each — written out here rather than mapped from the screen's
   * own array.
   *
   * That distinction is the whole test. Walking the rendered rows and asserting
   * "two cells, and the second names some issue" is satisfied by a table holding
   * one invented row. The claim is that *these five* are what the screen still
   * owes, and only a list kept independently of the implementation can make it.
   *
   * Three rows have gone since issue #301: the session list, the search and a
   * recording whose file has gone are all on the screen, and each has a case of
   * its own above. Shrinking this list without honouring it fails here.
   */
  const MUST_BE_NAMED: readonly (readonly [string, RegExp, readonly number[]])[] = [
    ['clips and highlights', /^clips, and the highlights/i, [74, 76, 91]],
    ['favourites', /^favourites, and filtering/i, [58]],
    ['thumbnails and waveforms', /^a thumbnail against each recording/i, [57, 66, 301]],
    ['playing a clip', /^playing a clip/i, [52]],
    ['restoring something deleted', /^restoring something deleted/i, [94]],
  ];

  it('names each part it still owes, and the issue that lands it', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime(ATTACHED, null, { sessions: () => Promise.resolve(page([])) });
    renderApp();
    await openLibrary(user);

    // The library is empty in this case, so the only table on the screen is the
    // one that says what it still owes.
    const waiting = await screen.findByRole('table');
    expect(
      within(waiting)
        .getAllByRole('columnheader')
        .map((header) => header.textContent),
    ).toEqual(['What the Library screen will show', 'What has to exist first']);

    const rows = within(waiting).getAllByRole('row').slice(1);
    expect(rows).toHaveLength(MUST_BE_NAMED.length);

    for (const [subject, shows, issues] of MUST_BE_NAMED) {
      const matching = rows.filter((row) =>
        shows.test(within(row).getAllByRole('cell')[0]?.textContent ?? ''),
      );
      expect(matching, `one row for ${subject}`).toHaveLength(1);

      const needs = within(matching[0] as HTMLElement).getAllByRole('cell')[1]?.textContent ?? '';
      for (const issue of issues) {
        expect(needs, `${subject} is waiting on #${String(issue)}`).toMatch(
          new RegExp(`#${String(issue)}\\b`),
        );
      }
    }
  });

  /*
   * Reached with the keyboard alone, and focus lands in the screen afterwards so
   * that a screen reader announces the change. The shell owns both behaviours;
   * this checks them through the Library link specifically, because "keyboard
   * navigation" is one of issue #60's three acceptance criteria and a screen
   * only reachable by mouse fails it whatever the shell does.
   */
  it('is reached with Tab and Enter, and takes focus when it opens', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime({ link: 'connecting' });
    renderApp();

    // Skip link, Home, Library.
    await user.tab();
    await user.tab();
    await user.tab();
    expect(document.activeElement).toHaveTextContent('Library');

    await user.keyboard('{Enter}');

    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Library');
    expect(screen.getByRole('main')).toHaveFocus();
  });

  /*
   * Rendered on its own rather than through the shell, so that the screen's own
   * heading structure is the subject: a screen whose only heading was its title
   * gives a screen-reader user nothing to navigate between.
   */
  it('has a heading for each of its parts', async () => {
    stubRecorderLinkRuntime(ATTACHED, null, { sessions: () => Promise.resolve(page([])) });
    render(<LibraryScreen />);

    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Library');
    await waitFor(() => {
      expect(
        screen.getAllByRole('heading', { level: 2 }).map((heading) => heading.textContent),
      ).toEqual(['Nothing recorded yet', 'What this screen will show']);
    });
  });
});
