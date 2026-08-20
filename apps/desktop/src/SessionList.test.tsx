import type { LibraryClip, LibraryRecording, LibrarySession } from '@clipped/shared';
import { act, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
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
  // Inside `act`, because the listener this wakes sets React state. Outside it
  // the update is scheduled and never flushed before the assertions read the
  // DOM, so the test sees the unwindowed first render and reports that the
  // window did not engage - which is what it did report until this wrapped it.
  act(() => {
    shell.dispatchEvent(new Event('scroll'));
  });

  return shell;
}

/**
 * Vitest's default is five seconds, and these take three or four on a fast
 * machine — because mounting ten thousand sessions in jsdom is genuinely that
 * slow, and mounting them is the thing being measured rather than an accident
 * of the setup. On a CI runner that margin is not there: every case in this
 * file has timed out at five seconds on one.
 *
 * Raised rather than shrinking the fixture, because the ten thousand is
 * `docs/library.md`'s own figure and the point of these cases is what the
 * component does at that scale. A timeout tuned to the machine would be an
 * assertion about the runner, not about the code.
 */
const MOUNTING_TEN_THOUSAND = 30_000;

/**
 * No recording controls exercised here; only their absence from the DOM matters.
 *
 * `canExport` is the offered answer because these cases are about how many rows
 * are mounted, and a recorder that could not export would draw a differently
 * worded control on every one of them — a difference this file has no business
 * being sensitive to.
 */
const ACTIONS = {
  outcome: { state: 'idle' } as const,
  canExport: { offered: true } as const,
  open: () => undefined,
  reveal: () => undefined,
  exportToMp4: () => undefined,
};

describe('SessionList, scrolled against a measured shell', () => {
  afterEach(() => {
    document.body.replaceChildren();
  });

  it(
    'mounts every session when nothing has been measured yet',
    () => {
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
    },
    MOUNTING_TEN_THOUSAND,
  );

  it(
    'holds a small, bounded slice of ten thousand sessions once the shell reports a real height',
    () => {
      const shell = renderWindowed(TEN_THOUSAND_SESSIONS, 900, 0);
      const table = shell.querySelector('table') as HTMLElement;

      const bodies = sessionBodies(table);
      expect(bodies.length).toBeGreaterThan(0);
      expect(bodies.length).toBeLessThan(50);
      // The library's other 9,900-odd sessions are still accounted for, in the
      // one spacer row standing in for the height beneath what is mounted.
      expect(tbodyCount(table)).toBeLessThan(bodies.length + 3);
    },
    MOUNTING_TEN_THOUSAND,
  );

  it(
    'draws the sessions actually near the scroll position, not a fixed prefix of the library',
    () => {
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
    },
    MOUNTING_TEN_THOUSAND,
  );

  it(
    'reaches the last session once scrolled to the end of the library',
    () => {
      const shell = renderWindowed(TEN_THOUSAND_SESSIONS, 900, 1_000_000_000);
      const table = shell.querySelector('table') as HTMLElement;

      const bodies = sessionBodies(table);
      expect(bodies.length).toBeGreaterThan(0);
      expect(bodies.length).toBeLessThan(50);
      expect(bodies[bodies.length - 1]?.textContent).toContain('Session 9999');
    },
    MOUNTING_TEN_THOUSAND,
  );
});

/**
 * Issue #673. "There is no file" has more than one cause, and a user can act on
 * only one of them.
 *
 * The case that produced this: a real library held five recordings that failed
 * before an encoder opened — nothing was ever written — and every row read
 * "file missing", which says footage was lost. It had not been. The recorder had
 * written down the actual reason, an encoder that would not take an odd
 * dimension, and the window offered no route to it.
 *
 * Both directions are asserted, because a fix that said "did not record" for
 * everything would pass a test that only checked the first.
 */
describe('a recording with no file says which kind of no file it is', () => {
  afterEach(() => {
    document.body.replaceChildren();
  });

  const GONE_AT = '2026-08-14T09:51:12.3730555+01:00';

  it('says a failed recording never started, rather than that its file is missing', () => {
    const { container } = render(
      <SessionList
        sessions={[session(1, { outcome: 'failed', missing_since: GONE_AT })]}
        label="Sessions"
        actions={ACTIONS}
      />,
    );

    const text = container.textContent ?? '';
    expect(text).toContain('did not record');
    expect(text).not.toContain('file missing');
  });

  it('still says a file has gone when a recording made one and it is not there', () => {
    const { container } = render(
      <SessionList
        sessions={[session(2, { outcome: 'recorded', missing_since: GONE_AT })]}
        label="Sessions"
        actions={ACTIONS}
      />,
    );

    const text = container.textContent ?? '';
    expect(text).toContain('file missing');
    expect(text).not.toContain('did not record');
  });

  it('counts the two separately in the sitting summary', () => {
    // The shape the real library was in: several that never recorded, one that
    // did and whose file is gone. One number covering both would be the same
    // untruth in a different place.
    const recording = (id: number, outcome: string): LibraryRecording => ({
      recording_id: id,
      session_index: 0,
      path: `D:\\clips\\mixed-${String(id)}.mkv`,
      started_at: '2026-08-11T20:14:00+01:00',
      duration_seconds: 600,
      size_bytes: 100_000,
      favourite: false,
      tags: [],
      outcome,
      missing_since: GONE_AT,
    });
    const mixed: LibrarySession = {
      ...session(3),
      recordings: [recording(31, 'failed'), recording(32, 'failed'), recording(33, 'recorded')],
    };

    const { container } = render(
      <SessionList sessions={[mixed]} label="Sessions" actions={ACTIONS} />,
    );

    const text = container.textContent ?? '';
    expect(text).toContain('3 recordings, 2 did not record, 1 file missing');
  });
});

