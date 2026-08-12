import { cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it } from 'vitest';

import { storedDocument } from '../test/editDocumentFixture';
import { EditorScreen } from './EditorScreen';

/**
 * The Editor screen's contract, as tests.
 *
 * Three properties, and each of them is the kind that rots quietly:
 *
 * - **nothing is invented.** With no clip the screen says so and draws no
 *   timeline; with a clip it draws what the document says and marks what it
 *   cannot get — the picture and the waveforms — as absent rather than as
 *   empty (AGENTS.md section 27).
 * - **the timeline is operable from the keyboard**, which for a timeline is the
 *   whole of AGENTS.md section 46: an editor that needs precise dragging to put
 *   a playhead on a cut is one that cannot be used without a steady hand.
 *   Every case below drives real keys at the real element rather than calling a
 *   handler.
 * - **a clip that will not open says why.** Each refusal is the model's own
 *   compatibility rule, at the layer somebody reads.
 *
 * The keyboard cases assert the *timecode the screen shows*, not the state
 * behind it, so a key that moved a variable nothing draws would fail.
 */

afterEach(() => {
  // Testing Library only registers its own teardown when Vitest's globals are
  // on, and they are off here.
  cleanup();
});

/** The playhead, which is a slider because that is what it is. */
function playhead(): HTMLElement {
  return screen.getByRole('slider', { name: 'Playhead' });
}

/** What the transport says the position is. */
function position(): string {
  return playhead().getAttribute('aria-valuetext') ?? '';
}

/**
 * What the screen says is at the playhead, as the values of its term list.
 *
 * Read as `definition` elements rather than by searching for a string, because
 * a recording's name appears on the timeline as well and a test that found
 * either would not be about the preview at all.
 */
function facts(): readonly string[] {
  return screen.getAllByRole('definition').map((value) => value.textContent ?? '');
}

/** The screen, with the three-segment fixture open. */
function renderWithClip(changes: Record<string, unknown> = {}): void {
  render(<EditorScreen clip={storedDocument(changes)} />);
}

