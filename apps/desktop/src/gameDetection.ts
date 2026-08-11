import type { RecorderLinkState } from './useRecorderLink';

/**
 * What the window can honestly say about game detection.
 *
 * Beside `GamesScreen.tsx` rather than inside it for the same reason
 * `describeRecorderLink` sits beside the shell: the wording is the part worth
 * testing, and a module that exports a component and a function is a module
 * neither Fast Refresh nor a reader can take apart.
 */

/** The few words shown as the detection state, and one sentence explaining it. */
export interface GameDetectionText {
  /**
   * The state, in a few words.
   *
   * A phrase rather than a label, because a state that is a claim has to carry
   * whom it is a claim about: "This recorder is not detecting games" is true,
   * and the "Not detecting games" it replaced was a statement about the machine
   * that this window has no way to make.
   */
  readonly state: string;
  /** One sentence saying what that means for the person reading it. */
  readonly detail: string;
}

/**
 * What to say about game detection, given where the recorder link stands.
 *
 * A pure function for the same two reasons `describeRecorderLink` is one: the
 * wording is testable without a window, and every link state has exactly one
 * rendering rather than a chain of conditions inside a component.
 *
 * # What the link can and cannot establish
 *
 * The link sees exactly one thing: the recorder this window started or attached
 * to. `clipped-recorder watch` serves no protocol, so a watcher somebody started
 * in a terminal is invisible to it — and [`WHAT_WORKS_TODAY`] below tells the
 * reader to start exactly that. **So no rendering here may say that games are
 * going undetected on this machine.** The window has not looked, and cannot.
 *
 * Only one of the five renderings claims anything about detection at all, and it
 * names the recorder it is talking about. That claim is an inference rather than
 * a reading, and it holds: the supervisor starts, and can only attach to,
 * `clipped-recorder serve` (`SERVE` in `crates/ipc/src/supervisor.rs`), and
 * `serve` is the only subcommand that listens on the endpoint. `serve` does not
 * watch for games: `clipped-recorder watch` is a separate subcommand that takes
 * no `--endpoint`, which is the whole of issue #241's fourth acceptance
 * criterion. So a recorder this window can see is a recorder that is not
 * detecting games — which is a statement about that recorder and nothing else.
 *
 * The other four say "Not known", including the one for a recorder that could
 * not be reached at all. A window with no link has established nothing, so
 * saying detection was off would be a claim nobody measured, and would
 * contradict the sentence directly beneath it (AGENTS.md section 27).
 */
export function describeGameDetection(link: RecorderLinkState | null): GameDetectionText {
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
      return {
        state: 'This recorder is not detecting games',
        detail:
          'The recorder this window is attached to runs clipped-recorder serve, which records ' +
          'what it is asked to record. Games are detected by clipped-recorder watch, which is a ' +
          'separate mode and serves no protocol, so this window can neither start one nor see ' +
          'one that is already running.',
      };
  }
}

/**
 * The one thing anybody reading this screen can act on today.
 *
 * It is true in every link state, so it is stated once rather than repeated
 * into all five renderings above. A screen that only says what is missing
 * leaves somebody with nothing to do; automatic recording is built and running,
 * and the sentence says where (AGENTS.md section 45, `docs/sessions.md`).
 */
export const WHAT_WORKS_TODAY =
  'Automatic recording does work, from a terminal: clipped-recorder watch records a game as it ' +
  'launches and writes a session record beside the files. Making that something this window can ' +
  'start and follow is issue #241.';
