import type { LibrarySession } from '@clipped/shared';
import { useEffect, useMemo, useState, type RefObject } from 'react';

/**
 * Keeping a session list scrollable when it holds thousands of sittings
 * (issue #60's second acceptance criterion).
 *
 * `SessionList` mounted every session it was given, each in its own `<tbody>`
 * with a row per recording under it. That is fine for the page of 25 the
 * Library screen asks for at a time, and it stops being fine long before
 * somebody has pressed "Show more" enough times to hold `docs/library.md`'s
 * own fixture — ten thousand sittings — in one table: a browser laying out and
 * painting tens of thousands of rows on every scroll frame is the jank this
 * criterion is about, and no amount of asking the recorder for data faster
 * fixes a cost that is now entirely on this side of the wire.
 *
 * # What is measured, and what is estimated
 *
 * A sitting's real height depends on how many recordings it drew, and this
 * module never sees a real one — mounting every session once to measure it
 * would be the cost virtualisation exists to avoid. So each session's height
 * is *estimated* from its row count, at {@link ESTIMATED_ROW_HEIGHT_PX} per
 * row, which is close enough to keep the scrollbar roughly proportionate and
 * the visible window roughly right. It is not pixel-perfect and does not need
 * to be: the failure this guards against is a table with ten thousand rows in
 * the DOM at once, not a scrollbar thumb a few pixels short.
 *
 * # Off by default, on by measurement
 *
 * {@link sessionWindow} renders every session, unwindowed, whenever the
 * viewport it was given is `0` — which is what jsdom always reports, having no
 * real layout engine. So every existing case that renders a `SessionList`
 * keeps seeing every session it always saw; nothing here can make a screen
 * draw fewer rows than a test already asserts on. It only starts trimming the
 * table once {@link useSessionWindow} finds a real, measured `.clipped-shell__main`
 * with a non-zero `clientHeight` to trim it against.
 */

/** The estimated height of one table row, in pixels. See the module doc. */
export const ESTIMATED_ROW_HEIGHT_PX = 40;

/** How many sessions beyond the measured viewport are kept mounted either side. */
export const DEFAULT_OVERSCAN = 4;

/**
 * A sitting's estimated height: one row for itself, and one more per
 * recording drawn under it — which is only when `showsRecordings` is true,
 * because Home draws no recording rows at all (`SessionList`'s own `actions`
 * contract).
 */
export function estimatedSessionHeight(session: LibrarySession, showsRecordings: boolean): number {
  const rows = 1 + (showsRecordings ? session.recordings.length : 0);
  return rows * ESTIMATED_ROW_HEIGHT_PX;
}

/**
 * The running total of estimated height before each session, and after the
 * last one.
 *
 * Length `sessions.length + 1`: `heights[i]` is where session `i` starts and
 * `heights[sessions.length]` is the table's whole estimated height, which is
 * what the two spacer rows are measured against.
 */
export function cumulativeHeights(
  sessions: readonly LibrarySession[],
  showsRecordings: boolean,
): readonly number[] {
  const heights: number[] = [0];
  let total = 0;
  for (const session of sessions) {
    total += estimatedSessionHeight(session, showsRecordings);
    heights.push(total);
  }
  return heights;
}

/** The smallest index into `cumulative` whose value exceeds `target`. */
function firstIndexAbove(cumulative: readonly number[], target: number): number {
  let low = 0;
  let high = cumulative.length - 1;
  while (low < high) {
    const mid = (low + high) >>> 1;
    if ((cumulative[mid] ?? 0) <= target) {
      low = mid + 1;
    } else {
      high = mid;
    }
  }
  return low;
}

/** Which sessions a windowed `SessionList` should mount, and what to skip. */
export interface SessionWindow {
  /** The first session index to render. */
  readonly start: number;
  /** One past the last session index to render. */
  readonly end: number;
  /**
   * Whether this is a real window, trimmed against a measured viewport, or
   * `start: 0, end: sessions.length` because there was nothing to measure yet.
   */
  readonly virtualized: boolean;
  /** The height, in pixels, the skipped sessions before `start` stand in for. */
  readonly topSpacerPx: number;
  /** The height, in pixels, the skipped sessions after `end` stand in for. */
  readonly bottomSpacerPx: number;
}

