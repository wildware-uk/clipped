import { act, cleanup, render, screen, waitFor, within } from '@testing-library/react';
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
 * The other half is the player (issue #304), and what these cases assert about
 * it is what leaves the window rather than what is drawn over it: which file
 * was opened, which track was asked for, and that a recording nothing can play
 * gets a sentence instead of a transport. A media element in jsdom loads
 * nothing at all, so anything asserted about a picture would be an assertion
 * about jsdom — the address the element is pointed at is the real claim.
 */

/** What the Tauri host answers for a recording with three sound tracks. */
function opened(url: string, audioTrack: number) {
  return {
    url,
    audio_track: audioTrack,
    audio_tracks: [
      { index: 1, name: 'Compatibility Mix', default: true },
      { index: 2, name: 'Game', default: false },
      { index: 3, name: 'Microphone', default: false },
    ],
    prepared: audioTrack !== 1,
  };
}

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
      features: [],
      status: { state: 'idle' },
    });
    openClip('r-99');

    const panel = screen.getByRole('region', { name: 'Recording' });
    expect(within(panel).getByRole('heading', { level: 2 })).toHaveTextContent(
      'Not known to this window',
    );
    // The distinction the whole screen turns on. Nothing here has been to the
    // disk, so "the file has gone" is a claim it has no standing to make; the
    // library index is what looks, and this screen has not looked in it
    // (AGENTS.md section 27, issues #56 and #52).
    expect(within(panel).getByText(/#52/)).toBeVisible();
    expect(panel.textContent).not.toMatch(/missing|deleted|no such recording/i);
  });

  it('draws no player for a recording it has not been told the file of', () => {
    stubRecorderLinkRuntime({ link: 'connecting' });
    openClip('r-99');

    const main = screen.getByRole('main');

    // A media element with nothing it may load, a transport that would drive
    // nothing, and a scrubber that would imply a duration nobody measured. The
    // player appears when there is a file to point it at and not before
    // (AGENTS.md section 27).
    expect(main.querySelectorAll('video, audio, source, track')).toHaveLength(0);
    expect(main.querySelectorAll('img, canvas')).toHaveLength(0);
    expect(within(main).queryAllByRole('button')).toHaveLength(0);
    expect(within(main).queryAllByRole('slider')).toHaveLength(0);
    expect(within(main).queryAllByRole('combobox')).toHaveLength(0);
  });

  it('opens the file a killed recorder left, and plays what came back', async () => {
    // The whole round trip: the screen asks the host for the file, the host
    // answers with an address, and the element is pointed at that address. It
    // is never pointed at a path — this window cannot load one
    // (`playbackReach.test.ts`).
    const runtime = stubRecorderLinkRuntime({ link: 'connecting' }, null, {
      openPlayback: () => opened('http://clip.localhost/1', 1),
    });
    openClip('r-7');

    runtime.emit(INTERRUPTION);

    const main = screen.getByRole('main');
    const player = await waitFor(() => {
      const found = main.querySelector('video');
      expect(found).not.toBeNull();
      return found as HTMLVideoElement;
    });

    expect(player.getAttribute('src')).toBe('http://clip.localhost/1');
    expect(player.controls).toBe(true);
    expect(
      runtime.invocations.filter((invocation) => invocation.command === 'open_playback'),
    ).toContainEqual({
      command: 'open_playback',
      args: { source: 'D:\\clips\\2026-08-11 cs2.mkv', audioTrack: undefined },
    });
  });

  it('asks the recorder for the track that was chosen, and plays what it answers', async () => {
    // The criterion the whole design turns on: a media element cannot choose an
    // audio track, so choosing one is a different file. A screen that only
    // moved a highlight would satisfy a test about the button and go on playing
    // the same sound.
    const user = userEvent.setup();
    const runtime = stubRecorderLinkRuntime({ link: 'connecting' }, null, {
      openPlayback: (args) => {
        const track = args['audioTrack'] === undefined ? 1 : Number(args['audioTrack']);
        return opened(`http://clip.localhost/${String(track)}`, track);
      },
    });
    openClip('r-7');
    runtime.emit(INTERRUPTION);

    const main = screen.getByRole('main');
    await waitFor(() => {
      expect(main.querySelector('video')).not.toBeNull();
    });

    // Named, because "Audio 2" in a row of three is a control nobody can tell
    // from the others (AGENTS.md section 46).
    const microphone = within(main).getByRole('button', { name: 'Microphone' });
    expect(within(main).getByRole('button', { name: 'Compatibility Mix' })).toHaveAttribute(
      'aria-pressed',
      'true',
    );

    await user.click(microphone);

    await waitFor(() => {
      expect(main.querySelector('video')?.getAttribute('src')).toBe('http://clip.localhost/3');
    });
    expect(microphone).toHaveAttribute('aria-pressed', 'true');
    expect(
      runtime.invocations.filter((invocation) => invocation.command === 'open_playback'),
    ).toContainEqual({
      command: 'open_playback',
      args: { source: 'D:\\clips\\2026-08-11 cs2.mkv', audioTrack: 3 },
    });
  });

  it('keeps the position when another track is chosen', async () => {
    // Choosing a track is a different file, so the element starts at zero.
    // Somebody four minutes into a match who wants to hear the microphone
    // asked for the microphone, not to start again — and a player that jumped
    // back to the beginning would make the selector unusable for the thing it
    // is for.
    const user = userEvent.setup();
    const runtime = stubRecorderLinkRuntime({ link: 'connecting' }, null, {
      openPlayback: (args) => {
        const track = args['audioTrack'] === undefined ? 1 : Number(args['audioTrack']);
        return opened(`http://clip.localhost/${String(track)}`, track);
      },
    });
    openClip('r-7');
    runtime.emit(INTERRUPTION);

    const main = screen.getByRole('main');
    const player = await waitFor(() => {
      const found = main.querySelector('video');
      expect(found).not.toBeNull();
      return found as HTMLVideoElement;
    });

    // jsdom's media element has no timeline of its own, so the position is
    // stood up here — a real one is what the element measures while it plays,
    // and what the screen does with it is the whole of what this asserts.
    let position = 0;
    Object.defineProperty(player, 'currentTime', {
      configurable: true,
      get: () => position,
      set: (value: number) => {
        position = value;
      },
    });
    position = 254.5;

    await user.click(within(main).getByRole('button', { name: 'Microphone' }));
    await waitFor(() => {
      expect(main.querySelector('video')?.getAttribute('src')).toBe('http://clip.localhost/3');
    });

    // The new file is loaded, and it starts at the beginning like any other.
    position = 0;
    act(() => {
      player.dispatchEvent(new Event('loadedmetadata'));
    });

    expect(position).toBe(254.5);
  });

  it('says a recording whose file has gone has gone, rather than drawing a player', async () => {
    // Issue #304's fourth criterion, in the window. The sentence is the
    // recorder's own — it named the file and said what probably happened to it
    // — and there is no transport above it (AGENTS.md sections 27 and 45).
    const runtime = stubRecorderLinkRuntime({ link: 'connecting' }, null, {
      openPlayback: () => {
        throw {
          code: 'playback_failed',
          message:
            '2026-08-11 cs2.mkv is not there any more. It may have been moved or deleted, or the drive it is on may not be connected.',
        };
      },
    });
    openClip('r-7');
    runtime.emit(INTERRUPTION);

    const main = screen.getByRole('main');
    await waitFor(() => {
      expect(within(main).getByText(/not there any more/i)).toBeVisible();
    });
    expect(main.querySelectorAll('video')).toHaveLength(0);
  });

  it('offers no player for a recording that is still being written', async () => {
    // Its container has no trailer yet, so there is nothing whose length a
    // transport could describe — and the recorder is still appending to it.
    const runtime = stubRecorderLinkRuntime({ link: 'connecting' }, null, {
      openPlayback: () => opened('http://clip.localhost/1', 1),
    });
    openClip('r-3');

    runtime.emit({
      event: 'state',
      link: 'attached',
      recorder_process_id: 91,
      features: [],
      status: {
        state: 'recording',
        recording_id: 'r-3',
        output: 'D:\\clips\\r-3.mkv',
        target: 'process cs2.exe',
        elapsed_ms: 5_000,
      },
    });

    const main = screen.getByRole('main');
    await waitFor(() => {
      expect(
        within(main).getByRole('heading', { level: 2, name: 'Being recorded now' }),
      ).toBeVisible();
    });
    expect(main.querySelectorAll('video')).toHaveLength(0);
    expect(
      runtime.invocations.some((invocation) => invocation.command === 'open_playback'),
      'nothing should be opened for a file another process is still writing',
    ).toBe(false);
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
      features: [],
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
      features: [],
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
    // library index that would offer the rest is read by the Library screen
    // since #301, and looking one up from here is issue #52.
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

  it('says what it still does not do, and names the issue for each', () => {
    stubRecorderLinkRuntime({ link: 'connecting' });
    openClip('r-7');

    const main = screen.getByRole('main');
    // What is left after the player: a poster frame nothing has ever drawn and
    // a waveform that is a file this window has no route to. Each row names the
    // work, which is the contract every unbuilt row on every screen keeps.
    expect(within(main).getAllByText(/poster frame/i).length).toBeGreaterThan(0);
    expect(within(main).getAllByText(/waveform/i).length).toBeGreaterThan(0);
    expect(within(main).getAllByText(/#\d+/).length).toBeGreaterThan(0);
    // And nothing on the screen claims a player is still to come.
    expect(main.textContent).not.toMatch(/cannot play a Clipped recording/i);
  });
});
