import type { WindowControls } from '@clipped/ui';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { useEffect, useMemo, useState } from 'react';

/**
 * Wires the title bar's buttons to the window they belong to.
 *
 * This is the only module in the application that talks to the window manager.
 * Keeping it out of `@clipped/ui` is what lets the shell render in a test
 * runner, and keeping it in one place is what stops a second component from
 * reaching for `getCurrentWindow()` because it was easier than a prop.
 *
 * Every call here is asynchronous - the request crosses into the Rust side and
 * back - so each one reports its own failure rather than being dropped
 * (AGENTS.md section 15). Failures are logged, not shown: telling the user
 * that minimising failed needs the notification system in issue #110.
 */
export function useWindowControls(): WindowControls {
  const [isMaximised, setIsMaximised] = useState(false);

  useEffect(() => {
    const appWindow = getCurrentWindow();
    let current = true;

    const refresh = () => {
      appWindow.isMaximized().then(
        (maximised) => {
          if (current) {
            setIsMaximised(maximised);
          }
        },
        (error: unknown) => {
          report('read whether the window is maximised', error);
        },
      );
    };

    refresh();

    // Maximising, restoring and snapping to a screen edge all arrive as a
    // resize. Polling would be the alternative, and this window may sit
    // open for days (AGENTS.md section 59).
    const listening = appWindow.onResized(refresh);
    listening.catch((error: unknown) => {
      report('listen for window resizes', error);
    });

    return () => {
      current = false;
      listening.then(
        (stop) => {
          stop();
        },
        () => {
          // Already reported above; there is nothing to unlisten.
        },
      );
    };
  }, []);

  return useMemo(
    () => ({
      isMaximised,
      onMinimise: () => {
        run('minimise the window', getCurrentWindow().minimize());
      },
      onToggleMaximise: () => {
        run('maximise or restore the window', getCurrentWindow().toggleMaximize());
      },
      onClose: () => {
        // `close()` asks the window to close, which is what lets a
        // close handler intervene. Once Clipped lives in the tray
        // (issue #50) that handler is what will hide the window
        // instead of ending the process.
        run('close the window', getCurrentWindow().close());
      },
    }),
    [isMaximised],
  );
}

function run(action: string, request: Promise<void>): void {
  request.catch((error: unknown) => {
    report(action, error);
  });
}

function report(action: string, error: unknown): void {
  console.error(`Clipped could not ${action}.`, error);
}
