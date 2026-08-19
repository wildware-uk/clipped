import { cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { APPLICATION_SOURCE, USER_LABEL_PREFIX, type EventMark } from './events';
import { RecordingTimeline } from './RecordingTimeline';
import { TIMELINE_COLUMNS, type MarksRead } from './recordingMarks';
import {
  CUSTOM_MARK,
  LONG_RECORDING_SECONDS,
  MANY_MARKS,
  MANY_MARKS_COUNT,
  markAt,
  PLUGIN_MARK,
  SAMPLE_MARKS,
} from './test/eventMarkFixture';

/**
 * The recording timeline as it is drawn, and what pressing one of its markers
 * does (issue #65).
 *
 * The marks are the recorder's own exemplar out of `protocol-schema.json`, so
 * every mark drawn here is a mark a real `library_events` reply really carries.
 *
 * # The two failures these are written against
 *
 * **"The markers are drawn."** True of a build that draws every one of them at
 * `left: 0`, which is a timeline with one clickable thing on it. So the
 * positions are read off the elements and compared with each other, not merely
 * counted.
 *
 * **"Clicking seeks."** True of a build that seeks to zero every time. So every
 * click is compared against that marker's own position, and the assertion says
 * which marker and which second when it fails.
 */

/** The recorder answered with these marks. */
function read(marks: readonly EventMark[]): MarksRead {
  return { state: 'read', marks };
}

/** Renders the timeline over a recording of `durationSeconds`, and watches the seeks. */
function draw(
  marks: readonly EventMark[],
  durationSeconds: number | null = 60,
): { readonly seeks: number[] } {
  const seeks: number[] = [];
  render(
    <RecordingTimeline
      read={read(marks)}
      durationSeconds={durationSeconds}
      of="cs2.mkv"
      unasked="nothing was asked"
      onSeek={(seconds) => seeks.push(seconds)}
    />,
  );
  return { seeks };
}

/** Every marker on the lane, in the order it is drawn. */
function markers(): readonly HTMLElement[] {
  return within(screen.getByRole('list', { name: 'Marks on cs2.mkv' })).getAllByRole('button');
}

/** Where one marker is drawn, as the fraction its inline style carries. */
function positionOf(marker: HTMLElement): string {
  return (marker.parentElement as HTMLElement).style.left;
}

/** The outline a marker is drawn as: the `d` of its one path. */
function shapeOf(marker: HTMLElement): string {
  return marker.querySelector('path')?.getAttribute('d') ?? '';
}

describe('the recording timeline', () => {
  afterEach(() => {
    cleanup();
  });

  it('draws one marker per mark of the recorder’s own exemplar', () => {
    draw(SAMPLE_MARKS);

    expect(markers()).toHaveLength(SAMPLE_MARKS.length);
  });

  it('draws them at different places, rather than all at the start', () => {
    draw(SAMPLE_MARKS);

    const places = markers().map(positionOf);

    expect(new Set(places).size, `every marker was drawn at ${places.join(', ')}`).toBe(
      places.length,
    );
    expect(places[0]).not.toBe(places[1]);
  });

  it('seeks to the marker that was pressed, and not to the start', async () => {
    // The exemplar's two marks are four and nine and a half seconds in.
    const { seeks } = draw(SAMPLE_MARKS);
    const drawn = markers();

    await userEvent.click(drawn[1]!);

    const expected = (SAMPLE_MARKS[1]?.at ?? 0) / 1_000_000_000;
    expect(
      seeks,
      `pressing "${drawn[1]?.getAttribute('aria-label') ?? ''}" should seek to ${String(expected)}s`,
    ).toEqual([expected]);
  });

  it('seeks each marker somewhere different', async () => {
    const { seeks } = draw(SAMPLE_MARKS);

    for (const marker of markers()) {
      await userEvent.click(marker);
    }

    expect(new Set(seeks).size, `the seeks were ${seeks.join(', ')}`).toBe(seeks.length);
    expect(seeks).toEqual(SAMPLE_MARKS.map((mark) => mark.at / 1_000_000_000));
  });

  it('says what it moved to, for anybody not watching the player', async () => {
    draw(SAMPLE_MARKS);
    const marker = markers()[0]!;

    await userEvent.click(marker);

    expect(screen.getByText(/^Moved to /)).toHaveTextContent(
      marker.getAttribute('aria-label') ?? '',
    );
  });

  describe('telling one mark’s source from another’s', () => {
    /** A plugin's mark, one of Clipped's own, and one somebody typed. */
    const THREE: readonly EventMark[] = [
      markAt(10),
      { ...markAt(30), source: APPLICATION_SOURCE },
      { ...markAt(50), kind: `${USER_LABEL_PREFIX}Ace on Nuke`, source: APPLICATION_SOURCE },
    ];

    it('names the source in words, in every marker’s accessible name', () => {
      draw(THREE, 60);

      expect(markers().map((marker) => marker.getAttribute('aria-label'))).toEqual([
        `Kill at 00:10.000, reported by the ${PLUGIN_MARK.source} plugin`,
        'Kill at 00:30.000, marked by Clipped',
        'Ace on Nuke at 00:50.000, labelled by you',
      ]);
    });

    it('draws each of them as a different outline, which is not a colour', () => {
      draw(THREE, 60);

      const shapes = markers().map(shapeOf);

      expect(new Set(shapes).size, `the three were drawn as ${shapes.join(' / ')}`).toBe(3);
      for (const shape of shapes) {
        expect(shape).not.toBe('');
      }
    });

    it('says the same thing again in a legend, in words beside the outline', () => {
      draw(THREE, 60);

      const legend = within(screen.getByRole('list', { name: 'What the marks are' }));

      expect(legend.getByText('Plugin')).toBeVisible();
      expect(legend.getByText('Clipped')).toBeVisible();
      expect(legend.getByText('Yours')).toBeVisible();
    });

    it('lists no origin this recording has none of', () => {
      // Both exemplar marks are a plugin's. A legend entry for Clipped over a
      // recording with none of its marks is the empty lane this screen refuses.
      draw(SAMPLE_MARKS);

      const legend = within(screen.getByRole('list', { name: 'What the marks are' }));

      expect(legend.getByText('Plugin')).toBeVisible();
      expect(legend.queryByText('Clipped')).toBeNull();
      expect(legend.queryByText('Yours')).toBeNull();
    });

    it('attributes a marker covering two sources to neither of them', () => {
      const together: readonly EventMark[] = [
        markAt(10),
        { ...markAt(10.1), kind: `${USER_LABEL_PREFIX}mine`, source: APPLICATION_SOURCE },
      ];

      draw(together, 60);
      const drawn = markers();

      expect(drawn).toHaveLength(1);
      expect(drawn[0]?.getAttribute('aria-label')).toContain('from several sources');
      expect(shapeOf(drawn[0]!)).not.toBe(shapeOf(renderShape(markAt(10))));
    });
  });

  describe('a three-hour recording with ten thousand marks', () => {
    /*
     * The same fixture `recordingMarks.test.ts` measures the arithmetic
     * against, this time through the component, so the bound is a bound on what
     * a browser is actually asked to lay out. Still no frame rate: nothing in
     * jsdom paints, and `docs/desktop-ui.md` says so rather than claiming one.
     */
    it('mounts no more markers than there are columns', () => {
      draw(MANY_MARKS, LONG_RECORDING_SECONDS);

      const drawn = markers();

      expect(MANY_MARKS).toHaveLength(MANY_MARKS_COUNT);
      expect(drawn.length).toBeLessThanOrEqual(TIMELINE_COLUMNS);
      expect(drawn.length).toBeGreaterThan(TIMELINE_COLUMNS / 2);
    });

    it('mounts them at increasing positions across the whole recording', () => {
      draw(MANY_MARKS, LONG_RECORDING_SECONDS);

      const places = markers().map((marker) => Number.parseFloat(positionOf(marker)));

      let previous = -1;
      for (const place of places) {
        expect(
          place,
          `a marker at ${String(place)}% follows one at ${String(previous)}%`,
        ).toBeGreaterThan(previous);
        previous = place;
      }
      expect(places[places.length - 1]).toBeGreaterThan(99);
    });

    it('still seeks to a mark’s own second, three hours in', async () => {
      const { seeks } = draw(MANY_MARKS, LONG_RECORDING_SECONDS);
      const drawn = markers();
      const last = drawn[drawn.length - 1]!;

      await userEvent.click(last);

      const sought = seeks[0] ?? 0;
      expect(
        MANY_MARKS.some((mark) => mark.at === Math.round(sought * 1_000_000_000)),
        `"${last.getAttribute('aria-label') ?? ''}" sought to ${String(sought)}s, which is no mark of this recording`,
      ).toBe(true);
      expect(sought).toBeGreaterThan(LONG_RECORDING_SECONDS * 0.99);
    });

    it('says how many marks it is standing for, so none of them looks lost', () => {
      draw(MANY_MARKS, LONG_RECORDING_SECONDS);

      expect(screen.getByText(new RegExp(`^${String(MANY_MARKS_COUNT)} marks`))).toBeVisible();
    });
  });

  describe('what it draws when there is nothing to place', () => {
    it('says why nothing was asked, rather than drawing an empty lane', () => {
      render(
        <RecordingTimeline
          read={{ state: 'unasked' }}
          durationSeconds={60}
          of="cs2.mkv"
          unasked="This window has not been told which recording this is."
          onSeek={vi.fn()}
        />,
      );

      expect(
        screen.getByText('This window has not been told which recording this is.'),
      ).toBeVisible();
      expect(screen.queryByRole('list', { name: 'Marks on cs2.mkv' })).toBeNull();
    });

    it('tells a recording with no marks from a read that has not happened', () => {
      render(
        <RecordingTimeline
          read={{ state: 'reading' }}
          durationSeconds={60}
          of="cs2.mkv"
          unasked=""
          onSeek={vi.fn()}
        />,
      );
      expect(screen.getByText(/Reading the marks/)).toBeVisible();

      cleanup();
      draw([]);
      expect(screen.getByText(/No marks on this recording/)).toBeVisible();
    });

    it('gives the recorder’s own sentence when the read failed', () => {
      render(
        <RecordingTimeline
          read={{
            state: 'unread',
            problem: { code: 'library_unavailable', message: 'library.db is locked.' },
          }}
          durationSeconds={60}
          of="cs2.mkv"
          unasked=""
          onSeek={vi.fn()}
        />,
      );

      expect(screen.getByText('Your library could not be read')).toBeVisible();
      expect(screen.getByText(/library\.db is locked\./)).toBeVisible();
    });

    it('says it has marks but no length, rather than drawing them against a guess', () => {
      draw(SAMPLE_MARKS, null);

      expect(screen.getByText(/has not reported its length yet/)).toBeVisible();
      expect(screen.queryByRole('list', { name: 'Marks on cs2.mkv' })).toBeNull();
    });

    it('says what has no producer, so four missing sources do not look broken', () => {
      draw(SAMPLE_MARKS);

      expect(
        screen.getAllByText(/A bookmark is written beside the recording/).length,
      ).toBeGreaterThan(0);
    });
  });

  it('names what is drawn on the recording it is drawn for', () => {
    draw([...SAMPLE_MARKS, CUSTOM_MARK]);

    expect(screen.getByRole('list', { name: 'Marks on cs2.mkv' })).toBeVisible();
  });
});

/** One mark drawn on its own, for comparing an outline against. */
function renderShape(mark: EventMark): HTMLElement {
  const { container } = render(
    <RecordingTimeline
      read={read([mark])}
      durationSeconds={60}
      of="alone.mkv"
      unasked=""
      onSeek={vi.fn()}
    />,
  );
  return container.querySelector('.clipped-marks__mark') as HTMLElement;
}
