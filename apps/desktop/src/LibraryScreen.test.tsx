import { cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { StrictMode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { App } from './App';
import { LibraryScreen } from './LibraryScreen';
import { A_COUNT, textRuns } from './test/counts';
import { stubRecorderLinkRuntime } from './test/recorderLinkRuntime';

/**
 * The Library screen's contract, as tests (issue #60).
 *
 * The deck draws a Sessions / Clips / Highlights tab strip, a row of filter
 * chips, a search field and a grid of thumbnails. None of it can be honoured:
 * the library index is built inside the recorder's process and nothing carries a
 * row of it to this window (issue #301). So every case here is about the screen
 * refusing to look finished — an empty Sessions tab, a "0 clips" or a search box
 * that cannot search would each be indistinguishable from the real thing on a
 * library nobody has read (AGENTS.md sections 27 and 54).
 */

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

describe('the Library screen', () => {
  beforeEach(() => {
    window.location.hash = '';
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    window.location.hash = '';
  });

  /*
   * The sentence somebody needs is which of the two it is: an empty library, or
   * a library nothing has looked at. It is the second, and saying so is the
   * whole screen — with the issue that changes it, so the reader has somewhere
   * to go (AGENTS.md section 45).
   */
  it('says the library has not been read, rather than that it is empty', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime({
      link: 'attached',
      recorder_process_id: 7,
      status: { state: 'idle' },
    });
    renderApp();
    await openLibrary(user);

    const panel = screen.getByRole('region', { name: 'Why this list is empty' });
    expect(within(panel).getByRole('heading', { level: 2 })).toHaveTextContent(
      /^This window cannot read the library$/,
    );
    expect(panel).toHaveTextContent(/no command that reads it|no file-system permission/i);
    expect(panel).toHaveTextContent(/#301\b/);
  });

  /*
   * The deck's search field, filter chips, tab strip and row menus all drive
   * something that cannot be reached, so none of them is drawn. Asserted as a
   * property rather than as four cases: anything operable inside the screen
   * would have to do something, and nothing here can.
   *
   * `tablist` is called out separately because it is the one somebody is most
   * likely to add: issue #215 describes the tab strip in detail, and a strip
   * over three panels that all say "nothing to show" is exactly the component
   * with no consumer that issue exists to prevent.
   */
  it('offers no control, because nothing it would drive can be reached', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime({
      link: 'attached',
      recorder_process_id: 7,
      status: { state: 'idle' },
    });
    renderApp();
    await openLibrary(user);

    const main = screen.getByRole('main');
    expect(within(main).queryAllByRole('button')).toHaveLength(0);
    expect(within(main).queryAllByRole('link')).toHaveLength(0);
    expect(within(main).queryAllByRole('searchbox')).toHaveLength(0);
    expect(within(main).queryAllByRole('textbox')).toHaveLength(0);
    expect(within(main).queryAllByRole('combobox')).toHaveLength(0);
    expect(within(main).queryAllByRole('checkbox')).toHaveLength(0);
    expect(within(main).queryAllByRole('radio')).toHaveLength(0);
    expect(within(main).queryAllByRole('tablist')).toHaveLength(0);
    expect(within(main).queryAllByRole('tab')).toHaveLength(0);
  });

  /*
   * The tempting failure: a dense-looking screen of zeroes. A "0 sessions" on a
   * library the window has not read is a measurement nobody took, and it is
   * indistinguishable from the true answer on a machine that has recorded
   * nothing.
   *
   * Matched as a bare figure against the nouns SPEC.md sections 17, 29 and 30
   * count, so a count of anything fails until something has been measured. Run
   * over each text node rather than over the screen's `textContent`, for the
   * reason `test/counts.ts` sets out.
   */
  it('draws no count, because nothing here has counted anything', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime({
      link: 'attached',
      recorder_process_id: 7,
      status: { state: 'idle' },
    });
    renderApp();
    await openLibrary(user);

    for (const run of textRuns(screen.getByRole('main'))) {
      expect(run).not.toMatch(A_COUNT);
    }
  });

  /*
   * The screen's one table is what it cannot show and why, not an empty list of
   * sessions. The headings are asserted so that a table of the deck's own
   * columns — Session / Game / Date / Duration, with no rows — fails.
   */
  it('draws a table of what is missing rather than an empty table of sessions', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime({
      link: 'attached',
      recorder_process_id: 7,
      status: { state: 'idle' },
    });
    renderApp();
    await openLibrary(user);

    const headers = within(screen.getByRole('table'))
      .getAllByRole('columnheader')
      .map((header) => header.textContent);
    expect(headers).toEqual(['What the Library screen will show', 'What has to exist first']);
  });

  /*
   * What SPEC.md sections 17, 29 and 30 ask of this screen, and the issue that
   * supplies each — written out here rather than mapped from the screen's own
   * array.
   *
   * That distinction is the whole test. Walking the rendered rows and asserting
   * "two cells, and the second names some issue" is satisfied by a table holding
   * one invented row. The claim is that *these eight* are what the screen owes
   * and each is pinned to the work that lands it, and only a list kept
   * independently of the implementation can make it. Adding a ninth row is a new
   * promise to the reader, which is why the count is asserted too.
   */
  const MUST_BE_NAMED: readonly (readonly [string, RegExp, readonly number[]])[] = [
    ['sessions and the footage inside them', /^sessions, each with the matches/i, [56, 301]],
    ['clips and highlights', /^clips, and the highlights/i, [74, 76, 301]],
    ['search', /^search by game, date/i, [59, 301]],
    ['favourites', /^favourites, and filtering/i, [58, 301]],
    ['thumbnails and waveforms', /^a thumbnail against each recording/i, [57, 66, 301]],
    ['a recording whose file has gone', /whose file has gone/i, [301]],
    ['playing a clip', /^playing a clip/i, [52]],
    ['restoring something deleted', /^restoring something deleted/i, [94]],
  ];

  it('names each part it owes, and the issue that lands it', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime({
      link: 'attached',
      recorder_process_id: 7,
      status: { state: 'idle' },
    });
    renderApp();
    await openLibrary(user);

    const rows = within(screen.getByRole('table')).getAllByRole('row').slice(1);
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
  it('has a heading for each of its two parts', () => {
    render(<LibraryScreen />);

    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Library');
    expect(
      screen.getAllByRole('heading', { level: 2 }).map((heading) => heading.textContent),
    ).toEqual(['This window cannot read the library', 'What this screen will show']);
  });
});
