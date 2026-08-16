import type { LibraryRecording, LibrarySession } from '@clipped/shared';
import { render } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import { SessionList } from './SessionList';

/**
 * `virtualWindow.test.ts` proves the windowing maths in isolation, against
 * `docs/library.md`'s own ten-thousand-session fixture. What that leaves
 * unproved is the wiring: that `SessionList` actually asks `.clipped-shell__main`
 * how tall it is, and actually trims what it mounts once it gets a real answer
 * — rather than, say, discarding the measurement, or windowing the wrong list.
 *
 * jsdom never lays anything out, so there is no `.clipped-shell__main` with a
 * real height to find without one being built by hand. That is what
 * `measuredShell` below does: a real `<main className="clipped-shell__main">`
 * with `clientHeight` and `getBoundingClientRect` overridden to the numbers a
 * real window would have measured, so `useSessionWindow`'s `closest(...)` finds
 * exactly the element `AppShell.tsx` renders and reacts to it exactly the way
 * it would to a real one.
 */

/** A minimal sitting, distinguished only by its id. */
function session(id: number, recording: Partial<LibraryRecording> = {}): LibrarySession {
  return {
    session_id: `session-${String(id)}`,
    game_id: 'cs2',
    game_name: `Session ${String(id)}`,
    started_at: '2026-08-11T20:14:00+01:00',
    favourite: false,
    recordings: [
      {
        recording_id: id,
        session_index: 0,
        path: `D:\\clips\\session-${String(id)}.mkv`,
        started_at: '2026-08-11T20:14:00+01:00',
        duration_seconds: 600,
        size_bytes: 100_000,
        favourite: false,
        tags: [],
        ...recording,
      },
    ],
    clips: [],
  };
}

/** The documented fixture: ten thousand sittings, same as `virtualWindow.test.ts`. */
const TEN_THOUSAND_SESSIONS: readonly LibrarySession[] = Array.from(
  { length: 10_000 },
  (_unused, i) => session(i),
);

/** Every `<tbody>` this test mounted, so it can be measured across cases. */
function tbodyCount(table: HTMLElement): number {
  return table.querySelectorAll('tbody').length;
}

/** The session `<tbody>` elements, excluding the two aria-hidden spacers. */
function sessionBodies(table: HTMLElement): readonly HTMLElement[] {
  return Array.from(table.querySelectorAll<HTMLElement>('tbody:not([aria-hidden])'));
}

/**
 * Mounts `SessionList` inside a stand-in for the shell's own scrolling
 * `<main>`, with `clientHeight` and `getBoundingClientRect` fixed to numbers a
 * real, laid-out window would have reported. `scrollTopPx` positions the table
 * beneath it: the table's own `getBoundingClientRect` is offset upward by
 * exactly that many pixels, which is what scrolling `scrollTopPx` down a real
 * page would have done to it.
 */
function renderWindowed(
  sessions: readonly LibrarySession[],
  viewportHeight: number,
  scrollTopPx: number,
): HTMLElement {
  const shell = document.createElement('main');
  shell.className = 'clipped-shell__main';
  document.body.appendChild(shell);

  Object.defineProperty(shell, 'clientHeight', { value: viewportHeight, configurable: true });
  shell.getBoundingClientRect = () =>
    ({
      top: 0,
      bottom: viewportHeight,
      left: 0,
      right: 0,
      width: 0,
      height: viewportHeight,
    }) as DOMRect;

  render(<SessionList sessions={sessions} label="Sessions" actions={ACTIONS} />, {
    container: shell,
  });

  const table = shell.querySelector('table');
  if (table === null) {
    throw new Error('SessionList did not render a table');
  }
  table.getBoundingClientRect = () =>
    ({ top: -scrollTopPx, bottom: 0, left: 0, right: 0, width: 0, height: 0 }) as DOMRect;

  // The mount's own measurement ran before the mocks above existed to answer
  // it, so the first render is still the unwindowed one — correctly, for a
  // window that has not been measured yet. A scroll event is what a real
  // browser sends continuously as somebody scrolls, and it is what this hook
  // listens for to re-measure; firing one is how this test asks it to look
  // again now that there is something real to find.
  shell.dispatchEvent(new Event('scroll'));

  return shell;
}

/** No recording controls exercised here; only their absence from the DOM matters. */
const ACTIONS = {
  outcome: { state: 'idle' } as const,
  open: () => undefined,
  reveal: () => undefined,
  exportToMp4: () => undefined,
};

describe('SessionList, scrolled against a measured shell', () => {
  afterEach(() => {
    document.body.replaceChildren();
  });

  it('mounts every session when nothing has been measured yet', () => {
    const shell = document.createElement('main');
    shell.className = 'clipped-shell__main';
    document.body.appendChild(shell);
    // clientHeight stays at jsdom's default of 0: nothing here mocks it.

    render(<SessionList sessions={TEN_THOUSAND_SESSIONS} label="Sessions" actions={ACTIONS} />, {
      container: shell,
    });

    const table = shell.querySelector('table');
    expect(table).not.toBeNull();
    expect(sessionBodies(table as HTMLElement)).toHaveLength(10_000);
  });

  it('holds a small, bounded slice of ten thousand sessions once the shell reports a real height', () => {
    const shell = renderWindowed(TEN_THOUSAND_SESSIONS, 900, 0);
    const table = shell.querySelector('table') as HTMLElement;

    const bodies = sessionBodies(table);
    expect(bodies.length).toBeGreaterThan(0);
    expect(bodies.length).toBeLessThan(50);
    // The library's other 9,900-odd sessions are still accounted for, in the
    // one spacer row standing in for the height beneath what is mounted.
    expect(tbodyCount(table)).toBeLessThan(bodies.length + 3);
  });

  it('draws the sessions actually near the scroll position, not a fixed prefix of the library', () => {
    // What tells a real window from one that only ever truncated the list to
    // its first N: scrolled halfway down a ten-thousand-session library, the
    // sessions on screen are from the middle of it.
    const shell = renderWindowed(TEN_THOUSAND_SESSIONS, 900, 400_000);
    const table = shell.querySelector('table') as HTMLElement;

    const bodies = sessionBodies(table);
    expect(bodies.length).toBeGreaterThan(0);
    expect(bodies.length).toBeLessThan(50);

    const names = bodies.map((body) => body.textContent ?? '');
    expect(names.some((text) => text.includes('Session 0'))).toBe(false);
    expect(names.some((text) => /Session \d{4}/.test(text))).toBe(true);
  });

  it('reaches the last session once scrolled to the end of the library', () => {
    const shell = renderWindowed(TEN_THOUSAND_SESSIONS, 900, 1_000_000_000);
    const table = shell.querySelector('table') as HTMLElement;

    const bodies = sessionBodies(table);
    expect(bodies.length).toBeGreaterThan(0);
    expect(bodies.length).toBeLessThan(50);
    expect(bodies[bodies.length - 1]?.textContent).toContain('Session 9999');
  });
});
