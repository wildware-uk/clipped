import { describe, expect, it } from 'vitest';

import {
  describePlaybackProblem,
  headlinePlaybackProblem,
  playbackKeyAction,
  trackLabel,
} from './playback';

/**
 * The wording around the player (issue #304).
 *
 * The player itself is asserted through the screen, where the round trip is
 * (`ClipPlaybackScreen.test.tsx`). What is here is the two things that are
 * decisions rather than plumbing: what an unnamed track is called, and what
 * somebody is told when a recording will not play.
 */

describe('a track on the selector', () => {
  it('is called what the recording called it', () => {
    expect(trackLabel({ index: 3, name: 'Microphone' }, 2)).toBe('Microphone');
  });

  it('is called by its position when the recording named none', () => {
    // Not "Game" and not "Audio track": a file that named nothing has told this
    // window nothing about what is on it, and a name invented here would be a
    // claim about somebody's recording (AGENTS.md section 27).
    expect(trackLabel({ index: 2 }, 1)).toBe('Audio 2');
  });
});

describe('a recording that will not play', () => {
  it('passes on the recorder’s own sentence about a file that has gone', () => {
    // The one that matters: the recorder looked at the disk, so it can say what
    // this window cannot. Rewording it here would lose the file name.
    const problem = {
      code: 'playback_failed',
      message: 'match.mkv is not there any more. It may have been moved or deleted.',
    };

    expect(describePlaybackProblem(problem)).toBe(problem.message);
    expect(headlinePlaybackProblem(problem)).toBe('That recording cannot be played');
  });

  it('says a recorder too old to serve one is too old, and what to do about it', () => {
    // A recorder built before this command exists refuses by name, and "that
    // recording cannot be played" would send somebody looking at their file.
    const problem = { code: 'unknown_command', message: 'this recorder has no `open_playback`' };

    expect(headlinePlaybackProblem(problem)).toBe('This recorder cannot play a recording');
    expect(describePlaybackProblem(problem)).toMatch(/restarting clipped/i);
  });

  it('says which of the two ends could not be reached when the link is down', () => {
    const problem = { code: 'recorder_unreachable', message: 'the pipe is not there' };

    expect(describePlaybackProblem(problem)).toMatch(/could not ask the recorder/i);
    expect(describePlaybackProblem(problem)).toContain('the pipe is not there');
  });
});

describe('the keys the playback screen answers', () => {
  /** A press, with no modifier unless one is asked for. */
  function press(
    key: string,
    over: Partial<Record<'shiftKey' | 'ctrlKey' | 'altKey' | 'metaKey', boolean>> = {},
  ) {
    return {
      key,
      ctrlKey: false,
      altKey: false,
      metaKey: false,
      shiftKey: false,
      ...over,
    };
  }

  it('plays and pauses on space and on K', () => {
    expect(playbackKeyAction(press(' '), null)).toEqual({ kind: 'toggle' });
    expect(playbackKeyAction(press('k'), null)).toEqual({ kind: 'toggle' });
    expect(playbackKeyAction(press('K'), null)).toEqual({ kind: 'toggle' });
  });

  it('steps five seconds with an arrow and one with Shift held', () => {
    expect(playbackKeyAction(press('ArrowRight'), null)).toEqual({ kind: 'seek', seconds: 5 });
    expect(playbackKeyAction(press('ArrowLeft'), null)).toEqual({ kind: 'seek', seconds: -5 });
    expect(playbackKeyAction(press('ArrowRight', { shiftKey: true }), null)).toEqual({
      kind: 'seek',
      seconds: 1,
    });
    expect(playbackKeyAction(press('ArrowLeft', { shiftKey: true }), null)).toEqual({
      kind: 'seek',
      seconds: -1,
    });
  });

  it('goes to the ends on Home and End', () => {
    expect(playbackKeyAction(press('Home'), null)).toEqual({ kind: 'start' });
    expect(playbackKeyAction(press('End'), null)).toEqual({ kind: 'end' });
  });

  /*
   * The half that matters more than the shortcuts. Adding a screen-wide space
   * that plays the recording, at the cost of a focused track button no longer
   * being pressable by keyboard, would be a worse screen than one with no
   * shortcuts at all.
   */
  it('declines a key the focused control is going to use itself', () => {
    expect(playbackKeyAction(press(' '), 'BUTTON')).toBeNull();
    expect(playbackKeyAction(press(' '), 'A')).toBeNull();
    expect(playbackKeyAction(press(' '), 'INPUT')).toBeNull();
    expect(playbackKeyAction(press('ArrowRight'), 'INPUT')).toBeNull();
    expect(playbackKeyAction(press('ArrowRight'), 'SELECT')).toBeNull();
    // The transport answers all of them itself, and knows its own granularity.
    expect(playbackKeyAction(press('ArrowRight'), 'VIDEO')).toBeNull();
  });

  /*
   * A focused button uses space and Enter; it does not use the arrows, so the
   * screen still answers those. Without this, tabbing to a track button would
   * turn the arrow keys off.
   */
  it('still answers the arrows while a button has focus', () => {
    expect(playbackKeyAction(press('ArrowRight'), 'BUTTON')).toEqual({ kind: 'seek', seconds: 5 });
  });

  /*
   * `Ctrl+Left` is a word jump and `Alt+Left` is Back. Neither belongs to this
   * window, and a screen that swallowed them would break navigation to add a
   * seek nobody asked for.
   */
  it('leaves a modified press alone', () => {
    for (const modifier of ['ctrlKey', 'altKey', 'metaKey'] as const) {
      expect(playbackKeyAction(press('ArrowLeft', { [modifier]: true }), null)).toBeNull();
      expect(playbackKeyAction(press(' ', { [modifier]: true }), null)).toBeNull();
    }
  });

  it('claims no other key', () => {
    for (const key of ['Enter', 'Escape', 'Tab', 'a', 'F5', 'PageDown']) {
      expect(playbackKeyAction(press(key), null)).toBeNull();
    }
  });
});
