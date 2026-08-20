import type { SessionSummary } from '@clipped/shared';
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
  recorderCanDo,
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
      features: [],
      status: { state: 'idle' },
    });
    expect(idle.state).toBe('Idle');
    expect(idle.detail).toMatch(/Nothing is being recorded/);

    const recording = describeRecorderLink({
      link: 'attached',
      recorder_process_id: 4242,
      features: [],
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

  it('names the game it is recording, when the recording knows one', () => {
    // Issue #588, in the block that is on screen whichever screen is open.
    // `target` is the capture selector — `process 4242` for a recording nobody
    // asked for — and the sitting on the status is the recorder having already
    // turned it into a name with the catalogue only it holds (issue #241). A
    // window that had been handed the name and drew the selector would be
    // withholding rather than lying, which AGENTS.md section 27 rules out
    // equally.
    const shown = describeRecorderLink({
      link: 'attached',
      recorder_process_id: 4242,
      features: ['automatic'],
      status: {
        state: 'recording',
        recording_id: 'r-1',
        output: 'D:\\clips\\cs2-1.mkv',
        target: 'process 4242',
        elapsed_ms: 4200,
        session: {
          session_id: 'cs2-20260816-201400',
          game_name: 'Counter-Strike 2',
          started_at: '2026-08-16T20:14:00+01:00',
          recordings: [],
        },
      },
    });

    expect(shown.state).toBe('Recording');
    expect(shown.detail).toBe('Recording Counter-Strike 2.');
    expect(shown.detail).not.toContain('4242');
  });

  it('tells a recorder that is watching for a game from one that is idle', () => {
    // The distinction issue #584 put on the wire, arriving in the one place a
    // user sees on every screen. This block drew a watching recorder as "Idle —
    // the recorder is running. Nothing is being recorded", which is the same
    // sentence it draws for a recorder that will never record anything — and
    // the recorder the window starts for itself is the watching kind.
    const watching = describeRecorderLink({
      link: 'attached',
      recorder_process_id: 4242,
      features: ['automatic'],
      status: { state: 'watching' },
    });

    expect(watching.state).toBe('Watching');
    expect(watching.detail).toMatch(/will record the next one/);

    // And a sitting still open in its restart grace keeps the game's name,
    // rather than blanking it for those few seconds and filling it in again.
    const inASitting = describeRecorderLink({
      link: 'attached',
      recorder_process_id: 4242,
      features: ['automatic'],
      status: {
        state: 'watching',
        session: {
          session_id: 'cs2-20260816-201400',
          game_name: 'Counter-Strike 2',
          started_at: '2026-08-16T20:14:00+01:00',
          recordings: [],
        },
      },
    });

    expect(inASitting.state).toBe('Watching');
    expect(inASitting.detail).toContain('Counter-Strike 2');

    /*
     * The interval this window used to describe as its own opposite.
     *
     * A game is recognised, a recording is started for it, and the recorder
     * waits for the game to draw something worth capturing. On a real Garry's
     * Mod launch that took 48 seconds, and for all of it this window said the
     * recorder "will record it again if it starts" — of a game that had started,
     * with a recording already running for it (issue #739). The user read that,
     * reasonably concluded nothing was happening, and went looking for a manual
     * control.
     *
     * So the assertion is not only that the new sentence is right. It is that
     * the old one is *gone*: a build that appended the wait to the existing
     * sentence would pass a `toContain` and still tell somebody their game is
     * not being recorded.
     */
    const starting = describeRecorderLink({
      link: 'attached',
      recorder_process_id: 4242,
      features: ['automatic'],
      status: {
        state: 'watching',
        session: {
          session_id: 'garrys-mod-20260821-000426',
          game_name: "Garry's Mod",
          started_at: '2026-08-21T00:04:26+01:00',
          recordings: [],
        },
        pending: { game_name: "Garry's Mod", waiting_ms: 48_000 },
      },
    });

    expect(starting.state).toBe('Starting');
    expect(starting.detail).toContain("Garry's Mod");
    expect(starting.detail).not.toMatch(/if it starts/);
    expect(starting.detail).not.toMatch(/watching for a game/);
    // And it says how long, because 48 seconds and 2 seconds call for different
    // patience and the recorder is the only side that knows which this is.
    expect(starting.detail).toContain('48 seconds');

    // A wait that has only just begun reads as a duration rather than as a
    // timestamp: "waiting 00:01" is a position in a video.
    const justStarted = describeRecorderLink({
      link: 'attached',
      recorder_process_id: 4242,
      features: ['automatic'],
      status: {
        state: 'watching',
        pending: { game_name: "Garry's Mod", waiting_ms: 1_000 },
      },
    });

    expect(justStarted.detail).toContain('1 second');
    expect(justStarted.detail).not.toContain('1 seconds');

    // The other direction: an idle recorder is not described as one that is
    // about to record something.
    const idle = describeRecorderLink({
      link: 'attached',
      recorder_process_id: 4242,
      features: [],
      status: { state: 'idle' },
    });
    expect(idle.state).toBe('Idle');
    expect(idle.detail).not.toMatch(/watch/i);
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

/**
 * A sitting the recorder closed because the window it was recording changed
 * size, with the one file it left.
 *
 * One recording and no successor: that is what ADR 0012 says a recording
 * somebody *asked for* does with a resize, and it is the case issue #625 is
 * about. An automatic sitting would have a second entry after this one.
 */
const ENDED_BY_RESIZE: SessionSummary = {
  session_id: 'cs2-20260816-201400',
  game_name: 'Counter-Strike 2',
  started_at: '2026-08-16T20:14:00+01:00',
  ended_at: '2026-08-16T20:24:00+01:00',
  end_reason: 'recording-ended',
  recordings: [
    {
      session_index: 1,
      output: 'D:\\clips\\clipped-cs2-20260816-201400.mkv',
      outcome: 'recorded',
      end_reason: 'target-resized',
      duration_ms: 600_000,
    },
  ],
};

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

    expect(result.current).toEqual({
      link: null,
      observedAt: null,
      interrupted: null,
      failed: null,
      ended: null,
    });
  });

  /*
   * A recording that failed while the recorder stayed up. The state that follows
   * it about a second later is "idle" — true, and silent about what happened —
   * so a hook that dropped the failure would leave the window with no record that
   * anything went wrong, which is precisely what a support report is for
   * (issue #101).
   */
  it('keeps a recording failure, which the idle state that follows it does not carry', async () => {
    const runtime = stubRecorderLinkRuntime({ link: 'connecting' });
    const { result } = renderHook(() => useRecorderLink());

    runtime.emit({
      event: 'state',
      link: 'attached',
      recorder_process_id: 91,
      features: [],
      status: {
        state: 'recording',
        recording_id: 'r-7',
        output: 'D:\\clips\\2026-08-11 cs2.mkv',
        target: 'process cs2.exe',
        elapsed_ms: 42_000,
      },
    });
    runtime.emit({
      event: 'recording_failed',
      recording_id: 'r-7',
      error: { code: 'recording_failed', message: 'the encoder stopped accepting frames' },
    });
    runtime.emit({
      event: 'state',
      link: 'attached',
      recorder_process_id: 91,
      features: [],
      status: { state: 'idle' },
    });

    await waitFor(() => {
      expect(result.current.link).toEqual({
        link: 'attached',
        recorder_process_id: 91,
        features: [],
        status: { state: 'idle' },
      });
    });

    expect(result.current.failed?.recording_id).toBe('r-7');
    expect(result.current.failed?.error.message).toBe('the encoder stopped accepting frames');
    // The event carries no path, so the file comes from the recording status
    // this window saw — and only because its identifier is the one that failed.
    expect(result.current.failed?.output).toBe('D:\\clips\\2026-08-11 cs2.mkv');
  });

  /*
   * The same rule `src-tauri/src/notifications.rs` applies to the toast's "Show
   * the file" button: a failure for a recording this window never saw claims no
   * file at all. Naming the last file seen would put a different recording in
   * front of somebody diagnosing this one.
   */
  it('refuses to name a file for a recording it never saw', async () => {
    const runtime = stubRecorderLinkRuntime({ link: 'connecting' });
    const { result } = renderHook(() => useRecorderLink());

    runtime.emit({
      event: 'state',
      link: 'attached',
      recorder_process_id: 91,
      features: [],
      status: {
        state: 'recording',
        recording_id: 'r-1',
        output: 'D:\\clips\\someone-elses.mkv',
        target: 'process cs2.exe',
        elapsed_ms: 1000,
      },
    });
    runtime.emit({
      event: 'recording_failed',
      recording_id: 'r-9',
      error: { code: 'internal', message: 'the muxer could not write a frame' },
    });

    await waitFor(() => {
      expect(result.current.failed?.recording_id).toBe('r-9');
    });
    expect(result.current.failed?.output).toBeNull();
  });

  /*
   * The sitting a recording that ended by itself is announced in (issue #625).
   * `useRecording` only ever learns of an ending this window asked for, from the
   * reply to its own `stop_recording`; a recording finished by its window being
   * dragged to a new size has no such reply, so this event is the only thing the
   * window is told — and the "idle" state a moment later is true and silent, in
   * exactly the way the failure above is.
   *
   * The payload is flattened beside the tag rather than nested under a `session`
   * key, because `RecorderLinkEvent::SessionEnded` is a newtype in an internally
   * tagged enumeration. `a_sitting_that_ended_survives_the_journey_into_a_window`
   * in `crates/ipc/src/supervisor/link.rs` is the other half of this: it holds
   * that shape still on the Rust side, and this reads it on the TypeScript one.
   */
  it('keeps the sitting that ended, which the idle state that follows it does not carry', async () => {
    const runtime = stubRecorderLinkRuntime({ link: 'connecting' });
    const { result } = renderHook(() => useRecorderLink());

    runtime.emit({ event: 'session_ended', ...ENDED_BY_RESIZE });
    await waitFor(() => {
      expect(result.current.ended).toEqual(ENDED_BY_RESIZE);
    });

    runtime.emit({
      event: 'state',
      link: 'attached',
      recorder_process_id: 91,
      features: [],
      status: { state: 'idle' },
    });
    await waitFor(() => {
      expect(result.current.link).toEqual({
        link: 'attached',
        recorder_process_id: 91,
        features: [],
        status: { state: 'idle' },
      });
    });

    expect(
      result.current.ended,
      'the sitting is dropped by the idle state that follows it, so nothing in the window is ' +
        'left to say why the recording stopped',
    ).toEqual(ENDED_BY_RESIZE);
    // The reason travels with the file rather than beside the sitting: it is
    // per-recording, and it is the whole of what the screen has to work with.
    expect(result.current.ended?.recordings.at(-1)?.end_reason).toBe('target-resized');
  });

  /*
   * `elapsed_ms` is measured when the recorder answers, so a status kept for four
   * minutes says a recording has run for eight seconds. The window cannot make it
   * fresher — nothing polls, and nothing should — so what it records is *when* it
   * was true, which is what makes the figure readable rather than misleading
   * (AGENTS.md section 27).
   */
  it('records when it last heard from the recorder, so a stale figure reads as one', async () => {
    const runtime = stubRecorderLinkRuntime({ link: 'connecting' });
    const { result } = renderHook(() => useRecorderLink());

    await waitFor(() => {
      expect(result.current.observedAt).toBeInstanceOf(Date);
    });
    const first = result.current.observedAt;

    runtime.emit({ event: 'state', link: 'unavailable', reason: 'the recorder went' });

    await waitFor(() => {
      expect(result.current.link).toEqual({ link: 'unavailable', reason: 'the recorder went' });
    });
    expect(result.current.observedAt?.getTime()).toBeGreaterThanOrEqual(first?.getTime() ?? 0);
    expect(result.current.observedAt).not.toBe(first);
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
      features: [],
      status: { state: 'idle' },
    });
    await waitFor(() => {
      expect(result.current.link).toEqual({
        link: 'attached',
        recorder_process_id: 91,
        features: [],
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

describe('recorderCanDo', () => {
  const attached = (features: readonly string[]): RecorderLinkState => ({
    link: 'attached',
    recorder_process_id: 91,
    features,
    status: { state: 'idle' },
  });

  it('answers for the recorder this window is attached to', () => {
    const link = attached(['recording', 'library']);

    expect(recorderCanDo(link, 'library')).toBe(true);
    expect(recorderCanDo(link, 'export')).toBe(false);
  });

  it('refuses while there is no recorder to have the capability', () => {
    // The honest answer for all three: a control drawn on the strength of the
    // recorder that was there is a control that refuses when pressed, which is
    // the failure issue #447 is about arriving one step later.
    expect(recorderCanDo(null, 'export')).toBe(false);
    expect(recorderCanDo({ link: 'connecting' }, 'export')).toBe(false);
    expect(
      recorderCanDo(
        {
          link: 'reconnecting',
          attempt: 1,
          attempts_allowed: 4,
          delay_ms: 500,
          reason: 'the connection ended',
        },
        'export',
      ),
    ).toBe(false);
    expect(recorderCanDo({ link: 'unavailable', reason: 'no recorder' }, 'export')).toBe(false);
  });

  it('carries a capability this build has no control for', () => {
    // A newer recorder may name one. Dropping it would make the list describe
    // this build rather than that recorder, and the next control added here
    // would read `false` against a recorder that can in fact do it.
    const link = attached(['recording', 'something_this_build_has_never_heard_of']);

    expect(link.link === 'attached' && link.features).toContain(
      'something_this_build_has_never_heard_of',
    );
  });
});
