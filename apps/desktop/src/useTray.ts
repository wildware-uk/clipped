import { listen } from '@tauri-apps/api/event';
import { useEffect, useState } from 'react';

/**
 * The two things the notification-area menu says to the window.
 *
 * The tray lives in the Rust side (`src-tauri/src/tray.rs`) because a webview
 * cannot own a tray icon, so everything it does that the window should know
 * about arrives as an event.
 *
 * - `tray-navigate` carries a route: Open Library and Settings raise the window
 *   and send it somewhere.
 * - `tray-notice` carries a sentence: a tray action that failed has nowhere of
 *   its own to report it — the menu is closed by the time the recorder answers —
 *   so the window comes up carrying the message (AGENTS.md section 45).
 */
const NAVIGATE_EVENT = 'tray-navigate';

/** The event a failed tray action arrives on. */
const NOTICE_EVENT = 'tray-notice';

/**
 * Whether this document is inside the Tauri window rather than a browser tab.
 *
 * The same check `useRecorderLink` makes, and for the same reason: `npm run
 * dev:web` serves this interface to a browser, where there is no tray and no
 * Rust side to hear from.
 */
function inTauriWindow(): boolean {
  return '__TAURI_INTERNALS__' in window;
}

/**
 * Follows the tray, and returns the last thing it had to report.
 *
 * `onNavigate` is called with a route whenever the tray sends the window
 * somewhere. It is not part of the return value because navigation is an event
 * rather than a state: acting on it twice — which storing it and rendering from
 * it would invite — would fight the user's own navigation.
 *
 * The notice **is** state, and it stays until the tray has something else to
 * say. A message about a recording that could not be started does not stop being
 * true because a second passed, and there is nowhere else in the window it could
 * be read.
 */
export function useTray(onNavigate: (path: string) => void): string | undefined {
  const [notice, setNotice] = useState<string | undefined>(undefined);

  useEffect(() => {
    if (!inTauriWindow()) {
      return;
    }

    let current = true;

    const navigation = listen<string>(NAVIGATE_EVENT, ({ payload }) => {
      if (current) {
        onNavigate(payload);
      }
    }).catch(() => {
      // Subscribing needs `core:event:allow-listen`, which the window's
      // capabilities grant. Without it the tray's Open Library would raise the
      // window and leave it on whatever screen it was already on — visibly
      // wrong rather than silently, which is the best that can be done from
      // this side.
      return undefined;
    });

    const notices = listen<string>(NOTICE_EVENT, ({ payload }) => {
      if (current) {
        setNotice(payload);
      }
    }).catch(() => undefined);

    return () => {
      current = false;
      for (const subscription of [navigation, notices]) {
        subscription
          // `UnlistenFn` is typed as returning nothing and returns a promise:
          // unsubscribing is a round trip to the Rust side like any other.
          .then((unlisten) => Promise.resolve<void>(unlisten?.()))
          .catch(() => {
            // Nothing to do: the listener is going away with the window.
          });
      }
    };
  }, [onNavigate]);

  return notice;
}

/**
 * The one sentence the status block shows, out of the ones there are.
 *
 * Both a tray notice and an interrupted recording are things that happened and
 * stay true, and the status block has room for one paragraph. Neither is dropped
 * — the interrupted recording names a file nothing else will ever mention, and
 * the tray notice is the result of something the user did a second ago — so they
 * are joined, newest first.
 */
export function joinNotices(...notices: readonly (string | undefined)[]): string | undefined {
  const said = notices.filter((notice): notice is string => notice !== undefined);
  return said.length === 0 ? undefined : said.join(' ');
}
