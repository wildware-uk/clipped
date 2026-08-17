import { recorderCanDo, type RecorderLinkState } from './useRecorderLink';

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
 * in a terminal is invisible to it — and {@link whatWorksToday} tells the reader
 * to start exactly that where nothing else is. **So no rendering here may say
 * that games are going undetected on this machine.** The window has not looked,
 * and cannot.
 *
 * Two of the six renderings claim anything about detection at all, and each
 * names the recorder it is talking about. Both come from the recorder's own
 * welcome rather than from an inference about what `serve` does: since
 * [issue #421](https://github.com/wildware-uk/clipped/issues/421) one binary
 * serves the protocol *and* watches for games, so "the recorder this window can
 * see is one that is not detecting games" stopped being true of the recorder
 * this window itself starts — `SupervisorSettings::watch_for_games` passes
 * `--watch-for-games`. What tells the two apart is `features::AUTOMATIC`, which
 * says the recorder records games by itself and which no recorder advertised
 * until [issue #587](https://github.com/wildware-uk/clipped/issues/587).
 *
 * That distinction is the feature's whole reason for existing. The protocol
 * version says a recorder can *describe* an automatic sitting; the feature says
 * it *makes* them, and a window that drew "Watching for games" from the version
 * alone would say something untrue about a recorder that will never record on
 * its own.
 *
 * The other four say "Not known", including the one for a recorder that could
 * not be reached at all. A window with no link has established nothing, so
 * saying detection was off — or on — would be a claim nobody measured, and would
 * contradict the sentence directly beneath it (AGENTS.md section 27). A recorder
 * that is merely *connecting* is never "cannot": that is the rule
 * [issue #447](https://github.com/wildware-uk/clipped/issues/447) settled, and
 * {@link recorderCanDo} answers `false` for all three unattached states, which
 * is why the capability is asked inside the `attached` arm rather than in front
 * of the switch.
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
      return recorderCanDo(link, 'automatic')
        ? {
            state: 'This recorder is watching for games',
            detail:
              'The recorder this window is attached to said it records games by itself. It holds ' +
              'the catalogue that decides which processes are games, and a game launching starts ' +
              'a sitting and a recording without anybody asking. Whether it is recording one ' +
              'right now is the recorder state in the sidebar.',
          }
        : {
            state: 'This recorder is not detecting games',
            detail:
              'The recorder this window is attached to did not say it records games by itself, ' +
              'so it records what it is asked to and nothing else. A recorder started elsewhere ' +
              '— clipped-recorder watch, from a terminal — serves no protocol and is invisible ' +
              'here, so this says nothing about the rest of the machine.',
          };
  }
}

/**
 * The one thing anybody reading this screen can act on, given the link.
 *
 * A screen that only says what is missing leaves somebody with nothing to do
 * (AGENTS.md section 45, `docs/sessions.md`), and what there is to do depends
 * on one thing: whether the recorder this window is attached to already records
 * games by itself.
 *
 * Where it does not — including every state where there is no recorder to ask,
 * because a terminal works whether or not this window has found one — the
 * answer is the command that does. Where it does, telling somebody to start a
 * second watcher in a terminal would have two recorders racing for the same
 * game, and the useful sentence is where the files went instead.
 */
export function whatWorksToday(link: RecorderLinkState | null): string {
  return recorderCanDo(link, 'automatic')
    ? 'Nothing needs starting: this recorder records a game as it launches and writes a session ' +
        'record beside the files each sitting produces. What it has recorded is in the Library, ' +
        'which brings itself up to date as each sitting ends.'
    : 'Automatic recording does work, from a terminal: clipped-recorder watch records a game as ' +
        'it launches and writes a session record beside the files. Making that something this ' +
        'window can start and follow is issue #241.';
}
