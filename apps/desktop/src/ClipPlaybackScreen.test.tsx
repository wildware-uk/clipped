import type { LibrarySession } from '@clipped/shared';
import { act, cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { StrictMode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { App } from './App';
import { SAMPLE_LANE, SAMPLE_MARKS } from './test/eventMarkFixture';
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

/** A recorder this window is talking to, advertising nothing in particular. */
const ATTACHED = {
  link: 'attached',
  recorder_process_id: 7,
  features: [],
  status: { state: 'idle' },
} as const;

/** The same recorder, built after issue #448 and able to answer for a picture. */
const ATTACHED_WITH_PREVIEWS = { ...ATTACHED, features: ['previews'] } as const;

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
    // What is left after the player, the poster and the peaks: a playhead, marks
    // on the waveform, and a waveform that follows the chosen track. Each row
    // names the work, which is the contract every unbuilt row on every screen
    // keeps.
    expect(within(main).getAllByText(/#\d+/).length).toBeGreaterThan(0);
    expect(within(main).getAllByText(/playhead/i).length).toBeGreaterThan(0);
    // And nothing on the screen claims a player, a poster frame or a waveform
    // is still to come. Issue #448 built the last two, and a row still naming
    // one would be a screen apologising for something it is doing.
    expect(main.textContent).not.toMatch(/cannot play a Clipped recording/i);
    expect(within(main).queryAllByText(/A poster frame before playback starts/i)).toHaveLength(0);
    expect(within(main).queryAllByText(/nothing has ever drawn one/i)).toHaveLength(0);
  });

  it('draws the recorder\u2019s own thumbnail as the poster frame, and the peaks under it', async () => {
    // Issue #448 on this screen: the picture and the peaks reach the window over
    // one command, and the element is handed the picture itself rather than an
    // address for it. A `poster` built from anything else \u2014 a file name, a
    // number on the `clip` scheme \u2014 is a picture this window cannot load, and
    // jsdom would not notice either, so the assertion is on what the attribute
    // actually says.
    const runtime = stubRecorderLinkRuntime(ATTACHED_WITH_PREVIEWS, null, {
      openPlayback: () => opened('http://clip.localhost/1', 1),
      preview: (args) =>
        args['kind'] === 'thumbnail'
          ? {
              kind: 'thumbnail',
              state: 'ready',
              tracks: [],
              picture: {
                media_type: 'image/jpeg',
                bytes: 'Zm9v',
                width: 640,
                height: 360,
                at_seconds: 12.5,
                blank: false,
              },
            }
          : {
              kind: 'waveform',
              state: 'ready',
              tracks: [
                {
                  index: 1,
                  name: 'Game',
                  sample_rate: 48000,
                  channels: 2,
                  duration_seconds: 0.04,
                  peaks: [-127, 127, -10, 10],
                },
              ],
            },
    });
    openClip('r-7');
    runtime.emit(INTERRUPTION);

    const main = screen.getByRole('main');
    await waitFor(() => {
      expect(main.querySelector('video')?.getAttribute('poster')).toBe(
        'data:image/jpeg;base64,Zm9v',
      );
    });
    expect(
      await within(main).findByRole('img', { name: /Sound of Game in 2026-08-11 cs2\.mkv/ }),
    ).toBeInTheDocument();

    // Both asked about the recording the player was pointed at, and the
    // waveform asked at a width it can draw. A screen that asked about the
    // wrong file would draw another recording's picture over this one, and
    // nothing in jsdom would report it.
    expect(runtime.invocations).toContainEqual({
      command: 'recording_preview',
      args: { source: 'D:\\clips\\2026-08-11 cs2.mkv', kind: 'thumbnail', buckets: null },
    });
    expect(runtime.invocations).toContainEqual({
      command: 'recording_preview',
      args: { source: 'D:\\clips\\2026-08-11 cs2.mkv', kind: 'waveform', buckets: 1200 },
    });
  });

  it('asks a recorder that cannot answer for neither a poster nor peaks', async () => {
    // A recorder from before issue #448 refuses `open_preview` by name. The
    // window checks the capability first, so the element keeps its own first
    // frame and nothing is asked \u2014 rather than two refusals arriving after the
    // event for a picture nobody was going to see.
    const runtime = stubRecorderLinkRuntime(ATTACHED, null, {
      openPlayback: () => opened('http://clip.localhost/1', 1),
    });
    openClip('r-7');
    runtime.emit(INTERRUPTION);

    const main = screen.getByRole('main');
    await waitFor(() => {
      expect(main.querySelector('video')).not.toBeNull();
    });

    expect(
      runtime.invocations.filter((invocation) => invocation.command === 'recording_preview'),
    ).toHaveLength(0);
    expect(main.querySelector('video')?.hasAttribute('poster')).toBe(false);
  });
});

