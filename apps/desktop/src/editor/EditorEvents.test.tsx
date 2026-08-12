import { cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it } from 'vitest';

import { storedDocument } from '../test/editDocumentFixture';
import { EditorScreen } from './EditorScreen';

/**
 * The game events on the recording's timeline (issue #71).
 *
 * Two properties are defended here, and both are AGENTS.md section 27:
 *
 * - **"nobody asked" is not "there were none".** This build cannot reach a
 *   library, so the ordinary state of the lane is *unknown*, and it has to read
 *   differently from a session that genuinely produced no events. A screen that
 *   drew the same empty lane for both would report a quiet session where there
 *   had merely been no question.
 * - **no mark is quietly skipped.** A kind this build has never heard of is
 *   drawn like any other, and an event the clip trimmed out is counted in words
 *   rather than moved to the nearest cut.
 *
 * The filter cases drive real keys at real buttons, because "filtering works
 * and is keyboard accessible" is the acceptance criterion and calling a handler
 * proves nothing about reaching it.
 *
 * A separate file from `EditorScreen.test.tsx` because it is a separate subject
 * with its own fixtures; both drive the same screen.
 */

afterEach(() => {
  // Testing Library only registers its own teardown when Vitest's globals are
  // on, and they are off here.
  cleanup();
});

/** The recording the fixture's first two segments draw on. */
const FIRST_RECORDING = 'rec-2026-08-11-cs2';

/** An event of that recording, `atSeconds` into its file. */
function event(kind: string, atSeconds: number, source = 'counter-strike-2') {
  return { recording: FIRST_RECORDING, at: atSeconds * 1_000_000_000, kind, source };
}

/** What the transport says the position is. */
function position(): string {
  return screen.getByRole('slider', { name: 'Playhead' }).getAttribute('aria-valuetext') ?? '';
}

/**
 * The marks on the timeline: what each is called, and where it is drawn.
 *
 * The position is read off the element's own `left`, which is what puts the
 * mark on screen — asserting a number held in state would pass for a mark drawn
 * in the wrong place.
 */
function marks(): readonly { readonly name: string; readonly at: number }[] {
  const lane = screen.queryByRole('list', { name: 'Events on this clip' });
  if (lane === null) {
    return [];
  }
  return within(lane)
    .queryAllByRole('button')
    .map((button) => ({
      name: button.getAttribute('aria-label') ?? '',
      at: Number.parseFloat(
        (button.parentElement as HTMLElement | null)?.style.left.replace('%', '') ?? 'NaN',
      ),
    }));
}

/** The kind toggles, with the state each is in. */
function filters(): readonly { readonly label: string; readonly pressed: string | null }[] {
  const group = screen.queryByRole('group', { name: 'Show events' });
  if (group === null) {
    return [];
  }
  return within(group)
    .getAllByRole('button')
    .map((button) => ({
      label: button.textContent ?? '',
      pressed: button.getAttribute('aria-pressed'),
    }));
}

