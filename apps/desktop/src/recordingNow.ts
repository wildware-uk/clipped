import type { RecorderStatus, SessionSummary } from '@clipped/shared';

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
      // The game where the recording knows one. `target` is the capture
      // selector — `process 4242` for a recording nobody asked for — and only
      // the recorder's catalogue can turn one into "Counter-Strike 2", which is
      // why the sitting is on the status at all (issue #241).
      state: `Recording ${status.session?.game_name ?? status.target}`,
      detail: 'The file below is being written now, and is playable as it grows.',
      elapsed: formatElapsed(status.elapsed_ms),
      output: status.output,
    };
  }

  // Nothing is being recorded — and what happens next is the difference between
  // these two. A recorder that is watching will record the next game to launch;
  // an idle one will record nothing until it is asked. Drawing both as "not
  // recording" is the collapse issue #584 took out of the protocol, and it
  // survived here until issue #588.
  if (status.state === 'watching') {
    const game = status.session?.game_name;
    return {
      state: 'This recorder is watching for a game',
      detail:
        game === undefined
          ? 'The recorder this window is attached to is not recording, and will record the next ' +
            'game that launches.'
          : `The recorder this window is attached to is in a ${game} sitting. Nothing is being ` +
            'recorded, and it will record that game again if it starts.',
    };
  }

  return {
    state: 'This recorder is not recording',
    detail:
      'The recorder this window is attached to is running and idle, and will record nothing ' +
      'until it is asked. A recorder started elsewhere — clipped-recorder watch, from a ' +
      'terminal — serves no protocol and is invisible here, so this says nothing about the rest ' +
      'of the machine.',
  };
}

/**
 * The word the recorder uses for a recording its capture target changed size
 * under.
 *
 * The hyphenated spelling, because that is what `SessionRecording.end_reason`
 * carries: the sidecar's and the index's vocabulary rather than the underscored
 * `EndReason` a `stop_recording` replies with. Written out here so the one place
 * that compares it is the one place that has to be changed if it ever moves.
 */
const ENDED_BY_RESIZE = 'target-resized';

/**
 * The word the recorder uses for a sitting that was one recording somebody asked
 * for.
 *
 * Only a sitting a `start_recording` opened ends this way: it has no game whose
 * exit could end it and no grace period to wait through, so it is over when its
 * recording is (`clipped_session::automatic::SessionEndReason::RecordingEnded`).
 * An automatic sitting ends `game-exited`, `system-resumed` or
 * `recorder-stopping`, which is what makes this an exact test rather than a
 * guess at which kind of sitting arrived.
 */
const A_RECORDING_SOMEBODY_ASKED_FOR = 'recording-ended';

/** What to say about a sitting a size change brought to an end, and its file. */
export interface ResizeEndingText {
  /** The sentences: why it ended, and what this mode does about it. */
  readonly detail: string;
  /**
   * The file that exists, in full.
   *
   * Never abbreviated, for the reason {@link RecordingNowText.output} is not: it
   * is the only thing here anybody can act on, and a path with a middle ellipsis
   * cannot be typed into Explorer (AGENTS.md sections 28 and 45).
   */
  readonly output: string;
}

/**
 * What to say about a sitting that ended because its window changed size.
 *
 * [ADR 0012](../../../docs/adr/0012-a-session-follows-a-resize-with-a-new-file.md)
 * settled that one file cannot hold two sizes, so a size change finishes the
 * file it happens in. What follows depends on who asked for the recording: an
 * *automatic* sitting starts the next file immediately, and a recording somebody
 * asked for — a `clipped-recorder record`, or this window's own record control —
 * stops there. [Issue #625](https://github.com/wildware-uk/clipped/issues/625)
 * is that the second case said nothing at all: the panel went from "Recording
 * cs2.exe" to "not recording" and took the path with it, so a sitting cut short
 * by somebody dragging a window looked exactly like one that ran to the end.
 *
 * Two things are asked, and both are needed. The sitting must be one somebody
 * asked for, which its own `end_reason` says exactly; and its **last** file must
 * be the one the resize finished. The first is what stops this sentence being
 * drawn over an automatic sitting, where "not carried on into a second one"
 * would be the opposite of the truth — a game that exits in the seconds after a
 * resize leaves an automatic sitting whose last file also ended
 * `target-resized`, and the shape alone cannot tell the two apart. The second is
 * what makes it about the resize rather than about the sitting: a recording
 * somebody stopped, or one whose window closed, ends the same sitting the same
 * way and says something else entirely.
 *
 * `undefined` for every other sitting, including one still open. A sentence
 * about a resize over a recording somebody stopped would be the invented state
 * AGENTS.md section 27 forbids.
 */
export function describeResizeEnding(ended: SessionSummary | null): ResizeEndingText | undefined {
  if (ended?.end_reason !== A_RECORDING_SOMEBODY_ASKED_FOR) {
    return undefined;
  }

  const last = ended.recordings.at(-1);
  if (last === undefined || last.end_reason !== ENDED_BY_RESIZE) {
    return undefined;
  }

  return {
    detail:
      'Recording ended because the window changed size. Clipped cannot put two sizes in one ' +
      'file, and a recording you asked for is not carried on into a second one — automatic ' +
      'recording is, and would have started a new file at the new size. Everything up to the ' +
      'change is in the file below, finished and playable.',
    output: last.output,
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
