import { FEATURES, type LibrarySession, type LibrarySessionPage } from '@clipped/shared';
import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { StrictMode } from 'react';
import { MemoryRouter } from 'react-router';
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

/**
 * Mounts the screen by itself, inside a router.
 *
 * The router is not decoration: the Play control on every row navigates to that
 * recording's playback screen (issue #304), so a screen rendered without one is
 * a screen whose rows cannot be drawn at all.
 *
 * `link` is `null`, which is what a screen rendered outside the window has:
 * there is no recorder, so no capability, so no thumbnail is asked for. The
 * cases below are about the trash and about refusals, and a column of round
 * trips they never stubbed would only be noise (issue #448).
 */
function renderScreen(): void {
  render(
    <MemoryRouter>
      <LibraryScreen link={null} />
    </MemoryRouter>,
  );
}

/** Opens the Library screen from the sidebar. */
async function openLibrary(user: ReturnType<typeof userEvent.setup>): Promise<void> {
  await user.click(screen.getByRole('link', { name: 'Library' }));
}

/**
 * A recorder of this build, attached and idle.
 *
 * `FEATURES` rather than a list typed here, so a capability added to the
 * protocol does not quietly leave every case below testing an older recorder
 * than the one it is about — the same reason `tray_model.rs`'s own cases are
 * built from `features::ALL` (issue #447).
 */
const ATTACHED = {
  link: 'attached',
  recorder_process_id: 7,
  features: [...FEATURES],
  status: { state: 'idle' },
} as const;

