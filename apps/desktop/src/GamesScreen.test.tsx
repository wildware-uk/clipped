import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { StrictMode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { App } from './App';
import { describeGameDetection } from './gameDetection';
import { GamesScreen } from './GamesScreen';
import { stubRecorderLinkRuntime } from './test/recorderLinkRuntime';
import type { RecorderLinkState } from './useRecorderLink';

/**
 * The Games screen's contract, as tests (issue #107).
 *
 * The two properties worth guarding here are the two that would rot silently.
 *
 * The first is that the screen shows the recorder's real state rather than a
 * sentence somebody typed. A screen whose wording is a constant looks identical
 * to one that is following the link, and stays identical after the link has
 * been disconnected from it — which is why the case below drives the whole
 * application and moves the link underneath it rather than rendering
 * `describeGameDetection`'s output next to itself.
 *
 * The second is that the screen offers nothing that does nothing. The deck
 * draws an Add Game button and a table of games; neither can be honoured in
 * this build (AGENTS.md section 27), so neither is drawn, and the assertions
 * are about the absence rather than about the presence of an explanation for
 * it.
 */

/** Mounts the application the way `main.tsx` does, StrictMode and all. */
function renderApp(): void {
  render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}

/** Opens the Games screen from the sidebar. */
async function openGames(user: ReturnType<typeof userEvent.setup>): Promise<void> {
  await user.click(screen.getByRole('link', { name: 'Games' }));
}

/** The live region the detection state is drawn in. */
function detectionPanel(): HTMLElement {
  return screen.getByRole('region', { name: 'Game detection' });
}

describe('what the Games screen says about detection', () => {
  /*
   * Every state the link has, and what each one means for game detection. The
   * five are listed rather than generated: the failure this guards against is
   * two states collapsing into one wording, and a table built from the states
   * themselves could not see that happen.
   */
  const STATES: readonly (readonly [string, RecorderLinkState | null, string, RegExp])[] = [
    ['outside the Clipped window', null, 'Not known', /not the Clipped window.*no recorder to ask/],
    ['while the link is being made', { link: 'connecting' }, 'Not known yet', /Looking for/],
    [
      'while the link is being remade',
      {
        link: 'reconnecting',
        attempt: 2,
        attempts_allowed: 4,
        delay_ms: 500,
        reason: 'The pipe closed.',
      },
      'Not known',
      /The pipe closed\. Attempt 2 of 4\./,
    ],
    [
      'when there is no recorder',
      { link: 'unavailable', reason: 'clipped-recorder.exe is not beside this application.' },
      'Not detecting games',
      /no recorder to detect them.*not beside this application/,
    ],
    [
      'when a recorder is attached',
      { link: 'attached', recorder_process_id: 7, status: { state: 'idle' } },
      'Not detecting games',
      /clipped-recorder serve.*clipped-recorder watch/,
    ],
  ];

  it.each(STATES)('is described %s', (_case, link, state, detail) => {
    const described = describeGameDetection(link);

    expect(described.state).toBe(state);
    expect(described.detail).toMatch(detail);
  });

  /*
   * The reason a recorder could not be reached is the only part of that state a
   * user can act on, and it is the recorder's own words. A rendering that
   * dropped it would leave "Not detecting games" and nothing to do about it
   * (AGENTS.md section 45).
   */
  it('carries the recorder link its own reason rather than a generic sentence', () => {
    const described = describeGameDetection({
      link: 'unavailable',
      reason: 'The endpoint could not be named: access denied.',
    });

    expect(described.detail).toContain('The endpoint could not be named: access denied.');
  });

  /*
   * "Not detecting games" and "not known" are different answers and are drawn
   * differently: a link that has not settled has not established anything, and
   * saying that detection is off would be a claim nobody measured.
   */
  it('does not claim detection is off while the link has not settled', () => {
    for (const link of [null, { link: 'connecting' } as const]) {
      expect(describeGameDetection(link).state).not.toBe('Not detecting games');
    }
  });
});

describe('the Games screen', () => {
  beforeEach(() => {
    window.location.hash = '';
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    window.location.hash = '';
  });

  it('follows the recorder link rather than showing a sentence that was typed once', async () => {
    const user = userEvent.setup();
    const runtime = stubRecorderLinkRuntime({ link: 'connecting' });
    renderApp();
    await openGames(user);

    await waitFor(() => {
      expect(within(detectionPanel()).getByRole('heading', { level: 2 })).toHaveTextContent(
        'Not known yet',
      );
    });

    runtime.emit({
      event: 'state',
      link: 'unavailable',
      reason: 'clipped-recorder.exe is not beside this application.',
    });

    await waitFor(() => {
      expect(within(detectionPanel()).getByRole('heading', { level: 2 })).toHaveTextContent(
        'Not detecting games',
      );
    });
    expect(
      within(detectionPanel()).getByText(/clipped-recorder\.exe is not beside this application\./),
    ).toBeVisible();
  });

  it('announces a change of state rather than only drawing it', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime({ link: 'connecting' });
    renderApp();
    await openGames(user);

    expect(detectionPanel()).toHaveAttribute('aria-live', 'polite');
  });

  /*
   * The deck draws an Add Game button, a row menu and an Edit Recording
   * Settings button against every game. None of them has anything behind it in
   * this build - the catalogue cannot be read or written from this window
   * (issue #245) - so none is drawn. This asserts the property rather than the
   * three cases: anything operable inside the screen would have to do
   * something, and nothing here can.
   */
  it('offers no control, because nothing it would drive can be reached', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime({
      link: 'attached',
      recorder_process_id: 7,
      status: { state: 'idle' },
    });
    renderApp();
    await openGames(user);

    const main = screen.getByRole('main');
    expect(within(main).queryAllByRole('button')).toHaveLength(0);
    expect(within(main).queryAllByRole('link')).toHaveLength(0);
    expect(within(main).queryAllByRole('textbox')).toHaveLength(0);
    expect(within(main).queryAllByRole('combobox')).toHaveLength(0);
    expect(within(main).queryAllByRole('checkbox')).toHaveLength(0);
    expect(within(main).queryAllByRole('radio')).toHaveLength(0);
  });

  /*
   * The screen's one table is what it cannot show and why, not a list of games.
   * An empty Game / Recording / Last played table is indistinguishable from a
   * machine that has played nothing, and this build has not looked.
   */
  it('draws a table of what is missing rather than an empty table of games', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime({
      link: 'attached',
      recorder_process_id: 7,
      status: { state: 'idle' },
    });
    renderApp();
    await openGames(user);

    const headers = within(screen.getByRole('table'))
      .getAllByRole('columnheader')
      .map((header) => header.textContent);
    expect(headers).toEqual(['What the Games screen will show', 'What has to exist first']);
  });

  /*
   * Every row has to name the work that supplies it, for the same reason the
   * unbuilt screens name the issue that builds them: "coming soon" is
   * indistinguishable from an application that is broken.
   */
  it('names an issue against every thing it cannot show', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime({
      link: 'attached',
      recorder_process_id: 7,
      status: { state: 'idle' },
    });
    renderApp();
    await openGames(user);

    const rows = within(screen.getByRole('table')).getAllByRole('row').slice(1);
    expect(rows.length).toBeGreaterThan(0);

    for (const row of rows) {
      const cells = within(row).getAllByRole('cell');
      expect(cells).toHaveLength(2);
      expect(cells[1]?.textContent).toMatch(/#\d+/);
    }
  });

  /*
   * Reached with the keyboard alone, and focus lands in the screen afterwards
   * so that a screen reader announces the change. The shell owns both
   * behaviours; this checks them through the Games link specifically, because
   * "keyboard navigable" is one of issue #107's three acceptance criteria and a
   * screen that is only reachable by mouse fails it whatever the shell does.
   */
  it('is reached with Tab and Enter, and takes focus when it opens', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime({ link: 'connecting' });
    renderApp();

    // Skip link, Home, Library, Games.
    await user.tab();
    await user.tab();
    await user.tab();
    await user.tab();
    expect(document.activeElement).toHaveTextContent('Games');

    await user.keyboard('{Enter}');

    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Games');
    expect(screen.getByRole('main')).toHaveFocus();
  });

  /*
   * Rendered on its own rather than through the shell, so that the screen's own
   * heading structure is the subject: a screen whose only heading was its title
   * gives a screen-reader user nothing to navigate between.
   */
  it('has a heading for each of its two parts', () => {
    render(<GamesScreen link={null} />);

    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Games');
    expect(
      screen.getAllByRole('heading', { level: 2 }).map((heading) => heading.textContent),
    ).toContain('What this screen will show');
  });
});
