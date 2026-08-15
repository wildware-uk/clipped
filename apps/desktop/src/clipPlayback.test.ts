import { describe, expect, it } from 'vitest';

import {
  clipPath,
  describeClip,
  formatElapsed,
  isClipPath,
  MISSING,
  PLAYBACK_BLOCKERS,
  recordingOf,
  resolveClip,
} from './clipPlayback';
import type { InterruptedRecording, RecorderLinkView } from './useRecorderLink';

/**
 * What the playback screen may say about a recording.
 *
 * The property under all of this is that the screen can only describe a
 * recording the recorder actually named. Two of the three resolutions come from
 * the link, the third is "this screen has looked nothing up", and none of them is
 * "missing" — because nothing here has looked at the disk (AGENTS.md section
 * 27, issue #52).
 */

const INTERRUPTED: InterruptedRecording = {
  recording_id: 'r-7',
  output: 'D:\\clips\\2026-08-11 cs2.mkv',
  target: 'process cs2.exe',
  elapsed_ms: 42_000,
};

/** A view with nothing in it: no window, no recorder, no interruption. */
const NOTHING: RecorderLinkView = {
  link: null,
  observedAt: null,
  interrupted: null,
  failed: null,
};

/** When a link in these fixtures was observed. Fixed, so nothing here is clock-dependent. */
const OBSERVED_AT = new Date('2026-08-11T12:00:00.000Z');

/** A link attached to a recorder writing one recording. */
function recording(recordingId: string): RecorderLinkView {
  return {
    ...NOTHING,
    link: {
      link: 'attached',
      recorder_process_id: 91,
      features: [],
      status: {
        state: 'recording',
        recording_id: recordingId,
        output: `D:\\clips\\${recordingId}.mkv`,
        target: 'process cs2.exe',
        elapsed_ms: 5_000,
      },
    },
    observedAt: OBSERVED_AT,
  };
}

describe('resolving a recording', () => {
  it('has nothing to say about a recording nothing told this window about', () => {
    const resolution = resolveClip('r-99', NOTHING);

    expect(resolution).toEqual({ found: 'unindexed', recordingId: 'r-99' });
    expect(recordingOf(resolution)).toBeNull();
  });

  it('does not claim a recording is missing, because nothing here has looked', () => {
    // The failure this guards against is the tempting one: an identifier the
    // window cannot resolve reported as a file that has gone. `missing_since`
    // in the library index is the only thing that has been to the disk (issue
    // #56); #301 put it on the wire, and #52 is what would look this
    // particular recording up in it.
    const { state, detail } = describeClip(resolveClip('r-99', NOTHING));

    expect(state).not.toMatch(/missing|gone|deleted|no such/i);
    expect(detail).not.toMatch(/missing|gone|deleted|no such/i);
    expect(detail).toMatch(/#52/);
  });

  it('reads the recording the recorder is writing now', () => {
    const resolution = resolveClip('r-3', recording('r-3'));

    expect(resolution.found).toBe('in-progress');
    expect(recordingOf(resolution)).toEqual({
      recordingId: 'r-3',
      output: 'D:\\clips\\r-3.mkv',
      target: 'process cs2.exe',
      elapsedMs: 5_000,
    });
  });

  it('does not answer for a recording other than the one being written', () => {
    expect(resolveClip('r-4', recording('r-3')).found).toBe('unindexed');
  });

  it('names the file a killed recorder left', () => {
    const resolution = resolveClip('r-7', { ...NOTHING, interrupted: INTERRUPTED });

    expect(resolution.found).toBe('interrupted');
    expect(recordingOf(resolution)?.output).toBe('D:\\clips\\2026-08-11 cs2.mkv');
    expect(describeClip(resolution).detail).toMatch(/not resumed/i);
  });

  it('prefers the recording being written to an interruption with the same identifier', () => {
    // A recorder is interrupted, a replacement is started and records the same
    // target again. The identifier that is live is the newer fact; answering
    // from the interruption would put a history entry in front of somebody
    // watching a recording happen.
    const view: RecorderLinkView = { ...recording('r-7'), interrupted: INTERRUPTED };

    expect(resolveClip('r-7', view).found).toBe('in-progress');
    expect(recordingOf(resolveClip('r-7', view))?.output).toBe('D:\\clips\\r-7.mkv');
  });

  it('says a recording being written cannot be played yet rather than offering to', () => {
    const { state, detail } = describeClip(resolveClip('r-3', recording('r-3')));

    expect(state).toBe('Being recorded now');
    expect(detail).toMatch(/#304/);
  });

  it('reads nothing out of a link that is not attached', () => {
    for (const link of [
      { link: 'connecting' },
      { link: 'unavailable', reason: 'no recorder' },
      { link: 'reconnecting', attempt: 1, attempts_allowed: 4, delay_ms: 500, reason: 'gone' },
      { link: 'attached', recorder_process_id: 91, features: [], status: { state: 'idle' } },
    ] as const) {
      expect(resolveClip('r-3', { ...NOTHING, link, observedAt: OBSERVED_AT }).found).toBe(
        'unindexed',
      );
    }
  });
});

describe('a clip path', () => {
  it('round-trips an identifier that is not path-safe', () => {
    // A recording identifier comes from the recorder and this window does not
    // decide its shape, so it is encoded rather than trusted.
    const path = clipPath('a/b?c#d');

    expect(path).toBe('/clip/a%2Fb%3Fc%23d');
    expect(decodeURIComponent(path.slice('/clip/'.length))).toBe('a/b?c#d');
    expect(isClipPath(path)).toBe(true);
  });

  it('is not confused with a screen', () => {
    expect(isClipPath('/library')).toBe(false);
    expect(isClipPath('/clipboard')).toBe(false);
    expect(isClipPath('/')).toBe(false);
  });
});

describe('an elapsed time', () => {
  it('is minutes and seconds, and hours only when there are some', () => {
    expect(formatElapsed(0)).toBe('00:00');
    expect(formatElapsed(42_000)).toBe('00:42');
    expect(formatElapsed(9 * 60_000 + 5_000)).toBe('09:05');
    expect(formatElapsed(2 * 3_600_000 + 3 * 60_000 + 4_000)).toBe('2:03:04');
  });

  it('does not run backwards on a nonsense figure', () => {
    // The recorder measures this and the window only renders it, so a negative
    // is a bug elsewhere - but "-1:-1" on screen would be a worse way to find
    // out than a zero.
    expect(formatElapsed(-1)).toBe('00:00');
  });
});

describe('the tables the screen draws', () => {
  it('gives every blocker somewhere the reader can check it', () => {
    expect(PLAYBACK_BLOCKERS.length).toBeGreaterThan(0);
    for (const blocker of PLAYBACK_BLOCKERS) {
      expect(blocker.fact.length).toBeGreaterThan(0);
      expect(blocker.evidence.length).toBeGreaterThan(0);
    }
  });

  it('names an issue against everything the screen cannot do', () => {
    // The contract the unbuilt screens keep: a row that says what is missing
    // has to say where the work is, or it is just an apology.
    for (const entry of MISSING) {
      expect(entry.needs).toMatch(/#\d+/);
    }
  });
});
