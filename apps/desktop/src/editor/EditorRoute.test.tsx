import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { EditorFor } from './EditorRoute';
import {
  SAMPLE_CLIP,
  STORED_CLIP,
  SYNTHESISED_CLIP,
  SUPERSEDING_SAVE,
  clipDocumentOf,
} from '../test/clipDocumentFixture';
import { stubRecorderLinkRuntime } from '../test/recorderLinkRuntime';

/**
 * Opening a real clip in the editor, over the real command (issue #306).
 *
 * Every fixture here is a frame the Rust build produced and recorded in
 * `protocol-schema.json` (`../test/clipDocumentFixture.ts`), so these tests
 * cannot drift from the protocol they are about — the PR #647 and #652
 * standard.
 *
 * # What each assertion is guarding against
 *
 * The trap this screen sets is a build that opens a clip and shows an *empty*
 * one. "The editor shows a clip" is true of an editor drawing nothing, so
 * nothing below asserts that: they assert the segments, the tracks and the
 * title that are **in the document the recorder sent**, so a build that lost
 * the document on the way fails naming what went missing rather than passing
 * with a dead playhead (AGENTS.md section 27).
 */

const RECORDER = { link: 'connected', ended: null };

afterEach(() => {
  // `globals: false` in the Vitest config means Testing Library registers no
  // automatic cleanup, so every file does its own (`ClipPlaybackScreen.test.tsx`
  // and `EditorScreen.test.tsx` the same way). Without it the second render in
  // a file finds the first one's editor still on the page.
  cleanup();
  vi.unstubAllGlobals();
});

/** The editor, asking the stubbed recorder for `clip`. */
function open(clip: string | null, commands: Parameters<typeof stubRecorderLinkRuntime>[2] = {}) {
  const runtime = stubRecorderLinkRuntime(RECORDER, null, commands);
  render(<EditorFor clip={clip} />);
  return runtime;
}

describe('opening a clip in the editor', () => {
  it('asks the recorder for the clip named in the address', async () => {
    const runtime = open(SAMPLE_CLIP, {
      clipDocument: (args) => Promise.resolve(clipDocumentOf(String(args['clip']))),
    });

    await screen.findByRole('list', { name: 'Tracks' });

    expect(runtime.invocations.filter((call) => call.command === 'library_clip_document')).toEqual([
      { command: 'library_clip_document', args: { clip: SAMPLE_CLIP } },
    ]);
  });

  it('draws what is in the document the recorder sent, not an empty one', async () => {
    // The assertion that matters. A build that answered with an empty document
    // would still render an editor; what it would not render is these lanes,
    // and this is the failure that says so.
    open(SAMPLE_CLIP, { clipDocument: () => Promise.resolve(STORED_CLIP) });

    const tracks = await screen.findByRole('list', { name: 'Tracks' });
    const lanes = within(tracks).getAllByRole('listitem');

    expect(lanes.length).toBeGreaterThan(0);
    expect(lanes[0]?.textContent).toContain('Video');
    expect(lanes[0]?.textContent).toContain('1 segment');
  });

  it('says nothing has been edited yet when the recorder built the document', async () => {
    // A saved replay. The window must be able to tell "this is your edit" from
    // "this is where your edit starts", because only one of them is stored.
    open(SAMPLE_CLIP, { clipDocument: () => Promise.resolve(SYNTHESISED_CLIP) });

    expect(await screen.findByText(/Nobody has edited this clip yet/)).toBeVisible();
  });

  it('does not say so for a clip whose document was stored', async () => {
    open(SAMPLE_CLIP, { clipDocument: () => Promise.resolve(STORED_CLIP) });

    await screen.findByRole('list', { name: 'Tracks' });
    expect(screen.queryByText(/Nobody has edited this clip yet/)).toBeNull();
  });

  it('says the stored text is older, and that saving keeps the original', async () => {
    open(SAMPLE_CLIP, {
      clipDocument: () => Promise.resolve({ ...STORED_CLIP, converted_from: 1 }),
    });

    const said = await screen.findByText(/saved by an older version of Clipped/);
    expect(said).toBeVisible();
    expect(said.textContent).toContain('keeps the original');
    expect(said.textContent).toContain('nothing has been changed');
  });

  it('says why a clip did not open, in the recorder’s words', async () => {
    open(SAMPLE_CLIP, {
      clipDocument: () =>
        Promise.reject({
          code: 'edit_unreadable',
          message:
            'clip 3 could not be opened: this edit was saved by a newer version of Clipped ' +
            '(format 3; this build reads up to 2). Update Clipped to open it. Nothing has been changed.',
        }),
    });

    const panel = await screen.findByRole('region', { name: 'Open clip' });
    expect(within(panel).getByText(/newer version of Clipped/)).toBeVisible();
    expect(within(panel).getByText(/left exactly as it was/)).toBeVisible();
    expect(screen.queryByRole('list', { name: 'Tracks' })).toBeNull();
  });

  it('tells a recorder that is too old from a clip that will not open', async () => {
    // `unknown_command` is a recorder built before this command existed. The
    // remedy is a Clipped that is up to date, not a different clip, and a
    // window that showed the raw refusal would send somebody looking at the
    // wrong thing.
    open(SAMPLE_CLIP, {
      clipDocument: () =>
        Promise.reject({
          code: 'unknown_command',
          message: 'this recorder has no `library_clip_document` command',
        }),
    });

    const panel = await screen.findByRole('region', { name: 'Open clip' });
    expect(within(panel).getByText(/Update Clipped/)).toBeVisible();
  });

  it('asks for nothing when no clip is named', async () => {
    const runtime = open(null);

    await screen.findByRole('heading', { name: 'No clip is open' });

    expect(runtime.invocations.filter((call) => call.command === 'library_clip_document')).toEqual(
      [],
    );
  });

  it('does not show one clip’s document while another is in flight', async () => {
    // The one wrong thing an editor can do with somebody's work: show them a
    // different clip's edit and let them save it over this one.
    let release: (value: unknown) => void = () => undefined;
    const runtime = stubRecorderLinkRuntime(RECORDER, null, {
      clipDocument: (args) =>
        String(args['clip']) === SAMPLE_CLIP
          ? Promise.resolve(STORED_CLIP)
          : new Promise((resolve) => {
              release = resolve;
            }),
    });
    const view = render(<EditorFor clip={SAMPLE_CLIP} />);
    await screen.findByRole('list', { name: 'Tracks' });

    view.rerender(<EditorFor clip="99" />);

    await waitFor(() => {
      expect(screen.queryByRole('list', { name: 'Tracks' })).toBeNull();
    });
    expect(screen.getByText(/Opening clip 99/)).toBeVisible();
    release(clipDocumentOf('99'));
    await screen.findByRole('list', { name: 'Tracks' });
    expect(
      runtime.invocations.filter((call) => call.command === 'library_clip_document'),
    ).toHaveLength(2);
  });
});

describe('the save exemplar', () => {
  it('carries the format of the text that was kept', () => {
    // Straight from the recorder's own frame: a save that replaced a format 1
    // document says so, which is how a window can tell somebody their original
    // is still there.
    expect(SUPERSEDING_SAVE.superseded).toBe(1);
    expect(SUPERSEDING_SAVE.clip).toBe(SAMPLE_CLIP);
  });
});
