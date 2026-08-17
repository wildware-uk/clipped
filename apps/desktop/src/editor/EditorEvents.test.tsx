import { cleanup, render, screen, within } from '@testing-library/react';
import userEvent, { type UserEvent } from '@testing-library/user-event';
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

/**
 * How the focused element reads, for the tab-order assertions.
 *
 * A mark carries its whole name in `aria-label` and has no text at all, so the
 * label is preferred; every other control is read by its words.
 */
function focusedName(): string {
  const element = document.activeElement;
  if (element === null || element === document.body) {
    return '(none)';
  }
  return element.getAttribute('aria-label') ?? element.textContent ?? '(unnamed)';
}

/**
 * Every stop in the editor's tab order, in order.
 *
 * Asserted **whole** rather than probed one stop at a time. This component is
 * where two features' controls meet — the export dialog's button (#90) and this
 * issue's filters and marks — and the order between them is a decision recorded
 * in `docs/desktop-ui.md`, not a consequence of which branch merged last. A
 * control added to the editor without a place in that order changes this list
 * and fails here, which is the point of comparing the list rather than hunting
 * for one element in it.
 */
async function tabOrder(user: UserEvent): Promise<readonly string[]> {
  const stops: string[] = [];
  // Tab wraps back through the body at the end, which is the terminator. The
  // cap only means a focus trap fails as a wrong list rather than as a test
  // that never returns.
  for (let stop = 0; stop < 32; stop += 1) {
    await user.tab();
    if (document.activeElement === null || document.activeElement === document.body) {
      break;
    }
    stops.push(focusedName());
  }
  return stops;
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
    ).toEqual(['Video3 segments', 'GameSolo-3.0 dB', 'MicrophoneSoloMuted']);
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

  it('puts every control of the editor in one documented order', async () => {
    const user = userEvent.setup();
    render(
      <EditorScreen
        clip={storedDocument()}
        events={[event('kill', 34), event('kill', 95), event('death', 35)]}
      />,
    );

    /*
     * The whole order, top to bottom, which is the order the screen is drawn
     * in:
     *
     * - **Export first.** It is drawn first, in the header beside the clip's
     *   title, and focus order follows visual order. Sending the whole-document
     *   action to the end would make a keyboard user's focus jump from the
     *   bottom of the screen back to the top.
     * - then the timeline's own controls, outermost inwards: the zoom, the kind
     *   filters, each track's Solo button, the marks themselves.
     * - **the playhead last**, because it is the innermost thing and the one a
     *   user stays on: the arrow keys, Home, End and Page Up/Down all work from
     *   there, so it is where a keyboard user wants to be left.
     *
     * Zoom out and Fit are disabled at the first zoom step, so neither is a tab
     * stop; that is why the zoom contributes one entry and not three. The
     * fixture has two audio tracks, "Game" and "Microphone", so Solo
     * contributes two.
     */
    expect(await tabOrder(user)).toEqual([
      'Export…',
      'Zoom in',
      'Kill (2)',
      'Death (1)',
      'Solo',
      'Solo',
      'Kill at 00:04.000, reported by counter-strike-2',
      'Death at 00:05.000, reported by counter-strike-2',
      'Kill at 00:11.000, reported by counter-strike-2',
      'Playhead',
    ]);
  });

  it('reaches every mark with the keyboard, and seeks with Enter', async () => {
    const user = userEvent.setup();
    render(<EditorScreen clip={storedDocument()} events={[event('kill', 34)]} />);

    // The sixth stop, by the order above: Export, Zoom in, the kind toggle,
    // the two Solo buttons, then the mark.
    await user.tab();
    await user.tab();
    await user.tab();
    await user.tab();
    await user.tab();
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

    // Past Export and Zoom in, which are the two controls drawn above the
    // filters; the whole order is asserted once, above.
    await user.tab();
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
