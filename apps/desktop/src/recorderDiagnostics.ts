import type { Diagnostics } from '@clipped/shared';
import { invoke } from '@tauri-apps/api/core';
import { useEffect, useState } from 'react';

import { asProblem, type LibraryProblem, type LibraryRead } from './library';

/**
 * Reading the recorder's own diagnostics from the window.
 *
 * Two of the twelve things SPEC.md section 36 asks a recorder to record — which
 * capture backend a recording in progress is using, and what this machine can
 * encode — arrive here through `get_diagnostics`
 * ([issue #302](https://github.com/wildware-uk/clipped/issues/302)). Before it
 * there was no command that carried either, which is why the Diagnostics screen
 * could report one of the twelve.
 *
 * # Why it is asked for rather than watched
 *
 * The same reason `hotkeys.ts` asks: both answers are settled before this window
 * is likely to exist. A recording chooses its capture backend in its first
 * milliseconds, and the capability report is read when the recorder starts —
 * from a cache, on a machine whose driver has not changed. An event would be
 * published to nobody, and a window that waited for one would show an empty
 * screen for ever.
 *
 * # Three answers, never two
 *
 * The same three {@link LibraryRead} carries. A recorder that could not be asked
 * and a machine with no hardware encoder are different facts, and drawing the
 * first as the second would say "Clipped found no encoder on this machine" about
 * a question nobody managed to put (AGENTS.md section 27). That confusion is the
 * whole reason the recorder advertises `diagnostics` in its welcome.
 */

/** Asks the recorder how it is capturing and what this machine can encode. */
export async function readDiagnostics(): Promise<Diagnostics> {
  return invoke<Diagnostics>('recorder_diagnostics');
}

/**
 * What the window says about diagnostics it could not read.
 *
 * `unknown_command` is the one worth its own sentence, and it is the sharpest
 * case on this screen. A recorder that started before this window was installed
 * has no `get_diagnostics` at all — so the honest thing to say is that this
 * *recorder* cannot be asked, never that the machine has nothing to report.
 */
export function describeDiagnosticsProblem(problem: LibraryProblem): string {
  switch (problem.code) {
    case 'no_recorder_configured':
    case 'recorder_unreachable':
      return `Clipped could not ask the recorder, so neither of these is known. ${problem.message}`;
    case 'unknown_command':
      return (
        'The recorder that is running is older than this window and cannot be asked how it is ' +
        'capturing or what this machine can encode. Restarting Clipped starts the recorder that ' +
        'came with it.'
      );
    default:
      return problem.message;
  }
}

/** The recorder's diagnostics, read once when the screen that shows them opens. */
export function useRecorderDiagnostics(): LibraryRead<Diagnostics> {
  const [read, setRead] = useState<LibraryRead<Diagnostics>>({ state: 'reading' });

  useEffect(() => {
    let current = true;
    readDiagnostics()
      .then((diagnostics) => {
        if (current) {
          setRead({ state: 'read', value: diagnostics });
        }
      })
      .catch((thrown: unknown) => {
        if (current) {
          setRead({ state: 'unread', problem: asProblem(thrown) });
        }
      });
    return () => {
      current = false;
    };
  }, []);

  return read;
}
