import {
  type Feature,
  type ProtocolError,
  type RecorderStatus,
  type RecordingStatus,
  type SessionSummary,
} from '@clipped/shared';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { useEffect, useRef, useState } from 'react';

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
      /**
       * What this recorder can do, from its handshake.
       *
       * Asked before a control that maps to a feature-gated command draws
       * itself. Without it the control is drawn against an older recorder, the
       * user chooses a file name, and *then* the command is refused with
       * `unknown_command` — a refusal arriving after the only part of the
       * interaction that cost them anything (issue #447).
       *
       * The names are `@clipped/shared`'s `FEATURES`. An unknown one is
       * carried, not refused: a newer recorder may name a capability this build
       * has no control for, and dropping it would make the list describe this
       * build rather than that recorder.
       */
      readonly features: readonly string[];
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

/**
 * A recording that ended because something went wrong, as the window saw it.
 *
 * The event carries a recording identifier and a refusal, and no path — so the
 * file is named from the last `recording` status this window saw, and **only**
 * when that status is the recording the failure names. A failure for a recording
 * this window never saw claims no file, because guessing that the last file seen
 * was the one that failed would put a different recording in front of somebody
 * diagnosing this one. That is the same rule `src-tauri/src/notifications.rs`
 * applies to the toast's "Show the file" button, kept in one form in each
 * process.
 */
export interface RecordingFailure {
  /** Which recording failed. */
  readonly recording_id: string;
  /** The recorder's own refusal: a stable code, and a sentence for a person. */
  readonly error: ProtocolError;
  /** The file it was writing, when this window knows which that was. */
  readonly output: string | null;
  /** When this window was told. The protocol carries no time of its own. */
  readonly seenAt: Date;
}

/** Everything the Rust side sends on the `recorder-link` event. */
type RecorderLinkEvent =
  | { readonly event: 'state'; readonly [key: string]: unknown }
  | ({ readonly event: 'recording_interrupted' } & InterruptedRecording)
  /*
   * The sitting's own fields sit *beside* the tag rather than under a `session`
   * key: `RecorderLinkEvent::SessionEnded` is a newtype in an internally tagged
   * enumeration, and `a_sitting_that_ended_survives_the_journey_into_a_window`
   * in `crates/ipc/src/supervisor/link.rs` is what holds that shape still. The
   * protocol's own `session_ended` frame nests it; this is the link's
   * re-announcement of it into the webview, and they are not the same shape.
   */
  | ({ readonly event: 'session_ended' } & SessionSummary)
  | {
      readonly event: 'recording_failed';
      readonly recording_id: string;
      readonly error: ProtocolError;
    };

/**
 * Everything the window knows about the recorder.
 *
 * Four fields rather than one because they answer different questions and have
 * different lifetimes.
 *
 * `link` is where the connection stands *now*, and `observedAt` is when that was
 * last true. The pair is what stops a figure inside a status being read as
 * current: `elapsed_ms` is measured at the moment the recorder answered, and a
 * status from four minutes ago says a recording had run for eight seconds
 * (AGENTS.md section 27). Nothing polls, because the link republishes on every
 * change; what the Diagnostics screen needs is not a fresher figure but an honest
 * label on the one there is.
 *
 * `interrupted`, `failed` and `ended` are things that *happened*, stay true
 * afterwards, and are each dropped by the state that follows them about a second
 * later. `interrupted` is the whole of what
 * [ADR 0006](../../../docs/adr/0006-recorder-lifetime-and-supervision.md)'s
 * recovery produces — "recovery names the file; it does not resume the
 * recording". `failed` is the one thing a support report about a failed recording
 * is for: a recorder that stayed up reports the failure once and then reports
 * "idle", and after that nothing in the window remembers that anything went wrong
 * (issue #101).
 *
 * `ended` is the third of that kind. A recording somebody stopped explains itself
 * in the reply to their stop; a recording that ended *by itself* has no reply,
 * and the sitting the recorder announces as it closes is the only thing the
 * window is ever told about it. Without it the panel goes from "Recording cs2.exe"
 * to "not recording" and takes the file with it, saying nothing about why — which
 * is what a recording cut short by its window being dragged to a new size looked
 * like (issue #625, ADR 0012).
 */
