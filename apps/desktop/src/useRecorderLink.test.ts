import { renderHook, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

// The window's whole privilege, imported rather than read off disk so that the
// bundler resolves the path: moving or renaming the file fails to build here
// instead of failing to be found at run time.
import capabilities from '../src-tauri/capabilities/default.json';
import { stubRecorderLinkRuntime } from './test/recorderLinkRuntime';
import {
  describeInterruption,
  describeRecorderLink,
  useRecorderLink,
  type InterruptedRecording,
  type RecorderLinkState,
} from './useRecorderLink';

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
    // asserted for the two that happen to be wrong today.
    //
    // What forces a fifth state to be considered is not this array — it is
    // hand-written, and adding a variant to `RecorderLinkState` does not add a
    // row to it. It is `noImplicitReturns` in `tsconfig.base.json`, which makes
    // `describeRecorderLink`'s `switch` fail to compile the moment one of its
    // arms is missing, because the function would then have a path that returns
    // nothing. This array is the check that today's four are all rendered as
    // something other than a running recorder.
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

/** A recording a recorder was killed in the middle of. */
const INTERRUPTED: InterruptedRecording = {
  recording_id: 'r-7',
  output: 'D:\\clips\\2026-08-11 cs2.mkv',
  target: 'process cs2.exe',
  elapsed_ms: 42_000,
};

/**
 * What the window says about a recording that was interrupted.
 *
 * ADR 0006 defines recovery as three statements and no more: a recording was
 * interrupted, here is the file, it was not resumed. Each is asserted
 * separately, because dropping any one of them is a different failure — a
 * notice with no path leaves the user to search their disk, and a notice that
 * did not say "not resumed" would imply a replacement recorder picked the
 * recording up, which is exactly what ADR 0006 rejected as undeliverable.
 */
describe('what the status block says about an interrupted recording', () => {
  it('says nothing at all when no recording was interrupted', () => {
    // Not an empty string: the block draws nothing rather than an empty rule.
    expect(describeInterruption(null)).toBeUndefined();
  });

  it('names the file, and says it was not resumed', () => {
    const shown = describeInterruption(INTERRUPTED);

    expect(shown).toContain('interrupted');
    expect(shown).toContain('D:\\clips\\2026-08-11 cs2.mkv');
    expect(shown).toMatch(/not resumed/i);
  });
});

/**
 * The half of `useRecorderLink` that only runs inside the Tauri window.
 *
 * jsdom is a browser, so `'__TAURI_INTERNALS__' in window` is false and the
 * hook returns nothing but `null` — which left the subscription, the reason the
 * hook exists at all, with no coverage. Everything below runs the real hook
 * against a stubbed runtime.
 *
 * What this cannot reach is the Rust side. Tauri decides whether to answer
 * `plugin:event|listen` in the process that owns the window, from
 * `capabilities/default.json`, and no amount of jsdom will run that — which is
 * how this hook shipped without `core:event:allow-listen` and no test could
 * see it. The last test here is the nearest honest substitute, and is the same
 * one `useWindowTitle.test.ts` uses for `set_title`.
 */
describe('following the recorder link inside the window', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it('reports no link at all outside the Clipped window', () => {
    const { result } = renderHook(() => useRecorderLink());

    expect(result.current).toEqual({ link: null, interrupted: null });
  });

  it('surfaces the file a killed recorder left, and does not lose it', async () => {
    // Acceptance criterion 2, at the layer the user is at. The supervisor
    // reports the interruption and then, about a second later, reports a state
    // — "reconnecting", then "attached" and idle. A hook that dropped the
    // interruption would leave the window saying "Idle. The recorder is
    // running. Nothing is being recorded.", which is true and is not what
    // happened (ADR 0006, AGENTS.md section 27).
    const runtime = stubRecorderLinkRuntime({ link: 'connecting' });
    const { result } = renderHook(() => useRecorderLink());

    await waitFor(() => {
      expect(result.current.link).toEqual({ link: 'connecting' });
    });

    runtime.emit({ event: 'recording_interrupted', ...INTERRUPTED });
    await waitFor(() => {
      expect(result.current.interrupted).toEqual(INTERRUPTED);
    });

    runtime.emit({
      event: 'state',
      link: 'attached',
      recorder_process_id: 91,
      status: { state: 'idle' },
    });
    await waitFor(() => {
      expect(result.current.link).toEqual({
        link: 'attached',
        recorder_process_id: 91,
        status: { state: 'idle' },
      });
    });

    expect(result.current.interrupted).toEqual(INTERRUPTED);
  });

  it('lets an event win over a first answer that arrives after it', async () => {
    // The two are independent round trips. An event handled before `invoke`
    // resolves is newer than the answer to that `invoke`, so letting the answer
    // land would put a stale snapshot on screen — in the one hook whose whole
    // job is to be honest about what the recorder is doing.
    const runtime = stubRecorderLinkRuntime({ link: 'connecting' });
    const { result } = renderHook(() => useRecorderLink());

    runtime.emit({ event: 'state', link: 'unavailable', reason: 'the recorder went' });

    await waitFor(() => {
      expect(result.current.link).toEqual({ link: 'unavailable', reason: 'the recorder went' });
    });
    // The `recorder_link_state` promise resolves somewhere in here; a few turns
    // of the microtask queue is more than it needs.
    await Promise.resolve();
    await Promise.resolve();

    expect(result.current.link).toEqual({ link: 'unavailable', reason: 'the recorder went' });
  });

  it('is granted every command it invokes by capabilities/default.json', async () => {
    const runtime = stubRecorderLinkRuntime({ link: 'connecting' });
    const { unmount } = renderHook(() => useRecorderLink());

    await waitFor(() => {
      expect(runtime.invocations.map((invocation) => invocation.command)).toContain(
        'plugin:event|listen',
      );
    });
    unmount();
    await waitFor(() => {
      expect(runtime.invocations.map((invocation) => invocation.command)).toContain(
        'plugin:event|unlisten',
      );
    });

    // Tauri names a permission after the command it grants:
    // `plugin:event|listen` is allowed by `core:event:allow-listen`. Commands
    // without a `plugin:` prefix are this application's own — `recorder_link_state`
    // is declared in `generate_handler!` and needs no capability.
    const permissionsNeeded = runtime.invocations
      .map((invocation) => invocation.command)
      .filter((command) => command.startsWith('plugin:'))
      .map((command) => {
        const [namespace, name] = command.replace(/^plugin:/, '').split('|');
        return `core:${namespace}:allow-${(name ?? '').replaceAll('_', '-')}`;
      });

    expect([...new Set(permissionsNeeded)].sort()).toEqual([
      'core:event:allow-listen',
      'core:event:allow-unlisten',
    ]);
    for (const permission of permissionsNeeded) {
      expect(capabilities.permissions).toContain(permission);
    }
  });
});
