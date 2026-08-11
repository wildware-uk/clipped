import { describe, expect, it } from 'vitest';

import { describeRecorderLink, type RecorderLinkState } from './useRecorderLink';

/**
 * What the status block says, as tests.
 *
 * The wording is the whole of what the window tells somebody about a recorder
 * they cannot see, so AGENTS.md section 27's rule — never show a state the
 * application does not have — is checked here rather than left to review. Every
 * case below is a state the Rust side can genuinely be in, and the assertions
 * are about what is said and what is *not*.
 */
describe('what the status block says about the recorder', () => {
  it('does not claim a recorder outside the Clipped window', () => {
    // `npm run dev:web` and the test environment: there is no Rust side to ask,
    // which is a different thing from a recorder that is not there.
    const shown = describeRecorderLink(null);

    expect(shown.state).toBe('Not connected');
    expect(shown.detail).toMatch(/not the Clipped window/);
  });

  it('says it is still looking rather than guessing at a state', () => {
    expect(describeRecorderLink({ link: 'connecting' })).toEqual({
      state: 'Connecting',
      detail: 'Looking for the recorder.',
    });
  });

  it('distinguishes a recorder that is running from one that is recording', () => {
    const idle = describeRecorderLink({
      link: 'attached',
      recorder_process_id: 4242,
      status: { state: 'idle' },
    });
    expect(idle.state).toBe('Idle');
    expect(idle.detail).toMatch(/Nothing is being recorded/);

    const recording = describeRecorderLink({
      link: 'attached',
      recorder_process_id: 4242,
      status: {
        state: 'recording',
        recording_id: 'r-1',
        output: 'D:\\clips\\session.mkv',
        target: 'process cs2.exe',
        elapsed_ms: 4200,
      },
    });
    expect(recording.state).toBe('Recording');
    expect(recording.detail).toContain('process cs2.exe');
  });

  it('carries the reason and the attempt while it is reconnecting', () => {
    // A window that said only "Reconnecting" would leave the user with no idea
    // whether to wait or to do something (AGENTS.md section 45).
    const shown = describeRecorderLink({
      link: 'reconnecting',
      attempt: 2,
      attempts_allowed: 4,
      delay_ms: 2000,
      reason: 'The connection to the recorder ended.',
    });

    expect(shown.state).toBe('Reconnecting');
    expect(shown.detail).toBe('The connection to the recorder ended. Attempt 2 of 4.');
  });

  it('shows the recorder’s own reason when the link has given up', () => {
    const shown = describeRecorderLink({
      link: 'unavailable',
      reason: 'The recorder was not found at C:\\Clipped\\clipped-recorder.exe.',
    });

    expect(shown.state).toBe('Not available');
    expect(shown.detail).toBe('The recorder was not found at C:\\Clipped\\clipped-recorder.exe.');
  });

  it('never says "Idle" for a link that is not attached', () => {
    // The failure this whole type exists to prevent: a window showing an idle
    // recorder while the recorder is gone. Walked over every state rather than
    // asserted for the two that happen to be wrong today, so a fifth state
    // added later has to be considered here.
    const notAttached: readonly RecorderLinkState[] = [
      { link: 'connecting' },
      { link: 'reconnecting', attempt: 1, attempts_allowed: 4, delay_ms: 1000, reason: 'gone' },
      { link: 'unavailable', reason: 'gone' },
    ];

    for (const state of notAttached) {
      const shown = describeRecorderLink(state);
      expect(shown.state).not.toBe('Idle');
      expect(shown.state).not.toBe('Recording');
    }
  });
});
