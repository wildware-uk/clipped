import { type RecorderStatus } from '@clipped/shared';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { useEffect, useState } from 'react';

/**
 * Where the window's link with the recorder process stands.
 *
 * These four shapes mirror `RecorderLinkState` in `crates/ipc`, which is what
 * the Rust side serialises. They are written here rather than in
 * `packages/shared` because they are not part of the recorder control protocol:
 * no recorder ever sends one, so `crates/ipc`'s schema does not describe one and
 * the conformance test in `packages/shared/src/ipc` has nothing to check them
 * against. What the recorder itself says — `RecorderStatus` — *is* a protocol
 * message, and is taken from `@clipped/shared` rather than written a second time
 * here (AGENTS.md section 55).
 */
export type RecorderLinkState =
  | { readonly link: 'connecting' }
  | {
      readonly link: 'attached';
      readonly recorder_process_id: number;
      readonly status: RecorderStatus;
    }
  | {
      readonly link: 'reconnecting';
      readonly attempt: number;
      readonly attempts_allowed: number;
      readonly delay_ms: number;
      readonly reason: string;
    }
  | { readonly link: 'unavailable'; readonly reason: string };

/** Everything the Rust side sends on the `recorder-link` event. */
type RecorderLinkEvent =
  | { readonly event: 'state'; readonly [key: string]: unknown }
  | { readonly event: 'recording_interrupted'; readonly [key: string]: unknown }
  | { readonly event: 'recording_failed'; readonly [key: string]: unknown };

/** The name the Rust side emits under. */
const LINK_EVENT = 'recorder-link';

/**
 * The state inside a `state` event.
 *
 * The Rust side tags the event with `event: "state"` and flattens the state
 * beside it, so the state is the payload without that one field.
 */
function withoutTag(payload: RecorderLinkEvent): RecorderLinkState {
  const copy: Record<string, unknown> = { ...payload };
  delete copy.event;
  return copy as unknown as RecorderLinkState;
}

/**
 * Whether this document is inside the Tauri window rather than a browser tab.
 *
 * `npm run dev:web` serves the same interface to a browser, where there is no
 * Rust side to ask and therefore no recorder to reach. Saying so is the honest
 * answer; guessing at a state would be the failure AGENTS.md section 27
 * describes.
 */
function inTauriWindow(): boolean {
  return '__TAURI_INTERNALS__' in window;
}

/**
 * Follows the recorder link, or reports that there is none to follow.
 *
 * `null` means this interface is not running inside the Clipped window, which is
 * a different thing from the recorder being unreachable and is rendered
 * differently.
 *
 * The window asks once and then follows the event, because both carry the whole
 * state rather than a delta: a window that missed an event recovers on the next
 * one, and the first answer is never raced by a subscription that started after
 * it.
 */
export function useRecorderLink(): RecorderLinkState | null {
  const [state, setState] = useState<RecorderLinkState | null>(null);

  useEffect(() => {
    if (!inTauriWindow()) {
      return;
    }

    let current = true;

    invoke<RecorderLinkState>('recorder_link_state')
      .then((answer) => {
        if (current) {
          setState(answer);
        }
      })
      .catch((error: unknown) => {
        // The command exists in every build that has this window, so a failure
        // here is a bug rather than a state. Reporting it as "unavailable" with
        // the reason is better than an interface that shows nothing at all.
        if (current) {
          setState({ link: 'unavailable', reason: String(error) });
        }
      });

    const subscription = listen<RecorderLinkEvent>(LINK_EVENT, ({ payload }) => {
      // Only the state; the interruption and failure events are what a
      // notification would render (issues #110 and #53), and this block has
      // nowhere to put them.
      if (current && payload.event === 'state') {
        setState(withoutTag(payload));
      }
    }).catch((error: unknown) => {
      // Subscribing needs `core:event:allow-listen` in
      // `src-tauri/capabilities/default.json`; without it Tauri rejects this
      // and the first answer above would be the last thing the window ever
      // learned, going stale in silence. A window that cannot follow the
      // recorder has to say so rather than show an answer from a minute ago
      // (AGENTS.md section 27).
      if (current) {
        setState({
          link: 'unavailable',
          reason: `This window cannot follow the recorder: ${String(error)}`,
        });
      }
      return undefined;
    });

    return () => {
      current = false;
      subscription
        .then((unlisten) => {
          unlisten?.();
        })
        .catch(() => {
          // Nothing to do: the listener is going away with the window.
        });
    };
  }, []);

  return state;
}

/** The two or three words shown as the recorder's state, and one sentence. */
export interface RecorderStatusText {
  readonly state: string;
  readonly detail: string;
}

/**
 * What to show for a link state.
 *
 * A pure function so that the wording is testable without a window, and so that
 * every state has exactly one rendering rather than a chain of conditions inside
 * a component.
 */
export function describeRecorderLink(link: RecorderLinkState | null): RecorderStatusText {
  if (link === null) {
    return {
      state: 'Not connected',
      detail:
        'This page is not the Clipped window, so it has no recorder to talk to. Run npm run dev.',
    };
  }

  switch (link.link) {
    case 'connecting':
      return { state: 'Connecting', detail: 'Looking for the recorder.' };
    case 'attached':
      return link.status.state === 'recording'
        ? {
            state: 'Recording',
            detail: `Recording ${link.status.target}.`,
          }
        : {
            state: 'Idle',
            detail: 'The recorder is running. Nothing is being recorded.',
          };
    case 'reconnecting':
      return {
        state: 'Reconnecting',
        detail: `${link.reason} Attempt ${String(link.attempt)} of ${String(link.attempts_allowed)}.`,
      };
    case 'unavailable':
      return { state: 'Not available', detail: link.reason };
  }
}
