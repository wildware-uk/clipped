import type {
  LibraryRecording,
  LibrarySession,
  LibrarySessionPage,
  Preview,
} from '@clipped/shared';
import { act, cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { StrictMode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { App } from './App';
import { CONCURRENT_PREVIEWS } from './preview';
import { stubRecorderLinkRuntime, type StubbedRuntime } from './test/recorderLinkRuntime';

/**
 * A thumbnail in the library list (issue #448).
 *
 * The first time any build has drawn one, and the criteria the issue is shaped
 * around are the two these cases are about: that the picture reaching the window
 * is *that recording's* picture, and that a recording with no thumbnail **yet**
 * is distinguishable from one whose thumbnail could not be read. The second is
 * the one a careless screen gets wrong by drawing both as an empty square, so
 * every case below asserts on what a screen reader would be told as well as on
 * what is drawn.
 *
 * The whole application is rendered, in `StrictMode`, and driven through the
 * sidebar — the house style in `LibraryScreen.test.tsx`. StrictMode is not
 * decoration here either: it mounts every row twice, and a client that asked the
 * recorder once per mount would double a library's worth of round trips.
 */

/** A thumbnail that is here, carrying these bytes. */
function ready(bytes: string): Preview {
  return {
    kind: 'thumbnail',
    state: 'ready',
    tracks: [],
    picture: {
      media_type: 'image/jpeg',
      bytes,
      width: 640,
      height: 360,
      at_seconds: 1.1,
      blank: false,
    },
  };
}

/** One recording of a sitting. */
function recording(id: number, path: string): LibraryRecording {
  return {
    recording_id: id,
    session_index: id,
    path,
    started_at: '2026-08-11T20:14:00+01:00',
    duration_seconds: 600,
    size_bytes: 100_000,
    favourite: false,
    tags: [],
  };
}

/** A sitting holding these recordings. */
function session(recordings: readonly LibraryRecording[]): LibrarySession {
  return {
    session_id: 'cs2-20260811-201400',
    game_id: 'cs2',
    game_name: 'Counter-Strike 2',
    started_at: '2026-08-11T20:14:00+01:00',
    favourite: false,
    recordings,
    clips: [],
  };
}

/** One page holding one sitting of these recordings. */
function page(recordings: readonly LibraryRecording[]): LibrarySessionPage {
  return { sessions: [session(recordings)] };
}

/**
 * A recorder that can serve previews, which is what makes the window ask.
 *
 * `features` is the handshake's own list, and `previews` is the name issue #448
 * added to it.
 */
const SERVES_PREVIEWS = {
  link: 'attached',
  recorder_process_id: 7,
  features: ['previews'],
  status: { state: 'idle' },
} as const;

/** A recorder from before issue #448: attached, and naming no such capability. */
const OLDER = {
  link: 'attached',
  recorder_process_id: 7,
  features: ['library'],
  status: { state: 'idle' },
} as const;

/** Mounts the application the way `main.tsx` does. */
function renderApp(): void {
  render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}

/** Opens the Library screen from the sidebar. */
async function openLibrary(user: ReturnType<typeof userEvent.setup>): Promise<void> {
  await user.click(screen.getByRole('link', { name: 'Library' }));
}

/**
 * Lets every settled promise run its handlers.
 *
 * A macrotask rather than `await Promise.resolve()`: answering a request runs a
 * `then` and then a `finally` that starts the next one, so a single microtask
 * tick stops half way down the chain and the case would report a queue that
 * never advanced.
 */
async function settle(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
}

/** Every `recording_preview` this window sent. */
function previewsAsked(runtime: StubbedRuntime): readonly Record<string, unknown>[] {
  return runtime.invocations
    .filter((invocation) => invocation.command === 'recording_preview')
    .map((invocation) => invocation.args);
}

describe('a thumbnail in the library list', () => {
  beforeEach(() => {
    window.location.hash = '';
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    window.location.hash = '';
  });

  /*
   * Issue #448's first acceptance criterion. The assertion is not that an
   * `<img>` exists — a component that drew the same picture on every row would
   * satisfy that — but that each row's `src` carries the bytes the recorder
   * answered *for that recording*, which is the one thing a mismatched request
   * and reply gets wrong.
   */
  it('draws each recording with its own picture', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime(SERVES_PREVIEWS, null, {
      sessions: () =>
        Promise.resolve(
          page([recording(1, 'D:\\clips\\first.mkv'), recording(2, 'D:\\clips\\second.mkv')]),
        ),
      preview: (args) => ready(args['source'] === 'D:\\clips\\first.mkv' ? 'Zmlyc3Q=' : 'c2Vjb25k'),
    });
    renderApp();
    await openLibrary(user);

    const first = await screen.findByRole('img', {
      name: 'Thumbnail of first.mkv, Counter-Strike 2',
    });
    const second = await screen.findByRole('img', {
      name: 'Thumbnail of second.mkv, Counter-Strike 2',
    });

    expect(first).toHaveAttribute('src', 'data:image/jpeg;base64,Zmlyc3Q=');
    expect(second).toHaveAttribute('src', 'data:image/jpeg;base64,c2Vjb25k');
  });

  /*
   * Issue #448's second acceptance criterion, and the load-bearing one: three
   * recordings in the three states the protocol has, drawn three different ways.
   * A component that drew a blank tile for `pending` and `unavailable` alike
   * passes every other case in this file and fails this one.
   */
  it('tells a thumbnail that is not made yet from one there will never be', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime(SERVES_PREVIEWS, null, {
      sessions: () =>
        Promise.resolve(
          page([
            recording(1, 'D:\\clips\\done.mkv'),
            recording(2, 'D:\\clips\\new.mkv'),
            recording(3, 'D:\\clips\\gone.mkv'),
          ]),
        ),
      preview: (args): Preview => {
        if (args['source'] === 'D:\\clips\\done.mkv') {
          return ready('Zmlyc3Q=');
        }
        return args['source'] === 'D:\\clips\\new.mkv'
          ? { kind: 'thumbnail', state: 'pending', tracks: [] }
          : {
              kind: 'thumbnail',
              state: 'unavailable',
              tracks: [],
              reason: 'gone.mkv holds no video stream.',
            };
      },
    });
    renderApp();
    await openLibrary(user);

    const drawn = await screen.findByRole('img', {
      name: 'Thumbnail of done.mkv, Counter-Strike 2',
    });
    const notYet = await screen.findByRole('img', {
      name: 'No thumbnail yet for new.mkv, Counter-Strike 2. Clipped is making one.',
    });
    const never = await screen.findByRole('img', {
      name: 'No thumbnail for gone.mkv, Counter-Strike 2. gone.mkv holds no video stream.',
    });

    // Three different names is half of it; the other half is that a person
    // looking at the screen can tell them apart too.
    expect(drawn).toHaveAttribute('src');
    expect(notYet).not.toHaveAttribute('src');
    expect(never).not.toHaveAttribute('src');
    expect(notYet).toHaveTextContent('Not made yet');
    expect(never).toHaveTextContent('No picture');
    expect(notYet.textContent).not.toBe(never.textContent);
  });

  /*
   * A round trip that failed is a fourth thing, and not one of the protocol's
   * three: the library was read — that is where the row came from — and one
   * picture could not be. It says so in the recorder's own words rather than
   * being drawn as "there is not one" (AGENTS.md section 15).
   */
  it('says a thumbnail that could not be asked for could not be asked for', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime(SERVES_PREVIEWS, null, {
      sessions: () => Promise.resolve(page([recording(1, 'D:\\clips\\first.mkv')])),
      preview: () => {
        throw { code: 'recorder_unreachable', message: 'the recorder went away.' };
      },
    });
    renderApp();
    await openLibrary(user);

    expect(
      await screen.findByRole('img', {
        name: 'That thumbnail could not be read: first.mkv, Counter-Strike 2. Clipped could not ask the recorder for that thumbnail. the recorder went away.',
      }),
    ).toHaveTextContent('Not read');
  });

  /*
   * The round trip asks for the file the row was drawn for, and asks it once.
   * `kind` is on the wire because the same command serves waveforms, and
   * `buckets` goes over as `null` because the Rust side declares an `Option` and
   * an argument left off the object is one Tauri never sees.
   */
  it('asks for the recording it drew the row for, once', async () => {
    const user = userEvent.setup();
    const runtime = stubRecorderLinkRuntime(SERVES_PREVIEWS, null, {
      sessions: () => Promise.resolve(page([recording(1, 'D:\\clips\\first.mkv')])),
      preview: () => ready('Zmlyc3Q='),
    });
    renderApp();
    await openLibrary(user);
    await screen.findByRole('img', { name: 'Thumbnail of first.mkv, Counter-Strike 2' });

    // Once, not twice: StrictMode mounts every row twice, and two rows drawing
    // the same file share the one round trip rather than racing to make two.
    expect(previewsAsked(runtime)).toEqual([
      { source: 'D:\\clips\\first.mkv', kind: 'thumbnail', buckets: null },
    ]);
  });

  /*
   * The gate, and the reason it is a gate rather than a refusal handled per row.
   * A recorder from before issue #448 has no `open_preview`; asking anyway would
   * be one refused round trip for every recording on the page, which is issue
   * #447's complaint arriving twenty-five times at once.
   */
  it('asks a recorder that does not advertise previews for nothing at all', async () => {
    const user = userEvent.setup();
    const runtime = stubRecorderLinkRuntime(OLDER, null, {
      sessions: () => Promise.resolve(page([recording(1, 'D:\\clips\\first.mkv')])),
      // Deliberately answerable. If the window asks in spite of the handshake,
      // this case has to fail on the asking rather than on the refusal.
      preview: () => ready('Zmlyc3Q='),
    });
    renderApp();
    await openLibrary(user);

    const table = await screen.findByRole('table', { name: 'Sessions' });
    await waitFor(() => {
      expect(within(table).getByRole('button', { name: /^Play / })).toBeInTheDocument();
    });

    expect(previewsAsked(runtime)).toEqual([]);
    expect(within(table).queryAllByRole('img')).toEqual([]);
  });

  /*
   * How many round trips are on the wire at once, which is a bound the recorder
   * imposes rather than a preference: `RecorderLink::call` opens a fresh pipe
   * connection per command and the recorder serves eight of them
   * (`MAX_CONCURRENT_CONNECTIONS`) between everything this window does. A page
   * of rows each asking the instant it mounts is the first thing this window has
   * ever done that could exceed that, and the failure is not confined to the
   * tiles — it takes the answer out of whatever else was being asked.
   *
   * The peak is what is asserted, not the total: a client with no bound sends
   * all twelve and still ends up with twelve invocations recorded, exactly as a
   * bounded one does.
   */
  it('keeps a bounded number of round trips on the wire while a page fills in', async () => {
    const user = userEvent.setup();
    /** Every answer this case is holding back, so it can let them go at the end. */
    const held: ((preview: Preview) => void)[] = [];
    const runtime = stubRecorderLinkRuntime(SERVES_PREVIEWS, null, {
      sessions: () =>
        Promise.resolve(
          page(
            Array.from({ length: 12 }, (_unused, index) =>
              recording(index, `D:\\clips\\${String(index)}.mkv`),
            ),
          ),
        ),
      preview: () =>
        new Promise<Preview>((resolve) => {
          held.push(resolve);
        }),
    });
    renderApp();
    await openLibrary(user);

    const table = await screen.findByRole('table', { name: 'Sessions' });
    await waitFor(() => {
      expect(within(table).getAllByRole('img')).toHaveLength(12);
    });

    // Nothing has been answered, so every request ever sent is still in flight.
    // Fewer than the twelve rows first, and only then equal to the constant:
    // asserting against the constant alone would go on passing after somebody
    // raised it to a number that is not a bound at all.
    const onTheWire = previewsAsked(runtime).length;
    expect(onTheWire).toBeLessThan(12);
    expect(onTheWire).toBeGreaterThan(0);
    expect(onTheWire).toBe(CONCURRENT_PREVIEWS);

    // Letting one go frees exactly one slot, which the next waiting request
    // takes: the bound is a queue, not a cap that drops what does not fit into
    // it. Without this the case would also pass against a client that sent three
    // and abandoned the other nine.
    const sentFirst = previewsAsked(runtime).length;
    await act(async () => {
      held.shift()?.(ready('Zmlyc3Q='));
      await settle();
    });
    expect(previewsAsked(runtime)).toHaveLength(sentFirst + 1);

    // Then the rest, a slotful at a time, so that this case leaves nothing of
    // its own on the wire for the next one — the queue is module state, and is
    // meant to be, because the bound is on the process rather than on a screen.
    for (let round = 0; round < 12 && held.length > 0; round += 1) {
      const outstanding = held.splice(0);
      // One round at a time on purpose: each round is what lets the next slotful
      // be sent, so running them together would answer requests that have not
      // left yet.
      await act(async () => {
        for (const answer of outstanding) {
          answer(ready('Zmlyc3Q='));
        }
        await settle();
      });
    }
    expect(previewsAsked(runtime)).toHaveLength(12);
  });
});
