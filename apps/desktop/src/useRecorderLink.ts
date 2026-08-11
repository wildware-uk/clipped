import { type RecorderStatus, type RecordingStatus } from '@clipped/shared';
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

/**
 * The recording a recorder was writing when it died: `ActiveRecording` in
 * `crates/ipc`.
 *
 * Derived from the protocol's own `RecordingStatus` rather than written out a
 * second time (AGENTS.md section 55). The two are the same fields; the `state`
 * tag is what a *status* carries and an interruption does not, because the
 * recorder that would have had a state is gone.
 */
export type InterruptedRecording = Omit<RecordingStatus, 'state'>;

/** Everything the Rust side sends on the `recorder-link` event. */
type RecorderLinkEvent =
  | { readonly event: 'state'; readonly [key: string]: unknown }
  | ({ readonly event: 'recording_interrupted' } & InterruptedRecording)
  | { readonly event: 'recording_failed'; readonly [key: string]: unknown };

/**
 * Everything the window knows about the recorder.
 *
 * Two fields rather than one because they answer different questions and have
 * different lifetimes. `link` is where the connection stands *now*; `interrupted`
 * is something that happened, stays true afterwards, and is the whole of what
 * [ADR 0006](../../../docs/adr/0006-recorder-lifetime-and-supervision.md)'s
 * recovery produces — "recovery names the file; it does not resume the
 * recording". Folding it into `link` would lose it at the next state change,
 * which for a killed recorder is about a second later.
 */
export interface RecorderLinkView {
  /** Where the link with the recorder stands, or `null` outside the window. */
  readonly link: RecorderLinkState | null;
  /** The most recent recording a recorder died in the middle of, if any. */
  readonly interrupted: InterruptedRecording | null;
}

/** The name the Rust side emits under. */
const LINK_EVENT = 'recorder-link';

/**
 * The state inside a `state` event.
 *
 * The Rust side tags the event with `event: "state"` and flattens the state
 * beside it, so the state is the payload without that one field.
 */
function withoutTag<T>(payload: RecorderLinkEvent): T {
  const copy: Record<string, unknown> = { ...payload };
  delete copy.event;
  return copy as unknown as T;
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
 * A `link` of `null` means this interface is not running inside the Clipped
 * window, which is a different thing from the recorder being unreachable and is
 * rendered differently.
 *
 * The window asks once and then follows the event, because both carry the whole
 * state rather than a delta: a window that missed an event recovers on the next
 * one. The two are independent round trips, so the answer to the question can
 * arrive *after* an event that supersedes it; `superseded` is what stops that
 * answer overwriting newer state, because a snapshot from before the last event
 * is exactly the stale reading AGENTS.md section 27 is about.
 */
export function useRecorderLink(): RecorderLinkView {
  const [state, setState] = useState<RecorderLinkState | null>(null);
  const [interrupted, setInterrupted] = useState<InterruptedRecording | null>(null);

  useEffect(() => {
    if (!inTauriWindow()) {
      return;
    }

    let current = true;
    let superseded = false;

    invoke<RecorderLinkState>('recorder_link_state')
      .then((answer) => {
        if (current && !superseded) {
          setState(answer);
        }
      })
      .catch((error: unknown) => {
        // The command exists in every build that has this window, so a failure
        // here is a bug rather than a state. Reporting it as "unavailable" with
        // the reason is better than an interface that shows nothing at all.
        if (current && !superseded) {
          setState({ link: 'unavailable', reason: String(error) });
        }
      });

    const subscription = listen<RecorderLinkEvent>(LINK_EVENT, ({ payload }) => {
      if (!current) {
        return;
      }

      if (payload.event === 'state') {
        superseded = true;
        setState(withoutTag<RecorderLinkState>(payload));
        return;
      }

      if (payload.event === 'recording_interrupted') {
        // The one piece of information the recovery design exists to produce:
        // a recorder died mid-recording and left a playable file at a path
        // nobody else will ever tell the user about (ADR 0006). Kept until the
        // window closes, because it stays true — nothing later makes that file
        // any less real.
        setInterrupted(withoutTag<InterruptedRecording>(payload));
      }

      // `recording_failed` is a recording that failed while the recorder stayed
      // up, which the state that follows it already describes. Giving it a
      // notice of its own is issue #53.
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
        // Tauri types `UnlistenFn` as returning nothing, and it returns a
        // promise: unsubscribing is a round trip to the Rust side like any
        // other. Wrapped so that the round trip is part of this chain, because
        // a bare call leaves its rejection unhandled, and an unhandled
        // rejection in a webview is a console error nobody reads.
        .then((unlisten) => Promise.resolve<void>(unlisten?.()))
        .catch(() => {
          // Nothing to do: the listener is going away with the window.
        });
    };
  }, []);

  return { link: state, interrupted };
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

/**
 * What to say about a recording a recorder died in the middle of.
 *
 * Three things, and only those three, because they are what
 * [ADR 0006](../../../docs/adr/0006-recorder-lifetime-and-supervision.md)
 * settled recovery *is*: that a recording was interrupted, where the file it
 * left is, and that it was not resumed. Resuming is not something Clipped can
 * do honestly — a replacement recorder cannot go on writing a container another
 * process opened — so saying where the file is, is the whole of what can be
 * offered, and a user who is not told is a user who never finds it.
 *
 * The path is named in full and never abbreviated: it is the only thing here
 * that anybody can act on (AGENTS.md sections 28 and 45).
 */
export function describeInterruption(interrupted: InterruptedRecording | null): string | undefined {
  if (interrupted === null) {
    return undefined;
  }

  return `Recording interrupted. Not resumed. The file is at ${interrupted.output}.`;
}
