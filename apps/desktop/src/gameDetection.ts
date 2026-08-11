import type { RecorderLinkState } from './useRecorderLink';

/**
 * What the window can honestly say about game detection.
 *
 * Beside `GamesScreen.tsx` rather than inside it for the same reason
 * `describeRecorderLink` sits beside the shell: the wording is the part worth
 * testing, and a module that exports a component and a function is a module
 * neither Fast Refresh nor a reader can take apart.
 */

/** The two or three words shown as the detection state, and one sentence. */
export interface GameDetectionText {
  /** The state, in a few words. */
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
 * The claim made for an *attached* recorder deserves its justification, because
 * it is the only one here that is an inference rather than a reading. The
 * supervisor starts, and can only attach to, `clipped-recorder serve` (`SERVE`
 * in `crates/ipc/src/supervisor.rs`), and `serve` is the only subcommand that
 * listens on the endpoint. `serve` does not watch for games: `clipped-recorder
 * watch` is a separate subcommand that takes no `--endpoint`, which is the
 * whole of issue #241's fourth acceptance criterion. So a recorder this window
 * can see is a recorder that is not detecting games, and saying so is a reading
 * of the build rather than a guess about it.
 *
 * "Not known" and "Not detecting games" are deliberately different answers. A
 * link that has not settled has established nothing, and a screen that said
 * detection was off while it was still looking would be making a claim nobody
 * measured (AGENTS.md section 27).
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
        state: 'Not detecting games',
        detail: `There is no recorder to detect them. ${link.reason}`,
      };
    case 'attached':
      return {
        state: 'Not detecting games',
        detail:
          'The recorder this window is attached to runs clipped-recorder serve, which records ' +
          'what it is asked to record. Games are detected by clipped-recorder watch, which is a ' +
          'separate mode and serves no protocol, so this window can neither start it nor follow ' +
          'it.',
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