/**
 * The recording's timeline, and the marks on it (issue #65).
 *
 * These are the whole path, and they are here rather than beside the component
 * because the path is the claim: the Library hands a row over, the address
 * carries the *index's own* key, `library_events` is asked with that key, the
 * marks come back, and pressing one moves the element. Every one of those five
 * is a place this could be wrong in a way that compiles.
 *
 * The marks are the recorder's own exemplar out of `protocol-schema.json`, so a
 * field renamed in `crates/ipc/src/library.rs` fails here.
 *
 * A media element in jsdom loads nothing, so its `duration` is `NaN` and it
 * never fires `loadedmetadata`. Both are stood up by hand below - it is the one
 * thing in these cases that is not the real platform, and without it there is
 * nothing to place a mark against. What is asserted around it is real:
 * `currentTime` is a property jsdom keeps, so a seek that landed in the wrong
 * place is a seek these cases can see.
 */
describe('the marks on a recording', () => {
  beforeEach(() => {
    window.location.hash = '';
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    window.location.hash = '';
  });

  /** The sitting the Library draws, holding the recording the exemplar is about. */
  function session(): LibrarySession {
    return {
      session_id: 'cs2-20260811-201400',
      game_id: 'cs2',
      game_name: 'Counter-Strike 2',
      started_at: '2026-08-11T20:14:00+01:00',
      favourite: false,
      recordings: [
        {
          // The exemplar's marks are on this recording, and the Library puts
          // this number in the address when Play is pressed.
          recording_id: Number(SAMPLE_MARKS[0]?.recording ?? 0),
          session_index: 1,
          path: 'D:\\clips\\cs2-20260811-201400-1.mkv',
          started_at: '2026-08-11T20:14:00+01:00',
          favourite: false,
          tags: [],
        },
      ],
      clips: [],
    };
  }

  /** The three answers a recording with marks on it needs. */
  function answering() {
    return stubRecorderLinkRuntime(ATTACHED, null, {
      sessions: () => Promise.resolve({ sessions: [session()] }),
      openPlayback: () => opened('http://clip.localhost/1', 1),
      events: () => Promise.resolve(SAMPLE_LANE),
    });
  }

  /**
   * Opens the Library, presses Play, and gives back the player.
   *
   * The real way in. A screen opened at its address directly has no row and no
   * library key, which is a case of its own below.
   */
  async function playFromLibrary() {
    const user = userEvent.setup();
    renderApp();
    await user.click(screen.getByRole('link', { name: 'Library' }));
    await user.click(await screen.findByRole('button', { name: /^Play / }));

    const main = screen.getByRole('main');
    const player = await waitFor(() => {
      const found = main.querySelector('video');
      expect(found).not.toBeNull();
      return found as HTMLVideoElement;
    });
    return { user, main, player };
  }

  /** Tells the element how long the recording is, as a real one would. */
  function reportLength(player: HTMLVideoElement, seconds: number): void {
    Object.defineProperty(player, 'duration', { configurable: true, value: seconds });
    act(() => {
      player.dispatchEvent(new Event('loadedmetadata'));
    });
  }

  it('asks the library for the marks on the recording the Library handed over', async () => {
    const runtime = answering();

    await playFromLibrary();

    // The index's own key, as a string, and nothing else: the recorder parses
    // it as an `i64` before it opens the database.
    await waitFor(() => {
      expect(runtime.invocations).toContainEqual({
        command: 'library_events',
        args: { recording: SAMPLE_MARKS[0]?.recording },
      });
    });
  });

  it('moves the player to the mark that was pressed, and not to the start', async () => {
    answering();

    const { user, main, player } = await playFromLibrary();
    reportLength(player, 60);

    const lane = await within(main).findByRole('list', { name: /^Marks on / });
    const markers = within(lane).getAllByRole('button');
    expect(markers).toHaveLength(SAMPLE_MARKS.length);

    /*
     * The second mark, not the first: a build that seeks to zero, and a build
     * that always seeks to the earliest mark, both pass a case that presses the
     * first one.
     */
    const second = markers[1];
    const expected = (SAMPLE_MARKS[1]?.at ?? 0) / 1_000_000_000;

    await user.click(second!);

    expect(
      player.currentTime,
      `pressing "${second?.getAttribute('aria-label') ?? ''}" should put the player at ${String(expected)}s`,
    ).toBe(expected);
    expect(player.currentTime, 'and not at the start of the recording').not.toBe(0);
  });

  it('says who reported each mark, in words, without relying on a colour', async () => {
    answering();

    const { main, player } = await playFromLibrary();
    reportLength(player, 60);

    const lane = await within(main).findByRole('list', { name: /^Marks on / });
    for (const marker of within(lane).getAllByRole('button')) {
      expect(marker).toHaveAccessibleName(/reported by the .+ plugin$/);
    }
    expect(within(main).getByRole('list', { name: 'What the marks are' })).toBeVisible();
  });

  it('asks nothing, and says why, for a recording the library did not name', async () => {
    // An interrupted recording is named by the identifier the *recorder* gave
    // it, which is not the key the index holds it under. Sending it would spend
    // a round trip to be told the parameters were invalid, and would put "your
    // library could not be read" on a screen whose library is perfectly well.
    const runtime = stubRecorderLinkRuntime(ATTACHED, null, {
      openPlayback: () => opened('http://clip.localhost/1', 1),
    });
    openClip('r-7');
    runtime.emit(INTERRUPTION);

    const main = screen.getByRole('main');
    await waitFor(() => {
      expect(main.querySelector('video')).not.toBeNull();
    });

    expect(
      runtime.invocations.filter((invocation) => invocation.command === 'library_events'),
    ).toHaveLength(0);
    expect(within(main).getByText(/not the one the library indexes it under/)).toBeVisible();
    expect(within(main).queryByRole('list', { name: /^Marks on / })).toBeNull();
  });

  /**
   * The screen's keyboard shortcuts (SPEC.md section 42, issue #52).
   *
   * `<video controls>` answers these keys already — but only while the transport
   * has focus, which is exactly when nobody needs a shortcut. What was missing is
   * the rest of the screen: after pressing a track button or a mark, space did
   * nothing.
   *
   * These cases are about the wiring rather than the mapping. `playback.test.ts`
   * covers which key means what, on a pure function; what could still be wrong
   * after that is a handler bound to the wrong thing, or one that never reaches
   * the element — which is a tested function that does nothing.
   */
  describe('the playback screen’s keyboard shortcuts', () => {
    afterEach(() => {
      cleanup();
      vi.unstubAllGlobals();
      window.location.hash = '';
    });

    it('seeks with the arrow keys from anywhere on the screen', async () => {
      answering();

      const { user, player } = await playFromLibrary();
      reportLength(player, 60);
      player.currentTime = 10;

      // Focus deliberately not on the transport: that case is the element's own
      // and worked before this.
      await user.keyboard('{ArrowRight}');
      expect(player.currentTime).toBe(15);

      await user.keyboard('{ArrowLeft}');
      expect(player.currentTime).toBe(10);

      await user.keyboard('{Shift>}{ArrowRight}{/Shift}');
      expect(player.currentTime).toBe(11);
    });

    it('goes to the start and the end', async () => {
      answering();

      const { user, player } = await playFromLibrary();
      reportLength(player, 60);
      player.currentTime = 30;

      await user.keyboard('{End}');
      expect(player.currentTime).toBe(60);

      await user.keyboard('{Home}');
      expect(player.currentTime).toBe(0);
    });

    /*
     * A seek past either end is a seek to the end, not an exception and not a
     * position outside the recording. The clamp is the element's own length,
     * which is the only measurement of it this window has.
     */
    it('does not seek past either end of the recording', async () => {
      answering();

      const { user, player } = await playFromLibrary();
      reportLength(player, 8);
      player.currentTime = 6;

      await user.keyboard('{ArrowRight}');
      expect(player.currentTime).toBe(8);

      player.currentTime = 2;
      await user.keyboard('{ArrowLeft}');
      expect(player.currentTime).toBe(0);
    });

    /*
     * The trade this screen must not make: a screen-wide space that plays the
     * recording, bought by a focused track button no longer being pressable by
     * keyboard, is a worse screen than one with no shortcuts.
     */
    it('leaves space to a button that has focus', async () => {
      answering();

      const { user, main, player } = await playFromLibrary();
      reportLength(player, 60);
      player.currentTime = 20;

      const lane = await within(main).findByRole('list', { name: /^Marks on / });
      const marker = within(lane).getAllByRole('button')[1];
      marker?.focus();
      const wherePressingItPutUs = (SAMPLE_MARKS[1]?.at ?? 0) / 1_000_000_000;

      await user.keyboard(' ');

      // The button was activated, which is what space on a focused button does.
      expect(player.currentTime).toBe(wherePressingItPutUs);
    });

    /*
     * `Ctrl+Left` is a word jump and `Alt+Left` is Back. A screen that swallowed
     * them would break navigation to add a seek nobody asked for.
     */
    it('leaves a modified arrow alone', async () => {
      answering();

      const { user, player } = await playFromLibrary();
      reportLength(player, 60);
      player.currentTime = 10;

      await user.keyboard('{Control>}{ArrowRight}{/Control}');
      await user.keyboard('{Alt>}{ArrowRight}{/Alt}');

      expect(player.currentTime).toBe(10);
    });
  });
});
