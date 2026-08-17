import { cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it } from 'vitest';

import { storedDocument } from '../test/editDocumentFixture';
import { EditorScreen } from './EditorScreen';

/**
 * The export dialog's contract, as tests (issue #90).
 *
 * Three properties, and every one of them is about honesty rather than about
 * layout:
 *
 * - **it says what an export of *this* clip would be.** The fixture clip joins
 *   two recordings, sums two streams into one track, mutes another and draws
 *   text over the picture: four reasons an export is refused today, and the
 *   dialog has to name all four with the specifics out of the document, not a
 *   general apology (AGENTS.md sections 28 and 45). A clip with none of them
 *   has to be told it would be a fast copy.
 * - **it offers nothing the engine cannot honour.** Resolution, framerate,
 *   codec and quality are properties of a re-encode that does not exist, so a
 *   control for any of them would be one that silently does nothing (AGENTS.md
 *   section 27). The cases below assert the absence by substance — no control
 *   of any kind but Close, and none of the four words anywhere on it.
 * - **it invents no figure.** There is no estimated size, because nothing has
 *   measured one (#323), and no progress bar, because nothing can start an
 *   export (#322).
 *
 * The dialog is reached the way a person reaches it — the editor's Export
 * button — rather than rendered on its own, so the case covers the control as
 * well as the dialog.
 */

afterEach(() => {
  // Testing Library only registers its own teardown when Vitest's globals are
  // on, and they are off here.
  cleanup();
});

/** A clip of one recording with no mix and no text: the copy case. */
const COPYABLE: Record<string, unknown> = {
  sources: [{ id: 0, recording: 'rec-2026-08-11-cs2' }],
  segments: [
    {
      source: 0,
      span: { start: 30_000_000_000, end: 38_000_000_000 },
      speed: { numerator: 1, denominator: 1 },
      crop: null,
      rotation: 'none',
    },
  ],
  audio_tracks: [],
  overlays: [],
};

/** Opens the dialog from the editor, with the fixture clip open. */
async function openExport(changes: Record<string, unknown> = {}): Promise<HTMLElement> {
  const user = userEvent.setup();
  render(<EditorScreen clip={storedDocument(changes)} />);
  await user.click(screen.getByRole('button', { name: 'Export…' }));
  return screen.getByRole('dialog');
}

