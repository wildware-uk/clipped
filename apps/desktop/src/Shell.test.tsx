import { SCREENS } from '@clipped/shared';
import { cleanup, render, screen, within } from '@testing-library/react';
import userEvent, { type UserEvent } from '@testing-library/user-event';
import { StrictMode } from 'react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import { App } from './App';

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

  it('says which issue builds each screen instead of drawing an empty one', async () => {
    const user = userEvent.setup();
    renderApp();

    for (const entry of SCREENS) {
      await user.click(screen.getByRole('link', { name: entry.label }));

      expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent(entry.label);
      expect(screen.getByRole('heading', { level: 2, name: 'Not built yet' })).toBeVisible();
      expect(screen.getByText(new RegExp(`Issue #${entry.trackedIn} builds it`))).toBeVisible();
    }
  });

  it('offers no recorder controls while the recorder cannot be reached', () => {
    renderApp();

    const status = screen.getByRole('region', { name: 'Recorder status' });
    expect(within(status).getByText('Not connected')).toBeVisible();
    expect(within(status).queryAllByRole('button')).toHaveLength(0);

    // The only control in the whole shell is the skip link. Anything else would
    // be a button with nothing behind it, which is what AGENTS.md section 27
    // forbids until the recorder can be reached.
    expect(screen.getAllByRole('button').map((button) => button.textContent)).toEqual([
      'Skip to content',
    ]);
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
