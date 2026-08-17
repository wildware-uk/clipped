import { describe, expect, it } from 'vitest';

import { describePlaybackProblem, headlinePlaybackProblem, trackLabel } from './playback';

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
