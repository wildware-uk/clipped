import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { StrictMode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { App } from './App';
import { stubRecorderLinkRuntime } from './test/recorderLinkRuntime';

/**
 * The clip playback screen's contract, as tests (issue #52).
 *
 * Driven through `<App />` rather than by rendering the screen beside itself,
 * for the reason `GamesScreen.test.tsx` gives: the property is that the screen
 * follows the recorder rather than restating a sentence somebody typed, and a
 * screen whose wording is a constant looks identical to one that is following
 * the link. So these cases open the real application at a real address and then
 * move the link underneath it.
 *
 * The other half is absence, and it is the half that would rot: there is no
 * player, no transport, no scrubber and no poster frame, because this window
 * cannot play a Clipped recording at all (`clipPlayback.ts`, issue #304). A
 * scrubber is the one that matters most — it implies a duration, and nothing
 * here has measured one.
 */

/** An interruption the supervisor would emit, naming the file it left. */
const INTERRUPTION = {
  event: 'recording_interrupted',
  recording_id: 'r-7',
  output: 'D:\\clips\\2026-08-11 cs2.mkv',
  target: 'process cs2.exe',
  elapsed_ms: 42_000,
} as const;

/** Mounts what `main.tsx` mounts, StrictMode and all. */
function renderApp(): void {
  render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}

/** Opens the application at a recording's playback screen. */
function openClip(recordingId: string): void {
  window.location.hash = `#/clip/${recordingId}`;
  renderApp();
}

describe('the clip playback screen', () => {
  beforeEach(() => {
    window.location.hash = '';
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    window.location.hash = '';
  });

  it('says it has no way to look a recording up, rather than that it is missing', () => {
    stubRecorderLinkRuntime({
      link: 'attached',
      recorder_process_id: 91,
      status: { state: 'idle' },
    });
    openClip('r-99');

    const panel = screen.getByRole('region', { name: 'Recording' });
    expect(within(panel).getByRole('heading', { level: 2 })).toHaveTextContent(
      'Not known to this window',
    );
    // The distinction the whole screen turns on. Nothing here has been to the
    // disk, so "the file has gone" is a claim it has no standing to make; the
    // library index is what looks, and it cannot be reached (AGENTS.md section
    // 27, issues #56 and #305).
    expect(within(panel).getByText(/#305/)).toBeVisible();
    expect(panel.textContent).not.toMatch(/missing|deleted|no such recording/i);
  });

  it('draws no player, no transport and no poster frame', () => {
    stubRecorderLinkRuntime({ link: 'connecting' });
    openClip('r-99');

    const main = screen.getByRole('main');

    // A media element with nothing it may load, a transport that would drive
    // nothing, and a scrubber that would imply a duration nobody measured.
    expect(main.querySelectorAll('video, audio, source, track')).toHaveLength(0);
    expect(main.querySelectorAll('img, canvas')).toHaveLength(0);
    expect(within(main).queryAllByRole('button')).toHaveLength(0);
    expect(within(main).queryAllByRole('slider')).toHaveLength(0);
    expect(within(main).queryAllByRole('combobox')).toHaveLength(0);
  });

  it('names the file a killed recorder left, and what the recorder measured about it', async () => {
    const runtime = stubRecorderLinkRuntime({ link: 'connecting' });
    openClip('r-7');

    runtime.emit(INTERRUPTION);

    const main = screen.getByRole('main');
    await waitFor(() => {
      expect(within(main).getByRole('heading', { level: 2, name: 'Interrupted' })).toBeVisible();
    });
    expect(within(main).getByText('D:\\clips\\2026-08-11 cs2.mkv')).toBeVisible();
    expect(within(main).getByText('process cs2.exe')).toBeVisible();
    // 42 seconds, as minutes and seconds - and said as a lower bound, because
    // it is the last elapsed time the link heard rather than the length of a
    // file nothing has opened.
    expect(within(main).getByText('00:42')).toBeVisible();
    expect(within(main).getByText(/lower bound/i)).toBeVisible();
  });

  it('follows the recorder rather than restating a sentence', async () => {
    // The same address, three different answers, with nothing touched but the
    // link. A screen that had the wording baked in would pass the case above
    // and fail this one.
    const runtime = stubRecorderLinkRuntime({ link: 'connecting' });
    openClip('r-3');

    const panel = screen.getByRole('region', { name: 'Recording' });
    await waitFor(() => {
      expect(within(panel).getByRole('heading', { level: 2 })).toHaveTextContent(
        'Not known to this window',
      );
    });

    runtime.emit({
      event: 'state',
      link: 'attached',
      recorder_process_id: 91,
      status: {
        state: 'recording',
        recording_id: 'r-3',
        output: 'D:\\clips\\r-3.mkv',
        target: 'process cs2.exe',
        elapsed_ms: 5_000,
      },
    });

    await waitFor(() => {
      expect(within(panel).getByRole('heading', { level: 2 })).toHaveTextContent(
        'Being recorded now',
      );
    });

    runtime.emit({
      event: 'state',
      link: 'attached',
      recorder_process_id: 91,
      status: { state: 'idle' },
    });

    await waitFor(() => {
      expect(within(panel).getByRole('heading', { level: 2 })).toHaveTextContent(
        'Not known to this window',
      );
    });
  });

  it('is reached from the notice that named the interrupted recording', async () => {
    // The only way in that exists. The window follows one recorder, so the
    // recording a recorder died writing is the only one it can name; the
    // library index that would offer the rest is issue #305.
    const user = userEvent.setup();
    const runtime = stubRecorderLinkRuntime({ link: 'connecting' });
    renderApp();

    runtime.emit(INTERRUPTION);

    const status = screen.getByRole('region', { name: 'Recorder status' });
    const link = await waitFor(() => within(status).getByRole('link', { name: /open this/i }));

    await user.click(link);

    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Playback');
    expect(
      within(screen.getByRole('main')).getByRole('heading', { level: 2, name: 'Interrupted' }),
    ).toBeVisible();
  });

  it('names the screen in the window title, like every other screen', () => {
    stubRecorderLinkRuntime({ link: 'connecting' });
    openClip('r-7');

    expect(document.title).toBe('Clipped — Playback');
  });

  it('says what has to land before it can play anything, and names the issues', () => {
    stubRecorderLinkRuntime({ link: 'connecting' });
    openClip('r-7');

    const main = screen.getByRole('main');
    // The four blocking facts, and the work that answers them. This is the
    // whole of what the screen offers in place of a player.
    expect(within(main).getByText(/cannot load a file from the disk/i)).toBeVisible();
    expect(within(main).getByText(/Matroska, and WebView2 does not demux it/i)).toBeVisible();
    expect(within(main).getByText(/uncompressed PCM/i)).toBeVisible();
    expect(within(main).getByText(/cannot choose an audio track/i)).toBeVisible();
    expect(within(main).getAllByText(/#304/).length).toBeGreaterThan(0);
  });
});
