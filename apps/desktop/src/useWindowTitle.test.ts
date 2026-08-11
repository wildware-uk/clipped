import { renderHook } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

// The window's whole privilege, imported rather than read off disk so that the
// bundler resolves the path: moving or renaming the file fails to build here
// instead of failing to be found at run time.
import capabilities from '../src-tauri/capabilities/default.json';
import { useWindowTitle } from './useWindowTitle';

/**
 * The half of `useWindowTitle` that only runs inside the Tauri window.
 *
 * jsdom is a browser, so `'__TAURI_INTERNALS__' in window` is false and the
 * hook stops at `document.title` — which left the native call, the only reason
 * the hook exists, with no coverage at all. Standing that object up is enough
 * to reach it, because `__TAURI_INTERNALS__` is exactly what the Tauri API
 * calls: `getCurrentWindow()` reads the label out of it and `setTitle()` goes
 * through its `invoke`. The code under test is the real
 * `@tauri-apps/api/window`, not a mock of it.
 *
 * What this cannot reach is the Rust side of that call. Tauri decides whether
 * to answer `plugin:window|set_title` in the process that owns the window, from
 * `capabilities/default.json`, and no amount of jsdom will run that. The last
 * test here is the nearest honest substitute: it asserts that the capability
 * file grants the command this hook actually invokes, so deleting the
 * permission fails here rather than in a window nobody opened.
 */

/** The stub's record of what the interface asked the Tauri runtime for. */
interface Invocation {
  readonly command: string;
  readonly args: Record<string, unknown>;
}

/**
 * Makes this document look like the Tauri window to `@tauri-apps/api`.
 *
 * `invoke` resolves by default; `rejectWith` makes it fail, which is the path
 * where the hook logs and carries on rather than tearing down the interface.
 */
function stubTauriWindow(rejectWith?: Error): Invocation[] {
  const invocations: Invocation[] = [];

  vi.stubGlobal('__TAURI_INTERNALS__', {
    metadata: { currentWindow: { label: 'main' } },
    invoke: (command: string, args: Record<string, unknown>): Promise<void> => {
      invocations.push({ command, args });
      return rejectWith ? Promise.reject(rejectWith) : Promise.resolve();
    },
  });

  return invocations;
}

describe('useWindowTitle', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it('renames the native window, not only the document', () => {
    const invocations = stubTauriWindow();

    renderHook(() => {
      useWindowTitle('Clipped — Library');
    });

    expect(document.title).toBe('Clipped — Library');
    expect(invocations).toEqual([
      { command: 'plugin:window|set_title', args: { label: 'main', value: 'Clipped — Library' } },
    ]);
  });

  it('leaves the Tauri runtime alone in a browser', () => {
    renderHook(() => {
      useWindowTitle('Clipped — Library');
    });

    // `npm run dev:web` serves the same interface to a browser tab, where
    // `getCurrentWindow()` would throw reading a label off an object that is
    // not there. Reaching the assertion at all is the assertion.
    expect(document.title).toBe('Clipped — Library');
  });

  it('logs and carries on when the window cannot be renamed', async () => {
    const error = new Error('the window went away');
    stubTauriWindow(error);
    const logged = vi.spyOn(console, 'error').mockImplementation(() => undefined);

    renderHook(() => {
      useWindowTitle('Clipped — Trash');
    });

    // The rejection is handled some way after the effect returns, and how many
    // promises deep is the Tauri API's business rather than this hook's.
    await vi.waitFor(() => {
      expect(logged).toHaveBeenCalledWith('Could not set the window title', error);
    });
    expect(document.title).toBe('Clipped — Trash');
  });

  it('is granted the command it invokes by capabilities/default.json', () => {
    const invocations = stubTauriWindow();

    renderHook(() => {
      useWindowTitle('Clipped — Home');
    });

    // Tauri names a permission after the command it grants:
    // `plugin:window|set_title` is allowed by `core:window:allow-set-title`.
    const permissionsNeeded = invocations.map((invocation) => {
      const [namespace, command] = invocation.command.replace(/^plugin:/, '').split('|');
      return `core:${namespace}:allow-${(command ?? '').replaceAll('_', '-')}`;
    });

    expect(permissionsNeeded).toEqual(['core:window:allow-set-title']);
    for (const permission of permissionsNeeded) {
      expect(capabilities.permissions).toContain(permission);
    }
  });
});