/**
 * The clips a sitting produced (SPEC.md section 45, step 12).
 *
 * Until these rows existed, a saved replay was created by the recorder, indexed
 * by the library and carried in this very read as `session.clips` — and drawn
 * nowhere. The one thing a player presses a hotkey *for* was invisible in the
 * application, while two screens told them clips were not built yet.
 */
describe('the clips a sitting produced', () => {
  afterEach(() => {
    document.body.replaceChildren();
  });

  /** A sitting with one recording and one clip cut from it. */
  function withClip(clip: Partial<LibraryClip> = {}): LibrarySession {
    return {
      ...session(1),
      clips: [
        {
          clip_id: 7,
          path: 'D:/clips/session-1-replay-1.mkv',
          created_at: '2026-08-11T20:24:00+01:00',
          duration_seconds: 30,
          size_bytes: 4_000_000,
          favourite: false,
          tags: [],
          ...clip,
        },
      ],
    };
  }

  it('draws a clip with its length and size, marked as a clip in words', () => {
    render(<SessionList sessions={[withClip()]} label="Sessions" actions={ACTIONS} />);

    const text = document.body.textContent ?? '';
    expect(text).toContain('session-1-replay-1.mkv');
    expect(text).toContain('Clip');
    expect(text).toContain('30 s');
  });

  /*
   * The two controls a clip has, and the reason it has exactly these: opening
   * and revealing need a path and nothing else. Play resolves against a
   * *recording* identifier and Export is a decision rather than a widening,
   * which is why neither is offered here.
   */
  it('offers Open and Show in Explorer, naming the clip', async () => {
    const opened: string[] = [];
    const revealed: string[] = [];
    const user = userEvent.setup();
    render(
      <SessionList
        sessions={[withClip()]}
        label="Sessions"
        actions={{
          ...ACTIONS,
          open: (item) => opened.push(item.path),
          reveal: (item) => revealed.push(item.path),
        }}
      />,
    );

    await user.click(screen.getByRole('button', { name: /^Open session-1-replay-1\.mkv/ }));
    await user.click(
      screen.getByRole('button', { name: /^Show session-1-replay-1\.mkv.* in Explorer/ }),
    );

    expect(opened).toEqual(['D:/clips/session-1-replay-1.mkv']);
    expect(revealed).toEqual(['D:/clips/session-1-replay-1.mkv']);
  });

  /*
   * The wiring, not the mapping. `clipPlayback.test.ts` covers what the
   * playback screen does with a clip it is handed; what could still be wrong
   * after that is a Play button that hands over the wrong thing, or nothing.
   */
  it('offers Play, and hands over the clip itself', async () => {
    const played: LibraryClip[] = [];
    const user = userEvent.setup();
    render(
      <SessionList
        sessions={[withClip()]}
        label="Sessions"
        actions={ACTIONS}
        onPlayClip={(clip) => played.push(clip)}
      />,
    );

    await user.click(screen.getByRole('button', { name: /^Play session-1-replay-1\.mkv/ }));

    expect(played).toHaveLength(1);
    expect(played[0]?.clip_id).toBe(7);
  });

  /*
   * A screen with nowhere to play a clip must not draw a control that does
   * nothing, which is the failure AGENTS.md section 27 is about.
   */
  it('draws no Play where nothing can play a clip', () => {
    render(<SessionList sessions={[withClip()]} label="Sessions" actions={ACTIONS} />);

    expect(screen.queryByRole('button', { name: /^Play / })).toBeNull();
  });

  it('prefers the clip’s own title when it has one', () => {
    render(
      <SessionList
        sessions={[withClip({ title: 'That triple' })]}
        label="Sessions"
        actions={ACTIONS}
      />,
    );

    expect(document.body.textContent ?? '').toContain('That triple');
  });

  /*
   * Two absences that read the same and are not: a clip the library has never
   * seen a file for has nothing to open, and one whose file has gone has
   * something to explain. Offering a control for either would be a control
   * that fails when pressed.
   */
  it('says why a clip cannot be opened, and disables the controls', () => {
    const { unmount } = render(
      <SessionList
        sessions={[withClip({ missing_since: '2026-08-12T09:00:00+01:00' })]}
        label="Sessions"
        actions={ACTIONS}
      />,
    );

    expect(document.body.textContent ?? '').toContain('gone from where the library last saw it');
    expect(screen.getByRole('button', { name: /^Open session-1-replay-1\.mkv/ })).toBeDisabled();
    unmount();
    document.body.replaceChildren();

    const noFile = withClip();
    const clipWithoutPath = { ...noFile.clips[0] } as Record<string, unknown>;
    delete clipWithoutPath['path'];
    render(
      <SessionList
        sessions={[{ ...noFile, clips: [clipWithoutPath as unknown as LibraryClip] }]}
        label="Sessions"
        actions={ACTIONS}
      />,
    );

    expect(document.body.textContent ?? '').toContain('never seen a file for it');
    expect(screen.getByRole('button', { name: /^Open a clip/ })).toBeDisabled();
  });

  /*
   * Home passes no actions — it is a summary, and a list of files under each
   * sitting would bury it. The clips follow the recordings in that.
   */
  it('draws no clip rows where the recordings are not drawn either', () => {
    render(<SessionList sessions={[withClip()]} label="Sessions" />);

    expect(document.body.textContent ?? '').not.toContain('session-1-replay-1.mkv');
  });
});
