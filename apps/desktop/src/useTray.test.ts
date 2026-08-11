import { renderHook, waitFor } from '@testing-library/react';
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
