import type { RecorderStatus } from '@clipped/shared';

import { formatElapsed, type RecorderProblem } from './recording';
import type { RecorderLinkState } from './useRecorderLink';

/**
 * What the window can honestly say about the recording happening now.
 *
 * Beside `HomeScreen.tsx` rather than inside it, for the reason
 * `describeGameDetection` sits beside the Games screen: the wording is the part
 * worth testing, and a module exporting both a component and a function is one
 * neither Fast Refresh nor a reader can take apart.
 *
 * # Where the two arguments come from, and why there are two
 *
 * **The link** says whether there is a recorder at all: connecting, attached,
 * reconnecting, unavailable, or none of those because this is a browser tab
 * rather than the Clipped window. It is pushed, and it is the only thing that
 * can answer "is there anything to ask".
 *
 * **The status** is that recorder's own answer to `get_status`, asked once a
 * second by `useRecording` while this screen is open. It is what says whether a
 * recording is running and how long it has been running, and it is deliberately
 * *not* taken from the status inside the link: the recorder publishes
 * `status_changed` when a recording starts and when it ends and at no point
 * between (`apps/recorder/src/serve.rs`), so the `elapsed_ms` in the link is the
 * figure from the moment the recording began and never moves.
 *
 * The distinction is the whole of
 * [issue #389](https://github.com/wildware-uk/clipped/issues/389)'s third
 * acceptance criterion. A window that counted up from that figure with a timer
 * of its own, or that decided it was recording because the button had been
 * pressed, would go on saying "recording" after the recorder had died. Asking
 * means it stops saying so within one interval.
 */

/** The few words shown as the state, one sentence, the duration, and the file. */
export interface RecordingNowText {
  /**
   * The state, in a few words.
   *
   * A state that is a claim has to carry whom it is a claim about. "This
   * recorder is not recording" is true; "Nothing is being recorded" would be a
   * statement about the machine, and a `clipped-recorder watch` started in a
   * terminal serves no protocol and is invisible to this link — so it could be
   * recording a game while this window said otherwise. The same rule
   * `describeGameDetection` follows, for the same reason.
   */
  readonly state: string;
  /** One sentence saying what that means for the person reading it. */
  readonly detail: string;
  /**
   * How long the recording has been running, as the recorder measured it, or
   * `undefined` when nothing is being recorded.
   *
   * Every one of these is a figure that arrived in an answer to `get_status`.
   * There is no branch that computes one from a clock in this window: a
   * duration that kept counting while the recorder was gone would be the
   * invented figure AGENTS.md section 27 forbids, and it would be indefensible
   * precisely when it mattered.
   */
  readonly elapsed?: string;
  /**
   * The file being written, in full, or `undefined` when nothing is.
   *
   * Never abbreviated: it is the thing on this screen anybody can act on, and a
   * path with a middle ellipsis cannot be typed into Explorer (AGENTS.md
   * sections 28 and 45).
   */
  readonly output?: string;
}

/**
 * What to say about the recording in progress, given where the link stands and
 * what the recorder last answered.
 *
 * A pure function so that every combination has exactly one rendering rather
 * than a chain of conditions inside a component, and so that the wording can be
 * tested without a window.
 */
export function describeRecordingNow(
  link: RecorderLinkState | null,
  status: RecorderStatus | null,
  problem: RecorderProblem | null,
): RecordingNowText {
  if (link === null) {
    return {
      state: 'Not known',
      detail:
        'This page is not the Clipped window, so there is no recorder to ask. Run npm run dev.',
    };
  }

  switch (link.link) {
    case 'connecting':
      return { state: 'Not known yet', detail: 'Looking for the recorder.' };
    case 'reconnecting':
      return {
        state: 'Not known',
        detail: `${link.reason} Attempt ${String(link.attempt)} of ${String(link.attempts_allowed)}.`,
      };
    case 'unavailable':
      return {
        state: 'Not known',
        detail: `This window is not attached to a recorder, so it has nothing to ask. ${link.reason}`,
      };
    case 'attached':
      return whatTheRecorderSaid(status, problem);
  }
}

/**
 * What to say about a recorder that is there to be asked.
 *
 * Three answers, and the order matters. A failed ask comes first, because a
 * recorder that did not answer is the case a window must not paper over with
 * the answer it got a second ago — that is exactly the "says recording while the
 * recorder has died" failure. Not having asked yet comes next, and is not the
 * same as idle. Only then is there a status to draw.
 */
function whatTheRecorderSaid(
  status: RecorderStatus | null,
  problem: RecorderProblem | null,
): RecordingNowText {
  if (problem !== null) {
    return {
      state: 'Not known',
      detail: `This window asked the recorder what it is doing and did not get an answer. ${problem.message}`,
    };
  }

  if (status === null) {
    return { state: 'Not known yet', detail: 'Asking the recorder what it is doing.' };
  }

  if (status.state === 'recording') {
    return {
      state: `Recording ${status.target}`,
      detail: 'The file below is being written now, and is playable as it grows.',
      elapsed: formatElapsed(status.elapsed_ms),
      output: status.output,
    };
  }

  return {
    state: 'This recorder is not recording',
    detail:
      'The recorder this window is attached to is running and idle. A recorder started ' +
      'elsewhere — clipped-recorder watch, from a terminal — serves no protocol and is ' +
      'invisible here, so this says nothing about the rest of the machine.',
  };
}

/**
 * Where a recording goes, which is the question Home replaces the library with.
 *
 * True in every link state, so it is said once rather than folded into the
 * renderings above — and it is the useful answer to "where are my recordings?"
 * on a screen that cannot list one (AGENTS.md section 45).
 */
export const WHERE_RECORDINGS_GO =
  'Recordings are ordinary files, and stay usable without Clipped. Each sitting writes its ' +
  'video files and one session record beside them, in the output folder the recorder was given ' +
  '(docs/sessions.md). Nothing in this window has read that folder.';