/** A recorder that advertises everything this build's does, except one thing. */
function attachedWithout(missing: string) {
  return {
    ...ATTACHED,
    features: FEATURES.filter((feature) => feature !== missing),
  };
}

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

  /*
   * Issue #588's second half, where it ends up. The recorder has announced the
   * end of a sitting since issue #241 (PR #586) and `crates/ipc`'s link
   * discarded the event, so a sitting somebody had just finished playing was
   * missing from this screen until Clipped was restarted — which for a recorder
   * that outlives every window (ADR 0002) is a screen that is wrong for as long
   * as somebody keeps playing.
   *
   * Both directions, and the second is the one that matters: this screen must
   * not re-read on *any* link event. The link publishes a state whenever
   * anything about the recorder moves — a recording starting, a recording
   * ending, a reconnection — and a screen that asked the index again each time
   * would be paging back to the top of the Library while somebody was reading
   * it, which is exactly why `useFavourites` and `useLocks` do not re-read at
   * all.
   */
  it('brings the library up to date when a sitting ends, and not on any other event', async () => {
    const user = userEvent.setup();
    let recorded: readonly LibrarySession[] = [];
    const runtime = stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve(page(recorded)),
    });
    renderApp();
    await openLibrary(user);

    expect(
      await screen.findByRole('heading', { name: 'Nothing recorded yet' }),
    ).toBeInTheDocument();

    const reads = (): number =>
      runtime.invocations.filter((call) => call.command === 'library_sessions').length;
    const before = reads();

    // A state event: the recorder started recording. Something changed about
    // the recorder and nothing changed about the library. Given a turn of the
    // event loop to be acted on, so that "it did not re-read" is a reading
    // rather than a race this test happened to win.
    runtime.emit({
      event: 'state',
      ...ATTACHED,
      status: {
        state: 'recording',
        recording_id: 'r-1',
        output: 'D:\\clips\\cs2-20260811-201400-1.mkv',
        target: 'process cs2.exe',
        elapsed_ms: 4_200,
      },
    });
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(reads()).toBe(before);
    expect(screen.getByRole('heading', { name: 'Nothing recorded yet' })).toBeInTheDocument();

    // And then the sitting itself ending, with the file it produced. The index
    // has it by the time the screen asks, because the recorder asks for the
    // indexing run as it closes the sitting.
    recorded = [session()];
    runtime.emit({
      event: 'session_ended',
      session_id: 'cs2-20260811-201400',
      game_id: 'cs2',
      game_name: 'Counter-Strike 2',
      started_at: '2026-08-11T20:14:00+01:00',
      ended_at: '2026-08-11T22:03:00+01:00',
      end_reason: 'game-exited',
      recordings: [{ session_index: 1, output: 'D:\\clips\\cs2-20260811-201400-1.mkv' }],
    });

    const table = await screen.findByRole('table', { name: 'Sessions' });
    expect(within(table).getByText('Counter-Strike 2')).toBeVisible();
    expect(reads()).toBe(before + 1);
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
   * one invented row. The claim is that *these six* are what the screen still
   * owes, and only a list kept independently of the implementation can make it.
   *
   * Three rows have gone since issue #301: the session list, the search and a
   * recording whose file has gone are all on the screen, and each has a case of
   * its own above. Shrinking this list without honouring it fails here.
   *
   * The row that named thumbnails and waveforms together has become a row about
   * waveforms alone. Issue #448 built the transport and a thumbnail is drawn
   * against every recording, so naming one here would be the screen promising
   * something it already does — and #301, which that row named as the open
   * question about how bytes reach this window, is answered.
   */
  it('filters to favourites with one control, and says so in the search box', async () => {
    // SPEC.md section 17 asks for the filter; issue #695 is that marking worked
    // and nothing surfaced it. The term goes into the box as well as into the
    // query, so a person can see it, learn it and edit it into a longer search
    // -- and so the box and the list never disagree about what is shown.
    const user = userEvent.setup();
    const runtime = stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve({ sessions: [] }),
    });
    renderApp();
    await openLibrary(user);

    await user.click(await screen.findByRole('button', { name: 'Favourites only' }));

    await waitFor(() => {
      const queries = runtime.invocations
        .filter((call) => call.command === 'library_sessions')
        .map((call) => String(call.args['query'] ?? ''));
      expect(queries).toContain('favourite');
    });
    expect(screen.getByLabelText(/search your library/i)).toHaveValue('favourite');

    // And back, because a filter that cannot be cleared is a screen somebody
    // has to reload to escape.
    await user.click(screen.getByRole('button', { name: 'All sittings' }));
    expect(screen.getByLabelText(/search your library/i)).toHaveValue('');
  });

  const MUST_BE_NAMED: readonly (readonly [string, RegExp, readonly number[]])[] = [
    // Clips have left this list: a sitting lists the ones cut from it since
    // step 12 of SPEC.md section 45 was built, and a saved replay is one. What
    // is still owed is a *highlight* — something that scores a moment — which
    // is a different feature and a different issue.
    ['highlights', /^highlights a session/i, [76]],
    // Playing a *recording* is no longer on this list: Play is a control on
    // every row since issue #304, and a row promising what the screen already
    // does is worse than no row. Playing a **clip** is still waiting, and on
    // clips existing rather than on a player.
    ['playing a clip', /^playing a clip in this window/i, [91]],
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
   * Issue #399: the library could say what you had recorded and nothing could be
   * done with it. Each case below presses a control and asserts what left the
   * window, because that is the only place these can be got wrong in a way that
   * compiles — an Open that sent the destination, an Export that sent the
   * suggestion instead of what the person chose, a Show in Explorer that sent
   * `open_recording`.
   */
  it('plays a recording in this window, on the screen that draws a player', async () => {
    // Issue #304's way in. The row is handed over rather than looked up again:
    // this screen has it, and the playback screen would otherwise have to read
    // the whole library back to find one file (issue #52).
    const user = userEvent.setup();
    const runtime = stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve(page([session()])),
      openPlayback: () => ({
        url: 'http://clip.localhost/1',
        audio_track: 1,
        audio_tracks: [{ index: 1, name: 'Compatibility Mix', default: true }],
        prepared: false,
      }),
    });
    renderApp();
    await openLibrary(user);

    const play = await screen.findByRole('button', { name: /^Play / });
    await user.click(play);

    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Playback');
    const main = screen.getByRole('main');
    await waitFor(() => {
      expect(main.querySelector('video')?.getAttribute('src')).toBe('http://clip.localhost/1');
    });
    // The file the row was drawn for, and no track named: which one plays by
    // default is the recorder's answer, not this window's.
    expect(runtime.invocations).toContainEqual({
      command: 'open_playback',
      args: { source: 'D:\\clips\\cs2-20260811-201400-1.mkv', audioTrack: undefined },
    });
  });

  it('opens a recording in the system player, naming the file the row was drawn for', async () => {
    const user = userEvent.setup();
    const runtime = stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve(page([session()])),
      openRecording: () => null,
    });
    renderApp();
    await openLibrary(user);

    await user.click(
      await screen.findByRole('button', {
        name: 'Open cs2-20260811-201400-1.mkv, Counter-Strike 2',
      }),
    );

    await waitFor(() => {
      expect(
        runtime.invocations.filter((invocation) => invocation.command === 'open_recording'),
      ).toEqual([
        {
          command: 'open_recording',
          args: { path: 'D:\\clips\\cs2-20260811-201400-1.mkv' },
        },
      ]);
    });
    expect(await screen.findByRole('status')).toHaveTextContent(
      'Opened cs2-20260811-201400-1.mkv.',
    );
  });

  it('shows a recording in Explorer rather than opening it', async () => {
    const user = userEvent.setup();
    const runtime = stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve(page([session()])),
      revealRecording: () => null,
    });
    renderApp();
    await openLibrary(user);

    await user.click(
      await screen.findByRole('button', {
        name: 'Show cs2-20260811-201400-1.mkv, Counter-Strike 2 in Explorer',
      }),
    );

    await waitFor(() => {
      expect(runtime.invocations.map((invocation) => invocation.command)).toContain(
        'reveal_recording',
      );
    });
    expect(
      runtime.invocations.filter((invocation) => invocation.command === 'open_recording'),
      'showing a file in Explorer must not also launch a player',
    ).toEqual([]);
  });

  /*
   * The whole of the export path from the window's side: the dialog is opened
   * with the recording's own name suggested, and what the *person* chose is what
   * the recorder is sent. An export that sent the suggestion instead would write
   * the right file for every user who accepted the default and the wrong one for
   * everybody else, and no other test in the repository would notice.
   */
  it('exports to the file the person chose, not to the one it suggested', async () => {
    const user = userEvent.setup();
    const runtime = stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve(page([session()])),
      saveDialog: () => 'E:\\share\\ace on mirage.mp4',
      exportRecording: (args) => ({
        source: args['source'],
        destination: args['destination'],
        duration_ms: 6_540_000,
        packets: 588_120,
        bytes: 9_811_204_112,
        elapsed_ms: 4_182,
        lossless: true,
      }),
    });
    renderApp();
    await openLibrary(user);

    await user.click(
      await screen.findByRole('button', {
        name: 'Export cs2-20260811-201400-1.mkv, Counter-Strike 2 as MP4',
      }),
    );

    await waitFor(() => {
      expect(
        runtime.invocations.filter((invocation) => invocation.command === 'export_recording'),
      ).toEqual([
        {
          command: 'export_recording',
          args: {
            source: 'D:\\clips\\cs2-20260811-201400-1.mkv',
            destination: 'E:\\share\\ace on mirage.mp4',
          },
        },
      ]);
    });

    const dialog = runtime.invocations.find(
      (invocation) => invocation.command === 'plugin:dialog|save',
    );
    expect(
      JSON.stringify(dialog?.args),
      'the dialog opens on the recording, under the recording’s own name',
    ).toContain('D:\\\\clips\\\\cs2-20260811-201400-1.mp4');

    expect(await screen.findByRole('status')).toHaveTextContent(
      /Exported ace on mirage\.mp4 .* 9\.1 GB copied in 4\.2 s, without re-encoding\./,
    );
  });

  /*
   * Issue #446, from the window's side. A copy of a four-second recording
   * finishes before there is anything to draw; a copy of a two-hour one is what
   * this is for, and the only thing that reaches the window while the control
   * connection is blocked on the reply is an event.
   *
   * The export here never resolves until the test says so, which is what a long
   * one looks like from here.
   */
  it('shows how far a long export has got, and stops showing it when the file is written', async () => {
    const user = userEvent.setup();
    let finish: (summary: unknown) => void = () => undefined;
    const written = new Promise((resolve) => {
      finish = resolve;
    });

    const runtime = stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve(page([session()])),
      saveDialog: () => 'E:\\share\\ace on mirage.mp4',
      exportRecording: () => written,
    });
    renderApp();
    await openLibrary(user);

    await user.click(
      await screen.findByRole('button', {
        name: 'Export cs2-20260811-201400-1.mkv, Counter-Strike 2 as MP4',
      }),
    );

    // Before the recorder has said anything. The screen says an export is
    // running — which it can honestly support against any recorder — and draws
    // no bar, because a bar at nought that has never moved is a control that
    // does nothing (AGENTS.md section 27). This is also exactly what an older
    // recorder, which sends no progress at all, looks like for the whole copy.
    await waitFor(() => {
      expect(screen.getByRole('status')).toHaveTextContent(/Exporting cs2-20260811-201400-1\.mkv…/);
    });
    expect(
      screen.queryByRole('meter'),
      'a bar was drawn before the recorder had said how far it had got',
    ).toBeNull();

    const progress = (written_ms: number, packets: number, bytes: number): unknown => ({
      event: 'export_progress',
      source: 'D:\\clips\\cs2-20260811-201400-1.mkv',
      destination: 'E:\\share\\ace on mirage.mp4',
      written_ms,
      total_ms: 6_540_000,
      packets,
      bytes,
    });

    runtime.emit(progress(1_308_000, 117_624, 1_962_240_822));
    await waitFor(() => {
      expect(screen.getByRole('status')).toHaveTextContent(
        /Exporting cs2-20260811-201400-1\.mkv — 20%/,
      );
    });
    expect(Number(screen.getByRole('meter').getAttribute('value'))).toBeCloseTo(0.2, 2);

    // And it advances. A screen that drew the first event and ignored the rest
    // would satisfy every assertion above.
    runtime.emit(progress(4_905_000, 441_090, 7_358_403_084));
    await waitFor(() => {
      expect(screen.getByRole('status')).toHaveTextContent(
        /Exporting cs2-20260811-201400-1\.mkv — 75%/,
      );
    });
    expect(Number(screen.getByRole('meter').getAttribute('value'))).toBeCloseTo(0.75, 2);

    // The reply is the only thing that says an export finished, and when it
    // lands the bar goes with the sentence it replaced.
    finish({
      source: 'D:\\clips\\cs2-20260811-201400-1.mkv',
      destination: 'E:\\share\\ace on mirage.mp4',
      duration_ms: 6_540_000,
      packets: 588_120,
      bytes: 9_811_204_112,
      elapsed_ms: 4_182,
      lossless: true,
    });

    await waitFor(() => {
      expect(screen.getByRole('status')).toHaveTextContent(
        /Exported ace on mirage\.mp4 .* 9\.1 GB copied/,
      );
    });
    expect(screen.queryByRole('meter'), 'the bar outlived the export it was drawn for').toBeNull();
  });

  /*
   * An interrupted recording keeps every packet it wrote and no total, which is
   * the property ADR 0001 chose Matroska for. There is no percentage to draw for
   * one, and a denominator invented in the window would be a bar that lied — so
   * what it shows is what advances.
   */
  it('says how much has been copied when the recording never said how long it was', async () => {
    const user = userEvent.setup();
    const runtime = stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve(page([session()])),
      saveDialog: () => 'E:\\share\\ace on mirage.mp4',
      exportRecording: () => new Promise(() => undefined),
    });
    renderApp();
    await openLibrary(user);

    await user.click(
      await screen.findByRole('button', {
        name: 'Export cs2-20260811-201400-1.mkv, Counter-Strike 2 as MP4',
      }),
    );

    runtime.emit({
      event: 'export_progress',
      source: 'D:\\clips\\cs2-20260811-201400-1.mkv',
      destination: 'E:\\share\\ace on mirage.mp4',
      written_ms: 1_308_000,
      packets: 117_624,
      bytes: 1_962_240_822,
    });

    await waitFor(() => {
      expect(screen.getByRole('status')).toHaveTextContent(/1\.8 GB copied so far/);
    });
    // A meter with no value is the element's own way of saying "something is
    // happening and I cannot say how much", which is the honest drawing here. A
    // value of nought would be a claim.
    expect(screen.getByRole('meter').getAttribute('value')).toBeNull();
  });

  /*
   * Progress for somebody else's export must not move this one's bar. The
   * events name the file they belong to precisely so that this is answerable,
   * and matching on nothing would make a second window's copy of a different
   * recording repaint this screen.
   */
  it('ignores progress for a recording it is not exporting', async () => {
    const user = userEvent.setup();
    const runtime = stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve(page([session()])),
      saveDialog: () => 'E:\\share\\ace on mirage.mp4',
      exportRecording: () => new Promise(() => undefined),
    });
    renderApp();
    await openLibrary(user);

    await user.click(
      await screen.findByRole('button', {
        name: 'Export cs2-20260811-201400-1.mkv, Counter-Strike 2 as MP4',
      }),
    );
    await waitFor(() => {
      expect(screen.getByRole('status')).toHaveTextContent(/Exporting/);
    });

    runtime.emit({
      event: 'export_progress',
      source: 'D:\\clips\\dota2-20260810-190000-1.mkv',
      destination: 'E:\\share\\somebody else.mp4',
      written_ms: 3_270_000,
      total_ms: 6_540_000,
      packets: 294_060,
      bytes: 4_905_602_056,
    });

    // Nothing to wait for, which is the difficulty: give the subscription the
    // same turn of the loop the accepted events above got, then assert nothing
    // moved.
    await waitFor(() => {
      expect(screen.getByRole('status')).toHaveTextContent(/Exporting cs2-20260811-201400-1\.mkv…/);
    });
    expect(
      screen.queryByRole('meter'),
      'a bar was drawn from another recording’s export',
    ).toBeNull();
  });

  it('says nothing at all when the Save As dialog is dismissed', async () => {
    // Dismissing a dialog is somebody changing their mind, not a failure, and a
    // screen that reported it would be noise (AGENTS.md section 28). The
    // assertion that matters is that no export was sent.
    const user = userEvent.setup();
    const runtime = stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve(page([session()])),
      saveDialog: () => null,
    });
    renderApp();
    await openLibrary(user);

    await user.click(
      await screen.findByRole('button', {
        name: 'Export cs2-20260811-201400-1.mkv, Counter-Strike 2 as MP4',
      }),
    );

    await waitFor(() => {
      expect(runtime.invocations.map((invocation) => invocation.command)).toContain(
        'plugin:dialog|save',
      );
    });
    expect(runtime.invocations.map((invocation) => invocation.command)).not.toContain(
      'export_recording',
    );
    expect(screen.getByRole('status')).toBeEmptyDOMElement();
  });

  /*
   * Issue #399's fifth and sixth acceptance criteria on the screen. Both are
   * about *wording*: the heading has to say the name is taken rather than that
   * the export failed, and the sentence has to be the recorder's own — a screen
   * that wrote its own would leave somebody unable to tell a name that is taken
   * from a recording MP4 cannot hold.
   */
  it('says a destination that is taken is taken, and that nothing was written over', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve(page([session()])),
      saveDialog: () => 'E:\\share\\ace on mirage.mp4',
      exportRecording: () => {
        throw {
          code: 'destination_exists',
          message:
            'there is already a file at ace on mirage.mp4, and Clipped does not overwrite one; choose another name',
        };
      },
    });
    renderApp();
    await openLibrary(user);

    await user.click(
      await screen.findByRole('button', {
        name: 'Export cs2-20260811-201400-1.mkv, Counter-Strike 2 as MP4',
      }),
    );

    const status = await screen.findByRole('status');
    await waitFor(() => {
      expect(status).toHaveTextContent('That name is already taken');
    });
    expect(status).toHaveTextContent('choose another name');
    expect(status).toHaveTextContent('nothing was changed');
  });

  it('reports a refusal from the muxer in the muxer’s own wording', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve(page([session()])),
      saveDialog: () => 'E:\\share\\ace on mirage.mp4',
      exportRecording: () => {
        throw {
          code: 'export_failed',
          message:
            'match.mkv cannot be remuxed to MP4 without losing part of the recording: audio track 1 (wavpack). Nothing was written; the recording is unchanged and still playable as it is',
        };
      },
    });
    renderApp();
    await openLibrary(user);

    await user.click(
      await screen.findByRole('button', {
        name: 'Export cs2-20260811-201400-1.mkv, Counter-Strike 2 as MP4',
      }),
    );

    expect(await screen.findByRole('status')).toHaveTextContent(
      'audio track 1 (wavpack). Nothing was written',
    );
  });

  /*
   * A recording whose file has gone can have none of the three done to it, and
   * a control that would fail must not be offered as one that would work
   * (AGENTS.md section 27). Disabled and saying why, rather than hidden, because
   * a row with nothing on it explains nothing.
   */
  it('offers none of the three against a recording whose file has gone', async () => {
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

    for (const name of [
      'Open gone.mkv, Counter-Strike 2',
      'Show gone.mkv, Counter-Strike 2 in Explorer',
      'Export gone.mkv, Counter-Strike 2 as MP4',
    ]) {
      expect(await screen.findByRole('button', { name })).toBeDisabled();
    }
    expect(screen.getByRole('table', { name: 'Sessions' })).toHaveTextContent('file missing');
  });

  /*
   * Issue #447's first acceptance criterion, and the reason `features` is on the
   * link at all.
   *
   * The window can be newer than the recorder it finds: an installed Clipped
   * attaches to whatever is already listening, and a recorder built before
   * issue #399 has no `export_recording` command. Drawn as a working control,
   * the refusal arrives *after* the Save As dialog — so the person has named a
   * file that was never going to be written, which is the one thing AGENTS.md
   * section 27 forbids.
   *
   * The other two controls staying live is half the point: they are shell calls
   * this window's own host makes and have nothing to do with the recorder's
   * age, so a check that disabled the row would be as wrong as no check at all.
   */
  it('does not offer an Export control against a recorder that never said it could export', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime(attachedWithout('export'), null, {
      sessions: () => Promise.resolve(page([session()])),
    });
    renderApp();
    await openLibrary(user);

    const exportControl = await screen.findByRole('button', {
      name: /^Export cs2-20260811-201400-1\.mkv/,
    });
    expect(exportControl).toBeDisabled();
    // Said, not hidden, and said where a screen reader will hear it: `title` is
    // not announced, and `aria-label` replaces the text rather than adding to
    // it (AGENTS.md sections 27, 45 and 46).
    expect(exportControl).toHaveAccessibleName(/older than this window and cannot export/);
    expect(exportControl).toHaveTextContent('Export MP4 — this recorder cannot export');

    expect(
      screen.getByRole('button', { name: 'Open cs2-20260811-201400-1.mkv, Counter-Strike 2' }),
      'opening a file is the window host’s own shell call and has nothing to do with the recorder',
    ).toBeEnabled();
    expect(
      screen.getByRole('button', {
        name: 'Show cs2-20260811-201400-1.mkv, Counter-Strike 2 in Explorer',
      }),
    ).toBeEnabled();
  });

  /*
   * The other direction, which is what stops the check above from being written
   * as "never offer an Export control". A recorder of this build advertises
   * `export`, and the control is the one every case above presses.
   */
  it('offers it against a recorder whose welcome named export', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve(page([session()])),
    });
    renderApp();
    await openLibrary(user);

    const exportControl = await screen.findByRole('button', {
      name: 'Export cs2-20260811-201400-1.mkv, Counter-Strike 2 as MP4',
    });
    expect(exportControl).toBeEnabled();
    expect(exportControl).toHaveTextContent('Export MP4');
  });

  /*
   * Issue #447's third question: what a window shows while it is connecting.
   *
   * Features are not known until a recorder answers, so "this recorder cannot
   * export" is a claim about a recorder nobody has spoken to — and one that
   * would turn into a working control a second later. The control waits and
   * says it is waiting. This case fails if `connecting` is folded in with "the
   * recorder said no", which is the easy way to write the check and the wrong
   * one.
   */
  it('says it is waiting rather than claiming a recorder it has not reached cannot export', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime({ link: 'connecting' }, null, {
      sessions: () => Promise.resolve(page([session()])),
    });
    renderApp();
    await openLibrary(user);

    const exportControl = await screen.findByRole('button', {
      name: /^Export cs2-20260811-201400-1\.mkv/,
    });
    expect(exportControl).toBeDisabled();
    expect(exportControl).toHaveAccessibleName(/still looking for the recorder/);
    expect(
      exportControl,
      'a recorder that has not answered has not refused anything',
    ).not.toHaveAccessibleName(/older than this window/);
    expect(exportControl).toHaveTextContent('Export MP4 — waiting for the recorder');
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
  /** A trash with nothing in it, which is what most machines have. */
  function emptyTrash() {
    return { items: [], total_items: 0, total_bytes: 0, directory: 'D:/Clips.trash' };
  }

  function fullTrash() {
    return {
      items: [
        {
          kind: 'recording',
          id: 1,
          path: 'D:/Clips.trash/clipped-cs2.mkv',
          original_path: 'D:/Clips/clipped-cs2.mkv',
          deleted_at: '2026-08-15T09:00:00+01:00',
          size_bytes: 2_000_000_000,
          dependent_clips: 2,
        },
      ],
      total_items: 1,
      total_bytes: 2_000_000_000,
      directory: 'D:/Clips.trash',
    };
  }

  /**
   * A trash holding one thing that has no file at all.
   *
   * `path` and `original_path` are absent rather than blank, which is what the
   * recorder sends for a clip nothing has exported
   * ([issue #593](https://github.com/wildware-uk/clipped/issues/593)).
   */
  function trashHoldingAClipWithNoFile() {
    return {
      items: [
        {
          kind: 'clip',
          id: 7,
          deleted_at: '2026-08-16T09:00:00+01:00',
          dependent_clips: 0,
        },
      ],
      total_items: 1,
      total_bytes: 0,
      directory: 'D:/Clips.trash',
    };
  }

  it('has a heading for each of its parts', async () => {
    stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve(page([])),
      trash: () => Promise.resolve(emptyTrash()),
    });
    renderScreen();

    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Library');
    await waitFor(() => {
      expect(
        screen.getAllByRole('heading', { level: 2 }).map((heading) => heading.textContent),
      ).toEqual(['Nothing recorded yet', 'Trash', 'What this screen will show']);
    });
  });

  it('puts one thing back, and says where it went', async () => {
    // The second acceptance criterion. Restoring is the whole reason a trash
    // exists, and until this it could only be done from `cargo`.
    const user = userEvent.setup();
    const restored = vi.fn(() =>
      Promise.resolve({
        kind: 'recording',
        id: 1,
        path: 'D:/Clips/clipped-cs2.mkv',
        file_restored: true,
        renamed: false,
      }),
    );
    stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve(page([])),
      trash: () => Promise.resolve(fullTrash()),
      restoreFromTrash: restored,
    });
    renderScreen();

    await user.click(await screen.findByRole('button', { name: 'Restore' }));

    expect(restored).toHaveBeenCalledWith({ kind: 'recording', id: 1 });
    expect(await screen.findByRole('status')).toHaveTextContent(
      'Put D:/Clips/clipped-cs2.mkv back',
    );
  });

  it('says a restored recording whose file had already gone will show as missing', async () => {
    // Not a failure: the row comes back and reports itself missing, which is
    // the truth rather than a row with no explanation.
    const user = userEvent.setup();
    stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve(page([])),
      trash: () => Promise.resolve(fullTrash()),
      restoreFromTrash: () =>
        Promise.resolve({
          kind: 'recording',
          id: 1,
          path: 'D:/Clips/clipped-cs2.mkv',
          file_restored: false,
          renamed: false,
        }),
    });
    renderScreen();

    await user.click(await screen.findByRole('button', { name: 'Restore' }));

    expect(await screen.findByRole('status')).toHaveTextContent('will show as missing');
  });

  it('shows a deleted clip that has no file, and names it by what it is', async () => {
    // Issue #593. A generated highlight is a range of a recording until
    // somebody exports it, so a deleted one has no file to be in the trash and
    // nowhere to be put back to. It is still something the user deleted, so it
    // is on the screen — an empty cell would be indistinguishable from a value
    // the window failed to read.
    stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve(page([])),
      trash: () => Promise.resolve(trashHoldingAClipWithNoFile()),
    });
    renderScreen();

    expect(await screen.findByText(/No file/)).toBeInTheDocument();
    // Absent, not blank: an empty cell is indistinguishable from a value the
    // window failed to read, and the item is still restorable.
    expect(screen.queryByText('undefined')).not.toBeInTheDocument();
    expect(await screen.findByRole('button', { name: 'Restore' })).toBeInTheDocument();
  });

  it('does not tell somebody a clip that never had a file has gone missing', async () => {
    // The wrong sentence is the one meant for an item whose media had already
    // gone. Nothing has gone here, and nothing will show as missing.
    const user = userEvent.setup();
    stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve(page([])),
      trash: () => Promise.resolve(trashHoldingAClipWithNoFile()),
      restoreFromTrash: () =>
        Promise.resolve({ kind: 'clip', id: 7, file_restored: false, renamed: false }),
    });
    renderScreen();

    await user.click(await screen.findByRole('button', { name: 'Restore' }));

    const said = (await screen.findByRole('status')).textContent ?? '';
    expect(said).toContain('no file');
    expect(said).not.toContain('missing');
    expect(said).not.toContain('undefined');
  });

  it('asks before emptying the trash, and sends the counts it showed', async () => {
    // The third acceptance criterion, both halves: emptying takes two presses,
    // and what it confirms is the listing the user was looking at.
    const user = userEvent.setup();
    const emptied = vi.fn(() =>
      Promise.resolve({ removed: 1, reclaimed_bytes: 2_000_000_000, refused: [] }),
    );
    stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve(page([])),
      trash: () => Promise.resolve(fullTrash()),
      emptyTrash: emptied,
    });
    renderScreen();

    await user.click(await screen.findByRole('button', { name: 'Empty the trash…' }));
    expect(emptied).not.toHaveBeenCalled();
    expect(screen.getByRole('region', { name: 'Trash' })).toHaveTextContent('cannot be undone');

    await user.click(screen.getByRole('button', { name: 'Empty the trash' }));

    expect(emptied).toHaveBeenCalledWith({ items: 1, bytes: 2_000_000_000 });
    expect(await screen.findByRole('status')).toHaveTextContent('Removed 1 thing(s)');
  });

  it('keeps them when the confirmation is declined', async () => {
    const user = userEvent.setup();
    const emptied = vi.fn(() => Promise.resolve({ removed: 0, reclaimed_bytes: 0, refused: [] }));
    stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve(page([])),
      trash: () => Promise.resolve(fullTrash()),
      emptyTrash: emptied,
    });
    renderScreen();

    await user.click(await screen.findByRole('button', { name: 'Empty the trash…' }));
    await user.click(screen.getByRole('button', { name: 'Keep them' }));

    expect(emptied).not.toHaveBeenCalled();
    expect(screen.getByRole('button', { name: 'Empty the trash…' })).toBeInTheDocument();
  });

  it('says when a trash that changed underneath was refused rather than emptied', async () => {
    // The property `EmptyTrash::for_listing` exists for, surviving the round
    // trip: a trash that gained something between the listing and the button is
    // refused, and the user is told what changed rather than losing something
    // they never saw.
    const user = userEvent.setup();
    stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve(page([])),
      trash: () => Promise.resolve(fullTrash()),
      emptyTrash: () =>
        Promise.reject(
          Object.assign(new Error('refused'), {
            code: 'invalid_parameters',
            message: 'the trash holds 2 things (3.0 GB) and 1 (2.0 GB) was confirmed',
          }),
        ),
    });
    renderScreen();

    await user.click(await screen.findByRole('button', { name: 'Empty the trash…' }));
    await user.click(screen.getByRole('button', { name: 'Empty the trash' }));

    expect(await screen.findByRole('status')).toHaveTextContent('the trash holds 2 things');
  });

  it('says what is in the trash, and where the files went', async () => {
    // The half of issue #450 that is a read. A user who deleted something can
    // see that it is recoverable and where it is, which is what a trash is for.
    stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve(page([])),
      trash: () =>
        Promise.resolve({
          items: [
            {
              kind: 'recording',
              id: 1,
              path: 'D:/Clips.trash/clipped-cs2.mkv',
              original_path: 'D:/Clips/clipped-cs2.mkv',
              deleted_at: '2026-08-15T09:00:00+01:00',
              size_bytes: 2_000_000_000,
              dependent_clips: 2,
            },
          ],
          total_items: 1,
          total_bytes: 2_000_000_000,
          directory: 'D:/Clips.trash',
        }),
    });
    renderScreen();

    const trash = await screen.findByRole('region', { name: 'Trash' });
    // The path it *had*: a file inside the trash is named for the trash, and
    // nobody recognises their own recording by a name they have never seen.
    expect(trash).toHaveTextContent('D:/Clips/clipped-cs2.mkv');
    expect(trash).toHaveTextContent('2.0 GB');
    expect(trash).toHaveTextContent('2026-08-15');
    // The clips cut from it stop being recoverable when the trash is emptied,
    // so the screen says so before anybody does that.
    expect(trash).toHaveTextContent('2 clip(s) were cut from this');
  });

  it('tells an empty trash from a trash it could not read', async () => {
    stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve(page([])),
      trash: () => Promise.resolve(emptyTrash()),
    });
    renderScreen();

    const trash = await screen.findByRole('region', { name: 'Trash' });
    expect(trash).toHaveTextContent('Nothing has been deleted');
    expect(
      screen.queryByRole('heading', { name: 'The trash could not be read' }),
    ).not.toBeInTheDocument();
  });

  it('says when the trash could not be read, rather than showing it as empty', async () => {
    // A trash on a drive that is not there, or a recorder too old to have the
    // command. Either way "nothing has been deleted" would be a claim nobody
    // measured (issue #450's fourth acceptance criterion).
    stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve(page([])),
      trash: () =>
        Promise.reject(
          Object.assign(new Error('the trash could not be read'), {
            code: 'library_unavailable',
            message: 'the trash could not be read: the drive is not there',
          }),
        ),
    });
    renderScreen();

    expect(
      await screen.findByRole('heading', { name: 'The trash could not be read' }),
    ).toBeInTheDocument();
    expect(screen.queryByText('Nothing has been deleted.')).not.toBeInTheDocument();
  });

  /*
   * Favouriting (issue #58, SPEC.md section 29).
   *
   * The read has always carried `favourite` and automatic cleanup has always
   * protected what is marked. What did not exist was any way to *set* one, so
   * these are the tests that the star reaches the recorder rather than filling
   * itself in: the runtime rejects `set_favourite` unless a test stubs it, so a
   * screen that drew a star and asked nobody fails here rather than passing.
   */
  it('marks a sitting as one to keep, and says so as a pressed control', async () => {
    const user = userEvent.setup();
    const runtime = stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve(page([session()])),
      setFavourite: (args) =>
        Promise.resolve({
          kind: args['kind'],
          session_id: args['sessionId'],
          id: args['id'],
          favourite: args['favourite'],
          changed: true,
        }),
    });
    renderApp();
    await openLibrary(user);

    const star = await screen.findByRole('button', {
      name: /^Keep the Counter-Strike 2 sitting from /,
    });
    expect(star).toHaveAttribute('aria-pressed', 'false');
    await user.click(star);

    await waitFor(() => {
      expect(
        runtime.invocations.filter((invocation) => invocation.command === 'set_favourite'),
      ).toEqual([
        {
          command: 'set_favourite',
          args: {
            kind: 'session',
            sessionId: 'cs2-20260811-201400',
            id: 0,
            favourite: true,
          },
        },
      ]);
    });

    expect(
      await screen.findByRole('button', {
        name: /^Stop keeping the Counter-Strike 2 sitting from /,
      }),
    ).toHaveAttribute('aria-pressed', 'true');
  });

  it('addresses a recording by its identifier and a sitting by its name', async () => {
    // The one thing about this command that a careless wiring gets wrong: the
    // two kinds are addressed by different fields, because the schema keys them
    // differently. A recording sent as a `session_id` names nothing.
    const user = userEvent.setup();
    const runtime = stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve(page([session()])),
      setFavourite: (args) =>
        Promise.resolve({
          kind: args['kind'],
          session_id: args['sessionId'],
          id: args['id'],
          favourite: true,
          changed: true,
        }),
    });
    renderApp();
    await openLibrary(user);

    await user.click(
      await screen.findByRole('button', {
        name: 'Keep cs2-20260811-201400-1.mkv, Counter-Strike 2',
      }),
    );

    await waitFor(() => {
      expect(
        runtime.invocations.filter((invocation) => invocation.command === 'set_favourite'),
      ).toEqual([
        {
          command: 'set_favourite',
          args: { kind: 'recording', sessionId: '', id: 12, favourite: true },
        },
      ]);
    });
  });

  it('clears a mark that is already on, rather than setting it again', async () => {
    const user = userEvent.setup();
    const runtime = stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve(page([session({ favourite: true })])),
      setFavourite: (args) =>
        Promise.resolve({
          kind: args['kind'],
          session_id: args['sessionId'],
          id: args['id'],
          favourite: args['favourite'],
          changed: true,
        }),
    });
    renderApp();
    await openLibrary(user);

    await user.click(
      await screen.findByRole('button', {
        name: /^Stop keeping the Counter-Strike 2 sitting from /,
      }),
    );

    await waitFor(() => {
      expect(
        runtime.invocations
          .filter((invocation) => invocation.command === 'set_favourite')
          .map((invocation) => invocation.args['favourite']),
      ).toEqual([false]);
    });
  });

  it('draws the mark the library holds rather than the one that was asked for', async () => {
    // A row that has gone is written to by nothing. The recorder reads the mark
    // back after the write and answers with what is true, and a star that
    // filled in anyway would be the window disagreeing with its own next read
    // (AGENTS.md section 27).
    const user = userEvent.setup();
    stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve(page([session()])),
      setFavourite: (args) =>
        Promise.resolve({
          kind: args['kind'],
          session_id: args['sessionId'],
          id: args['id'],
          favourite: false,
          changed: false,
        }),
    });
    renderApp();
    await openLibrary(user);

    const star = await screen.findByRole('button', {
      name: /^Keep the Counter-Strike 2 sitting from /,
    });
    await user.click(star);

    await waitFor(() => {
      expect(star).toHaveAttribute('aria-pressed', 'false');
    });
  });

  it('says a mark that would not go on did not, rather than drawing it anyway', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve(page([session()])),
      setFavourite: () =>
        Promise.reject(
          Object.assign(new Error('the library could not be written'), {
            code: 'library_unavailable',
            message: 'the recording library could not be opened: the drive is not there',
          }),
        ),
    });
    renderApp();
    await openLibrary(user);

    const star = await screen.findByRole('button', {
      name: /^Keep the Counter-Strike 2 sitting from /,
    });
    await user.click(star);

    expect(await screen.findByRole('status')).toHaveTextContent(
      'That could not be kept. the recording library could not be opened: the drive is not there Nothing was changed.',
    );
    expect(star).toHaveAttribute('aria-pressed', 'false');
  });

  /*
   * Keeping a recording out of automatic cleanup's reach (issue #472).
   *
   * The column existed nowhere before this: `cleanup::Protection` named locked
   * recordings among the things it would not take, and there was no lock. These
   * are the tests that the padlock reaches the recorder, and that it tells
   * "you locked this" from "cleanup will not take this" — which are different
   * for every recording inside a locked sitting.
   */
  it('protects a recording from automatic cleanup, and says so as a pressed control', async () => {
    const user = userEvent.setup();
    const runtime = stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve(page([session()])),
      setLock: (args) =>
        Promise.resolve({
          kind: args['kind'],
          session_id: args['sessionId'],
          id: args['id'],
          locked: args['locked'],
          protected: args['locked'],
          changed: true,
        }),
    });
    renderApp();
    await openLibrary(user);

    const padlock = await screen.findByRole('button', {
      name: 'Protect cs2-20260811-201400-1.mkv, Counter-Strike 2 from automatic cleanup',
    });
    expect(padlock).toHaveAttribute('aria-pressed', 'false');
    await user.click(padlock);

    await waitFor(() => {
      expect(runtime.invocations.filter((invocation) => invocation.command === 'set_lock')).toEqual(
        [{ command: 'set_lock', args: { kind: 'recording', sessionId: '', id: 12, locked: true } }],
      );
    });

    expect(
      await screen.findByRole('button', {
        name: 'Stop protecting cs2-20260811-201400-1.mkv, Counter-Strike 2 from automatic cleanup',
      }),
    ).toHaveAttribute('aria-pressed', 'true');
  });

  it('says a recording inside a protected sitting is protected by it, and offers no control', async () => {
    // The case that separates `locked` from `protected`. A padlock drawn from
    // `locked` alone would show this recording as one cleanup may take, and
    // cleanup would not take it. A *control* drawn from `protected` would offer
    // to unlock something that has no lock to release.
    const user = userEvent.setup();
    stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () =>
        Promise.resolve(
          page([
            {
              ...session(),
              locked: true,
              recordings: [{ ...session().recordings[0]!, locked: false, protected: true }],
            },
          ]),
        ),
    });
    renderApp();
    await openLibrary(user);

    const bySitting = await screen.findByRole('button', {
      name: 'cs2-20260811-201400-1.mkv, Counter-Strike 2 is protected from automatic cleanup because its sitting is',
    });
    expect(bySitting).toBeDisabled();
    expect(
      screen.queryByRole('button', {
        name: 'Stop protecting cs2-20260811-201400-1.mkv, Counter-Strike 2 from automatic cleanup',
      }),
    ).not.toBeInTheDocument();
  });

  it('draws the lock the library holds rather than the one that was asked for', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve(page([session()])),
      setLock: (args) =>
        Promise.resolve({
          kind: args['kind'],
          session_id: args['sessionId'],
          id: args['id'],
          locked: false,
          protected: false,
          changed: false,
        }),
    });
    renderApp();
    await openLibrary(user);

    const padlock = await screen.findByRole('button', {
      name: 'Protect cs2-20260811-201400-1.mkv, Counter-Strike 2 from automatic cleanup',
    });
    await user.click(padlock);

    await waitFor(() => {
      expect(padlock).toHaveAttribute('aria-pressed', 'false');
    });
  });

  it('says a lock that would not go on did not, rather than drawing it anyway', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve(page([session()])),
      setLock: () =>
        Promise.reject(
          Object.assign(new Error('the library could not be written'), {
            code: 'library_unavailable',
            message: 'the recording library could not be opened: the drive is not there',
          }),
        ),
    });
    renderApp();
    await openLibrary(user);

    const padlock = await screen.findByRole('button', {
      name: 'Protect cs2-20260811-201400-1.mkv, Counter-Strike 2 from automatic cleanup',
    });
    await user.click(padlock);

    expect(await screen.findByRole('status')).toHaveTextContent(
      'That could not be kept from cleanup. the recording library could not be opened: the drive is not there Nothing was changed.',
    );
    expect(padlock).toHaveAttribute('aria-pressed', 'false');
  });
});