describe('the export dialog', () => {
  it('opens from the editor and closes back to the control that opened it', async () => {
    const user = userEvent.setup();
    render(<EditorScreen clip={storedDocument()} />);
    const opener = screen.getByRole('button', { name: 'Export…' });

    await user.click(opener);
    expect(screen.getByRole('dialog')).toHaveAccessibleName('Export clip');

    await user.click(screen.getByRole('button', { name: 'Close' }));
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(opener).toHaveFocus();
  });

  it('names the clip, its length and how many segments it has', async () => {
    const dialog = await openExport();

    expect(within(dialog).getByText(/Round 12 ace/)).toHaveTextContent(
      'Round 12 ace — 00:24.000 in 3 segments.',
    );
  });

  it('names every edit that rules out an export, with the specifics', async () => {
    // The fixture clip's four: two recordings, a track summing two streams, a
    // muted track, and one overlay — the same four `crates/export` would
    // report, in the order it reports them.
    const dialog = await openExport();
    const what = within(dialog).getByRole('region', { name: 'What an export would do' });

    expect(
      within(what).getByRole('heading', { name: 'Clipped cannot export this clip yet' }),
    ).toBeVisible();
    expect(
      within(what)
        .getAllByRole('listitem')
        .map((item) => item.textContent),
    ).toEqual([
      expect.stringContaining('This clip joins 2 recordings.'),
      expect.stringContaining('The “Game” track sums 2 recorded streams.'),
      expect.stringContaining('The “Microphone” track is muted.'),
      expect.stringContaining('1 piece of text is drawn over the picture.'),
    ]);
  });

  it('says what to change about each of them, rather than only what is wrong', async () => {
    // AGENTS.md section 45: a failure has to leave somebody with something to
    // do. Each reason carries the edit to undo to make the clip copyable.
    const dialog = await openExport();
    const items = within(dialog)
      .getAllByRole('listitem')
      .map((item) => item.textContent ?? '');

    expect(items.filter((item) => /copied|copies|export/i.test(item))).toHaveLength(items.length);
  });

  it('does not claim a clip it cannot export would be a fast copy', async () => {
    const dialog = await openExport();

    expect(within(dialog).queryByText(/rules out a fast copy/)).toBeNull();
  });

  it('says a clip with none of them would be a fast lossless copy', async () => {
    const dialog = await openExport(COPYABLE);
    const what = within(dialog).getByRole('region', { name: 'What an export would do' });

    expect(
      within(what).getByRole('heading', { name: 'Nothing in this edit rules out a fast copy' }),
    ).toBeVisible();
    expect(within(what).getByText(/bit for bit/)).toBeVisible();
    // The promise the whole editor is built on, said where an export is asked
    // for (AGENTS.md sections 56 and 57).
    expect(within(what).getByText(/never modified, moved or re-encoded/)).toBeVisible();
  });

  it('names what deciding a copy still needs the recording for, rather than promising one', async () => {
    // Half the plan needs the file, which this window cannot open (#322). A
    // dialog that said "this will be copied" without it would be promising
    // something the engine may refuse.
    const dialog = await openExport(COPYABLE);
    const checks = within(dialog)
      .getAllByRole('listitem')
      .map((item) => item.textContent ?? '');

    expect(checks.some((check) => /keyframe/.test(check))).toBe(true);
    expect(checks.some((check) => /codecs/.test(check))).toBe(true);
    expect(within(dialog).getByText(/cannot open \(issue #322\)/)).toBeVisible();
  });

  it.each([COPYABLE, {}])(
    'offers no control the engine could not honour, and no way to start an export',
    async (changes) => {
      const dialog = await openExport(changes);

      // Close is the only control: an Export button would fail, and every
      // setting the deck draws is a property of a re-encode that does not
      // exist. Asserted as "no control of any other kind" rather than as four
      // absent labels, so a quality slider added later fails here.
      expect(
        within(dialog)
          .getAllByRole('button')
          .map((button) => button.textContent),
      ).toEqual(['Close']);
      for (const role of ['combobox', 'radio', 'slider', 'checkbox', 'textbox', 'listbox']) {
        expect(within(dialog).queryAllByRole(role)).toHaveLength(0);
      }
      // "codec" is deliberately not in this list: the dialog does say that
      // whether the recording's codecs can be described decides a copy, which
      // is a fact about the recording rather than something to choose.
      expect(dialog.textContent ?? '').not.toMatch(
        /resolution|frame ?rate|bitrate|quality|preset/i,
      );
    },
  );

  it.each([COPYABLE, {}])('shows no size it has not measured, and no progress', async (changes) => {
    const dialog = await openExport(changes);

    // #323 is what makes a size answerable. Until then a figure here would be
    // one nobody measured (AGENTS.md section 27) — and it is the figure
    // somebody decides whether they have room for.
    expect(dialog.textContent ?? '').not.toMatch(/\d[\d.,]*\s*(?:[KMGT]i?B|bytes)\b/i);
    expect(within(dialog).queryByRole('progressbar')).toBeNull();
  });

  it('says an export cannot be started from this window, and names the issue', async () => {
    const dialog = await openExport(COPYABLE);

    const panel = within(dialog).getByRole('region', { name: 'Starting an export' });
    expect(
      within(panel).getByRole('heading', { name: 'No export can be started here yet' }),
    ).toBeVisible();
    expect(within(panel).getByText(/Issue #322/)).toBeVisible();
  });

  it('reports a clip the engine would refuse to plan rather than saying it can be copied', async () => {
    const dialog = await openExport({
      ...COPYABLE,
      audio_tracks: [
        {
          name: 'Game',
          inputs: [],
          gain_db: 0,
          muted: false,
          fade_in: 0,
          fade_out: 0,
        },
      ],
    });

    expect(
      within(dialog).getByRole('heading', { name: 'This clip cannot be exported as it stands' }),
    ).toBeVisible();
    expect(within(dialog).queryByText(/rules out a fast copy/)).toBeNull();
  });
});