/**
 * The window a scrolled table should draw, from the estimated offsets of
 * every session and where the viewport currently sits.
 *
 * A pure function of its inputs, so this is what the 10,000-session fixture
 * in `virtualWindow.test.ts` drives directly rather than through a rendered
 * table: the property that matters — the window stays a small, bounded slice
 * of the library at every scroll position — holds regardless of how a browser
 * would have painted it, and is cheaper to prove that way. `firstIndexAbove`
 * is a binary search rather than a scan for the same reason `library.md`'s own
 * paging is keyset rather than offset: a table of ten thousand sessions is
 * exactly the case where an `O(n)` search on every scroll event would be the
 * jank this module exists to remove.
 */
export function sessionWindow(
  cumulative: readonly number[],
  scrollOffset: number,
  viewportHeight: number,
  overscan: number,
): SessionWindow {
  const count = cumulative.length - 1;
  if (viewportHeight <= 0 || count === 0) {
    return { start: 0, end: count, virtualized: false, topSpacerPx: 0, bottomSpacerPx: 0 };
  }

  const top = Math.max(0, scrollOffset);
  const bottom = top + viewportHeight;

  // The session whose bottom edge first passes the viewport's top, minus the
  // overscan; and the session whose top edge first passes the viewport's
  // bottom, plus it.
  const start = Math.max(0, firstIndexAbove(cumulative, top) - 1 - overscan);
  const end = Math.min(count, firstIndexAbove(cumulative, bottom) + overscan);

  return {
    start,
    end,
    virtualized: true,
    topSpacerPx: cumulative[start] ?? 0,
    bottomSpacerPx: (cumulative[count] ?? 0) - (cumulative[end] ?? 0),
  };
}

/**
 * Measures `.clipped-shell__main` — the one element in the shell that scrolls
 * (`AppShell.tsx`) — around `anchor`, and keeps a {@link SessionWindow} against
 * it.
 *
 * `anchor` is the table itself rather than a container this hook owns, so
 * `SessionList` stays a table with nothing wrapped around it for layout's
 * sake; `closest` finds the ancestor that scrolls without either component
 * needing a ref threaded down from `AppShell` to know it. When there is no
 * such ancestor — every `LibraryScreen` case that renders the screen alone,
 * outside `<App>` — this falls back to the unwindowed state, which is the
 * `viewportHeight: 0` branch of {@link sessionWindow}.
 */
export function useSessionWindow(
  anchor: RefObject<HTMLElement | null>,
  sessions: readonly LibrarySession[],
  showsRecordings: boolean,
  overscan: number = DEFAULT_OVERSCAN,
): SessionWindow {
  const cumulative = useMemo(
    () => cumulativeHeights(sessions, showsRecordings),
    [sessions, showsRecordings],
  );

  const [range, setRange] = useState<SessionWindow>(() =>
    sessionWindow(cumulative, 0, 0, overscan),
  );

  useEffect(() => {
    const table = anchor.current;
    if (table === null) {
      return;
    }
    const container = table.closest<HTMLElement>('.clipped-shell__main');
    if (container === null) {
      return;
    }

    const measure = (): void => {
      // How far the table's own top has scrolled past the container's visible
      // top edge. Reading it this way, rather than off `container.scrollTop`
      // directly, needs no separate account of where the table sits inside the
      // container — the two rectangles already carry that.
      const scrollOffset =
        container.getBoundingClientRect().top - table.getBoundingClientRect().top;
      setRange(sessionWindow(cumulative, scrollOffset, container.clientHeight, overscan));
    };

    // The first measurement is taken on the next frame rather than called
    // here directly: this is an effect synchronising with layout, not a
    // derivation of the render it belongs to, and everything after mount
    // reaches `setRange` the same way — from a callback the browser invokes,
    // never from the body of the effect itself.
    const firstMeasure = requestAnimationFrame(measure);
    container.addEventListener('scroll', measure, { passive: true });
    window.addEventListener('resize', measure);
    return (): void => {
      cancelAnimationFrame(firstMeasure);
      container.removeEventListener('scroll', measure);
      window.removeEventListener('resize', measure);
    };
  }, [anchor, cumulative, overscan]);

  return range;
}