describe('the Editor screen with nothing open', () => {
  it('says that no clip can be opened, and names the work that changes it', () => {
    render(<EditorScreen />);

    const panel = screen.getByRole('region', { name: 'Open clip' });
    expect(within(panel).getByRole('heading', { name: 'No clip is open' })).toBeVisible();
    expect(within(panel).getByText(/#306/)).toBeVisible();
  });

  it('draws no timeline at all rather than an empty one', () => {
    render(<EditorScreen />);

    expect(screen.queryByRole('slider')).toBeNull();
    expect(screen.queryByText('No waveform')).toBeNull();
    expect(screen.queryAllByRole('button')).toHaveLength(0);
  });

  it('lists what the editor will do against what has to exist first', () => {
    render(<EditorScreen />);

    const headers = screen.getAllByRole('columnheader').map((cell) => cell.textContent);
    expect(headers).toEqual(['What the editor will do', 'What has to exist first']);
    expect(screen.getByText(/Export the clip to a file/)).toBeVisible();
  });
});

describe('the Editor screen with a clip open', () => {
  it('draws one lane per audio track, named, with what each contributes', () => {
    renderWithClip();

    const tracks = within(screen.getByRole('list', { name: 'Tracks' })).getAllByRole('listitem');

    // The ruler's spacer is hidden from the accessibility tree, so what is left
    // is the video lane and one lane per audio track: SPEC.md section 19's
    // "audio should visually appear as individual editable tracks".
    expect(tracks.map((track) => track.textContent)).toEqual([
      'Video3 segments',
      'Game-3.0 dB',
      'MicrophoneMuted',
    ]);
  });

  it('says a muted track is muted in words rather than by colour alone', () => {
    renderWithClip();

    expect(screen.getByText('Muted')).toBeVisible();
  });

  it('says which track is silenced by a solo elsewhere', () => {
    renderWithClip({
      audio_tracks: [
        { name: 'Game', inputs: [{ source: 0, stream: 0 }], soloed: true },
        { name: 'Microphone', inputs: [{ source: 0, stream: 1 }] },
      ],
    });

    expect(screen.getByText('Silent while another track is soloed')).toBeVisible();
  });

  it('says a lane has no waveform rather than drawing a flat line', () => {
    renderWithClip();

    // `docs/waveforms.md` is explicit: a track that could not be read is left
    // out with its reason rather than included as a flat line, which is
    // indistinguishable from a silent track.
    expect(screen.getAllByText('No waveform')).toHaveLength(2);
    expect(screen.getByText(/peaks are computed from the recording/)).toBeVisible();
  });

  it('says there is no picture, rather than drawing an empty frame', () => {
    renderWithClip();

    expect(screen.getByText(/No picture/)).toBeVisible();
  });

  it('names the recording, the source time and the segment under the playhead', () => {
    renderWithClip();

    // Zero into the clip is thirty seconds into the recording, because that is
    // where the first segment's span starts: the whole of what output time and
    // source time being different things means, on screen.
    expect(facts()).toEqual([
      '00:00.000 of 00:24.000',
      'rec-2026-08-11-cs2',
      '00:30.000',
      '1 of 3, starting at 00:00.000',
      'Unchanged',
      'Round 12',
    ]);
  });

  it('shows the text that is over the picture at this moment, and nothing when there is none', async () => {
    const user = userEvent.setup();
    renderWithClip();
    playhead().focus();

    expect(facts()).toContain('Round 12');

    await user.keyboard('{End}');

    expect(facts()).not.toContain('Round 12');
    expect(facts()).toContain('None');
  });

  it('says the end of a clip has nothing under it, because every range is half-open', async () => {
    const user = userEvent.setup();
    renderWithClip();

    playhead().focus();
    await user.keyboard('{End}');

    expect(position()).toBe('00:24.000 of 00:24.000');
    expect(screen.getByText('The end of the clip. Nothing plays here.')).toBeVisible();
  });

  it('says an empty clip is empty rather than showing a broken one', () => {
    render(
      <EditorScreen
        clip={JSON.stringify({ schema_version: 1, title: 'Empty', sources: [], segments: [] })}
      />,
    );

    expect(screen.getByText(/This clip is empty/)).toBeVisible();
    expect(position()).toBe('00:00.000 of 00:00.000');
  });
});

describe('the playhead', () => {
  it('is reached by Tab and announces where it is', async () => {
    const user = userEvent.setup();
    renderWithClip();

    // Zoom out and Fit are both disabled at the first zoom step, so the tab
    // order is Export, Zoom in, and then the playhead itself. This is the same
    // order `docs/desktop-ui.md` sets out and `EditorEvents.test.tsx` asserts
    // whole, seen on a clip with no events: the kind filters and the marks that
    // would sit between Zoom in and the playhead are simply absent here.
    await user.tab();
    expect(document.activeElement).toHaveTextContent('Export…');

    await user.tab();
    expect(document.activeElement).toHaveTextContent('Zoom in');

    await user.tab();
    expect(document.activeElement).toBe(playhead());
    expect(position()).toBe('00:00.000 of 00:24.000');
    expect(playhead()).toHaveAttribute('aria-valuemax', '24000');
  });

  it('steps by a tenth of a second, and by a second with Shift', async () => {
    const user = userEvent.setup();
    renderWithClip();
    playhead().focus();

    await user.keyboard('{ArrowRight}{ArrowRight}');
    expect(position()).toBe('00:00.200 of 00:24.000');

    await user.keyboard('{Shift>}{ArrowRight}{/Shift}');
    expect(position()).toBe('00:01.200 of 00:24.000');

    await user.keyboard('{ArrowLeft}');
    expect(position()).toBe('00:01.100 of 00:24.000');
  });

  it('does not run off either end of the clip', async () => {
    const user = userEvent.setup();
    renderWithClip();
    playhead().focus();

    await user.keyboard('{ArrowLeft}{ArrowLeft}');
    expect(position()).toBe('00:00.000 of 00:24.000');

    await user.keyboard('{End}{ArrowRight}');
    expect(position()).toBe('00:24.000 of 00:24.000');
  });

  /*
   * The one that makes a keyboard as accurate as a mouse rather than merely
   * possible: a cut is at eight seconds and at twenty, and Page Down lands
   * exactly on one. Nobody can drag to a nanosecond, and every operation
   * #84 built is about a boundary.
   */
  it('lands exactly on a cut with Page Down, and back with Page Up', async () => {
    const user = userEvent.setup();
    renderWithClip();
    playhead().focus();

    await user.keyboard('{PageDown}');
    expect(position()).toBe('00:08.000 of 00:24.000');

    await user.keyboard('{PageDown}');
    expect(position()).toBe('00:20.000 of 00:24.000');

    await user.keyboard('{PageUp}');
    expect(position()).toBe('00:08.000 of 00:24.000');

    await user.keyboard('{Home}');
    expect(position()).toBe('00:00.000 of 00:24.000');
  });

  it('shows the material at the moment it moved to, not at the one it left', async () => {
    const user = userEvent.setup();
    renderWithClip();
    playhead().focus();

    await user.keyboard('{PageDown}');

    // Eight seconds in is where the first cut is: the material jumps from 38s
    // of the recording to 92s, which is the whole point of a cut.
    expect(facts()).toContain('01:32.000');
    expect(facts()).toContain('2 of 3, starting at 00:08.000');
  });

  it('zooms with the keyboard as well as with the buttons', async () => {
    const user = userEvent.setup();
    renderWithClip();
    playhead().focus();

    await user.keyboard('+');
    expect(screen.getByText('Zoom 2×')).toBeVisible();

    await user.keyboard('+');
    expect(screen.getByText('Zoom 4×')).toBeVisible();

    await user.keyboard('-');
    expect(screen.getByText('Zoom 2×')).toBeVisible();

    await user.keyboard('0');
    expect(screen.getByText('Zoom 1×')).toBeVisible();
  });

  it('offers zoom as controls too, disabled at the ends where they would do nothing', async () => {
    const user = userEvent.setup();
    renderWithClip();

    // The three zoom controls and Export are the only controls on the screen.
    // Both are things this component can actually perform: the zoom is how the
    // timeline is drawn, and Export opens a dialog, which is this component's
    // own state. Every other action of an editor is somebody else's ticket, and
    // a button that could not reach it would be a button with nothing behind
    // it — which is also why the dialog Export opens has no Export button of
    // its own (issue #322).
    expect(screen.getAllByRole('button').map((button) => button.textContent)).toEqual([
      'Export…',
      'Zoom out',
      'Zoom in',
      'Fit',
    ]);
    expect(screen.getByRole('button', { name: 'Zoom out' })).toBeDisabled();

    await user.click(screen.getByRole('button', { name: 'Zoom in' }));

    expect(screen.getByText('Zoom 2×')).toBeVisible();
    expect(screen.getByRole('button', { name: 'Zoom out' })).toBeEnabled();
  });
});

describe('a clip that will not open', () => {
  const REFUSED: readonly (readonly [string, string, RegExp])[] = [
    ['is not JSON', '{', /not valid JSON/],
    [
      'was written by a newer Clipped',
      JSON.stringify({ schema_version: 9 }),
      /Update Clipped to open it/,
    ],
    [
      'carries a field this build does not understand',
      storedDocument({ tone_map: 'hlg' }),
      /does not understand/,
    ],
    [
      'has a segment with no length',
      storedDocument({
        segments: [
          {
            source: 0,
            span: { start: 0, end: 1_000_000_000 },
            speed: { numerator: 1, denominator: 0 },
          },
        ],
      }),
      /segments has no length|no length/,
    ],
  ];

  it.each(REFUSED)('says why when it %s', (_what, clip, expected) => {
    render(<EditorScreen clip={clip} />);

    const panel = screen.getByRole('region', { name: 'Open clip' });
    expect(
      within(panel).getByRole('heading', { name: 'This clip cannot be opened' }),
    ).toBeVisible();
    expect(within(panel).getByText(expected)).toBeVisible();
    expect(screen.queryByRole('slider')).toBeNull();
  });

  it('says the clip and its recordings are untouched, because that is the promise', () => {
    render(<EditorScreen clip="{" />);

    expect(screen.getByText(/left exactly as it was, and so is every recording/)).toBeVisible();
  });
});
