import type { RecorderLinkState } from './useRecorderLink';

/**
 * What the window can honestly say about the recording happening now.
 *
 * Beside `HomeScreen.tsx` rather than inside it, for the reason
 * `describeGameDetection` sits beside the Games screen: the wording is the part
 * worth testing, and a module exporting both a component and a function is one
 * neither Fast Refresh nor a reader can take apart.
 *
 * # Why this is the only real thing on the Home screen
 *
 * SPEC.md section 17 draws Home as recent sessions, recently clipped,
 * favourites and games. Every one of those is a read of the library index, and
 * **no read of it can reach this window**: `clipped-library` builds `library.db`
 * inside the recorder's process, the control protocol has no command that reads
 * it, and `src-tauri/capabilities/default.json` grants this window three `core:`
 * permissions and no file-system access at all. Issue #301.
 *
 * What is left is what the recorder link already carries, and it happens to be
 * the one thing somebody opening Home most wants: whether something is being
 * recorded this minute, and which file it is going into. That file is the only
 * library-shaped fact this build can state, and it is measured rather than
 * guessed — `ActiveRecording::output` is the path the recorder is writing.
 */

/** The few words shown as the state, one sentence, and the file if there is one. */
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
   * The file being written, in full, or `undefined` when nothing is.
   *
   * Never abbreviated: it is the only thing on this screen anybody can act on,
   * and a path with a middle ellipsis cannot be typed into Explorer (AGENTS.md
   * sections 28 and 45).
   */
  readonly output?: string;
}

/**
 * What to say about the recording in progress, given where the link stands.
 *
 * A pure function so that every link state has exactly one rendering rather
 * than a chain of conditions inside a component, and so that the wording can be
 * tested without a window.
 *
 * # What is deliberately not said
 *
 * **How long it has been recording.** `ActiveRecording::elapsed_ms` is on the
 * wire and is not shown, because of when it arrives: the recorder publishes
 * `status_changed` when a recording starts and when it ends (`apps/recorder/src/serve.rs`)
 * and at no point in between, so the elapsed time this window holds is the
 * elapsed time at the moment the recording started — near zero — and it never
 * moves. A duration frozen at 0:00 beside a recording that has been running for
 * an hour is worse than no duration, and counting up from it locally would be
 * this window inventing a figure nobody measured (AGENTS.md section 27).
 */
export function describeRecordingNow(link: RecorderLinkState | null): RecordingNowText {
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
      return link.status.state === 'recording'
        ? {
            state: `Recording ${link.status.target}`,
            detail: 'The file below is being written now, and is playable as it grows.',
            output: link.status.output,
          }
        : {
            state: 'This recorder is not recording',
            detail:
              'The recorder this window is attached to is running and idle. A recorder started ' +
              'elsewhere — clipped-recorder watch, from a terminal — serves no protocol and is ' +
              'invisible here, so this says nothing about the rest of the machine.',
          };
  }
}

/**
 * Where a recording goes, which is the question Home replaces the library with.
 *
 * True in every link state, so it is said once rather than folded into the five
 * renderings above — and it is the useful answer to "where are my recordings?"
 * on a screen that cannot list one (AGENTS.md section 45).
 */
export const WHERE_RECORDINGS_GO =
  'Recordings are ordinary files, and stay usable without Clipped. Each sitting writes its ' +
  'video files and one session record beside them, in the output folder the recorder was given ' +
  '(docs/sessions.md). Nothing in this window has read that folder.';
