import { act, renderHook, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

// The window's whole privilege, imported rather than read off disk so that the
// bundler resolves the path: moving or renaming the file fails to build here
// instead of failing to be found at run time.
import capabilities from '../src-tauri/capabilities/default.json';
import { stubRecorderLinkRuntime } from './test/recorderLinkRuntime';
import { joinNotices, useTray } from './useTray';

afterEach(() => {
  vi.unstubAllGlobals();
});

/**
 * What the tray says to the window.
 *
 * The tray is the first part of Clipped that acts on the recorder, and two of
 * its items — Open Library and Settings — do their work *here*, by asking the
 * window to go somewhere. The third thing it sends is a sentence about something
 * that failed, which the tray has nowhere of its own to show.
 */
describe('the window following its notification-area menu', () => {
  it('goes where the tray sends it', async () => {
    const runtime = stubRecorderLinkRuntime({ link: 'connecting' });
    const went: string[] = [];
    renderHook(() => useTray((path) => went.push(path)));

    await waitFor(() => {
      expect(
        runtime.invocations.some(
          (invocation) =>
            invocation.command === 'plugin:event|listen' &&
            invocation.args['event'] === 'tray-navigate',
        ),
      ).toBe(true);
    });

    runtime.emitTo('tray-navigate', '/library');
    expect(went).toEqual(['/library']);
  });

  it('does not treat a navigation as a notice, or the other way round', async () => {
    // The two events carry the same type and mean opposite things: one moves
    // the window, the other puts a sentence on the screen. A hook that
    // subscribed to the wrong name, or handled both with one callback, would
    // navigate to an error message.
    const runtime = stubRecorderLinkRuntime({ link: 'connecting' });
    const went: string[] = [];
    const { result } = renderHook(() => useTray((path) => went.push(path)));

    await waitFor(() => {
      expect(
        runtime.invocations.filter((invocation) => invocation.command === 'plugin:event|listen')
          .length,
      ).toBeGreaterThanOrEqual(2);
    });

    runtime.emitTo('tray-notice', 'Recording could not be started.');
    await waitFor(() => {
      expect(result.current).toBe('Recording could not be started.');
    });
    expect(went).toEqual([]);
  });

  it('keeps a notice until the tray has something else to say', async () => {
    const runtime = stubRecorderLinkRuntime({ link: 'connecting' });
    const { result, rerender } = renderHook(() => useTray(() => undefined));

    await waitFor(() => {
      expect(
        runtime.invocations.some((invocation) => invocation.args['event'] === 'tray-notice'),
      ).toBe(true);
    });

    runtime.emitTo('tray-notice', 'The recording could not be stopped.');
    await waitFor(() => {
      expect(result.current).toBe('The recording could not be stopped.');
    });

    // A render for any other reason does not clear it: the failure did not stop
    // having happened, and there is nowhere else in the window to read it.
    rerender();
    expect(result.current).toBe('The recording could not be stopped.');

    runtime.emitTo('tray-notice', 'Clipped did not exit.');
    await waitFor(() => {
      expect(result.current).toBe('Clipped did not exit.');
    });
  });

  it('shows what failed before the window existed to be told about it', async () => {
    // A notification-area icon that could not be added is the one that matters:
    // without a tray, closing this window quits Clipped rather than minimising,
    // and quitting leaves the recorder running. Tauri's `setup` runs before
    // React does, so it cannot be *sent* — nothing is subscribed yet — and it
    // is asked for instead.
    const notice = 'Clipped could not add its notification-area icon: no shell.';
    stubRecorderLinkRuntime({ link: 'connecting' }, notice);
    const { result } = renderHook(() => useTray(() => undefined));

    await waitFor(() => {
      expect(result.current).toBe(notice);
    });
  });

  it('says nothing about a startup that went as it should', async () => {
    const runtime = stubRecorderLinkRuntime({ link: 'connecting' });
    const { result } = renderHook(() => useTray(() => undefined));

    await waitFor(() => {
      expect(runtime.invocations.some((sent) => sent.command === 'startup_notice')).toBe(true);
    });
    expect(result.current).toBeUndefined();
  });

  it('lets the tray say something newer than the startup did', async () => {
    // Both are true and the status block holds one paragraph. The tray's is
    // what the user did a second ago, so it is the one on screen.
    const runtime = stubRecorderLinkRuntime({ link: 'connecting' }, 'The icon could not be added.');
    const { result } = renderHook(() => useTray(() => undefined));

    await waitFor(() => {
      expect(result.current).toBe('The icon could not be added.');
    });

    runtime.emitTo('tray-notice', 'The recording could not be stopped.');
    await waitFor(() => {
      expect(result.current).toBe('The recording could not be stopped.');
    });
  });

  it('does not overwrite something the tray said while it was still asking', async () => {
    // The command is a round trip, so the tray can report a failed action
    // before the answer to it arrives. The startup failure is the older of the
    // two by definition — it happened before this window existed — so it must
    // not land on top of what the user did a moment ago.
    let answer: (said: string | null) => void = () => undefined;
    const asking = new Promise<string | null>((resolve) => {
      answer = resolve;
    });
    const runtime = stubRecorderLinkRuntime({ link: 'connecting' }, asking);
    const { result } = renderHook(() => useTray(() => undefined));

    await waitFor(() => {
      expect(runtime.invocations.some((sent) => sent.args['event'] === 'tray-notice')).toBe(true);
    });
    runtime.emitTo('tray-notice', 'The recording could not be stopped.');
    await waitFor(() => {
      expect(result.current).toBe('The recording could not be stopped.');
    });

    await act(async () => {
      answer('Clipped could not add its notification-area icon.');
      await asking;
    });
    expect(result.current).toBe('The recording could not be stopped.');
  });

  it('asks the Tauri runtime for nothing outside the Clipped window', () => {
    // `npm run dev:web` serves the same interface to a browser, where there is
    // no tray at all. Subscribing there would throw into an unhandled rejection
    // nobody reads.
    const { result } = renderHook(() => useTray(() => undefined));

    expect(result.current).toBeUndefined();
    expect('__TAURI_INTERNALS__' in window).toBe(false);
  });

  it('is granted the permission both subscriptions need', () => {
    // Tauri denies what is not listed, so removing this line would make the
    // tray's Open Library raise a window that never moved — and nothing in the
    // interface would say why. Asserted against the file rather than assumed.
    expect(capabilities.permissions).toContain('core:event:allow-listen');
    expect(capabilities.permissions).toContain('core:event:allow-unlisten');
  });
});

describe('joining what the window has to report', () => {
  it('says nothing when there is nothing to say', () => {
    expect(joinNotices(undefined, undefined)).toBeUndefined();
  });

  it('keeps both, because neither stops being true', () => {
    // An interrupted recording names a file nothing else will ever mention, and
    // a tray notice is the result of something the user did a second ago.
    // Dropping either would lose the only place it is said.
    expect(
      joinNotices('Recording could not be started.', 'Recording interrupted. Not resumed.'),
    ).toBe('Recording could not be started. Recording interrupted. Not resumed.');
  });

  it('does not leave a gap where an absent notice was', () => {
    expect(joinNotices(undefined, 'Recording interrupted.')).toBe('Recording interrupted.');
    expect(joinNotices('Clipped did not exit.', undefined)).toBe('Clipped did not exit.');
  });
});