describe('events on the timeline', () => {
  it('says nobody has asked, rather than drawing a lane with nothing in it', () => {
    render(<EditorScreen clip={storedDocument()} />);

    expect(marks()).toEqual([]);
    expect(filters()).toEqual([]);
    expect(screen.queryByRole('list', { name: 'Events on this clip' })).toBeNull();
    expect(screen.getByText(/nothing in this window has asked/i)).toBeVisible();
    // The lane is not among the tracks either: there is nothing to say about a
    // lane nobody supplied.
    expect(
      within(screen.getByRole('list', { name: 'Tracks' }))
        .getAllByRole('listitem')
        .map((track) => track.textContent),
    ).toEqual(['Video3 segments', 'Game-3.0 dB', 'MicrophoneMuted']);
  });

  it('says a session had none differently from a session nobody asked about', () => {
    render(<EditorScreen clip={storedDocument()} events={[]} />);

    expect(screen.getByText('No events were reported during these recordings.')).toBeVisible();
    expect(screen.queryByText(/nothing in this window has asked/i)).toBeNull();
    expect(marks()).toEqual([]);
    // The lane is there and empty, because the question was asked and answered.
    expect(screen.getByRole('list', { name: 'Events on this clip' })).toBeInTheDocument();
    expect(screen.getByText('None on this clip')).toBeVisible();
  });

  it('draws each event where its moment is on the edited timeline', () => {
    // 34s of the recording is four seconds into a 24-second clip whose first
    // segment starts at 30s; 95s is eleven, because the second segment begins
    // eight seconds in and plays from 92s. Subtracting from the recording's own
    // zero would put both of them off the end of the clip.
    render(
      <EditorScreen clip={storedDocument()} events={[event('kill', 95), event('death', 34)]} />,
    );

    expect(marks().map((mark) => mark.name)).toEqual([
      'Death at 00:04.000, reported by counter-strike-2',
      'Kill at 00:11.000, reported by counter-strike-2',
    ]);
    expect(marks()[0]?.at).toBeCloseTo((4 / 24) * 100, 6);
    expect(marks()[1]?.at).toBeCloseTo((11 / 24) * 100, 6);
  });

  it('draws a kind this build has never met, named with the tag it arrived with', () => {
    render(
      <EditorScreen
        clip={storedDocument()}
        events={[
          event('kill', 34),
          event('objective_taken', 35),
          event('acme-cs2.flag_captured', 36, 'acme-cs2'),
        ]}
      />,
    );

    expect(marks().map((mark) => mark.name)).toEqual([
      'Kill at 00:04.000, reported by counter-strike-2',
      'objective_taken at 00:05.000, reported by counter-strike-2',
      'acme-cs2.flag_captured at 00:06.000, reported by acme-cs2',
    ]);
    // The kinds this build knows come first, in the vocabulary's own order;
    // the rest follow in alphabetical order rather than in the order the
    // session happened to produce them.
    expect(filters().map((filter) => filter.label)).toEqual([
      'Kill (1)',
      'acme-cs2.flag_captured (1)',
      'objective_taken (1)',
    ]);
  });

  it('counts the events this clip trimmed out rather than moving them to a cut', () => {
    // 50s of the recording is in neither of the two parts this clip uses.
    render(
      <EditorScreen clip={storedDocument()} events={[event('kill', 34), event('kill', 50)]} />,
    );

    expect(marks()).toHaveLength(1);
    expect(screen.getByText(/One event of these recordings is not on this clip/)).toBeVisible();
    expect(screen.getByText(/not drawn at the nearest cut/)).toBeVisible();
  });

  it('seeks the playhead to a mark when it is clicked', async () => {
    const user = userEvent.setup();
    render(<EditorScreen clip={storedDocument()} events={[event('kill', 95)]} />);

    await user.click(screen.getByRole('button', { name: /^Kill at 00:11.000/ }));

    expect(position()).toBe('00:11.000 of 00:24.000');
  });

  it('reaches every mark with the keyboard', async () => {
    const user = userEvent.setup();
    render(<EditorScreen clip={storedDocument()} events={[event('kill', 34)]} />);

    // Zoom out and Fit are disabled at the first zoom step, so the tab order
    // follows the screen: Zoom in, the kind toggle under it, then the mark.
    await user.tab();
    expect(document.activeElement).toHaveTextContent('Zoom in');
    await user.tab();
    expect(document.activeElement).toHaveTextContent('Kill (1)');
    await user.tab();
    expect(document.activeElement).toHaveAttribute(
      'aria-label',
      'Kill at 00:04.000, reported by counter-strike-2',
    );

    await user.keyboard('{Enter}');
    expect(position()).toBe('00:04.000 of 00:24.000');
  });
});

describe('the event filter', () => {
  /** The screen with two kills, one death and one kind nobody knows. */
  function renderWithEvents(): void {
    render(
      <EditorScreen
        clip={storedDocument()}
        events={[event('kill', 34), event('kill', 95), event('death', 35), event('objective', 36)]}
      />,
    );
  }

  it('offers one toggle per kind on the clip, counted, known kinds first', () => {
    renderWithEvents();

    expect(filters()).toEqual([
      { label: 'Kill (2)', pressed: 'true' },
      { label: 'Death (1)', pressed: 'true' },
      { label: 'objective (1)', pressed: 'true' },
    ]);
    expect(marks()).toHaveLength(4);
  });

  it('hides a kind when its toggle is pressed, and says so in words', async () => {
    const user = userEvent.setup();
    renderWithEvents();

    await user.click(screen.getByRole('button', { name: /^Kill \(2\)/ }));

    expect(marks().map((mark) => mark.name)).toEqual([
      'Death at 00:05.000, reported by counter-strike-2',
      'objective at 00:06.000, reported by counter-strike-2',
    ]);
    // Said rather than only drawn: a pressed state told by a fill alone is a
    // state told by colour alone (AGENTS.md section 46).
    expect(filters()[0]).toEqual({ label: 'Kill (2) hidden', pressed: 'false' });
    expect(screen.getByText('2 shown')).toBeVisible();
  });

  it('is worked entirely from the keyboard', async () => {
    const user = userEvent.setup();
    renderWithEvents();

    // Past Zoom in, which is the first control the screen offers.
    await user.tab();
    await user.tab();
    expect(document.activeElement).toHaveTextContent('Kill (2)');

    await user.keyboard(' ');
    expect(filters()[0]?.pressed).toBe('false');
    expect(marks()).toHaveLength(2);

    await user.tab();
    await user.keyboard('{Enter}');
    expect(filters()[1]?.pressed).toBe('false');
    expect(marks()).toHaveLength(1);

    // And back: hiding is reversible, so nothing the filter does is destructive.
    await user.keyboard('{Enter}');
    expect(filters()[1]?.pressed).toBe('true');
    expect(marks()).toHaveLength(2);
  });

  it('keeps the count of what was found, not of what is showing', async () => {
    const user = userEvent.setup();
    renderWithEvents();

    await user.click(screen.getByRole('button', { name: /^Kill \(2\)/ }));

    // A filter that renumbered itself as things were hidden would leave nobody
    // able to tell how many there are.
    expect(filters()[0]?.label).toBe('Kill (2) hidden');
  });
});
