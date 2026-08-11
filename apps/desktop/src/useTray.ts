import { invoke } from '@tauri-apps/api/core';
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
 * The command that reports something that failed before this window existed.
 *
 * The tray is built during Tauri's `setup`, which runs before React does, so a
 * failure there cannot be *sent* anywhere — there is nothing subscribed yet. It
 * is kept on the Rust side instead and asked for once, here. The one that
 * matters is a notification-area icon that could not be added: without it,
 * closing this window quits Clipped rather than minimising, and quitting leaves
 * the recorder running.
 */
const STARTUP_NOTICE_COMMAND = 'startup_notice';

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
 *
 * The first notice may be older than this window: `startup_notice` is asked for
 * once on mount, and carries anything that failed while Tauri was setting up —
 * a tray icon that could not be added, most of all. Anything the tray says
 * afterwards replaces it, because that is the newer thing to have happened.
 */
export function useTray(onNavigate: (path: string) => void): string | undefined {
  const [notice, setNotice] = useState<string | undefined>(undefined);

  // Its own effect, and deliberately not `onNavigate`'s: this asks a question
  // once, and re-running it whenever a callback changed identity would put a
  // dismissed-by-a-newer-notice startup failure back on screen.
  useEffect(() => {
    if (!inTauriWindow()) {
      return;
    }

    let current = true;
    invoke<string | null>(STARTUP_NOTICE_COMMAND)
      .then((said) => {
        if (current && said !== null) {
          // Behind anything the tray has already said. This is the older event
          // of the two however late the answer arrives.
          setNotice((shown) => shown ?? said);
        }
      })
      .catch(() => {
        // There is nothing useful to say about not being able to ask whether
        // there was anything to say.
      });

    return () => {
      current = false;
    };
  }, []);

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
