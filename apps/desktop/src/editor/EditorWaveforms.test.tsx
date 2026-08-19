// @vitest-environment jsdom
import type { PreviewTrack } from '@clipped/shared';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { EditorFor } from './EditorRoute';
import { EditorScreen } from './EditorScreen';
import type { PeaksOf } from './lanePeaks';
import { STORED_CLIP, SAMPLE_CLIP } from '../test/clipDocumentFixture';
import { storedDocument } from '../test/editDocumentFixture';
import { stubRecorderLinkRuntime } from '../test/recorderLinkRuntime';
import type { RecorderLinkState } from '../useRecorderLink';

/**
 * The waveforms under the Editor's audio lanes (issue #66).
 *
 * `lanePeaks.test.ts` is where the arithmetic is held to account — which part
 * of which recording each piece draws — because jsdom lays nothing out and a
 * rendered case cannot see the shape of a path. What this file asserts is the
 * half that is only true on a screen:
 *
 * - a lane draws **something**, and what it draws is **named by the range** it
 *   came from, so a picture is traceable to seconds of a recording by somebody
 *   who cannot see it;
 * - the three states of a preview are three different sentences, and none of
 *   them is a line through the middle of the lane (`docs/waveforms.md`);
 * - the round trip is real: opening a clip asks the recorder for the peaks of
 *   every recording that clip draws on.
 *
 * The document is `crates/edit`'s own three-segment clip — eight seconds from
 * 30s of one recording, twelve from 92s of it, four from 5s of a second — which
 * is a clip trimmed from the middle *and* a clip cut from two files.
 */

afterEach(() => {
  // `globals: false`, so Testing Library registers no automatic cleanup.
  cleanup();
  vi.unstubAllGlobals();
});

/** The fixture's two recordings, as the document names them. */
const RECORDING_A = 'rec-2026-08-11-cs2';
const RECORDING_B = 'rec-2026-08-11-cs2-b';

/** A track of `buckets` buckets spread over `seconds`, loud throughout. */
function loud(index: number, buckets: number, seconds: number, name: string): PreviewTrack {
  return {
    index,
    name,
    sample_rate: 48_000,
    channels: 2,
    duration_seconds: seconds,
    peaks: Array.from({ length: buckets * 2 }, (_, at) => (at % 2 === 0 ? -90 : 90)),
  };
}

/** What the recorder says about both recordings: two minutes and one. */
const READY: PeaksOf = (recording) => ({
  state: 'answered',
  preview: {
    kind: 'waveform',
    state: 'ready',
    tracks:
      recording === RECORDING_A
        ? [loud(0, 120, 120, 'Game'), loud(1, 120, 120, 'Microphone')]
        : [loud(0, 60, 60, 'Game')],
  },
});

/** The editor with the fixture clip open, and `waveforms` as its answer. */
function open(waveforms: PeaksOf | null): void {
  render(<EditorScreen clip={storedDocument()} waveforms={waveforms} />);
}

/** Every waveform picture on the screen, by the name a screen reader reads. */
function pictures(): readonly string[] {
  return screen.queryAllByRole('img').map((picture) => picture.getAttribute('aria-label') ?? '');
}

/** Every `<path>` anywhere on the screen, which a flat line would be one of. */
function paths(): number {
  return document.querySelectorAll('path').length;
}

describe('the peaks under the editor’s lanes', () => {
  it('draws one picture per segment, named by the part of the recording it is', () => {
    // The assertion the whole ticket turns on. "A waveform is drawn" is true of
    // a build that put the *whole* recording under a clip trimmed from its
    // middle; these names are not, because they say which seconds of which file
    // each picture is - and that is also what a screen reader gets, which is
    // the only thing it can get from a waveform.
    open(READY);

    expect(pictures()).toEqual([
      `Game from 00:30.000 to 00:38.000 of ${RECORDING_A}`,
      `Game from 01:32.000 to 01:44.000 of ${RECORDING_A}`,
      `Game from 00:05.000 to 00:09.000 of ${RECORDING_B}`,
      `Microphone from 00:30.000 to 00:38.000 of ${RECORDING_A}`,
      `Microphone from 01:32.000 to 01:44.000 of ${RECORDING_A}`,
    ]);
  });

  it('says a lane takes nothing from a recording rather than leaving it blank', () => {
    // The Microphone track lists an input for the first recording and none for
    // the second, so it has five seconds of nothing at the end - which is the
    // exported track being silent, not a waveform that failed to arrive.
    open(READY);

    expect(screen.getByText('Not in this recording')).toBeVisible();
    expect(screen.queryByText('No waveform')).toBeNull();
  });

  it('draws the peaks at the segment’s own place on the clip', () => {
    // Eight seconds, twelve and four of a twenty-four second clip. A picture in
    // the wrong place is a waveform that does not line up with the cut somebody
    // is about to make.
    open(READY);

    expect(
      screen
        .getAllByRole('img')
        .slice(0, 3)
        .map((picture) => picture.getAttribute('style')),
    ).toEqual([
      'left: 0%; width: 33.33333333333333%;',
      'left: 33.33333333333333%; width: 50%;',
      'left: 83.33333333333334%; width: 16.666666666666664%;',
    ]);
  });
});

