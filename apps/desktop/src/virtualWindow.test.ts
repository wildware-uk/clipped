import type { LibrarySession } from '@clipped/shared';
import { describe, expect, it } from 'vitest';

import {
  ESTIMATED_ROW_HEIGHT_PX,
  cumulativeHeights,
  estimatedSessionHeight,
  sessionWindow,
} from './virtualWindow';

/**
 * Issue #60's second acceptance criterion: "large libraries scroll smoothly
 * (measured with a documented fixture size)". The fixture is ten thousand
 * sessions, the same size `docs/library.md` measures the index itself
 * against, so a figure here and a figure there describe the same library.
 *
 * What is proved is structural rather than a frame rate: nothing in this
 * process can paint a frame to measure one. What *can* be shown, and is
 * shown below, is the property a frame rate would actually depend on —
 * that a windowed table never holds more than a small, bounded slice of the
 * library in the DOM, at any scroll position, for a library of this size.
 * That is the mechanism a browser needs to keep scrolling smooth; whether it
 * does, in a real window, is `docs/desktop-ui.md`'s own honest "not verified".
 */

/** A minimal sitting, distinguished only by its id and recording count. */
function session(id: number, recordings: number): LibrarySession {
  return {
    session_id: `session-${String(id)}`,
    game_id: 'cs2',
    game_name: 'Counter-Strike 2',
    started_at: '2026-08-11T20:14:00+01:00',
    favourite: false,
    recordings: Array.from({ length: recordings }, (_unused, index) => ({
      recording_id: id * 100 + index,
      session_index: index,
      path: `D:\\clips\\session-${String(id)}-${String(index)}.mkv`,
      started_at: '2026-08-11T20:14:00+01:00',
      favourite: false,
      tags: [],
    })),
    clips: [],
  };
}

/** The documented fixture: ten thousand sittings, one to three recordings each. */
const TEN_THOUSAND_SESSIONS: readonly LibrarySession[] = Array.from(
  { length: 10_000 },
  (_unused, i) => session(i, (i % 3) + 1),
);

describe('estimatedSessionHeight', () => {
  it('is one row when Home draws no recordings under it', () => {
    expect(estimatedSessionHeight(session(0, 5), false)).toBe(ESTIMATED_ROW_HEIGHT_PX);
  });

  it('is one row per recording, plus the session row itself, on the Library screen', () => {
    expect(estimatedSessionHeight(session(0, 3), true)).toBe(4 * ESTIMATED_ROW_HEIGHT_PX);
  });
});

describe('cumulativeHeights', () => {
  it('starts at zero and ends at the sum of every estimated height', () => {
    const sessions = [session(0, 1), session(1, 2)];
    const cumulative = cumulativeHeights(sessions, true);

    expect(cumulative[0]).toBe(0);
    expect(cumulative).toHaveLength(3);
    expect(cumulative[2]).toBe(
      estimatedSessionHeight(sessions[0]!, true) + estimatedSessionHeight(sessions[1]!, true),
    );
  });
});

describe('sessionWindow', () => {
  it('renders every session, unwindowed, when there is nothing measured to trim against', () => {
    // The property the rest of the suite depends on: a viewport of zero — what
    // jsdom always reports — is a no-op, so every existing rendered-list case
    // keeps seeing every session it always saw.
    const cumulative = cumulativeHeights(TEN_THOUSAND_SESSIONS, true);

    const window_ = sessionWindow(cumulative, 0, 0, 4);

    expect(window_).toEqual({
      start: 0,
      end: 10_000,
      virtualized: false,
      topSpacerPx: 0,
      bottomSpacerPx: 0,
    });
  });

  it('holds a small, bounded slice of ten thousand sessions at the top of the library', () => {
    const cumulative = cumulativeHeights(TEN_THOUSAND_SESSIONS, true);
    const viewportHeight = 900;
    const overscan = 4;

    const window_ = sessionWindow(cumulative, 0, viewportHeight, overscan);

    expect(window_.virtualized).toBe(true);
    expect(window_.start).toBe(0);
    expect(window_.topSpacerPx).toBe(0);
    // At two to four estimated rows a session, a 900px viewport holds well
    // under fifty of them even before the top is clamped to zero.
    expect(window_.end - window_.start).toBeLessThan(50);
    expect(window_.end).toBeLessThan(TEN_THOUSAND_SESSIONS.length);
  });

  it('holds a small, bounded slice at the bottom of the library too', () => {
    const cumulative = cumulativeHeights(TEN_THOUSAND_SESSIONS, true);
    const total = cumulative[cumulative.length - 1] ?? 0;
    const viewportHeight = 900;

    const window_ = sessionWindow(cumulative, total, viewportHeight, 4);

    expect(window_.end).toBe(TEN_THOUSAND_SESSIONS.length);
    expect(window_.bottomSpacerPx).toBe(0);
    expect(window_.end - window_.start).toBeLessThan(50);
  });

  it('holds a small, bounded slice at every one of a hundred scroll positions in between', () => {
    // The property a frame rate would depend on, checked everywhere rather
    // than at the two ends: nowhere in a ten-thousand-session library does
    // scrolling to it mount more than a small window of sessions.
    const cumulative = cumulativeHeights(TEN_THOUSAND_SESSIONS, true);
    const total = cumulative[cumulative.length - 1] ?? 0;
    const viewportHeight = 900;

    for (let step = 0; step <= 100; step += 1) {
      const scrollOffset = (total * step) / 100;
      const window_ = sessionWindow(cumulative, scrollOffset, viewportHeight, 4);

      expect(window_.start).toBeGreaterThanOrEqual(0);
      expect(window_.end).toBeLessThanOrEqual(TEN_THOUSAND_SESSIONS.length);
      expect(window_.end - window_.start).toBeLessThan(50);
    }
  });

  it('moves the window forward as the scroll offset grows, rather than always drawing the first sessions', () => {
    // What tells a real window from one that just truncated the list: the
    // sessions it draws are the ones actually near the scroll position, not a
    // fixed prefix of the library.
    const cumulative = cumulativeHeights(TEN_THOUSAND_SESSIONS, true);
    const total = cumulative[cumulative.length - 1] ?? 0;

    const top = sessionWindow(cumulative, 0, 900, 4);
    const middle = sessionWindow(cumulative, total / 2, 900, 4);
    const bottom = sessionWindow(cumulative, total, 900, 4);

    expect(middle.start).toBeGreaterThan(top.start);
    expect(bottom.start).toBeGreaterThan(middle.start);
    expect(middle.start).toBeGreaterThan(0);
    expect(middle.topSpacerPx).toBeGreaterThan(0);
    expect(middle.bottomSpacerPx).toBeGreaterThan(0);
  });

  it('covers the whole library exactly once between the spacers and the rendered slice', () => {
    // The spacers are a claim about height, not about which sessions exist:
    // whatever is skipped above `start` and below `end` is exactly what the
    // two spacers stand in for, at every scroll position.
    const cumulative = cumulativeHeights(TEN_THOUSAND_SESSIONS, true);
    const total = cumulative[cumulative.length - 1] ?? 0;

    for (const scrollOffset of [0, total * 0.1, total * 0.5, total * 0.9, total]) {
      const window_ = sessionWindow(cumulative, scrollOffset, 900, 4);
      const renderedHeight = (cumulative[window_.end] ?? 0) - (cumulative[window_.start] ?? 0);

      expect(window_.topSpacerPx + renderedHeight + window_.bottomSpacerPx).toBe(total);
    }
  });
});
