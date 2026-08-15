import { SCREENS } from '@clipped/shared';
import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent, { type UserEvent } from '@testing-library/user-event';
import { StrictMode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { App } from './App';
import { stubRecorderLinkRuntime } from './test/recorderLinkRuntime';

/**
 * The shell's contract, as tests.
 *
 * Two of the three acceptance criteria on issue #48 are checked here, because
 * both are the kind that rots quietly: that no part of the shell shows data it
 * does not have, and that the chrome is operable from the keyboard alone. The
 * third - that a clean clone builds - is CI's job.
 */

/**
 * Mounts what `main.tsx` mounts, `<StrictMode>` and all.
 *
 * Rendering `<App />` bare would be a different application to the one anybody
 * runs. StrictMode double-invokes effects on mount while preserving refs, which
 * is a real difference rather than a testing artefact - it is what `npm run
 * dev` and `npm run dev:web` do - and it defeated the guard that stops focus
 * being moved into `<main>` on first paint. That defect passed a bare `<App />`
 * and fails this, which is why the tree is built here rather than assumed.
 */
function renderApp(): void {
  render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}

/** Every focusable element, in the order Tab visits them. */
async function tabThrough(user: UserEvent, steps: number): Promise<readonly Element[]> {
  const visited: Element[] = [];
  for (let step = 0; step < steps; step += 1) {
    await user.tab();
    if (document.activeElement === null || document.activeElement === document.body) {
      break;
    }
    visited.push(document.activeElement);
  }
  return visited;
}

describe('the application shell', () => {
  beforeEach(() => {
    window.location.hash = '';
  });

  afterEach(() => {
    // Testing Library only registers its own teardown when Vitest's globals are
    // on, and they are off here: an assertion should say where it came from.
    cleanup();
    vi.unstubAllGlobals();
    window.location.hash = '';
  });

  it('lists every screen in the sidebar, in order', () => {
    renderApp();

    const links = [
      ...within(screen.getByRole('navigation', { name: 'Screens' })).getAllByRole('link'),
      ...within(screen.getByRole('navigation', { name: 'Maintenance' })).getAllByRole('link'),
    ];

    expect(links.map((link) => link.textContent)).toEqual(SCREENS.map((entry) => entry.label));
  });

  /*
   * The six screens that are written — Home and Library (issue #60), Games
   * (issue #107), Editor (issue #83), Settings (issue #51) and Diagnostics
   * (issue #101) — are therefore not in this list. They are named rather than
   * filtered by "has a screen", because a list that computed itself from the
   * same fact the shell routes on could not fail: the point of naming them is
   * that building a screen and forgetting to route it, or routing one that was
   * never built, both show up here.
   */
  const WRITTEN_SCREENS: readonly string[] = [
    'home',
    'library',
    'games',
    'editor',
    'settings',
    'diagnostics',
  ];
  const PLACEHOLDER_SCREENS = SCREENS.filter((entry) => !WRITTEN_SCREENS.includes(entry.id));

  it('says which issue builds each unwritten screen instead of drawing an empty one', async () => {
    const user = userEvent.setup();
    renderApp();

    expect(PLACEHOLDER_SCREENS).toHaveLength(SCREENS.length - WRITTEN_SCREENS.length);

    for (const entry of PLACEHOLDER_SCREENS) {
      await user.click(screen.getByRole('link', { name: entry.label }));

      expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent(entry.label);
      expect(screen.getByRole('heading', { level: 2, name: 'Not built yet' })).toBeVisible();
      expect(screen.getByText(new RegExp(`Issue #${entry.trackedIn} builds it`))).toBeVisible();
    }
  });

  /*
   * The other half of the check above: each written screen is routed to the
   * thing that was built rather than to the placeholder. The region each one
   * names is asserted as well as the absence of "Not built yet", so that
   * pointing a route at the wrong screen fails rather than passing on a heading
   * the sidebar supplied anyway.
   *
   * Home has no navigation of its own to click, being the route the application
   * opens on, and Settings is checked separately below because the landmark it
   * is known by is a tablist rather than a region.
   */
  const ROUTED: readonly (readonly [string, string])[] = [
    // The Library screen draws its sessions region whatever the read produced,
    // including the failure this test's unstubbed runtime provokes: it is the
    // region that says which of the three it was (issue #301).
    ['Library', 'Sessions'],
    ['Games', 'Game detection'],
    ['Editor', 'Open clip'],
    ['Diagnostics', 'Capture health'],
  ];

  it.each(ROUTED)(
    'routes %s to the screen that was built, not the placeholder',
    async (label, region) => {
      const user = userEvent.setup();
      renderApp();

      await user.click(screen.getByRole('link', { name: label }));

      expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent(label);
      expect(screen.queryByRole('heading', { level: 2, name: 'Not built yet' })).toBeNull();
      expect(screen.getByRole('region', { name: region })).toBeVisible();
    },
  );

  it('opens on Home, which is written rather than a placeholder', () => {
    renderApp();

    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Home');
    expect(screen.queryByRole('heading', { level: 2, name: 'Not built yet' })).toBeNull();
    expect(screen.getByRole('region', { name: 'Recording now' })).toBeVisible();
  });

  it('routes Settings to the screen that was built rather than to the placeholder', async () => {
    const user = userEvent.setup();
    renderApp();

    await user.click(screen.getByRole('link', { name: 'Settings' }));

    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Settings');
    expect(screen.queryByRole('heading', { level: 2, name: 'Not built yet' })).toBeNull();
    expect(screen.getByRole('tablist', { name: 'Settings sections' })).toBeVisible();
  });

  it('offers no working recorder control while the recorder cannot be reached', () => {
    renderApp();

    const status = screen.getByRole('region', { name: 'Recorder status' });
    expect(within(status).getByText('Not connected')).toBeVisible();
    expect(within(status).queryAllByRole('button')).toHaveLength(0);

    // Two controls in the whole shell, and no third. Anything else would be a
    // button with nothing behind it, which is what AGENTS.md section 27 forbids.
    expect(screen.getAllByRole('button').map((button) => button.textContent)).toEqual([
      'Skip to content',
      'Start recording',
    ]);

    // The record control exists because Home is a screen about recording, and
    // it is disabled and says why rather than being absent (issue #389): a
    // control that appears only once it would work leaves somebody with nothing
    // to read and nowhere to look. Nothing here is running, so nothing is
    // pressable.
    const record = screen.getByRole('button', { name: 'Start recording' });
    expect(record).toBeDisabled();
    expect(
      within(screen.getByRole('region', { name: 'Recording now' })).getByText(
        'There is no recorder to record with.',
      ),
    ).toBeVisible();
  });

  it('names the file a killed recorder left, rather than only saying "Idle"', async () => {
    // The whole of what ADR 0006 calls recovery, at the layer the user reads:
    // the supervisor emits `recording_interrupted` naming the file, and about a
    // second later a state that says the link is attached to a *replacement*
    // recorder doing nothing. Everything about that second message is true, and
    // on its own it leaves somebody with a recording they will never find.
    //
    // Rendered through `<App />` rather than through the hook, because the
    // defect this guards against is the notice being dropped anywhere between
    // the event and the screen.
    const runtime = stubRecorderLinkRuntime({ link: 'connecting' });
    renderApp();

    runtime.emit({
      event: 'recording_interrupted',
      recording_id: 'r-7',
      output: 'D:\\clips\\2026-08-11 cs2.mkv',
      target: 'process cs2.exe',
      elapsed_ms: 42_000,
    });
    runtime.emit({
      event: 'state',
      link: 'attached',
      recorder_process_id: 91,
      features: [],
      status: { state: 'idle' },
    });

    const status = screen.getByRole('region', { name: 'Recorder status' });
    await waitFor(() => {
      expect(within(status).getByText(/D:\\clips\\2026-08-11 cs2\.mkv/)).toBeVisible();
    });
    expect(within(status).getByText(/not resumed/i)).toBeVisible();
    // And still no controls: naming the file is what can be offered, and a
    // button that claimed to resume the recording could not keep the promise
    // (ADR 0006, AGENTS.md section 27).
    expect(within(status).queryAllByRole('button')).toHaveLength(0);
  });

  it('reaches the skip link and then every screen with Tab alone', async () => {
    const user = userEvent.setup();
    renderApp();

    const visited = await tabThrough(user, SCREENS.length + 2);

    expect(visited[0]).toHaveAccessibleName('Skip to content');
    expect(visited.slice(1, SCREENS.length + 1).map((element) => element.textContent)).toEqual(
      SCREENS.map((entry) => entry.label),
    );
  });

  it('navigates with Enter and marks the screen that is open', async () => {
    const user = userEvent.setup();
    renderApp();

    await user.tab();
    await user.tab();
    await user.tab();
    expect(document.activeElement).toHaveTextContent('Library');

    await user.keyboard('{Enter}');

    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Library');
    expect(screen.getByRole('link', { name: 'Library' })).toHaveAttribute('aria-current', 'page');
    expect(screen.getByRole('link', { name: 'Home' })).not.toHaveAttribute('aria-current');
  });

  it('moves focus to the screen after navigating, so the change is announced', async () => {
    const user = userEvent.setup();
    renderApp();

    expect(screen.getByRole('main')).not.toHaveFocus();

    await user.click(screen.getByRole('link', { name: 'Settings' }));

    expect(screen.getByRole('main')).toHaveFocus();
  });

  it('moves focus to the screen when the skip link is used', async () => {
    const user = userEvent.setup();
    renderApp();

    await user.tab();
    await user.keyboard('{Enter}');

    expect(screen.getByRole('main')).toHaveFocus();
  });

  it('names the open screen in the window title', async () => {
    const user = userEvent.setup();
    renderApp();

    expect(document.title).toBe('Clipped — Home');

    await user.click(screen.getByRole('link', { name: 'Diagnostics' }));

    expect(document.title).toBe('Clipped — Diagnostics');
  });

  it('names the address it could not resolve rather than showing a blank screen', () => {
    window.location.hash = '#/nowhere';
    renderApp();

    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Screen not found');
    expect(screen.getByText('/nowhere')).toBeVisible();
  });
});