describe('a lane with no peaks to draw', () => {
  it('says a waveform is still being made, and draws no line at all', () => {
    // The ordinary state of a recording written a minute ago. A flat line here
    // would be indistinguishable from a silent track, which `docs/waveforms.md`
    // forbids by name - so the lane says it in words and draws nothing.
    open(() => ({
      state: 'answered',
      preview: { kind: 'waveform', state: 'pending', tracks: [] },
    }));

    expect(screen.getAllByText('No waveform yet').length).toBeGreaterThan(0);
    expect(pictures()).toEqual([]);
    expect(paths()).toBe(0);
    expect(screen.getByText(/still reading the sound/)).toBeVisible();
  });

  it('says why there will not be one, and does not say it is coming', () => {
    open(() => ({
      state: 'answered',
      preview: {
        kind: 'waveform',
        state: 'unavailable',
        tracks: [],
        reason: 'that file is in a codec this build has no decoder for',
      },
    }));

    expect(screen.getAllByText('No waveform').length).toBeGreaterThan(0);
    expect(screen.queryByText('No waveform yet')).toBeNull();
    expect(screen.getByText(/no decoder for/)).toBeVisible();
    // The trap this ticket names third: a flat line would satisfy any case that
    // only looked for a picture.
    expect(paths()).toBe(0);
  });

  it('tells a recorder that cannot make waveforms from one that has none yet', () => {
    // A recorder built before issue #448 has no `open_preview`, so nothing was
    // asked of it. The remedy is an up-to-date Clipped rather than a different
    // clip, which is what the note says (AGENTS.md section 45).
    open(() => ({ state: 'unasked' }));

    expect(screen.getByText(/older than this window and cannot make waveforms/)).toBeVisible();
    expect(screen.queryByText('No waveform yet')).toBeNull();
    expect(paths()).toBe(0);
  });

  it('draws nothing while the answer is on its way, rather than a label that flickers', () => {
    open(() => ({ state: 'asking' }));

    expect(screen.queryByText('No waveform')).toBeNull();
    expect(screen.queryByText('No waveform yet')).toBeNull();
    expect(paths()).toBe(0);
  });

  it('says nobody asked when nobody did, which is not one of the four answers', () => {
    // No lookup at all: a caller with no recorder to ask. One statement for the
    // lane, because "nothing was asked" is a fact about the whole of it.
    open(null);

    expect(screen.getAllByText('No waveform')).toHaveLength(2);
    expect(screen.getByText(/have not been asked for/)).toBeVisible();
    expect(paths()).toBe(0);
  });
});

describe('opening a clip asks for its recordings’ peaks', () => {
  /** A recorder this window is talking to, able to answer for a preview. */
  const ATTACHED: RecorderLinkState = {
    link: 'attached',
    recorder_process_id: 7,
    features: ['previews'],
    status: { state: 'idle' },
  };

  /** The three-segment fixture, delivered the way the recorder delivers one. */
  const CLIP = { ...STORED_CLIP, document: storedDocument() };

  it('asks the recorder for every recording the document draws on, once each', async () => {
    // The round trip, over the real command. Two sources and two round trips -
    // and a clip that used one recording twice would still be one, which is
    // what `recordingsIn` deduplicates for.
    const runtime = stubRecorderLinkRuntime({ link: 'connecting' }, null, {
      clipDocument: () => Promise.resolve(CLIP),
      preview: (args) => ({
        kind: 'waveform',
        state: 'ready',
        tracks:
          args['source'] === RECORDING_A
            ? [loud(0, 120, 120, 'Game'), loud(1, 120, 120, 'Microphone')]
            : [loud(0, 60, 60, 'Game')],
      }),
    });

    render(<EditorFor clip={SAMPLE_CLIP} link={ATTACHED} />);

    await waitFor(() => {
      expect(pictures().length).toBeGreaterThan(0);
    });
    expect(runtime.invocations.filter((call) => call.command === 'recording_preview')).toEqual([
      {
        command: 'recording_preview',
        args: { source: RECORDING_A, kind: 'waveform', buckets: 4096 },
      },
      {
        command: 'recording_preview',
        args: { source: RECORDING_B, kind: 'waveform', buckets: 4096 },
      },
    ]);
  });

  it('draws what the recorder answered, in the lanes, at the right ranges', async () => {
    // The assertion that a round trip happening is not the same as an answer
    // reaching the screen: a build that asked and dropped the reply would draw
    // an editor with empty lanes and pass every case above this one.
    stubRecorderLinkRuntime({ link: 'connecting' }, null, {
      clipDocument: () => Promise.resolve(CLIP),
      preview: (args) => ({
        kind: 'waveform',
        state: 'ready',
        tracks:
          args['source'] === RECORDING_A
            ? [loud(0, 120, 120, 'Game'), loud(1, 120, 120, 'Microphone')]
            : [loud(0, 60, 60, 'Game')],
      }),
    });

    render(<EditorFor clip={SAMPLE_CLIP} link={ATTACHED} />);

    await waitFor(() => {
      expect(pictures()).toEqual([
        `Game from 00:30.000 to 00:38.000 of ${RECORDING_A}`,
        `Game from 01:32.000 to 01:44.000 of ${RECORDING_A}`,
        `Game from 00:05.000 to 00:09.000 of ${RECORDING_B}`,
        `Microphone from 00:30.000 to 00:38.000 of ${RECORDING_A}`,
        `Microphone from 01:32.000 to 01:44.000 of ${RECORDING_A}`,
      ]);
    });
  });

  it('asks nothing of a recorder that cannot answer for a preview', async () => {
    // Feature-gated, like the poster on the playback screen: a recorder from
    // before issue #448 would refuse each of these by name, and a refusal per
    // recording is a round trip spent to be told so.
    const runtime = stubRecorderLinkRuntime({ link: 'connecting' }, null, {
      clipDocument: () => Promise.resolve(CLIP),
    });

    render(<EditorFor clip={SAMPLE_CLIP} link={{ ...ATTACHED, features: [] }} />);

    await screen.findByText(/older than this window and cannot make waveforms/);
    expect(runtime.invocations.filter((call) => call.command === 'recording_preview')).toEqual([]);
  });
});
