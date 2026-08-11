import { getCurrentWindow } from '@tauri-apps/api/window';
import { useEffect } from 'react';

/**
 * Whether this document is inside the Tauri window rather than a browser tab.
 *
 * `npm run dev:web` serves the same interface to a browser, where there is no
 * native window to rename and the Tauri API has nothing to talk to.
 */
function inTauriWindow(): boolean {
  return '__TAURI_INTERNALS__' in window;
}

/**
 * Keeps the window's title in step with the screen on show.
 *
 * `document.title` is not enough on its own: a web view's title does not reach
 * the native window that hosts it, so without the second call the taskbar,
 * Alt+Tab and the screen reader's window announcement would all say "Clipped"
 * whatever is open.
 */
export function useWindowTitle(title: string): void {
  useEffect(() => {
    document.title = title;

    if (!inTauriWindow()) {
      return;
    }

    getCurrentWindow()
      .setTitle(title)
      .catch((error: unknown) => {
        // Renaming the window is a convenience, not a feature to fail over:
        // say so and carry on, rather than tearing down the interface.
        console.error('Could not set the window title', error);
      });
  }, [title]);
}