export interface RecorderLinkView {
  /** Where the link with the recorder stands, or `null` outside the window. */
  readonly link: RecorderLinkState | null;
  /** When `link` was last set, or `null` if it never has been. */
  readonly observedAt: Date | null;
  /** The most recent recording a recorder died in the middle of, if any. */
  readonly interrupted: InterruptedRecording | null;
  /** The most recent recording that ended because something failed, if any. */
  readonly failed: RecordingFailure | null;
  /**
   * The most recent sitting the recorder announced the end of, if any.
   *
   * The whole sitting rather than a verdict about it, because which of its files
   * is being talked about, and why that one ended, are both inside it — and a
   * screen that was handed a verdict could not tell a sitting that ended in one
   * file from one that went on into a second (ADR 0012).
   */
  readonly ended: SessionSummary | null;
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
export function inTauriWindow(): boolean {
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
  const [observedAt, setObservedAt] = useState<Date | null>(null);
  const [interrupted, setInterrupted] = useState<InterruptedRecording | null>(null);
  const [failed, setFailed] = useState<RecordingFailure | null>(null);
  const [ended, setEnded] = useState<SessionSummary | null>(null);
  /*
   * The last recording this window saw the recorder running, which is the only
   * way a `recording_failed` can be given a file: the event carries an
   * identifier and no path. A ref rather than state because nothing renders it —
   * it is read inside the subscription, where a state value would be the one
   * captured when the effect ran, which is `null` for ever.
   */
  const lastRecording = useRef<RecordingStatus | null>(null);

  useEffect(() => {
    if (!inTauriWindow()) {
      return;
    }

    let current = true;
    let superseded = false;

    /** Records a state and when it was seen, so a stale figure reads as one. */
    const observe = (link: RecorderLinkState): void => {
      if (link.link === 'attached' && link.status.state === 'recording') {
        lastRecording.current = link.status;
      }
      setState(link);
      setObservedAt(new Date());
    };

    invoke<RecorderLinkState>('recorder_link_state')
      .then((answer) => {
        if (current && !superseded) {
          observe(answer);
        }
      })
      .catch((error: unknown) => {
        // The command exists in every build that has this window, so a failure
        // here is a bug rather than a state. Reporting it as "unavailable" with
        // the reason is better than an interface that shows nothing at all.
        if (current && !superseded) {
          observe({ link: 'unavailable', reason: String(error) });
        }
      });

    const subscription = listen<RecorderLinkEvent>(LINK_EVENT, ({ payload }) => {
      if (!current) {
        return;
      }

      if (payload.event === 'state') {
        superseded = true;
        observe(withoutTag<RecorderLinkState>(payload));
        return;
      }

      if (payload.event === 'recording_interrupted') {
        // The one piece of information the recovery design exists to produce:
        // a recorder died mid-recording and left a playable file at a path
        // nobody else will ever tell the user about (ADR 0006). Kept until the
        // window closes, because it stays true — nothing later makes that file
        // any less real.
        setInterrupted(withoutTag<InterruptedRecording>(payload));
        return;
      }

      if (payload.event === 'session_ended') {
        // A sitting the recorder has closed, kept for the same reason the two
        // above are: the state that follows it a moment later is "idle", which
        // is true and says nothing about what just happened. It is the only
        // thing this window is ever told about a recording that ended without
        // being asked to, and it carries each file's own reason for ending
        // (issue #625).
        setEnded(withoutTag<SessionSummary>(payload));
        return;
      }

      if (payload.event === 'recording_failed') {
        // A recording that failed while the recorder stayed up. The state that
        // follows it a moment later is "idle", which is true and says nothing
        // about what happened, so this is kept: it is what a support report
        // about a failed recording is made of (issue #101). A notice of its own
        // in the status block is still issue #53.
        const seen = lastRecording.current;
        setFailed({
          recording_id: payload.recording_id,
          error: payload.error,
          output: seen?.recording_id === payload.recording_id ? seen.output : null,
          seenAt: new Date(),
        });
      }
    }).catch((error: unknown) => {
      // Subscribing needs `core:event:allow-listen` in
      // `src-tauri/capabilities/default.json`; without it Tauri rejects this
      // and the first answer above would be the last thing the window ever
      // learned, going stale in silence. A window that cannot follow the
      // recorder has to say so rather than show an answer from a minute ago
      // (AGENTS.md section 27).
      if (current) {
        observe({
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

  return { link: state, observedAt, interrupted, failed, ended };
}

/** The two or three words shown as the recorder's state, and one sentence. */
export interface RecorderStatusText {
  readonly state: string;
  readonly detail: string;
}

/**
 * The game the recorder's sitting is of, when the catalogue named one.
 *
 * Asked of the whole status rather than of each state, because a sitting spans
 * two of them: one being recorded is on `recording` and one waiting out its
 * restart grace is on `watching`, and they are the same sitting a few seconds
 * apart. `tray_model.rs` asks the protocol's own `RecorderStatus::session` for
 * exactly this, on the Rust side of the same window.
 *
 * `undefined` for a sitting the catalogue would not attribute and for a
 * recording that belongs to no sitting. Neither is a name to invent.
 */
function gameIn(status: RecorderStatus): string | undefined {
  return status.state === 'idle' ? undefined : status.session?.game_name;
}

/**
 * What to show for a link state.
 *
 * A pure function so that the wording is testable without a window, and so that
 * every state has exactly one rendering rather than a chain of conditions inside
 * a component.
 *
 * The `attached` arm has one rendering per `RecorderStatus` for the same
 * reason, and `watching` is one of them: a recorder that will record the next
 * game to launch and one that will never record anything are different answers,
 * and this block drew both as "Idle" until issue #588 — the same collapse issue
 * #584 removed from the protocol, surviving in the one place a user sees it on
 * every screen.
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
      return describeRecorderStatus(link.status);
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
 * The same two things, for what an attached recorder says it is doing.
 *
 * One arm per status and no default, so a fourth state added to the protocol
 * has to be decided here rather than drawn as whichever arm happened to be
 * last — `noImplicitReturns` is what enforces that.
 */
function describeRecorderStatus(status: RecorderStatus): RecorderStatusText {
  switch (status.state) {
    case 'recording':
      return {
        state: 'Recording',
        // The game where the recording knows one. `target` is a capture
        // selector — `process 4242` for a recording nobody asked for — and
        // turning one into "Counter-Strike 2" needs the catalogue, which lives
        // in the recorder; the sitting on the status is the recorder having
        // already done it (issue #241).
        detail: `Recording ${gameIn(status) ?? status.target}.`,
      };
    case 'watching': {
      const game = gameIn(status);
      return {
        state: 'Watching',
        detail:
          game === undefined
            ? 'The recorder is watching for a game to launch, and will record the next one.'
            : `The recorder is in a ${game} sitting, and will record it again if it starts.`,
      };
    }
    case 'idle':
      return {
        state: 'Idle',
        detail: 'The recorder is running. Nothing is being recorded.',
      };
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

/**
 * Whether the recorder this window is attached to can do `feature`.
 *
 * The check a control makes before it draws itself. `false` while connecting,
 * reconnecting or unavailable, which is the honest answer: there is no recorder
 * to have the capability, and a control drawn on the strength of the last one
 * is a control that refuses when pressed.
 *
 * **It is not enough on its own to word a disabled control**, because those
 * three states are `false` for three different reasons and only one of them is
 * "this recorder cannot". `exportOffer` in `recordingActions.ts` is what that
 * looks like done properly, and is the shape any second control should copy:
 * this answers whether, and something beside it answers what to say (issue
 * #447).
 *
 * Takes the link rather than a `Welcome` because that is what a window holds —
 * `hasFeature` in `@clipped/shared` is the same question asked of the handshake
 * itself, which only `crates/ipc` ever sees.
 */
export function recorderCanDo(link: RecorderLinkState | null, feature: Feature): boolean {
  return link?.link === 'attached' && link.features.includes(feature);
}
